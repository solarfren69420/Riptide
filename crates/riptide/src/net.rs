//! Online races through the Riptide relay (`riptide-server`, protocol `riptide-net`).
//!
//! The menu's lobby panel (O) connects, then hosts a room on the selected course (H) or joins one
//! by its 4-letter code (J). Everyone picks a boat; the host picks the course and starts. Each
//! game drives its own boat and draws the others from their updates ([`Remote`] boats replace
//! the AI). Cheats are locked off for as long as the player is in a room.

use crate::cheats::Cheats;
use crate::content::{Content, CourseSource};
use crate::controls::Input;
use crate::race::{Boat, Crush};
use crate::track::Track;
use crate::sheets::{controls_ids as ctl, physics as phy};
use crate::{Screen, Selection};
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use riptide_net::{decode, encode, BoatState, ClientMsg, Player, ServerMsg, FLAG_AIR, FLAG_BOOST, FLAG_CRUSH, FLAG_SUPER, VERSION};
use std::collections::HashMap;

/// The public relay; `RIPTIDE_SERVER` (desktop) or `?server=` (browser) points elsewhere.
pub const DEFAULT_SERVER: &str = "wss://solarfren.com/riptide/ws";

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Online::new())
            .add_systems(PreUpdate, pump)
            .add_systems(Update, (follow_server, lock_cheats).chain())
            .add_systems(OnEnter(Screen::Menu), spawn_lobby)
            .add_systems(Update, (lobby_input, lobby_text).chain().after(crate::menu::menu_input).run_if(in_state(Screen::Menu)))
            .add_systems(OnExit(Screen::Race), |mut online: ResMut<Online>| online.race = None);
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(mode) = std::env::var("RIPTIDE_ONLINE") {
            app.insert_resource(Script { mode, step: 0 }).add_systems(Update, scripted.before(follow_server));
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Off,
    Connecting,
    Up,
    Down(String),
}

pub enum Typing {
    Name(String),
    Code(String),
}

pub struct RoomView {
    pub code: String,
    pub host: u32,
    pub track: String,
    pub players: Vec<Player>,
    pub racing: bool,
}

pub struct OnlineRace {
    pub track: String,
    /// Grid order.
    pub players: Vec<Player>,
    /// The server said go: the countdown runs.
    pub go: bool,
    /// Latest update per player, with the local time it arrived.
    pub states: HashMap<u32, (BoatState, f32)>,
    pub finished: HashMap<u32, f32>,
    pub left: Vec<u32>,
    pub results: Option<Vec<(u32, Option<f32>)>>,
    entered: bool,
    sent_finish: bool,
    send_in: f32,
}

#[derive(Resource)]
pub struct Online {
    pub server: String,
    pub name: String,
    /// The lobby panel is showing.
    pub open: bool,
    pub link: Link,
    pub id: u32,
    pub room: Option<RoomView>,
    pub race: Option<OnlineRace>,
    /// The last race's results and its players (for the lobby).
    pub last: Option<(Vec<(u32, Option<f32>)>, Vec<Player>)>,
    pub notice: Option<String>,
    pub typing: Option<Typing>,
    want: bool,
    outbox: Vec<ClientMsg>,
}

/// A boat driven by another player.
#[derive(Component)]
pub struct Remote {
    pub id: u32,
}

/// The WebSocket (not `Send` in the browser, so a non-send resource).
struct Conn {
    tx: ewebsock::WsSender,
    rx: ewebsock::WsReceiver,
}

impl Online {
    fn new() -> Self {
        Self {
            server: setting("server").unwrap_or_else(|| DEFAULT_SERVER.into()),
            name: setting("name").or_else(saved_name).unwrap_or_else(|| "Racer".into()),
            open: false,
            link: Link::Off,
            id: 0,
            room: None,
            race: None,
            last: None,
            notice: None,
            typing: None,
            want: false,
            outbox: Vec::new(),
        }
    }

    pub fn send(&mut self, msg: ClientMsg) {
        self.outbox.push(msg);
    }

    pub fn is_host(&self) -> bool {
        self.room.as_ref().is_some_and(|r| r.host == self.id)
    }

    /// In an online race (from the start message to leaving the race screen).
    pub fn racing(&self) -> bool {
        self.race.is_some()
    }

    /// Hold the countdown until everyone has loaded.
    pub fn waiting(&self) -> bool {
        self.race.as_ref().is_some_and(|r| !r.go)
    }

