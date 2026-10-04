//! Riptide relay server: online race rooms over WebSockets (protocol: `riptide-net`).
//!
//! It never simulates anything: rooms with join codes, a ready check before the countdown,
//! relaying boat states, and the finish order. Listens on `RIPTIDE_RELAY_ADDR` (default
//! `127.0.0.1:3020`); put it behind a TLS proxy (nginx) for `wss://`.

use futures_util::{SinkExt, StreamExt};
use riptide_net::{decode, encode, ClientMsg, Player, ServerMsg, FINISH_GRACE, MAX_PLAYERS, MAX_RATE, READY_TIMEOUT, VERSION};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

/// Open connections at most (a small server: rooms of eight).
const MAX_CLIENTS: usize = 256;

struct Client {
    tx: UnboundedSender<String>,
    name: String,
    boat: String,
    room: Option<String>,
}

#[derive(PartialEq)]
enum Phase {
    Lobby,
    /// Loading the course: waiting for every game's Ready.
    Loading,
    Racing,
}

struct Room {
    host: u32,
    track: String,
    players: Vec<u32>,
    phase: Phase,
    /// Bumped per race, so a timer from an earlier race does nothing.
    race: u32,
    ready: Vec<u32>,
    finished: Vec<(u32, f32)>,
}

/// Work for later: (delay, room code, race number, what).
type Timer = (f32, String, u32, Due);

#[derive(Clone, Copy)]
enum Due {
    Go,
    End,
}

#[derive(Default)]
struct Hub {
    next_id: u32,
    clients: HashMap<u32, Client>,
    rooms: HashMap<String, Room>,
}

impl Hub {
    fn send(&self, id: u32, msg: &ServerMsg) {
        if let Some(c) = self.clients.get(&id) {
            let _ = c.tx.send(encode(msg));
        }
    }

    fn broadcast(&self, code: &str, msg: &ServerMsg, except: Option<u32>) {
        let text = encode(msg);
        if let Some(r) = self.rooms.get(code) {
            for id in r.players.iter().filter(|id| Some(**id) != except) {
                if let Some(c) = self.clients.get(id) {
                    let _ = c.tx.send(text.clone());
                }
            }
        }
    }

    fn players(&self, code: &str) -> Vec<Player> {
        let Some(r) = self.rooms.get(code) else { return Vec::new() };
        r.players
            .iter()
            .filter_map(|id| self.clients.get(id).map(|c| Player { id: *id, name: c.name.clone(), boat: c.boat.clone() }))
            .collect()
    }

    fn room_update(&self, code: &str) {
        if let Some(r) = self.rooms.get(code) {
            let msg = ServerMsg::Room { code: code.to_string(), host: r.host, track: r.track.clone(), players: self.players(code), racing: r.phase != Phase::Lobby };
            self.broadcast(code, &msg, None);
        }
    }

