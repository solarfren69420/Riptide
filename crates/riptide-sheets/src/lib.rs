//! Spreadsheet-driven build for Riptide.
//!
//! `sheets/index.csv` lists every sheet. Each sheet is a CSV whose first row names the columns,
//! whose second row (`@types,...`) types them, and whose first column is the row id.
//! Pipeline, run by `crates/riptide/build.rs` before every compile:
//!
//! 1. [`Book::load`]: read the sheets.
//! 2. [`Book::to_json`]: organise them as JSON, grouped by the index's `group` column.
//! 3. [`preflight`]: checkmark every row x column cell, overlap the sheets (refs, assets,
//!    key bindings, override types) and list what is unimplemented. Any error stops the build.
//! 4. [`codegen::generate`]: one Rust strut per row, from the JSON only.

pub mod codegen;
mod csv;
pub mod preflight;

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub use preflight::{preflight, AssetIndex, Report};

/// A column type from a sheet's `@types` row.
#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Str,
    I32,
    F32,
    Bool,
    /// Space-separated floats.
    Vec,
    Enum(Vec<String>),
    /// Id of a row in another sheet, or `-`.
    Ref(String),
    /// `lux:<entry>` or `ht:<entry>`, or `-`.
    Asset,
    /// `ok` or `n/a: <reason>`.
    Status,
    /// Space-separated Bevy `KeyCode` names, or `-`.
    Keys,
    /// Space-separated Bevy `GamepadButton` names, or `-`.
    Pad,
    /// Typed per row by the row's `type` column.
    Dyn,
}

impl Ty {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "str" => Ty::Str,
            "i32" => Ty::I32,
            "f32" => Ty::F32,
            "bool" => Ty::Bool,
            "vec" => Ty::Vec,
            "asset" => Ty::Asset,
            "status" => Ty::Status,
            "keys" => Ty::Keys,
            "pad" => Ty::Pad,
            "dyn" => Ty::Dyn,
            _ => {
                if let Some(v) = s.strip_prefix("enum:") {
                    Ty::Enum(v.split('|').map(str::to_string).collect())
                } else if let Some(r) = s.strip_prefix("ref:") {
                    Ty::Ref(r.to_string())
                } else {
                    bail!("unknown column type `{s}`")
                }
            }
        })
    }

    pub fn name(&self) -> String {
        match self {
            Ty::Str => "str".into(),
            Ty::I32 => "i32".into(),
            Ty::F32 => "f32".into(),
            Ty::Bool => "bool".into(),
            Ty::Vec => "vec".into(),
            Ty::Enum(v) => format!("enum:{}", v.join("|")),
            Ty::Ref(r) => format!("ref:{r}"),
            Ty::Asset => "asset".into(),
            Ty::Status => "status".into(),
            Ty::Keys => "keys".into(),
            Ty::Pad => "pad".into(),
            Ty::Dyn => "dyn".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sheet {
    pub id: String,
    pub group: String,
    pub file: PathBuf,
    pub doc: String,
    pub columns: Vec<String>,
    pub types: Vec<Ty>,
    pub rows: Vec<Vec<String>>,
}

impl Sheet {
    pub fn col(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }
    pub fn row(&self, id: &str) -> Option<&Vec<String>> {
        self.rows.iter().find(|r| r[0] == id)
    }
}

pub struct Book {
    pub dir: PathBuf,
    pub sheets: Vec<Sheet>,
}

fn read_sheet(path: &Path) -> Result<(Vec<String>, Vec<Ty>, Vec<Vec<String>>)> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut rows = csv::parse(&text).into_iter();
    let columns = rows.next().with_context(|| format!("{}: empty sheet", path.display()))?;
    let types_row = rows.next().with_context(|| format!("{}: missing @types row", path.display()))?;
    if types_row.first().map(String::as_str) != Some("@types") {
        bail!("{}: second row must start with @types", path.display());
    }
    if types_row.len() != columns.len() {
        bail!("{}: @types has {} cells for {} columns", path.display(), types_row.len(), columns.len());
    }
    let mut types = vec![Ty::Str];
    for t in &types_row[1..] {
        types.push(Ty::parse(t).with_context(|| path.display().to_string())?);
    }
    Ok((columns, types, rows.collect()))
}

impl Book {
    pub fn load(dir: &Path) -> Result<Self> {
        let (_, _, index) = read_sheet(&dir.join("index.csv"))?;
        let mut sheets = Vec::new();
        for r in index {
            let [id, group, file, doc] = r.as_slice() else { bail!("index.csv: row needs 4 cells: {r:?}") };
            let path = dir.join(file);
            let (columns, types, rows) = read_sheet(&path)?;
            sheets.push(Sheet {
                id: id.clone(),
                group: group.clone(),
                file: path,
                doc: doc.clone(),
                columns,
                types,
                rows,
            });
        }
        Ok(Self { dir: dir.to_path_buf(), sheets })
    }

    pub fn sheet(&self, id: &str) -> Option<&Sheet> {
        self.sheets.iter().find(|s| s.id == id)
    }

    /// Every CSV file the build depends on.
    pub fn files(&self) -> Vec<PathBuf> {
        std::iter::once(self.dir.join("index.csv")).chain(self.sheets.iter().map(|s| s.file.clone())).collect()
    }

    /// `{ group: { sheet: { doc, columns: [{name, type}], rows: [{col: value}] } } }`.
    pub fn to_json(&self) -> Value {
        let mut groups = Map::new();
        for s in &self.sheets {
            let rows: Vec<Value> = s
                .rows
                .iter()
                .map(|r| Value::Object(s.columns.iter().zip(r).map(|(c, v)| (c.clone(), Value::String(v.clone()))).collect()))
                .collect();
            let cols: Vec<Value> = s.columns.iter().zip(&s.types).map(|(c, t)| json!({"name": c, "type": t.name()})).collect();
            let g = groups.entry(s.group.clone()).or_insert_with(|| Value::Object(Map::new()));
            g.as_object_mut().unwrap().insert(s.id.clone(), json!({"doc": s.doc, "columns": cols, "rows": rows}));
        }
        Value::Object(groups)
    }
}

/// The whole pipeline: load, JSON, preflight, codegen. Writes `sheets.json`, `preflight.txt`
/// and `sheets.rs` into `out`. Fails (with the report) when preflight finds errors.
pub fn build(dir: &Path, out: &Path, assets: &dyn AssetIndex) -> Result<(Book, Report)> {
    let book = Book::load(dir)?;
    let json = book.to_json();
    std::fs::create_dir_all(out)?;
    std::fs::write(out.join("sheets.json"), serde_json::to_string_pretty(&json)?)?;
    let report = preflight(&book, assets);
    std::fs::write(out.join("preflight.txt"), report.to_string())?;
    if !report.errors.is_empty() {
        bail!("preflight failed ({} errors):\n{report}", report.errors.len());
    }
    std::fs::write(out.join("sheets.rs"), codegen::generate(&json)?)?;
    Ok((book, report))
}
