//! Dreamcast GD-ROM images in `.gdi` form, read through the ISO 9660 filesystem in the
//! high-density area (which starts at LBA 45000).

use crate::source::Source;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

struct Track {
    start_lba: u32,
    sector_size: usize,
    src: Box<dyn Source>,
}

#[derive(Debug, Clone, Copy)]
pub struct DiscFile {
    pub lba: u32,
    pub size: u32,
}

pub struct GdRom {
    tracks: Vec<Track>,
    files: BTreeMap<String, DiscFile>,
}

const HIGH_DENSITY_LBA: u32 = 45000;

impl GdRom {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open(gdi: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(gdi).with_context(|| format!("read {}", gdi.display()))?;
        let dir = gdi.parent().unwrap_or(std::path::Path::new("."));
        Self::from_parts(&text, |file| Ok(Box::new(crate::source::MapSource::open(&dir.join(file))?) as Box<dyn Source>))
    }

    /// Track files a `.gdi` names, in order: `(file name, start LBA, sector size)`.
    pub fn gdi_tracks(text: &str) -> Vec<(String, u32, usize)> {
        text.lines()
            .skip(1)
            .map(split_gdi_line)
            .filter(|p| p.len() >= 5)
            .filter_map(|p| Some((p[4].clone(), p[1].parse().ok()?, p[3].parse().ok()?)))
            .collect()
    }

    /// Open from the `.gdi` text and a source per track file (by the name the `.gdi` gives).
    pub fn from_parts(text: &str, mut open: impl FnMut(&str) -> Result<Box<dyn Source>>) -> Result<Self> {
        let mut tracks = Vec::new();
        for (file, start_lba, sector_size) in Self::gdi_tracks(text) {
            tracks.push(Track { start_lba, sector_size, src: open(&file)? });
        }
        tracks.sort_by_key(|t| t.start_lba);
        let mut rom = Self { tracks, files: BTreeMap::new() };
        let pvd = rom.sector(HIGH_DENSITY_LBA + 16)?;
        if &pvd[1..6] != b"CD001" {
            bail!("no ISO 9660 volume in the high-density area");
        }
        let root = pvd[156..156 + 34].to_vec();
        let lba = u32::from_le_bytes(root[2..6].try_into().unwrap());
        let size = u32::from_le_bytes(root[10..14].try_into().unwrap());
        rom.walk(lba, size, "", 0)?;
        Ok(rom)
    }

    fn sector(&self, lba: u32) -> Result<&[u8]> {
        let t = self
            .tracks
            .iter()
            .rev()
            .find(|t| t.start_lba <= lba)
            .context("LBA before first track")?;
        let header = if t.sector_size == 2352 { 16 } else { 0 };
        let o = (lba - t.start_lba) as usize * t.sector_size + header;
        t.src.read(o as u64, 2048).context("sector past end of track or not fetched")
    }

    fn walk(&mut self, lba: u32, size: u32, prefix: &str, depth: u32) -> Result<()> {
        if depth > 8 {
            return Ok(());
        }
        let data = self.read_extent(lba, size)?;
        let mut i = 0usize;
        let mut subdirs = Vec::new();
        while i < data.len() {
            let len = data[i] as usize;
            if len == 0 {
                i = (i / 2048 + 1) * 2048;
                continue;
            }
            let e = &data[i..(i + len).min(data.len())];
            if e.len() < 34 {
                break;
            }
            let elba = u32::from_le_bytes(e[2..6].try_into().unwrap());
            let esize = u32::from_le_bytes(e[10..14].try_into().unwrap());
            let flags = e[25];
            let nlen = e[32] as usize;
            let raw = &e[33..(33 + nlen).min(e.len())];
            if raw != [0] && raw != [1] {
                let name = String::from_utf8_lossy(raw);
                let name = name.split(';').next().unwrap_or("").to_ascii_uppercase();
                let path = format!("{prefix}{name}");
                if flags & 2 != 0 {
                    subdirs.push((elba, esize, format!("{path}/")));
                } else {
                    self.files.insert(path, DiscFile { lba: elba, size: esize });
                }
            }
            i += len;
        }
        for (l, s, p) in subdirs {
            self.walk(l, s, &p, depth + 1)?;
        }
        Ok(())
    }

    fn read_extent(&self, lba: u32, size: u32) -> Result<Vec<u8>> {
        let n = (size as usize).div_ceil(2048);
        let mut out = Vec::with_capacity(n * 2048);
        // Touch every sector even after one is missing: a sparse source then queues the whole
        // extent in one go instead of one sector per fetch round.
        let mut missing = None;
        for k in 0..n as u32 {
            match self.sector(lba + k) {
                Ok(s) if missing.is_none() => out.extend_from_slice(s),
                Ok(_) => {}
                Err(e) => missing = missing.or(Some(e)),
            }
        }
        if let Some(e) = missing {
            return Err(e);
        }
        out.truncate(size as usize);
        Ok(out)
    }

    /// Some sector reads failed only because their bytes aren't fetched yet (browser build).
    pub fn pending(&self) -> bool {
        self.tracks.iter().any(|t| t.src.pending())
    }

    pub fn files(&self) -> impl Iterator<Item = (&str, &DiscFile)> {
        self.files.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Read a file by name (case-insensitive, no leading slash needed).
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let key = name.trim_start_matches('/').to_ascii_uppercase();
        let f = self.files.get(&key).with_context(|| format!("{name} not on disc"))?;
        self.read_extent(f.lba, f.size)
    }
}

fn split_gdi_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
