slint::include_modules!();

mod backend;
mod detail;
mod image_util;
mod state;

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use backend::Backend;
use detail::{DetailState, filename_of, format_metadata, select};
use imgfind::filters::{Filters, GpsFilter};
use state::{SearchResult, SearchState, ViewState};

/// Whether the current tile grid was populated by a text search or a
/// vector-similarity search.  Stored in an `Arc<Mutex<_>>` so both the
/// `on_search` / `on_search_similar` closures (which set it) and
/// `on_load_more` (which reads it) share the same value across threads.
#[derive(Clone)]
enum SearchMode {
    Text(String),
    Similar(String),
}

#[derive(Parser)]
#[command(name = "imgfind-gui", about = "CLIP image search — native GUI")]
struct Args {
    /// Directory to search for an imgfind database (walks up from here).
    #[arg(long)]
    dir: Option<String>,
}

/// Map a [0, 1] slider fraction to bytes, treating extremes as unbounded.
///
/// When `fraction` is exactly 0.0 the lower side is unbounded (`None`);
/// when it is exactly 1.0 the upper side is unbounded (`None`).  This
/// ensures that a fully-reset slider shows all images rather than
/// filtering to a zero-width range.
fn fraction_to_bytes(fraction: f32, min: i64, max: i64, is_lo: bool) -> Option<i64> {
    if is_lo && fraction <= 0.0 {
        return None;
    }
    if !is_lo && fraction >= 1.0 {
        return None;
    }
    let bytes = min as f64 + fraction as f64 * (max - min) as f64;
    Some(bytes.round() as i64)
}

