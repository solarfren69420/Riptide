//! Riptide: a boat racer built on H2Overdrive and Hydro Thunder assets.
//!
//! Tracks and most boats come from H2Overdrive's `triton.lux`; the classic Hydro Thunder boats
//! are read straight off the Dreamcast GD-ROM image. Paths default to the local installs and can
//! be overridden with `RIPTIDE_LUX` / `RIPTIDE_GDI`.
//!
//! Headless capture (no window): `RIPTIDE_SHOT=out/shot RIPTIDE_SHOT_FRAMES=60,200
//! [RIPTIDE_TRACK=wa] [RIPTIDE_BOAT=Banshee] [RIPTIDE_SHOT_MENU=1]` writes `out/shot_<frame>.png`.
//! Testing: `RIPTIDE_SHOT_SIZE=320x180` (smaller target), `RIPTIDE_SIM_DT=0.05` (fixed game step per
//! frame), `RIPTIDE_EXIT_ON_FINISH=1` (stop when the autopiloted player finishes; logs `RESULT`).
//! `scripts/race-all.sh` races every playable track this way.
//! `RIPTIDE_SHOT_AT=x,y,z,yaw-degrees` places the capture boat/camera at a particular feature.

mod boatrig;
mod boatshader;
mod terrain;
mod cheats;
mod collide;
mod content;
mod effects;
mod floating;
mod hud;
mod controls;
mod menu;
mod nav;
mod net;
mod race;
mod ramps;
mod recovery;
mod sheets;
mod sound;
mod track;
mod h2water;
mod water;
#[cfg(target_arch = "wasm32")]
mod web;

use bevy::camera::{ImageRenderTarget, RenderTarget};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::window::{ExitCondition, WindowResolution};
use content::{Content, ModelCache};

#[derive(States, Default, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Screen {
    #[default]
    Menu,
    /// Browser build: fetching the chosen course from the player's files.
    Loading,
    Race,
    /// Bounces straight back into `Race` so its scoped entities respawn.
    Restart,
}

#[derive(Resource, Default)]
pub struct Selection {
    pub level: usize,
    pub boat: usize,
    /// Headless capture target; `None` renders to the window.
    pub render_target: Option<RenderTarget>,
}

/// The player's boat drives itself (capture mode).
#[derive(Resource)]
pub struct Autopilot;

#[derive(Resource)]
struct ShotPlan {
    prefix: String,
    frames: Vec<u32>,
    image: Handle<Image>,
    frame: u32,
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> AppExit {
    // RIPTIDE_RECORD=<file>: write every triton.lux entry the run reads (the web build's
    // per-course fetch lists are made this way; see scripts/web-manifests.sh).
    let record = std::env::var("RIPTIDE_RECORD").ok();
    if record.is_some() {
        riptide_assets::lux::record();
    }
    let content = match Content::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("riptide: cannot load game data: {e:#}");
            eprintln!("Set RIPTIDE_LUX to H2Overdrive's triton.lux (and RIPTIDE_GDI to a Hydro Thunder .gdi).");
            return AppExit::error();
        }
    };
    run(content)
}

/// In the browser the page starts the game through [`web::start`] once the player has picked
/// their game files.
#[cfg(target_arch = "wasm32")]
fn main() {}

