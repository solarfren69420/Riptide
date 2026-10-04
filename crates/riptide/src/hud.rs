//! H2Overdrive's own HUD: the `h2_hud` cards (1360x768 virtual screen, X Anchor 0 left /
//! 1 centre / 2 right) that the `hud` sheet selects, drawn with the game's art and bitmap fonts
//! (`h2_font_glyphs`).

use crate::cheats::Tuning;
use crate::content::{CourseSource, Models};
use crate::race::{ArcadeTimer, Boat, RaceClock};
use crate::sheets::{physics as phy, HudRole, H2_FONT_GLYPHS, H2_HUD, H2_LEVELS, HUD};
use crate::{Screen, Selection};
use bevy::prelude::*;
use std::collections::HashMap;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(Screen::Race), spawn)
            .add_systems(Update, (needles, texts, radar, banners).run_if(in_state(Screen::Race)));
    }
}

/// Virtual-screen pixels to viewport width (the layout keeps its 1360x768 aspect).
fn v(px: f32) -> Val {
    vw(px / 1360.0 * 100.0)
}

#[derive(Component)]
struct Needle(HudRole, f32, f32);

#[derive(Component)]
struct BitmapText {
    role: HudRole,
    font: &'static str,
    scale: f32,
    color: Color,
    shown: String,
}

#[derive(Component)]
struct RadarMap {
    level: usize,
    size: f32,
}

#[derive(Component)]
struct Blip(usize);

#[derive(Component)]
struct Banner(HudRole);