/// Format `bytes` as a human-readable size string, e.g. "2.3 MB".
fn format_bytes(bytes: i64) -> String {
    const MB: f64 = 1_048_576.0;
    const KB: f64 = 1_024.0;
    if bytes >= MB as i64 {
        format!("{:.1} MB", bytes as f64 / MB)
    } else if bytes >= KB as i64 {
        format!("{:.0} KB", bytes as f64 / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Build a `Filters` from current UI slider/chip/GPS state.
fn build_filters(
    lo: f32,
    hi: f32,
    size_bounds: (i64, i64),
    selected_exts: &HashSet<String>,
    gps_mode: i32,
) -> Filters {
    let (min, max) = size_bounds;
    let size_min = fraction_to_bytes(lo, min, max, true);
    let size_max = fraction_to_bytes(hi, min, max, false);
    let extensions: Vec<String> = {
        let mut v: Vec<String> = selected_exts.iter().cloned().collect();
        v.sort();
        v
    };
    let gps = match gps_mode {
        1 => GpsFilter::HasGps,
        2 => GpsFilter::NoGps,
        _ => GpsFilter::Any,
    };
    Filters { size_min, size_max, extensions, gps }
}

/// Rebuild the `type-chips` model from the available extensions, marking
/// those present in `selected` as `on: true`.
fn build_chips_model(
    all_exts: &[String],
    selected: &HashSet<String>,
) -> ModelRc<(SharedString, bool)> {
    let chips: Vec<(SharedString, bool)> = all_exts
        .iter()
        .map(|ext| (SharedString::from(ext.as_str()), selected.contains(ext)))
        .collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(chips)))
}

/// Build the human-readable size-label string from lo/hi fractions.
fn build_size_label(lo: f32, hi: f32, size_bounds: (i64, i64)) -> SharedString {
    let (min, max) = size_bounds;
    let lo_bytes = fraction_to_bytes(lo, min, max, true);
    let hi_bytes = fraction_to_bytes(hi, min, max, false);
    match (lo_bytes, hi_bytes) {
        (None, None) => "Size: all".into(),
        (Some(lo_b), None) => format!("{} – ∞", format_bytes(lo_b)).into(),
        (None, Some(hi_b)) => format!("0 – {}", format_bytes(hi_b)).into(),
        (Some(lo_b), Some(hi_b)) => {
            format!("{} – {}", format_bytes(lo_b), format_bytes(hi_b)).into()
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let args = Args::parse();
    let backend = Backend::open(args.dir.as_deref()).context("Failed to open imgfind database")?;
    backend.start_loading_model();

    // Fetch size bounds once at startup for the [0,1]↔bytes mapping.
    let size_bounds = backend.size_bounds().unwrap_or((0, 0));

    // All extensions known to the DB, used to drive the type-chips model.
    let all_extensions: Vec<String> = backend.extensions().unwrap_or_default();

    let window = MainWindow::new().context("Failed to create window")?;

    // Initial UI state: model loading, search disabled.
    window.set_can_search(false);
    window.set_status("Loading model...".into());
    window.set_show_load_more(false);
    window.set_tiles(ModelRc::default());
    window.set_lightbox_open(false);
    window.set_detail_open(false);
    // Filter-bar initial state.
    window.set_size_lo(0.0);
    window.set_size_hi(1.0);
    window.set_size_label("Size: all".into());
    window.set_type_chips(build_chips_model(&all_extensions, &HashSet::new()));
    window.set_gps_mode(0);

    // State shared with background threads via Arc<Mutex<_>>.
    let state: Arc<Mutex<SearchState>> = Arc::new(Mutex::new(SearchState::new()));

    // Current lightbox index. None when the lightbox is closed.
    let lb_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

    // Currently selected detail image. None when the panel is closed.
    let detail: Arc<Mutex<Option<DetailState>>> = Arc::new(Mutex::new(None));

    // Tracks whether the tile grid was populated by a text or similarity search,
    // so `on_load_more` knows which backend method to call.
    let search_mode: Arc<Mutex<SearchMode>> = Arc::new(Mutex::new(SearchMode::Text(String::new())));

    // Live filter state, shared with every query closure.
    let filters: Arc<Mutex<Filters>> = Arc::new(Mutex::new(Filters::default()));

    // Currently-selected extension set (drives chip `on` state + Filters.extensions).
    let selected_exts: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    // Poll for model readiness every 250 ms.  The timer performs exactly ONE
    // job: detect the loading→ready transition, enable the search box, and
    // stop itself.  After that point, search-start (false) and
    // search-completion (true) are the sole owners of `can_search`.
    let model_ready_handled = Arc::new(AtomicBool::new(false));
    let model_timer = Timer::default();
    {
        let weak = window.as_weak();
        let backend_poll = backend.clone();
        let handled = Arc::clone(&model_ready_handled);
        model_timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            if handled.load(Ordering::Relaxed) {
                return;
            }
            if backend_poll.model_ready() {
                if let Some(w) = weak.upgrade() {
                    w.set_can_search(true);
                    w.set_status("Enter a search query to find images.".into());
                }
                // Mark handled *before* stopping so a hypothetical reentrant
                // tick that fires during stop() is also a no-op.
                handled.store(true, Ordering::Relaxed);
            }
        });
    }

    // --- search callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let detail_ref = Arc::clone(&detail);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let backend_search = backend.clone();
        window.on_search(move |query| {
            let query = query.trim().to_string();
            let current_filters = filters_ref.lock().unwrap().clone();

            if query.is_empty() {
                if current_filters == Filters::default() {
                    // No query, no active filters — reset to idle.
                    *state_ref.lock().unwrap() = SearchState::new();
                    *detail_ref.lock().unwrap() = None;
                    *lb_ref.lock().unwrap() = None;
                    *mode_ref.lock().unwrap() = SearchMode::Text(String::new());
                    if let Some(w) = weak.upgrade() {
                        w.set_status("Enter a search query to find images.".into());
                        w.set_show_load_more(false);
                        w.set_tiles(ModelRc::default());
                        w.set_detail_open(false);
                        w.set_lightbox_open(false);
                    }
                } else {
                    // Empty query but filters active — browse with filters.
                    *lb_ref.lock().unwrap() = None;
                    *detail_ref.lock().unwrap() = None;
                    *mode_ref.lock().unwrap() = SearchMode::Text(String::new());
                    state_ref.lock().unwrap().start_search(String::new());
                    if let Some(w) = weak.upgrade() {
                        w.set_lightbox_open(false);
                        w.set_detail_open(false);
                        w.set_status("Searching\u{2026}".into());
                        w.set_show_load_more(false);
                        w.set_can_search(false);
                    }
                    spawn_browse(
                        weak.clone(),
                        Arc::clone(&state_ref),
                        backend_search.clone(),
                        current_filters,
                        0,
                    );
                }
                return;
            }

            // Dismiss any open lightbox and detail panel before showing new
            // results — stored indices would otherwise be stale.
            *lb_ref.lock().unwrap() = None;
            *detail_ref.lock().unwrap() = None;
            *mode_ref.lock().unwrap() = SearchMode::Text(query.clone());
            if let Some(w) = weak.upgrade() {
                w.set_lightbox_open(false);
                w.set_detail_open(false);
            }

            state_ref.lock().unwrap().start_search(query.clone());
            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_show_load_more(false);
                w.set_can_search(false);
            }

            spawn_search(
                weak.clone(),
                Arc::clone(&state_ref),
                backend_search.clone(),
                query,
                0,
                current_filters,
            );
        });
    }

    // --- load-more callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let backend_more = backend.clone();
        window.on_load_more(move || {
            let offset = state_ref.lock().unwrap().next_offset();
            let mode = mode_ref.lock().unwrap().clone();
            let current_filters = filters_ref.lock().unwrap().clone();

            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_show_load_more(false);
                w.set_can_search(false);
            }

            match mode {
                SearchMode::Text(query) if !query.is_empty() => {
                    spawn_search(
                        weak.clone(),
                        Arc::clone(&state_ref),
                        backend_more.clone(),
                        query,
                        offset,
                        current_filters,
                    );
                }
                SearchMode::Similar(seed) if !seed.is_empty() => {
                    spawn_similar(
                        weak.clone(),
                        Arc::clone(&state_ref),
                        backend_more.clone(),
                        seed,
                        offset,
                        current_filters,
                    );
                }
                // Text("") means we're in browse mode.
                SearchMode::Text(_) => {
                    spawn_browse(
                        weak.clone(),
                        Arc::clone(&state_ref),
                        backend_more.clone(),
                        current_filters,
                        offset,
                    );
                }
                _ => {
                    // No active search — restore UI to idle.
                    if let Some(w) = weak.upgrade() {
                        w.set_can_search(true);
                        w.set_show_load_more(false);
                    }
                }
            }
        });
    }

    // --- tile-selected callback: show detail panel for the clicked tile ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let detail_ref = Arc::clone(&detail);
        let backend_detail = backend.clone();
        window.on_tile_selected(move |index| {
            let idx = index as usize;
            let path = {
                let s = state_ref.lock().unwrap();
                s.results.get(idx).map(|r| r.path.clone())
            };
            let Some(path) = path else { return };

            // Compute detail identity synchronously (just string ops).
            let ds = select(path.clone());
            *detail_ref.lock().unwrap() = Some(ds.clone());

            // Open the panel immediately with a placeholder while the worker loads.
            if let Some(w) = weak.upgrade() {
                w.set_detail_open(true);
                w.set_detail_filename(ds.filename.into());
                w.set_detail_image(Default::default());
                w.set_detail_meta("Loading\u{2026}".into());
            }

            // Load thumbnail + metadata on a background thread.
            let weak2 = weak.clone();
            let backend2 = backend_detail.clone();
            std::thread::spawn(move || {
                let meta_result = backend2.metadata(&path);
                let thumb_result = backend2.thumbnail(&path, 512);

                slint::invoke_from_event_loop(move || {
                    let Some(w) = weak2.upgrade() else { return };
                    match thumb_result {
                        Ok(bytes) => match image_util::jpeg_to_slint_image(&bytes) {
                            Ok(img) => w.set_detail_image(img),
                            Err(e) => tracing::warn!("detail thumb decode failed: {e}"),
                        },
                        Err(e) => tracing::warn!("detail thumbnail failed: {e}"),
                    }
                    match meta_result {
                        Ok(meta) => w.set_detail_meta(format_metadata(&meta).into()),
                        Err(e) => {
                            tracing::warn!("detail metadata failed: {e}");
                            w.set_detail_meta("".into());
                        }
                    }
                })
                .ok();
            });
        });
    }

    // --- tile-activated callback: open lightbox at the activated index ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let backend_lb = backend.clone();
        window.on_tile_activated(move |index| {
            let idx = index as usize;
            let path = state_ref
                .lock()
                .unwrap()
                .results
                .get(idx)
                .map(|r| r.path.clone());
            let Some(rel) = path else { return };
            *lb_ref.lock().unwrap() = Some(idx);
            load_lightbox_image(weak.clone(), backend_lb.clone(), rel);
        });
    }

    // --- tile-open-external callback ---
    {
        let backend_open = backend.clone();
        let state_ref = Arc::clone(&state);
        window.on_tile_open_external(move |index| {
            let path = {
                let s = state_ref.lock().unwrap();
                s.results.get(index as usize).map(|r| r.path.clone())
            };
            if let Some(rel) = path {
                let abs = backend_open.abs_path(&rel);
                if let Err(e) = open::that(&abs) {
                    tracing::warn!("Failed to open {abs:?}: {e}");
                }
            }
        });
    }

    // --- detail-close callback ---
    {
        let weak = window.as_weak();
        let detail_ref = Arc::clone(&detail);
        window.on_detail_close(move || {
            *detail_ref.lock().unwrap() = None;
            if let Some(w) = weak.upgrade() {
                w.set_detail_open(false);
            }
        });
    }

    // --- detail-view-full callback: open the lightbox showing the seed image ---
    //
    // Uses the seed's path directly from the `detail` holder rather than a
    // grid index, so it stays valid after `search-similar` replaces the tile
    // grid while the panel remains open on the seed.
    {
        let weak = window.as_weak();
        let detail_ref = Arc::clone(&detail);
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let backend_vf = backend.clone();
        window.on_detail_view_full(move || {
            let seed_path = {
                let d = detail_ref.lock().unwrap();
                d.as_ref().map(|ds| ds.path.clone())
            };
            let Some(rel) = seed_path else { return };

            // Resolve the seed's position in the current grid so that lightbox
            // prev/next navigation starts from the correct slot.  If the seed
            // is absent from the current results (e.g. after a search-similar
            // that filtered the seed out of its own result set), lb_index is
            // None and prev/next will start from slot 0 — that is intentional.
            let idx = {
                let st = state_ref.lock().unwrap();
                st.results.iter().position(|r| r.path == rel)
            };
            *lb_ref.lock().unwrap() = idx;

            load_lightbox_image(weak.clone(), backend_vf.clone(), rel);
        });
    }

    // --- search-similar callback ---
    //
    // Replaces the tile grid with images similar to the currently open seed,
    // while keeping the detail panel open on the seed.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let detail_ref = Arc::clone(&detail);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let backend_sim = backend.clone();
        window.on_search_similar(move || {
            let seed_path = {
                let d = detail_ref.lock().unwrap();
                d.as_ref().map(|ds| ds.path.clone())
            };
            let Some(seed_path) = seed_path else { return };

            let filename = filename_of(&seed_path);
            let current_filters = filters_ref.lock().unwrap().clone();
            *mode_ref.lock().unwrap() = SearchMode::Similar(seed_path.clone());

            // `start_search` records the seed path as the "committed query" so
            // `apply_page` / `apply_error` work correctly for offset tracking.
            // NOTE: `committed_query` holds a file path here, NOT a text query.
            // load-more reads `search_mode` (not this field) to dispatch the
            // next page, and `committed_query` is never displayed — do NOT rely
            // on it being a real text query during a similarity search.
            state_ref.lock().unwrap().start_search(seed_path.clone());
            if let Some(w) = weak.upgrade() {
                w.set_status(format!("Similar to {filename}").into());
                w.set_show_load_more(false);
                w.set_can_search(false);
                // detail-open stays TRUE — panel remains on the seed.
            }

            spawn_similar(
                weak.clone(),
                Arc::clone(&state_ref),
                backend_sim.clone(),
                seed_path,
                0,
                current_filters,
            );
        });
    }

    // --- lightbox-close callback ---
    {
        let weak = window.as_weak();
        let lb_ref = Arc::clone(&lb_index);
        window.on_lightbox_close(move || {
            *lb_ref.lock().unwrap() = None;
            if let Some(w) = weak.upgrade() {
                w.set_lightbox_open(false);
            }
        });
    }

    // --- lightbox-prev callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let backend_lb = backend.clone();
        window.on_lightbox_prev(move || {
            let new_idx = {
                let mut guard = lb_ref.lock().unwrap();
                let current = guard.unwrap_or(0);
                let next = clamp_prev(current);
                *guard = Some(next);
                next
            };
            let path = state_ref
                .lock()
                .unwrap()
                .results
                .get(new_idx)
                .map(|r| r.path.clone());
            if let Some(rel) = path {
                load_lightbox_image(weak.clone(), backend_lb.clone(), rel);
            }
        });
    }

    // --- lightbox-next callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let backend_lb = backend.clone();
        window.on_lightbox_next(move || {
            let (new_idx, len) = {
                let s = state_ref.lock().unwrap();
                let len = s.results.len();
                let mut guard = lb_ref.lock().unwrap();
                let current = guard.unwrap_or(0);
                let next = clamp_next(current, len);
                *guard = Some(next);
                (next, len)
            };
            if len == 0 {
                return;
            }
            let path = state_ref
                .lock()
                .unwrap()
                .results
                .get(new_idx)
                .map(|r| r.path.clone());
            if let Some(rel) = path {
                load_lightbox_image(weak.clone(), backend_lb.clone(), rel);
            }
        });
    }

    // --- filter-bar debounce timer ---
    //
    // A single stable `FnMut` timer callback is registered once; the three
    // filter-change callbacks below restart it on every change so rapid
    // events (e.g. slider dragging) coalesce into one query.  The callback
    // reads all relevant state from Arcs at fire time, so it always uses
    // the *latest* values regardless of which event triggered the restart.
    // The debounce timer is shared by all three filter callbacks below.
    // Each callback calls `start_debounce(&timer, …)` which replaces the
    // pending timer fire with a new 250 ms single-shot containing the
    // latest filter snapshot.
    // Timer is !Send+!Sync, so Rc (not Arc) is correct — all callbacks run
    // on the Slint UI thread.
    let debounce_timer: Rc<Timer> = Rc::new(Timer::default());

    // `on_filters_changed` — fired by the range slider after each handle move.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_exts_ref = Arc::clone(&selected_exts);
        let backend_fc = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_filters_changed(move || {
            let lo = weak.upgrade().map(|w| w.get_size_lo()).unwrap_or(0.0);
            let hi = weak.upgrade().map(|w| w.get_size_hi()).unwrap_or(1.0);
            let gps_mode = weak.upgrade().map(|w| w.get_gps_mode()).unwrap_or(0);
            let exts = selected_exts_ref.lock().unwrap().clone();
            let new_filters = build_filters(lo, hi, size_bounds, &exts, gps_mode);
            let label = build_size_label(lo, hi, size_bounds);
            if let Some(w) = weak.upgrade() {
                w.set_size_label(label);
            }
            *filters_ref.lock().unwrap() = new_filters.clone();
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                backend_fc.clone(),
                new_filters,
            );
        });
    }

    // `on_ext_toggled` — fired when the user clicks a file-type chip.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_exts_ref = Arc::clone(&selected_exts);
        let all_exts_et = all_extensions.clone();
        let backend_et = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_ext_toggled(move |name| {
            let ext = name.to_string().to_lowercase();
            {
                let mut set = selected_exts_ref.lock().unwrap();
                if set.contains(&ext) {
                    set.remove(&ext);
                } else {
                    set.insert(ext);
                }
            }
            let selected = selected_exts_ref.lock().unwrap().clone();
            let model = build_chips_model(&all_exts_et, &selected);
            let (lo, hi, gps_mode) = weak
                .upgrade()
                .map(|w| (w.get_size_lo(), w.get_size_hi(), w.get_gps_mode()))
                .unwrap_or((0.0, 1.0, 0));
            if let Some(w) = weak.upgrade() {
                w.set_type_chips(model);
            }
            let new_filters = build_filters(lo, hi, size_bounds, &selected, gps_mode);
            *filters_ref.lock().unwrap() = new_filters.clone();
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                backend_et.clone(),
                new_filters,
            );
        });
    }

    // `on_gps_mode_changed` — fired when the user clicks Any/Has/No GPS.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_exts_ref = Arc::clone(&selected_exts);
        let backend_gps = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_gps_mode_changed(move |mode| {
            if let Some(w) = weak.upgrade() {
                w.set_gps_mode(mode);
            }
            let (lo, hi) = weak
                .upgrade()
                .map(|w| (w.get_size_lo(), w.get_size_hi()))
                .unwrap_or((0.0, 1.0));
            let exts = selected_exts_ref.lock().unwrap().clone();
            let new_filters = build_filters(lo, hi, size_bounds, &exts, mode);
            *filters_ref.lock().unwrap() = new_filters.clone();
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                backend_gps.clone(),
                new_filters,
            );
        });
    }

    // Keep the model timer and debounce timer alive for the entire event loop.
    let _ = model_timer;
    let _ = debounce_timer;

    window.run().context("Event loop failed")?;
    Ok(())
}

