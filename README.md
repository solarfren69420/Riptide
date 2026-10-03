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

## 💾 Game files (read this first!)

Riptide is only the engine. It ships with **no** game content, so you need your own copies of the
games. It reads exactly **two things**, and nothing else from either game:

1. **H2Overdrive's `triton.lux`** (required): one big archive file from the arcade game.
2. **Hydro Thunder's Dreamcast disc, as a GDI dump** (optional): adds the Hydro Thunder courses
   and boats.

Below is exactly what Riptide was built and tested with, so you can check yours matches.

### 1. H2Overdrive: `triton.lux` (required)

**What it is:** H2Overdrive is a 2009 arcade boat racer by Raw Thrills. The arcade cabinet runs
Windows, and the whole game (every model, texture, track, sound and setting) is packed into one
file called `triton.lux`. That single file is all Riptide needs.

**Where to find it:** in your H2Overdrive install folder (the folder TeknoParrot or the cabinet
runs the game from). It sits next to files like `sdaemon.exe`, `Settings.xml` and `Scores.xml`:

```text
H2Overdrive/            ← your install folder (any name)
├── sdaemon.exe         ← the game itself (Riptide doesn't need it)
├── Settings.xml
├── Scores.xml
├── movies/
└── triton.lux          ← ✅ this is the one Riptide needs
```

**The exact version Riptide was made with:**

| | |
|---|---|
| Game | **H2Overdrive** (Raw Thrills, 2009), PC-based arcade release |
| File | `triton.lux` |
| Size | **3,300,747,006 bytes** (about 3.1 GB) |
| File date | 6 November 2009 |
| Archive header | LUX version 3, **5,147 entries** |
| MD5 | `5f3f132361416a978e9370d48ab213ae` |
| SHA-1 | `3ed3768e05dde4e3399e63565c40bd026ae9d4c5` |

If your checksum is different, you probably have a different release or revision. It may still
work, but it isn't tested.

### 2. Hydro Thunder: Dreamcast disc as a GDI dump (optional)

**What it is:** Hydro Thunder (Midway, 1999) on the Sega Dreamcast. Dreamcast discs (called
GD-ROMs) are usually backed up as a **GDI dump**: one small text file named `something.gdi` that
lists the disc's tracks, plus one `.bin` file per track. Riptide reads the game's files straight
out of that dump; you don't need to extract anything.

**What the folder should look like:** all 38 files together in one folder:

```text
Hydro Thunder (USA)/
├── disc.gdi            ← ✅ the file you point Riptide at
├── track01.bin
├── track02.bin
├── track03.bin         ← the main game data (134 MB)
├── ...                 ← track04.bin to track36.bin (music and audio)
└── track37.bin         ← more game data (553 MB)
```

**The exact disc Riptide was made with:**

| | |
|---|---|
| Game | **Hydro Thunder**, Sega Dreamcast, **USA** release (Midway) |
| Product number | **T-9702N** |
| Version | **V1.020** (disc date 1999-10-04) |
| Format | GDI dump, **37 tracks**, raw 2352-byte sectors (the standard Redump-style layout) |
| Total size | 1,188,616,128 bytes of `.bin` files (about 1.2 GB) |
| SHA-1 `disc.gdi` | `7acc178f9695038b3fd8ba8daf0a11c880076d33` |
| SHA-1 `track03.bin` | `d8252cd17a048e8ab801a79b0101b9769414e7dd` (140,242,704 bytes) |
| SHA-1 `track37.bin` | `53b62d8ad8faa123e90f92f812b8f3df189f4425` (579,617,472 bytes) |

**Other formats:** `.cdi`, `.chd` and `.iso` images **don't work** as they are. If you have a
`.chd`, convert it to GDI first with MAME's `chdman` tool:

```sh
chdman extractcd -i "Hydro Thunder (USA).chd" -o disc.gdi
```

