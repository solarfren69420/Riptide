//! Sound from H2Overdrive's own data: `h2_samples` (FSB4 banks), `h2_sounddefs` (variants with
//! volume and pitch ranges), `h2_globalsounds` + `sound_events` (what each game event plays) and
//! `h2_enginedefs` (layered engine loops by RPM).

use crate::content::Content;
use crate::sheets::{
    H2EnginedefsRow, H2SounddefsRow, H2_ENGINEDEFS, H2_GLOBALSOUNDS, H2_SAMPLES, H2_SOUNDDEFS, SOUND_EVENTS,
};
use bevy::audio::{AudioSink, AudioSinkPlayback, PlaybackMode, Volume};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use riptide_assets::fsb::{self, FsbSample};
use std::collections::HashMap;
use std::sync::Arc;

pub struct SoundPlugin;

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoundBanks>();
    }
}

#[derive(Resource, Default)]
pub struct SoundBanks {
    banks: HashMap<String, Arc<Vec<FsbSample>>>,
    sources: HashMap<usize, Option<Handle<AudioSource>>>,
    seed: u32,
}

/// One playable variant of a sound def.
struct Variant {
    sample: usize,
    volume: f32,
    min_pitch: f32,
    max_pitch: f32,
}

fn variant(d: &H2SounddefsRow, i: usize) -> Option<Variant> {
    let (sample, volume, min_pitch, max_pitch) = match i {
        0 => (d.sound_name0, d.sound_vol2d0, d.sound_minpitchmul0, d.sound_maxpitchmul0),
        1 => (d.sound_name1, d.sound_vol2d1, d.sound_minpitchmul1, d.sound_maxpitchmul1),
        2 => (d.sound_name2, d.sound_vol2d2, d.sound_minpitchmul2, d.sound_maxpitchmul2),
        3 => (d.sound_name3, d.sound_vol2d3, d.sound_minpitchmul3, d.sound_maxpitchmul3),
        4 => (d.sound_name4, d.sound_vol2d4, d.sound_minpitchmul4, d.sound_maxpitchmul4),
        5 => (d.sound_name5, d.sound_vol2d5, d.sound_minpitchmul5, d.sound_maxpitchmul5),
        6 => (d.sound_name6, d.sound_vol2d6, d.sound_minpitchmul6, d.sound_maxpitchmul6),
        7 => (d.sound_name7, d.sound_vol2d7, d.sound_minpitchmul7, d.sound_maxpitchmul7),
        _ => return None,
    };
    Some(Variant { sample: sample?, volume, min_pitch, max_pitch })
}

#[derive(SystemParam)]
pub struct Sfx<'w, 's> {
    commands: Commands<'w, 's>,
    banks: ResMut<'w, SoundBanks>,
    sources: ResMut<'w, Assets<AudioSource>>,
    content: Res<'w, Content>,
}

/// Headless test runs (`RIPTIDE_SHOT`) and `RIPTIDE_MUTE=1` decode and log sounds but never play
/// them: tests must not be heard.
fn muted() -> bool {
    static MUTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *MUTED.get_or_init(|| std::env::var_os("RIPTIDE_SHOT").is_some() || std::env::var_os("RIPTIDE_MUTE").is_some())
}

