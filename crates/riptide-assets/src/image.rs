//! Decoded RGBA8 images plus the DDS reader for H2Overdrive `txtr1.*` entries.

use anyhow::{bail, Result};

#[derive(Debug, Clone)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, top row first.
    pub rgba: Vec<u8>,
}

impl RgbaImage {
    pub fn has_alpha(&self) -> bool {
        self.rgba.chunks_exact(4).any(|p| p[3] < 250)
    }

    /// True when alpha is only ever ~0 or ~255 (cut-out foliage, fences).
    pub fn alpha_is_binary(&self) -> bool {
        self.rgba.chunks_exact(4).all(|p| p[3] < 8 || p[3] > 247)
    }
}

/// `txtr1.*` blob: a 0x1c-byte TXTR header followed by a complete DDS file.
pub fn decode_txtr(blob: &[u8]) -> Result<RgbaImage> {
    let dds = blob
        .windows(4)
        .take(0x40)
        .position(|w| w == b"DDS ")
        .map(|p| &blob[p..])
        .ok_or_else(|| anyhow::anyhow!("no DDS payload"))?;
    decode_dds(dds)
}

pub fn decode_dds(d: &[u8]) -> Result<RgbaImage> {
    if d.len() < 128 || &d[..4] != b"DDS " {
        bail!("not a DDS file");
    }
    let rd = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let height = rd(12);
    let width = rd(16);
    let pf_flags = rd(80);
    let fourcc = &d[84..88];
    let bits = rd(88);
    let masks = [rd(92), rd(96), rd(100), rd(104)];
    let data = &d[128..];
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        bail!("bad DDS size {width}x{height}");
    }
    let mut out = vec![0u8; (width * height * 4) as usize];
    if pf_flags & 4 != 0 {
        match fourcc {
            b"DXT1" => decode_bc(data, width, height, &mut out, Bc::Dxt1)?,
            b"DXT2" | b"DXT3" => decode_bc(data, width, height, &mut out, Bc::Dxt3)?,
            b"DXT4" | b"DXT5" => decode_bc(data, width, height, &mut out, Bc::Dxt5)?,
            other => bail!("unsupported DDS fourcc {:?}", String::from_utf8_lossy(other)),
        }
    } else {
        let bpp = (bits / 8) as usize;
        if bpp == 0 || bpp > 4 {
            bail!("unsupported DDS bit count {bits}");
        }
        let need = width as usize * height as usize * bpp;
        if data.len() < need {
            bail!("DDS data truncated");
        }
        let has_alpha = pf_flags & 1 != 0;
        let luminance = pf_flags & 0x20000 != 0;
        let alpha_only = pf_flags & 2 != 0 && pf_flags & 0x40 == 0 && !luminance;
        for i in 0..(width * height) as usize {
            let mut v = 0u32;
            for b in 0..bpp {
                v |= (data[i * bpp + b] as u32) << (8 * b);
            }
            let px = &mut out[i * 4..i * 4 + 4];
            if alpha_only {
                px.copy_from_slice(&[255, 255, 255, extract(v, masks[3])]);
            } else if luminance {
                let l = extract(v, masks[0]);
                let a = if has_alpha { extract(v, masks[3]) } else { 255 };
                px.copy_from_slice(&[l, l, l, a]);
            } else {
                let a = if has_alpha { extract(v, masks[3]) } else { 255 };
                px.copy_from_slice(&[
                    extract(v, masks[0]),
                    extract(v, masks[1]),
                    extract(v, masks[2]),
                    a,
                ]);
            }
        }
    }
    Ok(RgbaImage { width, height, rgba: out })
}

fn extract(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 255;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    (((v & mask) >> shift) * 255 / max) as u8
}

#[derive(Clone, Copy, PartialEq)]
enum Bc {
    Dxt1,
    Dxt3,
    Dxt5,
}

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]
}

fn decode_bc(data: &[u8], w: u32, h: u32, out: &mut [u8], kind: Bc) -> Result<()> {
    let bw = w.div_ceil(4) as usize;
    let bh = h.div_ceil(4) as usize;
    let block = if kind == Bc::Dxt1 { 8 } else { 16 };
    if data.len() < bw * bh * block {
        bail!("DXT data truncated");
    }
    for by in 0..bh {
        for bx in 0..bw {
            let b = &data[(by * bw + bx) * block..][..block];
            let (alpha, color) = if kind == Bc::Dxt1 { (None, b) } else { (Some(&b[..8]), &b[8..]) };
            let c0 = u16::from_le_bytes([color[0], color[1]]);
            let c1 = u16::from_le_bytes([color[2], color[3]]);
            let p0 = rgb565(c0);
            let p1 = rgb565(c1);
            let mut pal = [[0u8; 4]; 4];
            pal[0] = [p0[0], p0[1], p0[2], 255];
            pal[1] = [p1[0], p1[1], p1[2], 255];
            let mix = |a: u8, b: u8, wa: u32, wb: u32| ((a as u32 * wa + b as u32 * wb) / (wa + wb)) as u8;
            if c0 > c1 || kind != Bc::Dxt1 {
                for k in 0..3 {
                    pal[2][k] = mix(p0[k], p1[k], 2, 1);
                    pal[3][k] = mix(p0[k], p1[k], 1, 2);
                }
                pal[2][3] = 255;
                pal[3][3] = 255;
            } else {
                for k in 0..3 {
                    pal[2][k] = mix(p0[k], p1[k], 1, 1);
                }
                pal[2][3] = 255;
                pal[3] = [0, 0, 0, 0];
            }
            let bits = u32::from_le_bytes(color[4..8].try_into().unwrap());
            let alphas: [u8; 16] = match (kind, alpha) {
                (Bc::Dxt3, Some(a)) => {
                    let v = u64::from_le_bytes(a.try_into().unwrap());
                    std::array::from_fn(|i| (((v >> (4 * i)) & 15) as u8) * 17)
                }
                (Bc::Dxt5, Some(a)) => {
                    let a0 = a[0] as u32;
                    let a1 = a[1] as u32;
                    let mut ap = [0u8; 8];
                    ap[0] = a0 as u8;
                    ap[1] = a1 as u8;
                    if a0 > a1 {
                        for i in 1..7 {
                            ap[i + 1] = ((a0 * (7 - i as u32) + a1 * i as u32) / 7) as u8;
                        }
                    } else {
                        for i in 1..5 {
                            ap[i + 1] = ((a0 * (5 - i as u32) + a1 * i as u32) / 5) as u8;
                        }
                        ap[6] = 0;
                        ap[7] = 255;
                    }
                    let mut v = 0u64;
                    for (i, &x) in a[2..8].iter().enumerate() {
                        v |= (x as u64) << (8 * i);
                    }
                    std::array::from_fn(|i| ap[((v >> (3 * i)) & 7) as usize])
                }
                _ => [255; 16],
            };
            for py in 0..4 {
                for px in 0..4 {
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= w as usize || y >= h as usize {
                        continue;
                    }
                    let i = py * 4 + px;
                    let mut c = pal[((bits >> (2 * i)) & 3) as usize];
                    if kind != Bc::Dxt1 {
                        c[3] = alphas[i];
                    }
                    out[(y * w as usize + x) * 4..][..4].copy_from_slice(&c);
                }
            }
        }
    }
    Ok(())
}
