//! `seed-sheets`: dump H2Overdrive's own data tables from `triton.lux` into game sheets.
//!
//! Sheet format (see `crates/riptide-sheets`): row 1 = column names, row 2 = `@types` row,
//! first column = row id. Properties the game leaves at their default get that default; a
//! declared-but-empty value is written as `-`.

use anyhow::{Context, Result};
use riptide_assets::h2level::{parse_object_list_with_defaults, XmlObject};
use riptide_assets::lux::LuxArchive;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

pub fn objects(lux: &LuxArchive, name: &str) -> Result<Vec<XmlObject>> {
    let blob = lux.get(&format!("data.{name}")).with_context(|| format!("data.{name} missing"))?;
    parse_object_list_with_defaults(&String::from_utf8_lossy(blob))
}

fn cell(s: &str) -> String {
    if s.is_empty() {
        "-".into()
    } else if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn value(o: &XmlObject, key: &str) -> String {
    o.props.get(key).map(|v| v.join(" ")).unwrap_or_default()
}

/// Engine property type -> sheet column type.
fn sheet_type(engine: &str) -> &'static str {
    match engine {
        "TYPE_FLOAT" => "f32",
        "TYPE_INT" => "i32",
        "TYPE_BOOL" => "bool",
        "TYPE_COLOR" | "TYPE_VEC3" | "TYPE_QUAT" => "vec",
        _ => "str",
    }
}

/// Rows = objects, columns = every property any of them declares.
pub fn wide(objs: &[XmlObject]) -> String {
    wide_with(objs, |_| None)
}

/// Like [`wide`], with some columns typed as references (`ref:<sheet>`); their values are
/// normalised to the target ids (`-` when empty or unset).
pub fn wide_with(objs: &[XmlObject], refs: impl Fn(&str) -> Option<&'static str>) -> String {
    let mut cols: BTreeMap<&str, &str> = BTreeMap::new();
    for o in objs {
        for (k, t) in &o.types {
            cols.entry(k).or_insert(refs(k).unwrap_or(sheet_type(t)));
        }
    }
    let mut out = String::new();
    let names: Vec<String> = std::iter::once("id".to_string()).chain(cols.keys().map(|c| cell(c))).collect();
    let types: Vec<&str> = std::iter::once("@types").chain(cols.values().copied()).collect();
    writeln!(out, "{}\n{}", names.join(","), types.join(",")).unwrap();
    for o in objs {
        let row: Vec<String> = std::iter::once(cell(&o.name))
            .chain(cols.keys().map(|c| {
                let v = value(o, c);
                match refs(c) {
                    Some(_) if v.is_empty() || v.eq_ignore_ascii_case("ffffffff") => "-".to_string(),
                    Some("ref:h2_samples") => cell(&sample_id(&v)),
                    _ => cell(&v),
                }
            }))
            .collect();
        writeln!(out, "{}", row.join(",")).unwrap();
    }
    out
}

/// Rows = properties: `id,object,type,value`, the value typed per row by `type`.
fn long(objs: &[XmlObject]) -> String {
    let mut out = String::from("id,object,type,value\n@types,str,str,dyn\n");
    for o in objs {
        let mut keys: Vec<&String> = o.types.keys().collect();
        keys.sort();
        for k in keys {
            writeln!(out, "{},{},{},{}", cell(k), cell(&o.name), sheet_type(&o.types[k]), cell(&value(o, k))).unwrap();
        }
    }
    out
}

