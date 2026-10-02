//! Hydro Thunder track entries (`H*TRH0` in each track's `.R2`, e.g. `GRAV.R2`).
//!
//! Body layout (offsets body-relative, Hydro Thunder units, left-handed):
//! - `+0x00` start-slot count (16), then slots of 0x14 from `+0x04`:
//!   `{[sector], f32 x, y, z, u32 yaw (65536 = one turn)}`.
//! - `+0x144` sector count, `+0x148` portal count, `+0x150` instance count.
//! - `+0x178` [sectors], 0xd4 each: `+0x00` [entry portal], `+0x04` [exit portal].
//!   Following exit -> entry links gives the driving order. `+0x20/+0x24` and `+0x28/+0x2c`
//!   are the AI driving band at the entry / exit portal, as distances from its bank point.
//! - `+0x17c` [portals], 0x44 each: `f32 bank x, z, across dx, dz, width, u32 flags, f32 water y,
//!   travel dx, dz`. A portal is the river cross-section between two sectors.
//! - `+0x184` [instances], 0x40 each: `f32 x, y, z`, `+0x0e u16 yaw`, `+0x18 f32 scale`, `+0x24` [geometry],
//!   `+0x30 char name[8]`.
//! - `+0x1cc` a geometry header in the `G*` object layout (see [`crate::htgeom`]) holding the
//!   whole course; each of its groups is one visibility sector.

use crate::h2level::Edge;
use crate::htgeom::decode_geometry_at;
use crate::model::Model;
use crate::r2::R2Object;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;

const SECTOR: usize = 0xd4;
const INSTANCE: usize = 0x40;

#[derive(Debug, Clone)]
pub struct HtInstance {
    pub geometry: String,
    pub name: String,
    pub position: [f32; 3],
    /// Radians, Bevy convention (u16 at `+0x0e`, 65536 = one turn, like the start slots).
    pub yaw: f32,
    pub scale: f32,
}

#[derive(Debug, Clone)]
pub struct HtTrack {
    pub terrain: Model,
    /// River cross-sections in driving order (right-handed, Z mirrored like the models).
    pub path: Vec<Edge>,
    /// Per cross-section, the AI driving band as fractions from `start` to `end`.
    pub lanes: Vec<[f32; 2]>,
    /// The course closes on itself.
    pub looped: bool,
    pub instances: Vec<HtInstance>,
    /// Start slots: position and yaw (radians, Bevy convention: 0 faces -Z).
    pub starts: Vec<([f32; 3], f32)>,
}

