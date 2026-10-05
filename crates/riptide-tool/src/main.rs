//! `riptide-tool`: inspect and export H2Overdrive / Hydro Thunder assets.
//!
//! ```text
//! riptide-tool lux-list [filter]          riptide-tool ht-list [filter]
//! riptide-tool lux-cat <kind.name>        (raw entry bytes to stdout)
//! riptide-tool lux-tex <name> <out.png>   riptide-tool ht-tex <name> <out.png>
//! riptide-tool lux-mesh <name> <out.obj>  riptide-tool ht-geom <name> <out.obj>
//! riptide-tool lux-render <name> <out.png> [yaw]
//! riptide-tool ht-render <name> <out.png> [yaw]
//! riptide-tool coll <code>               (collision triangle count + bounds vs visible terrain)
//! riptide-tool level <code> [out.png]     (summary + top-down render of the track)
//! riptide-tool seed-sheets <dir>        (dump game tables to raw CSV sheets: <dir>/raw_*.csv)
//! ```

use anyhow::{bail, Context, Result};
use riptide_assets::image::RgbaImage;
use riptide_assets::model::Model;
use riptide_assets::{h2coll, h2level, h2mesh, ht, image, lux};
use std::collections::HashMap;
use std::path::Path;

mod hackworld;
mod seed;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a = |i: usize| args.get(i).map(String::as_str).context("missing argument");
    match args.first().map(String::as_str) {
        Some("lux-list") => {
            let l = open_lux()?;
            for e in l.entries() {
                if args.len() < 2 || e.name.contains(a(1)?) {
                    println!("{:>10} {}", e.size, e.name);
                }
            }
        }
        Some("lux-cat") => {
            let l = open_lux()?;
            let b = l.get(a(1)?).context("no such entry")?;
            std::io::Write::write_all(&mut std::io::stdout(), b)?;
        }
        Some("lux-tex") => {
            let l = open_lux()?;
            let img = image::decode_txtr(l.get(&format!("txtr1.{}", a(1)?)).context("no such texture")?)?;
            write_png(Path::new(a(2)?), &img)?;
        }
        Some("rig-render") => {
            // rig-render <mesh> <clip> <seconds> out.png [yaw]: the rigged mesh posed by the clip.
            let l = open_lux()?;
            let name = a(1)?;
            let rigged = h2mesh::decode_rigged(name, l.get(&format!("mesh32.{name}")).context("no such mesh")?)?;
            let clip = riptide_assets::h2anim::decode_anim(a(2)?, l.get(&format!("anim4.{}", a(2)?)).context("no such clip")?)?;
            let t: f32 = a(3)?.parse()?;
            let m = riptide_assets::h2anim::pose(&rigged, &clip, t);
            let mut tex = HashMap::new();
            for p in &m.parts {
                if let Some(t) = &p.texture {
                    if let Some(img) = l.get(&format!("txtr1.{t}")).and_then(|b| image::decode_txtr(b).ok()) {
                        tex.insert(t.clone(), img);
                    }
                }
            }
            let yaw: f32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(35.0);
            write_png(Path::new(a(4)?), &render(&m, &tex, yaw, 25.0, 768))?;
        }
        Some("anim") => {
            // anim <clip>: decoded anim4 clip: per track, key counts and first/last values.
            let l = open_lux()?;
            let name = a(1)?;
            let c = riptide_assets::h2anim::decode_anim(name, l.get(&format!("anim4.{name}")).context("no such clip")?)?;
            println!("{}: {:.3} s ({:.0} frames), {} tracks", c.name, c.duration, c.duration * riptide_assets::h2anim::FPS, c.tracks.len());
            for t in &c.tracks {
                let r0 = t.rot.first().map(|k| k.1);
                let r1 = t.rot.last().map(|k| k.1);
                let p0 = t.pos.first().map(|k| k.1);
                println!("  {:<26} pos {:>3} rot {:>3} scale {:>2}  pos0 {:?}  rot {:?} -> {:?}  scale {:?}", t.name, t.pos.len(), t.rot.len(), t.scale.len(), p0, r0, r1, t.scale);
            }
        }
        Some("rig-check") => {
            // rig-check <mesh>: rigged parts placed by their bones' rest matrices must match the
            // baked mesh; prints both bounds and the part/bone counts.
            let l = open_lux()?;
            let name = a(1)?;
            let blob = l.get(&format!("mesh32.{name}")).context("no such mesh")?;
            let baked = h2mesh::decode_mesh(name, blob)?;
            let rigged = h2mesh::decode_rigged(name, blob)?;
            let mut placed = rigged.clone();
            for p in &mut placed.parts {
                if let Some(b) = p.bone {
                    let m = rigged.bones[b as usize].rest;
                    for v in &mut p.positions {
                        let [x, y, z] = *v;
                        *v = [
                            m[0] * x + m[4] * y + m[8] * z + m[12],
                            m[1] * x + m[5] * y + m[9] * z + m[13],
                            m[2] * x + m[6] * y + m[10] * z + m[14],
                        ];
                    }
                }
            }
            println!("baked  {:?}", baked.bounds());
            println!("rigged {:?}  ({} parts, {} bones)", placed.bounds(), rigged.parts.len(), rigged.bones.len());
        }
        Some("rig") => {
            // rig <mesh>: the mesh's bones: index, name, the two header words, rest translation.
            let l = open_lux()?;
            let b = l.get(&format!("mesh32.{}", a(1)?)).context("no such mesh")?;
            let nfix = lux::u32_at(b, 8) as usize;
            let start = 0x14 + nfix * 0x2c;
            let mut ptrs = HashMap::new();
            for i in 0..nfix {
                let r = &b[0x14 + i * 0x2c..0x14 + (i + 1) * 0x2c];
                if lux::u32_at(r, 0) == 1 {
                    ptrs.insert(lux::u32_at(r, 4) as usize, lux::u32_at(r, 8) as usize);
                }
            }
            let body = &b[start..];
            let n = lux::u32_at(body, 0x40) as usize;
            let at = *ptrs.get(&0x4c).context("no bone table")?;
            let f = |o: usize| f32::from_le_bytes(body[o..o + 4].try_into().unwrap());
            for i in 0..n {
                let r = at + i * 0x170;
                let name = lux::cstr(&body[r + 4..r + 0x24]);
                let (w24, w28) = (lux::u32_at(body, r + 0x24), lux::u32_at(body, r + 0x28));
                let t = [f(r + 0x60 + 48), f(r + 0x60 + 52), f(r + 0x60 + 56)];
                println!("{i:2} {name:<28} {w24:#6x} {w28:#6x}  rest at [{:8.3} {:8.3} {:8.3}]", t[0], t[1], t[2]);
            }
        }
        Some("lux-fix") => {
            // lux-fix <entry>: body of a relocatable blob (mesh32, anim4, ...) as words, with
            // internal pointers shown as `->target` and external references by name.
            let l = open_lux()?;
            let b = l.get(a(1)?).context("no such entry")?;
            let nfix = lux::u32_at(b, 8) as usize;
            let start = 0x14 + nfix * 0x2c;
            let mut ptrs = HashMap::new();
            for i in 0..nfix {
                let r = &b[0x14 + i * 0x2c..0x14 + (i + 1) * 0x2c];
                let (kind, at, target) = (lux::u32_at(r, 0), lux::u32_at(r, 4) as usize, lux::u32_at(r, 8));
                let shown = if kind == 1 { format!("->{target:#x}") } else { format!("ext {}", lux::cstr(&r[0x0c..])) };
                ptrs.insert(at, shown);
            }
            let body = &b[start..];
            println!("{} fixups, body {:#x} bytes", nfix, body.len());
            for o in (0..body.len().saturating_sub(3)).step_by(4) {
                let w = lux::u32_at(body, o);
                let f = f32::from_bits(w);
                let shown = match ptrs.get(&o) {
                    Some(p) => p.clone(),
                    None if f.is_finite() && f.abs() > 1e-4 && f.abs() < 1e5 && w > 0x0100_0000 => format!("{f:.4}"),
                    None => format!("{w:#x}"),
                };
                println!("{o:#06x}: {shown}");
            }
        }
        Some("lux-mesh") => {
            let l = open_lux()?;
            let m = lux_model(&l, a(1)?)?;
            describe(&m);
            std::fs::write(a(2)?, m.to_obj())?;
        }
        Some("lux-render") => {
            let l = open_lux()?;
            let m = lux_model(&l, a(1)?)?;
            describe(&m);
            let mut tex = HashMap::new();
            for p in &m.parts {
                if let Some(t) = &p.texture {
                    if let Some(b) = l.get(&format!("txtr1.{t}")) {
                        if let Ok(img) = image::decode_txtr(b) {
                            tex.insert(t.clone(), img);
                        }
                    }
                }
            }
            let yaw: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(35.0);
            write_png(Path::new(a(2)?), &render(&m, &tex, yaw, 25.0, 768))?;
        }
        Some("h2o-track") => {
            // h2o-track <pid> [seconds]: log the running H2Overdrive player boat 30 times a second
            // (reads /proc/<pid>/mem, never pauses the game): time, x y z (game space), speed, state.
            // Player boat [0x793EB0]; position +0xA0..A8, speed +0x1394, state +0x588.
            use std::io::{Read, Seek, SeekFrom};
            let mut mem = std::fs::File::open(format!("/proc/{}/mem", a(1)?))?;
            let secs: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600.0);
            let mut read = |addr: u64, n: usize| -> Option<Vec<u8>> {
                let mut b = vec![0u8; n];
                mem.seek(SeekFrom::Start(addr)).ok()?;
                mem.read_exact(&mut b).ok()?;
                Some(b)
            };
            let t0 = std::time::Instant::now();
            while t0.elapsed().as_secs_f32() < secs {
                let boat = read(0x793EB0, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as u64).unwrap_or(0);
                if boat != 0 {
                    if let (Some(p), Some(s), Some(st)) = (read(boat + 0xa0, 12), read(boat + 0x1394, 4), read(boat + 0x588, 4)) {
                        let f = |b: &[u8], i: usize| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
                        println!("{:.3} {:.1} {:.1} {:.1} {:.1} {:x}", t0.elapsed().as_secs_f32(), f(&p, 0), f(&p, 1), f(&p, 2), f(&s, 0), u32::from_le_bytes(st.try_into().unwrap()));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(33));
            }
        }
        Some("h2o-find") => {
            // h2o-find <pid> <before> <after> <f32>...: find a float sequence (to within 1e-4) in the running
            // game's writable memory (/proc/<pid>/maps + mem, read only) and dump <before> bytes before and
            // <after> bytes after each hit as offset: hex float lines. Used to read the constant blocks
            // H2Overdrive feeds its water shader, found by an edge's known colours.
            use std::io::{Read, Seek, SeekFrom};
            let pid = a(1)?;
            let before: usize = a(2)?.parse()?;
            let after: usize = a(3)?.parse()?;
            let want: Vec<f32> = args[4..].iter().map(|s| s.parse()).collect::<Result<_, _>>()?;
            let maps = std::fs::read_to_string(format!("/proc/{pid}/maps"))?;
            let mut mem = std::fs::File::open(format!("/proc/{pid}/mem"))?;
            let mut hits = 0;
            for line in maps.lines() {
                let mut f = line.split_whitespace();
                let (Some(range), Some(perms)) = (f.next(), f.next()) else { continue };
                if !perms.starts_with("rw") {
                    continue;
                }
                let (lo, hi) = range.split_once('-').unwrap();
                let (lo, hi) = (u64::from_str_radix(lo, 16)?, u64::from_str_radix(hi, 16)?);
                if hi - lo > 1 << 30 {
                    continue;
                }
                let mut buf = vec![0u8; (hi - lo) as usize];
                if mem.seek(SeekFrom::Start(lo)).is_err() || mem.read_exact(&mut buf).is_err() {
                    continue;
                }
                let fl = |i: usize| f32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
                let n = want.len() * 4;
                let mut i = 0;
                while i + n <= buf.len() {
                    if (0..want.len()).all(|k| (fl(i + 4 * k) - want[k]).abs() < 1e-4) {
                        hits += 1;
                        println!("HIT at {:x}", lo + i as u64);
                        let start = i.saturating_sub(before) & !3;
                        let end = (i + n + after).min(buf.len());
                        let mut o = start;
                        while o + 4 <= end {
                            println!("  {:+06x} {:08x} {}", o as i64 - i as i64, u32::from_le_bytes(buf[o..o + 4].try_into().unwrap()), fl(o));
                            o += 4;
                        }
                        if hits >= 40 {
                            return Ok(());
                        }
                    }
                    i += 4;
                }
            }
            println!("{hits} hits");
        }
        Some("shader") => {
            // shader <name> [outdir]: the shader programs in shad4.<name> (H2Overdrive effects), their
            // constant tables and disassembly; with outdir, one .asm file per program.
            let l = open_lux()?;
            let blob = l.get(&format!("shad4.{}", a(1)?)).context("no such shader")?;
            for (i, p) in d3d9_shader::programs(blob).iter().enumerate() {
                let asm = d3d9_shader::disassemble(p);
                println!("{} #{i}: {} {}.{} at +0x{:x}, {} tokens, {} constants", a(1)?, if p.pixel { "ps" } else { "vs" }, p.major, p.minor, p.offset, p.tokens.len(), p.constants.len());
                match args.get(2) {
                    Some(dir) => std::fs::write(format!("{dir}/{}_{i}_{}.asm", a(1)?, if p.pixel { "ps" } else { "vs" }), &asm)?,
                    None => print!("{}{asm}", d3d9_shader::disasm::constant_listing(p)),
                }
            }
        }
        Some("shader-wgsl") => {
            // shader-wgsl <name> [outdir]: translate shad4.<name>'s programs to WGSL and validate them with
            // naga (the compiler wgpu / Bevy use).
            let l = open_lux()?;
            let blob = l.get(&format!("shad4.{}", a(1)?)).context("no such shader")?;
            let (mut ok, mut bad) = (0, 0);
            for (i, p) in d3d9_shader::programs(blob).iter().enumerate() {
                let src = d3d9_shader::wgsl::translate(p, &Default::default());
                let kind = if p.pixel { "ps" } else { "vs" };
                if let Some(dir) = args.get(2) {
                    std::fs::write(format!("{dir}/{}_{i}_{kind}.wgsl", a(1)?), &src)?;
                }
                let checked = naga::front::wgsl::parse_str(&src).map_err(|e| e.emit_to_string(&src)).and_then(|m| {
                    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
                        .validate(&m)
                        .map(|_| ())
                        .map_err(|e| e.emit_to_string(&src))
                });
                match checked {
                    Ok(()) => ok += 1,
                    Err(e) => {
                        bad += 1;
                        println!("{} #{i} {kind}: {}", a(1)?, e.lines().take(12).collect::<Vec<_>>().join("\n"));
                    }
                }
            }
            println!("{}: {ok} valid, {bad} invalid", a(1)?);
        }
        Some("kat") => {
            // kat <FILE.KAT> [outdir]: list a Hydro Thunder sound bank; with outdir, write each sample as WAV.
            let g = riptide_assets::gdi::GdRom::open(&riptide_assets::default_gdi_path())?;
            let bank = g.read(a(1)?)?;
            for (i, s) in riptide_assets::kat::parse(&bank)?.iter().enumerate() {
                println!("{} #{i}: {:.2} s {} Hz {}-bit{}", a(1)?, s.seconds(), s.rate, s.bits, if s.looped { " loop" } else { "" });
                if let (Ok(dir), Some(pcm)) = (a(2), riptide_assets::kat::decode(&bank, s)) {
                    std::fs::write(format!("{dir}/{}_{i}.wav", a(1)?.trim_end_matches(".KAT")), riptide_assets::kat::wav(&pcm, s.rate))?;
                }
            }
        }
        Some("ht-file") => {
            let g = riptide_assets::gdi::GdRom::open(&riptide_assets::default_gdi_path())?;
            if args.len() < 2 { for (n, f) in g.files() { eprintln!("{n} {f:?}"); } } else { use std::io::Write; std::io::stdout().write_all(&g.read(a(1)?)?)?; }
        }
        Some("ht-r2") => {
            // List the entries of any R2 on the disc: size, relocations, kind, name.
            let g = riptide_assets::gdi::GdRom::open(&riptide_assets::default_gdi_path())?;
            let r2 = riptide_assets::r2::R2Archive::parse(g.read(a(1)?)?)?;
            for e in r2.entries() {
                if args.len() < 3 || e.name.contains(a(2)?) {
                    let kind = r2.object(&e.name).map(|o| o.kind as char).unwrap_or('?');
                    println!("{:>9} {:>6} {kind} {}", e.size, e.relocations, e.name);
                }
            }
        }
        Some("ht-obj") => {
            // Dump one R2 entry body to a file and print its pointer table.
            let g = riptide_assets::gdi::GdRom::open(&riptide_assets::default_gdi_path())?;
            let r2 = riptide_assets::r2::R2Archive::parse(g.read(a(1)?)?)?;
            let o = r2.object(a(2)?)?;
            std::fs::write(a(3)?, o.body)?;
            println!("kind {} body {} bytes, {} internal, {} external", o.kind as char, o.body.len(), o.internal.len(), o.external.len());
            let mut ints = o.internal.clone();
            ints.sort();
            for s in &ints {
                println!("int {s:#x} -> {:#x}", o.u32(*s).unwrap_or(0));
            }
            for (s, n) in &o.external {
                println!("ext {s:#x} -> {n}");
            }
        }
        Some("ht-dump") => {
            // ht-dump FILE ENTRY OFF STRIDE COUNT: rows of u32 words, shown as float when plausible.
            let g = riptide_assets::gdi::GdRom::open(&riptide_assets::default_gdi_path())?;
            let r2 = riptide_assets::r2::R2Archive::parse(g.read(a(1)?)?)?;
            let o = r2.object(a(2)?)?;
            let num = |s: &str| usize::from_str_radix(s.trim_start_matches("0x"), if s.starts_with("0x") { 16 } else { 10 });
            let (off, stride, count) = (num(a(3)?)?, num(a(4)?)?, num(a(5)?)?);
            for i in 0..count {
                let b = off + i * stride;
                let mut line = format!("{i:>4} {b:#08x}:");
                for w in (0..stride).step_by(4) {
                    let Some(v) = o.u32(b + w) else { break };
                    let f = f32::from_bits(v);
                    let ext = o.external_at(b + w);
                    if let Some(n) = ext {
                        line += &format!(" [{n}]");
                    } else if o.internal.contains(&(b + w)) {
                        line += &format!(" ->{v:x}");
                    } else if v == 0 {
                        line += " 0";
                    } else if f.is_finite() && f.abs() > 1e-4 && f.abs() < 1e7 {
                        line += &format!(" {f:.3}");
                    } else {
                        line += &format!(" #{v:x}");
                    }
                }
                println!("{line}");
            }
        }
        Some("ht-track") => {
            // ht-track FILE ENTRY [out.obj]: decode a Hydro Thunder track and summarise it.
            let h = ht::HydroThunder::open(&riptide_assets::default_gdi_path())?;
            let t = h.load_track(a(1)?, a(2)?)?;
            let len: f32 = t.path.windows(2).map(|w| {
                let m = |e: &riptide_assets::h2level::Edge| [(e.start[0] + e.end[0]) / 2.0, (e.start[2] + e.end[2]) / 2.0];
                let (p, q) = (m(&w[0]), m(&w[1]));
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
            }).sum();
            println!("{}: {} tris, {} parts, path {} edges ({len:.0} units, looped {}), {} instances, {} starts", a(2)?, t.terrain.triangle_count(), t.terrain.parts.len(), t.path.len(), t.looped, t.instances.len(), t.starts.len());
            println!("  river: {} sectors (including side routes), {} drops over 40 Riptide units", t.river.len(), t.river.iter().filter(|[a,b]| (a.water-b.water)*1.6 > 40.0).count());
            for (i, [a, b]) in t.river.iter().enumerate() {
                if (b.water - a.water) * 1.6 > 20.0 {
                    println!("  RISE river {i}: {:.0} -> {:.0} at [{:.0}, {:.0}] (Riptide {:.0}, {:.0})", a.water, b.water, (b.start[0] + b.end[0]) * 0.5, (b.start[2] + b.end[2]) * 0.5, (b.start[0] + b.end[0]) * 0.8, -(b.start[2] + b.end[2]) * 0.8);
                }
            }
            if let Some((lo, hi)) = t.terrain.bounds() { println!("  bounds {lo:?} .. {hi:?}"); }
            if let (Some(f), Some(l)) = (t.path.first(), t.path.last()) { println!("  path first {:?}..{:?} last {:?}", f.start, f.end, l.start); }
            if let Some(s) = t.starts.first() { println!("  start {:?}", s); }
            for (i, e) in t.path.iter().enumerate().take(std::env::var("N").ok().and_then(|n| n.parse().ok()).unwrap_or(0)) {
                println!("  edge {i}: [{:.0},{:.0}]-[{:.0},{:.0}] mid [{:.0}, {:.0}] width {:.0} water {}", e.start[0], e.start[2], e.end[0], e.end[2], (e.start[0] + e.end[0]) / 2.0, (e.start[2] + e.end[2]) / 2.0, ((e.end[0] - e.start[0]).powi(2) + (e.end[2] - e.start[2]).powi(2)).sqrt(), e.water);
            }
            // Surfaces straight above/below each start slot: height and texture.
            let mut probes: Vec<[f32; 3]> = t.starts.iter().take(1).map(|s| s.0).collect();
            probes.extend(t.starts.iter().take(1).map(|s| [s.0[0], s.0[1], -s.0[2]]));
            probes.extend(t.path.iter().step_by(10).map(|e| [(e.start[0] + e.end[0]) / 2.0, e.water, (e.start[2] + e.end[2]) / 2.0]));
            for sp in &probes {
                println!("  probe {:?}", [sp[0] as i32, sp[2] as i32]);
                for part in &t.terrain.parts {
                    for tri in part.indices.chunks(3) {
                        let p: Vec<[f32; 3]> = tri.iter().map(|&i| part.positions[i as usize]).collect();
                        let s = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
                        let q = *sp;
                        let (d1, d2, d3) = (s(p[0], p[1], q), s(p[1], p[2], q), s(p[2], p[0], q));
                        let inside = (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0);
                        if inside {
                            println!("  under start {:?}: y {:.1} {:.1} {:.1} tex {:?}", [q[0] as i32, q[2] as i32], p[0][1], p[1][1], p[2][1], part.texture);
                        }
                    }
                }
            }
            // FX=1: every effect instance (`G?F*`: waterfalls, glows, birds) with its world position.
            if std::env::var_os("FX").is_some() {
                for inst in t.instances.iter().filter(|i| i.geometry.as_bytes().get(2) == Some(&b'F')) {
                    println!("  fx {} at world {:.0}, {:.0}, {:.0} yaw {:.2} scale {:.2}", inst.geometry, inst.position[0] * 1.6, inst.position[1] * 1.6, inst.position[2] * 1.6, inst.yaw, inst.scale);
                }
            }
            // NEAR="x,z" (Riptide world units): the instances (placed scenery) within 400 of it.
            if let Some((px, pz)) = std::env::var("NEAR").ok().and_then(|s| { let (a, b) = s.split_once(',')?; Some((a.parse::<f32>().ok()? / 1.6, b.parse::<f32>().ok()? / 1.6)) }) {
                for (i, [a, b]) in t.river.iter().enumerate() {
                    let mid = |e: &riptide_assets::h2level::Edge| [(e.start[0] + e.end[0]) / 2.0, (e.start[2] + e.end[2]) / 2.0];
                    let d = |m: [f32; 2]| ((m[0] - px).powi(2) + (m[1] - pz).powi(2)).sqrt();
                    if d(mid(a)).min(d(mid(b))) < 1500.0 / 1.6 {
                        println!("  river {i}: water {:.1} -> {:.1} (world {:.0} -> {:.0}), y at banks {:.1} {:.1}", a.water, b.water, a.water * 1.6, b.water * 1.6, a.start[1], a.end[1]);
                    }
                }
                for inst in &t.instances {
                    let d = ((inst.position[0] - px).powi(2) + (inst.position[2] - pz).powi(2)).sqrt();
                    if d < 400.0 / 1.6 {
                        let bounds = h.geometry(&inst.geometry).ok().and_then(|m| m.bounds());
                        println!("  instance {} at {:?} (world {:.0}, {:.0}) yaw {:.2} scale {:.2} bounds {bounds:?}", inst.geometry, inst.position, inst.position[0] * 1.6, inst.position[2] * 1.6, inst.yaw, inst.scale);
                    }
                }
            }
            // PROBE="x,z" (Riptide world units, HT scale 1.6 applied): steep triangles near that point.
            if let Some((px, pz)) = std::env::var("PROBE").ok().and_then(|s| { let (a, b) = s.split_once(',')?; Some((a.parse::<f32>().ok()? / 1.6, b.parse::<f32>().ok()? / 1.6)) }) {
                for part in &t.terrain.parts {
                    for tri in part.indices.chunks(3) {
                        let p: Vec<[f32; 3]> = tri.iter().map(|&i| part.positions[i as usize]).collect();
                        let c = [(p[0][0] + p[1][0] + p[2][0]) / 3.0, (p[0][2] + p[1][2] + p[2][2]) / 3.0];
                        let (ymin, ymax) = (p.iter().map(|v| v[1]).fold(f32::MAX, f32::min), p.iter().map(|v| v[1]).fold(f32::MIN, f32::max));
                        if ((c[0] - px).powi(2) + (c[1] - pz).powi(2)).sqrt() < 120.0 && ymin < 15.0 && ymax > 0.0 {
                            println!("  near: tex {:?} y {ymin:.0}..{ymax:.0} centre {:?}", part.texture, c);
                        }
                    }
                }
            }
            if let Ok(out) = a(3) {
                if out.ends_with(".png") {
                    // Top-down map: course, racing line (cyan to magenta), start slots (yellow).
                    let lvl = h2level::H2Level { path: t.path.clone(), starts: t.starts.iter().map(|(p, _)| (*p, [0.0, 0.0, 0.0, 1.0])).collect(), ..Default::default() };
                    write_png(Path::new(out), &render_top(&t.terrain, &lvl, 1024))?;
                } else {
                    std::fs::write(out, t.terrain.to_obj())?;
                }
            }
        }
        Some("ht-r2-tex") => {
            // ht-r2-tex FILE.R2 TEXTURE out.png: a texture from any R2 on the disc.
            let g = riptide_assets::gdi::GdRom::open(&riptide_assets::default_gdi_path())?;
            let r2 = riptide_assets::r2::R2Archive::parse(g.read(a(1)?)?)?;
            let img = riptide_assets::pvr::decode_pvrt_in(r2.raw(a(2)?).context("no such texture")?)?;
            write_png(Path::new(a(3)?), &img)?;
        }
        Some("fsb-list") => {
            // fsb-list <bank> (e.g. wa_mp3): samples of an FSB4 bank in triton.lux.
            let l = open_lux()?;
            let bank = l.get(&format!("fbnk.{}", a(1)?)).context("no such bank")?;
            for s in riptide_assets::fsb::parse(bank)? {
                println!("{:<30} {:>9} {:>5}Hz {}ch {:<9} loop {} {:>8} bytes", s.name, s.samples, s.frequency, s.channels, s.codec(), s.looped(), s.size);
            }
        }
        Some("fsb-get") => {
            // fsb-get <bank> <sample> <out-without-extension>
            let l = open_lux()?;
            let bank = l.get(&format!("fbnk.{}", a(1)?)).context("no such bank")?;
            let s = riptide_assets::fsb::parse(bank)?.into_iter().find(|s| s.name == a(2).unwrap_or("")).context("no such sample")?;
            let (bytes, ext) = riptide_assets::fsb::extract(bank, &s)?;
            let path = format!("{}.{ext}", a(3)?);
            std::fs::write(&path, bytes)?;
            println!("wrote {path}");
        }
        Some("lux-wide") => {
            // lux-wide <table> <class> : that class's objects as a sheet (all properties, defaults filled).
            let l = open_lux()?;
            let mut objs = seed::objects(&l, a(1)?)?;
            objs.retain(|o| o.class == a(2).unwrap_or(""));
            print!("{}", seed::wide(&objs));
        }
        Some("ht-list") => {
            let h = ht::HydroThunder::open(&riptide_assets::default_gdi_path())?;
            for e in h.main.entries() {
                if args.len() < 2 || e.name.contains(a(1)?) {
                    println!("{:>9} {:>6} {}", e.size, e.relocations, e.name);
                }
            }
        }
        Some("ht-tex") => {
            let h = ht::HydroThunder::open(&riptide_assets::default_gdi_path())?;
            let img = h.texture(a(1)?)?;
            let n = (img.rgba.len() / 4).max(1);
            let zero = img.rgba.chunks_exact(4).filter(|p| p[3] == 0).count();
            let full = img.rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
            println!("{}: {}x{} alpha 0 in {:.0}%, 255 in {:.0}%", a(1)?, img.width, img.height, zero as f32 * 100.0 / n as f32, full as f32 * 100.0 / n as f32);
            if let Ok(out) = a(2) {
                write_png(Path::new(out), &img)?;
            }
        }
        Some("ht-geom") => {
            let h = ht::HydroThunder::open(&riptide_assets::default_gdi_path())?;
            let m = h.geometry(a(1)?)?;
            describe(&m);
            std::fs::write(a(2)?, m.to_obj())?;
        }
        Some("ht-render") => {
            let h = ht::HydroThunder::open(&riptide_assets::default_gdi_path())?;
            let mut m = Model::default();
            for name in a(1)?.split(',') {
                m.parts.extend(h.geometry(name)?.parts);
            }
            describe(&m);
            let mut tex = HashMap::new();
            for p in &m.parts {
                if let Some(t) = &p.texture {
                    match h.texture(t) {
                        Ok(img) => {
                            tex.insert(t.clone(), img);
                        }
                        Err(e) => eprintln!("texture {t}: {e:#}"),
                    }
                }
            }
            let yaw: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(35.0);
            write_png(Path::new(a(2)?), &render(&m, &tex, yaw, 25.0, 768))?;
        }
        Some("hackworld-seed") => {
            let l = open_lux()?;
            hackworld::seed(&l, Path::new(a(1)?), |m| lux_model(&l, m).ok()?.bounds())?
        }
        Some("seed-sheets") => seed::seed_sheets(&open_lux()?, Path::new(a(1)?))?,
        Some("coll") => {
            let l = open_lux()?;
            let code = a(1)?;
            let name = format!("coll4.wc_{code}");
            let tris = h2coll::decode_collision(l.get(&name).with_context(|| format!("no {name}"))?)?;
            // Flags histogram, and PROBE="x,z" (Riptide coords: z mirrored) lists nearby triangles.
            let flagged = h2coll::decode_collision_flags(l.get(&name).context("coll")?)?;
            let mut hist: std::collections::BTreeMap<u32, usize> = Default::default();
            for (_, f) in &flagged { *hist.entry(*f).or_default() += 1; }
            println!("flags {hist:x?}");
            if let Some((px, pz)) = std::env::var("PROBE").ok().and_then(|s| { let (a, b) = s.split_once(',')?; Some((a.parse::<f32>().ok()?, -b.parse::<f32>().ok()?)) }) {
                for (t, f) in &flagged {
                    let c = [(t[0][0] + t[1][0] + t[2][0]) / 3.0, (t[0][2] + t[1][2] + t[2][2]) / 3.0];
                    if ((c[0] - px).powi(2) + (c[1] - pz).powi(2)).sqrt() < 250.0 {
                        let ys: Vec<i32> = t.iter().map(|v| v[1] as i32).collect();
                        println!("  near: flags {f:#x} centre [{:.0}, {:.0}] y {ys:?}", c[0], -c[1]);
                    }
                }
            }
            let bb = |pts: &mut dyn Iterator<Item = [f32; 3]>| {
                let mut b: Option<([f32; 3], [f32; 3])> = None;
                for p in pts {
                    let (lo, hi) = b.get_or_insert((p, p));
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                b
            };
            println!("{name}: {} triangles, bounds {:?}", tris.len(), bb(&mut tris.iter().flatten().copied()));
            let lvl = h2level::load_level(&l, code)?;
            let mut world = Model::default();
            for s in &lvl.sector_meshes {
                if let Ok(m) = lux_model(&l, s) {
                    world.parts.extend(m.parts);
                }
            }
            // visible meshes are Z-mirrored on load; undo for comparison
            let mut it = world.parts.iter().flat_map(|p| p.positions.iter().map(|p| [p[0], p[1], -p[2]]));
            println!("visible terrain (unmirrored): bounds {:?}", bb(&mut it));
        }
        Some("level") => {
            let l = open_lux()?;
            if args.len() < 2 {
                for (c, t) in h2level::race_levels(&l) {
                    println!("{c}  {t}");
                }
                return Ok(());
            }
            let lvl = h2level::load_level(&l, a(1)?)?;
            println!(
                "{} '{}': {} sector meshes, {} props, {} boosters, {} starts, {} path edges, {} water quads, skyboxes {:?}",
                lvl.code,
                lvl.title,
                lvl.sector_meshes.len(),
                lvl.props.len(),
                lvl.boosters.len(),
                lvl.starts.len(),
                lvl.path.len(),
                lvl.water.len(),
                lvl.skyboxes.iter().map(|s| &s.mesh).collect::<Vec<_>>()
            );
            if args.get(2).map(String::as_str) == Some("PATH") {
                println!("finish buoys {:?}", lvl.finish);
                for (i, e) in lvl.path.iter().enumerate() {
                    println!("{i}: mid {:.0} {:.0} {:.0} width {:.0}", (e.start[0] + e.end[0]) * 0.5, e.water, (e.start[2] + e.end[2]) * 0.5, ((e.start[0] - e.end[0]).powi(2) + (e.start[2] - e.end[2]).powi(2)).sqrt());
                }
                return Ok(());
            }
            if args.get(2).map(String::as_str) == Some("PROPS") {
                // Each prop: mesh, position, and the water height of the nearest path edge.
                for p in &lvl.props {
                    let w = lvl.path.iter().min_by(|a, b| {
                        let d = |e: &riptide_assets::h2level::Edge| (e.start[0] + e.end[0] - 2.0 * p.position[0]).powi(2) + (e.start[2] + e.end[2] - 2.0 * p.position[2]).powi(2);
                        d(a).total_cmp(&d(b))
                    }).map(|e| e.water).unwrap_or(0.0);
                    println!("{} {:.0} {:.0} {:.0} water {:.0} above {:+.0} scale {:.2}", p.mesh, p.position[0], p.position[1], p.position[2], w, p.position[1] - w, p.scale);
                }
                return Ok(());
            }
            if args.get(2).map(String::as_str) == Some("WATER") {
println!("{} sounds: {:?}", lvl.sounds.len(), lvl.sounds.iter().filter(|s| s.radii.is_none()).map(|s| (&s.name, &s.sound)).collect::<Vec<_>>());
println!("{} fires: {:?}", lvl.fires.len(), lvl.fires.iter().take(3).map(|f| (&f.def, f.position, f.scale)).collect::<Vec<_>>());
                println!("{} water edges, {} water sectors", lvl.water_edges.len(), lvl.water_sectors.len());
                for s in &lvl.water_sectors {
                    let (a, b) = (&lvl.water_edges[s.leading], &lvl.water_edges[s.trailing]);
                    if (a.water - b.water).abs() > 30.0 {
                        println!("DROP {} -> {}: {:.0} -> {:.0} ({:+.0})", a.name, b.name, a.water, b.water, b.water - a.water);
                    }
                }
                for e in &lvl.water_edges {
                    println!("{e:?}");
                }
                return Ok(());
            }
            let mut missing: Vec<&str> =
                lvl.props.iter().map(|p| p.mesh.as_str()).filter(|m| !l.contains(&format!("mesh32.{m}"))).collect();
            missing.sort();
            missing.dedup();
            println!("props with no mesh32: {missing:?}");
            // NEAR=x,z (Riptide world units): placed props within 800 of it.
            if let Some((px, pz)) = std::env::var("NEAR").ok().and_then(|s| { let (a, b) = s.split_once(',')?; Some((a.parse::<f32>().ok()?, b.parse::<f32>().ok()?)) }) {
                for p in lvl.props.iter().chain(lvl.boosters.iter().map(|b| &b.placement)) {
                    let d = ((p.position[0] - px).powi(2) + (p.position[2] - pz).powi(2)).sqrt();
                    if d < 800.0 {
                        println!("  prop {} at {:.0} {:.0} {:.0} scale {:.2} dist {d:.0} anim {:?}", p.mesh, p.position[0], p.position[1], p.position[2], p.scale, p.anim.as_ref().map(|a| &a.clip));
                    }
                }
            }
            if std::env::var_os("EDGES").is_some() {
                // EDGES=1: every racing-line cross-section: index, mid point, water level.
                for (i, e) in lvl.path.iter().enumerate() {
                    let m = [(e.start[0] + e.end[0]) / 2.0, (e.start[2] + e.end[2]) / 2.0];
                    println!("edge {i:3} mid [{:8.0} {:8.0}] water {:7.1}", m[0], m[1], e.water);
                }
            }
            if let Some(out) = args.get(2) {
                let mut world = Model::default();
                for s in &lvl.sector_meshes {
                    match lux_model(&l, s) {
                        Ok(m) => world.parts.extend(m.parts),
                        Err(e) => eprintln!("{s}: {e:#}"),
                    }
                }
                write_png(Path::new(out), &render_top(&world, &lvl, 1024))?;
            }
        }
        _ => bail!("usage: see the doc comment in crates/riptide-tool/src/main.rs"),
    }
    Ok(())
}