    fn player_name(&self, id: u32) -> String {
        let from = |ps: &[Player]| ps.iter().find(|p| p.id == id).map(|p| p.name.clone());
        self.race
            .as_ref()
            .and_then(|r| from(&r.players))
            .or_else(|| self.room.as_ref().and_then(|r| from(&r.players)))
            .or_else(|| self.last.as_ref().and_then(|(_, ps)| from(ps)))
            .unwrap_or_else(|| format!("Racer {id}"))
    }

    fn receive(&mut self, msg: ServerMsg, now: f32) {
        match msg {
            ServerMsg::Welcome { id } => self.id = id,
            ServerMsg::Room { code, host, track, players, racing } => {
                self.room = Some(RoomView { code, host, track, players, racing });
            }
            ServerMsg::Start { track, players } => {
                self.notice = None;
                self.race = Some(OnlineRace {
                    track,
                    players,
                    go: false,
                    states: HashMap::new(),
                    finished: HashMap::new(),
                    left: Vec::new(),
                    results: None,
                    entered: false,
                    sent_finish: false,
                    send_in: 0.0,
                });
            }
            ServerMsg::Go => {
                if let Some(r) = &mut self.race {
                    r.go = true;
                }
            }
            ServerMsg::State { id, state } => {
                if let Some(r) = &mut self.race {
                    r.states.insert(id, (state, now));
                }
            }
            ServerMsg::Finished { id, time } => {
                if let Some(r) = &mut self.race {
                    r.finished.insert(id, time);
                }
            }
            ServerMsg::Results { order } => {
                if let Some(r) = &mut self.race {
                    r.results = Some(order.clone());
                    self.last = Some((order, r.players.clone()));
                }
            }
            ServerMsg::Left { id } => {
                if let Some(r) = &mut self.race {
                    r.left.push(id);
                }
            }
            ServerMsg::Error { message } => self.notice = Some(message),
        }
    }

    /// Final standings, or who is still racing.
    pub fn standings(&self) -> String {
        let fmt = |t: f32| format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0);
        let Some(r) = &self.race else { return String::new() };
        match &r.results {
            Some(order) => {
                let mut s = String::from("RESULTS\n");
                for (i, (id, t)) in order.iter().enumerate() {
                    let time = t.map_or("did not finish".to_string(), fmt);
                    s += &format!("{}. {}  {time}\n", i + 1, self.player_name(*id));
                }
                s + "Esc: back to the lobby"
            }
            None => {
                let still = r.players.iter().filter(|p| !r.finished.contains_key(&p.id) && !r.left.contains(&p.id)).count();
                format!("Waiting for {still} more racer{}...", if still == 1 { "" } else { "s" })
            }
        }
    }
}

/// `?key=` from the page address (browser) or `RIPTIDE_<KEY>` (desktop).
fn setting(key: &str) -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let search = web_sys::window()?.location().search().ok()?;
        search.trim_start_matches('?').split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key && !v.is_empty()).then(|| js_sys::decode_uri_component(v).ok().and_then(|s| s.as_string()).unwrap_or_else(|| v.to_string()))
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::env::var(format!("RIPTIDE_{}", key.to_uppercase())).ok().filter(|s| !s.is_empty())
    }
}

#[cfg(target_arch = "wasm32")]
fn saved_name() -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item("riptide-name").ok()?
}

#[cfg(target_arch = "wasm32")]
fn save_name(name: &str) {
    if let Some(Ok(Some(s))) = web_sys::window().map(|w| w.local_storage()) {
        let _ = s.set_item("riptide-name", name);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn name_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("riptide/name.txt"))
}

#[cfg(not(target_arch = "wasm32"))]
fn saved_name() -> Option<String> {
    std::fs::read_to_string(name_path()?).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(not(target_arch = "wasm32"))]
fn save_name(name: &str) {
    if let Some(p) = name_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, name);
    }
}

