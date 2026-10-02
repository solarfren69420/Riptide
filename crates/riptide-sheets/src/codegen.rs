//! Code generation: one Rust strut per sheet row, built from the organised JSON only.
//!
//! Every sheet `foo` becomes `pub struct FooRow` and `pub const FOO: &[FooRow]`, plus
//! `pub mod foo_ids` with each row's index. Tuning sheets with a `value` column additionally get one
//! constant per row (`pub mod foo`) and, for their `f32` rows, `FooValues`: a runtime-tweakable
//! struct (cheat overrides write into it through `field_mut`).

use crate::Ty;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct",
    "super", "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final",
    "gen", "macro", "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Rust identifier for a sheet/column/value name: `snake_case`, or `CamelCase` when `camel`.
pub fn ident(name: &str, camel: bool) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let mut s = if camel {
        words.iter().map(|w| w[..1].to_ascii_uppercase() + &w[1..]).collect::<String>()
    } else {
        words.join("_")
    };
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, if camel { 'N' } else { '_' });
    }
    if KEYWORDS.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

struct Col {
    name: String,
    ty: Ty,
}

struct SheetJ<'a> {
    id: String,
    group: String,
    doc: String,
    cols: Vec<Col>,
    rows: &'a [Value],
}

fn cell<'a>(row: &'a Value, col: &str) -> &'a str {
    row.get(col).and_then(Value::as_str).unwrap_or("")
}

fn f32_lit(v: &str) -> Result<String> {
    let x: f32 = v.parse().with_context(|| format!("`{v}` is not a number"))?;
    Ok(format!("{x:?}"))
}

fn str_lit(v: &str) -> String {
    format!("{:?}", if v == "-" { "" } else { v })
}

fn bool_lit(v: &str) -> &'static str {
    if matches!(v, "1" | "true") {
        "true"
    } else {
        "false"
    }
}

fn vec_lit(v: &str) -> Result<String> {
    if v == "-" {
        return Ok("&[]".into());
    }
    let items: Result<Vec<String>> = v.split_whitespace().map(f32_lit).collect();
    Ok(format!("&[{}]", items?.join(", ")))
}

fn enum_name(sheet: &str, col: &str) -> String {
    format!("{}{}", ident(sheet, true), ident(col, true))
}

fn rust_type(sheet: &str, c: &Col) -> String {
    match &c.ty {
        Ty::Str | Ty::Asset => "&'static str".into(),
        Ty::I32 => "i32".into(),
        Ty::F32 => "f32".into(),
        Ty::Bool => "bool".into(),
        Ty::Vec => "&'static [f32]".into(),
        Ty::Enum(_) => enum_name(sheet, &c.name),
        Ty::Ref(_) => "Option<usize>".into(),
        Ty::Status => "Status".into(),
        Ty::Keys => "&'static [KeyCode]".into(),
        Ty::Pad => "&'static [GamepadButton]".into(),
        Ty::Dyn => "Value".into(),
    }
}

