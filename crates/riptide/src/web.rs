//! Browser build. The page hands over the player's own game files (`triton.lux`, optionally the
//! Hydro Thunder `.gdi` and its track files); nothing is uploaded anywhere. The archives read
//! them through sparse sources: a read of bytes not fetched yet is queued, the bytes are sliced
//! out of the player's file, and the read is retried.

use crate::content::{Content, CourseSource};
use crate::{Screen, Selection};
use crate::sheets::{BOATS, H2_UPGRADES, TRACK_PREVIEWS};
use anyhow::{anyhow, Result};
use bevy::prelude::*;
use riptide_assets::gdi::GdRom;
use riptide_assets::ht::HydroThunder;
use riptide_assets::lux::LuxArchive;
use riptide_assets::source::{Source, SparseSource};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};

/// Per course, the `triton.lux` entries a race reads (`<track id> <entry>` lines, from
/// `scripts/web-manifests.sh`); `boats` lists every boat's.
const MANIFESTS: &str = include_str!("../../../web/manifests.txt");

/// A sparse source and the player's file it is filled from.
struct Backed {
    file: web_sys::File,
    src: Arc<SparseSource>,
}

thread_local! {
    static FILES: RefCell<Vec<Backed>> = const { RefCell::new(Vec::new()) };
}

static LOADED: AtomicBool = AtomicBool::new(false);
static FETCHING: AtomicBool = AtomicBool::new(false);

fn back(file: web_sys::File) -> Arc<SparseSource> {
    let src = Arc::new(SparseSource::new(file.size() as u64));
    FILES.with(|f| f.borrow_mut().push(Backed { file, src: src.clone() }));
    src
}

async fn slice(file: &web_sys::File, a: u64, b: u64) -> Result<Vec<u8>, JsValue> {
    let blob = file.slice_with_f64_and_f64(a as f64, b as f64)?;
    let buf = JsFuture::from(blob.array_buffer()).await?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Fetch every queued range. Whether anything was queued.
async fn fetch_misses() -> bool {
    let jobs: Vec<(web_sys::File, Arc<SparseSource>, Vec<(u64, u64)>)> = FILES.with(|f| {
        f.borrow()
            .iter()
            .map(|b| (b.file.clone(), b.src.clone(), b.src.take_misses()))
            .filter(|j| !j.2.is_empty())
            .collect()
    });
    let any = !jobs.is_empty();
    for (file, src, ranges) in jobs {
        for (a, b) in ranges {
            match slice(&file, a, b).await {
                Ok(d) => src.insert(a, d),
                Err(e) => warn!("reading {} [{a}..{b}]: {e:?}", file.name()),
            }
        }
    }
    any
}

fn any_pending() -> bool {
    FILES.with(|f| f.borrow().iter().any(|b| b.src.pending()))
}

/// Run `f` until it no longer fails for want of unfetched bytes.
async fn fill<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    loop {
        match f() {
            Ok(t) if !any_pending() => return Ok(t),
            Ok(_) => {
                fetch_misses().await;
            }
            Err(e) => {
                if !fetch_misses().await {
                    return Err(e);
                }
            }
        }
    }
}

