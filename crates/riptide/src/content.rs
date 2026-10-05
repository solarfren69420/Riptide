//! Asset access for the game: the two archives, the boat roster, and a cache that turns
//! decoded models into Bevy meshes and materials.

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crate::sheets::{BoatsGame, BoatsRow, BOAT_TUNING, H2BoatdefsRow, TracksGame, TracksRow, BOATS, H2_BOATDEFS, H2_LEVELS, HT_TRACKS, TRACKS, TRACK_PREVIEWS};
use riptide_assets::h2mesh::decode_mesh;
use riptide_assets::ht::HydroThunder;
use riptide_assets::image::RgbaImage;
use riptide_assets::lux::LuxArchive;
use riptide_assets::model::{Blend, Model};
use std::collections::HashMap;
use std::sync::Arc;

/// A selectable boat: its `boats` sheet row and the `h2_boatdefs` row that drives its handling.
#[derive(Clone, Debug)]
pub struct BoatInfo {
    pub row: &'static BoatsRow,
    pub def: &'static H2BoatdefsRow,
    pub name: String,
    pub game: &'static str,
    /// Uniform model scale into H2Overdrive world units.
    pub scale: f32,
    /// `boat_tuning` multipliers: top speed, thrust, turn rate, grip (1 for normal boats).
    pub tune: [f32; 4],
}

impl BoatInfo {
    pub fn index(&self) -> usize {
        BOATS.iter().position(|r| r.id == self.row.id).unwrap_or(0)
    }
}

/// Where a track's course comes from.
#[derive(Clone, Debug)]
pub enum CourseSource {
    /// H2Overdrive level code.
    H2(String),
    /// Hydro Thunder track: R2 file and `H*` entry on the disc.
    Ht { file: String, entry: String, laps: u32, water: Option<String>, sky: Option<String> },
    /// Hackworld: open water laid out by the `hackworld` sheet; sky, light and the like from `base`.
    Sandbox { base: String },
}

#[derive(Clone, Debug)]
pub struct TrackChoice {
    pub id: &'static str,
    pub name: String,
    pub game: TracksGame,
    pub source: CourseSource,
}

#[derive(Resource)]
pub struct Content {
    pub lux: Arc<LuxArchive>,
    pub ht: Option<Arc<HydroThunder>>,
    pub boats: Vec<BoatInfo>,
    /// Raceable tracks of both games, in menu order.
    pub tracks: Vec<TrackChoice>,
}

impl Content {
    /// Open the game files at their configured paths (`RIPTIDE_LUX`, `RIPTIDE_GDI`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load() -> anyhow::Result<Self> {
        let lux = Arc::new(LuxArchive::open(&riptide_assets::default_lux_path())?);
        let ht = match HydroThunder::open(&riptide_assets::default_gdi_path()) {
            Ok(h) => Some(Arc::new(h)),
            Err(e) => {
                warn!("Hydro Thunder disc not loaded: {e:#}");
                None
            }
        };
        Ok(Self::from_archives(lux, ht))
    }

    /// Build the roster and course list over already-open game files.
    pub fn from_archives(lux: Arc<LuxArchive>, ht: Option<Arc<HydroThunder>>) -> Self {
        // Every boat row whose status is ok, in menu order.
        let mut rows: Vec<&'static BoatsRow> = BOATS.iter().filter(|r| r.status.is_ok()).collect();
        rows.sort_by_key(|r| r.menu_order);
        let boats = rows
            .into_iter()
            .filter(|r| r.game != BoatsGame::Ht || ht.is_some())
            .filter_map(|r| {
                Some(BoatInfo {
                    row: r,
                    def: &H2_BOATDEFS[r.boatdef?],
                    name: r.display_name.to_string(),
                    game: if r.game == BoatsGame::H2 { "H2Overdrive" } else { "Hydro Thunder" },
                    scale: r.scale,
                    tune: BOAT_TUNING
                        .iter()
                        .find(|t| t.boat.is_some_and(|b| BOATS[b].id == r.id))
                        .map_or([1.0; 4], |t| [t.speed_mult, t.thrust_mult, t.turn_mult, t.grip_mult]),
                })
            })
            .collect();
        // Every track whose racing line and geometry are ok, in menu order.
        let mut rows: Vec<&'static TracksRow> = TRACKS.iter().filter(|t| t.racing_line.is_ok() && t.geometry.is_ok()).collect();
        rows.sort_by_key(|t| t.menu_order);
        let tracks = rows
            .into_iter()
            .filter_map(|t| {
                let source = match t.game {
                    TracksGame::H2 => CourseSource::H2(H2_LEVELS[t.h2_level?].id.to_string()),
                    TracksGame::Sandbox => CourseSource::Sandbox { base: H2_LEVELS[t.h2_level?].id.to_string() },
                    TracksGame::Ht => {
                        ht.as_ref()?;
                        let row = &HT_TRACKS[t.ht_track?];
                        let (file, entry) = row.track.strip_prefix("httrack:")?.split_once('/')?;
                        CourseSource::Ht {
                            file: file.into(),
                            entry: entry.into(),
                            laps: row.laps.max(1) as u32,
                            water: row.water.split_once('/').map(|(_, t)| t.to_string()),
                            sky: row.sky.strip_prefix("ht:").map(str::to_string),
                        }
                    }
                };
                Some(TrackChoice { id: t.id, name: t.display_name.to_string(), game: t.game, source })
            })
            .collect();
        Self { lux, ht, boats, tracks }
    }
}

