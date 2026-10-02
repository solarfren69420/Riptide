//! Spreadsheet-driven build: before every compile, read `sheets/`, organise it as JSON, run the
//! preflight (checkmarks + sheet overlap) and generate `sheets.rs`, one strut per row.
//! Any preflight error stops the build. Reports land in `target/sheets/`.

use riptide_sheets::preflight::GameAssets;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root");
    let sheets = root.join("sheets");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed={}", sheets.display());
    println!("cargo:rerun-if-env-changed=RIPTIDE_LUX");
    println!("cargo:rerun-if-env-changed=RIPTIDE_GDI");
    let result = riptide_sheets::build(&sheets, &out, &GameAssets::open());
    // Keep a readable copy of the JSON and the report next to the build output.
    let report_dir = root.join("target/sheets");
    let _ = std::fs::create_dir_all(&report_dir);
    for f in ["sheets.json", "preflight.txt"] {
        let _ = std::fs::copy(out.join(f), report_dir.join(f));
    }
    match result {
        Ok((_, report)) => {
            if !report.unimplemented.is_empty() {
                println!(
                    "cargo:warning=sheets preflight clean; {} unimplemented items listed in {}",
                    report.unimplemented.len(),
                    report_dir.join("preflight.txt").display()
                );
            }
        }
        Err(e) => {
            for line in format!("{e:#}").lines() {
                println!("cargo:warning={line}");
            }
            panic!("sheets preflight failed - fix the sheets (see {})", report_dir.join("preflight.txt").display());
        }
    }
}
