# 🌊 Riptide

**A from-scratch Rust boat racer that plays _H2Overdrive_ and _Hydro Thunder_ — on your own copies of the games.**

Riptide reads the original games' files (models, textures, levels, sounds, handling data) straight
from the discs and archives you own, and races them in a new engine built on [Bevy](https://bevyengine.org).
No game content ships in this repository.

![status](https://img.shields.io/badge/status-playable%20alpha-orange)
![progress](https://img.shields.io/badge/progress-~71%25-blue)
![courses](https://img.shields.io/badge/courses-25%20playable-brightgreen)
![boats](https://img.shields.io/badge/boats-25-brightgreen)
![rust](https://img.shields.io/badge/rust-bevy%200.18-dea584)

---

## ✨ Highlights

- **25 playable courses**: every retail course of both games, plus **Hackworld**, an open-water sandbox
- **25 boats** with their original handling data, including H2Overdrive's three secret boats
- **The games' own feel**: boost, gold super-boost, **Hull Crusher**, boost jumps and ramp launches,
  all driven by the original tuning values
- **Original audio**: layered engine sounds, per-boat boosters, announcer, effects and per-track music
- **Original HUD** layout, fonts and dials, rebuilt from H2Overdrive's HUD data
- **Effects** from the games' own definitions: rocket flames, Hull Crusher lightning, wakes, rooster tails
- **Cheats menu**: infinite boost / gold / Hull Crusher, no wipeouts, no time limit, camera zoom, boat speed

## 📊 How far along is it?

**About 71% of the planned feature set** (14 of 21 items done, 2 partly done; partial counts as half). That's a count of the
checklist below, not a byte-level decompilation figure.

| Area | Status | Notes |
|---|---|---|
| H2Overdrive courses | ✅ 10 / 10 | Every retail course, including both backward variants |
| Hydro Thunder courses | ✅ 14 / 14 | Decoded from the Dreamcast disc: river corridors, scenery, boosts |
| Hackworld sandbox | ✅ | Open sea with the games' ramps, props and boosts |
| Boats | ✅ 25 | 12 H2Overdrive (3 secret) + 13 Hydro Thunder |
| Handling from the original boat data | ✅ | Speed, thrust, turning and grip per boat |
| Boost, gold boost, Hull Crusher, jumps | ✅ | Timings and heights from the games' tuning tables |
| AI racers | ✅ | Racing lines, lanes, obstacle avoidance, catch-up |
| Arcade checkpoint timer | ✅ 9 / 10 H2 courses | Hydro Thunder checkpoint times not decoded yet |
| Engine, effect and voice sound | ✅ | From the original sound banks |
| Music | 🟡 | H2Overdrive tracks ✅ · Hydro Thunder music not decoded (plays the H2O theme) |
| HUD | ✅ | Original layout, dials and fonts |
| Water, skies, lighting | ✅ | Animated water, per-level skyboxes and sun |
| Boost flames and Hull Crusher lightning | ✅ | Original definitions and art; sprite renderer approximates the original shader |
| Collision | 🟡 | Walls and ramps work; a few spots still trap boats |
| Cheats menu | ✅ | |
| Boat part animations (wings, flaps) | ✅ | Decoded anim4 clips, boost deployment, wing movement and upgrade attachments |
| Chaser boats | ❌ | AI-only boats not implemented |
| Career mode and upgrades | ❌ | |
| Hydro Thunder checkpoint times | ❌ | |
| Multiplayer | ❌ | |
| Full front-end menus | ❌ | Minimal boat and track select for now |

## 🚧 What still needs to be done

Pick anything here. Each item says what's missing and where to start.

**Gameplay**

1. **Collision robustness.** Walls and ramps work, and Wild America finishes under the autopilot,
   but some H2Overdrive courses still have spots that trap boats (Hong Kong Buoy after its 3rd
   checkpoint). Run `scripts/race-all.sh`, find the DNFs, and look at `Collider` in
   `crates/riptide/src/race.rs`. H2Overdrive's collision mesh (`coll4.wc_<level>`) has inconsistent
   triangle winding, and giant invisible gate quads that the original's level scripts remove.
2. **Animation fidelity.** The animation format is decoded and boat parts move. Compare each
   boat's deployment and stow poses against the original game, including upgrade levels and
   Hull Crusher transitions (`Anim Boost Partition Frame`, `Anim Wing Deploy` / `Stow`).
3. **Chaser boats.** H2Overdrive's AI-only chasers (Headhunter, Scotland Yard, Zodiac, Hong Kong
   Phooey) have models and boat definitions but no AI behaviour (rows marked `n/a` in `sheets/boats.csv`).
4. **Career mode and upgrades.** Experience, armour, engine and spoiler upgrades. The data is in
   `data.global_Experience` and the boats' `_Armor`, `_Engine_Upgrade` and `_Spoiler_Upgrade` meshes.
5. **Hydro Thunder checkpoint times.** Its courses have no arcade timer yet. The times are probably in the
   game's executable (`1ST_READ.BIN`).
6. **AI quality.** AI boats are slower than the arcade timer expects on some courses.

**Presentation**

7. **Hydro Thunder music.** It's in the Dreamcast's AICA sound banks, not decoded yet; its courses
   play the H2Overdrive theme meanwhile.
8. **Front-end menus.** Only a minimal boat and track select exists. The original menu layouts are in
   `data.wr_Boat` and related `CSHudCard` data.
9. **Hydro Thunder water.** The water ribbon spills over the banks where a river is narrower than
   its course portal.
10. **Hackworld polish.** Ramps render plain white, and floating props sit at sea level.
11. **Radar.** The minimap still needs work on Hydro Thunder courses and Hackworld.

**Verification**

12. **Unverified guesses.** `sheets/evidence.csv` lists every claim about the original games still
    marked _hypothesis_ (ramp launch speeds, flame and lightning parameters, collision barriers).
    Confirming or correcting them against the original game's code or behaviour is valuable.
13. **Multiplayer.** Split-screen or network play doesn't exist yet.

## 🛠️ What you need

- **Rust**: the toolchain is pinned in `rust-toolchain.toml`
- **A Vulkan-capable GPU and driver**
- **On Linux**, Bevy's build dependencies. On Debian/Ubuntu:
  `sudo apt install pkg-config libasound2-dev libudev-dev`
- **Your own copies of the games**, see below

## 💾 Game files

Riptide needs **one file from H2Overdrive** and, optionally, **one disc image of Hydro Thunder**.
Nothing else from either game is used.

### H2Overdrive: `triton.lux` (required)

| | |
|---|---|
| **Version** | The 2009 Raw Thrills **arcade** release (the PC-based cabinet) |
| **File** | `triton.lux`, about **3.1 GB**, in the game's install folder next to `Settings.xml` and `Scores.xml` |
| **Holds** | Every model, texture, level, collision mesh, sound bank and data table |
| **Tell Riptide** | `RIPTIDE_LUX=/path/to/triton.lux` |
| **Default** | `~/MEGA downloads/TRITON/TRITON/triton.lux` |

Only `triton.lux` is read. The game's executable, DLLs, movies and loaders aren't needed.

### Hydro Thunder: Dreamcast disc image (optional)

| | |
|---|---|
| **Version** | **Hydro Thunder (USA)**, Sega Dreamcast |
| **Format** | A **GDI dump**: `disc.gdi` plus its `track01.bin` … `track37.bin`, all in one folder, about **1.2 GB** in total (Redump-style raw 2352-byte sectors) |
| **Holds** | `HYDRODC.R2` (boats, textures, fonts) and one `.R2` archive per course, read straight out of the image |
| **Tell Riptide** | `RIPTIDE_GDI=/path/to/disc.gdi` |
| **Default** | `~/Games/Dreamcast/Hydro Thunder (USA)/disc.gdi` |

`.cdi`, `.chd` and `.iso` images aren't supported. Convert them to GDI first: for example,
`chdman extractcd` turns a CHD back into GDI plus BIN files. Without a Hydro Thunder image, Riptide
still runs, with H2Overdrive courses and boats only.

### Setting the paths

Export the variables in your shell, or put them in front of every command:

```sh
export RIPTIDE_LUX="$HOME/Games/H2Overdrive/triton.lux"
export RIPTIDE_GDI="$HOME/Games/Dreamcast/Hydro Thunder (USA)/disc.gdi"
```

To check that both are found:

```sh
cargo run --release -p riptide-tool -- level wa       # prints Wild America's contents from triton.lux
cargo run --release -p riptide-tool -- ht-track AMAZ.R2 HJTAMAZTRH0   # reads Lost Island from the disc image
```

## 🚀 Build and run

Riptide is generated from spreadsheets (see below). The sheets in `sheets/game/` are extracted from
_your_ game files, so create them first:

```sh
# 1. Extract the game data tables from your triton.lux
cargo run --release -p riptide-tool -- seed-sheets sheets/game

# 2. Build and play
cargo run --release -p riptide
```

If any sheet cell is missing or wrong, the build stops before compiling and writes a report to
`target/sheets/preflight.txt`.

## 🌐 Browser build and hosting

The browser version runs the engine on the player's computer. They choose their local
`triton.lux` and optionally the Hydro Thunder `.gdi` and its track files. The browser reads
only the byte ranges needed; game files are never uploaded or stored on the host.
Selected ranges are cached in the player's browser memory during play.

```sh
scripts/build-web.sh
python3 -m http.server -d web 8080
```

Open `http://localhost:8080`. For public hosting, publish `web/index.html` and `web/pkg/`
over HTTPS with the `.wasm` MIME type `application/wasm`. The page uses relative paths,
so it can live at `/riptide/` on an existing site or on a static host such as GitHub Pages.
There is no backend or upload endpoint. Publish the built site; `web/pkg/` is ignored by Git.

The current engine build is approximately 73 MB before HTTP compression (about 14 MB with gzip).
Enable compression and caching on the host. Each player downloads the engine, then reads their own game files
locally. The host's storage does not grow with player game files. Browser play requires WebGPU;
desktop builds remain available for unsupported browsers.

## 🎮 Controls

| Action | Keyboard | Pad |
|---|---|---|
| Throttle | ↑ / W | Right trigger / A |
| Brake / reverse | ↓ / S | Left trigger |
| Steer | ← → / A D | D-pad |
| Boost | Space / Shift | B / right bumper |
| **Jump** | Boost + tap ↓ (tap again in mid-air for a double jump) | |
| Restart / leave race | R / Esc | Select |
| Cheats menu | Tab | |

**Cheat hotkeys:**

| Key | Cheat |
|---|---|
| F1 | Infinite boost |
| F2 | Infinite gold boost |
| F3 | Infinite Hull Crusher |
| F4 | No wipeouts |
| F5 | No AI catch-up |
| F6 | Sturdy (no crash penalties) |
| F7 | Unlock the secret boats |
| F8 | All cheats on/off |
| F9 | Camera zoom (cycles) |
| F10 | No time limit |
| F11 | Boat speed (cycles ×0.5 to ×3) |

## 📋 Spreadsheet-driven

Everything the game knows lives in CSV sheets under `sheets/`. Rows are objects (boats, tracks,
cheats, HUD cards), columns are properties, and every cell is checked before each build.

- **The sheets are the source of truth.** Rust code is generated from them, one struct per row.
- **Preflight** checks every row × column: types, references between sheets, missing values, and
  game assets that must exist. A failure stops the build with a report.
- **Evidence ledger** (`sheets/evidence.csv`): every claim about how the original games work is
  marked _observed_, _hypothesis_ or _design_, so guesses can't pass for facts.

## 🧰 Repository layout

| Path | What's there |
|---|---|
| `crates/riptide` | The game |
| `crates/riptide-assets` | Readers for the original formats: `triton.lux` meshes, textures, collision, FSB4 sound banks, Hydro Thunder R2 archives and tracks |
| `crates/riptide-sheets` | Sheet loader, preflight and code generator |
| `crates/riptide-tool` | Command-line tools: extract tables, dump assets, inspect tracks, generate Hackworld |
| `sheets/` | The hand-written sheets |
| `scripts/` | Test helpers (headless race runs) |

## 🤖 One-shot prompt for your AI agent

Using an AI coding agent (Claude Code or similar)? Clone the repo, open the agent in it, and paste:

````text
You're working on Riptide, a Rust (Bevy 0.18) boat racer that plays H2Overdrive and Hydro Thunder
courses from the user's own game files. Read README.md first.

Setup:
1. My H2Overdrive triton.lux is at: <PATH>   (export RIPTIDE_LUX=<PATH>)
   My Hydro Thunder disc.gdi is at: <PATH or "none">   (export RIPTIDE_GDI=<PATH>)
2. Run: cargo run --release -p riptide-tool -- seed-sheets sheets/game
3. Run: cargo build --release -p riptide   and fix nothing yet; just confirm it builds.

Rules of this codebase:
- The CSV sheets in sheets/ are the source of truth. Rows = objects, columns = properties.
  Change behaviour in the sheets first; code is generated from them (crates/riptide-sheets).
- Every build runs a preflight over every row x column. Never bypass it; fix the sheet it names
  (report: target/sheets/preflight.txt).
- Invent nothing about the original games. Every value comes from the game data, or is marked
  "riptide design" in its sheet's source column. Claims about how the originals work go into
  sheets/evidence.csv as observed / hypothesis / design.
- Never commit sheets/game/ or any game asset. It's extracted from the user's own copy.

Testing (headless, no GPU needed):
  VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json RIPTIDE_TRACK=wa RIPTIDE_SIM_DT=0.05 \
  RIPTIDE_SHOT=out/shot RIPTIDE_SHOT_FRAMES=200,400 RIPTIDE_SHOT_SIZE=960x540 RIPTIDE_DEBUG=1 \
  target/release/riptide
- The player boat is autopiloted; screenshots land at out/shot_<frame>.png; RIPTIDE_DEBUG
  logs position, speed, wall contacts and checkpoints.
- RIPTIDE_TRACK takes a track id from sheets/tracks.csv, an H2 level code or an HT file stem;
  RIPTIDE_BOAT picks a boat by name. RIPTIDE_EXIT_ON_FINISH=1 quits at the finish.
- RIPTIDE_TEST_LANE=0..1 holds a lane; RIPTIDE_TEST_BOOST / _JUMP / _BRAKE=<seconds> trigger
  those inputs at that time.
- scripts/race-all.sh races every playable course and reports FINISHED or DNF.

Task: pick item <N> from "What still needs to be done" in README.md. Start by reproducing the
problem headlessly, explain what you find, then fix it. Prove the fix with a before/after test run.
````

## ⚖️ Legal

Riptide is an independent, fan-made engine reimplementation, not affiliated with or endorsed by the
original publishers or developers. It contains **no** game assets, data or code from _H2Overdrive_
or _Hydro Thunder_. You need your own legally obtained copies of the games to play.