/// A model with its skeleton: pieces ride bones (see [`crate::boatrig`]).
pub struct Rig {
    pub pieces: Vec<Piece>,
    pub bones: Vec<riptide_assets::model::Bone>,
}

/// One drawable piece of a cached model.
#[derive(Clone)]
pub struct Piece {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
    /// H2Overdrive boat parts: the original boat shader (crate::boatshader), drawn instead of `material`.
    pub boat: Option<Handle<crate::boatshader::BoatMaterial>>,
    /// H2Overdrive two-texture terrain (crate::terrain), drawn instead of `material`.
    pub terrain: Option<Handle<crate::terrain::TerrainMat>>,
    /// H2Overdrive lightmapped parts (`OP_*L*`): the `_LM` texture, read on the mesh's UV_1.
    pub lightmap: Option<Handle<Image>>,
    /// Rigged models: the bone the piece rides (its vertices are in that bone's space).
    pub bone: Option<u16>,
}

impl Piece {
    /// Give `e` this piece's material: the original boat shader or two-texture terrain where the
    /// piece has one, else its standard material.
    pub fn apply(&self, e: &mut bevy::ecs::system::EntityCommands) {
        if let Some(lm) = &self.lightmap {
            e.insert(bevy::pbr::Lightmap { image: lm.clone(), uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0), bicubic_sampling: false });
        }
        match (&self.boat, &self.terrain) {
            (Some(b), _) => e.insert(MeshMaterial3d(b.clone())),
            (None, Some(t)) => e.insert(MeshMaterial3d(t.clone())),
            _ => e.insert(MeshMaterial3d(self.material.clone())),
        };
    }
}

#[derive(Resource, Default)]
pub struct ModelCache {
    models: HashMap<String, Option<Arc<Vec<Piece>>>>,
    rigs: HashMap<String, Option<Arc<Rig>>>,
    clips: HashMap<String, Option<Arc<riptide_assets::h2anim::Clip>>>,
    textures: HashMap<String, Option<(Handle<Image>, TexAlpha)>>,
    deferred: Vec<DeferredMaterial>,
    /// Stand-in textures for the boat shader: white, black, a flat normal map, a grey cube.
    boat_defaults: Option<[Handle<Image>; 4]>,
}

struct DeferredMaterial {
    handle: Handle<StandardMaterial>,
    source: Source,
    texture: String,
    shader: String,
    blend: Blend,
}

impl ModelCache {
    /// Drop Hydro Thunder entries: each track R2 reuses names for its own art.
    pub fn forget_ht(&mut self) {
        self.models.retain(|k, _| !k.starts_with("ht:"));
        self.textures.retain(|k, _| !k.starts_with("ht:"));
        self.deferred.retain(|m| m.source != Source::Ht);
    }
}