/// Restart the debounce timer with a fresh 250 ms single-shot callback.
///
/// Each filter-change event calls this function, which replaces any pending
/// timer fire with a new one carrying the *latest* filter snapshot.  Because
/// `Timer::start` accepts `FnMut + 'static` and we cannot move non-`Copy`
/// values out of an outer `FnMut` callback, we allocate a new closure per
/// restart — which is fine for a 250 ms debounce rate.
fn start_debounce(
    timer: &Timer,
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    mode_ref: Arc<Mutex<SearchMode>>,
    backend: Backend,
    filters: Filters,
) {
    timer.start(TimerMode::SingleShot, Duration::from_millis(250), move || {
        fire_debounced_query(weak.clone(), Arc::clone(&state_ref), Arc::clone(&mode_ref), backend.clone(), filters.clone());
    });
}

/// Called from all three debounce closures after rebuilding `Filters`.
///
/// Reads `search_mode` and either runs a text search, similarity search, or
/// browse, starting from offset 0.
fn fire_debounced_query(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    mode_ref: Arc<Mutex<SearchMode>>,
    backend: Backend,
    filters: Filters,
) {
    let mode = mode_ref.lock().unwrap().clone();
    match mode {
        SearchMode::Text(query) if !query.is_empty() => {
            state_ref.lock().unwrap().start_search(query.clone());
            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_show_load_more(false);
                w.set_can_search(false);
            }
            spawn_search(weak, state_ref, backend, query, 0, filters);
        }
        // Text("") = browse mode (or idle — browse with filters anyway).
        SearchMode::Text(_) => {
            state_ref.lock().unwrap().start_search(String::new());
            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_show_load_more(false);
                w.set_can_search(false);
            }
            spawn_browse(weak, state_ref, backend, filters, 0);
        }
        SearchMode::Similar(seed) => {
            state_ref.lock().unwrap().start_search(seed.clone());
            if let Some(w) = weak.upgrade() {
                let filename = filename_of(&seed);
                w.set_status(format!("Similar to {filename}").into());
                w.set_show_load_more(false);
                w.set_can_search(false);
            }
            spawn_similar(weak, state_ref, backend, seed, 0, filters);
        }
    }
}