fn literal(sheet: &str, c: &Col, v: &str, row: &Value, index: &HashMap<String, HashMap<String, usize>>) -> Result<String> {
    Ok(match &c.ty {
        Ty::Str | Ty::Asset => str_lit(v),
        Ty::I32 => v.parse::<i32>().with_context(|| format!("`{v}` is not an integer"))?.to_string(),
        Ty::F32 => f32_lit(v)?,
        Ty::Bool => bool_lit(v).into(),
        Ty::Vec => vec_lit(v)?,
        Ty::Enum(_) => format!("{}::{}", enum_name(sheet, &c.name), ident(v, true)),
        Ty::Ref(target) if v == "-" => {
            let _ = target;
            "None".into()
        }
        Ty::Ref(target) => {
            let i = index.get(target).and_then(|m| m.get(v)).with_context(|| format!("unresolved ref {target}.{v}"))?;
            format!("Some({i})")
        }
        Ty::Status if v == "ok" => "Status::Ok".into(),
        Ty::Status => format!("Status::Na({:?})", v.trim_start_matches("n/a:").trim()),
        Ty::Keys | Ty::Pad if v == "-" => "&[]".into(),
        Ty::Keys => format!("&[{}]", v.split_whitespace().map(|k| format!("KeyCode::{k}")).collect::<Vec<_>>().join(", ")),
        Ty::Pad => format!("&[{}]", v.split_whitespace().map(|k| format!("GamepadButton::{k}")).collect::<Vec<_>>().join(", ")),
        Ty::Dyn => match Ty::parse(cell(row, "type"))? {
            Ty::F32 => format!("Value::F32({})", f32_lit(v)?),
            Ty::I32 => format!("Value::I32({})", v.parse::<i32>()?),
            Ty::Bool => format!("Value::Bool({})", bool_lit(v)),
            Ty::Vec => format!("Value::Vec({})", vec_lit(v)?),
            _ => format!("Value::Str({})", str_lit(v)),
        },
    })
}