/// Build and run the app over loaded game data (desktop and web).
pub fn run(content: Content) -> AppExit {
    let record = std::env::var("RIPTIDE_RECORD").ok();
    if content.tracks.is_empty() || content.boats.is_empty() {
        eprintln!("riptide: no playable levels or boats found");
        return AppExit::error();
    }
    let shot = std::env::var("RIPTIDE_SHOT").ok();
    let mut sel = Selection::default();
    if let Ok(t) = std::env::var("RIPTIDE_TRACK") {
        // A track id (`ship_graveyard`), an H2 level code (`wa`) or an HT file stem (`grav`).
        let t = t.to_ascii_lowercase();
        sel.level = content
            .tracks
            .iter()
            .position(|c| {
                c.id == t
                    || match &c.source {
                        content::CourseSource::H2(code) => *code == t,
                        content::CourseSource::Ht { file, .. } => file.to_ascii_lowercase().starts_with(&format!("{t}.")),
                        content::CourseSource::Sandbox { .. } => false,
                    }
            })
            .unwrap_or(0);
    }
    if let Ok(b) = std::env::var("RIPTIDE_BOAT") {
        let b = b.to_ascii_lowercase();
        sel.boat = content.boats.iter().position(|x| x.name.to_ascii_lowercase().contains(&b)).unwrap_or(0);
    }

    let mut app = App::new();
    let window = if shot.is_some() {
        WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..default() }
    } else {
        WindowPlugin {
            primary_window: Some(Window {
                title: "Riptide".into(),
                resolution: WindowResolution::new(1600, 900),
                // In the browser: draw into the page's canvas and fill its container.
                canvas: cfg!(target_arch = "wasm32").then(|| "#riptide".into()),
                fit_canvas_to_parent: true,
                prevent_default_event_handling: true,
                ..default()
            }),
            ..default()
        }
    };
    let plugins = DefaultPlugins.set(window).set(ImagePlugin { default_sampler: content::repeat_sampler() });
    if shot.is_some() {
        // Off-screen captures need a schedule runner, but no display or audio device.
        app.add_plugins(plugins.disable::<bevy::winit::WinitPlugin>().disable::<bevy::audio::AudioPlugin>());
        // Scripted online races run in real time: one RIPTIDE_SIM_DT step per frame, paced to the
        // clock, so remote boats arrive when they would for a player.
        let paced = std::env::var_os("RIPTIDE_ONLINE").and(std::env::var("RIPTIDE_SIM_DT").ok()).and_then(|s| s.parse::<f64>().ok());
        app.add_plugins(match paced {
            Some(dt) => bevy::app::ScheduleRunnerPlugin::run_loop(std::time::Duration::from_secs_f64(dt)),
            None => bevy::app::ScheduleRunnerPlugin::default(),
        });
        app.init_asset::<bevy::audio::AudioSource>();
    } else {
        app.add_plugins(plugins);
    }
    app
        .insert_resource(ClearColor(Color::srgb(0.55, 0.68, 0.82)))
        .insert_resource(content)
        .init_resource::<ModelCache>()
        .init_state::<Screen>()
        .add_systems(Update, (content::finish_textures, content::animate_ht_textures))
        .add_plugins((cheats::CheatsPlugin, net::NetPlugin, menu::MenuPlugin, race::RacePlugin, sound::SoundPlugin, effects::EffectsPlugin, hud::HudPlugin, boatrig::BoatRigPlugin, water::WaterPlugin, h2water::H2WaterPlugin, boatshader::BoatShaderPlugin, terrain::TerrainPlugin))
        .add_systems(OnEnter(Screen::Restart), |mut next: ResMut<NextState<Screen>>| next.set(Screen::Race));
    #[cfg(target_arch = "wasm32")]
    app.add_plugins(web::WebPlugin);
    if let Some(path) = record {
        app.add_systems(Last, move |mut n: Local<u32>| {
            *n += 1;
            if *n % 20 == 0 {
                let _ = std::fs::write(&path, format!("{}\n", riptide_assets::lux::recorded().join("\n")));
            }
        });
    }

    if let Some(prefix) = shot {
        let frames: Vec<u32> = std::env::var("RIPTIDE_SHOT_FRAMES")
            .unwrap_or_else(|_| "90".into())
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        let menu = std::env::var("RIPTIDE_SHOT_MENU").is_ok();
        let image = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            // RIPTIDE_SHOT_SIZE=WxH (default 1280x720).
            let (w, h) = std::env::var("RIPTIDE_SHOT_SIZE")
                .ok()
                .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
                .unwrap_or((1280, 720));
            images.add(Image::new_target_texture(w, h, TextureFormat::Rgba8UnormSrgb, None))
        };
        sel.render_target = Some(RenderTarget::Image(ImageRenderTarget { handle: image.clone(), scale_factor: 1.0 }));
        // RIPTIDE_SIM_DT=0.05: advance game time by a fixed step per frame, so a slow software
        // renderer still simulates the race at full rate.
        if let Some(dt) = std::env::var("RIPTIDE_SIM_DT").ok().and_then(|s| s.parse::<f32>().ok()) {
            app.insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(std::time::Duration::from_secs_f32(dt)));
        }
        app.insert_resource(ShotPlan { prefix, frames, image, frame: 0 })
            .add_systems(Update, shot_driver);
        if !menu {
            app.insert_resource(Autopilot);
            // Scripted online races (RIPTIDE_ONLINE) begin in the menu, where the lobby is.
            if std::env::var_os("RIPTIDE_ONLINE").is_none() {
                app.insert_state(Screen::Race);
            }
        }
    }
    app.insert_resource(sel);
    app.run()
}

fn shot_driver(mut commands: Commands, mut plan: ResMut<ShotPlan>, mut exit: MessageWriter<AppExit>, mut cams: Query<&mut Camera>) {
    plan.frame += 1;
    let f = plan.frame;
    // Headless tests only need pictures on capture frames: cameras draw on those (and two
    // frames before, so the image is fresh) and the rest of the run is pure simulation.
    // RIPTIDE_RENDER_ALL=1 draws every frame.
    let draw = std::env::var_os("RIPTIDE_RENDER_ALL").is_some() || plan.frames.iter().any(|&c| c >= f && c <= f + 2);
    for mut cam in &mut cams {
        if cam.is_active != draw {
            cam.is_active = draw;
        }
    }
    if plan.frames.contains(&f) {
        let path = format!("{}_{f}.png", plan.prefix);
        commands.spawn(Screenshot::image(plan.image.clone())).observe(save_to_disk(path));
    }
    if f > plan.frames.iter().copied().max().unwrap_or(0) + 20 {
        exit.write(AppExit::Success);
    }
}