/// Decode raw JPEG bytes (already on the UI thread) into Slint `Image` tiles.
fn build_tiles_model(results: &[SearchResult], raw_thumbs: Vec<Option<Vec<u8>>>) -> ModelRc<Tile> {
    let tiles: Vec<Tile> = results
        .iter()
        .zip(raw_thumbs)
        .map(|(r, maybe_bytes)| {
            let img = maybe_bytes
                .and_then(|bytes| image_util::jpeg_to_slint_image(&bytes).ok())
                .unwrap_or_default();
            let size_kb = r.file_size.unwrap_or(0) / 1024;
            Tile {
                path: r.path.clone().into(),
                image: img,
                size_kb: size_kb as i32,
            }
        })
        .collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(tiles)))
}

fn status_text_for(vs: ViewState, error_msg: Option<&str>) -> slint::SharedString {
    match vs {
        ViewState::Idle => "Enter a search query to find images.".into(),
        ViewState::Loading => "Searching\u{2026}".into(),
        ViewState::Error => error_msg.unwrap_or("Unknown error").into(),
        ViewState::Empty => "No images found.".into(),
        ViewState::Results => "".into(),
    }
}

/// Spawn a background search thread and marshal results back to the UI thread.
///
/// DB and disk I/O (thumbnail fetches) happen on the worker thread; only the
/// non-`Send` `slint::Image` decode runs inside `invoke_from_event_loop`.
fn spawn_search(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    backend: Backend,
    query: String,
    offset: usize,
    filters: Filters,
) {
    std::thread::spawn(move || {
        let res = backend.search(&query, offset, &filters);

        // Fetch raw thumbnail bytes on the worker thread (DB/disk I/O).
        // `Vec<Option<Vec<u8>>>` is Send; `slint::Image` is not.
        let raw_thumbs: Vec<Option<Vec<u8>>> = match &res {
            Ok(results) => results
                .iter()
                .map(|r| backend.thumbnail(&r.path, 300).ok())
                .collect(),
            Err(_) => Vec::new(),
        };

        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };

            let (vs, has_more, error_msg, results) = {
                let mut s = state_ref.lock().unwrap();
                match res {
                    Ok(page) => s.apply_page(page, offset),
                    Err(e) => s.apply_error(e.to_string(), offset),
                }
                let vs = s.view_state();
                let has_more = s.has_more;
                let error_msg = s.error.clone();
                let results = s.results.clone();
                (vs, has_more, error_msg, results)
            };

            // Only the (non-Send) JPEG→slint::Image decode runs here on the UI thread.
            if matches!(vs, ViewState::Results | ViewState::Empty) {
                let model = build_tiles_model(&results, raw_thumbs);
                w.set_tiles(model);
            }

            w.set_status(status_text_for(vs, error_msg.as_deref()));
            w.set_show_load_more(has_more && vs == ViewState::Results);
            w.set_can_search(true);
        })
        .ok();
    });
}

