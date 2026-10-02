//! `hackworld-seed`: write `sheets/hackworld.csv`, the Hackworld sandbox layout. Open water with
//! H2Overdrive's own ramps, props taken from every H2 level, and boost pickups, placed by a fixed
//! seed (re-running gives the same sheet). The ring the AI races around is kept free of props.

use anyhow::Result;
use riptide_assets::h2level;
use riptide_assets::lux::LuxArchive;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// Layout constants (mirrored by the `physics` sheet's `hackworld_*` rows).
const HALF: f32 = 24_000.0;
const RING_RADIUS: f32 = 14_000.0;
const RING_WIDTH: f32 = 5_000.0;
/// A prop whose lowest point sits below this height (placed, scaled) reaches the water: solid.
const SOLID_REACH: f32 = 150.0;

const RAMPS: [&str; 6] = ["pg_com_ramp_lo", "pg_com_ramp_med", "pg_com_ramp_high", "pg_com_ramp_insane", "pg_wa_ramp1", "pg_qc_RoofRamp"];

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 % 1_000_000) as f32 / 1_000_000.0
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.next()
    }
}

pub fn seed(lux: &LuxArchive, out: &Path, model: impl Fn(&str) -> Option<([f32; 3], [f32; 3])>) -> Result<()> {
    // Every static prop mesh of every race level, with the level it came from.
    let mut props: BTreeMap<String, String> = BTreeMap::new();
    for (code, _) in h2level::race_levels(lux) {
        let Ok(l) = h2level::load_level(lux, &code) else { continue };
        for p in l.props.iter().filter(|p| p.path.is_none()) {
            props.entry(p.mesh.clone()).or_insert(code.clone());
        }
    }
    // Keep boat-sized to building-sized things: no specks, no whole islands. Collision helpers
    // (`*ColSph`, `*CollSph`: invisible bubbles round the original's moving boats) and effect
    // sprites (fireflies, fly swarms) are not scenery.
    let helper = |m: &str| {
        let m = m.to_ascii_lowercase();
        ["colsph", "collsph", "fly", "firefl"].iter().any(|k| m.contains(k))
    };
    let props: Vec<(String, String, f32, f32)> = props
        .into_iter()
        .filter(|(m, _)| !helper(m))
        .filter_map(|(m, code)| {
            let (lo, hi) = model(&m)?;
            let size = (hi[0] - lo[0]).max(hi[2] - lo[2]);
            (60.0..=2500.0).contains(&size).then_some((m, code, size, lo[1]))
        })
        .collect();
    let mut rng = Rng(0x4841_434b);
    let mut csv = String::from("id,kind,model,pickup,x,y,z,yaw,scale,solid,status,source\n@types,enum:ramp|prop|booster,asset,ref:pickups,f32,f32,f32,f32,f32,bool,status,str\n");
    let on_ring = |x: f32, z: f32| ((x * x + z * z).sqrt() - RING_RADIUS).abs() < RING_WIDTH * 0.5;
    let mut n = 0;
    let mut row = |csv: &mut String, kind: &str, model: &str, pickup: &str, p: [f32; 3], yaw: f32, scale: f32, solid: bool, source: &str| {
        n += 1;
        writeln!(csv, "hw{n:04},{kind},{model},{pickup},{:.0},{:.0},{:.0},{yaw:.0},{scale:.2},{solid},ok,{source}", p[0], p[1], p[2]).unwrap();
    };
    // Ramp park by the start line: one of each ramp, small to huge, all facing the ring's direction.
    for (i, r) in RAMPS.iter().enumerate() {
        for (j, s) in [1.0f32, 2.0, 3.5].iter().enumerate() {
            let x = RING_RADIUS + (i as f32 - 2.5) * 700.0;
            let z = -2500.0 - j as f32 * 1800.0;
            row(&mut csv, "ramp", &format!("lux:mesh32.{r}"), "-", [x, 0.0, z], 180.0, *s, true, "ramp park");
        }
    }
    // Ramps on the ring (the AI hits these too), aimed along it.
    for k in 0..40 {
        let a = k as f32 / 40.0 * std::f32::consts::TAU + rng.range(0.0, 0.05);
        let r = RING_RADIUS + rng.range(-RING_WIDTH * 0.35, RING_WIDTH * 0.35);
        let (x, z) = (r * a.cos(), r * a.sin());
        // Travel runs with increasing angle; a ramp rises along its local -Z like a boat faces,
        // so its yaw is the tangent heading (180 - angle).
        let yaw = 180.0 - a.to_degrees();
        let ramp = RAMPS[(rng.next() * 4.0) as usize];
        row(&mut csv, "ramp", &format!("lux:mesh32.{ramp}"), "-", [x, 0.0, z], yaw, rng.range(1.0, 3.0), true, "ring ramp");
    }
    // Scattered ramps everywhere else, any direction, any size.
    for _ in 0..140 {
        let (x, z) = (rng.range(-HALF, HALF), rng.range(-HALF, HALF));
        let ramp = RAMPS[(rng.next() * RAMPS.len() as f32) as usize % RAMPS.len()];
        row(&mut csv, "ramp", &format!("lux:mesh32.{ramp}"), "-", [x, 0.0, z], rng.range(0.0, 360.0), rng.range(1.0, 5.0), true, "scatter");
    }
    // Props from every level, off the ring.
    let mut placed = 0;
    while placed < 220 && !props.is_empty() {
        let (x, z) = (rng.range(-HALF, HALF), rng.range(-HALF, HALF));
        if on_ring(x, z) || (x - RING_RADIUS).abs() < 3000.0 && z < 0.0 && z > -8000.0 {
            continue;
        }
        let (m, code, size, low) = &props[(rng.next() * props.len() as f32) as usize % props.len()];
        let scale = (rng.range(0.8, 1.6) * if *size < 200.0 { 2.0 } else { 1.0 }).min(3.0);
        // Only props standing in the water block boats; floating ones (balloons, signs) are scenery.
        let solid = low * scale < SOLID_REACH;
        row(&mut csv, "prop", &format!("lux:mesh32.{m}"), "-", [x, 0.0, z], rng.range(0.0, 360.0), scale, solid, &format!("prop from {code}"));
        placed += 1;
    }
    // Boosts: plenty on the ring, more scattered.
    for k in 0..90 {
        let (x, z) = if k < 50 {
            let a = rng.range(0.0, std::f32::consts::TAU);
            let r = RING_RADIUS + rng.range(-RING_WIDTH * 0.4, RING_WIDTH * 0.4);
            (r * a.cos(), r * a.sin())
        } else {
            (rng.range(-HALF, HALF), rng.range(-HALF, HALF))
        };
        let kind = ["blue", "red", "gold"][if rng.next() < 0.15 { 2 } else { (rng.next() * 2.0) as usize }];
        row(&mut csv, "booster", "-", kind, [x, 30.0, z], 0.0, 1.0, false, "boost");
    }
    std::fs::write(out, csv)?;
    println!("wrote {} ({n} objects, {} prop meshes to pick from)", out.display(), props.len());
    Ok(())
}