pub fn generate(json: &Value) -> Result<String> {
    let mut sheets = Vec::new();
    for (gname, group) in json.as_object().context("json root")? {
        for (id, s) in group.as_object().context("group")? {
            let cols = s["columns"]
                .as_array()
                .context("columns")?
                .iter()
                .map(|c| Ok(Col { name: c["name"].as_str().unwrap_or("").to_string(), ty: Ty::parse(c["type"].as_str().unwrap_or(""))? }))
                .collect::<Result<Vec<_>>>()?;
            sheets.push(SheetJ {
                id: id.clone(),
                group: gname.clone(),
                doc: s["doc"].as_str().unwrap_or("").to_string(),
                cols,
                rows: s["rows"].as_array().context("rows")?,
            });
        }
    }
    let index: HashMap<String, HashMap<String, usize>> = sheets
        .iter()
        .map(|s| (s.id.clone(), s.rows.iter().enumerate().map(|(i, r)| (cell(r, "id").to_string(), i)).collect()))
        .collect();

    let mut o = String::new();
    o.push_str(
        "// @generated by riptide-sheets from sheets/*.csv (via sheets.json). Do not edit: change the sheets.\n\
         #[allow(unused_imports)]\nuse bevy::prelude::{GamepadButton, KeyCode};\n\n\
         /// `ok`, or `n/a: <reason>` for something not implemented.\n\
         #[derive(Clone, Copy, Debug, PartialEq)]\npub enum Status {\n    Ok,\n    Na(&'static str),\n}\n\n\
         impl Status {\n    pub fn is_ok(&self) -> bool {\n        matches!(self, Status::Ok)\n    }\n}\n\n\
         /// A cell typed per row by its sheet's `type` column.\n\
         #[derive(Clone, Copy, Debug, PartialEq)]\npub enum Value {\n    F32(f32),\n    I32(i32),\n    Bool(bool),\n    Str(&'static str),\n    Vec(&'static [f32]),\n}\n",
    );
    for s in &sheets {
        let ty = ident(&s.id, true);
        // Enums.
        for c in &s.cols {
            if let Ty::Enum(opts) = &c.ty {
                let name = enum_name(&s.id, &c.name);
                writeln!(o, "\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub enum {name} {{").unwrap();
                for v in opts {
                    writeln!(o, "    {},", ident(v, true)).unwrap();
                }
                writeln!(o, "}}\n\nimpl {name} {{\n    pub fn as_str(&self) -> &'static str {{\n        match self {{").unwrap();
                for v in opts {
                    writeln!(o, "            {name}::{} => {v:?},", ident(v, true)).unwrap();
                }
                o.push_str("        }\n    }\n}\n");
            }
        }
        // Row struct and one strut per row.
        writeln!(o, "\n/// {}\n#[derive(Clone, Debug)]\npub struct {ty}Row {{", s.doc).unwrap();
        for c in &s.cols {
            writeln!(o, "    /// `{}` ({})\n    pub {}: {},", c.name, c.ty.name(), ident(&c.name, false), rust_type(&s.id, c)).unwrap();
        }
        writeln!(o, "}}\n\npub const {}: &[{ty}Row] = &[", ident(&s.id, false).to_ascii_uppercase()).unwrap();
        for row in s.rows {
            writeln!(o, "    {ty}Row {{").unwrap();
            for c in &s.cols {
                let v = cell(row, &c.name);
                let lit = literal(&s.id, c, v, row, &index).with_context(|| format!("{}[{}].{}", s.id, cell(row, "id"), c.name))?;
                writeln!(o, "        {}: {lit},", ident(&c.name, false)).unwrap();
            }
            o.push_str("    },\n");
        }
        o.push_str("];\n");
        // Row indices.
        writeln!(o, "\n#[allow(dead_code)]\npub mod {}_ids {{", ident(&s.id, false)).unwrap();
        for (i, row) in s.rows.iter().enumerate() {
            writeln!(o, "    pub const {}: usize = {i};", ident(cell(row, "id"), false).to_ascii_uppercase()).unwrap();
        }
        o.push_str("}\n");
        // Key/value sheets: constants plus a tweakable struct of the f32 rows.
        if let Some(vc) = s.cols.iter().find(|c| c.name == "value" && s.group == "tuning") {
            let row_ty = |row: &Value| -> Result<Ty> {
                Ok(if vc.ty == Ty::Dyn { Ty::parse(cell(row, "type"))? } else { vc.ty.clone() })
            };
            writeln!(o, "\n#[allow(dead_code)]\npub mod {} {{", ident(&s.id, false)).unwrap();
            for row in s.rows {
                let v = cell(row, "value");
                let name = ident(cell(row, "id"), false).to_ascii_uppercase();
                let (t, lit) = match row_ty(row)? {
                    Ty::F32 => ("f32", f32_lit(v)?),
                    Ty::I32 => ("i32", v.parse::<i32>()?.to_string()),
                    Ty::Bool => ("bool", bool_lit(v).to_string()),
                    Ty::Vec => ("&[f32]", vec_lit(v)?),
                    _ => ("&str", str_lit(v)),
                };
                writeln!(o, "    pub const {name}: {t} = {lit};").unwrap();
            }
            o.push_str("}\n");
            let mut f32_rows = Vec::new();
            for row in s.rows {
                if row_ty(row)? == Ty::F32 {
                    f32_rows.push((cell(row, "id"), ident(cell(row, "id"), false), f32_lit(cell(row, "value"))?));
                }
            }
            writeln!(o, "\n/// The `f32` rows of `{}`, adjustable at runtime.\n#[derive(Clone, Debug)]\npub struct {ty}Values {{", s.id).unwrap();
            for (_, f, _) in &f32_rows {
                writeln!(o, "    pub {f}: f32,").unwrap();
            }
            writeln!(o, "}}\n\npub const {}_DEFAULT: {ty}Values = {ty}Values {{", ident(&s.id, false).to_ascii_uppercase()).unwrap();
            for (_, f, lit) in &f32_rows {
                writeln!(o, "    {f}: {lit},").unwrap();
            }
            writeln!(o, "}};\n\nimpl {ty}Values {{\n    /// The field for row id `id`.\n    pub fn field_mut(&mut self, id: &str) -> Option<&mut f32> {{\n        match id {{").unwrap();
            for (id, f, _) in &f32_rows {
                writeln!(o, "            {id:?} => Some(&mut self.{f}),").unwrap();
            }
            o.push_str("            _ => None,\n        }\n    }\n\n    /// The value for row id `id`.\n    pub fn get(&self, id: &str) -> Option<f32> {\n        let mut copy = self.clone();\n        copy.field_mut(id).map(|v| *v)\n    }\n}\n");
        }
    }
    if o.contains("\u{0}") {
        bail!("NUL in generated code");
    }
    Ok(o)
}