/// Connect when asked; move messages both ways.
fn pump(world: &mut World) {
    let now = world.resource::<Time>().elapsed_secs();
    let connect = {
        let o = world.resource::<Online>();
        o.want && world.get_non_send_resource::<Conn>().is_none()
    };
    if connect {
        let url = world.resource::<Online>().server.clone();
        let mut online = world.resource_mut::<Online>();
        online.want = false;
        match ewebsock::connect(url, ewebsock::Options::default()) {
            Ok((tx, rx)) => {
                online.link = Link::Connecting;
                world.insert_non_send_resource(Conn { tx, rx });
            }
            Err(e) => online.link = Link::Down(e),
        }
    }
    let Some(mut conn) = world.remove_non_send_resource::<Conn>() else { return };
    let mut online = world.resource_mut::<Online>();
    let mut closed = false;
    while let Some(ev) = conn.rx.try_recv() {
        match ev {
            ewebsock::WsEvent::Opened => {
                online.link = Link::Up;
                let hello = ClientMsg::Hello { version: VERSION, name: online.name.clone(), boat: String::new() };
                online.outbox.insert(0, hello);
            }
            ewebsock::WsEvent::Message(ewebsock::WsMessage::Text(t)) => {
                if let Some(m) = decode::<ServerMsg>(&t) {
                    online.receive(m, now);
                }
            }
            ewebsock::WsEvent::Error(e) => {
                online.link = Link::Down(e);
                closed = true;
            }
            ewebsock::WsEvent::Closed => {
                if online.link != Link::Off {
                    online.link = Link::Down("the server closed the connection".into());
                }
                closed = true;
            }
            _ => {}
        }
    }
    if online.link == Link::Up {
        for m in std::mem::take(&mut online.outbox) {
            conn.tx.send(ewebsock::WsMessage::Text(encode(&m)));
        }
    }
    if closed {
        online.room = None;
        online.outbox.clear();
    } else {
        world.insert_non_send_resource(conn);
    }
}

fn lock_cheats(online: Res<Online>, mut cheats: ResMut<Cheats>) {
    let lock = online.room.is_some() || online.racing();
    if cheats.locked != lock {
        cheats.locked = lock;
    }
}

/// Start the race when the server says so; keep the menu's course and boat in step with the room.
fn follow_server(
    mut online: ResMut<Online>,
    mut sel: ResMut<Selection>,
    content: Res<Content>,
    state: Res<State<Screen>>,
    mut next: ResMut<NextState<Screen>>,
) {
    if *state.get() != Screen::Menu {
        return;
    }
    let online = &mut *online;
    if let Some(race) = online.race.as_mut().filter(|r| !r.entered) {
        match content.tracks.iter().position(|t| t.id == race.track) {
            Some(i) => {
                race.entered = true;
                sel.level = i;
                online.open = false;
                // The browser build fetches the course from the player's files first.
                next.set(if cfg!(target_arch = "wasm32") { Screen::Loading } else { Screen::Race });
            }
            None => {
                online.notice = Some(format!("{}: that course isn't in your game files", race.track));
                online.race = None;
                online.room = None;
                online.send(ClientMsg::Leave);
            }
        }
        return;
    }
    let me = online.id;
    let host = online.is_host();
    let Some(room) = online.room.as_mut().filter(|r| !r.racing) else { return };
    let mut out = Vec::new();
    let here = content.tracks.get(sel.level);
    if host {
        if let Some(t) = here.filter(|t| t.id != room.track && !matches!(t.source, CourseSource::Sandbox { .. })) {
            room.track = t.id.to_string();
            out.push(ClientMsg::Track { track: room.track.clone() });
        }
    } else if let Some(i) = content.tracks.iter().position(|t| t.id == room.track) {
        sel.level = i;
    }
    if let Some(b) = content.boats.get(sel.boat) {
        if let Some(p) = room.players.iter_mut().find(|p| p.id == me && p.boat != b.row.id) {
            p.boat = b.row.id.to_string();
            out.push(ClientMsg::Boat { boat: p.boat.clone() });
        }
    }
    for m in out {
        online.send(m);
    }
}

#[derive(Component)]
struct LobbyPanel;

fn spawn_lobby(mut commands: Commands) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            right: percent(4),
            top: percent(9),
            width: percent(36),
            padding: UiRect::all(px(14)),
            border_radius: BorderRadius::all(px(14)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.01, 0.04, 0.09, 0.92)),
        GlobalZIndex(5),
        Visibility::Hidden,
        LobbyPanel,
        DespawnOnExit(Screen::Menu),
        children![(Text::new(""), TextFont { font_size: 14.0, ..default() }, TextColor(Color::srgb(0.85, 0.93, 0.97)))],
    ));
}

