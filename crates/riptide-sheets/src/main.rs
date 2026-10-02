//! `sheets`: run the spreadsheet pipeline by hand.
//!
//! ```text
//! sheets check [dir]          preflight only: checkmarks + overlap report (exit 1 on errors)
//! sheets build [dir] [out]    json + preflight + codegen into out (default: <dir>/build)
//! ```

use anyhow::{bail, Result};
use riptide_sheets::preflight::GameAssets;
use riptide_sheets::{preflight, Book};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("sheets"));
    match args.first().map(String::as_str) {
        Some("check") => {
            let report = preflight(&Book::load(&dir)?, &GameAssets::open());
            let _ = std::io::Write::write_all(&mut std::io::stdout(), report.to_string().as_bytes());
            if !report.errors.is_empty() {
                std::process::exit(1);
            }
        }
        Some("build") => {
            let out = args.get(2).map(PathBuf::from).unwrap_or_else(|| dir.join("build"));
            let (_, report) = riptide_sheets::build(&dir, &out, &GameAssets::open())?;
            print!("{report}");
            println!("wrote {}/sheets.json, preflight.txt, sheets.rs", out.display());
        }
        _ => bail!("usage: sheets check [dir] | sheets build [dir] [out]"),
    }
    Ok(())
}