/// `data.triton_levels`: `[English Name]` followed by `key = value` lines, joined with each
/// level's `CLevelInfo` and which data files ship for it.
fn levels(lux: &LuxArchive) -> Result<String> {
    let text = String::from_utf8_lossy(lux.get("data.triton_levels").context("data.triton_levels missing")?).to_string();
    let mut out = String::from(
        "id,engine_name,class,num_laps,starting_seconds,skybox,has_ai,has_sectors,has_startpoints,has_worldobs,sun_color,sun_scale,sun_pitch,sun_yaw,ambient_color,ambient_scale,map_texel0,map_texel1,map_world0,map_world1\n\
         @types,str,str,i32,i32,str,bool,bool,bool,bool,vec,f32,f32,f32,vec,f32,vec,vec,vec,vec\n",
    );
    let mut entries: Vec<(String, String, String)> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with("//")) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            entries.push((name.to_string(), String::new(), String::new()));
        } else if let (Some((k, v)), Some(e)) = (line.split_once('='), entries.last_mut()) {
            match k.trim() {
                "prefix" => e.1 = v.trim().to_string(),
                "class" => e.2 = v.trim().to_string(),
                _ => {}
            }
        }
    }
    for (name, code, class) in entries {
        let info = objects(lux, &format!("{code}_level")).ok();
        let info = info.as_ref().and_then(|v| v.iter().find(|o| o.class == "CLevelInfo"));
        // Levels without a CLevelInfo (or without the property) run on engine defaults: 0.
        let num = |k: &str| info.map(|o| value(o, k)).filter(|v| !v.is_empty()).unwrap_or_else(|| "0".into());
        let has = |s: &str| lux.contains(&format!("data.{code}_{s}")).to_string();
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            cell(&code),
            cell(&name),
            cell(&class),
            num("Num Laps"),
            num("Starting Seconds"),
            cell(&info.map(|o| value(o, "Skybox 0 Mesh Name")).unwrap_or_default()),
            has("ai"),
            has("sectors"),
            has("startpoints"),
            has("worldobs"),
            num_or(info, "Terrain Dir Light Color motif color", "1 1 1 1"),
            num_or(info, "Terrain Dir Light Color motif scale", "1"),
            num_or(info, "Terrain Dir Light Pitch", "45"),
            num_or(info, "Terrain Dir Light Yaw", "0"),
            num_or(info, "Terrain Amb Light Color motif color", "0.3 0.3 0.3 1"),
            num_or(info, "Terrain Amb Light Color motif scale", "1"),
            format!("{} {}", num_or(info, "Map Texel 0 X", "0"), num_or(info, "Map Texel 0 Y", "0")),
            format!("{} {}", num_or(info, "Map Texel 1 X", "0"), num_or(info, "Map Texel 1 Y", "0")),
            format!("{} {}", num_or(info, "Map World 0 X", "0"), num_or(info, "Map World 0 Z", "0")),
            format!("{} {}", num_or(info, "Map World 1 X", "0"), num_or(info, "Map World 1 Z", "0"))
        )
        .unwrap();
    }
    Ok(out)
}

pub fn seed_sheets(lux: &LuxArchive, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let write = |name: &str, body: String| -> Result<()> {
        let path = dir.join(name);
        std::fs::write(&path, body)?;
        println!("wrote {}", path.display());
        Ok(())
    };
    write(
        "h2_boatdefs.csv",
        wide_with(&objects(lux, "global_boatdef")?, |c| match c {
            "Engine Def" => Some("ref:h2_enginedefs"),
            "FlameDef Boost" | "FlameDef Super" | "FlameDef Super 2" => Some("ref:h2_rocket_flames"),
            _ => None,
        }),
    )?;
    // Rocket flames (boost exhaust, torches): flame -> up to four particle layers -> colour motifs.
    // Global motifs, then each level's own (`data.<lvl>_motifs`); the first definition of a name wins.
    let mut motifs = objects(lux, "global_Motifs")?;
    let mut local: Vec<String> = lux
        .entries()
        .iter()
        .filter_map(|e| e.name.strip_prefix("data.")?.strip_suffix("_motifs").map(str::to_string))
        .collect();
    local.sort();
    for code in local {
        for o in objects(lux, &format!("{code}_motifs"))? {
            if o.class == "CSMotifDef" && !motifs.iter().any(|m| m.name.eq_ignore_ascii_case(&o.name)) {
                motifs.push(o);
            }
        }
    }
    write("h2_motifs.csv", wide(&motifs))?;
    write(
        "h2_rocket_layers.csv",
        wide_with(&objects(lux, "global_RocketLayerDefs")?, |c| c.ends_with(" Motif").then_some("ref:h2_motifs")),
    )?;
    write("h2_bolts.csv", wide(&objects(lux, "global_bolts")?.into_iter().filter(|o| o.class == "CSBoltDef").collect::<Vec<_>>()))?;
    write(
        "h2_rocket_flames.csv",
        wide_with(&objects(lux, "global_RocketFlameDefs")?, |c| c.starts_with("Layer").then_some("ref:h2_rocket_layers")),
    )?;
    let mut game = objects(lux, "global_TritonGame")?;
    game.retain(|o| o.class == "CTritonGameDef");
    game.extend(objects(lux, "global_Experience")?);
    write("h2_globals.csv", long(&game))?;
    write("h2_levels.csv", levels(lux)?)?;
    write("h2_tripwires.csv", tripwires(lux)?)?;
    let lib = objects(lux, "global_SoundLibrary")?;
    let class = |c: &str| lib.iter().filter(|o| o.class == c).cloned().collect::<Vec<_>>();
    // Some defs name samples the banks don't ship: keep those names in `Missing Samples`.
    let (sample_csv, have) = samples(lux)?;
    let mut defs = class("CSSoundDef");
    for d in &mut defs {
        let mut missing = Vec::new();
        for (k, v) in d.props.iter_mut().filter(|(k, _)| k.starts_with("Sound_Name")) {
            if let Some(name) = v.first_mut().filter(|n| !n.is_empty() && !have.contains(&sample_id(n))) {
                missing.push(format!("{k}={name}"));
                name.clear();
            }
        }
        missing.sort();
        d.types.insert("Missing Samples".into(), "TYPE_STRING".into());
        d.props.insert("Missing Samples".into(), vec![missing.join(" ")]);
    }
    write("h2_sounddefs.csv", wide_with(&defs, |c| c.starts_with("Sound_Name").then_some("ref:h2_samples")))?;
    write("h2_enginedefs.csv", wide_with(&class("CEngineDef"), |c| c.ends_with("Sound Def").then_some("ref:h2_sounddefs")))?;
    write("h2_samples.csv", sample_csv)?;
    let gs = objects(lux, "global_TritonGame")?.into_iter().filter(|o| o.class == "CTritonGlobalSound").collect::<Vec<_>>();
    let mut out = String::from("id,sounddef\n@types,ref:h2_sounddefs\n");
    for o in &gs {
        let mut keys: Vec<&String> = o.types.keys().filter(|k| o.types[*k] == "TYPE_OBJECT_REF").collect();
        keys.sort();
        for k in keys {
            let v = value(o, k);
            let v = if v.is_empty() || v.eq_ignore_ascii_case("ffffffff") { "-".to_string() } else { v };
            writeln!(out, "{},{}", cell(k), cell(&v)).unwrap();
        }
    }
    write("h2_globalsounds.csv", out)?;
    let hud: Vec<_> = objects(lux, "global_GameHud")?.into_iter().filter(|o| o.class == "CSHudCard").collect();
    write("h2_hud.csv", wide(&hud))?;
    write("h2_font_glyphs.csv", font_glyphs(lux)?)?;
    write("h2_upgrades.csv", upgrades(lux)?)?;
    Ok(())
}

