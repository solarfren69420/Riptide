//! Title screen: pick a track and a boat, with the boat spinning on display.

use crate::cheats::Cheats;
use crate::content::{attach, BoatInfo, CourseSource, Models, Piece, TrackChoice};
use crate::controls::Input;
use crate::sheets::{controls_ids as ctl, BoatsUnlock, CheatsEffect};
use crate::{Screen, Selection};
use bevy::prelude::*;
use bevy::camera::{visibility::RenderLayers, ImageRenderTarget, RenderTarget};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use std::collections::HashSet;

/// How many View presses on a parent boat reveal its secret boat (the cabinet trick).
const VIEW_PRESSES: u32 = 3;

pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Unlocks>()
            .add_systems(OnEnter(Screen::Menu), spawn_menu)
            .add_systems(Update, (menu_input, refresh_track, refresh_preview, spin, menu_text).chain().run_if(in_state(Screen::Menu)));
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
enum MenuText { Track, Boat, Detail }

#[derive(Component)]
struct TrackImage { background: bool }

#[derive(Resource, Default)]
struct PreviewScene { choice: Option<usize>, root: Option<Entity>, image: Option<Handle<Image>> }

#[derive(Component)]
struct ShownBoat(usize);

fn spawn_menu(mut commands: Commands, sel: Res<Selection>, mut models: Models) {
    commands.insert_resource(PreviewScene::default());
    let scope = DespawnOnExit(Screen::Menu);
    let choice = models.content.tracks[sel.level].clone();
    let track_image = models.track_preview(&choice);
    let mut cam = commands.spawn((
        Camera3d::default(),
        IsDefaultUiCamera,
        Transform::from_xyz(0.0, 60.0, 190.0).looking_at(Vec3::new(0.0, 4.0, 0.0), Vec3::Y),
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
    // Leave the left side for track/boat selection text while the boat remains on display.
    commands.insert_resource(ClearColor(Color::srgb(0.02, 0.055, 0.08)));
    commands.spawn((Transform::from_xyz(82.0, -3.0, 0.0), Visibility::default(), Turntable, ShownBoat(usize::MAX), scope.clone()));
    commands
        .spawn((
            Node {
                width: percent(100),
                height: percent(100),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(px(24)),
                ..default()
            },
            scope,
        ))
        .with_children(|c| {
            let mut background = ImageNode::default();
            if let Some((image, rect)) = &track_image { background.image = image.clone(); background.rect = *rect; }
            background.color = Color::srgba(0.22, 0.32, 0.40, 0.35);
            c.spawn((Node { position_type: PositionType::Absolute, left: px(0), top: px(0), width: percent(100), height: percent(100), ..default() },
                background, TrackImage { background: true }, ZIndex(-1)));
            c.spawn((
                Text::new("RIPTIDE"),
                TextFont { font_size: 44.0, ..default() },
                TextColor(Color::srgb(1.0, 0.82, 0.25)),
                TextShadow::default(),
            ));
            c.spawn((Text::new("CHOOSE YOUR COURSE  /  CHOOSE YOUR BOAT"), TextFont { font_size: 13.0, ..default() }, TextColor(Color::srgb(0.55,0.76,0.85))));
            c.spawn((Node { position_type: PositionType::Absolute, left: percent(6), top: percent(22), width: percent(40),
                padding: UiRect::all(px(22)), flex_direction: FlexDirection::Column, row_gap: px(14), border_radius: BorderRadius::all(px(14)), ..default() },
                BackgroundColor(Color::srgba(0.015,0.055,0.08,0.86)))).with_children(|panel| {
                panel.spawn((Text::new("SELECT COURSE"), TextFont { font_size: 13.0, ..default() }, TextColor(Color::srgb(0.25,0.8,0.95))));
                panel.spawn((Text::new(""), TextFont { font_size: 27.0, ..default() }, MenuText::Track));
                let mut image = ImageNode::default();
                if let Some((handle, rect)) = &track_image { image.image = handle.clone(); image.rect = *rect; }
                panel.spawn((Node { width: percent(100), aspect_ratio: Some(1.6), ..default() }, image, TrackImage { background: false }));
                panel.spawn((Text::new(""), TextFont { font_size: 15.0, ..default() }, TextColor(Color::srgb(0.62,0.77,0.83)), MenuText::Detail));
            });
            c.spawn((Node { position_type: PositionType::Absolute, left: percent(52), bottom: percent(24), width: percent(42), justify_content: JustifyContent::Center, ..default() },
                children![(Text::new(""), TextFont { font_size: 24.0, ..default() }, TextLayout::new_with_justify(Justify::Center), TextShadow::default(), MenuText::Boat)]));
            c.spawn((Node { position_type: PositionType::Absolute, bottom: px(28), width: percent(100), justify_content: JustifyContent::Center, ..default() },
                children![(Text::new("Left / Right  Track     Up / Down  Boat     Enter / Space  Start race\nV  Secret boat (press 3 times on its parent)     Tab  Cheats     Esc  Quit"), TextFont { font_size: 16.0, ..default() }, TextLayout::new_with_justify(Justify::Center), TextColor(Color::srgb(0.69,0.82,0.88)))]));
        });
}

fn refresh_track(mut commands: Commands, sel: Res<Selection>, mut models: Models, mut scene: ResMut<PreviewScene>, mut images: Query<(&mut ImageNode, &TrackImage)>) {
    let choice = models.content.tracks[sel.level].clone();
    if scene.choice != Some(sel.level) {
        if let Some(root) = scene.root.take() { commands.entity(root).try_despawn(); }
        scene.image = None;
        scene.choice = Some(sel.level);
        for (mut image, _) in &mut images { image.image = ImageNode::default().image; image.rect = None; }
    }
    let preview = models.track_preview(&choice).or_else(|| {
        if scene.image.is_none() {
            let (root, image) = render_preview(&mut commands, &mut models, &choice)?;
            scene.root = Some(root);
            scene.image = Some(image);
        }
        scene.image.clone().map(|image| (image, None))
    });
    let Some((handle, rect)) = preview else { return };
    for (mut image, kind) in &mut images {
        if image.image != handle || image.rect != rect {
            image.image = handle.clone();
            image.rect = rect;
            image.color = if kind.background { Color::srgba(0.22,0.32,0.40,0.35) } else { Color::WHITE };
        }
    }
}

/// Render a real course scene locally when no original select image exists.
fn render_preview(commands: &mut Commands, models: &mut Models, choice: &TrackChoice) -> Option<(Entity, Handle<Image>)> {
    let mut pieces: Vec<(Piece, Transform)> = Vec::new();
    let (start, yaw, water, texture) = match &choice.source {
        CourseSource::Ht { entry, water, .. } => {
            let ht = models.content.ht.clone()?;
            let course = riptide_assets::httrack::decode_track(&ht.main.object(entry).ok()?).ok()?;
            ht.clear_track();
            models.cache.forget_ht();
            let (position, yaw) = *course.starts.first()?;
            let surface = course.river.iter().map(|[a,b]| [a.start,a.end,b.end,b.start]).collect::<Vec<_>>();
            for p in models.ht_model(course.terrain).iter() { pieces.push((p.clone(), Transform::IDENTITY)); }
            for inst in course.instances {
                if let Some(mesh) = models.ht(&inst.geometry) {
                    let transform = Transform::from_translation(Vec3::from(inst.position)).with_rotation(Quat::from_rotation_y(inst.yaw)).with_scale(Vec3::splat(inst.scale));
                    pieces.extend(mesh.iter().map(|p| (p.clone(),transform)));
                }
            }
            (Vec3::from(position),yaw,surface,water.as_deref().and_then(|w| models.ht_texture(w)))
        }
        CourseSource::H2(code) => {
            let level = riptide_assets::h2level::load_level(&models.content.lux,code).ok()?;
            let names = level.sector_meshes.iter().chain(level.props.iter().map(|p| &p.mesh)).map(|n| format!("mesh32.{n}"))
                .chain([format!("txtr1.wt_{code}_water"),"txtr1.wt_qc_water".to_string()]);
            models.content.lux.prefetch(names);
            if models.content.lux.pending() || level.path.len()<2 { return None; }
            let (position, rotation) = *level.starts.first()?;
            let forward = Quat::from_array(rotation) * Vec3::NEG_Z;
            for name in &level.sector_meshes {
                pieces.extend(models.lux(name)?.iter().map(|p| (p.clone(),Transform::IDENTITY)));
            }
            for prop in &level.props {
                if let Some(mesh) = models.lux(&prop.mesh) {
                    let transform = Transform::from_translation(Vec3::from(prop.position)).with_rotation(Quat::from_array(prop.rotation)).with_scale(Vec3::splat(prop.scale));
                    pieces.extend(mesh.iter().map(|p| (p.clone(),transform)));
                }
            }
            let water = level.water.iter().map(|q| q.corners).chain(level.path.windows(2).map(|w| [w[0].start,w[0].end,w[1].end,w[1].start])).collect();
            (Vec3::from(position),(-forward.x).atan2(-forward.z),water,models.lux_texture(&format!("wt_{code}_water")).or_else(||models.lux_texture("wt_qc_water")))
        }
        CourseSource::Sandbox { .. } => {
            let h = crate::sheets::physics::HACKWORLD_WATER_HALF;
            (Vec3::ZERO,0.0,vec![[[-h,0.0,-h],[h,0.0,-h],[h,0.0,h],[-h,0.0,h]]],models.lux_texture("wt_qc_water"))
        }
    };
    let image = models.images.add(Image::new_target_texture(640,400,TextureFormat::Rgba8UnormSrgb,None));
    let root = commands.spawn((Transform::IDENTITY,Visibility::default(),DespawnOnExit(Screen::Menu),Name::new("course preview"))).id();
    let layer = RenderLayers::layer(1);
    commands.entity(root).with_children(|c| {
        for (piece,transform) in pieces { c.spawn((Mesh3d(piece.mesh),MeshMaterial3d(piece.material),transform,layer.clone())); }
        c.spawn((DirectionalLight { illuminance: 16000.0, ..default() },Transform::from_rotation(Quat::from_euler(EulerRot::YXZ,0.6,-0.8,0.0)),layer.clone()));
        let forward = Quat::from_rotation_y(yaw)*Vec3::NEG_Z;
        // H2 starting grids sit behind large lap banners: frame the course beyond them.
        let view_start = if matches!(choice.source, CourseSource::H2(_)) { start + forward * 400.0 } else { start };
        c.spawn((Camera3d::default(),Camera { order: -1, clear_color: ClearColorConfig::Custom(Color::srgb(0.45,0.68,0.88)), ..default() },
            RenderTarget::Image(ImageRenderTarget { handle: image.clone(), scale_factor: 1.0 }),
            Projection::Perspective(PerspectiveProjection { fov: 65.0f32.to_radians(),near:2.0,far:400000.0,..default() }),
            Transform::from_translation(view_start-forward*180.0+Vec3::Y*160.0).looking_at(view_start+forward*700.0+Vec3::Y*60.0,Vec3::Y),layer.clone()));
    });
    let mut vertices = Vec::new(); let mut uvs = Vec::new(); let mut indices = Vec::new();
    for quad in water {
        let i = vertices.len() as u32;
        for p in quad { vertices.push([p[0],p[1]+0.5,p[2]]); uvs.push([p[0]/350.0,p[2]/350.0]); }
        indices.extend([i,i+1,i+2,i,i+2,i+3]);
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList,RenderAssetUsages::RENDER_WORLD);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL,vec![[0.0,1.0,0.0];vertices.len()]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION,vertices);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0,uvs);mesh.insert_indices(Indices::U32(indices));
    let material = models.materials.add(StandardMaterial { base_color: if texture.is_some() { Color::WHITE } else { Color::srgb(0.03,0.28,0.4) },
        base_color_texture:texture,unlit:true,cull_mode:None,double_sided:true,..default() });
    let water = commands.spawn((Mesh3d(models.meshes.add(mesh)),MeshMaterial3d(material),layer)).id();
    commands.entity(root).add_child(water);
    Some((root,image))
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
        // The browser build fetches the course from the player's files first.
        next.set(if cfg!(target_arch = "wasm32") { Screen::Loading } else { Screen::Race });
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
    let node = commands.spawn((Transform::from_scale(Vec3::splat(info.scale * 1.6)), Visibility::default())).id();
    commands.entity(e).add_child(node);
    match crate::boatrig::spawn(&mut commands, &mut models, &info, node) {
        Some(rig) => {
            commands.entity(node).insert(rig);
        }
        None => {
            if let Some(p) = models.boat(&info) {
                attach(&mut commands, node, &p);
            }
        }
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
    mut q: Query<(&mut Text, &MenuText)>,
) {
    let boats = &models.content.boats;
    let boat = &boats[sel.boat];
    let level = &models.content.tracks[sel.level];
    let shown: Vec<usize> = (0..boats.len()).filter(|&i| unlocks.visible(&cheats, &boats[i])).collect();
    let pos = shown.iter().position(|&i| i == sel.boat).map_or(0, |p| p + 1);
    for (mut text, kind) in &mut q {
        let s = match kind {
            MenuText::Track => level.name.clone(),
            MenuText::Boat => format!("{}\n{}  |  {}  |  {pos}/{}", boat.name, boat.game, boat.row.tier.as_str(), shown.len()),
            MenuText::Detail => format!("{}  |  COURSE {} / {}\nLeft / Right to change course", if level.game == crate::sheets::TracksGame::H2 { "H2Overdrive" } else { "Hydro Thunder" }, sel.level+1, models.content.tracks.len()),
        };
        if text.0 != s { text.0 = s; }
    }
}