/// Start the game over the player's files: `lux` is `triton.lux`; `disc` holds the Hydro Thunder
/// `.gdi` and its track files (may be empty).
#[wasm_bindgen(js_name = startGame)]
pub async fn start(lux: web_sys::File, disc: js_sys::Array) -> Result<(), JsValue> {
    let fail = |e: anyhow::Error| JsValue::from_str(&format!("{e:#}"));
    let lux_src = back(lux);
    let archive = fill(|| LuxArchive::from_source(Box::new(lux_src.clone()))).await.map_err(fail)?;

    // Menu entities are constructed once: load their models AND texture dependencies first.
    // Otherwise a successful mesh read permanently caches materials with grey fallback colours.
    let mut startup: Vec<String> = manifest("boats").map(str::to_string).collect();
    startup.extend(TRACK_PREVIEWS.iter().filter_map(|row| row.image.strip_prefix("lux:")).map(str::to_string));
    for row in BOATS {
        startup.extend([row.model, row.anim].into_iter().filter_map(|n| n.strip_prefix("lux:")).map(str::to_string));
    }
    for row in H2_UPGRADES {
        startup.extend([row.mesh, row.anim].into_iter().filter_map(|n| n.strip_prefix("lux:")).map(str::to_string));
    }
    fill(|| { archive.prefetch(&startup); Ok(()) }).await.map_err(fail)?;

    let files: Vec<web_sys::File> = disc.iter().filter_map(|v| v.dyn_into().ok()).collect();
    let ht = match files.iter().find(|f| f.name().to_ascii_lowercase().ends_with(".gdi")) {
        Some(gdi) => {
            let text = String::from_utf8_lossy(&slice(gdi, 0, gdi.size() as u64).await?).into_owned();
            let mut tracks = Vec::new();
            for (name, _, _) in GdRom::gdi_tracks(&text) {
                let file = files.iter().find(|f| f.name().eq_ignore_ascii_case(&name));
                let file = file.ok_or_else(|| JsValue::from_str(&format!("{name} (named in the .gdi) wasn't picked")))?;
                tracks.push((name, back(file.clone())));
            }
            let open = || {
                let disc = GdRom::from_parts(&text, |name| {
                    let src = tracks.iter().find(|(n, _)| n == name).map(|(_, s)| s.clone());
                    Ok(Box::new(src.ok_or_else(|| anyhow!("{name} missing"))?) as Box<dyn Source>)
                })?;
                HydroThunder::from_disc(disc)
            };
            match fill(open).await {
                Ok(h) => Some(Arc::new(h)),
                Err(e) => {
                    warn!("Hydro Thunder disc not loaded: {e:#}");
                    None
                }
            }
        }
        None => None,
    };
    crate::run(Content::from_archives(Arc::new(archive), ht));
    Ok(())
}

/// Loading screen and the background fetcher.
pub struct WebPlugin;

impl Plugin for WebPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(Screen::Loading), begin_loading)
            .add_systems(Update, (finish_loading.run_if(in_state(Screen::Loading)), fetch_late));
    }
}

fn manifest(id: &str) -> impl Iterator<Item = &'static str> + '_ {
    MANIFESTS.lines().filter_map(move |l| l.strip_prefix(id)?.strip_prefix(' '))
}

/// Fetch the chosen course (and every boat) before the race starts.
fn begin_loading(mut commands: Commands, sel: Res<Selection>, content: Res<Content>) {
    LOADED.store(false, Ordering::SeqCst);
    commands.spawn((
        Text::new("Loading course from your game files…"),
        TextFont { font_size: 32.0, ..default() },
        Node { position_type: PositionType::Absolute, left: percent(5), bottom: percent(8), ..default() },
        DespawnOnExit(Screen::Loading),
    ));
    let Some(choice) = content.tracks.get(sel.level).cloned() else { return };
    let (lux, ht) = (content.lux.clone(), content.ht.clone());
    let names: Vec<&'static str> = manifest(choice.id).chain(manifest("boats")).collect();
    spawn_local(async move {
        let touch = || {
            lux.prefetch(&names);
            if let (CourseSource::Ht { file, entry, .. }, Some(ht)) = (&choice.source, &ht) {
                ht.has_track(file, entry);
            }
            Ok(())
        };
        if let Err(e) = fill(touch).await {
            warn!("loading {}: {e:#}", choice.id);
        }
        LOADED.store(true, Ordering::SeqCst);
    });
}

fn finish_loading(mut next: ResMut<NextState<Screen>>) {
    if LOADED.load(Ordering::SeqCst) {
        next.set(Screen::Race);
    }
}

/// Anything read mid-race that the lists missed (a rare sound, a boat preview) is fetched in
/// the background; it shows up the next time it's asked for.
fn fetch_late() {
    if any_pending() && !FETCHING.swap(true, Ordering::SeqCst) {
        spawn_local(async {
            while fetch_misses().await {}
            FETCHING.store(false, Ordering::SeqCst);
        });
    }
}