/// Like `spawn_search` but calls the vector-similarity backend.
///
/// The detail panel stays open (callers keep `detail-open` true); this
/// function only updates the tile grid.  Closures are `Send + 'static`
/// for the same reasons as `spawn_search`.
fn spawn_similar(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    backend: Backend,
    seed_path: String,
    offset: usize,
    filters: Filters,
) {
    std::thread::spawn(move || {
        let res = backend.search_similar(&seed_path, offset, &filters);

        let raw_thumbs: Vec<Option<Vec<u8>>> = match &res {
            Ok(results) => results
                .iter()
                .map(|r| backend.thumbnail(&r.path, 300).ok())
                .collect(),
            Err(_) => Vec::new(),
        };

        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };

            let (vs, has_more, error_msg, results) = {
                let mut s = state_ref.lock().unwrap();
                match res {
                    Ok(page) => s.apply_page(page, offset),
                    Err(e) => s.apply_error(e.to_string(), offset),
                }
                let vs = s.view_state();
                let has_more = s.has_more;
                let error_msg = s.error.clone();
                let results = s.results.clone();
                (vs, has_more, error_msg, results)
            };

            if matches!(vs, ViewState::Results | ViewState::Empty) {
                let model = build_tiles_model(&results, raw_thumbs);
                w.set_tiles(model);
            }

            w.set_status(status_text_for(vs, error_msg.as_deref()));
            w.set_show_load_more(has_more && vs == ViewState::Results);
            w.set_can_search(true);
        })
        .ok();
    });
}

