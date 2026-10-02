//! Preflight: checkmark every row x column intersection, then overlap the sheets and point out
//! everything that will fail (errors, which stop the build) or is unimplemented (`n/a` statuses).

use crate::{codegen::ident, Book, Sheet, Ty};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Lookup of game assets referenced by `asset` cells.
pub trait AssetIndex {
    /// `triton.lux` entry, e.g. `mesh32.BG_Rogue`.
    fn lux(&self, entry: &str) -> bool;
    /// `HYDRODC.R2` entry, e.g. `GBBBANSHUP0`.
    fn ht(&self, entry: &str) -> bool;
    /// A track R2 on the HT disc holding entry `entry`, e.g. `GRAV.R2` / `HGTGRAVTRH0`.
    fn ht_track(&self, file: &str, entry: &str) -> bool;
}

/// The real game archives.
pub struct GameAssets {
    lux: Option<riptide_assets::lux::LuxArchive>,
    ht: Option<riptide_assets::ht::HydroThunder>,
}

impl GameAssets {
    pub fn open() -> Self {
        Self {
            lux: riptide_assets::lux::LuxArchive::open(&riptide_assets::default_lux_path()).ok(),
            ht: riptide_assets::ht::HydroThunder::open(&riptide_assets::default_gdi_path()).ok(),
        }
    }
}

impl AssetIndex for GameAssets {
    fn lux(&self, entry: &str) -> bool {
        self.lux.as_ref().is_some_and(|l| l.contains(entry))
    }
    fn ht(&self, entry: &str) -> bool {
        self.ht.as_ref().is_some_and(|h| h.main.contains(entry))
    }
    fn ht_track(&self, file: &str, entry: &str) -> bool {
        self.ht.as_ref().is_some_and(|h| h.has_track(file, entry))
    }
}

#[derive(Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub unimplemented: Vec<String>,
    /// Evidence rows not yet observed (hypothesis / design / unknown / contradicted).
    pub unverified: Vec<String>,
    pub sheets: usize,
    pub rows: usize,
    pub cells: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "PREFLIGHT: {} sheets, {} rows, {} cells checked; {} errors, {} unimplemented",
            self.sheets,
            self.rows,
            self.cells,
            self.errors.len(),
            self.unimplemented.len()
        )?;
        if !self.errors.is_empty() {
            writeln!(f, "\nWILL FAIL (fix in the sheets):")?;
            for e in &self.errors {
                writeln!(f, "  x {e}")?;
            }
        }
        if !self.unverified.is_empty() {
            writeln!(f, "\nUNVERIFIED (evidence ledger):")?;
            for u in &self.unverified {
                writeln!(f, "  ? {u}")?;
            }
        }
        if !self.unimplemented.is_empty() {
            writeln!(f, "\nUNIMPLEMENTED (n/a statuses):")?;
            for u in &self.unimplemented {
                writeln!(f, "  - {u}")?;
            }
        }
        Ok(())
    }
}

const KEY_NAMES: &[&str] = &[
    "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Space", "Enter", "Escape", "Tab", "Backspace", "ShiftLeft",
    "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "Home", "End", "PageUp", "PageDown", "Insert",
    "Delete", "Minus", "Equal", "Comma", "Period", "Slash", "Semicolon", "Quote", "BracketLeft", "BracketRight",
    "Backslash", "Backquote",
];
const PAD_NAMES: &[&str] = &[
    "South", "East", "North", "West", "C", "Z", "LeftTrigger", "LeftTrigger2", "RightTrigger", "RightTrigger2", "Select",
    "Start", "Mode", "LeftThumb", "RightThumb", "DPadUp", "DPadDown", "DPadLeft", "DPadRight",
];

fn key_ok(k: &str) -> bool {
    let tail = |p: &str| k.strip_prefix(p);
    KEY_NAMES.contains(&k)
        || tail("Key").is_some_and(|t| t.len() == 1 && t.chars().all(|c| c.is_ascii_uppercase()))
        || tail("Digit").is_some_and(|t| t.len() == 1 && t.chars().all(|c| c.is_ascii_digit()))
        || tail("F").is_some_and(|t| t.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)))
}