/// Every `CSTripwire` of every level: checkpoints (`Script Name` = Checkpoint, seconds in
/// `Blind Int Data`), start/finish lines, sky swaps, ramp achievements.
fn tripwires(lux: &LuxArchive) -> Result<String> {
    let mut out = String::from(
        "id,level,name,script,int_data,float_data,position,scale,trigger_shape\n@types,ref:h2_levels,str,str,i32,f32,vec,f32,i32\n",
    );
    let mut codes: Vec<String> = lux
        .entries()
        .iter()
        .filter_map(|e| e.name.strip_prefix("data.")?.strip_suffix("_tripwires").map(str::to_string))
        .collect();
    codes.sort();
    for code in codes {
        for o in objects(lux, &format!("{code}_tripwires"))?.iter().filter(|o| o.class == "CSTripwire") {
            let num = |k: &str, d: &str| Some(value(o, k)).filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string());
            writeln!(
                out,
                "{},{},{},{},{},{},{},{},{}",
                cell(&format!("{code}.{}", o.name)),
                cell(&code),
                cell(&o.name),
                cell(&value(o, "Script Name")),
                num("Blind Int Data", "0"),
                num("Blind Float Data", "0"),
                cell(&value(o, "Position")),
                num("Scale", "1"),
                num("Trigger Shape", "0")
            )
            .unwrap();
        }
    }
    Ok(out)
}

/// Every sample of every FSB4 bank (`fbnk.*`): which bank holds it and how it is encoded.
fn samples(lux: &LuxArchive) -> Result<(String, std::collections::HashSet<String>)> {
    let mut out = String::from("id,file,bank,codec,frequency,channels,samples,looped\n@types,str,str,str,i32,i32,i32,bool\n");
    let mut seen = std::collections::HashSet::new();
    let mut banks: Vec<&str> = lux.entries().iter().filter_map(|e| e.name.strip_prefix("fbnk.")).collect();
    banks.sort();
    for bank in banks {
        let data = lux.get(&format!("fbnk.{bank}")).context("bank")?;
        for s in riptide_assets::fsb::parse(data)? {
            // A name can appear in several banks (per-level copies); the first one wins.
            let id = sample_id(&s.name);
            if !seen.insert(id.clone()) {
                continue;
            }
            writeln!(out, "{},{},{bank},{},{},{},{},{}", cell(&id), cell(&s.name), s.codec(), s.frequency, s.channels, s.samples, s.looped()).unwrap();
        }
    }
    Ok((out, seen))
}