/// Boats are lit at runtime and carry near-black vertex colours (specular terms, not
/// prelighting); only keep colour streams bright enough to be baked light.
fn keep_bright_colors(model: &mut Model) {
    for p in &mut model.parts {
        let n = p.colors.len().max(1) as f32;
        let mean = p.colors.iter().map(|c| c[0] + c[1] + c[2]).sum::<f32>() / (3.0 * n);
        if mean < 0.12 {
            p.colors.clear();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TexAlpha {
    None,
    Binary,
    Smooth,
}

#[derive(SystemParam)]
pub struct Models<'w> {
    pub content: Res<'w, Content>,
    pub cache: ResMut<'w, ModelCache>,
    pub meshes: ResMut<'w, Assets<Mesh>>,
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub images: ResMut<'w, Assets<Image>>,
    /// Water materials (crate::water).
    pub water: ResMut<'w, Assets<crate::water::WaterMat>>,
    /// H2Overdrive's own water shader (crate::h2water) and the shaders it translates into.
    pub h2water: ResMut<'w, Assets<crate::h2water::H2WaterMaterial>>,
    pub waterfalls: ResMut<'w, Assets<crate::h2water::WaterfallMaterial>>,
    pub boats: ResMut<'w, Assets<crate::boatshader::BoatMaterial>>,
    pub terrains: ResMut<'w, Assets<crate::terrain::TerrainMat>>,
    pub shaders: ResMut<'w, Assets<Shader>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Lux,
    Ht,
}

impl Models<'_> {
    /// Original H2 select artwork. Other courses get a rendered scene in the menu.
    pub fn track_preview(&mut self, choice: &TrackChoice) -> Option<(Handle<Image>, Option<Rect>)> {
        if let Some(row) = TRACK_PREVIEWS.iter().find(|r| r.track.is_some_and(|i| TRACKS[i].id == choice.id)) {
            let image = self.lux_texture(row.image.strip_prefix("lux:txtr1.")?)?;
            let size = self.images.get(&image)?.size();
            let rect = Rect::new(row.x as f32, row.y as f32,
                ((row.x + row.width) as f32).min(size.x as f32), ((row.y + row.height) as f32).min(size.y as f32));
            return Some((image, Some(rect)));
        }
        None
    }

    /// A miss may only mean the bytes aren't fetched yet (browser build): don't remember it.
    fn pending(&self) -> bool {
        self.content.lux.pending() || self.content.ht.as_ref().is_some_and(|h| h.pending())
    }

    /// Pieces of an H2Overdrive `mesh32` (name without prefix).
    pub fn lux(&mut self, name: &str) -> Option<Arc<Vec<Piece>>> {
        let key = format!("lux:{name}");
        if let Some(hit) = self.cache.models.get(&key) {
            return hit.clone();
        }
        let model = self
            .content
            .lux
            .get(&format!("mesh32.{name}"))
            .and_then(|b| decode_mesh(name, b).map_err(|e| warn!("{name}: {e:#}")).ok());
        let pieces = model.map(|mut m| {
            // H2Overdrive lighting lives in lightmaps; its vertex colour stream is black. Its alpha is
            // the two-texture terrain blend (crate::terrain): kept as white with that alpha.
            for p in &mut m.parts {
                let two = p.uvs1.len() == p.positions.len() && p.textures.len() >= 2 && p.colors.len() == p.positions.len();
                if two && p.shader.as_deref().is_some_and(|s| s.starts_with("OP_2V")) {
                    p.colors = p.colors.iter().map(|c| [1.0, 1.0, 1.0, c[3]]).collect();
                } else {
                    p.colors.clear();
                }
            }
            Arc::new(self.build(&m, Source::Lux, false))
        });
        if pieces.is_some() || !self.pending() {
            self.cache.models.insert(key, pieces.clone());
        }
        pieces
    }

    /// Pieces of a Hydro Thunder geometry object (or several, comma separated).
    pub fn ht(&mut self, names: &str) -> Option<Arc<Vec<Piece>>> {
        let key = format!("ht:{names}");
        if let Some(hit) = self.cache.models.get(&key) {
            return hit.clone();
        }
        let ht = self.content.ht.clone()?;
        let mut model = Model { name: names.to_string(), ..Default::default() };
        for n in names.split(',') {
            match ht.geometry(n) {
                Ok(m) => model.parts.extend(m.parts),
                Err(e) => warn!("{n}: {e:#}"),
            }
        }
        keep_bright_colors(&mut model);
        let pieces = (!model.parts.is_empty()).then(|| Arc::new(self.build(&model, Source::Ht, false)));
        if pieces.is_some() || !self.pending() {
            self.cache.models.insert(key, pieces.clone());
        }
        pieces
    }

    /// Pieces for an already-decoded Hydro Thunder model (a track's terrain); not cached.
    pub fn ht_model(&mut self, mut model: Model) -> Arc<Vec<Piece>> {
        keep_bright_colors(&mut model);
        Arc::new(self.build(&model, Source::Ht, false))
    }

    /// Unlit variant (sky domes).
    pub fn lux_unlit(&mut self, name: &str) -> Option<Arc<Vec<Piece>>> {
        let key = format!("luxunlit:{name}");
        if let Some(hit) = self.cache.models.get(&key) {
            return hit.clone();
        }
        let model = self.content.lux.get(&format!("mesh32.{name}")).and_then(|b| decode_mesh(name, b).ok());
        let pieces = model.map(|mut m| {
            for p in &mut m.parts {
                p.colors.clear();
            }
            Arc::new(self.build(&m, Source::Lux, true))
        });
        if pieces.is_some() || !self.pending() {
            self.cache.models.insert(key, pieces.clone());
        }
        pieces
    }

    /// An H2Overdrive `mesh32` with its skeleton (name without prefix), for animating.
    pub fn lux_rig(&mut self, name: &str) -> Option<Arc<Rig>> {
        let key = name.to_ascii_lowercase();
        if let Some(hit) = self.cache.rigs.get(&key) {
            return hit.clone();
        }
        let model = self
            .content
            .lux
            .get(&format!("mesh32.{name}"))
            .and_then(|b| riptide_assets::h2mesh::decode_rigged(name, b).map_err(|e| warn!("{name}: {e:#}")).ok());
        let rig = model.map(|mut m| {
            for p in &mut m.parts {
                p.colors.clear();
            }
            let pieces = self.build(&m, Source::Lux, false);
            Arc::new(Rig { pieces, bones: m.bones })
        });
        if rig.is_some() || !self.pending() {
            self.cache.rigs.insert(key, rig.clone());
        }
        rig
    }

    /// An H2Overdrive `anim4` clip by name (without prefix).
    pub fn lux_clip(&mut self, name: &str) -> Option<Arc<riptide_assets::h2anim::Clip>> {
        let key = name.to_ascii_lowercase();
        if let Some(hit) = self.cache.clips.get(&key) {
            return hit.clone();
        }
        let clip = self
            .content
            .lux
            .get(&format!("anim4.{name}"))
            .and_then(|b| riptide_assets::h2anim::decode_anim(name, b).map_err(|e| warn!("{name}: {e:#}")).ok())
            .map(Arc::new);
        if clip.is_some() || !self.pending() {
            self.cache.clips.insert(key, clip.clone());
        }
        clip
    }

    pub fn boat(&mut self, info: &BoatInfo) -> Option<Arc<Vec<Piece>>> {
        match info.row.model.split_once(':') {
            Some(("lux", entry)) => self.lux(entry.strip_prefix("mesh32.").unwrap_or(entry)),
            Some(("ht", geometry)) => self.ht(geometry),
            _ => None,
        }
    }

    /// A Hydro Thunder texture (loaded track first, then HYDRODC.R2).
    pub fn ht_texture(&mut self, name: &str) -> Option<Handle<Image>> {
        self.texture(Source::Ht, name).map(|t| t.0)
    }

    /// A diffuse texture from `triton.lux` by name (without `txtr1.`).
    pub fn lux_texture(&mut self, name: &str) -> Option<Handle<Image>> {
        self.texture(Source::Lux, name).map(|t| t.0)
    }

    /// A HUD texture for Photoshop blend mode 7 (Lighten): black drops out, so alpha becomes
    /// the brightest channel (Bevy UI has no blend modes).
    pub fn hud_lighten(&mut self, name: &str) -> Option<Handle<Image>> {
        let key = format!("hudl:{name}");
        if let Some(hit) = self.cache.textures.get(&key) {
            return hit.as_ref().map(|t| t.0.clone());
        }
        let img = self.content.lux.get(&format!("txtr1.{name}")).and_then(|b| riptide_assets::image::decode_txtr(b).ok());
        let out = img.map(|mut img| {
            for px in img.rgba.chunks_exact_mut(4) {
                px[3] = px[3].min(px[0].max(px[1]).max(px[2]));
            }
            (self.images.add(to_bevy_image(img)), TexAlpha::Smooth)
        });
        if out.is_some() || !self.pending() {
            self.cache.textures.insert(key, out.clone());
        }
        out.map(|t| t.0)
    }

    /// A normal map from `triton.lux`: uploaded linear (not sRGB), as normal maps must be.
    pub fn lux_normal_map(&mut self, name: &str) -> Option<Handle<Image>> {
        let key = format!("luxn:{name}");
        if let Some(hit) = self.cache.textures.get(&key) {
            return hit.as_ref().map(|t| t.0.clone());
        }
        let img = self.content.lux.get(&format!("txtr1.{name}")).and_then(|b| riptide_assets::image::decode_txtr(b).ok());
        let out = img.map(|img| {
            let mut image = to_bevy_image(img);
            image.texture_descriptor.format = TextureFormat::Rgba8Unorm;
            (self.images.add(image), TexAlpha::None)
        });
        if out.is_some() || !self.pending() {
            self.cache.textures.insert(key, out.clone());
        }
        out.map(|t| t.0)
    }

    fn texture(&mut self, source: Source, name: &str) -> Option<(Handle<Image>, TexAlpha)> {
        let key = format!("{}:{name}", if source == Source::Lux { "lux" } else { "ht" });
        if let Some(hit) = self.cache.textures.get(&key) {
            return hit.clone();
        }
        let decoded: Option<RgbaImage> = match source {
            Source::Lux => {
                let lux = &self.content.lux;
                let blob = lux.get(&format!("txtr1.{name}")).or_else(|| {
                    // Some references drop the `_com` infix the archive uses for shared art.
                    let (pre, rest) = name.split_once('_')?;
                    lux.get(&format!("txtr1.{pre}_com_{rest}"))
                });
                blob.and_then(|b| riptide_assets::image::decode_txtr(b).ok())
            }
            Source::Ht => self.content.ht.as_ref().and_then(|h| h.texture(name).ok()),
        };
        let out = decoded.map(|img| {
            let alpha = if !img.has_alpha() {
                TexAlpha::None
            } else if img.alpha_is_binary() {
                TexAlpha::Binary
            } else {
                TexAlpha::Smooth
            };
            (self.images.add(to_bevy_image(img)), alpha)
        });
        if out.is_some() || !self.pending() {
            self.cache.textures.insert(key, out.clone());
        }
        out
    }

    /// The original boat shader for an H2Overdrive boat part (`FX_Boat*`), with its paint, light
    /// masks (`_LM`) and normal map (`_N`) from the material's texture slots; adds tangents.
    fn boat_material(&mut self, source: Source, part: &riptide_assets::model::MeshPart, mesh: &mut Mesh) -> Option<Handle<crate::boatshader::BoatMaterial>> {
        if source != Source::Lux || !part.shader.as_deref().is_some_and(|s| s.starts_with("FX_Boat")) || std::env::var("RIPTIDE_BOATSHADER").is_ok_and(|v| v == "0") {
            return None;
        }
        let blob = self.content.lux.get("shad4.FX_BoatLocal")?;
        let sh = crate::boatshader::install(&mut self.shaders, blob)?;
        let paint = self.texture(source, part.texture.as_deref()?)?.0;
        let [white, black, flat, cube] = self.boat_defaults();
        let masks: Vec<Handle<Image>> = part.textures.iter().filter(|t| t.contains("_LM")).filter_map(|t| self.texture(source, t).map(|x| x.0)).collect();
        let normal = part.textures.iter().find(|t| t.ends_with("_N")).and_then(|t| self.lux_normal_map(t)).unwrap_or(flat);
        let n = part.positions.len();
        if part.tangents.len() == n {
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, part.tangents.clone());
        } else if mesh.generate_tangents().is_err() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_TANGENT, vec![[1.0f32, 0.0, 0.0, 1.0]; n]);
        }
        let _ = (white, &black);
        Some(self.boats.add(crate::boatshader::BoatMaterial {
            ps: crate::boatshader::constants(&sh, crate::sheets::physics::BOATSHADER_SPECULAR_EXPONENT),
            paint,
            mask1: masks.first().cloned().unwrap_or(black.clone()),
            mask2: masks.get(1).cloned().unwrap_or(black.clone()),
            mask3: masks.get(2).cloned().unwrap_or(black),
            normal,
            sky: cube,
        }))
    }

    /// H2Overdrive two-texture terrain (`OP_2*`): the second texture on the second UV set, blended by
    /// its alpha x the vertex alpha (crate::terrain). Sets the mesh's UV_1 and white-with-alpha colour.
    fn terrain_material(&mut self, source: Source, part: &riptide_assets::model::MeshPart, mesh: &mut Mesh, base: &StandardMaterial) -> Option<Handle<crate::terrain::TerrainMat>> {
        let n = part.positions.len();
        if source != Source::Lux || !part.shader.as_deref().is_some_and(|s| s.starts_with("OP_2V")) || part.uvs1.len() != n || part.colors.len() != n {
            return None;
        }
        // On by default; RIPTIDE_TERRAIN2=0 draws the first texture only.
        if std::env::var("RIPTIDE_TERRAIN2").is_ok_and(|v| v == "0") {
            return None;
        }
        let second = self.texture(source, part.textures.get(1)?)?.0;
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, part.colors.clone());
        Some(self.terrains.add(crate::terrain::TerrainMat { base: base.clone(), extension: crate::terrain::TerrainExt { second, params: Vec4::ZERO } }))
    }

    fn boat_defaults(&mut self) -> [Handle<Image>; 4] {
        if let Some(d) = &self.cache.boat_defaults {
            return d.clone();
        }
        use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension};
        let one = Extent3d { width: 1, height: 1, depth_or_array_layers: 1 };
        let px = |c: [u8; 4], f: TextureFormat| Image::new_fill(one, TextureDimension::D2, &c, f, RenderAssetUsages::RENDER_WORLD);
        // The original binds `pt_tst_skytest_08` (a 256x256 lake-and-mountains picture) as the boats'
        // reflection cube: the same picture on every face.
        let mut cube = match self.content.lux.get("txtr1.pt_tst_skytest_08").and_then(|b| riptide_assets::image::decode_txtr(b).ok()) {
            Some(img) => {
                let (w, h) = (img.width, img.height);
                let data: Vec<u8> = (0..6).flat_map(|_| img.rgba.iter().copied()).collect();
                Image::new(Extent3d { width: w, height: h, depth_or_array_layers: 6 }, TextureDimension::D2, data, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD)
            }
            None => Image::new_fill(Extent3d { depth_or_array_layers: 6, ..one }, TextureDimension::D2, &[150, 160, 175, 255], TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::RENDER_WORLD),
        };
        cube.texture_view_descriptor = Some(TextureViewDescriptor { dimension: Some(TextureViewDimension::Cube), ..default() });
        let d = [
            self.images.add(px([255, 255, 255, 255], TextureFormat::Rgba8UnormSrgb)),
            self.images.add(px([0, 0, 0, 255], TextureFormat::Rgba8UnormSrgb)),
            self.images.add(px([128, 128, 255, 128], TextureFormat::Rgba8Unorm)),
            self.images.add(cube),
        ];
        self.cache.boat_defaults = Some(d.clone());
        d
    }

    fn build(&mut self, model: &Model, source: Source, unlit: bool) -> Vec<Piece> {
        let mut out = Vec::new();
        // H2Overdrive props are drawn in passes over the same triangles: D_Opaque (depth only), L_VV
        // (vertex-colour light), then the textured pass. Drawn as separate opaque surfaces they
        // z-fought (grey and black speckle); keep only the textured pass where one matches.
        let textured: std::collections::HashSet<(usize, usize)> =
            model.parts.iter().filter(|p| p.texture.is_some()).map(|p| (p.positions.len(), p.indices.len())).collect();
        for part in &model.parts {
            if part.indices.is_empty() {
                continue;
            }
            let pass_only = matches!(part.shader.as_deref(), Some("D_Opaque") | Some("L_VV"));
            if source == Source::Lux && pass_only && part.texture.is_none() && textured.contains(&(part.positions.len(), part.indices.len())) {
                continue;
            }
            let n = part.positions.len();
            let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::RENDER_WORLD);
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, part.positions.clone());
            let normals = if part.normals.len() == n { part.normals.clone() } else { vec![[0.0, 1.0, 0.0]; n] };
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
            let uvs = if part.uvs.len() == n { part.uvs.clone() } else { vec![[0.0, 0.0]; n] };
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            if part.colors.len() == n {
                // Pre-lit colour: brighten so mid-grey prelighting reads as neutral.
                let c: Vec<[f32; 4]> =
                    part.colors.iter().map(|c| [(c[0] * 1.6).min(1.0), (c[1] * 1.6).min(1.0), (c[2] * 1.6).min(1.0), 1.0]).collect();
                mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, c);
            }
            mesh.insert_indices(Indices::U32(part.indices.clone()));

            let tex = part.texture.as_deref().and_then(|t| self.texture(source, t));
            let shader = part.shader.as_deref().unwrap_or("");
            let alpha_mode = material_alpha(source, part.texture.as_deref().unwrap_or(""), shader, part.blend, tex.as_ref().map(|t| t.1));
            // Sky effect layers (London's lightning flashes: FX_Textured, white on black) add light.
            let alpha_mode = if unlit && shader.starts_with("FX_") { AlphaMode::Add } else { alpha_mode };
            let deferred = tex.is_none() && part.texture.is_some() && self.pending();
            // Lightmapped H2Overdrive parts (OP_OLP, OP_2LP, OP_CLP ...), opt-in (RIPTIDE_LIGHTMAPS=1): on the software
            // renderer meshes with UV_1 drew nothing through the terrain extension (CHECKLIST.md). The
            // `_LM` texture on the second
            // UV set (a 0..1 atlas) adds its baked light, as the original pixel program does.
            let lightmap = (source == Source::Lux
                && !unlit
                && part.uvs1.len() == n
                && part.shader.as_deref().is_some_and(|s| s.len() >= 5 && s.starts_with("OP_") && s.as_bytes()[4] == b'L')
                && std::env::var("RIPTIDE_LIGHTMAPS").is_ok_and(|v| v == "1"))
                .then(|| part.textures.iter().find(|t| t.contains("_LM")).cloned())
                .flatten()
                .and_then(|t| self.texture(source, &t).map(|x| x.0));
            if lightmap.is_some() {
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, part.uvs1.clone());
                debug!("lightmap {:?} on {:?}", part.textures.iter().find(|t| t.contains("_LM")), part.shader);
            }
            let material = StandardMaterial {
                base_color: if tex.is_some() { Color::WHITE } else { Color::srgb(0.55, 0.55, 0.58) },
                base_color_texture: tex.map(|t| t.0),
                alpha_mode,
                perceptual_roughness: 0.85,
                reflectance: 0.2,
                double_sided: part.double_sided || unlit,
                cull_mode: if part.double_sided || unlit { None } else { Some(bevy::render::render_resource::Face::Back) },
                unlit,
                fog_enabled: !unlit,
                ..default()
            };
            let handle = self.materials.add(material.clone());
            let boat = self.boat_material(source, part, &mut mesh);
            // Not on sky domes (unlit): their materials stay as they are.
            let terrain = match &lightmap {
                // Through the terrain extension (crate::terrain, lightmap mode): Bevy's own Lightmap
                // component showed no effect.
                Some(lm) => Some(self.terrains.add(crate::terrain::TerrainMat {
                    base: material.clone(),
                    extension: crate::terrain::TerrainExt { second: lm.clone(), params: Vec4::new(1.0, crate::sheets::physics::LIGHTMAP_STRENGTH, 0.0, 0.0) },
                })),
                None if unlit => None,
                None => self.terrain_material(source, part, &mut mesh, &material),
            };
            let lightmap: Option<Handle<Image>> = None;
            if deferred {
                self.cache.deferred.push(DeferredMaterial {
                    handle: handle.clone(), source, texture: part.texture.clone().unwrap(), shader: shader.to_string(), blend: part.blend,
                });
            }
            out.push(Piece { mesh: self.meshes.add(mesh), material: handle, bone: part.bone, boat, terrain, lightmap });
        }
        out
    }
}

