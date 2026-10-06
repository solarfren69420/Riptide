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

/// [`load_list`] with every declared property present (its `DEFAULT` comment when unset): the
/// game's own values, not ours.
pub fn load_list_with_defaults(lux: &LuxArchive, name: &str) -> Result<Vec<XmlObject>> {
    let blob = lux.get(&format!("data.{name}")).with_context(|| format!("data.{name} missing"))?;
    parse_object_list_with_defaults(&String::from_utf8_lossy(blob))
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub mesh: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
    /// Set when a path controller drives the object (traffic, trains, gondolas).
    pub path: Option<MotionPath>,
    /// A looping clip on the object's own skeleton (`Animation`: sawblades, spike blocks,
    /// cranes, wildlife).
    pub anim: Option<PropAnim>,
    /// `CSEntityController_Rotate`: turning about a local axis (signs, wheels, fans).
    pub spin: Option<Spin>,
    /// `CSEntityController_Slide`: sliding back and forth along a local axis.
    pub slide: Option<Slide>,
    /// A physics object (mesh def Physics Type 1 or 2): boats collide with it and ride over it.
    pub solid: bool,
    /// Physics Type 2 (logs, rafts, crates, houseboats): it floats and boats push it; its `Coll Mass`.
    pub float_mass: Option<f32>,
    /// Anchored on a bungee (buoys: mesh def `Bungee Force XZ`): knocked aside when hit, springing back.
    pub bungee: Option<f32>,
}