    /// A free 4-letter room code.
    fn new_code(&self, seed: u32) -> String {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let mut x = (seed.wrapping_mul(2_654_435_761) ^ nanos) | 1;
        loop {
            let code: String = (0..4)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    // No I or O: easy to read out loud.
                    b"ABCDEFGHJKLMNPQRSTUVWXYZ"[(x % 24) as usize] as char
                })
                .collect();
            if !self.rooms.contains_key(&code) {
                return code;
            }
        }
    }

    fn go(&mut self, code: &str) {
        let Some(r) = self.rooms.get_mut(code).filter(|r| r.phase == Phase::Loading) else { return };
        r.phase = Phase::Racing;
        self.broadcast(code, &ServerMsg::Go, None);
    }

    /// Start once every player still in the room has loaded.
    fn check_ready(&mut self, code: &str) {
        if self.rooms.get(code).is_some_and(|r| r.phase == Phase::Loading && r.players.iter().all(|p| r.ready.contains(p))) {
            self.go(code);
        }
    }

    /// End the race: everyone finished, or the grace after the first finisher ran out.
    fn end(&mut self, code: &str, force: bool) {
        let Some(r) = self.rooms.get_mut(code) else { return };
        let all = r.players.iter().all(|id| r.finished.iter().any(|(f, _)| f == id));
        if r.phase == Phase::Lobby || !(all || force) {
            return;
        }
        r.phase = Phase::Lobby;
        let mut order: Vec<(u32, Option<f32>)> = r.finished.iter().map(|(id, t)| (*id, Some(*t))).collect();
        order.sort_by(|a, b| a.1.unwrap_or(f32::MAX).total_cmp(&b.1.unwrap_or(f32::MAX)));
        order.extend(r.players.iter().filter(|id| !r.finished.iter().any(|(f, _)| f == *id)).map(|id| (*id, None)));
        self.broadcast(code, &ServerMsg::Results { order }, None);
        self.room_update(code);
    }

    fn leave(&mut self, id: u32) {
        let Some(code) = self.clients.get_mut(&id).and_then(|c| c.room.take()) else { return };
        let Some(r) = self.rooms.get_mut(&code) else { return };
        r.players.retain(|p| *p != id);
        if r.host == id {
            if let Some(next) = r.players.first() {
                r.host = *next;
            }
        }
        if r.players.is_empty() {
            self.rooms.remove(&code);
            return;
        }
        self.broadcast(&code, &ServerMsg::Left { id }, None);
        self.check_ready(&code);
        self.end(&code, false);
        self.room_update(&code);
    }

    fn timer(&mut self, code: &str, race: u32, due: Due) {
        if self.rooms.get(code).is_some_and(|r| r.race == race) {
            match due {
                Due::Go => self.go(code),
                Due::End => self.end(code, true),
            }
        }
    }

    fn handle(&mut self, id: u32, msg: ClientMsg) -> Option<Timer> {
        let room = self.clients.get(&id).and_then(|c| c.room.clone());
        match msg {
            ClientMsg::Hello { version, name, boat } => {
                if version != VERSION {
                    self.send(id, &ServerMsg::Error { message: format!("game version {version}, server speaks {VERSION}: reload the page") });
                    return None;
                }
                if let Some(c) = self.clients.get_mut(&id) {
                    let name: String = name.chars().filter(|c| !c.is_control()).take(16).collect();
                    if !name.trim().is_empty() {
                        c.name = name.trim().to_string();
                    }
                    c.boat = boat.chars().take(40).collect();
                }
                self.send(id, &ServerMsg::Welcome { id });
            }
            ClientMsg::Create { track } => {
                self.leave(id);
                let code = self.new_code(id);
                let track = track.chars().take(40).collect();
                self.rooms.insert(code.clone(), Room { host: id, track, players: vec![id], phase: Phase::Lobby, race: 0, ready: Vec::new(), finished: Vec::new() });
                if let Some(c) = self.clients.get_mut(&id) {
                    c.room = Some(code.clone());
                }
                self.room_update(&code);
            }
            ClientMsg::Join { code } => {
                let code = code.trim().to_ascii_uppercase();
                let problem = match self.rooms.get(&code) {
                    None => Some("no room with that code"),
                    Some(r) if r.phase != Phase::Lobby => Some("that room is racing: try again when the race ends"),
                    Some(r) if r.players.len() >= MAX_PLAYERS => Some("that room is full"),
                    _ => None,
                };
                if let Some(message) = problem {
                    self.send(id, &ServerMsg::Error { message: message.into() });
                    return None;
                }
                self.leave(id);
                if let Some(r) = self.rooms.get_mut(&code) {
                    r.players.push(id);
                }
                if let Some(c) = self.clients.get_mut(&id) {
                    c.room = Some(code.clone());
                }
                self.room_update(&code);
            }
            ClientMsg::Boat { boat } => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.boat = boat.chars().take(40).collect();
                }
                if let Some(code) = room {
                    self.room_update(&code);
                }
            }
            ClientMsg::Track { track } => {
                let code = room?;
                if let Some(r) = self.rooms.get_mut(&code).filter(|r| r.host == id && r.phase == Phase::Lobby) {
                    r.track = track.chars().take(40).collect();
                }
                self.room_update(&code);
            }
            ClientMsg::Start => {
                let code = room?;
                let r = self.rooms.get_mut(&code).filter(|r| r.host == id && r.phase == Phase::Lobby)?;
                r.phase = Phase::Loading;
                r.race += 1;
                r.ready.clear();
                r.finished.clear();
                let (track, race) = (r.track.clone(), r.race);
                let msg = ServerMsg::Start { track, players: self.players(&code) };
                self.broadcast(&code, &msg, None);
                self.room_update(&code);
                return Some((READY_TIMEOUT, code, race, Due::Go));
            }
            ClientMsg::Ready => {
                let code = room?;
                if let Some(r) = self.rooms.get_mut(&code).filter(|r| r.phase == Phase::Loading) {
                    if !r.ready.contains(&id) {
                        r.ready.push(id);
                    }
                }
                self.check_ready(&code);
            }
            ClientMsg::State(state) => {
                let code = room?;
                if self.rooms.get(&code).is_some_and(|r| r.phase == Phase::Racing) {
                    self.broadcast(&code, &ServerMsg::State { id, state }, Some(id));
                }
            }
            ClientMsg::Finish { time } => {
                let code = room?;
                let r = self.rooms.get_mut(&code).filter(|r| r.phase == Phase::Racing)?;
                if r.finished.iter().any(|(f, _)| *f == id) || !time.is_finite() || time <= 0.0 {
                    return None;
                }
                r.finished.push((id, time));
                let (first, race) = (r.finished.len() == 1, r.race);
                self.broadcast(&code, &ServerMsg::Finished { id, time }, None);
                self.end(&code, false);
                if first {
                    return Some((FINISH_GRACE, code, race, Due::End));
                }
            }
            ClientMsg::Leave => self.leave(id),
        }
        None
    }
}