fn lobby_input(
    input: Input,
    mut keys_typed: MessageReader<KeyboardInput>,
    mut online: ResMut<Online>,
    sel: Res<Selection>,
    content: Res<Content>,
    cheats: Res<Cheats>,
) {
    if let Some(typing) = online.typing.as_mut() {
        let mut done = None;
        for ev in keys_typed.read().filter(|e| e.state.is_pressed()) {
            match (&ev.logical_key, &mut *typing) {
                (Key::Escape, _) => done = Some(false),
                (Key::Enter, _) => done = Some(true),
                (Key::Backspace, Typing::Name(s) | Typing::Code(s)) => {
                    s.pop();
                }
                (Key::Character(c), Typing::Name(s)) => {
                    s.extend(c.chars().filter(|c| !c.is_control()));
                    *s = s.chars().take(16).collect();
                }
                (Key::Character(c), Typing::Code(s)) => {
                    s.extend(c.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase()));
                    *s = s.chars().take(4).collect();
                }
                (Key::Space, Typing::Name(s)) if s.len() < 16 => s.push(' '),
                _ => {}
            }
        }
        if let Some(Typing::Code(s)) = &online.typing {
            if s.len() == 4 && done.is_none() {
                done = Some(true);
            }
        }
        if let Some(ok) = done {
            match online.typing.take() {
                Some(Typing::Name(s)) if ok && !s.trim().is_empty() => {
                    online.name = s.trim().to_string();
                    save_name(&online.name);
                    // The server learns a new name on the next connection's hello.
                    let name = online.name.clone();
                    online.send(ClientMsg::Hello { version: VERSION, name, boat: String::new() });
                }
                Some(Typing::Code(s)) if ok && s.len() == 4 => online.send(ClientMsg::Join { code: s }),
                _ => {}
            }
        }
        return;
    }
    // Typed characters only matter while typing (the N / J that opened it must not count).
    keys_typed.clear();
    if cheats.menu_open {
        return;
    }
    if input.just_pressed(ctl::ONLINE) {
        online.open = !online.open;
        // Opening the lobby connects (or retries a failed connection).
        if online.open && matches!(online.link, Link::Off | Link::Down(_)) {
            online.want = true;
            online.link = Link::Connecting;
        }
    }
    if !online.open && online.room.is_none() {
        return;
    }
    if input.just_pressed(ctl::LOBBY_NAME) {
        online.typing = Some(Typing::Name(String::new()));
        return;
    }
    if input.just_pressed(ctl::MENU_QUIT) {
        if online.room.is_some() {
            online.send(ClientMsg::Leave);
            online.room = None;
        } else {
            online.open = false;
        }
        return;
    }
    if online.room.is_some() || online.link != Link::Up {
        return;
    }
    if input.just_pressed(ctl::LOBBY_HOST) {
        match content.tracks.get(sel.level) {
            Some(t) if matches!(t.source, CourseSource::Sandbox { .. }) => {
                online.notice = Some("Hackworld is a sandbox: pick a real course to race online".into());
            }
            Some(t) => {
                online.notice = None;
                let track = t.id.to_string();
                online.send(ClientMsg::Create { track });
            }
            None => {}
        }
    }
    if input.just_pressed(ctl::LOBBY_JOIN) {
        online.notice = None;
        online.typing = Some(Typing::Code(String::new()));
    }
}

