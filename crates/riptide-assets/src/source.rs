//! Random-access byte sources behind the archive readers.
//!
//! On the desktop a source is the memory-mapped file. In the browser the game files stay on the
//! player's disk: a [`SparseSource`] holds only the byte ranges fetched so far, and a read outside
//! them is queued (see [`SparseSource::take_misses`]) for the page to fetch.

use std::collections::BTreeMap;
use std::sync::{Mutex, RwLock};

pub trait Source: Send + Sync {
    /// `len` bytes at `off`, or `None` when they aren't available (out of range, or not fetched).
    fn read(&self, off: u64, len: usize) -> Option<&[u8]>;
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Reads have failed only because their bytes aren't fetched yet (sparse sources).
    fn pending(&self) -> bool {
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct MapSource(pub memmap2::Mmap);

#[cfg(not(target_arch = "wasm32"))]
impl MapSource {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        // SAFETY: read-only game data that nothing rewrites while we run.
        Ok(Self(unsafe { memmap2::Mmap::map(&file)? }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Source for MapSource {
    fn read(&self, off: u64, len: usize) -> Option<&[u8]> {
        let o = usize::try_from(off).ok()?;
        self.0.get(o..o.checked_add(len)?)
    }
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
}

/// A file of known size of which only some byte ranges are present.
pub struct SparseSource {
    len: u64,
    /// Fetched ranges by (start, end); they may overlap. Append-only: a chunk is never removed
    /// or replaced, so a slice handed out by [`Source::read`] stays valid for the source's life.
    chunks: RwLock<BTreeMap<(u64, u64), Box<[u8]>>>,
    /// Longest chunk, bounding how far back a lookup scans.
    longest: std::sync::atomic::AtomicU64,
    misses: Mutex<Vec<(u64, u64)>>,
}

impl SparseSource {
    pub fn new(len: u64) -> Self {
        Self { len, chunks: RwLock::new(BTreeMap::new()), longest: 0.into(), misses: Mutex::new(Vec::new()) }
    }

    /// Add a fetched range. A range already fully present is ignored.
    pub fn insert(&self, off: u64, data: Vec<u8>) {
        if data.is_empty() || self.read_inner(off, data.len()).is_some() {
            return;
        }
        let end = off + data.len() as u64;
        self.longest.fetch_max(data.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.chunks.write().unwrap().entry((off, end)).or_insert_with(|| data.into_boxed_slice());
    }

    /// Missing ranges, coalesced and sorted. Nearby disc sectors share a file slice:
    /// their 304-byte raw-sector headers must not cause tens of thousands of browser reads.
    pub fn take_misses(&self) -> Vec<(u64, u64)> {
        let mut m = std::mem::take(&mut *self.misses.lock().unwrap());
        m.sort_unstable();
        let mut out: Vec<(u64, u64)> = Vec::new();
        for (a, b) in m {
            match out.last_mut() {
                Some(last) if a <= last.1 || (a.saturating_sub(last.1) <= 4096 && b.saturating_sub(last.0) <= 16 * 1024 * 1024) => last.1 = last.1.max(b),
                _ => out.push((a, b)),
            }
        }
        out
    }

    pub fn has_misses(&self) -> bool {
        !self.misses.lock().unwrap().is_empty()
    }

    /// Bytes held in memory.
    pub fn resident(&self) -> u64 {
        self.chunks.read().unwrap().values().map(|c| c.len() as u64).sum()
    }

    fn read_inner(&self, off: u64, len: usize) -> Option<&[u8]> {
        let chunks = self.chunks.read().unwrap();
        let end = off.checked_add(len as u64)?;
        let reach = self.longest.load(std::sync::atomic::Ordering::Relaxed);
        // Chunks starting at or before `off`, nearest first; one starting further back than the
        // longest chunk can't reach `off`.
        let ((start, _), chunk) = chunks
            .range(..=(off, u64::MAX))
            .rev()
            .take_while(|((s, _), _)| off - s <= reach)
            .find(|((_, e), _)| *e >= end)?;
        let at = usize::try_from(off - start).ok()?;
        let slice = chunk.get(at..at + len)?;
        // SAFETY: chunks are boxed and append-only (never removed, replaced or mutated), so the
        // bytes outlive the lock guard for as long as `self` lives.
        Some(unsafe { std::slice::from_raw_parts(slice.as_ptr(), slice.len()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_disc_sectors_are_read_together_without_losing_offsets() {
        let source = SparseSource::new(100_000);
        assert!(source.read(16,2048).is_none());
        assert!(source.read(2352+16,2048).is_none());
        assert_eq!(source.take_misses(),vec![(16,4416)]);
        let data: Vec<u8> = (16..4416).map(|i| (i%251) as u8).collect();
        source.insert(16,data);
        assert_eq!(source.read(2352+16,2048).unwrap()[0],(2368%251) as u8);
    }
}

impl Source for SparseSource {
    fn read(&self, off: u64, len: usize) -> Option<&[u8]> {
        if off.checked_add(len as u64)? > self.len {
            return None;
        }
        let hit = self.read_inner(off, len);
        if hit.is_none() {
            self.misses.lock().unwrap().push((off, off + len as u64));
        }
        hit
    }
    fn len(&self) -> u64 {
        self.len
    }
    fn pending(&self) -> bool {
        self.has_misses()
    }
}

/// Shared sources (the web loader keeps a handle to fill each sparse source it hands out).
impl<T: Source + ?Sized> Source for std::sync::Arc<T> {
    fn read(&self, off: u64, len: usize) -> Option<&[u8]> {
        (**self).read(off, len)
    }
    fn len(&self) -> u64 {
        (**self).len()
    }
    fn pending(&self) -> bool {
        (**self).pending()
    }
}