/// Repair already-spawned materials when a browser's local file read finishes.
pub fn finish_textures(mut models: Models) {
    for m in std::mem::take(&mut models.cache.deferred) {
        if let Some((image, alpha)) = models.texture(m.source, &m.texture) {
            if let Some(material) = models.materials.get_mut(&m.handle) {
                material.base_color = Color::WHITE;
                material.base_color_texture = Some(image);
                material.alpha_mode = material_alpha(m.source, &m.texture, &m.shader, m.blend, Some(alpha));
            }
        } else if models.pending() {
            models.cache.deferred.push(m);
        } else {
            warn!("texture {} could not be decoded", m.texture);
        }
    }
}

fn material_alpha(source: Source, texture: &str, shader: &str, blend: Blend, alpha: Option<TexAlpha>) -> AlphaMode {
    let is_prop_tex = source == Source::Lux && texture.to_ascii_lowercase().starts_with("pt_");
    match (blend, alpha) {
        (Blend::Add, _) => AlphaMode::Add,
        (Blend::Blend, _) => AlphaMode::Blend,
        (Blend::Cutout, _) => AlphaMode::Mask(0.5),
        (Blend::Solid, _) => AlphaMode::Opaque,
        // Boat and world texture alpha stores gloss. Prop art and shaders ending
        // in A use it for cutout coverage instead.
        (_, Some(TexAlpha::Binary)) if source == Source::Ht || shader.ends_with('A') || is_prop_tex => AlphaMode::Mask(0.5),
        (_, Some(TexAlpha::Smooth)) if source == Source::Ht => AlphaMode::Mask(0.4),
        (_, Some(TexAlpha::Smooth)) if shader.ends_with('A') || is_prop_tex => AlphaMode::Mask(0.35),
        _ => AlphaMode::Opaque,
    }
}

