//! Dreamcast PowerVR (`PVRT`) texture decoder.

use crate::image::RgbaImage;
use anyhow::{bail, Context, Result};

/// Find and decode the first `PVRT` chunk in `blob`.
pub fn decode_pvrt_in(blob: &[u8]) -> Result<RgbaImage> {
    let p = blob.windows(4).position(|w| w == b"PVRT").context("no PVRT chunk")?;
    decode_pvrt(&blob[p..])
}

pub fn decode_pvrt(d: &[u8]) -> Result<RgbaImage> {
    if d.len() < 16 || &d[..4] != b"PVRT" {
        bail!("not a PVRT chunk");
    }
    let pixel = d[8];
    let format = d[9];
    let w = u16::from_le_bytes([d[12], d[13]]) as usize;
    let h = u16::from_le_bytes([d[14], d[15]]) as usize;
    let data = &d[16..];
    if w == 0 || h == 0 || w > 1024 || h > 1024 {
        bail!("bad PVR size {w}x{h}");
    }
    let mut out = vec![0u8; w * h * 4];
    let texel = |v: u16| -> [u8; 4] { convert(pixel, v) };
    let get16 = |o: usize| -> Option<u16> { data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) };

    match format {
        // Twiddled (square), twiddled + mipmaps, twiddled rectangle.
        1 | 2 | 0x0d | 0x12 => {
            let base = if format == 2 || format == 0x12 { mip_offset_16(w) } else { 0 };
            let s = w.min(h);
            for y in 0..h {
                for x in 0..w {
                    // Rectangles are a strip of twiddled squares along the long side.
                    let block = if w > h { x / s } else { y / s };
                    let idx = block * s * s + twiddle(x % s, y % s);
                    let v = get16(base + idx * 2).unwrap_or(0);
                    out[(y * w + x) * 4..][..4].copy_from_slice(&texel(v));
                }
            }
        }
        // VQ, VQ + mipmaps, small VQ.
        3 | 4 | 0x10 | 0x11 => {
            let entries = if format == 0x10 || format == 0x11 { small_vq_entries(w, format == 0x11) } else { 256 };
            let cb = entries * 8;
            let base = cb + if format == 4 || format == 0x11 { vq_mip_offset(w) } else { 0 };
            for y in 0..h {
                for x in 0..w {
                    let bi = twiddle(x / 2, y / 2);
                    let code = *data.get(base + bi).unwrap_or(&0) as usize;
                    let sub = ((x & 1) << 1) | (y & 1);
                    let v = get16(code * 8 + sub * 2).unwrap_or(0);
                    out[(y * w + x) * 4..][..4].copy_from_slice(&texel(v));
                }
            }
        }
        // Linear rectangle, linear with stride.
        9 | 0x0b => {
            for i in 0..w * h {
                let v = get16(i * 2).unwrap_or(0);
                out[i * 4..][..4].copy_from_slice(&texel(v));
            }
        }
        // Palettised: the palette lives in PVR registers, not the file. Show luminance.
        5 | 6 | 7 | 8 => {
            let four = format == 5 || format == 6;
            for y in 0..h {
                for x in 0..w {
                    let t = twiddle(x, y);
                    let v = if four {
                        let b = *data.get(t / 2).unwrap_or(&0);
                        (if t & 1 == 0 { b & 15 } else { b >> 4 }) * 17
                    } else {
                        *data.get(t).unwrap_or(&0)
                    };
                    out[(y * w + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        other => bail!("unsupported PVR data format {other:#x}"),
    }
    if pixel == 3 {
        yuv_fixup(&mut out, w, h, format, data);
    }
    Ok(RgbaImage { width: w as u32, height: h as u32, rgba: out })
}

/// Morton order with the X bits in odd positions, as the PowerVR TA expects.
pub fn twiddle(x: usize, y: usize) -> usize {
    let mut r = 0;
    for i in 0..11 {
        r |= ((y >> i) & 1) << (2 * i);
        r |= ((x >> i) & 1) << (2 * i + 1);
    }
    r
}

fn mip_offset_16(w: usize) -> usize {
    // 1x1 sits at 6 bytes; each larger level follows the previous one.
    let mut off = 6;
    let mut s = 1;
    while s < w {
        off += s * s * 2;
        s *= 2;
    }
    off
}

fn vq_mip_offset(w: usize) -> usize {
    let mut off = 1;
    let mut s = 1;
    if w == 1 {
        return 0;
    }
    while s < w / 2 {
        off += s * s;
        s *= 2;
    }
    off
}

fn small_vq_entries(w: usize, mip: bool) -> usize {
    match (w, mip) {
        (..=8, _) => 16,
        (16, false) => 32,
        (16, true) => 16,
        (32, false) => 128,
        (32, true) => 64,
        _ => 256,
    }
}

fn convert(pixel: u8, v: u16) -> [u8; 4] {
    let v = v as u32;
    match pixel {
        0 => {
            let a = if v & 0x8000 != 0 { 255 } else { 0 };
            [scale(v >> 10 & 31, 31), scale(v >> 5 & 31, 31), scale(v & 31, 31), a]
        }
        1 => [scale(v >> 11 & 31, 31), scale(v >> 5 & 63, 63), scale(v & 31, 31), 255],
        2 => [scale(v >> 8 & 15, 15), scale(v >> 4 & 15, 15), scale(v & 15, 15), scale(v >> 12 & 15, 15)],
        // YUV is fixed up afterwards; keep the raw bytes for now.
        3 => [(v & 0xff) as u8, (v >> 8) as u8, 0, 255],
        _ => {
            let l = (v >> 8) as u8;
            [l, l, l, 255]
        }
    }
}

fn scale(v: u32, max: u32) -> u8 {
    (v * 255 / max) as u8
}

/// YUV422: each horizontal texel pair shares U and V. Raw texels are stored as (Y, U|V).
fn yuv_fixup(out: &mut [u8], w: usize, h: usize, _format: u8, _data: &[u8]) {
    for y in 0..h {
        for x in (0..w.saturating_sub(1)).step_by(2) {
            let a = (y * w + x) * 4;
            let b = a + 4;
            let (y0, u) = (out[a + 1] as f32, out[a] as f32 - 128.0);
            let (y1, v) = (out[b + 1] as f32, out[b] as f32 - 128.0);
            for (o, yy) in [(a, y0), (b, y1)] {
                let r = yy + 1.375 * v;
                let g = yy - 0.34375 * u - 0.6875 * v;
                let bl = yy + 1.71875 * u;
                out[o] = r.clamp(0.0, 255.0) as u8;
                out[o + 1] = g.clamp(0.0, 255.0) as u8;
                out[o + 2] = bl.clamp(0.0, 255.0) as u8;
                out[o + 3] = 255;
            }
        }
    }
}
