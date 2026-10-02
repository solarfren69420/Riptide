//! Cheats from the `cheats` sheet: hotkeys, the Tab menu, and the tuning they override.
//!
//! [`Tuning`] is H2Overdrive's global table (`h2_globals`) with every active cheat's
//! `cheat_overrides` rows written over it; gameplay reads globals only through it.

use crate::controls::Input;
use crate::sheets::{
    cheats_ids, controls_ids, CheatsEffect, H2GlobalsValues, CHEATS, CHEAT_OVERRIDES, H2_GLOBALS_DEFAULT,
};
use bevy::prelude::*;
use std::path::PathBuf;

pub struct CheatsPlugin;

impl Plugin for CheatsPlugin {
    fn build(&self, app: &mut App) {
        let cheats = Cheats::load();
        let tuning = cheats.tuning();
        app.insert_resource(cheats)
            .insert_resource(Tuning(tuning))
            .add_systems(Startup, spawn_menu)
            .add_systems(Update, (hotkeys, menu_input, apply, draw_menu).chain());
    }
}

#[derive(Resource)]
pub struct Cheats {
    pub on: Vec<bool>,
    /// Per cheat: which of its `params` levels is selected (cycling cheats).
    level: Vec<usize>,
    pub menu_open: bool,
    cursor: usize,
}

/// H2Overdrive globals with active cheat overrides applied.
#[derive(Resource)]
pub struct Tuning(pub H2GlobalsValues);

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("riptide/cheats.txt"))
}

impl Cheats {
    /// Enabled cheat ids, one per line, from the last session.
    fn load() -> Self {
        let saved = config_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
        let on = CHEATS.iter().map(|c| saved.lines().any(|l| l.trim() == c.id)).collect();
        Self { on, level: vec![0; CHEATS.len()], menu_open: false, cursor: 0 }
    }

    fn save(&self) {
        let Some(p) = config_path() else { return };
        let body: String = CHEATS.iter().zip(&self.on).filter(|(_, on)| **on).map(|(c, _)| format!("{}\n", c.id)).collect();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&p, body) {
            warn!("saving cheats to {}: {e}", p.display());
        }
    }

    /// Is any cheat with this effect switched on?
    pub fn active(&self, effect: CheatsEffect) -> bool {
        CHEATS.iter().zip(&self.on).any(|(c, on)| *on && c.effect == effect)
    }

    /// The selected level of the first switched-on cheat with this effect (1 when none).
    pub fn level(&self, effect: CheatsEffect) -> f32 {
        CHEATS
            .iter()
            .enumerate()
            .find(|(i, c)| self.on[*i] && c.effect == effect && !c.params.is_empty())
            .map_or(1.0, |(i, c)| c.params[self.level[i] % c.params.len()])
    }

    /// Chase camera distance multiplier (1 unless the zoom cheat is on).
    pub fn zoom(&self) -> f32 {
        self.level(CheatsEffect::CameraZoom)
    }

    /// The player boat's top speed multiplier (1 unless the boat speed cheat is on).
    pub fn speed(&self) -> f32 {
        self.level(CheatsEffect::SpeedMult)
    }

    fn toggle(&mut self, i: usize) {
        let n = CHEATS[i].params.len();
        if n > 0 && self.on[i] {
            // Cycling cheats step through their levels; they switch off after the last one.
            self.level[i] += 1;
            if self.level[i] >= n {
                self.level[i] = 0;
                self.on[i] = false;
            }
        } else {
            self.on[i] = !self.on[i];
            // Camera zoom's first level is 1 (no zoom): start on the next one.
            self.level[i] = if i == cheats_ids::CAMERA_ZOOM { 1.min(n.saturating_sub(1)) } else { 0 };
        }
    }

    fn tuning(&self) -> H2GlobalsValues {
        let mut t = H2_GLOBALS_DEFAULT;
        for o in CHEAT_OVERRIDES {
            if o.cheat.is_some_and(|c| self.on[c]) {
                let id = o.global.map(|g| crate::sheets::H2_GLOBALS[g].id).unwrap_or("");
                match t.field_mut(id) {
                    Some(f) => *f = o.value,
                    None => warn!("cheat override {}: {id} is not an f32 global", o.id),
                }
            }
        }
        t
    }
}

