# Riptide

An open-source Rust (Bevy) boat racer that plays **H2Overdrive** and **Hydro Thunder** courses
from your own copies of those games. Includes Hackworld, an open-water sandbox built from the
games' own ramps and props.

**No game content is included.** Models, textures, sounds, levels and the data tables extracted
from them stay on your machine; Riptide reads them from the game files you own.

## Requirements

- Rust (see `rust-toolchain.toml`)
- H2Overdrive's `triton.lux`. Set `RIPTIDE_LUX=/path/to/triton.lux`.
- Optional: a Hydro Thunder (Dreamcast) GD-ROM dump for its courses and boats.

## Build

The game is generated from spreadsheets (`sheets/`): rows are objects, columns are properties,
and every cell is checked by a preflight before each compile. The sheets under `sheets/game/`
are extracted from your game files, so create them first:

```sh
cargo run --release -p riptide-tool -- seed-sheets sheets/game
cargo run --release -p riptide
```

The build stops with a preflight report (`target/sheets/preflight.txt`) if any sheet cell is
missing or wrong.

## Controls and cheats

Tab opens the cheats menu. F1–F7 and F9–F11 toggle cheats (infinite boost, gold boost, Hull
Crusher, camera zoom, no time limit, boat speed); F8 toggles all of them. Boost + brake jumps.