#[derive(Debug, Clone, Copy)]
pub struct Spin {
    /// Local axis, Z mirrored like everything else.
    pub axis: [f32; 3],
    pub revs_per_second: f32,
    /// Starting turn, a fraction of a revolution.
    pub phase: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Slide {
    pub axis: [f32; 3],
    /// How far it travels from its placed position (units).
    pub distance: f32,
    pub round_trips_per_second: f32,
    pub phase: f32,
    /// Interpolation Type 1: eased at the ends; 0: constant speed.
    pub eased: bool,
}

#[derive(Debug, Clone)]
pub struct PropAnim {
    /// `anim4` entry name.
    pub clip: String,
    /// `Anim Speed x`: playback rate (1 = the clip's own).
    pub speed: f32,
    /// `Anim Start`: phase to start at, as a fraction of the clip.
    pub start: f32,
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

/// A `CSPropWaterEdge`: a line across the river with the water's look and waves there
/// (H2Overdrive blends a sector's leading and trailing edge across it, shad4.FX_Water2).
/// Positions and directions are in Riptide space (Z mirrored, like every level position).
#[derive(Debug, Clone, Default)]
pub struct WaterEdge {
    pub name: String,
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub water: f32,
    /// Calm, Normal, Choppy, Stormy, Rapids, Tidal or Wrappers.
    pub wave_type: String,
    pub wave_intensity: f32,
    /// Degrees, as authored (original space).
    pub wave_direction: f32,
    pub bump_direction: f32,
    pub bump_speed: f32,
    pub flow_speed: f32,
    pub opaqueness: f32,
    /// `Real` (planar reflection) or another mode.
    pub reflection_type: String,
    /// RGBA motif colours.
    pub water_color: [f32; 4],
    pub whitewash_color: [f32; 4],
    pub specular_color: [f32; 4],
    pub reflection_tint: [f32; 4],
    /// The waterfall the original draws where the water drops after this edge (`Waterfall *`).
    pub waterfall: WaterfallFields,
    pub light: Option<WaterLight>,
}

/// An edge's `Waterfall *` properties (engine defaults when unset).
#[derive(Debug, Clone, Copy, Default)]
pub struct WaterfallFields {
    pub disable: bool,
    pub width: f32,
    pub speed: f32,
    pub flare: f32,
    pub curve: f32,
    pub light_scale: f32,
    pub color: [f32; 4],
}

/// A `CSPropLightEdgeInfo` (an edge's `Light Info Ob`): one directional light plus ambient.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaterLight {
    /// Direction the light shines, Riptide space.
    pub direction: [f32; 3],
    pub color: [f32; 4],
    pub intensity: f32,
    pub ambient: [f32; 4],
    pub ambient_intensity: f32,
}

/// A `CSPropWaterSector`: indices into `H2Level::water_edges`.
#[derive(Debug, Clone, Copy)]
pub struct WaterSector {
    pub leading: usize,
    pub trailing: usize,
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

/// One action of a level tripwire (`<code>_tripwires`, slots 1..4): a script verb (Fire, Tripwire,
/// StartAnim, CameraShake, TidalWave, Skybox, PlaySound2D, ...) on a referenced object, with its
/// blind data and delay.
#[derive(Clone, Debug, Default)]
pub struct TripAction {
    pub script: String,
    pub target: String,
    pub int: i32,
    pub float: f32,
    pub delay: f32,
}

/// A level tripwire: crossing it runs its actions (the dam collapse, eruptions, rock slides, sky
/// swaps). Trigger Shape 4 is a plane across the course through `position`.
#[derive(Clone, Debug, Default)]
pub struct Tripwire {
    pub name: String,
    pub position: [f32; 3],
    pub scale: f32,
    pub shape: i32,
    pub actions: Vec<TripAction>,
}

/// A geyser (CGeyser): its flame def erupts on a cycle, Off -> Low (a trickle) -> High (the
/// eruption, which throws boats over it), starting at `phase` seconds in.
#[derive(Clone, Debug)]
pub struct Geyser {
    pub def: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub radius: f32,
    pub off: f32,
    pub low: f32,
    pub high: f32,
    pub low_intensity: f32,
    pub high_intensity: f32,
    pub phase: f32,
    pub random_phase: bool,
}

/// A level sound (`<code>_sounds`): a positional loop (CSSound: full volume inside `inner`, silent
/// beyond `outer`) or a one-shot gate (CSMusicTripwire with a Sound Def: the Tarzan yell on Wild
/// America's final drop).
#[derive(Clone, Debug)]
pub struct LevelSound {
    pub name: String,
    pub sound: String,
    pub position: [f32; 3],
    pub volume: f32,
    /// `Some((inner, outer))` for a positional loop, `None` for a gate.
    pub radii: Option<(f32, f32)>,
    /// An announcer voice-over (CVoiceOver): its gate is tight, so a secret's line plays only on the secret.
    pub voice: bool,
}

/// A level fire, smoke or torch (`<code>_Fire`, CRocketFlameEntity): a rocket flame def run at a
/// fixed spot, emitting along its local +Z (Riptide -Z after the mirror).
#[derive(Clone, Debug)]
pub struct LevelFire {
    pub name: String,
    pub def: String,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: f32,
}

#[derive(Debug, Clone, Default)]
pub struct H2Level {
    pub code: String,
    pub fires: Vec<LevelFire>,
    pub sounds: Vec<LevelSound>,
    pub geysers: Vec<Geyser>,
    pub tripwires: Vec<Tripwire>,
    pub title: String,
    /// Terrain meshes (`sg_<code>_PropSectorN`), placed at the origin.
    pub sector_meshes: Vec<String>,
    pub props: Vec<Placement>,
    pub boosters: Vec<Booster>,
    pub starts: Vec<([f32; 3], [f32; 4])>,
    /// Racing line: AI sector cross-sections in driving order (first lap).
    pub path: Vec<Edge>,
    pub water: Vec<Quad>,
    /// Water edges and the sectors between them (the original's water shader inputs).
    pub water_edges: Vec<WaterEdge>,
    pub water_sectors: Vec<WaterSector>,
    /// Vertical curtains at river drops (generated for Hydro Thunder sector portals).
    pub waterfalls: Vec<Quad>,
    pub skyboxes: Vec<Placement>,
    /// Centre of the finish-line buoys (`*Finish*` game meshes), when the level has them.
    pub finish: Option<[f32; 3]>,
    /// Every finish buoy, for the line between them.
    pub finish_buoys: Vec<[f32; 3]>,
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

    let defs = load_list_with_defaults(lux, "global_GameMeshDef").unwrap_or_default();
    let mesh_defs: HashMap<String, String> =
        defs.iter().filter_map(|o| Some((o.name.clone(), o.get("Mesh Name 1").filter(|m| !m.is_empty())?.to_string()))).collect();
    // Physics Type 1 (moored boats) and 2 (floating logs, rafts, crates, houseboats) are things
    // boats hit and ride over; 0 is scenery.
    let solid_defs: std::collections::HashSet<String> =
        defs.iter().filter(|o| matches!(o.f32("Physics Type").map(|t| t as i32), Some(1 | 2))).map(|o| o.name.clone()).collect();
    let bungee_defs: HashMap<String, f32> =
        defs.iter().filter_map(|o| Some((o.name.clone(), o.f32("Bungee Force XZ").filter(|f| *f > 0.0)?))).collect();
    let float_defs: HashMap<String, f32> = defs
        .iter()
        .filter(|o| o.f32("Physics Type").map(|t| t as i32) == Some(2))
        .map(|o| (o.name.clone(), o.f32("Coll Mass").unwrap_or(1000.0)))
        .collect();

