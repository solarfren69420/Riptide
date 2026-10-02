//! FMOD FSB4 sound banks (`fbnk.*` in `triton.lux`).
//!
//! Header (48 bytes): `"FSB4"`, `i32 samples`, `i32 sample-header bytes`, `i32 data bytes`,
//! `u32 version`, `u32 flags`, 8 zero bytes, 16-byte hash. Each sample header (80 bytes):
//! `u16 size`, `char name[30]`, `u32 length (samples)`, `u32 data bytes`, `u32 loop start`,
//! `u32 loop end`, `u32 mode`, `i32 frequency`, `u16 volume`, `i16 pan`, `u16 priority`,
//! `u16 channels`, ... Sample data follows the headers, in header order.

use anyhow::{bail, Context, Result};

/// `mode` flags (FMOD 3/4 `FSOUND_*`).
pub const LOOP_NORMAL: u32 = 0x0000_0002;
pub const BITS16: u32 = 0x0000_0010;
pub const MPEG: u32 = 0x0000_0200;
pub const IMAADPCM: u32 = 0x0040_0000;

#[derive(Debug, Clone)]
pub struct FsbSample {
    pub name: String,
    pub samples: u32,
    pub mode: u32,
    pub frequency: u32,
    pub channels: u16,
    pub loop_start: u32,
    pub loop_end: u32,
    /// Byte range of the encoded data inside the bank.
    pub offset: usize,
    pub size: usize,
}

impl FsbSample {
    pub fn looped(&self) -> bool {
        self.mode & LOOP_NORMAL != 0
    }
    pub fn codec(&self) -> &'static str {
        if self.mode & MPEG != 0 {
            "mpeg"
        } else if self.mode & IMAADPCM != 0 {
            "ima-adpcm"
        } else if self.mode & BITS16 != 0 {
            "pcm16"
        } else {
            "pcm8"
        }
    }
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}

pub fn parse(bank: &[u8]) -> Result<Vec<FsbSample>> {
    if bank.len() < 48 || &bank[..4] != b"FSB4" {
        bail!("not an FSB4 bank");
    }
    let count = u32_at(bank, 4) as usize;
    let headers = u32_at(bank, 8) as usize;
    let mut data = 48 + headers;
    let mut out = Vec::with_capacity(count);
    let mut h = 48;
    for _ in 0..count {
        if h + 64 > bank.len() {
            bail!("sample header out of range");
        }
        let size = u16_at(bank, h) as usize;
        let name_bytes = &bank[h + 2..h + 32];
        let name = String::from_utf8_lossy(&name_bytes[..name_bytes.iter().position(|&b| b == 0).unwrap_or(30)]).to_string();
        let bytes = u32_at(bank, h + 36) as usize;
        out.push(FsbSample {
            name,
            samples: u32_at(bank, h + 32),
            loop_start: u32_at(bank, h + 40),
            loop_end: u32_at(bank, h + 44),
            mode: u32_at(bank, h + 48),
            frequency: u32_at(bank, h + 52),
            channels: u16_at(bank, h + 62),
            offset: data,
            size: bytes,
        });
        data += bytes;
        h += size.max(64);
    }
    if data > bank.len() {
        bail!("sample data runs past the bank ({data} > {})", bank.len());
    }
    Ok(out)
}

/// A playable file for one sample: MPEG data as-is (`.mp3`), PCM wrapped in a WAV header.
pub fn extract(bank: &[u8], s: &FsbSample) -> Result<(Vec<u8>, &'static str)> {
    let data = bank.get(s.offset..s.offset + s.size).context("sample data out of range")?;
    match s.codec() {
        "mpeg" => Ok((data.to_vec(), "mp3")),
        "pcm16" | "pcm8" => {
            let bits: u16 = if s.mode & BITS16 != 0 { 16 } else { 8 };
            let ch = s.channels.max(1);
            let mut w = Vec::with_capacity(44 + data.len());
            let rate = s.frequency;
            let align = ch * bits / 8;
            w.extend_from_slice(b"RIFF");
            w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
            w.extend_from_slice(b"WAVEfmt ");
            w.extend_from_slice(&16u32.to_le_bytes());
            w.extend_from_slice(&1u16.to_le_bytes());
            w.extend_from_slice(&ch.to_le_bytes());
            w.extend_from_slice(&rate.to_le_bytes());
            w.extend_from_slice(&(rate * align as u32).to_le_bytes());
            w.extend_from_slice(&align.to_le_bytes());
            w.extend_from_slice(&bits.to_le_bytes());
            w.extend_from_slice(b"data");
            w.extend_from_slice(&(data.len() as u32).to_le_bytes());
            w.extend_from_slice(data);
            Ok((w, "wav"))
        }
        other => bail!("{}: codec {other} not supported", s.name),
    }
}