Without Hydro Thunder, Riptide still runs fine with just the H2Overdrive courses and boats.

### How to check a checksum

A checksum is a fingerprint of a file: if yours matches the table, you have the same file.

- **Windows** (Command Prompt): `certutil -hashfile triton.lux SHA1`
- **macOS** (Terminal): `shasum -a 1 triton.lux`
- **Linux** (terminal): `sha1sum triton.lux`

Hashing a 3 GB file takes a little while; that's normal.

### Telling Riptide where your files are

Riptide finds the files through two settings, `RIPTIDE_LUX` and `RIPTIDE_GDI`. Set them in the
terminal you start Riptide from (use your own paths):

**Linux / macOS:**

```sh
export RIPTIDE_LUX="$HOME/Games/H2Overdrive/triton.lux"
export RIPTIDE_GDI="$HOME/Games/Hydro Thunder (USA)/disc.gdi"
```

**Windows (PowerShell):**

```powershell
$env:RIPTIDE_LUX = "C:\Games\H2Overdrive\triton.lux"
$env:RIPTIDE_GDI = "C:\Games\Hydro Thunder (USA)\disc.gdi"
```

These last until you close the terminal. To check that Riptide can read both:

```sh
cargo run --release -p riptide-tool -- level wa                       # should print Wild America's contents
cargo run --release -p riptide-tool -- ht-track AMAZ.R2 HJTAMAZTRH0   # should print Lost Island's course
```

**In the browser version**, you don't set anything: the page asks you to pick `triton.lux`, and
optionally the `.gdi` together with all its `track*.bin` files. They're read straight from your
disk and never uploaded.

### Troubleshooting

- **"cannot load game data" / "not a LUX archive":** `RIPTIDE_LUX` doesn't point at the real
  `triton.lux`. Check the path, including quotes if it has spaces.
- **"Hydro Thunder disc not loaded":** check that `RIPTIDE_GDI` points at the `.gdi` file itself, and
  that all 37 `track*.bin` files are in the same folder with their original names.
- **Hydro Thunder courses missing from the menu:** same as above. The H2Overdrive courses still
  work without it.

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

Before creating the menu or course, the browser preloads meshes and their texture references.
Materials also retry textures that finish reading later. Nearby raw disc sectors share file reads.
The selection screen changes its preview and backdrop with the track: original select scenes
where available, and a scene rendered locally from the course elsewhere.

Hydro Thunder water covers the connected sector graph, including shortcuts. Waterfall curtains
are generated at portal drops; their appearance approximates the original effect.

```sh
scripts/build-web.sh
python3 -m http.server -d web 8080
```

Open `http://localhost:8080`. To prepare a public deployment, run `scripts/package-web.sh`
and publish the contents of `out/site/` over HTTPS. The package contains a gzip-compressed
engine that the browser decompresses automatically. Engine files use a build hash in their URLs
so a new release cannot silently reuse an older cached engine. The page uses relative paths,
so it can live at `/riptide/` on an existing site or on a static host such as GitHub Pages.
There is no backend or upload endpoint. Publish the built site; `web/pkg/` is ignored by Git.

The current engine build is approximately 73 MB uncompressed (about 14 MB downloaded in the package).
Enable caching on the host. Each player downloads the engine, then reads their own game files
locally. The host's storage does not grow with player game files. Browser play requires WebGPU;
desktop builds remain available for unsupported browsers.

### GitHub Pages

The built site is published on the repository's `gh-pages` branch. In GitHub, open
**Settings → Pages**, choose **Deploy from a branch**, and select **gh-pages / (root)**.
After GitHub's deployment finishes, the expected URL is
`https://solarfren69420.github.io/Riptide/`.

For later releases, rebuild with `scripts/build-web.sh`, run `scripts/package-web.sh`,
and commit the resulting site contents to `gh-pages`. Changes to `main` alone do not
update the deployed engine. Game archives and extracted assets must stay out of that branch.

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
