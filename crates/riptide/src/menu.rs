//! Title screen: pick a track and a boat, with the boat spinning on display.

use crate::cheats::Cheats;
use crate::content::{attach, BoatInfo, Models};
use crate::controls::{help, Input};
use crate::sheets::{controls_ids as ctl, BoatsUnlock, CheatsEffect};
use crate::{Screen, Selection};
use bevy::prelude::*;
use std::collections::HashSet;

/// How many View presses on a parent boat reveal its secret boat (the cabinet trick).
const VIEW_PRESSES: u32 = 3;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Unlocks>()
            .add_systems(OnEnter(Screen::Menu), spawn_menu)
            .add_systems(Update, (menu_input, refresh_preview, spin, menu_text).chain().run_if(in_state(Screen::Menu)));
    }
}

/// Secret boats revealed this session (`boats` row indices), and the View press streak.
#[derive(Resource, Default)]
pub struct Unlocks {
    revealed: HashSet<usize>,
    streak: (usize, u32),
}

impl Unlocks {
    pub fn visible(&self, cheats: &Cheats, b: &BoatInfo) -> bool {
        b.row.unlock != BoatsUnlock::Secret || cheats.active(CheatsEffect::UnlockSecrets) || self.revealed.contains(&b.index())
    }
}

#[derive(Component)]
struct Turntable;

#[derive(Component)]
struct MenuText;

#[derive(Component)]
struct ShownBoat(usize);

fn spawn_menu(mut commands: Commands, sel: Res<Selection>) {
    let scope = DespawnOnExit(Screen::Menu);
    let mut cam = commands.spawn((
        Camera3d::default(),
        IsDefaultUiCamera,
        Transform::from_xyz(0.0, 55.0, 150.0).looking_at(Vec3::new(0.0, 5.0, 0.0), Vec3::Y),
        scope.clone(),
    ));
    if let Some(t) = sel.render_target.clone() {
        cam.insert(t);
    }
    commands.spawn((
        DirectionalLight { illuminance: 12_000.0, ..default() },
        Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.6, -0.8, 0.0)),
        scope.clone(),
    ));
    commands.insert_resource(GlobalAmbientLight { color: Color::WHITE, brightness: 1200.0, ..default() });
    commands.spawn((Transform::IDENTITY, Visibility::default(), Turntable, ShownBoat(usize::MAX), scope.clone()));
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::all(px(28)),
                ..default()
            },
            scope,
        ))
        .with_children(|c| {
            c.spawn((
                Text::new("RIPTIDE"),
                TextFont { font_size: 64.0, ..default() },
                TextColor(Color::srgb(1.0, 0.82, 0.25)),
                TextShadow::default(),
            ));
            c.spawn((Text::new(""), TextFont { font_size: 22.0, ..default() }, TextShadow::default(), MenuText));
        });
}