/// A cell nobody has checked off yet.
fn unchecked(v: &str) -> bool {
    let v = v.trim();
    v.is_empty() || v.eq_ignore_ascii_case("todo") || v.ends_with('?')
}

pub fn preflight(book: &Book, assets: &dyn AssetIndex) -> Report {
    let mut r = Report { sheets: book.sheets.len(), ..Default::default() };
    let mut structs = HashSet::new();
    for s in &book.sheets {
        if !structs.insert(ident(&s.id, true)) {
            r.errors.push(format!("{}: sheet id collides with another sheet's generated type name", s.id));
        }
        check_sheet(book, s, assets, &mut r);
    }
    overlap(book, &mut r);
    r
}

fn check_sheet(book: &Book, s: &Sheet, assets: &dyn AssetIndex, r: &mut Report) {
    let mut fields = HashMap::new();
    for c in &s.columns {
        if let Some(prev) = fields.insert(ident(c, false), c) {
            r.errors.push(format!("{}: columns `{prev}` and `{c}` generate the same field name", s.id));
        }
    }
    let type_col = s.col("type");
    let mut ids = HashSet::new();
    let mut consts = HashSet::new();
    for (i, row) in s.rows.iter().enumerate() {
        r.rows += 1;
        let at = format!("{}[{}]", s.id, row.first().map(String::as_str).unwrap_or("?"));
        if row.len() != s.columns.len() {
            r.errors.push(format!("{at} (data row {}): {} cells for {} columns", i + 1, row.len(), s.columns.len()));
            continue;
        }
        if !ids.insert(row[0].clone()) {
            r.errors.push(format!("{at}: duplicate row id"));
        } else if !consts.insert(crate::codegen::ident(&row[0], false).to_ascii_uppercase()) {
            r.errors.push(format!("{at}: id collides with another row's generated constant (ids differ only in case or punctuation)"));
        }
        for ((col, ty), v) in s.columns.iter().zip(&s.types).zip(row) {
            r.cells += 1;
            let cell = format!("{at}.{col}");
            if unchecked(v) {
                r.errors.push(format!("{cell}: unchecked (empty / TODO / ends with ?)"));
                continue;
            }
            let ty = if *ty == Ty::Dyn {
                match type_col.and_then(|c| Ty::parse(&row[c]).ok()) {
                    Some(t @ (Ty::Str | Ty::I32 | Ty::F32 | Ty::Bool | Ty::Vec)) => t,
                    _ => {
                        r.errors.push(format!("{cell}: dyn value needs a scalar `type` column in the same row"));
                        continue;
                    }
                }
            } else {
                ty.clone()
            };
            if let Err(e) = check_value(book, &ty, v, assets) {
                r.errors.push(format!("{cell} = `{v}`: {e}"));
            }
            if ty == Ty::Status && v != "ok" {
                r.unimplemented.push(format!("{cell}: {}", v.trim_start_matches("n/a:").trim()));
            }
        }
    }
}