/// Browse all indexed images subject to `filters`, paginated by `offset`.
///
/// Mirrors `spawn_search` but calls `backend.browse` instead of embedding a
/// text query.  Used when the query box is empty but filters are active, and
/// for the filter-bar debounce when `search_mode` is idle/browse.
fn spawn_browse(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    backend: Backend,
    filters: Filters,
    offset: usize,
) {
    std::thread::spawn(move || {
        let res = backend.browse(&filters, offset);

        let raw_thumbs: Vec<Option<Vec<u8>>> = match &res {
            Ok(results) => results
                .iter()
                .map(|r| backend.thumbnail(&r.path, 300).ok())
                .collect(),
            Err(_) => Vec::new(),
        };

        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };

            let (vs, has_more, error_msg, results) = {
                let mut s = state_ref.lock().unwrap();
                match res {
                    Ok(page) => s.apply_page(page, offset),
                    Err(e) => s.apply_error(e.to_string(), offset),
                }
                let vs = s.view_state();
                let has_more = s.has_more;
                let error_msg = s.error.clone();
                let results = s.results.clone();
                (vs, has_more, error_msg, results)
            };

            if matches!(vs, ViewState::Results | ViewState::Empty) {
                let model = build_tiles_model(&results, raw_thumbs);
                w.set_tiles(model);
            }

            w.set_status(status_text_for(vs, error_msg.as_deref()));
            w.set_show_load_more(has_more && vs == ViewState::Results);
            w.set_can_search(true);
        })
        .ok();
    });
}