fn hotkeys(input: Input, mut cheats: ResMut<Cheats>) {
    let mut changed = false;
    for (i, c) in CHEATS.iter().enumerate() {
        if c.hotkey.iter().any(|k| input.keys.just_pressed(*k)) {
            cheats.toggle(i);
            changed = true;
        }
    }
    if input.just_pressed(controls_ids::CHEATS_ALL) {
        // Like the trainer's F8: all on, or all off when they already are.
        let all = cheats.on.iter().all(|o| *o);
        cheats.on.iter_mut().for_each(|o| *o = !all);
        changed = true;
    }
    if input.just_pressed(controls_ids::CHEATS_MENU) {
        cheats.menu_open = !cheats.menu_open;
    }
    if changed {
        cheats.save();
    }
}

fn menu_input(input: Input, mut cheats: ResMut<Cheats>) {
    if !cheats.menu_open {
        return;
    }
    let n = CHEATS.len();
    if input.just_pressed(controls_ids::CHEATS_UP) {
        cheats.cursor = (cheats.cursor + n - 1) % n;
    }
    if input.just_pressed(controls_ids::CHEATS_DOWN) {
        cheats.cursor = (cheats.cursor + 1) % n;
    }
    if input.just_pressed(controls_ids::CHEATS_TOGGLE) {
        let i = cheats.cursor;
        cheats.toggle(i);
        cheats.save();
    }
}

fn apply(cheats: Res<Cheats>, mut tuning: ResMut<Tuning>) {
    if cheats.is_changed() {
        tuning.0 = cheats.tuning();
    }
}

#[derive(Component)]
struct CheatMenu;

#[derive(Component)]
struct CheatBadge;

fn spawn_menu(mut commands: Commands) {
    commands.spawn((
        Node { position_type: PositionType::Absolute, right: px(24), top: px(18), padding: UiRect::all(px(16)), ..default() },
        BackgroundColor(Color::srgba(0.0, 0.03, 0.08, 0.86)),
        GlobalZIndex(10),
        Visibility::Hidden,
        CheatMenu,
        children![(Text::new(""), TextFont { font_size: 20.0, ..default() }, TextShadow::default())],
    ));
    commands.spawn((
        Node { position_type: PositionType::Absolute, right: px(24), top: px(18), ..default() },
        GlobalZIndex(9),
        CheatBadge,
        children![(
            Text::new(""),
            TextFont { font_size: 15.0, ..default() },
            TextColor(Color::srgb(1.0, 0.75, 0.2)),
        )],
    ));
}

fn draw_menu(
    cheats: Res<Cheats>,
    mut menu: Query<(&mut Visibility, &Children), (With<CheatMenu>, Without<CheatBadge>)>,
    mut badge: Query<(&mut Visibility, &Children), With<CheatBadge>>,
    mut texts: Query<&mut Text>,
) {
    if !cheats.is_changed() {
        return;
    }
    if let Ok((mut vis, children)) = menu.single_mut() {
        *vis = if cheats.menu_open { Visibility::Visible } else { Visibility::Hidden };
        let mut s = String::from("CHEATS\n\n");
        for (i, c) in CHEATS.iter().enumerate() {
            let state = if !cheats.on[i] {
                "off".to_string()
            } else if !c.params.is_empty() {
                format!("x{}", c.params[cheats.level[i] % c.params.len()])
            } else {
                "ON".to_string()
            };
            let key = c.hotkey.first().map(|k| format!("{k:?}")).unwrap_or_default();
            let cursor = if i == cheats.cursor { ">" } else { " " };
            s += &format!("{cursor} [{key:>3}] {:<24} {state}\n", c.label);
        }
        let d = &CHEATS[cheats.cursor % CHEATS.len()];
        s += &format!("\n{}\n\n{}", d.description, crate::controls::help("cheats"));
        s += "\nF8: all cheats on/off   Tab: close";
        if let Some(mut t) = children.first().and_then(|c| texts.get_mut(*c).ok()) {
            t.0 = s;
        }
    }
    if let Ok((mut vis, children)) = badge.single_mut() {
        *vis = if cheats.menu_open { Visibility::Hidden } else { Visibility::Visible };
        let on: Vec<&str> = CHEATS.iter().zip(&cheats.on).filter(|(_, o)| **o).map(|(c, _)| c.label).collect();
        let s = if on.is_empty() { "Tab: cheats".to_string() } else { format!("CHEATS: {}", on.join(", ")) };
        if let Some(mut t) = children.first().and_then(|c| texts.get_mut(*c).ok()) {
            t.0 = s;
        }
    }
}
