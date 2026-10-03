//! H2Overdrive level data: the `data.<code>_*` XML object lists inside `triton.lux`.
//!
//! Every file is `<ObjectList><Object class name category><Property name type><_0>..</_0>`.
//! Object references are hashes followed by a `<!-- category:Name -->` comment, which is what we
//! resolve them by. Positions and quaternions are converted to right-handed (Z flipped).

use crate::lux::LuxArchive;
use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct XmlObject {
    pub class: String,
    pub name: String,
    /// Property name -> element values (text, or the referenced object's name).
    pub props: HashMap<String, Vec<String>>,
    /// Property name -> declared type (`TYPE_FLOAT`, ...).
    pub types: HashMap<String, String>,
}

impl XmlObject {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.props.get(key).and_then(|v| v.first()).map(String::as_str)
    }
    pub fn f32(&self, key: &str) -> Option<f32> {
        self.get(key)?.trim().parse().ok()
    }
    pub fn floats(&self, key: &str) -> Option<Vec<f32>> {
        let v: Vec<f32> = self.get(key)?.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        (!v.is_empty()).then_some(v)
    }
    pub fn vec3(&self, key: &str) -> Option<[f32; 3]> {
        let v = self.floats(key)?;
        (v.len() >= 3).then(|| [v[0], v[1], -v[2]])
    }
    /// Quaternion `[x, y, z, w]`, mirrored across Z.
    pub fn quat(&self, key: &str) -> Option<[f32; 4]> {
        let v = self.floats(key)?;
        (v.len() >= 4).then(|| [-v[0], -v[1], v[2], v[3]])
    }
}

pub fn parse_object_list(xml: &str) -> Result<Vec<XmlObject>> {
    parse_objects(xml, false)
}

/// Like [`parse_object_list`], but properties left at their engine default (written as
/// `<!-- DEFAULT: <_0> v </_0> -->`) get that default, so every declared property is present.
pub fn parse_object_list_with_defaults(xml: &str) -> Result<Vec<XmlObject>> {
    parse_objects(xml, true)
}

fn parse_objects(xml: &str, defaults: bool) -> Result<Vec<XmlObject>> {
    let doc = roxmltree::Document::parse(xml)?;
    let mut out = Vec::new();
    for obj in doc.root_element().children().filter(|n| n.has_tag_name("Object")) {
        let mut o = XmlObject {
            class: obj.attribute("class").unwrap_or_default().to_string(),
            name: obj.attribute("name").unwrap_or_default().to_string(),
            props: HashMap::new(),
            types: HashMap::new(),
        };
        for p in obj.children().filter(|n| n.has_tag_name("Property")) {
            let pname = p.attribute("name").unwrap_or_default().to_string();
            o.types.insert(pname.clone(), p.attribute("type").unwrap_or_default().to_string());
            let is_ref = p.attribute("type") == Some("TYPE_OBJECT_REF");
            let mut values = Vec::new();
            for el in p.children().filter(|n| n.is_element()) {
                if is_ref {
                    let target = el
                        .children()
                        .find(|c| c.is_comment())
                        .and_then(|c| c.text())
                        .map(|t| t.trim().rsplit(':').next().unwrap_or("").trim().to_string());
                    values.push(target.unwrap_or_default());
                } else {
                    values.push(el.text().unwrap_or("").trim().to_string());
                }
            }
            if values.is_empty() && defaults {
                values = p
                    .children()
                    .filter(|c| c.is_comment())
                    .filter_map(|c| c.text()?.trim().strip_prefix("DEFAULT:").map(str::to_string))
                    .flat_map(|t| {
                        // `<_0> a </_0><_1> b </_1>`: every piece but the last ends at a closing tag.
                        let pieces: Vec<&str> = t.split("</_").collect();
                        pieces[..pieces.len() - 1]
                            .iter()
                            .filter_map(|s| s.rsplit_once('>').map(|(_, v)| v.trim().to_string()))
                            .collect::<Vec<_>>()
                    })
                    .collect();
            }
            if !values.is_empty() {
                o.props.insert(pname, values);
            }
        }
        out.push(o);
    }
    Ok(out)
}

