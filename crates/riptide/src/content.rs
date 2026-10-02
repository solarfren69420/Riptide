//! Asset access for the game: the two archives, the boat roster, and a cache that turns
//! decoded models into Bevy meshes and materials.

use bevy::asset::RenderAssetUsages;
use bevy::ecs::system::SystemParam;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crate::sheets::{BoatsGame, BoatsRow, BOAT_TUNING, H2BoatdefsRow, TracksGame, TracksRow, BOATS, H2_BOATDEFS, H2_LEVELS, HT_TRACKS, TRACKS};
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
        BOATS.iter().position(|r| std::ptr::eq(r, self.row)).unwrap_or(0)
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
    pub fn load() -> anyhow::Result<Self> {
        let lux = Arc::new(LuxArchive::open(&riptide_assets::default_lux_path())?);
        let ht = match HydroThunder::open(&riptide_assets::default_gdi_path()) {
            Ok(h) => Some(Arc::new(h)),
            Err(e) => {
                warn!("Hydro Thunder disc not loaded: {e:#}");
                None
            }
        };
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
                        .find(|t| t.boat.is_some_and(|b| std::ptr::eq(&BOATS[b], r)))
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
        Ok(Self { lux, ht, boats, tracks })
    }
}

/// One drawable piece of a cached model.
#[derive(Clone)]
pub struct Piece {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

#[derive(Resource, Default)]
pub struct ModelCache {
    models: HashMap<String, Option<Arc<Vec<Piece>>>>,
    textures: HashMap<String, Option<(Handle<Image>, TexAlpha)>>,
}

impl ModelCache {
    /// Drop Hydro Thunder entries: each track R2 reuses names for its own art.
    pub fn forget_ht(&mut self) {
        self.models.retain(|k, _| !k.starts_with("ht:"));
        self.textures.retain(|k, _| !k.starts_with("ht:"));
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Lux,
    Ht,
}

impl Models<'_> {
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
            // H2Overdrive lighting lives in lightmaps; its vertex colour stream is black.
            for p in &mut m.parts {
                p.colors.clear();
            }
            Arc::new(self.build(&m, Source::Lux, false))
        });
        self.cache.models.insert(key, pieces.clone());
        pieces
    }

    /// Pieces of a Hydro Thunder geometry object (or several, comma separated).
    pub fn ht(&mut self, names: &str) -> Option<Arc<Vec<Piece>>> {
        let key = format!("ht:{names}");
        if let Some(hit) = self.cache.models.get(&key) {
            return hit.clone();
        }
        let ht = self.content.ht.clone()?;
        let mut model = Model { name: names.to_string(), parts: Vec::new() };
        for n in names.split(',') {
            match ht.geometry(n) {
                Ok(m) => model.parts.extend(m.parts),
                Err(e) => warn!("{n}: {e:#}"),
            }
        }
        keep_bright_colors(&mut model);
        let pieces = (!model.parts.is_empty()).then(|| Arc::new(self.build(&model, Source::Ht, false)));
        self.cache.models.insert(key, pieces.clone());
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
        self.cache.models.insert(key, pieces.clone());
        pieces
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
        self.cache.textures.insert(key, out.clone());
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
        self.cache.textures.insert(key, out.clone());
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
        self.cache.textures.insert(key, out.clone());
        out
    }

    fn build(&mut self, model: &Model, source: Source, unlit: bool) -> Vec<Piece> {
        let mut out = Vec::new();
        for part in &model.parts {
            if part.indices.is_empty() {
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
            let is_prop_tex = source == Source::Lux && part.texture.as_deref().is_some_and(|t| t.to_ascii_lowercase().starts_with("pt_"));
            let shader = part.shader.as_deref().unwrap_or("");
            let alpha_mode = match (part.blend, tex.as_ref().map(|t| t.1)) {
                (Blend::Add, _) => AlphaMode::Add,
                (Blend::Blend, _) => AlphaMode::Blend,
                (Blend::Cutout, _) => AlphaMode::Mask(0.5),
                // H2Overdrive opaque shaders ending in `A` alpha-test their diffuse map, and prop
                // art (`pt_*`: tree cards, fences, signs) is cut out. Boat and world textures use
                // alpha for gloss, so they stay opaque.
                (_, Some(TexAlpha::Binary)) if source == Source::Ht || shader.ends_with('A') || is_prop_tex => AlphaMode::Mask(0.5),
                (_, Some(TexAlpha::Smooth)) if source == Source::Ht => AlphaMode::Mask(0.4),
                (_, Some(TexAlpha::Smooth)) if shader.ends_with('A') || is_prop_tex => AlphaMode::Mask(0.35),
                _ => AlphaMode::Opaque,
            };
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
            out.push(Piece { mesh: self.meshes.add(mesh), material: self.materials.add(material) });
        }
        out
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
            c.spawn((Mesh3d(p.mesh.clone()), MeshMaterial3d(p.material.clone()), Transform::IDENTITY));
        }
    });
}