fn lobby_text(
    online: Res<Online>,
    content: Res<Content>,
    sel: Res<Selection>,
    mut panel: Query<(&mut Visibility, &Children), With<LobbyPanel>>,
    mut texts: Query<&mut Text>,
) {
    let Ok((mut vis, children)) = panel.single_mut() else { return };
    let show = online.open || online.room.is_some();
    let want = if show { Visibility::Visible } else { Visibility::Hidden };
    if *vis != want {
        *vis = want;
    }
    if !show {
        return;
    }
    let course = |id: &str| content.tracks.iter().find(|t| t.id == id).map_or(id.to_string(), |t| t.name.clone());
    let boat = |id: &str| content.boats.iter().find(|b| b.row.id == id).map_or(id.to_string(), |b| b.name.clone());
    let mut s = String::from("ONLINE RACES\n");
    s += &match &online.link {
        Link::Off | Link::Connecting => "Connecting...\n".to_string(),
        Link::Up => String::new(),
        Link::Down(e) => format!("Can't reach the race server ({e}).\nO: close, then O again to retry.\n"),
    };
    s += &match &online.typing {
        Some(Typing::Name(n)) => format!("\nYour name: {n}_\nEnter: save   Esc: cancel\n"),
        _ => format!("Name: {}   (N: change)\n", online.name),
    };
    match (&online.room, &online.typing) {
        (_, Some(Typing::Code(c))) => s += &format!("\nRoom code: {c}{}\nType the 4 letters   Esc: cancel\n", "_".repeat(4 - c.len())),
        (None, _) if online.link == Link::Up => {
            let here = content.tracks.get(sel.level).map_or(String::new(), |t| t.name.clone());
            s += &format!("\nH  Host a room on {here}\nJ  Join a room by its code\nEsc  Close\n");
        }
        (Some(r), _) => {
            s += &format!("\nROOM {}   (friends join with this code)\nCourse: {}\n\n", r.code, course(&r.track));
            for p in &r.players {
                let tags = [(p.id == r.host, "host"), (p.id == online.id, "you")]
                    .iter()
                    .filter(|(on, _)| *on)
                    .map(|(_, t)| *t)
                    .collect::<Vec<_>>()
                    .join(", ");
                let tags = if tags.is_empty() { String::new() } else { format!("  ({tags})") };
                s += &format!("  {}  -  {}{tags}\n", p.name, if p.boat.is_empty() { "choosing".into() } else { boat(&p.boat) });
            }
            s += &if r.racing {
                "\nThis room is racing...\n".to_string()
            } else if r.host == online.id {
                "\nEnter: START RACE\nLeft/Right: course   Up/Down: boat\n".to_string()
            } else {
                "\nWaiting for the host to start.   Up/Down: boat\n".to_string()
            };
            s += "Esc: leave the room\n";
        }
        _ => {}
    }
    if let Some((order, players)) = &online.last {
        s += "\nLast race:\n";
        for (i, (id, t)) in order.iter().enumerate().take(8) {
            let name = players.iter().find(|p| p.id == *id).map_or("?", |p| p.name.as_str());
            let time = t.map_or("did not finish".to_string(), |t| format!("{}:{:05.2}", (t / 60.0) as u32, t % 60.0));
            s += &format!("  {}. {name}  {time}\n", i + 1);
        }
    }
    if let Some(n) = &online.notice {
        s += &format!("\n{n}\n");
    }
    s += "\nCheats are off in online races.";
    if let Some(mut t) = children.first().and_then(|c| texts.get_mut(*c).ok()) {
        if t.0 != s {
            t.0 = s;
        }
    }
}

/// The boats on the grid have spawned: tell the server this game is ready.
pub fn race_ready(online: Option<ResMut<Online>>) {
    if let Some(mut online) = online.filter(|o| o.racing()) {
        online.send(ClientMsg::Ready);
    }
}

/// The player's boat to the server, `net_send_hz` times a second, and its finish.
pub fn send_state(time: Res<Time>, online: Option<ResMut<Online>>, boats: Query<&Boat, Without<Remote>>) {
    let Some(mut online) = online else { return };
    let Some(p) = boats.iter().find(|b| b.player) else { return };
    let Some(race) = online.race.as_mut().filter(|r| r.go) else { return };
    let mut out = Vec::new();
    race.send_in -= time.delta_secs();
    if race.send_in <= 0.0 {
        race.send_in += 1.0 / phy::NET_SEND_HZ.max(1.0);
        race.send_in = race.send_in.max(0.0);
        let flags = if p.boosting { FLAG_BOOST } else { 0 }
            | if p.super_time > 0.0 { FLAG_SUPER } else { 0 }
            | if p.crush.smashing() { FLAG_CRUSH } else { 0 }
            | if p.airborne { FLAG_AIR } else { 0 };
        out.push(ClientMsg::State(BoatState {
            t: 0.0,
            pos: p.pos.to_array(),
            vel: p.vel.to_array(),
            vy: p.vy,
            yaw: p.yaw,
            speed: p.speed,
            steer: p.control.steer,
            wipeout: p.wipeout,
            flags,
            lap: p.lap,
        }));
    }
    if let Some(t) = p.finished.filter(|_| !race.sent_finish) {
        race.sent_finish = true;
        out.push(ClientMsg::Finish { time: t });
    }
    for m in out {
        online.send(m);
    }
}