/// Previous lightbox index, clamped at 0 (no wrap).
fn clamp_prev(current: usize) -> usize {
    current.saturating_sub(1)
}

/// Next lightbox index, clamped to the last valid index (no wrap).
/// Returns `current` unchanged when there are no results.
fn clamp_next(current: usize, len: usize) -> usize {
    if len == 0 {
        return current;
    }
    (current + 1).min(len - 1)
}

/// Load the full-size image for `rel_path` on a background thread, then set the
/// lightbox image and open the overlay on the UI thread.
///
/// File I/O and decoding are both off the UI thread so large originals don't
/// freeze the window. `slint::Image` is non-Send, so it is constructed inside
/// the `invoke_from_event_loop` closure where we are on the UI thread.
fn load_lightbox_image(weak: Weak<MainWindow>, backend: Backend, rel_path: String) {
    std::thread::spawn(move || {
        let abs = backend.abs_path(&rel_path);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Lightbox: failed to read {abs:?}: {e}");
                return;
            }
        };

        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            match image_util::jpeg_to_slint_image(&bytes) {
                Ok(img) => {
                    w.set_lightbox_image(img);
                    w.set_lightbox_open(true);
                }
                Err(e) => {
                    tracing::warn!("Lightbox: failed to decode {rel_path}: {e}");
                    // Leave the lightbox closed rather than showing a blank image.
                }
            }
        })
        .ok();
    });
}