/// Glyphs by (font, code point).
#[derive(Resource)]
struct Glyphs(HashMap<(&'static str, u32), usize>, HashMap<&'static str, Handle<Image>>);

fn spawn(mut commands: Commands, mut models: Models, sel: Res<Selection>) {
    let scope = DespawnOnExit(Screen::Race);
    let level = match models.content.tracks.get(sel.level).map(|t| &t.source) {
        Some(CourseSource::H2(code)) => H2_LEVELS.iter().position(|l| l.id == *code),
        _ => None,
    };
    let mut glyphs = HashMap::new();
    let mut atlases = HashMap::new();
    for (i, g) in H2_FONT_GLYPHS.iter().enumerate() {
        glyphs.insert((g.font, g.code as u32), i);
        if !atlases.contains_key(g.font) {
            if let Some(h) = models.lux_texture(g.font) {
                atlases.insert(g.font, h);
            }
        }
    }
    let root = commands
        .spawn((Node { width: percent(100), height: percent(100), position_type: PositionType::Absolute, ..default() }, GlobalZIndex(5), scope))
        .id();
    for row in HUD.iter().filter(|r| r.status.is_ok()) {
        let Some(card) = row.card.map(|c| &H2_HUD[c]) else { continue };
        let (w, h) = (card.x_size.max(1.0), card.y_size.max(1.0));
        let mut node = Node { position_type: PositionType::Absolute, top: v(card.y_position), width: v(w), height: v(h), ..default() };
        match card.x_anchor {
            2 => node.right = v(card.x_position),
            1 => {
                // Centred cards: span the screen and centre their content.
                node.left = px(0);
                node.width = percent(100);
                node.justify_content = JustifyContent::Center;
            }
            _ => node.left = v(card.x_position),
        }
        // Blend mode 7 (Lighten) art is drawn on black: key it; everything is tinted by the card's
        // Background Motif colour (the needle colours come from there).
        let tex = (!card.texture_name.is_empty())
            .then(|| if card.photoshop_blending_mode == 7 { models.hud_lighten(card.texture_name) } else { models.lux_texture(card.texture_name) })
            .flatten();
        let bg = card.background_motif_motif_color;
        let tint = Color::linear_rgba(bg.first().copied().unwrap_or(1.0), bg.get(1).copied().unwrap_or(1.0), bg.get(2).copied().unwrap_or(1.0), bg.get(3).copied().unwrap_or(1.0));
        let mut e = commands.spawn(node);
        match row.role {
            HudRole::Static => {
                if let Some(t) = tex {
                    e.insert(ImageNode { image: t, color: tint, ..default() });
                }
            }
            HudRole::SpeedNeedle | HudRole::BoostNeedle => {
                // Rotate around the visible dial ring. The boost ring sits in the upper-left
                // of its texture, so its sheet pivot overrides the needle card centre.
                let (cx, cy) = if row.pivot_x > 0.0 && row.pivot_y > 0.0 {
                    (row.pivot_x, row.pivot_y)
                } else {
                    (card.x_position + w / 2.0, card.y_position + h / 2.0)
                };
                // A zero-size pivot at the dial centre; the needle sprite pivots on its ring
                // (72% along) and points left at rest.
                let len = w * row.scale;
                e.insert(Node {
                    position_type: PositionType::Absolute,
                    left: v(cx),
                    top: v(cy),
                    width: px(0),
                    height: px(0),
                    ..default()
                });
                e.insert(Needle(row.role, row.angle_min, row.angle_span));
                if let Some(t) = tex {
                    e.with_children(|c| {
                        c.spawn((
                            Node {
                                position_type: PositionType::Absolute,
                                left: v(-0.72 * len),
                                top: v(-len / 8.0),
                                width: v(len),
                                height: v(len / 4.0),
                                ..default()
                            },
                            ImageNode { image: t, color: tint, ..default() },
                        ));
                    });
                }
            }
            HudRole::MphText | HudRole::RaceTime | HudRole::TimeRemaining | HudRole::Position => {
                let c = card.text_color;
                e.insert(BitmapText {
                    role: row.role,
                    font: card.font_name,
                    scale: card.text_scale.max(0.1),
                    color: Color::linear_rgba(c.first().copied().unwrap_or(1.0), c.get(1).copied().unwrap_or(1.0), c.get(2).copied().unwrap_or(1.0), 1.0),
                    shown: String::new(),
                });
                e.insert(Node { flex_direction: FlexDirection::Row, align_items: AlignItems::FlexEnd, ..e_node(card) });
            }
            HudRole::RadarMap => {
                let name = format!("ht_radar_map_{}", H2_LEVELS[level.unwrap_or(0)].id);
                if let (Some(l), Some(t)) = (level, models.lux_texture(&name)) {
                    // The square inscribed in the round frame, so no corners stick out.
                    let side = w * std::f32::consts::FRAC_1_SQRT_2;
                    let inset = (w - side) / 2.0;
                    e.insert(Node { position_type: PositionType::Absolute, right: v(card.x_position + inset), top: v(card.y_position + inset), width: v(side), height: v(side), ..default() });
                    e.insert((ImageNode { image: t, rect: Some(Rect::new(0.0, 0.0, 1.0, 1.0)), ..default() }, RadarMap { level: l, size: side }));
                }
            }
            HudRole::RadarBlips => {
                // Blips live inside the radar card: one per rival, placed each frame.
                let radar = HUD.iter().find(|r| r.role == HudRole::RadarMap).and_then(|r| r.card).map(|c| &H2_HUD[c]);
                if let Some(rc) = radar {
                    // The same inscribed square as the map: blips are placed in its coordinates.
                    let side = rc.x_size.max(1.0) * std::f32::consts::FRAC_1_SQRT_2;
                    let inset = (rc.x_size.max(1.0) - side) / 2.0;
                    e.insert(Node { position_type: PositionType::Absolute, right: v(rc.x_position + inset), top: v(rc.y_position + inset), width: v(side), height: v(side), ..default() });
                    e.with_children(|c| {
                        for i in 0..phy::RACERS as usize {
                            c.spawn((
                                Node { position_type: PositionType::Absolute, width: v(9.0), height: v(9.0), border_radius: BorderRadius::MAX, ..default() },
                                BackgroundColor(Color::srgb(1.0, 0.85, 0.1)),
                                Visibility::Hidden,
                                Blip(i),
                            ));
                        }
                        // The player: white, in the middle (the map scrolls under it).
                        c.spawn((
                            Node { position_type: PositionType::Absolute, left: percent(50), top: percent(50), margin: UiRect::all(v(-5.5)), width: v(11.0), height: v(11.0), border_radius: BorderRadius::MAX, ..default() },
                            BackgroundColor(Color::WHITE),
                            BorderColor::all(Color::BLACK),
                        ));
                    });
                }
            }
            HudRole::BannerTimeExtended | HudRole::BannerGo => {
                if let Some(t) = tex {
                    e.insert((ImageNode::new(t), Banner(row.role), Visibility::Hidden));
                }
            }
        }
        let id = e.id();
        commands.entity(root).add_child(id);
    }
    commands.insert_resource(Glyphs(glyphs, atlases));
}

/// A text card's own box (its anchor handling is done by the caller).
fn e_node(card: &crate::sheets::H2HudRow) -> Node {
    let mut n = Node { position_type: PositionType::Absolute, top: v(card.y_position), height: v(card.y_size.max(1.0)), ..default() };
    match card.x_anchor {
        2 => n.right = v(card.x_position),
        1 => {
            n.left = px(0);
            n.width = percent(100);
            n.justify_content = JustifyContent::Center;
        }
        _ => n.left = v(card.x_position),
    }
    n
}

fn needles(boats: Query<&Boat>, tuning: Res<Tuning>, mut q: Query<(&Needle, &mut UiTransform)>, mut missing: Query<(Entity, &Needle), Without<UiTransform>>, mut commands: Commands) {
    for (e, _) in &mut missing {
        commands.entity(e).insert(UiTransform::IDENTITY);
    }
    let Some(p) = boats.iter().find(|b| b.player) else { return };
    let g = &tuning.0;
    for (n, mut t) in &mut q {
        let deg = match n.0 {
            // The game's MPH-to-angle factor, clamped to the dial (hud.angle_span).
            HudRole::SpeedNeedle => (p.vel.length() / phy::SPEED_SCALE * g.needle_mph_scale_factor).clamp(0.0, n.2) + n.1,
            // Fuel across the boost face's arc (hud.angle_min, angle_span).
            _ => n.1 + (p.fuel / g.boost_fuel_max_regular.max(1e-3)).clamp(0.0, 1.0) * n.2,
        };
        t.rotation = Rot2::degrees(deg);
    }
}

#[allow(clippy::too_many_arguments)]
fn texts(
    mut commands: Commands,
    glyphs: Option<Res<Glyphs>>,
    boats: Query<(Entity, &Boat)>,
    clock: Res<RaceClock>,
    timer: Option<Res<ArcadeTimer>>,
    track: Res<crate::track::Track>,
    tuning: Res<Tuning>,
    mut q: Query<(Entity, &mut BitmapText, Option<&Children>)>,
) {
    let Some(glyphs) = glyphs else { return };
    let Some((pe, p)) = boats.iter().find(|(_, b)| b.player) else { return };
    let place = {
        let mut order: Vec<(f32, Entity)> = boats
            .iter()
            .map(|(e, b)| match clock.finish_order.iter().position(|x| *x == e) {
                Some(i) => (1e9 - i as f32, e),
                None => (track.race_distance(b.lap, b.tp.progress), e),
            })
            .collect();
        order.sort_by(|a, b| b.0.total_cmp(&a.0));
        order.iter().position(|(_, e)| *e == pe).unwrap_or(0) + 1
    };
    for (e, mut bt, children) in &mut q {
        let s = match bt.role {
            HudRole::MphText => format!("{:.0}", p.vel.length() / phy::SPEED_SCALE * tuning.0.digital_mph_scale_factor),
            HudRole::RaceTime => {
                let t = p.finished.unwrap_or(clock.t).max(0.0);
                format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0)
            }
            HudRole::TimeRemaining => match timer.as_ref().filter(|t| t.enabled) {
                Some(t) => format!("{:.0}", t.left.ceil()),
                None => String::new(),
            },
            _ => format!("{place}/{}", boats.iter().count()),
        };
        if s == bt.shown {
            continue;
        }
        bt.shown = s.clone();
        if let Some(ch) = children {
            for c in ch.iter() {
                commands.entity(c).despawn();
            }
        }
        let (font, scale, color) = (bt.font, bt.scale, bt.color);
        commands.entity(e).with_children(|c| {
            for ch in s.chars() {
                // Glyphs a font lacks (ht_font04 is digits only) come from ht_font02, then
                // font_Magnum_150 (which has `/`), sized to this font's digit height.
                let digit_h = |f: &str| glyphs.0.get(&(f, '0' as u32)).map(|&i| H2_FONT_GLYPHS[i].h as f32);
                let found = [font, "ht_font02", "font_Magnum_150"].into_iter().find_map(|f| glyphs.0.get(&(f, ch as u32)).map(|&i| (f, i)));
                let Some((from, gi)) = found else {
                    c.spawn(Node { width: v(8.0 * scale), ..default() });
                    continue;
                };
                let scale = match (digit_h(font), digit_h(from)) {
                    (Some(a), Some(b)) if from != font && b > 0.0 => scale * a / b,
                    _ => scale,
                };
                let g = &H2_FONT_GLYPHS[gi];
                let Some(atlas) = glyphs.1.get(g.font).cloned() else { continue };
                c.spawn((
                    Node {
                        width: v(g.w as f32 * scale),
                        height: v(g.h as f32 * scale),
                        margin: UiRect { right: v((g.dw - g.w) as f32 * scale), bottom: v((g.dy - g.h) as f32 * scale), ..default() },
                        ..default()
                    },
                    ImageNode {
                        image: atlas.clone(),
                        rect: Some(Rect::new(g.x as f32, g.y as f32, (g.x + g.w) as f32, (g.y + g.h) as f32)),
                        color,
                        ..default()
                    },
                ));
            }
        });
    }
}