async fn serve(stream: TcpStream, hub: Arc<Mutex<Hub>>) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else { return };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = unbounded_channel::<String>();
    let id = {
        let mut h = hub.lock().unwrap();
        if h.clients.len() >= MAX_CLIENTS {
            return;
        }
        h.next_id += 1;
        let id = h.next_id;
        h.clients.insert(id, Client { tx, name: format!("Racer {id}"), boat: String::new(), room: None });
        id
    };
    let writer = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if sink.send(Message::text(text)).await.is_err() {
                break;
            }
        }
    });
    let (mut window, mut count) = (Instant::now(), 0u32);
    while let Some(Ok(msg)) = source.next().await {
        let Message::Text(text) = msg else { continue };
        if window.elapsed() >= Duration::from_secs(1) {
            (window, count) = (Instant::now(), 0);
        }
        count += 1;
        if count > MAX_RATE || text.len() > 4096 {
            continue;
        }
        let Some(m) = decode::<ClientMsg>(&text) else { continue };
        let due = hub.lock().unwrap().handle(id, m);
        if let Some((secs, code, race, what)) = due {
            let hub = hub.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs_f32(secs)).await;
                hub.lock().unwrap().timer(&code, race, what);
            });
        }
    }
    {
        let mut h = hub.lock().unwrap();
        h.leave(id);
        h.clients.remove(&id);
    }
    writer.abort();
}

async fn run(listener: TcpListener) {
    let hub = Arc::new(Mutex::new(Hub::default()));
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            let _ = stream.set_nodelay(true);
            tokio::spawn(serve(stream, hub.clone()));
        }
    }
}