fn open_lux() -> Result<lux::LuxArchive> {
    lux::LuxArchive::open(&riptide_assets::default_lux_path())
}

fn lux_model(l: &lux::LuxArchive, name: &str) -> Result<Model> {
    h2mesh::decode_mesh(name, l.get(&format!("mesh32.{name}")).with_context(|| format!("no mesh32.{name}"))?)
}

fn describe(m: &Model) {
    let b = m.bounds();
    println!("{}: {} parts, {} tris, bounds {:?}", m.name, m.parts.len(), m.triangle_count(), b);
    for p in &m.parts {
        println!(
            "  {} verts {} tris tex={:?} shader={:?} uv={} col={} mean rgba {:?}",
            p.positions.len(),
            p.indices.len() / 3,
            p.texture,
            p.shader,
            p.uvs.len(),
            p.colors.len(),
            {
                let n = p.colors.len().max(1) as f32;
                let s = p.colors.iter().fold([0.0f32; 4], |a, c| [a[0] + c[0], a[1] + c[1], a[2] + c[2], a[3] + c[3]]);
                s.map(|v| (v / n * 100.0).round() / 100.0)
            }
        );
    }
}

fn write_png(path: &Path, img: &RgbaImage) -> Result<()> {
    let f = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&img.rgba)?;
    println!("wrote {} ({}x{})", path.display(), img.width, img.height);
    Ok(())
}