/// World XZ (game coordinates, Z un-mirrored) to radar map texels via the level's two
/// calibration points (CLevelInfo Map Texel/World 0 and 1). The texels are in a 1024-px
/// map's space for every level (all under 1024), which is how the 2048-px maps load.
fn texel(level: usize, p: Vec3) -> Vec2 {
    let l = &H2_LEVELS[level];
    let g = |v: &[f32]| Vec2::new(v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0));
    let (t0, t1, w0, w1) = (g(l.map_texel0), g(l.map_texel1), g(l.map_world0), g(l.map_world1));
    let world = Vec2::new(p.x, -p.z);
    let f = (world - w0) / (w1 - w0);
    t0 + (t1 - t0) * f
}

fn radar(boats: Query<&Boat>, mut map: Query<(&RadarMap, &mut ImageNode)>, mut blips: Query<(&Blip, &mut Node, &mut Visibility)>) {
    let Some(p) = boats.iter().find(|b| b.player) else { return };
    let Ok((rm, mut img)) = map.single_mut() else { return };
    let c = texel(rm.level, p.pos);
    let r = phy::RADAR_RANGE;
    img.rect = Some(Rect::from_center_half_size(c, Vec2::splat(r)));
    let rivals: Vec<&Boat> = boats.iter().filter(|b| !b.player).collect();
    for (b, mut node, mut vis) in &mut blips {
        let Some(o) = rivals.get(b.0) else {
            *vis = Visibility::Hidden;
            continue;
        };
        let d = (texel(rm.level, o.pos) - c) / r * 0.5 + 0.5;
        if d.x < 0.05 || d.x > 0.95 || d.y < 0.05 || d.y > 0.95 {
            *vis = Visibility::Hidden;
            continue;
        }
        *vis = Visibility::Inherited;
        node.left = v(d.x * rm.size - 4.5);
        node.top = v(d.y * rm.size - 4.5);
    }
}

fn banners(clock: Res<RaceClock>, timer: Option<Res<ArcadeTimer>>, mut q: Query<(&Banner, &mut Visibility)>) {
    for (b, mut vis) in &mut q {
        let on = match b.0 {
            HudRole::BannerGo => (0.0..1.2).contains(&clock.t),
            _ => timer.as_ref().is_some_and(|t| t.banner.is_some()),
        };
        *vis = if on { Visibility::Inherited } else { Visibility::Hidden };
    }
}