impl Sfx<'_, '_> {
    fn rand(&mut self) -> f32 {
        let s = &mut self.banks.seed;
        *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*s >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Decoded-on-demand audio for an `h2_samples` row.
    fn source(&mut self, sample: usize) -> Option<Handle<AudioSource>> {
        if let Some(hit) = self.banks.sources.get(&sample) {
            return hit.clone();
        }
        let row = &H2_SAMPLES[sample];
        let lux = self.content.lux.clone();
        let bank = lux.get(&format!("fbnk.{}", row.bank));
        let list = match self.banks.banks.get(row.bank) {
            Some(l) => Some(l.clone()),
            None => bank.and_then(|b| fsb::parse(b).ok()).map(|l| {
                let l = Arc::new(l);
                self.banks.banks.insert(row.bank.to_string(), l.clone());
                l
            }),
        };
        let out = (|| {
            let s = list?.iter().find(|s| s.name == row.file)?.clone();
            let (bytes, _) = fsb::extract(bank?, &s).map_err(|e| warn!("{}: {e:#}", row.file)).ok()?;
            Some(self.sources.add(AudioSource { bytes: bytes.into() }))
        })();
        if std::env::var_os("RIPTIDE_DEBUG").is_some() {
            info!("sound: sample {} ({}) -> {}", row.id, row.bank, if out.is_some() { "decoded" } else { "FAILED" });
        }
        self.banks.sources.insert(sample, out.clone());
        out
    }

    /// Play a sound def once: a random variant at its volume, pitch random in its range.
    pub fn def(&mut self, def: usize, gain: f32) {
        let d = &H2_SOUNDDEFS[def];
        let variants: Vec<Variant> = (0..8).filter_map(|i| variant(d, i)).collect();
        if variants.is_empty() {
            return;
        }
        let v = &variants[(self.rand() * variants.len() as f32) as usize % variants.len()];
        let (sample, volume, lo, hi) = (v.sample, v.volume, v.min_pitch, v.max_pitch);
        let Some(src) = self.source(sample) else { return };
        let pitch = if hi > lo { lo + (hi - lo) * self.rand() } else { lo.max(0.05) };
        let settings = PlaybackSettings::DESPAWN.with_volume(Volume::Linear(d.volume_2d * volume * gain)).with_speed(pitch.max(0.05));
        if std::env::var_os("RIPTIDE_DEBUG").is_some() {
            info!("sound: play {} vol {:.2} pitch {:.2}", d.id, d.volume_2d * volume * gain, pitch);
        }
        if !muted() {
            self.commands.spawn((AudioPlayer(src), settings));
        }
    }

    /// Play the sound a `sound_events` row names (see `crate::sheets::sound_events_ids`).
    pub fn event(&mut self, ev: usize) {
        let row = &SOUND_EVENTS[ev];
        if !row.status.is_ok() {
            return;
        }
        if let Some(def) = row.global.and_then(|g| H2_GLOBALSOUNDS[g].sounddef) {
            self.def(def, 1.0);
        }
        if let Some(def) = row.sounddef {
            self.def(def, 1.0);
        }
    }

    /// Play a `sound_events` row's sound once (first variant at its own volume), returning its
    /// entity so it can be cut short with [`Self::stop`].
    pub fn event_once(&mut self, ev: usize, scope: impl Bundle) -> Option<Entity> {
        let row = &SOUND_EVENTS[ev];
        let def = row.sounddef.or(row.global.and_then(|g| H2_GLOBALSOUNDS[g].sounddef))?;
        let d = &H2_SOUNDDEFS[def];
        let v = variant(d, 0)?;
        let src = self.source(v.sample)?;
        if std::env::var_os("RIPTIDE_DEBUG").is_some() {
            info!("sound: play {}", d.id);
        }
        if muted() {
            return None;
        }
        let settings = PlaybackSettings::DESPAWN.with_volume(Volume::Linear(d.volume_2d * v.volume));
        Some(self.commands.spawn((AudioPlayer(src), settings, scope)).id())
    }

    /// Stop a sound started by [`Self::event_loop`] or [`Self::event_once`].
    pub fn stop(&mut self, e: Entity) {
        self.commands.entity(e).try_despawn();
    }

    /// Loop a `sound_events` row's sound (first variant at its own volume) until the returned
    /// entity is despawned.
    pub fn event_loop(&mut self, ev: usize, scope: impl Bundle) -> Option<Entity> {
        let row = &SOUND_EVENTS[ev];
        let def = row.sounddef.or(row.global.and_then(|g| H2_GLOBALSOUNDS[g].sounddef))?;
        let d = &H2_SOUNDDEFS[def];
        let v = variant(d, 0)?;
        let src = self.source(v.sample)?;
        if std::env::var_os("RIPTIDE_DEBUG").is_some() {
            info!("sound: loop {}", d.id);
        }
        if muted() {
            return None;
        }
        let settings = PlaybackSettings { mode: PlaybackMode::Loop, volume: Volume::Linear(d.volume_2d * v.volume), ..default() };
        Some(self.commands.spawn((AudioPlayer(src), settings, scope)).id())
    }

    /// Loop an `h2_samples` row (race music) at `volume`.
    pub fn music(&mut self, sample: usize, volume: f32, scope: impl Bundle) {
        if let Some(src) = self.source(sample) {
            let settings = PlaybackSettings { mode: PlaybackMode::Loop, volume: Volume::Linear(volume), ..default() };
            if !muted() {
                self.commands.spawn((AudioPlayer(src), settings, scope));
            }
        }
    }

    /// Start a looping sound def (first variant), silent until [`EngineLayer`] drives it.
    fn looped(&mut self, def: usize) -> Option<Entity> {
        let v = variant(&H2_SOUNDDEFS[def], 0)?;
        let src = self.source(v.sample)?;
        if muted() {
            return None;
        }
        let settings = PlaybackSettings { mode: PlaybackMode::Loop, volume: Volume::Linear(0.0), ..default() };
        Some(self.commands.spawn((AudioPlayer(src), settings)).id())
    }
}

/// One engine loop and the RPM window that shapes it (CEngineDef `<Layer> *` columns).
#[derive(Component, Clone, Copy)]
pub struct EngineLayer {
    min_rpm: f32,
    max_rpm: f32,
    min_pitch: f32,
    max_pitch: f32,
    ramp_end: f32,
    fall_start: f32,
    fall_factor: f32,
    vol1: f32,
    vol2: f32,
    master: f32,
}

impl EngineLayer {
    /// Volume and pitch at unit RPM `u` (0 idle .. 1 top speed).
    fn at(&self, u: f32) -> (f32, f32) {
        let span = (self.max_rpm - self.min_rpm).max(1e-3);
        let t = ((u - self.min_rpm) / span).clamp(0.0, 1.0);
        let fade_in = if u < self.min_rpm {
            0.0
        } else if self.ramp_end > self.min_rpm {
            ((u - self.min_rpm) / (self.ramp_end - self.min_rpm)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let fade_out = if u > self.fall_start { (1.0 - (u - self.fall_start) * self.fall_factor).clamp(0.0, 1.0) } else { 1.0 };
        let volume = (self.vol1 + (self.vol2 - self.vol1) * t) * fade_in * fade_out * self.master;
        (volume, self.min_pitch + (self.max_pitch - self.min_pitch) * t)
    }
}

/// The player's engine: which boat it follows.
#[derive(Component)]
pub struct EngineOf(pub Entity);

macro_rules! layer {
    ($e:expr, $def:ident, $min_rpm:ident, $max_rpm:ident, $min_p:ident, $max_p:ident, $ramp:ident, $fs:ident, $ff:ident, $v1:ident, $v2:ident) => {
        ($e.$def, EngineLayer {
            min_rpm: $e.$min_rpm,
            max_rpm: $e.$max_rpm,
            min_pitch: $e.$min_p,
            max_pitch: $e.$max_p,
            ramp_end: $e.$ramp,
            fall_start: $e.$fs,
            fall_factor: $e.$ff,
            vol1: $e.$v1,
            vol2: $e.$v2,
            master: $e.master_volume,
        })
    };
}

fn layers(e: &H2EnginedefsRow) -> Vec<(Option<usize>, EngineLayer)> {
    vec![
        layer!(e, idle_sound_def, idle_min_unit_rpm, idle_max_unit_rpm, idle_min_pitch, idle_max_pitch, idle_ramp_up_end, idle_fall_off_start, idle_fall_off_factor, idle_volume_1, idle_volume_2),
        layer!(e, low_sound_def, low_min_unit_rpm, low_max_unit_rpm, low_min_pitch, low_max_pitch, low_ramp_up_end, low_fall_off_start, low_fall_off_factor, low_volume_1, low_volume_2),
        layer!(e, med_sound_def, med_min_unit_rpm, med_max_unit_rpm, med_min_pitch, med_max_pitch, med_ramp_up_end, med_fall_off_start, med_fall_off_factor, med_volume_1, med_volume_2),
        layer!(e, high_sound_def, high_min_unit_rpm, high_max_unit_rpm, high_min_pitch, high_max_pitch, high_ramp_up_end, high_fall_off_start, high_fall_off_factor, high_volume_1, high_volume_2),
        layer!(e, sub_sound_def, sub_min_unit_rpm, sub_max_unit_rpm, sub_min_pitch, sub_max_pitch, sub_ramp_up_end, sub_fall_off_start, sub_fall_off_factor, sub_volume_1, sub_volume_2),
        layer!(e, amblow_sound_def, amblow_min_unit_rpm, amblow_max_unit_rpm, amblow_min_pitch, amblow_max_pitch, amblow_ramp_up_end, amblow_fall_off_start, amblow_fall_off_factor, amblow_volume_1, amblow_volume_2),
        layer!(e, ambhigh_sound_def, ambhigh_min_unit_rpm, ambhigh_max_unit_rpm, ambhigh_min_pitch, ambhigh_max_pitch, ambhigh_ramp_up_end, ambhigh_fall_off_start, ambhigh_fall_off_factor, ambhigh_volume_1, ambhigh_volume_2),
    ]
}

/// Start the layered engine loops for `boat` using engine def row `engine`.
pub fn start_engine(sfx: &mut Sfx, boat: Entity, engine: usize, scope: impl Bundle + Clone) {
    for (def, layer) in layers(&H2_ENGINEDEFS[engine]) {
        let Some(def) = def else { continue };
        if std::env::var_os("RIPTIDE_DEBUG").is_some() {
            info!("sound: engine layer {}", H2_SOUNDDEFS[def].id);
        }
        if let Some(e) = sfx.looped(def) {
            sfx.commands.entity(e).insert((layer, EngineOf(boat), scope.clone()));
        }
    }
}

/// Drive engine loops from `rpm(boat)` each frame.
pub fn drive_engines(rpm: impl Fn(Entity) -> Option<f32>, layers: &mut Query<(&EngineLayer, &EngineOf, &mut AudioSink)>) {
    for (layer, of, mut sink) in layers.iter_mut() {
        let Some(u) = rpm(of.0) else { continue };
        let (vol, pitch) = layer.at(u.clamp(0.0, 1.2));
        sink.set_volume(Volume::Linear(vol));
        sink.set_speed(pitch.max(0.05));
    }
}