pub fn decode_track(obj: &R2Object) -> Result<HtTrack> {
    if obj.kind != b'H' {
        bail!("{} is not a track", obj.name);
    }
    let nsect = obj.u32(0x144).context("header")? as usize;
    let nport = obj.u32(0x148).context("header")? as usize;
    let ninst = obj.u32(0x150).context("header")? as usize;
    let sectors = obj.ptr(0x178).context("no sectors")?;
    let instances = obj.ptr(0x184).context("no instances")?;
    if nsect == 0 || nsect > 4096 || nport > 4096 || ninst > 16384 {
        bail!("{}: implausible header counts", obj.name);
    }

    // Sector graph: entry portal -> sectors starting there.
    let mut links: Vec<(usize, usize)> = Vec::new();
    let mut sectors_of: Vec<usize> = Vec::new();
    for s in 0..nsect {
        let b = sectors + s * SECTOR;
        if let (Some(a), Some(e)) = (obj.ptr(b), obj.ptr(b + 4)) {
            links.push((a, e));
            sectors_of.push(s);
        }
    }
    let mut from: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, (a, _)) in links.iter().enumerate() {
        from.entry(*a).or_default().push(i);
    }

    // Start slots and the sector the grid sits in.
    let nstart = (obj.u32(0).unwrap_or(0) as usize).min(32);
    let mut starts = Vec::new();
    let mut start_sector = None;
    for i in 0..nstart {
        let b = 4 + i * 0x14;
        let (Some(x), Some(y), Some(z), Some(a)) = (obj.f32(b + 4), obj.f32(b + 8), obj.f32(b + 12), obj.u32(b + 16)) else {
            continue;
        };
        if i == 0 {
            start_sector = obj.ptr(b).and_then(|p| (p >= sectors).then(|| (p - sectors) / SECTOR));
        }
        // HT yaw 0 faces +Z, 0x4000 faces +X. After the Z mirror that is Bevy yaw `-angle`.
        let angle = (a & 0xffff) as f32 / 65536.0 * std::f32::consts::TAU;
        starts.push(([x, y, -z], -angle));
    }

    // The racing line: the longest chain of sectors through the start sector. Walk back to the
    // first sector of the course, then take the longest forward route (branches are shortcuts).
    // Walking back from the start reaching the start again means a circuit.
    let start = start_sector.unwrap_or(0).min(links.len().saturating_sub(1));
    let prev = |s: usize| links.iter().position(|(_, e)| *e == links[s].0);
    let (mut first, mut looped) = (start, false);
    for _ in 0..links.len() {
        match prev(first) {
            Some(p) if p == start => {
                looped = true;
                break;
            }
            Some(p) => first = p,
            None => break,
        }
    }
    let origin = if looped { start } else { first };
    // The longest route finds where the course ends; the shortest route there (by distance)
    // drops side loops around islands that the longest route would wander through.
    let longest_chain = longest(&links, &from, origin, looped);
    let mid_of = |p: usize| -> Option<(f32, f32)> { Some((obj.f32(p)?, obj.f32(p + 4)?)) };
    let length = |s: usize| -> f32 {
        match (mid_of(links[s].0), mid_of(links[s].1)) {
            (Some(a), Some(b)) => ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt(),
            _ => 1.0,
        }
    };
    let chain = match longest_chain.last() {
        Some(&end) => shortest(&links, &from, origin, end, length).unwrap_or(longest_chain),
        None => longest_chain,
    };

    let portal = |p: usize| -> Option<(Edge, bool, f32)> {
        let x = obj.f32(p)?;
        let z = obj.f32(p + 4)?;
        let width = obj.f32(p + 0x10)?;
        let water = obj.f32(p + 0x18)?;
        let (ax, az) = (obj.f32(p + 8)?, obj.f32(p + 0xc)?);
        // Put the start bank on the left of the portal's own travel vector (`+0x1c`).
        let (tx, tz) = (obj.f32(p + 0x1c)?, obj.f32(p + 0x20)?);
        // (x, z) is one bank; the portal runs `width` along `across` to the other.
        let (a, b) = ([x, water, -z], [x + ax * width, water, -(z + az * width)]);
        let flip = tx * az - tz * ax < 0.0;
        Some((if flip { Edge { start: b, end: a, water } } else { Edge { start: a, end: b, water } }, flip, width))
    };
    // AI lanes: each sector stores, per end, the driving band as distances from the portal's
    // bank point: `+0x20/+0x24` at the entry, `+0x28/+0x2c` at the exit.
    let lane = |sector: usize, at: usize, flip: bool, width: f32| -> [f32; 2] {
        let b = sectors + sector * SECTOR;
        let (lo, hi) = (obj.f32(b + at).unwrap_or(0.0), obj.f32(b + at + 4).unwrap_or(width));
        let (lo, hi) = ((lo / width.max(1.0)).clamp(0.0, 1.0), (hi / width.max(1.0)).clamp(0.0, 1.0));
        let (lo, hi) = if flip { (1.0 - hi, 1.0 - lo) } else { (lo, hi) };
        if hi > lo + 0.02 { [lo, hi] } else { [0.15, 0.85] }
    };
    let mut path = Vec::new();
    let mut lanes = Vec::new();
    if let Some(&s0) = chain.first() {
        if let Some((e, flip, w)) = portal(links[s0].0) {
            path.push(e);
            lanes.push(lane(sectors_of[s0], 0x20, flip, w));
        }
    }
    for &s in &chain {
        if let Some((e, flip, w)) = portal(links[s].1) {
            path.push(e);
            lanes.push(lane(sectors_of[s], 0x28, flip, w));
        }
    }
    let mut inst = Vec::new();
    for i in 0..ninst {
        let b = instances + i * INSTANCE;
        let Some(geometry) = obj.external_at(b + 0x24) else { continue };
        let (Some(x), Some(y), Some(z)) = (obj.f32(b), obj.f32(b + 4), obj.f32(b + 8)) else { continue };
        let scale = obj.f32(b + 0x18).filter(|s| s.is_finite() && *s > 0.01 && *s < 100.0).unwrap_or(1.0);
        let name = obj
            .body
            .get(b + 0x30..b + 0x38)
            .map(|n| String::from_utf8_lossy(n).trim_end_matches('\0').to_string())
            .unwrap_or_default();
        // Records whose `+0x10` high half is 0x1002 use `+0x0c` for something else: no yaw.
        let special = obj.u32(b + 0x10).is_some_and(|v| v >> 16 == 0x1002);
        let yaw = if special { 0.0 } else { -(obj.u16(b + 0x0e).unwrap_or(0) as f32 / 65536.0 * std::f32::consts::TAU) };
        inst.push(HtInstance { geometry: geometry.to_string(), name, position: [x, y, -z], yaw, scale });
    }

    let terrain = decode_geometry_at(obj, 0x1cc, true)?;
    Ok(HtTrack { terrain, path, lanes, looped, instances: inst, starts })
}