/// How sound defs name a sample: `so_com_321go.wav` -> `com_321go` (lower case).
pub fn sample_id(file: &str) -> String {
    let f = file.to_ascii_lowercase();
    let f = f.strip_prefix("so_").unwrap_or(&f);
    f.rsplit_once('.').map_or(f, |(stem, _)| stem).to_string()
}

/// A `CLevelInfo` property, or the given engine default when the level has none.
fn num_or(info: Option<&XmlObject>, key: &str, default: &str) -> String {
    info.map(|o| value(o, key)).filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_string())
}

/// The `data.<font>` glyph tables (`char C  X Y W H dx dy dw` lines) of every font whose atlas
/// (`txtr1.<font>`) ships: one row per glyph.
fn font_glyphs(lux: &LuxArchive) -> Result<String> {
    let mut out = String::from("id,font,code,x,y,w,h,dx,dy,dw,line_height\n@types,str,i32,i32,i32,i32,i32,i32,i32,i32,i32\n");
    let mut fonts: Vec<&str> = lux
        .entries()
        .iter()
        .filter_map(|e| e.name.strip_prefix("data."))
        .filter(|n| lux.contains(&format!("txtr1.{n}")))
        .collect();
    fonts.sort();
    for font in fonts {
        let text = String::from_utf8_lossy(lux.get(&format!("data.{font}")).context("font")?).to_string();
        let height = text.lines().find_map(|l| l.strip_prefix("height").map(|v| v.trim().to_string())).unwrap_or_else(|| "0".into());
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("char ") else { continue };
            let mut chars = rest.chars();
            let Some(ch) = chars.next() else { continue };
            let nums: Vec<&str> = chars.as_str().split_whitespace().collect();
            if nums.len() < 7 || nums.iter().any(|n| n.parse::<i32>().is_err()) {
                continue;
            }
            writeln!(
                out,
                "{},{},{},{},{}",
                cell(&format!("{font}.{}", ch as u32)),
                cell(font),
                ch as u32,
                nums[..7].join(","),
                height
            )
            .unwrap();
        }
    }
    Ok(out)
}

/// `data.upgrades`: which extra meshes (and their clips) hang off which bone ("channel") of each
/// boat, per upgrade axis and level. Level 0 is the boat as raced without upgrades.
fn upgrades(lux: &LuxArchive) -> Result<String> {
    let blob = lux.get("data.upgrades").context("data.upgrades missing")?;
    let text = String::from_utf8_lossy(blob);
    let doc = roxmltree::Document::parse(&text).context("data.upgrades XML")?;
    let mut out = String::from("id,axis,level,boat,channel,mesh,anim\n@types,str,i32,ref:h2_boatdefs,str,asset,asset\n");
    let asset = |kind: &str, v: Option<&str>| match v.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => format!("lux:{kind}.{n}"),
        None => "-".into(),
    };
    // The file lists some axes twice (a later "Booster" block): later rows get a suffix.
    let mut seen = std::collections::HashSet::new();
    for axis in doc.descendants().filter(|n| n.has_tag_name("UpgradeAxis")) {
        let a = axis.attribute("name").unwrap_or("?");
        for level in axis.children().filter(|n| n.has_tag_name("Level")) {
            let l = level.attribute("id").unwrap_or("0");
            for boat in level.children().filter(|n| n.has_tag_name("boat")) {
                let b = boat.attribute("name").unwrap_or("?");
                for (k, m) in boat.children().filter(|n| n.has_tag_name("Mesh")).enumerate() {
                    let mut id = format!("{a}.{l}.{b}.{k}");
                    while !seen.insert(id.clone()) {
                        id.push('b');
                    }
                    writeln!(
                        out,
                        "{},{},{l},{},{},{},{}",
                        cell(&id),
                        cell(a),
                        cell(b),
                        cell(m.attribute("channel").unwrap_or("-")),
                        asset("mesh32", m.attribute("mesh")),
                        asset("anim4", m.attribute("anim"))
                    )
                    .unwrap();
                }
            }
        }
    }
    Ok(out)
}