pub fn load_list(lux: &LuxArchive, name: &str) -> Result<Vec<XmlObject>> {
    let blob = lux.get(&format!("data.{name}")).with_context(|| format!("data.{name} missing"))?;
    parse_object_list(&String::from_utf8_lossy(blob))
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub mesh: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
    /// Set when a path controller drives the object (traffic, trains, gondolas).
    pub path: Option<MotionPath>,
}

#[derive(Debug, Clone)]
pub struct MotionPath {
    pub points: Vec<[f32; 3]>,
    /// World units per second.
    pub speed: f32,
    pub looped: bool,
    /// Turn to face the direction of travel.
    pub align: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub water: f32,
}

#[derive(Debug, Clone)]
pub struct Quad {
    pub corners: [[f32; 3]; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoostKind {
    Blue,
    Red,
    Gold,
}

#[derive(Debug, Clone)]
pub struct Booster {
    pub kind: BoostKind,
    pub placement: Placement,
}

#[derive(Debug, Clone, Default)]
pub struct H2Level {
    pub code: String,
    pub title: String,
    /// Terrain meshes (`sg_<code>_PropSectorN`), placed at the origin.
    pub sector_meshes: Vec<String>,
    pub props: Vec<Placement>,
    pub boosters: Vec<Booster>,
    pub starts: Vec<([f32; 3], [f32; 4])>,
    /// Racing line: AI sector cross-sections in driving order (first lap).
    pub path: Vec<Edge>,
    pub water: Vec<Quad>,
    /// Vertical curtains at river drops (generated for Hydro Thunder sector portals).
    pub waterfalls: Vec<Quad>,
    pub skyboxes: Vec<Placement>,
    /// Centre of the finish-line buoys (`*Finish*` game meshes), when the level has them.
    pub finish: Option<[f32; 3]>,
}

/// Levels that ship complete data (sectors, AI, start points).
pub fn race_levels(lux: &LuxArchive) -> Vec<(String, String)> {
    let titles: HashMap<String, String> = load_list(lux, "global_levels")
        .map(|objs| {
            objs.iter()
                .filter_map(|o| {
                    let code = o.name.strip_prefix("level_")?.to_string();
                    Some((code, o.get("EnglishName")?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let mut out: Vec<(String, String)> = ["wa", "tl", "tf", "qc", "lo", "hk", "du", "rt", "rs", "pp", "rn", "ys"]
        .iter()
        .filter(|c| {
            lux.contains(&format!("data.{c}_ai"))
                && lux.contains(&format!("data.{c}_sectors"))
                && lux.contains(&format!("data.{c}_startpoints"))
        })
        .map(|c| (c.to_string(), titles.get(*c).cloned().unwrap_or_else(|| format!("Track {}", c.to_uppercase()))))
        .collect();
    out.sort();
    out
}

pub fn load_level(lux: &LuxArchive, code: &str) -> Result<H2Level> {
    let mut lvl = H2Level { code: code.to_string(), ..Default::default() };
    let prefix = format!("sg_{code}_");
    lvl.sector_meshes = lux.names_of_kind("mesh32").filter(|n| n.starts_with(&prefix)).map(str::to_string).collect();

    let mesh_defs: HashMap<String, String> = load_list(lux, "global_GameMeshDef")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|o| Some((o.name.clone(), o.get("Mesh Name 1")?.to_string())))
        .collect();

    if let Ok(objs) = load_list(lux, &format!("{code}_worldobs")) {
        let paths = motion_paths(&objs);
        let buoys: Vec<[f32; 3]> = objs
            .iter()
            .filter(|o| o.name.to_ascii_lowercase().contains("finish"))
            .filter_map(|o| o.vec3("Position"))
            .collect();
        if !buoys.is_empty() {
            let n = buoys.len() as f32;
            lvl.finish = Some([0, 1, 2].map(|k| buoys.iter().map(|b| b[k]).sum::<f32>() / n));
        }
        for o in &objs {
            let Some(position) = o.vec3("Position") else { continue };
            let rotation = o.quat("Orientation").unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let scale = o.f32("Scale").unwrap_or(1.0);
            let mesh = o
                .get("MeshName")
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .or_else(|| o.get("Game Mesh Def").and_then(|d| mesh_defs.get(d)).cloned());
            let Some(mesh) = mesh else { continue };
            let path = o
                .props
                .get("Controllers")
                .and_then(|cs| cs.iter().find_map(|c| paths.get(c.as_str())))
                .cloned();
            let placement = Placement { mesh, position, rotation, scale, path };
            if o.class == "CBooster" {
                let kind = match o.f32("Type").map(|t| t as i32).unwrap_or(0) {
                    1 => BoostKind::Red,
                    2 => BoostKind::Gold,
                    _ => BoostKind::Blue,
                };
                lvl.boosters.push(Booster { kind, placement });
            } else {
                lvl.props.push(placement);
            }
        }
    }

    for o in load_list(lux, &format!("{code}_startpoints")).unwrap_or_default() {
        if let Some(p) = o.vec3("Position") {
            lvl.starts.push((p, o.quat("Orientation").unwrap_or([0.0, 0.0, 0.0, 1.0])));
        }
    }

    if let Ok(objs) = load_list(lux, &format!("{code}_level")) {
        if let Some(info) = objs.first() {
            lvl.title = info.get("Level Name").unwrap_or_default().to_string();
            for i in 0..4 {
                if let Some(mesh) = info.get(&format!("Skybox {i} Mesh Name")).filter(|m| !m.is_empty()) {
                    let rot = info.f32(&format!("Skybox {i} Rotation")).unwrap_or(0.0);
                    let (s, c) = (rot * 0.5).sin_cos();
                    lvl.skyboxes.push(Placement {
                        mesh: mesh.to_string(),
                        position: [0.0; 3],
                        rotation: [0.0, s, 0.0, c],
                        scale: info.f32(&format!("Skybox {i} Scale")).unwrap_or(1.0),
                        path: None,
                    });
                }
            }
        }
    }

    let sectors = load_list(lux, &format!("{code}_sectors")).unwrap_or_default();
    let edges = edge_table(&sectors);
    for s in sectors.iter().filter(|o| o.class == "CSPropWaterSector") {
        if let (Some(a), Some(b)) = (
            s.get("Leading edge").and_then(|n| edges.get(n)),
            s.get("Trailing edge").and_then(|n| edges.get(n)),
        ) {
            let at = |p: [f32; 3], h: f32| [p[0], h, p[2]];
            lvl.water.push(Quad {
                corners: [at(a.start, a.water), at(a.end, a.water), at(b.end, b.water), at(b.start, b.water)],
            });
        }
    }

    let ai = load_list(lux, &format!("{code}_ai")).unwrap_or_default();
    lvl.path = racing_line(&ai, lvl.starts.first().map(|s| s.0), "CSPropAISector");
    if lvl.path.len() < 3 {
        // No AI line (Revenge of the Nile): the water sectors chain the same way.
        let water = load_list(lux, &format!("{code}_sectors")).unwrap_or_default();
        lvl.path = racing_line(&water, lvl.starts.first().map(|s| s.0), "CSPropWaterSector");
    }
    Ok(lvl)
}

/// Path controllers by name, resolved to their point chains (`CSPathPoint` linked by
/// `Next Point`).
fn motion_paths(objs: &[XmlObject]) -> HashMap<String, MotionPath> {
    let points: HashMap<&str, &XmlObject> =
        objs.iter().filter(|o| o.class == "CSPathPoint").map(|o| (o.name.as_str(), o)).collect();
    let mut out = HashMap::new();
    for c in objs.iter().filter(|o| o.class == "CSEntityController_Path") {
        let Some(start) = c.get("Path Object") else { continue };
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut cur = Some(start);
        let mut looped = false;
        while let Some(name) = cur {
            if !seen.insert(name) {
                looped = true;
                break;
            }
            let Some(p) = points.get(name) else { break };
            if let Some(v) = p.vec3("Position") {
                chain.push(v);
            }
            cur = p.get("Next Point").filter(|n| !n.is_empty());
        }
        if chain.len() < 2 {
            continue;
        }
        let speed = c.f32("Speed In World Units").filter(|v| *v > 0.0).unwrap_or(300.0);
        out.insert(
            c.name.clone(),
            MotionPath {
                points: chain,
                speed,
                looped: looped || c.get("Loop") == Some("1"),
                align: c.get("Align Z Axis") == Some("1"),
            },
        );
    }
    out
}

fn edge_table(objs: &[XmlObject]) -> HashMap<String, Edge> {
    objs.iter()
        .filter(|o| o.class.ends_with("Edge"))
        .filter_map(|o| {
            let start = o.vec3("Edge start")?;
            let end = o.vec3("Edge end")?;
            let water = o.f32("Water height").unwrap_or((start[1] + end[1]) * 0.5);
            Some((o.name.clone(), Edge { start, end, water }))
        })
        .collect()
}

/// Walk the AI sector graph from the sector nearest the start grid, always taking the
/// first successor, until the loop closes or the track ends.
fn racing_line(ai: &[XmlObject], start: Option<[f32; 3]>, sector_class: &str) -> Vec<Edge> {
    let edges = edge_table(ai);
    let sectors: Vec<(&str, &str)> = ai
        .iter()
        .filter(|o| o.class == sector_class)
        .filter_map(|o| Some((o.get("Leading edge")?, o.get("Trailing edge")?)))
        .filter(|(a, b)| edges.contains_key(*a) && edges.contains_key(*b))
        .collect();
    if sectors.is_empty() {
        return Vec::new();
    }
    let mid = |e: &Edge| [(e.start[0] + e.end[0]) * 0.5, e.water, (e.start[2] + e.end[2]) * 0.5];
    let dist2 = |a: [f32; 3], b: [f32; 3]| (a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2) + (a[1] - b[1]).powi(2);
    let from: HashMap<&str, Vec<usize>> = sectors.iter().enumerate().fold(HashMap::new(), |mut m, (i, (a, _))| {
        m.entry(*a).or_default().push(i);
        m
    });
    // Pick the sector whose leading edge is nearest the grid; the grid sits just past it.
    let origin = start.unwrap_or_else(|| mid(&edges[sectors[0].0]));
    let first = (0..sectors.len())
        .min_by(|&a, &b| {
            let da = dist2(mid(&edges[sectors[a].0]), origin);
            let db = dist2(mid(&edges[sectors[b].0]), origin);
            da.total_cmp(&db)
        })
        .unwrap();
    let mut seen = vec![false; sectors.len()];
    let mut line = vec![edges[sectors[first].0]];
    let mut cur = first;
    loop {
        seen[cur] = true;
        let trailing = sectors[cur].1;
        line.push(edges[trailing]);
        let next = from.get(trailing).and_then(|v| v.iter().copied().find(|&i| !seen[i]));
        match next {
            Some(n) => cur = n,
            None => break,
        }
    }
    // Keep each cross-section's start on the same side as its predecessor's.
    for i in 1..line.len() {
        let (p, c) = (line[i - 1], line[i]);
        if dist2(p.start, c.start) + dist2(p.end, c.end) > dist2(p.start, c.end) + dist2(p.end, c.start) {
            line[i] = Edge { start: c.end, end: c.start, water: c.water };
        }
    }
    line
}