fn menu_input(
    input: Input,
    mut sfx: crate::sound::Sfx,
    cheats: Res<Cheats>,
    mut unlocks: ResMut<Unlocks>,
    mut sel: ResMut<Selection>,
    models: Models,
    mut next: ResMut<NextState<Screen>>,
    mut exit: MessageWriter<AppExit>,
    mut last_boat: Local<usize>,
    mut last_level: Local<usize>,
) {
    if cheats.menu_open {
        return;
    }
    let boats = &models.content.boats;
    let nb = boats.len();
    let nl = models.content.tracks.len();
    let visible = |i: usize| unlocks.visible(&cheats, &boats[i]);
    // Step to the next visible boat in direction `d` (1 forward, nb - 1 back).
    let step = |from: usize, d: usize| {
        let mut i = from;
        for _ in 0..nb {
            i = (i + d) % nb;
            if visible(i) {
                return i;
            }
        }
        from
    };
    if !visible(sel.boat) {
        sel.boat = step(sel.boat, 1);
    }
    if input.just_pressed(ctl::MENU_PREV_BOAT) {
        sel.boat = step(sel.boat, nb - 1);
    }
    if input.just_pressed(ctl::MENU_NEXT_BOAT) {
        sel.boat = step(sel.boat, 1);
    }
    if input.just_pressed(ctl::MENU_PREV_TRACK) {
        sel.level = (sel.level + nl - 1) % nl;
    }
    if input.just_pressed(ctl::MENU_NEXT_TRACK) {
        sel.level = (sel.level + 1) % nl;
    }
    if input.just_pressed(ctl::MENU_VIEW) {
        let here = boats[sel.boat].index();
        unlocks.streak = if unlocks.streak.0 == here { (here, unlocks.streak.1 + 1) } else { (here, 1) };
        if unlocks.streak.1 >= VIEW_PRESSES {
            // Reveal (and jump to) the secret boat this one is the parent of.
            if let Some(i) = boats.iter().position(|b| b.row.unlock_parent == Some(here)) {
                unlocks.revealed.insert(boats[i].index());
                sel.boat = i;
            }
            unlocks.streak = (here, 0);
        }
    }
    // Selection sounds: the announcer names a newly picked boat; other moves tick.
    if sel.boat != *last_boat {
        *last_boat = sel.boat;
        match boats[sel.boat].row.voice.and_then(|g| crate::sheets::H2_GLOBALSOUNDS[g].sounddef) {
            Some(def) => sfx.def(def, 1.0),
            None => sfx.event(crate::sheets::sound_events_ids::MENU_TICK),
        }
    }
    if sel.level != *last_level {
        *last_level = sel.level;
        sfx.event(crate::sheets::sound_events_ids::MENU_TICK);
    }
    if input.just_pressed(ctl::MENU_START) {
        next.set(Screen::Race);
    }
    if input.just_pressed(ctl::MENU_QUIT) && sel.render_target.is_none() {
        exit.write(AppExit::Success);
    }
}

fn refresh_preview(
    mut commands: Commands,
    sel: Res<Selection>,
    mut models: Models,
    mut table: Query<(Entity, &mut ShownBoat, Option<&Children>), With<Turntable>>,
) {
    let Ok((e, mut shown, children)) = table.single_mut() else { return };
    if shown.0 == sel.boat {
        return;
    }
    shown.0 = sel.boat;
    if let Some(ch) = children {
        for c in ch.iter() {
            commands.entity(c).despawn();
        }
    }
    let info = models.content.boats[sel.boat].clone();
    if let Some(p) = models.boat(&info) {
        let node = commands.spawn((Transform::from_scale(Vec3::splat(info.scale * 1.4)), Visibility::default())).id();
        commands.entity(e).add_child(node);
        attach(&mut commands, node, &p);
    }
}

fn spin(time: Res<Time>, mut q: Query<&mut Transform, With<Turntable>>) {
    for mut t in &mut q {
        t.rotation = Quat::from_rotation_y(time.elapsed_secs() * 0.6);
    }
}

fn menu_text(
    sel: Res<Selection>,
    models: Models,
    cheats: Res<Cheats>,
    unlocks: Res<Unlocks>,
    mut q: Query<&mut Text, With<MenuText>>,
) {
    let Ok(mut text) = q.single_mut() else { return };
    let boats = &models.content.boats;
    let boat = &boats[sel.boat];
    let level = &models.content.tracks[sel.level];
    let shown: Vec<usize> = (0..boats.len()).filter(|&i| unlocks.visible(&cheats, &boats[i])).collect();
    let pos = shown.iter().position(|&i| i == sel.boat).map_or(0, |p| p + 1);
    let s = format!(
        "TRACK  < {} >  ({})   [{}/{}]\nBOAT   {}  ({}, {})   [{pos}/{}]\n\n{}",
        level.name,
        if level.game == crate::sheets::TracksGame::H2 { "H2Overdrive" } else { "Hydro Thunder" },
        sel.level + 1,
        models.content.tracks.len(),
        boat.name,
        boat.game,
        boat.row.tier.as_str(),
        shown.len(),
        help("menu"),
    );
    if text.0 != s {
        text.0 = s;
    }
}