#[cfg(test)]
mod tests {
    use super::{
        build_size_label, clamp_next, clamp_prev, format_bytes, fraction_to_bytes,
    };

    #[test]
    fn clamp_prev_at_zero_stays_zero() {
        assert_eq!(clamp_prev(0), 0);
    }

    #[test]
    fn clamp_prev_advances_backward() {
        assert_eq!(clamp_prev(5), 4);
    }

    #[test]
    fn clamp_next_at_last_stays_last() {
        assert_eq!(clamp_next(9, 10), 9);
    }

    #[test]
    fn clamp_next_advances_forward() {
        assert_eq!(clamp_next(3, 10), 4);
    }

    #[test]
    fn clamp_next_empty_len_returns_current() {
        assert_eq!(clamp_next(5, 0), 5);
    }

    // --- fraction_to_bytes ---

    #[test]
    fn lo_at_zero_is_unbounded() {
        assert_eq!(fraction_to_bytes(0.0, 0, 1_000_000, true), None);
    }

    #[test]
    fn hi_at_one_is_unbounded() {
        assert_eq!(fraction_to_bytes(1.0, 0, 1_000_000, false), None);
    }

    #[test]
    fn lo_mid_maps_correctly() {
        // 0.5 of [0, 1 000 000] = 500 000
        assert_eq!(fraction_to_bytes(0.5, 0, 1_000_000, true), Some(500_000));
    }

    #[test]
    fn hi_mid_maps_correctly() {
        assert_eq!(fraction_to_bytes(0.5, 0, 1_000_000, false), Some(500_000));
    }

    #[test]
    fn lo_at_one_is_not_unbounded() {
        // lo == 1.0 is NOT the "unbounded" extreme for the lo side (0.0 is).
        assert!(fraction_to_bytes(1.0, 0, 1_000_000, true).is_some());
    }

    #[test]
    fn hi_at_zero_is_not_unbounded() {
        assert!(fraction_to_bytes(0.0, 0, 1_000_000, false).is_some());
    }

    // --- format_bytes ---

    #[test]
    fn format_bytes_megabytes() {
        assert_eq!(format_bytes(2_097_152), "2.0 MB");
    }

    #[test]
    fn format_bytes_kilobytes() {
        assert_eq!(format_bytes(2048), "2 KB");
    }

    #[test]
    fn format_bytes_small() {
        assert_eq!(format_bytes(512), "512 B");
    }

    // --- build_size_label ---

    #[test]
    fn size_label_both_at_extremes_is_all() {
        let label = build_size_label(0.0, 1.0, (0, 10_000_000));
        assert_eq!(label, "Size: all");
    }

    #[test]
    fn size_label_partial_range() {
        // lo=0 (unbounded), hi=0.5 → "0 – 4.8 MB" (5_000_000 bytes = 4.8 MB)
        let label = build_size_label(0.0, 0.5, (0, 10_000_000));
        assert!(label.contains("4.8 MB"), "got: {label}");
    }
}