/// Longest simple chain of sectors from `origin` following exit -> entry portal links.
fn longest(links: &[(usize, usize)], from: &HashMap<usize, Vec<usize>>, origin: usize, looped: bool) -> Vec<usize> {
    let mut best: Vec<usize> = Vec::new();
    let mut stack: Vec<(usize, usize)> = vec![(origin, 0)];
    let mut path = vec![origin];
    let mut on = vec![false; links.len()];
    on[origin] = true;
    let mut steps = 0usize;
    // Iterative DFS with a work cap: tracks have a handful of branches.
    while let Some(&(s, k)) = stack.last() {
        steps += 1;
        if steps > 2_000_000 {
            break;
        }
        let next = from.get(&links[s].1).map(Vec::as_slice).unwrap_or(&[]);
        let open: Vec<usize> = next.iter().copied().filter(|n| !on[*n]).collect();
        if k == 0 && (open.is_empty() || (looped && next.contains(&origin))) && path.len() > best.len() {
            best = path.clone();
        }
        if k < open.len() {
            stack.last_mut().unwrap().1 += 1;
            let n = open[k];
            on[n] = true;
            path.push(n);
            stack.push((n, 0));
        } else {
            stack.pop();
            if let Some(n) = path.pop() {
                on[n] = false;
            }
        }
    }
    best
}

/// Shortest chain of sectors from `origin` to `end` (inclusive), weighting each sector by `len`.
fn shortest(
    links: &[(usize, usize)],
    from: &HashMap<usize, Vec<usize>>,
    origin: usize,
    end: usize,
    len: impl Fn(usize) -> f32,
) -> Option<Vec<usize>> {
    let n = links.len();
    let mut dist = vec![f32::INFINITY; n];
    let mut prev = vec![usize::MAX; n];
    let mut done = vec![false; n];
    dist[origin] = len(origin);
    // O(n^2) Dijkstra: tracks have ~100-300 sectors.
    loop {
        let u = (0..n).filter(|&i| !done[i] && dist[i].is_finite()).min_by(|&a, &b| dist[a].total_cmp(&dist[b]))?;
        if u == end {
            break;
        }
        done[u] = true;
        for &v in from.get(&links[u].1).map(Vec::as_slice).unwrap_or(&[]) {
            let d = dist[u] + len(v);
            if !done[v] && d < dist[v] {
                dist[v] = d;
                prev[v] = u;
            }
        }
    }
    let mut chain = vec![end];
    while let Some(&last) = chain.last() {
        if last == origin {
            break;
        }
        let p = prev[last];
        if p == usize::MAX || chain.len() > n {
            return None;
        }
        chain.push(p);
    }
    chain.reverse();
    Some(chain)
}
