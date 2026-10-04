//! Riptide's online race protocol: JSON messages over a WebSocket between the game and the
//! relay server (`riptide-server`). The server keeps rooms (a host and up to seven guests, each
//! with a boat), starts races with a shared countdown, relays each boat's state ~20 times a
//! second and collects finish times. Races are authoritative per boat: each game simulates its
//! own boat and draws the others from what they send.

use serde::{Deserialize, Serialize};

/// Bumped whenever a message changes shape: the server turns older games away.
pub const VERSION: u32 = 1;
/// Players per room.
pub const MAX_PLAYERS: usize = 8;
/// Seconds the server waits for every game to load the course before starting anyway.
pub const READY_TIMEOUT: f32 = 60.0;
/// Seconds after the first finisher before the race ends for everyone (the rest: did not finish).
pub const FINISH_GRACE: f32 = 60.0;
/// Most messages a game may send per second (states are ~20); extra ones are dropped.
pub const MAX_RATE: u32 = 60;

/// One boat's state, sent ~20 times a second while racing.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct BoatState {
    /// Race clock of the sender (seconds since GO).
    pub t: f32,
    pub pos: [f32; 3],
    /// Horizontal velocity (x, z) and vertical speed.
    pub vel: [f32; 2],
    pub vy: f32,
    pub yaw: f32,
    pub speed: f32,
    /// Steering -1..1 (the boat leans into turns).
    pub steer: f32,
    /// Seconds left of a wipeout (the boat spins).
    pub wipeout: f32,
    /// [`FLAG_BOOST`] | [`FLAG_SUPER`] | [`FLAG_CRUSH`] | [`FLAG_AIR`].
    pub flags: u8,
    /// Laps completed.
    pub lap: u32,
}

pub const FLAG_BOOST: u8 = 1;
pub const FLAG_SUPER: u8 = 2;
pub const FLAG_CRUSH: u8 = 4;
pub const FLAG_AIR: u8 = 8;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Player {
    pub id: u32,
    pub name: String,
    /// `boats` sheet id.
    pub boat: String,
}

/// Game -> server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello { version: u32, name: String, boat: String },
    /// Host a new room on `track` (`tracks` sheet id).
    Create { track: String },
    Join { code: String },
    /// Change boat (in the room, between races).
    Boat { boat: String },
    /// Host only: change the course (between races).
    Track { track: String },
    /// Host only: start the race.
    Start,
    /// The course is loaded and the boats are on the grid.
    Ready,
    State(BoatState),
    Finish { time: f32 },
    Leave,
}

/// Server -> game.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome { id: u32 },
    /// The room as it is now (sent on every change).
    Room { code: String, host: u32, track: String, players: Vec<Player>, racing: bool },
    /// Everyone: load `track` and reply [`ClientMsg::Ready`]; grid order = `players`.
    Start { track: String, players: Vec<Player> },
    /// Everyone has loaded (or the wait ran out): start the countdown.
    Go,
    State { id: u32, state: BoatState },
    Finished { id: u32, time: f32 },
    /// Everyone finished (or left): final order, best first; `None` = did not finish.
    Results { order: Vec<(u32, Option<f32>)> },
    Left { id: u32 },
    Error { message: String },
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    serde_json::to_string(msg).unwrap_or_default()
}

pub fn decode<'a, T: Deserialize<'a>>(text: &'a str) -> Option<T> {
    serde_json::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn messages_round_trip() {
        let m = ClientMsg::State(BoatState { t: 1.5, pos: [1.0, 2.0, 3.0], flags: FLAG_BOOST, ..Default::default() });
        assert_eq!(decode::<ClientMsg>(&encode(&m)), Some(m));
        let s = ServerMsg::Start { track: "wild_america".into(), players: vec![], };
        assert_eq!(decode::<ServerMsg>(&encode(&s)), Some(s));
    }
}