/// Upload an RGBA image with a CPU-built mip chain (box filter), capped at 1024 px.
pub fn to_bevy_image(mut img: RgbaImage) -> Image {
    while img.width > 1024 || img.height > 1024 {
        img = half(&img);
    }
    let (w, h) = (img.width, img.height);
    let mut data = img.rgba.clone();
    let mut levels = 1;
    let mut cur = img;
    while cur.width > 1 || cur.height > 1 {
        cur = half(&cur);
        data.extend_from_slice(&cur.rgba);
        levels += 1;
    }
    let mut image = Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data[..(w * h * 4) as usize].to_vec(),
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(repeat_sampler());
    image
}

pub fn repeat_sampler() -> ImageSamplerDescriptor {
    ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    }
}

fn half(img: &RgbaImage) -> RgbaImage {
    let nw = (img.width / 2).max(1);
    let nh = (img.height / 2).max(1);
    let mut out = vec![0u8; (nw * nh * 4) as usize];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let mut s = 0u32;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(img.width - 1);
                    let sy = (y * 2 + dy).min(img.height - 1);
                    s += img.rgba[((sy * img.width + sx) * 4 + c) as usize] as u32;
                }
                out[((y * nw + x) * 4 + c) as usize] = (s / 4) as u8;
            }
        }
    }
    RgbaImage { width: nw, height: nh, rgba: out }
}

/// Spawn `pieces` as children of `parent`.
pub fn attach(commands: &mut Commands, parent: Entity, pieces: &[Piece]) {
    commands.entity(parent).with_children(|c| {
        for p in pieces {
            let mut e = c.spawn((Mesh3d(p.mesh.clone()), Transform::IDENTITY));
            p.apply(&mut e);
        }
    });
}