/// Move the other players' boats to where their updates say they are.
pub fn apply_remote(
    mut commands: Commands,
    time: Res<Time>,
    online: Option<Res<Online>>,
    track: Res<Track>,
    mut boats: Query<(Entity, &Remote, &mut Boat)>,
) {
    let Some(race) = online.as_ref().and_then(|o| o.race.as_ref()) else { return };
    let now = time.elapsed_secs();
    let k = (phy::NET_SMOOTH * time.delta_secs()).min(1.0);
    for (e, r, mut b) in &mut boats {
        if race.left.contains(&r.id) {
            commands.entity(e).despawn();
            continue;
        }
        if let Some((s, at)) = race.states.get(&r.id) {
            let age = (now - at).clamp(0.0, phy::NET_EXTRAPOLATE);
            let vel = Vec2::from(s.vel);
            let target = Vec3::from(s.pos) + Vec3::new(vel.x, 0.0, vel.y) * age;
            // Far off (a respawn, the first update): jump there.
            b.pos = if b.pos.distance(target) > 600.0 { target } else { b.pos + (target - b.pos) * k };
            b.vel = vel;
            b.vy = s.vy;
            let turn = (s.yaw - b.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            b.yaw += turn * k;
            b.speed = s.speed;
            b.control.steer = s.steer;
            b.control.boost = s.flags & FLAG_BOOST != 0;
            b.boosting = b.control.boost;
            b.super_time = if s.flags & FLAG_SUPER != 0 { b.super_time.max(0.5) } else { 0.0 };
            b.crush = if s.flags & FLAG_CRUSH != 0 { Crush::Active(1.0) } else { Crush::Off };
            b.airborne = s.flags & FLAG_AIR != 0;
            b.wipeout = s.wipeout;
            b.lap = s.lap;
            b.surface = b.pos.y;
            b.tp = track.locate(b.pos, b.tp.seg);
        }
        if let Some(t) = race.finished.get(&r.id) {
            b.finished.get_or_insert(*t);
        }
    }
}

/// Scripted rooms (desktop, for tests and command-line hosting): `RIPTIDE_ONLINE=host:<players>`
/// hosts the selected course and starts once that many racers are in; `join:<code>` joins.
/// Each step is logged (`ONLINE ...`); with `RIPTIDE_EXIT_ON_FINISH` the game quits on results.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource)]
struct Script {
    mode: String,
    step: u32,
}

#[cfg(not(target_arch = "wasm32"))]
fn scripted(
    mut script: ResMut<Script>,
    mut online: ResMut<Online>,
    sel: Res<Selection>,
    content: Res<Content>,
    mut exit: MessageWriter<AppExit>,
    mut seen: Local<(bool, bool, bool, bool)>,
) {
    if online.link == Link::Off {
        online.want = true;
        online.link = Link::Connecting;
    }
    if let Link::Down(e) = &online.link {
        error!("ONLINE link down: {e}");
        exit.write(AppExit::error());
        return;
    }
    if online.link != Link::Up {
        return;
    }
    let (kind, arg) = script.mode.split_once(':').unwrap_or((script.mode.as_str(), ""));
    let (kind, arg) = (kind.to_string(), arg.to_string());
    match (kind.as_str(), script.step) {
        ("host", 0) => {
            let track = content.tracks[sel.level].id.to_string();
            online.send(ClientMsg::Create { track });
            script.step = 1;
        }
        ("host", 1) => {
            let want: usize = arg.parse().unwrap_or(2);
            if online.room.as_ref().is_some_and(|r| r.players.len() >= want && !r.racing) {
                online.send(ClientMsg::Start);
                script.step = 2;
            }
        }
        ("join", 0) => {
            online.send(ClientMsg::Join { code: arg });
            script.step = 1;
        }
        _ => {}
    }
    if let Some(r) = online.room.as_ref().filter(|_| !seen.0) {
        seen.0 = true;
        info!("ONLINE room {} on {} as {}", r.code, r.track, online.id);
    }
    if let Some(n) = online.notice.as_ref().filter(|_| !seen.3) {
        seen.3 = true;
        warn!("ONLINE notice: {n}");
    }
    let Some(race) = &online.race else { return };
    if !seen.1 && race.go {
        seen.1 = true;
        info!("ONLINE go: {} racers on {}", race.players.len(), race.track);
    }
    if let Some(order) = race.results.as_ref().filter(|_| !seen.2) {
        seen.2 = true;
        let line: Vec<String> = order.iter().map(|(id, t)| format!("{id}={}", t.map_or("DNF".into(), |t| format!("{t:.2}")))).collect();
        info!("ONLINE results {}", line.join(" "));
        if std::env::var_os("RIPTIDE_EXIT_ON_FINISH").is_some() {
            exit.write(AppExit::Success);
        }
    }
}