#[tokio::main]
async fn main() {
    let addr = std::env::var("RIPTIDE_RELAY_ADDR").unwrap_or_else(|_| "127.0.0.1:3020".into());
    let listener = TcpListener::bind(&addr).await.expect("bind");
    eprintln!("riptide-server: listening on {addr}");
    run(listener).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use riptide_net::BoatState;
    use std::time::Duration;

    type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

    async fn send(ws: &mut Ws, m: ClientMsg) {
        ws.send(Message::text(encode(&m))).await.unwrap();
    }

    /// Next message matching `want`, skipping others (e.g. room updates).
    async fn expect(ws: &mut Ws, want: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let Some(Ok(Message::Text(t))) = ws.next().await else { panic!("closed") };
                let m: ServerMsg = decode(&t).expect("bad message");
                if want(&m) {
                    return m;
                }
            }
        })
        .await
        .expect("timed out")
    }

    #[tokio::test]
    async fn two_players_race() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(run(listener));
        let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        send(&mut a, ClientMsg::Hello { version: VERSION, name: "Alice".into(), boat: "banshee".into() }).await;
        let ServerMsg::Welcome { id: ia } = expect(&mut a, |m| matches!(m, ServerMsg::Welcome { .. })).await else { unreachable!() };
        send(&mut b, ClientMsg::Hello { version: VERSION, name: "Bob".into(), boat: "tidal_blade".into() }).await;
        let ServerMsg::Welcome { id: ib } = expect(&mut b, |m| matches!(m, ServerMsg::Welcome { .. })).await else { unreachable!() };

        send(&mut a, ClientMsg::Create { track: "lost_island".into() }).await;
        let ServerMsg::Room { code, host, .. } = expect(&mut a, |m| matches!(m, ServerMsg::Room { .. })).await else { unreachable!() };
        assert_eq!(host, ia);
        assert_eq!(code.len(), 4);

        send(&mut b, ClientMsg::Join { code: code.to_lowercase() }).await;
        expect(&mut a, |m| matches!(m, ServerMsg::Room { players, .. } if players.len() == 2)).await;

        // Only the host can start.
        send(&mut b, ClientMsg::Start).await;
        send(&mut a, ClientMsg::Start).await;
        let ServerMsg::Start { track, players, .. } = expect(&mut b, |m| matches!(m, ServerMsg::Start { .. })).await else { unreachable!() };
        assert_eq!(track, "lost_island");
        assert_eq!(players.iter().map(|p| p.id).collect::<Vec<_>>(), vec![ia, ib]);

        // States are dropped until everyone has loaded; then Go.
        send(&mut a, ClientMsg::Ready).await;
        send(&mut b, ClientMsg::Ready).await;
        expect(&mut a, |m| matches!(m, ServerMsg::Go)).await;
        expect(&mut b, |m| matches!(m, ServerMsg::Go)).await;

        let st = BoatState { t: 1.0, pos: [10.0, 0.0, 5.0], lap: 1, ..Default::default() };
        send(&mut a, ClientMsg::State(st)).await;
        let got = expect(&mut b, |m| matches!(m, ServerMsg::State { .. })).await;
        assert_eq!(got, ServerMsg::State { id: ia, state: st });

        // A late joiner is turned away mid-race.
        let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        send(&mut c, ClientMsg::Hello { version: VERSION, name: "Cat".into(), boat: "banshee".into() }).await;
        send(&mut c, ClientMsg::Join { code: code.clone() }).await;
        expect(&mut c, |m| matches!(m, ServerMsg::Error { .. })).await;

        send(&mut b, ClientMsg::Finish { time: 90.5 }).await;
        send(&mut a, ClientMsg::Finish { time: 95.0 }).await;
        let res = expect(&mut a, |m| matches!(m, ServerMsg::Results { .. })).await;
        assert_eq!(res, ServerMsg::Results { order: vec![(ib, Some(90.5)), (ia, Some(95.0))] });

        // Wrong version is refused.
        send(&mut c, ClientMsg::Hello { version: VERSION + 1, name: "Old".into(), boat: String::new() }).await;
        expect(&mut c, |m| matches!(m, ServerMsg::Error { .. })).await;
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    fn hub_with(n: u32) -> (Hub, String, Vec<tokio::sync::mpsc::UnboundedReceiver<String>>) {
        let mut h = Hub::default();
        let mut rxs = Vec::new();
        for id in 1..=n {
            let (tx, rx) = unbounded_channel();
            h.clients.insert(id, Client { tx, name: format!("P{id}"), boat: String::new(), room: None });
            rxs.push(rx);
        }
        h.handle(1, ClientMsg::Create { track: "lost_island".into() });
        let code = h.clients[&1].room.clone().unwrap();
        for id in 2..=n {
            h.handle(id, ClientMsg::Join { code: code.clone() });
        }
        (h, code, rxs)
    }

    fn got(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>, want: impl Fn(&ServerMsg) -> bool) -> Option<ServerMsg> {
        while let Ok(t) = rx.try_recv() {
            let m: ServerMsg = decode(&t).unwrap();
            if want(&m) {
                return Some(m);
            }
        }
        None
    }

    #[test]
    fn a_player_who_never_loads_cannot_block_the_start() {
        let (mut h, code, mut rx) = hub_with(2);
        let (_, c, race, _) = h.handle(1, ClientMsg::Start).expect("ready timer");
        assert_eq!(c, code);
        h.handle(1, ClientMsg::Ready);
        assert!(got(&mut rx[0], |m| matches!(m, ServerMsg::Go)).is_none());
        h.timer(&code, race, Due::Go);
        assert!(got(&mut rx[0], |m| matches!(m, ServerMsg::Go)).is_some());
    }

    #[test]
    fn the_race_ends_after_the_grace_and_stale_timers_do_nothing() {
        let (mut h, code, mut rx) = hub_with(3);
        let (_, _, race, _) = h.handle(1, ClientMsg::Start).unwrap();
        for id in 1..=3 {
            h.handle(id, ClientMsg::Ready);
        }
        // A cheat-sized time is still a time; NaN and negative ones are refused.
        assert!(h.handle(2, ClientMsg::Finish { time: f32::NAN }).is_none());
        let grace = h.handle(2, ClientMsg::Finish { time: 80.0 }).expect("grace timer");
        assert!(h.handle(3, ClientMsg::Finish { time: 85.0 }).is_none());
        h.timer(&code, race + 1, Due::End); // an older/other race: ignored
        assert!(got(&mut rx[0], |m| matches!(m, ServerMsg::Results { .. })).is_none());
        h.timer(&grace.1, grace.2, Due::End);
        let res = got(&mut rx[0], |m| matches!(m, ServerMsg::Results { .. })).unwrap();
        assert_eq!(res, ServerMsg::Results { order: vec![(2, Some(80.0)), (3, Some(85.0)), (1, None)] });
    }

    #[test]
    fn host_leaving_hands_the_room_on() {
        let (mut h, code, mut rx) = hub_with(2);
        h.leave(1);
        assert_eq!(h.rooms[&code].host, 2);
        assert!(got(&mut rx[1], |m| matches!(m, ServerMsg::Room { host: 2, .. })).is_some());
        h.leave(2);
        assert!(h.rooms.is_empty());
    }
}