    if let Ok(objs) = load_list_with_defaults(lux, &format!("{code}_worldobs")) {
        let paths = motion_paths(&objs);
        let spins = spin_controllers(&objs);
        let slides = slide_controllers(&objs);
        let buoys: Vec<[f32; 3]> = objs
            .iter()
            // Finish buoys only: Wild America also has `Finish_line_Balloons_01` decorations elsewhere.
            .filter(|o| { let n = o.name.to_ascii_lowercase(); n.contains("finish") && n.contains("buoy") })
            .filter_map(|o| o.vec3("Position"))
            .collect();
        if !buoys.is_empty() {
            let n = buoys.len() as f32;
            lvl.finish = Some([0, 1, 2].map(|k| buoys.iter().map(|b| b[k]).sum::<f32>() / n));
            lvl.finish_buoys = buoys.clone();
        }
        for o in &objs {
            if o.class == "CGeyser" {
                if let (Some(def), Some(position)) = (o.get("Geyser Flame Def").filter(|d| !d.is_empty()), o.vec3("Position")) {
                    lvl.geysers.push(Geyser {
                        def: def.to_string(),
                        position,
                        rotation: o.quat("Orientation").unwrap_or([0.0, 0.0, 0.0, 1.0]),
                        radius: o.f32("Scale").unwrap_or(40.0),
                        off: o.f32("Off Sec").unwrap_or(3.0),
                        low: o.f32("Low Sec").unwrap_or(1.5),
                        high: o.f32("High Sec").unwrap_or(3.0),
                        low_intensity: o.f32("Low Unit Intensity").unwrap_or(0.0),
                        high_intensity: o.f32("High Unit Intensity").unwrap_or(1.0),
                        phase: o.f32("Phase Sec").unwrap_or(0.0),
                        random_phase: o.get("Random Phase").is_some_and(|v| v.trim() == "1"),
                    });
                }
                continue;
            }
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
            let (spin, slide) = (controlled(o, &spins), controlled(o, &slides));
            let anim = o.get("Animation").filter(|a| !a.is_empty()).map(|clip| PropAnim {
                clip: clip.to_string(),
                speed: o.f32("Anim Speed x").unwrap_or(1.0),
                start: o.f32("Anim Start").unwrap_or(0.0),
            });
            let solid = o.get("Game Mesh Def").is_some_and(|d| solid_defs.contains(d));
            let float_mass = o.get("Game Mesh Def").and_then(|d| float_defs.get(d)).copied();
            let bungee = o.get("Game Mesh Def").and_then(|d| bungee_defs.get(d)).copied().filter(|_| float_mass.is_none());
            let placement = Placement { mesh, position, rotation, scale, path, anim, spin, slide, solid, float_mass, bungee };
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

    for o in load_list_with_defaults(lux, &format!("{code}_sounds")).unwrap_or_default() {
        // Voice-overs (CVoiceOver): a Message number 0..13 into the announcer's lines, matched by the
        // objects' own names across every level (message 6, the default, is both the secrets and
        // the shortcuts: picked by name). Played once, like a gate.
        if o.class == "CVoiceOver" {
            let msg = o.get("Message").and_then(|m| m.trim().parse::<i32>().ok()).unwrap_or(6);
            let line = match msg {
                1 => Some("com_StayOnTarget"),
                2 => Some("com_TightTurnsAhead"),
                3 => Some("com_DontLikeLooksOfThis"),
                4 => Some("com_FinishLineAhead"),
                5 => Some("com_GetThatBooster"),
                6 if o.name.to_ascii_lowercase().contains("shortcut") => Some("com_HeyAShortcut"),
                6 => Some("com_YouFoundASecret"),
                8 => Some("com_ThisLooksTricky"),
                9 => Some("com_OhSoClose"),
                10 => Some("com_StaySharp"),
                _ => None,
            };
            if let (Some(line), Some(position)) = (line, o.vec3("Position")) {
                lvl.sounds.push(LevelSound { name: o.name.clone(), sound: line.to_string(), position, volume: 1.0, radii: None, voice: true });
            }
            continue;
        }
        let (Some(sound), Some(position)) = (o.get("Sound Def").filter(|d| !d.is_empty()), o.vec3("Position")) else { continue };
        let radii = match o.class.as_str() {
            "CSSound" => {
                let outer = o.f32("OuterRadiusOverride").filter(|r| *r > 0.0).or(o.f32("Scale")).unwrap_or(1000.0);
                Some((o.f32("InnerRadiiOverride").unwrap_or(outer * 0.5).min(outer), outer))
            }
            "CSMusicTripwire" => None,
            _ => continue,
        };
        lvl.sounds.push(LevelSound { name: o.name.clone(), sound: sound.to_string(), position, volume: o.f32("Volume").unwrap_or(1.0), radii, voice: false });
    }

    for o in load_list_with_defaults(lux, &format!("{code}_tripwires")).unwrap_or_default() {
        let Some(position) = o.vec3("Position") else { continue };
        let int = |k: &str| o.get(k).and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(0);
        let actions = ["", " 2", " 3", " 4"]
            .iter()
            .filter_map(|n| {
                let script = o.get(&format!("Script Name{n}"))?.trim();
                if script.is_empty() || script == "(none)" {
                    return None;
                }
                Some(TripAction {
                    script: script.to_string(),
                    target: o.get(&format!("Reference Object{n}")).unwrap_or("").trim().to_string(),
                    int: int(&format!("Blind Int Data{n}")),
                    float: o.f32(&format!("Blind Float Data{n}")).unwrap_or(0.0),
                    delay: o.f32(&format!("Delay Seconds{n}")).unwrap_or(0.0),
                })
            })
            .collect();
        lvl.tripwires.push(Tripwire {
            name: o.name.clone(),
            position,
            scale: o.f32("Scale").unwrap_or(1.0),
            shape: int("Trigger Shape"),
            actions,
        });
    }

    for o in load_list_with_defaults(lux, &format!("{code}_Fire")).unwrap_or_default() {
        let (Some(def), Some(position)) = (o.get("FlameDef").filter(|d| !d.is_empty()), o.vec3("Position")) else { continue };
        lvl.fires.push(LevelFire {
            name: o.name.clone(),
            def: def.to_string(),
            position,
            rotation: o.quat("Orientation").unwrap_or([0.0, 0.0, 0.0, 1.0]),
            scale: o.f32("Scale").unwrap_or(1.0),
        });
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
                        anim: None,
                        spin: None,
                        slide: None,
                        solid: false,
                        float_mass: None,
                        bungee: None,
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

    water_sectors(lux, code, &mut lvl);

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
    // Entity paths and game-mesh paths (London's paddleboats, Hong Kong's traffic) share the fields.
    for c in objs.iter().filter(|o| o.class == "CSEntityController_Path" || o.class == "CGMController_Path") {
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

/// Controller axis `0` / `1` / `2` (X / Y / Z) in output space (Z mirrored).
fn axis(i: i32) -> [f32; 3] {
    match i {
        0 => [1.0, 0.0, 0.0],
        1 => [0.0, 1.0, 0.0],
        _ => [0.0, 0.0, -1.0],
    }
}

fn spin_controllers(objs: &[XmlObject]) -> HashMap<String, Spin> {
    objs.iter()
        .filter(|o| o.class == "CSEntityController_Rotate")
        .map(|c| {
            let spin = Spin {
                axis: axis(c.f32("Rotation Axis").unwrap_or(2.0) as i32),
                revs_per_second: c.f32("Revolutions per Second").unwrap_or(0.0),
                phase: c.f32("Phase").unwrap_or(0.0),
            };
            (c.name.clone(), spin)
        })
        .collect()
}

fn slide_controllers(objs: &[XmlObject]) -> HashMap<String, Slide> {
    objs.iter()
        .filter(|o| o.class == "CSEntityController_Slide")
        .map(|c| {
            let slide = Slide {
                axis: axis(c.f32("Slide Axis").unwrap_or(0.0) as i32),
                distance: c.f32("Slide Distance").unwrap_or(0.0),
                round_trips_per_second: c.f32("Round Trips Per Second").unwrap_or(0.0),
                phase: c.f32("Phase").unwrap_or(0.0),
                eased: c.f32("Interpolation Type").unwrap_or(0.0) as i32 == 1,
            };
            (c.name.clone(), slide)
        })
        .collect()
}

/// The first of `o`'s `Controllers` found in `by_name`.
fn controlled<T: Clone>(o: &XmlObject, by_name: &HashMap<String, T>) -> Option<T> {
    o.props.get("Controllers").and_then(|cs| cs.iter().find_map(|c| by_name.get(c.as_str()))).cloned()
}

/// Water edges with every property (engine defaults filled in), their lights, and the sectors.
fn water_sectors(lux: &LuxArchive, code: &str, lvl: &mut H2Level) {
    let Ok(objs) = load_list_with_defaults(lux, &format!("{code}_sectors")) else {
        return;
    };
    let lights: HashMap<String, WaterLight> = load_list_with_defaults(lux, &format!("{code}_lights"))
        .unwrap_or_default()
        .iter()
        .filter(|o| o.class == "CSPropLightEdgeInfo")
        .map(|o| {
            let d = o.floats("Directional Direction").unwrap_or_default();
            let c4 = |k: &str| {
                let v = o.floats(k).unwrap_or_default();
                [0, 1, 2, 3].map(|i| v.get(i).copied().unwrap_or(if i == 3 { 1.0 } else { 0.0 }))
            };
            let light = WaterLight {
                direction: [d.first().copied().unwrap_or(0.0), d.get(1).copied().unwrap_or(-1.0), -d.get(2).copied().unwrap_or(0.0)],
                color: c4("Directional motif color"),
                intensity: o.f32("Directional Intensity").unwrap_or(1.0),
                ambient: c4("Ambient motif color"),
                ambient_intensity: o.f32("Ambient Intensity").unwrap_or(0.0),
            };
            (o.name.clone(), light)
        })
        .collect();
    let mut index = HashMap::new();
    for o in objs.iter().filter(|o| o.class == "CSPropWaterEdge") {
        let (Some(start), Some(end)) = (o.vec3("Edge start"), o.vec3("Edge end")) else { continue };
        let c4 = |k: &str| {
            let v = o.floats(k).unwrap_or_default();
            [0, 1, 2, 3].map(|i| v.get(i).copied().unwrap_or(1.0))
        };
        index.insert(o.name.clone(), lvl.water_edges.len());
        lvl.water_edges.push(WaterEdge {
            name: o.name.clone(),
            start,
            end,
            water: o.f32("Water height").unwrap_or((start[1] + end[1]) * 0.5),
            wave_type: o.get("Wave Type").unwrap_or("Normal").trim().to_string(),
            wave_intensity: o.f32("Wave Intensity").unwrap_or(0.0),
            wave_direction: o.f32("Wave Direction").unwrap_or(0.0),
            bump_direction: o.f32("Bump Direction").unwrap_or(0.0),
            bump_speed: o.f32("Bump Speed").unwrap_or(0.0),
            flow_speed: o.f32("Flow Speed").unwrap_or(0.0),
            opaqueness: o.f32("Opaqueness").unwrap_or(1.0),
            reflection_type: o.get("Reflection Type").unwrap_or("Real").trim().to_string(),
            water_color: c4("Water Color motif color"),
            whitewash_color: c4("Whitewash Color motif color"),
            specular_color: c4("Specular Color motif color"),
            reflection_tint: c4("Reflection Tint motif color"),
            light: o.get("Light Info Ob").and_then(|n| lights.get(n.trim())).copied(),
            waterfall: WaterfallFields {
                disable: o.get("Waterfall Disable").is_some_and(|v| v.trim() == "1"),
                width: o.f32("Waterfall Width").unwrap_or(1.0),
                speed: o.f32("Waterfall Speed").unwrap_or(0.5),
                flare: o.f32("Waterfall Flare").unwrap_or(0.2),
                curve: o.f32("Waterfall Curve").unwrap_or(0.2),
                light_scale: o.f32("Waterfall Light Scale").unwrap_or(1.3),
                color: c4("Waterfall Color motif color"),
            },
        });
    }
    for s in objs.iter().filter(|o| o.class == "CSPropWaterSector") {
        if let (Some(&leading), Some(&trailing)) =
            (s.get("Leading edge").and_then(|n| index.get(n)), s.get("Trailing edge").and_then(|n| index.get(n)))
        {
            lvl.water_sectors.push(WaterSector { leading, trailing });
        }
    }
}
