//! Hydro Thunder (Dreamcast) asset access: the GD-ROM, its R2 archives, boats and tracks.
//!
//! Names come from the `ht_boats` / `ht_tracks` sheets; this module only reads the disc.

use crate::gdi::GdRom;
use crate::htgeom::decode_geometry;
use crate::httrack::{decode_track, HtTrack};
use crate::image::RgbaImage;
use crate::model::Model;
use crate::pvr::decode_pvrt_in;
use crate::r2::R2Archive;
use anyhow::{Context, Result};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::{Arc, RwLock};

pub struct HydroThunder {
    pub disc: GdRom,
    pub main: R2Archive,
    /// The loaded track's own archive; searched before `main` for geometry and textures.
    track: RwLock<Option<Arc<R2Archive>>>,
}

impl HydroThunder {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(gdi: &Path) -> Result<Self> {
        Self::from_disc(GdRom::open(gdi)?)
    }

    pub fn from_disc(disc: GdRom) -> Result<Self> {
        let main = R2Archive::parse(disc.read("HYDRODC.R2").context("read HYDRODC.R2")?)?;
        Ok(Self { disc, main, track: RwLock::new(None) })
    }

    /// Disc reads are waiting on bytes not fetched yet (browser build).
    pub fn pending(&self) -> bool {
        self.disc.pending()
    }

    /// Menu scenes use the main archive's course assets rather than the previous race's overrides.
    pub fn clear_track(&self) {
        if let Ok(mut track) = self.track.write() { *track = None; }
    }

    fn archives(&self) -> Vec<Arc<R2Archive>> {
        self.track.read().ok().and_then(|t| t.clone()).into_iter().collect()
    }

    pub fn geometry(&self, name: &str) -> Result<Model> {
        for a in self.archives() {
            if a.contains(name) {
                return decode_geometry(&a.object(name)?);
            }
        }
        decode_geometry(&self.main.object(name)?)
    }

    pub fn texture(&self, name: &str) -> Result<RgbaImage> {
        for a in self.archives() {
            if let Some(raw) = a.raw(name) {
                return decode_pvrt_in(raw);
            }
        }
        decode_pvrt_in(self.main.raw(name).with_context(|| format!("{name} not in HYDRODC.R2 or the track"))?)
    }

    /// Load a track: `file` is its `.R2` on the disc, `entry` the `H*` object inside. The
    /// track's archive stays loaded for [`Self::geometry`] / [`Self::texture`].
    pub fn load_track(&self, file: &str, entry: &str) -> Result<HtTrack> {
        let archive = Arc::new(R2Archive::parse(self.disc.read(file).with_context(|| format!("read {file}"))?)?);
        let track = decode_track(&archive.object(entry)?).with_context(|| format!("{file}:{entry}"))?;
        if let Ok(mut t) = self.track.write() {
            *t = Some(archive);
        }
        Ok(track)
    }

    /// Does the disc have this file with this entry?
    pub fn has_track(&self, file: &str, entry: &str) -> bool {
        self.disc.read(file).ok().and_then(|d| R2Archive::parse(d).ok()).is_some_and(|a| a.contains(entry))
    }
}