/// Minimal perspective rasteriser: textured, vertex-coloured, z-buffered.
fn render(m: &Model, tex: &HashMap<String, RgbaImage>, yaw_deg: f32, pitch_deg: f32, size: usize) -> RgbaImage {
    let (lo, hi) = m.bounds().unwrap_or(([-1.0; 3], [1.0; 3]));
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let r = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt() / 2.0;
    let (sy, cy) = yaw_deg.to_radians().sin_cos();
    let (sp, cp) = pitch_deg.to_radians().sin_cos();
    let dist = r * 2.6;
    let project = |p: [f32; 3]| -> [f32; 3] {
        let x = p[0] - c[0];
        let y = p[1] - c[1];
        let z = p[2] - c[2];
        // yaw around Y, then pitch around X; camera looks down -Z.
        let x1 = x * cy - z * sy;
        let z1 = x * sy + z * cy;
        let y2 = y * cp - z1 * sp;
        let z2 = y * sp + z1 * cp;
        let depth = dist - z2;
        let f = size as f32 * 0.9;
        [size as f32 / 2.0 + x1 * f / depth, size as f32 / 2.0 - y2 * f / depth, depth]
    };
    raster(m, tex, size, size, project, [40, 44, 52])
}

fn render_top(world: &Model, lvl: &h2level::H2Level, size: usize) -> RgbaImage {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for e in &lvl.path {
        for p in [e.start, e.end] {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let span = (hi[0] - lo[0]).max(hi[2] - lo[2]) * 1.15;
    let cx = (lo[0] + hi[0]) / 2.0;
    let cz = (lo[2] + hi[2]) / 2.0;
    let project = |p: [f32; 3]| -> [f32; 3] {
        [
            size as f32 / 2.0 + (p[0] - cx) / span * size as f32,
            size as f32 / 2.0 + (p[2] - cz) / span * size as f32,
            100000.0 - p[1],
        ]
    };
    let mut img = raster(world, &HashMap::new(), size, size, project, [20, 20, 30]);
    let mut dot = |p: [f32; 3], col: [u8; 3]| {
        let q = project(p);
        for dy in -2i32..=2 {
            for dx in -2i32..=2 {
                let x = q[0] as i32 + dx;
                let y = q[1] as i32 + dy;
                if x >= 0 && y >= 0 && (x as usize) < size && (y as usize) < size {
                    img.rgba[(y as usize * size + x as usize) * 4..][..3].copy_from_slice(&col);
                }
            }
        }
    };
    for e in &lvl.path {
        dot(e.start, [0, 200, 0]);
        dot(e.end, [220, 40, 40]);
    }
    for (i, e) in lvl.path.iter().enumerate() {
        let mid = [(e.start[0] + e.end[0]) / 2.0, e.water, (e.start[2] + e.end[2]) / 2.0];
        let t = i as f32 / lvl.path.len().max(1) as f32;
        dot(mid, [(255.0 * t) as u8, (255.0 * (1.0 - t)) as u8, 255]);
    }
    for (p, _) in &lvl.starts {
        dot(*p, [255, 255, 0]);
    }
    for b in &lvl.boosters {
        dot(b.placement.position, [255, 0, 0]);
    }
    img
}

fn raster(
    m: &Model,
    tex: &HashMap<String, RgbaImage>,
    w: usize,
    h: usize,
    project: impl Fn([f32; 3]) -> [f32; 3],
    bg: [u8; 3],
) -> RgbaImage {
    let mut color = vec![0u8; w * h * 4];
    for px in color.chunks_exact_mut(4) {
        px.copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
    }
    let mut zbuf = vec![f32::MAX; w * h];
    let light = {
        let l = [0.4f32, 0.8, 0.45];
        let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        [l[0] / n, l[1] / n, l[2] / n]
    };
    for p in &m.parts {
        let t = p.texture.as_ref().and_then(|t| tex.get(t));
        let pts: Vec<[f32; 3]> = p.positions.iter().map(|&v| project(v)).collect();
        for tri in p.indices.chunks_exact(3) {
            let [i0, i1, i2] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            let (a, b, c) = (pts[i0], pts[i1], pts[i2]);
            if a[2] <= 0.0 || b[2] <= 0.0 || c[2] <= 0.0 {
                continue;
            }
            let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            if area.abs() < 1e-6 {
                continue;
            }
            let minx = a[0].min(b[0]).min(c[0]).max(0.0) as usize;
            let maxx = (a[0].max(b[0]).max(c[0]).ceil() as usize).min(w.saturating_sub(1));
            let miny = a[1].min(b[1]).min(c[1]).max(0.0) as usize;
            let maxy = (a[1].max(b[1]).max(c[1]).ceil() as usize).min(h.saturating_sub(1));
            let nrm = p.normals.get(i0).copied().unwrap_or([0.0, 1.0, 0.0]);
            let shade = 0.45 + 0.55 * (nrm[0] * light[0] + nrm[1] * light[1] + nrm[2] * light[2]).abs();
            for y in miny..=maxy {
                for x in minx..=maxx {
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((b[0] - fx) * (c[1] - fy) - (b[1] - fy) * (c[0] - fx)) / area;
                    let w1 = ((c[0] - fx) * (a[1] - fy) - (c[1] - fy) * (a[0] - fx)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
                    let zi = y * w + x;
                    if z >= zbuf[zi] {
                        continue;
                    }
                    let mut rgba = [200.0f32, 200.0, 200.0, 255.0];
                    if let (Some(img), true) = (t, p.uvs.len() == p.positions.len()) {
                        let u = w0 * p.uvs[i0][0] + w1 * p.uvs[i1][0] + w2 * p.uvs[i2][0];
                        let v = w0 * p.uvs[i0][1] + w1 * p.uvs[i1][1] + w2 * p.uvs[i2][1];
                        let tx = ((u.rem_euclid(1.0)) * img.width as f32) as usize % img.width as usize;
                        let ty = ((v.rem_euclid(1.0)) * img.height as f32) as usize % img.height as usize;
                        let s = &img.rgba[(ty * img.width as usize + tx) * 4..][..4];
                        rgba = [s[0] as f32, s[1] as f32, s[2] as f32, s[3] as f32];
                    }
                    if rgba[3] < 64.0 {
                        continue;
                    }
                    if p.colors.len() == p.positions.len() && t.is_none() {
                        let cc = p.colors[i0];
                        rgba[0] *= cc[0].powf(1.0 / 2.2) * 1.2;
                        rgba[1] *= cc[1].powf(1.0 / 2.2) * 1.2;
                        rgba[2] *= cc[2].powf(1.0 / 2.2) * 1.2;
                    }
                    zbuf[zi] = z;
                    let o = &mut color[zi * 4..zi * 4 + 4];
                    o[0] = (rgba[0] * shade).min(255.0) as u8;
                    o[1] = (rgba[1] * shade).min(255.0) as u8;
                    o[2] = (rgba[2] * shade).min(255.0) as u8;
                }
            }
        }
    }
    RgbaImage { width: w as u32, height: h as u32, rgba: color }
}
