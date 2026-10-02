//! Readers for H2Overdrive (`triton.lux`) and Hydro Thunder (Dreamcast GD-ROM) assets.

pub mod fsb;
pub mod gdi;
pub mod h2coll;
pub mod h2level;
pub mod h2mesh;
pub mod ht;
pub mod htgeom;
pub mod httrack;
pub mod image;
pub mod lux;
pub mod model;
pub mod pvr;
pub mod r2;

use std::path::PathBuf;

/// Default asset locations, overridable with `RIPTIDE_LUX` and `RIPTIDE_GDI`.
pub fn default_lux_path() -> PathBuf {
    std::env::var_os("RIPTIDE_LUX").map(PathBuf::from).unwrap_or_else(|| home().join("MEGA downloads/TRITON/TRITON/triton.lux"))
}

pub fn default_gdi_path() -> PathBuf {
    std::env::var_os("RIPTIDE_GDI")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Games/Dreamcast/Hydro Thunder (USA)/disc.gdi"))
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}
