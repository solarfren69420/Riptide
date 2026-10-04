//! Hydro Thunder (Dreamcast) `.KAT` sound banks: one per course, per boat (`RADH.KAT`, engine
//! `E_RADH.KAT`) and shared (`COMM`, `GAME`).
//!
//! `u32 count`, then `count` entries of 44 bytes: `{u32 type (1), u32 offset, u32 size,
//! u32 sample rate, u32 loop, u32 bits, 5 x u32 0}`. Data is the AICA sound chip's 4-bit ADPCM
//! (`bits` 4: Yamaha ADPCM, low nibble first) or 16-bit little-endian PCM (`bits` 16).

use anyhow::{bail, Result};

#[derive(Debug, Clone)]
pub struct KatSample {
    pub offset: usize,
    pub size: usize,
    pub rate: u32,
    pub looped: bool,
    pub bits: u32,
}

impl KatSample {
    pub fn seconds(&self) -> f32 {
        let samples = if self.bits == 4 { self.size * 2 } else if self.bits == 16 { self.size / 2 } else { self.size };
        samples as f32 / self.rate.max(1) as f32
    }
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

pub fn parse(bank: &[u8]) -> Result<Vec<KatSample>> {
    let count = u32_at(bank, 0).unwrap_or(0) as usize;
    if count > 4096 {
        bail!("implausible KAT sample count {count}");
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = 4 + i * 44;
        let (Some(offset), Some(size), Some(rate), Some(looped), Some(bits)) =
            (u32_at(bank, e + 4), u32_at(bank, e + 8), u32_at(bank, e + 12), u32_at(bank, e + 16), u32_at(bank, e + 20))
        else {
            bail!("KAT entry {i} runs past the bank");
        };
        out.push(KatSample { offset: offset as usize, size: size as usize, rate, looped: looped != 0, bits });
    }
    Ok(out)
}

/// The AICA's 4-bit ADPCM (the Yamaha YMZ280B scheme): each nibble moves the signal by a
/// multiple of a step that grows or shrinks with the nibble's size.
pub fn decode_adpcm(data: &[u8]) -> Vec<i16> {
    const DIFF: [i32; 8] = [1, 3, 5, 7, 9, 11, 13, 15];
    const SCALE: [i32; 8] = [230, 230, 230, 230, 307, 409, 512, 614];
    let (mut signal, mut step) = (0i32, 127i32);
    let mut out = Vec::with_capacity(data.len() * 2);
    for &byte in data {
        for nibble in [byte & 0x0f, byte >> 4] {
            let n = nibble as usize & 7;
            let delta = step * DIFF[n] / 8;
            signal = if nibble & 8 != 0 { signal - delta } else { signal + delta };
            signal = signal.clamp(-32768, 32767);
            step = (step * SCALE[n] / 256).clamp(127, 24576);
            out.push(signal as i16);
        }
    }
    out
}

/// One sample as 16-bit PCM.
pub fn decode(bank: &[u8], s: &KatSample) -> Option<Vec<i16>> {
    let data = bank.get(s.offset..s.offset + s.size)?;
    Some(match s.bits {
        4 => decode_adpcm(data),
        16 => data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
        8 => data.iter().map(|&b| ((b as i8) as i16) << 8).collect(),
        _ => return None,
    })
}

/// A mono 16-bit WAV file for `pcm` at `rate`.
pub fn wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data = pcm.len() * 2;
    let mut w = Vec::with_capacity(44 + data);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data as u32).to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}