fn check_value(book: &Book, ty: &Ty, v: &str, assets: &dyn AssetIndex) -> Result<(), String> {
    let none = v == "-";
    match ty {
        Ty::Str => Ok(()),
        Ty::I32 => v.parse::<i32>().map(|_| ()).map_err(|_| "not an integer".into()),
        Ty::F32 => v.parse::<f32>().map(|_| ()).map_err(|_| "not a number".into()),
        Ty::Bool => match v {
            "true" | "false" | "0" | "1" => Ok(()),
            _ => Err("not a bool (true/false/0/1)".into()),
        },
        Ty::Vec if none => Ok(()),
        Ty::Vec => v.split_whitespace().all(|x| x.parse::<f32>().is_ok()).then_some(()).ok_or("not a list of numbers".into()),
        Ty::Enum(opts) => opts.iter().any(|o| o == v).then_some(()).ok_or(format!("not one of {}", opts.join("|"))),
        Ty::Ref(_) if none => Ok(()),
        Ty::Ref(target) => match book.sheet(target) {
            None => Err(format!("referenced sheet `{target}` is not in index.csv")),
            Some(t) if t.row(v).is_none() => Err(format!("no row `{v}` in sheet `{target}`")),
            Some(_) => Ok(()),
        },
        Ty::Asset if none => Ok(()),
        Ty::Asset => match v.split_once(':') {
            Some(("lux", e)) if assets.lux(e) => Ok(()),
            Some(("ht", e)) if assets.ht(e) => Ok(()),
            Some(("httrack", fe)) if fe.split_once('/').is_some_and(|(f, e)| assets.ht_track(f, e)) => Ok(()),
            Some(("lux" | "ht" | "httrack", _)) => Err("asset not found in the game data".into()),
            _ => Err("asset must be lux:<entry>, ht:<entry> or httrack:<FILE.R2>/<entry>".into()),
        },
        Ty::Status => {
            if v == "ok" || v.strip_prefix("n/a:").is_some_and(|why| !why.trim().is_empty()) {
                Ok(())
            } else {
                Err("status must be `ok` or `n/a: <reason>`".into())
            }
        }
        Ty::Keys if none => Ok(()),
        Ty::Keys => v.split_whitespace().find(|k| !key_ok(k)).map_or(Ok(()), |k| Err(format!("unknown KeyCode `{k}`"))),
        Ty::Pad if none => Ok(()),
        Ty::Pad => {
            v.split_whitespace().find(|k| !PAD_NAMES.contains(k)).map_or(Ok(()), |k| Err(format!("unknown GamepadButton `{k}`")))
        }
        Ty::Dyn => Err("dyn".into()),
    }
}

fn get<'a>(s: &'a Sheet, row: &'a [String], col: &str) -> &'a str {
    s.col(col).and_then(|c| row.get(c)).map(String::as_str).unwrap_or("")
}

/// Cross-sheet checks: the sheets laid over each other must agree.
fn overlap(book: &Book, r: &mut Report) {
    // 1. A typed `value` that overrides a ref'd dyn row must match that row's type.
    for s in &book.sheets {
        let Some(vc) = s.col("value") else { continue };
        for (c, ty) in s.types.iter().enumerate() {
            let Ty::Ref(target) = ty else { continue };
            let Some(t) = book.sheet(target) else { continue };
            let (Some(tvc), Some(ttc)) = (t.col("value"), t.col("type")) else { continue };
            if t.types[tvc] != Ty::Dyn {
                continue;
            }
            for row in &s.rows {
                if let Some(trow) = t.row(&row[c]) {
                    if Ty::parse(&trow[ttc]).ok().as_ref() != Some(&s.types[vc]) {
                        r.errors.push(format!(
                            "{}[{}].value is {} but {}[{}] is {}",
                            s.id,
                            row[0],
                            s.types[vc].name(),
                            t.id,
                            trow[0],
                            trow[ttc]
                        ));
                    }
                }
            }
        }
    }
    // 2. No key or pad button bound twice in one context (`global` overlaps every context).
    let mut binds: Vec<(String, String, String)> = Vec::new();
    for s in &book.sheets {
        for (c, ty) in s.types.iter().enumerate() {
            if !matches!(ty, Ty::Keys | Ty::Pad) {
                continue;
            }
            for row in &s.rows {
                let ctx = s.col("context").map(|i| row[i].clone()).unwrap_or_else(|| "global".into());
                for k in row[c].split_whitespace().filter(|k| *k != "-") {
                    let kind = if *ty == Ty::Keys { "key" } else { "pad" };
                    let who = format!("{}[{}]", s.id, row[0]);
                    for (octx, ok, owho) in &binds {
                        if ok == &format!("{kind}:{k}") && (octx == &ctx || octx == "global" || ctx == "global") {
                            r.errors.push(format!("{kind} {k} bound twice: {owho} ({octx}) and {who} ({ctx})"));
                        }
                    }
                    binds.push((ctx.clone(), format!("{kind}:{k}"), who));
                }
            }
        }
    }
    // 3. `menu_order` columns are unique within their sheet.
    for s in &book.sheets {
        let Some(c) = s.col("menu_order") else { continue };
        let mut seen = HashSet::new();
        for row in &s.rows {
            if !seen.insert(&row[c]) {
                r.errors.push(format!("{}[{}].menu_order {} is used twice", s.id, row[0], row[c]));
            }
        }
    }
    // Evidence ledger: every target is a real `sheet.row.column` cell.
    if let Some(ev) = book.sheet("evidence") {
        for row in &ev.rows {
            let target = get(ev, row, "target");
            let found = target.split_once('.').and_then(|(sheet, rest)| {
                let (row_id, col) = rest.rsplit_once('.')?;
                let s = book.sheet(sheet)?;
                (s.row(row_id).is_some() && s.col(col).is_some()).then_some(())
            });
            if found.is_none() {
                r.errors.push(format!("evidence[{}]: target `{target}` is not a sheet.row.column cell", row[0]));
            }
            let status = get(ev, row, "status");
            if status != "observed" {
                r.unverified.push(format!("{} ({status}) {target}: {}", row[0], get(ev, row, "observation")));
            }
        }
    }
    // 4. Roster rules.
    if let (Some(boats), Some(defs)) = (book.sheet("boats"), book.sheet("h2_boatdefs")) {
        for row in &boats.rows {
            let at = format!("boats[{}]", row[0]);
            let secret = get(boats, row, "unlock") == "secret";
            let parent = get(boats, row, "unlock_parent");
            if secret != (parent != "-") {
                r.errors.push(format!("{at}: unlock=secret needs an unlock_parent (and only then)"));
            }
            if parent != "-" && boats.row(parent).is_some_and(|p| get(boats, p, "unlock") != "default") {
                r.errors.push(format!("{at}: unlock_parent `{parent}` is not itself a default boat"));
            }
            if get(boats, row, "status") == "ok" {
                match defs.row(get(boats, row, "boatdef")) {
                    None => r.errors.push(format!("{at}: selectable boat needs a boatdef")),
                    Some(d) if !matches!(get(defs, d, "Human Racer"), "1" | "true") => {
                        r.errors.push(format!("{at}: boatdef {} is not a Human Racer", d[0]))
                    }
                    _ => {}
                }
                if get(boats, row, "model") == "-" {
                    r.errors.push(format!("{at}: selectable boat needs a model"));
                }
            }
        }
    }
    // 5. Track rules: an `ok` status must be backed by shipped level data.
    if let (Some(tracks), Some(levels)) = (book.sheet("tracks"), book.sheet("h2_levels")) {
        for row in &tracks.rows {
            let at = format!("tracks[{}]", row[0]);
            let lvl = levels.row(get(tracks, row, "h2_level"));
            let has = |k: &str| lvl.is_some_and(|l| get(levels, l, k) == "true");
            if get(tracks, row, "game") == "h2" && get(tracks, row, "racing_line") == "ok" && !((has("has_ai") || has("has_sectors")) && has("has_startpoints")) {
                r.errors.push(format!("{at}: racing_line ok but its level has no _ai or _sectors line, or no _startpoints"));
            }
            if get(tracks, row, "game") == "h2" && get(tracks, row, "geometry") == "ok" && !has("has_sectors") {
                r.errors.push(format!("{at}: geometry ok but its level has no _sectors data"));
            }
            if get(tracks, row, "game") == "h2" && get(tracks, row, "h2_level") == "-" && get(tracks, row, "geometry") == "ok" {
                r.errors.push(format!("{at}: H2 track with geometry ok needs an h2_level"));
            }
            if get(tracks, row, "game") == "ht" && get(tracks, row, "geometry") == "ok" {
                let has_file = book
                    .sheet("ht_tracks")
                    .and_then(|s| s.row(get(tracks, row, "ht_track")).map(|r| get(s, r, "track") != "-"))
                    .unwrap_or(false);
                if !has_file {
                    r.errors.push(format!("{at}: HT track with geometry ok needs an ht_tracks row with a track file"));
                }
            }
        }
    }
}
