slint::include_modules!();

mod backend;
mod chords;
mod detail;
mod detail_cache;
mod image_util;
mod loader;
mod nav;
mod preload;
mod selection;
mod state;
mod tagset;
mod window;

use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel, Weak};

use backend::Backend;
use detail::{DetailState, filename_of, format_metadata, select};
use imgfind::filters::{Filters, GpsFilter, TagMatch};
use imgfind::sort::{RowMeta, Sort, SortDir, SortKey};
use imgfind::ui_state::{PersistedMode, UiState};
use state::{SearchState, ViewState};

/// Whether the current tile grid was populated by a text search or a
/// vector-similarity search.  Stored in an `Arc<Mutex<_>>` so both the
/// `on_search` / `on_search_similar` closures (which set it) and the
/// `fire_debounced_query` path (which reads it) share the same value across threads.
#[derive(Clone)]
enum SearchMode {
    Text(String),
    Similar(String),
}

/// How the grid selection should be updated after a result set is installed
/// or reordered.
enum SelectAfter {
    /// Clear the selection to `None` (new text/browse that replaces the grid).
    Clear,
    /// Re-clamp the prior index to the new length (load-more, similarity).
    KeepIndex,
    /// Select index 0 if results are non-empty, else `None` (sort-key change).
    First,
    /// Select the row whose `id` equals the given id; `None` id or not found
    /// yields `None` (sort-direction toggle — follow the item to its new position).
    ById(Option<i64>),
}

/// Resolve the post-results selection index.
///
/// `prev` is the prior selected index (only used by `KeepIndex`).
fn resolve_selection(
    after: &SelectAfter,
    results: &[RowMeta],
    prev: Option<usize>,
) -> Option<usize> {
    match after {
        SelectAfter::Clear => None,
        SelectAfter::KeepIndex => prev.filter(|&i| i < results.len()),
        SelectAfter::First => (!results.is_empty()).then_some(0),
        SelectAfter::ById(Some(id)) => results.iter().position(|r| r.id == *id),
        SelectAfter::ById(None) => None,
    }
}

/// Selection-update policy forwarded to the three `spawn_*` helpers.
///
/// Bundling into a struct keeps the helpers under clippy's `too_many_arguments`
/// limit while keeping the policy explicit at every call site.
struct SelectionPolicy {
    /// Shared selection state to update when results land.
    selected: Arc<Mutex<Option<usize>>>,
    /// Controls how the selection is updated when results land.
    after: SelectAfter,
}

#[derive(Parser)]
#[command(name = "imgfind-gui", about = "CLIP image search — native GUI")]
struct Args {
    /// Directory to search for an imgfind database (walks up from here).
    #[arg(short, long)]
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

/// Build the always-visible bottom statusline from the current selection and
/// the full result set. ASCII separators only (Slint default-font glyph safety).
// wired in Task 3
#[allow(dead_code)]
fn format_statusline(sel: &selection::Selection, results: &[RowMeta]) -> String {
    let total_bytes: i64 = results.iter().filter_map(|r| r.size).sum();
    let label = match sel.mode() {
        selection::SelectionMode::Normal => "NORMAL",
        selection::SelectionMode::Free => "VISUAL (FREE)",
        selection::SelectionMode::Range { .. } => "VISUAL (RANGE)",
    };
    let base = format!(
        "{label} - {} images - {}",
        results.len(),
        format_bytes(total_bytes)
    );
    if sel.is_active() && !sel.is_empty() {
        let sel_bytes: i64 = sel
            .set()
            .iter()
            .filter_map(|&i| results.get(i).and_then(|r| r.size))
            .sum();
        format!(
            "{base} | selected {} - {}",
            sel.set().len(),
            format_bytes(sel_bytes)
        )
    } else {
        base
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
    Filters {
        size_min,
        size_max,
        extensions,
        gps,
        ..Default::default()
    }
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
        // ASCII-only separators/labels: the Slint default font has no glyph for
        // en-dash or U+221E and would render them as tofu (see project memory).
        (None, None) => "Size: all".into(),
        (Some(lo_b), None) => format!("{} - max", format_bytes(lo_b)).into(),
        (None, Some(hi_b)) => format!("0 - {}", format_bytes(hi_b)).into(),
        (Some(lo_b), Some(hi_b)) => {
            format!("{} - {}", format_bytes(lo_b), format_bytes(hi_b)).into()
        }
    }
}

fn main() -> Result<()> {
    // Honor RUST_LOG when set; default to `info`. Writes to stdout so logs are
    // visible when the GUI is launched from a terminal (e.g. `imgfind gui`).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let args = Args::parse();
    let backend = Backend::open(args.dir.as_deref()).context("Failed to open imgfind database")?;
    backend.start_loading_model();

    // Load config early — T19 and later tasks can reuse `gui_config` without
    // calling Config::load() again.
    let gui_config = imgfind::config::Config::load()
        .map(|c| c.gui)
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to load config, using defaults: {e}");
            imgfind::config::GuiConfig::default()
        });
    let preload_n = gui_config.preload_neighbors;

    // Fetch size bounds once at startup for the [0,1]↔bytes mapping.
    let size_bounds = backend.size_bounds().unwrap_or((0, 0));

    // All extensions known to the DB, used to drive the type-chips model.
    let all_extensions: Vec<String> = backend.extensions().unwrap_or_default();

    let window = MainWindow::new().context("Failed to create window")?;

    // Wrap each TagEditor's pills into rows that fit its current width. Shared
    // by all four TagEditor instances via the TagWrap global; reactive on width.
    window.global::<TagWrap>().on_rows(|tags, width| {
        let tags: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        // 11.0 = pill Text font-size in tag_editor.slint.
        let rows = tagset::wrap_tag_rows(&tags, width, 11.0);
        let model: Vec<ModelRc<SharedString>> = rows
            .into_iter()
            .map(|r| {
                ModelRc::new(VecModel::from(
                    r.into_iter().map(SharedString::from).collect::<Vec<_>>(),
                ))
            })
            .collect();
        ModelRc::new(VecModel::from(model))
    });

    // Initial UI state: model loading, search disabled.
    window.set_can_search(false);
    window.set_status("Loading model...".into());
    window.set_tiles(ModelRc::default());
    window.set_total_items(0);
    window.set_lightbox_open(false);
    window.set_detail_open(false);
    // Filter-bar initial state.
    window.set_size_lo(0.0);
    window.set_size_hi(1.0);
    window.set_size_label("Size: all".into());
    window.set_type_chips(build_chips_model(&all_extensions, &HashSet::new()));
    window.set_gps_mode(0);
    // Sort selector initial state — browse mode: Relevance not available.
    // Seed the selector to the configured default sort so the UI matches the
    // initial browse order from the moment results appear.
    {
        let default_sort = gui_config.resolved_sort();
        window.set_sort_options(make_sort_options_model(false));
        window.set_sort_index(sort_key_to_browse_index(default_sort.key));
        window.set_sort_desc(matches!(default_sort.dir, SortDir::Desc));
    }

    // State shared with background threads via Arc<Mutex<_>>.
    let state: Arc<Mutex<SearchState>> = Arc::new(Mutex::new(SearchState::new()));

    // Current lightbox index. None when the lightbox is closed.
    let lb_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

    // Index into `state.results` of the keyboard/mouse-selected tile (mirrors the
    // Slint `selected-index` property). Shares the index space with `lb_index`.
    let selected: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

    // Monotonic counter bumped on every lightbox load request. A background decode
    // only applies its result if its captured generation is still the latest, so a
    // slow decode can't land after the user has already navigated to another image.
    let lb_generation: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // Monotonic counter bumped at the start of every preload dispatch.
    // Bumping before spawning ensures stale in-flight preloads stop when the
    // user moves on to a different focus image.
    let preload_generation: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // Monotonic counter bumped every time a NEW result set is installed (each
    // search/similar/browse, and reset paths). The thumbnail worker tags each
    // response with the generation its request was issued under; the loader
    // timer drops responses whose generation no longer matches, so thumbnails
    // from a superseded search can't leak into the current grid.
    let grid_generation: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // Currently selected detail image. None when the panel is closed.
    let detail: Arc<Mutex<Option<DetailState>>> = Arc::new(Mutex::new(None));

    // Tracks whether the tile grid was populated by a text or similarity search,
    // so the filter-bar debounce (`fire_debounced_query`) knows which backend method to call.
    let search_mode: Arc<Mutex<SearchMode>> = Arc::new(Mutex::new(SearchMode::Text(String::new())));

    // Live filter state, shared with every query closure.
    let filters: Arc<Mutex<Filters>> = Arc::new(Mutex::new(Filters::default()));

    // Currently-selected extension set (drives chip `on` state + Filters.extensions).
    let selected_exts: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    // True only while `restore_session` is pre-filling the UI from a persisted
    // session. The `search` callback bails out while set so that programmatically
    // setting the search field (or any restore side effect) can never kick off a
    // re-query — the restored result list must survive verbatim.
    let restoring: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // Left-rail tag state. `brushes` holds the five color brushes' tag lists
    // (index-aligned with `colors::BrushColor`); `mm_buffer` is the editable
    // "Most Recent" staging set. Both are pure in-memory + persisted to
    // `UiState` on exit (no DB writes or background threads while editing).
    let brushes: Arc<Mutex<[Vec<String>; 5]>> = Arc::new(Mutex::new(Default::default()));
    let mm_buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Pending keyboard-chord prefix (e.g. after `m` we await a brush letter) and
    // the single-shot timer that resets a stale prefix after ~800ms. Both live on
    // the UI thread; the timer is !Send so `Rc` (not `Arc`) is correct.
    let pending_chord: Arc<Mutex<chords::Pending>> = Arc::new(Mutex::new(chords::Pending::None));
    let chord_timer: Rc<Timer> = Rc::new(Timer::default());

    // Count of tag text editors currently in edit mode. Each TagEditor bumps it
    // via `editor-editing-changed`; while it's > 0 the window's
    // `chords-suppressed` flag is set so chord keys go to the editor, not the
    // chord machine. A counter (vs a bool) is robust to a transient overlap when
    // one editor commits-on-blur as another gains focus. UI-thread-only → `Cell`.
    let editing_count: Rc<std::cell::Cell<i32>> = Rc::new(std::cell::Cell::new(0));

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
                    // Only show the empty-state hint when the grid has no
                    // results yet (the startup browse may have already filled
                    // it while the model was loading in parallel).
                    if w.get_total_items() == 0 {
                        w.set_status("Enter a search query to find images.".into());
                    }
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
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let restoring_ref = Arc::clone(&restoring);
        let backend_search = backend.clone();
        window.on_search(move |query| {
            // Never run a query while restoring a persisted session: the restored
            // result list (rehydrated by id) must be shown verbatim, with no
            // "Searching…" flash and no re-query.
            if restoring_ref.load(Ordering::SeqCst) {
                return;
            }
            let query = query.trim().to_string();
            let current_filters = filters_ref.lock().unwrap().clone();

            if query.is_empty() {
                if current_filters == Filters::default() {
                    // No query, no active filters — reset to idle.
                    *state_ref.lock().unwrap() = SearchState::new();
                    *detail_ref.lock().unwrap() = None;
                    *lb_ref.lock().unwrap() = None;
                    *selected_ref.lock().unwrap() = None;
                    *mode_ref.lock().unwrap() = SearchMode::Text(String::new());
                    // Bump the generation so the loader timer resets its
                    // in-flight set / last range and stops loading the old set.
                    loader::bump_generation(&grid_gen_ref);
                    if let Some(w) = weak.upgrade() {
                        w.set_status("Enter a search query to find images.".into());
                        w.set_tiles(ModelRc::default());
                        w.set_total_items(0);
                        w.set_detail_open(false);
                        w.set_lightbox_open(false);
                        w.set_selected_index(-1);
                        // Reset sort selector to browse mode (no Relevance option).
                        w.set_sort_options(make_sort_options_model(false));
                        w.set_sort_index(0);
                        w.set_sort_desc(false);
                    }
                    state_ref.lock().unwrap().sort = Sort::default();
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
                        w.set_can_search(false);
                    }
                    spawn_browse(
                        weak.clone(),
                        Arc::clone(&state_ref),
                        Arc::clone(&grid_gen_ref),
                        backend_search.clone(),
                        current_filters,
                        SelectionPolicy {
                            selected: Arc::clone(&selected_ref),
                            after: SelectAfter::Clear,
                        },
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
                w.set_can_search(false);
            }

            spawn_search(
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&grid_gen_ref),
                backend_search.clone(),
                query,
                current_filters,
                SelectionPolicy {
                    selected: Arc::clone(&selected_ref),
                    after: SelectAfter::Clear,
                },
            );
        });
    }

    // --- tile-selected callback: show detail panel for the clicked tile ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let detail_ref = Arc::clone(&detail);
        let selected_ref = Arc::clone(&selected);
        let backend_detail = backend.clone();
        let preload_gen_detail = Arc::clone(&preload_generation);
        window.on_tile_selected(move |index| {
            let idx = index as usize;
            // Keep the shared selection in sync so keyboard and mouse share one highlight.
            *selected_ref.lock().unwrap() = Some(idx);
            if let Some(w) = weak.upgrade() {
                w.set_selected_index(index);
            }
            let path = {
                let s = state_ref.lock().unwrap();
                s.results.get(idx).map(|r| r.path.clone())
            };
            let Some(path) = path else { return };

            // Compute detail identity synchronously (just string ops).
            let ds = select(path.clone());
            *detail_ref.lock().unwrap() = Some(ds.clone());

            // Open the panel immediately. A cache hit can show the image now;
            // a miss shows a placeholder until the worker decodes it.
            let cached = detail_cache::get(&path);
            if let Some(w) = weak.upgrade() {
                w.set_detail_open(true);
                w.set_detail_filename(ds.filename.into());
                match &cached {
                    Some(img) => w.set_detail_image(img.clone()),
                    None => w.set_detail_image(Default::default()),
                }
                w.set_detail_meta("Loading\u{2026}".into());
            }

            // Load the preview image (only on a cache miss) and metadata on
            // background threads. The image is never held hostage by the
            // (potentially slower) metadata read: each posts its own
            // `invoke_from_event_loop`.
            let path_for_tags = path.clone();
            spawn_detail_image(weak.clone(), backend_detail.clone(), path.clone(), cached);
            spawn_detail_meta(weak.clone(), backend_detail.clone(), path.clone());

            // Preload neighbors at 512px for fast detail-panel switching, also
            // decoding + caching them on the UI thread so back/forward lands an
            // already-decoded image.
            {
                let my_gen = preload_gen_detail.fetch_add(1, Ordering::SeqCst) + 1;
                let paths = {
                    let s = state_ref.lock().unwrap();
                    neighbor_paths(&s.results, idx, preload_n)
                };
                spawn_detail_preload(
                    weak.clone(),
                    backend_detail.clone(),
                    paths,
                    Arc::clone(&preload_gen_detail),
                    my_gen,
                );
            }

            // Load tags on a background thread; marshal back to the UI thread.
            {
                let backend3 = backend_detail.clone();
                let w = weak.clone();
                std::thread::spawn(move || {
                    let tags = backend3.tags_for(&path_for_tags).unwrap_or_default();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = w.upgrade() {
                            push_detail_tags(&w, &tags);
                        }
                    });
                });
            }
        });
    }

    // --- tile-activated callback: open lightbox at the activated index ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let selected_ref = Arc::clone(&selected);
        let backend_lb = backend.clone();
        let gen_ref = Arc::clone(&lb_generation);
        let preload_gen_ta = Arc::clone(&preload_generation);
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
            // Seed the grid selection so closing the lightbox (Esc) lands on this tile.
            *selected_ref.lock().unwrap() = Some(idx);
            if let Some(w) = weak.upgrade() {
                w.set_selected_index(index);
            }
            load_lightbox_image(weak.clone(), backend_lb.clone(), rel, gen_ref.clone());

            // Preload neighbors at LIGHTBOX_SIZE so prev/next navigation is instant.
            {
                let my_gen = preload_gen_ta.fetch_add(1, Ordering::SeqCst) + 1;
                let paths = {
                    let s = state_ref.lock().unwrap();
                    neighbor_paths(&s.results, idx, preload_n)
                };
                spawn_preload(
                    backend_lb.clone(),
                    paths,
                    imgfind::thumbnail::LIGHTBOX_SIZE,
                    Arc::clone(&preload_gen_ta),
                    my_gen,
                );
            }
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

    // --- detail-tag-committed callback: diff desired vs current, apply adds/removes ---
    {
        let backend = backend.clone();
        let detail_ref = Arc::clone(&detail);
        let w = window.as_weak();
        window.on_detail_tag_committed(move |text| {
            let Some(path) = detail_ref.lock().unwrap().as_ref().map(|d| d.path.clone()) else {
                return;
            };
            let backend = backend.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let mut desired = Vec::new();
                tagset::set_words(&mut desired, text.as_str());
                let current = backend.tags_for(&path).unwrap_or_default();
                for t in &desired {
                    if !current.contains(t) {
                        let _ = backend.add_tag(&path, t);
                    }
                }
                for t in &current {
                    if !desired.contains(t) {
                        let _ = backend.remove_tag(&path, t);
                    }
                }
                let tags = backend.tags_for(&path).unwrap_or_default();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w.upgrade() {
                        push_detail_tags(&w, &tags);
                    }
                });
            });
        });
    }

    // --- detail-tag-remove callback: remove one tag from the current image ---
    {
        let backend = backend.clone();
        let detail_ref = Arc::clone(&detail);
        let w = window.as_weak();
        window.on_detail_tag_remove(move |tag| {
            let Some(path) = detail_ref.lock().unwrap().as_ref().map(|d| d.path.clone()) else {
                return;
            };
            let backend = backend.clone();
            let w = w.clone();
            std::thread::spawn(move || {
                let _ = backend.remove_tag(&path, tag.as_str());
                let tags = backend.tags_for(&path).unwrap_or_default();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w.upgrade() {
                        push_detail_tags(&w, &tags);
                    }
                });
            });
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
        let gen_ref = Arc::clone(&lb_generation);
        let preload_gen_vf = Arc::clone(&preload_generation);
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

            load_lightbox_image(weak.clone(), backend_vf.clone(), rel, gen_ref.clone());

            // Preload neighbors when a valid grid position is known.
            if let Some(center) = idx {
                let my_gen = preload_gen_vf.fetch_add(1, Ordering::SeqCst) + 1;
                let paths = {
                    let s = state_ref.lock().unwrap();
                    neighbor_paths(&s.results, center, preload_n)
                };
                spawn_preload(
                    backend_vf.clone(),
                    paths,
                    imgfind::thumbnail::LIGHTBOX_SIZE,
                    Arc::clone(&preload_gen_vf),
                    my_gen,
                );
            }
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
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
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

            // `start_search` records the seed path as the "committed query".
            // NOTE: `committed_query` holds a file path here, NOT a text query;
            // `search_mode` (not this field) drives dispatch, and
            // `committed_query` is never displayed — do NOT rely on it being a
            // real text query during a similarity search.
            state_ref.lock().unwrap().start_search(seed_path.clone());
            if let Some(w) = weak.upgrade() {
                w.set_status(format!("Similar to {filename}").into());
                w.set_can_search(false);
                // detail-open stays TRUE — panel remains on the seed.
            }

            spawn_similar(
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&grid_gen_ref),
                backend_sim.clone(),
                seed_path,
                current_filters,
                SelectionPolicy {
                    selected: Arc::clone(&selected_ref),
                    after: SelectAfter::KeepIndex,
                },
            );
        });
    }

    // --- lightbox-close callback ---
    {
        let weak = window.as_weak();
        let lb_ref = Arc::clone(&lb_index);
        let gen_ref = Arc::clone(&lb_generation);
        window.on_lightbox_close(move || {
            tracing::debug!("lightbox-close invoked");
            *lb_ref.lock().unwrap() = None;
            // Invalidate any in-flight decode so its completion cannot re-assert
            // lightbox-open = true and reopen the lightbox we just closed.
            gen_ref.fetch_add(1, Ordering::SeqCst);
            if let Some(w) = weak.upgrade() {
                w.set_lightbox_open(false);
                // Keyboard focus returns to the grid via the app-keys capture scope.
            }
        });
    }

    // --- lightbox-prev callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let selected_ref = Arc::clone(&selected);
        let backend_lb = backend.clone();
        let gen_ref = Arc::clone(&lb_generation);
        let preload_gen_prev = Arc::clone(&preload_generation);
        window.on_lightbox_prev(move || {
            tracing::debug!("lightbox-prev invoked");
            let new_idx = {
                let mut guard = lb_ref.lock().unwrap();
                let current = guard.unwrap_or(0);
                let next = clamp_prev(current);
                *guard = Some(next);
                next
            };
            // Mirror into the grid selection so closing the lightbox lands on this tile.
            *selected_ref.lock().unwrap() = Some(new_idx);
            if let Some(w) = weak.upgrade() {
                w.set_selected_index(new_idx as i32);
            }
            let path = state_ref
                .lock()
                .unwrap()
                .results
                .get(new_idx)
                .map(|r| r.path.clone());
            if let Some(rel) = path {
                load_lightbox_image(weak.clone(), backend_lb.clone(), rel, gen_ref.clone());
            }

            // Preload neighbors in arc order so the next likely images are warm.
            {
                let my_gen = preload_gen_prev.fetch_add(1, Ordering::SeqCst) + 1;
                let paths = {
                    let s = state_ref.lock().unwrap();
                    neighbor_paths(&s.results, new_idx, preload_n)
                };
                spawn_preload(
                    backend_lb.clone(),
                    paths,
                    imgfind::thumbnail::LIGHTBOX_SIZE,
                    Arc::clone(&preload_gen_prev),
                    my_gen,
                );
            }
        });
    }

    // --- lightbox-next callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let selected_ref = Arc::clone(&selected);
        let backend_lb = backend.clone();
        let gen_ref = Arc::clone(&lb_generation);
        let preload_gen_next = Arc::clone(&preload_generation);
        window.on_lightbox_next(move || {
            tracing::debug!("lightbox-next invoked");
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
            // Mirror into the grid selection so closing the lightbox lands on this tile.
            *selected_ref.lock().unwrap() = Some(new_idx);
            if let Some(w) = weak.upgrade() {
                w.set_selected_index(new_idx as i32);
            }
            let path = state_ref
                .lock()
                .unwrap()
                .results
                .get(new_idx)
                .map(|r| r.path.clone());
            if let Some(rel) = path {
                load_lightbox_image(weak.clone(), backend_lb.clone(), rel, gen_ref.clone());
            }

            // Preload neighbors in arc order so the next likely images are warm.
            {
                let my_gen = preload_gen_next.fetch_add(1, Ordering::SeqCst) + 1;
                let paths = {
                    let s = state_ref.lock().unwrap();
                    neighbor_paths(&s.results, new_idx, preload_n)
                };
                spawn_preload(
                    backend_lb.clone(),
                    paths,
                    imgfind::thumbnail::LIGHTBOX_SIZE,
                    Arc::clone(&preload_gen_next),
                    my_gen,
                );
            }
        });
    }

    // --- grid-nav callback: keyboard hjkl/arrow movement in the tile grid ---
    {
        let state_ref = Arc::clone(&state);
        let selected_ref = Arc::clone(&selected);
        let weak = window.as_weak();
        window.on_grid_nav(move |dir_i, cols_i| {
            let Some(dir) = nav::NavDir::from_i32(dir_i) else {
                return;
            };
            let len = state_ref.lock().unwrap().results.len();
            let cur = *selected_ref.lock().unwrap();
            let new = nav::move_selection(cur, dir, cols_i.max(0) as usize, len);
            *selected_ref.lock().unwrap() = new;
            if let Some(w) = weak.upgrade() {
                w.set_selected_index(new.map(|i| i as i32).unwrap_or(-1));
                // Live-update the detail panel only if it is already open.
                if let Some(i) = new
                    && w.get_detail_open()
                {
                    w.invoke_tile_selected(i as i32);
                }
            }
        });
    }

    // --- grid-open-detail callback: Enter key opens the detail panel for the selected tile ---
    {
        let selected_ref = Arc::clone(&selected);
        let weak = window.as_weak();
        window.on_grid_open_detail(move || {
            // Copy the selection out and DROP the guard before invoking
            // tile-selected, which re-locks `selected`. std::sync::Mutex is not
            // reentrant, so holding the guard across the invoke (as an if-let
            // scrutinee temporary does — it lives through the body) deadlocks the
            // UI thread.
            let sel = *selected_ref.lock().unwrap();
            if let (Some(i), Some(w)) = (sel, weak.upgrade()) {
                w.invoke_tile_selected(i as i32);
            }
        });
    }

    // --- grid-open-lightbox callback: Space key opens the lightbox for the selected tile ---
    {
        let selected_ref = Arc::clone(&selected);
        let weak = window.as_weak();
        window.on_grid_open_lightbox(move || {
            // Drop the `selected` guard before invoking tile-activated (which
            // re-locks `selected`); see on_grid_open_detail above — a held
            // if-let scrutinee guard across the invoke deadlocks the UI thread.
            let sel = *selected_ref.lock().unwrap();
            if let (Some(i), Some(w)) = (sel, weak.upgrade()) {
                w.invoke_tile_activated(i as i32);
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
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_fc = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_filters_changed(move || {
            let (lo, hi, gps_mode) = match weak.upgrade() {
                Some(w) => (w.get_size_lo(), w.get_size_hi(), w.get_gps_mode()),
                None => return,
            };
            let exts = selected_exts_ref.lock().unwrap().clone();
            let mut new_filters = build_filters(lo, hi, size_bounds, &exts, gps_mode);
            let label = build_size_label(lo, hi, size_bounds);
            if let Some(w) = weak.upgrade() {
                w.set_size_label(label);
            }
            {
                let mut stored = filters_ref.lock().unwrap();
                new_filters.carry_tag_filter_from(&stored);
                *stored = new_filters.clone();
            }
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_fc.clone(),
                new_filters,
                Arc::clone(&selected_ref),
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
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
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
            let active_exts = selected_exts_ref.lock().unwrap().clone();
            let model = build_chips_model(&all_exts_et, &active_exts);
            let (lo, hi, gps_mode) = weak
                .upgrade()
                .map(|w| (w.get_size_lo(), w.get_size_hi(), w.get_gps_mode()))
                .unwrap_or((0.0, 1.0, 0));
            if let Some(w) = weak.upgrade() {
                w.set_type_chips(model);
            }
            let mut new_filters = build_filters(lo, hi, size_bounds, &active_exts, gps_mode);
            {
                let mut stored = filters_ref.lock().unwrap();
                new_filters.carry_tag_filter_from(&stored);
                *stored = new_filters.clone();
            }
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_et.clone(),
                new_filters,
                Arc::clone(&selected_ref),
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
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
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
            let mut new_filters = build_filters(lo, hi, size_bounds, &exts, mode);
            {
                let mut stored = filters_ref.lock().unwrap();
                new_filters.carry_tag_filter_from(&stored);
                *stored = new_filters.clone();
            }
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_gps.clone(),
                new_filters,
                Arc::clone(&selected_ref),
            );
        });
    }

    // `on_filter_tags_committed` — fired when the user commits text in the tag
    // filter editor; replaces the tag set, reflects it to the UI, then kicks the
    // shared debounce so the current-mode query re-runs filtered.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_tc = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_filter_tags_committed(move |text| {
            let Some(w) = weak.upgrade() else { return };
            let new_filters = {
                let mut f = filters_ref.lock().unwrap();
                tagset::set_words(&mut f.tags, text.as_str());
                push_filter_tags(&w, &f);
                f.clone()
            };
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_tc.clone(),
                new_filters,
                Arc::clone(&selected_ref),
            );
        });
    }

    // `on_filter_tag_remove` — fired when the user removes a single tag pill.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_tr = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_filter_tag_remove(move |tag| {
            let Some(w) = weak.upgrade() else { return };
            let new_filters = {
                let mut f = filters_ref.lock().unwrap();
                tagset::remove(&mut f.tags, tag.as_str());
                push_filter_tags(&w, &f);
                f.clone()
            };
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_tr.clone(),
                new_filters,
                Arc::clone(&selected_ref),
            );
        });
    }

    // `on_tag_match_toggled` — flips the tag-filter mode between AND (AllOf) and
    // OR (AnyOf).
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_tm = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_tag_match_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let new_filters = {
                let mut f = filters_ref.lock().unwrap();
                f.tag_match = match f.tag_match {
                    TagMatch::AllOf => TagMatch::AnyOf,
                    TagMatch::AnyOf => TagMatch::AllOf,
                };
                push_filter_tags(&w, &f);
                f.clone()
            };
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_tm.clone(),
                new_filters,
                Arc::clone(&selected_ref),
            );
        });
    }

    // `on_tags_enabled_toggled` — flips the master enable for the tag filter.
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_te = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        window.on_tags_enabled_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let new_filters = {
                let mut f = filters_ref.lock().unwrap();
                f.tags_enabled = !f.tags_enabled;
                push_filter_tags(&w, &f);
                f.clone()
            };
            start_debounce(
                &timer,
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen_ref),
                backend_te.clone(),
                new_filters,
                Arc::clone(&selected_ref),
            );
        });
    }

    // --- sort-changed callback: user picked a different sort key ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_sc = backend.clone();
        window.on_sort_changed(move || {
            apply_sort_change(
                weak.clone(),
                Arc::clone(&mode_ref),
                Arc::clone(&state_ref),
                Arc::clone(&filters_ref),
                Arc::clone(&selected_ref),
                Arc::clone(&grid_gen_ref),
                backend_sc.clone(),
                SelectAfter::First,
            );
        });
    }

    // --- sort-dir-toggled callback: user clicked the Asc/Desc button ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let filters_ref = Arc::clone(&filters);
        let selected_ref = Arc::clone(&selected);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_sc = backend.clone();
        window.on_sort_dir_toggled(move || {
            let Some(w) = weak.upgrade() else { return };
            let new_desc = !w.get_sort_desc();
            w.set_sort_desc(new_desc);
            // Capture the currently selected item's id BEFORE the resort,
            // so we can follow it to its new position after the direction flip.
            let prev_id: Option<i64> = {
                let sel = *selected_ref.lock().unwrap();
                let s = state_ref.lock().unwrap();
                sel.and_then(|i| s.results.get(i)).map(|r| r.id)
            };
            apply_sort_change(
                weak.clone(),
                Arc::clone(&mode_ref),
                Arc::clone(&state_ref),
                Arc::clone(&filters_ref),
                Arc::clone(&selected_ref),
                Arc::clone(&grid_gen_ref),
                backend_sc.clone(),
                SelectAfter::ById(prev_id),
            );
        });
    }

    // --- brush-committed callback: set a brush's tags from raw editor text ---
    {
        let brushes_ref = Arc::clone(&brushes);
        let mm_ref = Arc::clone(&mm_buffer);
        let weak = window.as_weak();
        let backend_bc = backend.clone();
        window.on_brush_committed(move |idx, text| {
            let Some(w) = weak.upgrade() else { return };
            {
                let mut b = brushes_ref.lock().unwrap();
                if let Some(slot) = b.get_mut(idx as usize) {
                    tagset::set_words(slot, text.as_str());
                }
                let recent = mm_ref.lock().unwrap();
                push_rail_models(&w, &b, &recent);
            }
            persist_rail(&backend_bc, &brushes_ref, &mm_ref, w.get_rail_visible());
        });
    }

    // --- brush-remove callback: remove one tag from a brush ---
    {
        let brushes_ref = Arc::clone(&brushes);
        let mm_ref = Arc::clone(&mm_buffer);
        let weak = window.as_weak();
        let backend_br = backend.clone();
        window.on_brush_remove(move |idx, tag| {
            let Some(w) = weak.upgrade() else { return };
            {
                let mut b = brushes_ref.lock().unwrap();
                if let Some(slot) = b.get_mut(idx as usize) {
                    tagset::remove(slot, tag.as_str());
                }
                let recent = mm_ref.lock().unwrap();
                push_rail_models(&w, &b, &recent);
            }
            persist_rail(&backend_br, &brushes_ref, &mm_ref, w.get_rail_visible());
        });
    }

    // --- recent-remove callback: remove one tag from the Most Recent buffer ---
    {
        let brushes_ref = Arc::clone(&brushes);
        let mm_ref = Arc::clone(&mm_buffer);
        let weak = window.as_weak();
        let backend_rr = backend.clone();
        window.on_recent_remove(move |tag| {
            let Some(w) = weak.upgrade() else { return };
            {
                let b = brushes_ref.lock().unwrap();
                let mut recent = mm_ref.lock().unwrap();
                tagset::remove(&mut recent, tag.as_str());
                push_rail_models(&w, &b, &recent);
            }
            persist_rail(&backend_rr, &brushes_ref, &mm_ref, w.get_rail_visible());
        });
    }

    // --- toggle-rail callback: flip left-rail visibility ---
    {
        let weak = window.as_weak();
        window.on_toggle_rail(move || {
            if let Some(w) = weak.upgrade() {
                w.set_rail_visible(!w.get_rail_visible());
            }
        });
    }

    // --- editor-editing-changed callback: track active tag editors ---
    //
    // Each TagEditor fires this on entering (true) / leaving (false) edit mode.
    // We keep a UI-thread count and reflect "any editor editing" into the window
    // so `capture-key-pressed` suppresses chord forwarding while a tag field types.
    {
        let weak = window.as_weak();
        let editing_count = Rc::clone(&editing_count);
        window.on_editor_editing_changed(move |editing| {
            let n = (editing_count.get() + if editing { 1 } else { -1 }).max(0);
            editing_count.set(n);
            if let Some(w) = weak.upgrade() {
                w.set_chords_suppressed(n > 0);
            }
        });
    }

    // --- key callback: keyboard-chord dispatcher ---
    //
    // app.slint forwards one chord trigger/completion key per press here (only
    // when no text editor or the modal has focus). `chords::resolve` advances the
    // pure state machine; we arm/cancel an ~800ms prefix-reset timer and execute
    // the resolved action. All Backend writes happen on background threads.
    {
        let pending_chord = Arc::clone(&pending_chord);
        let chord_timer = Rc::clone(&chord_timer);
        let brushes_ref = Arc::clone(&brushes);
        let mm_ref = Arc::clone(&mm_buffer);
        let filters_ref = Arc::clone(&filters);
        let detail_ref = Arc::clone(&detail);
        let selected_ref = Arc::clone(&selected);
        let lb_ref = Arc::clone(&lb_index);
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let grid_gen_ref = Arc::clone(&grid_generation);
        let backend_key = backend.clone();
        let timer = Rc::clone(&debounce_timer);
        let weak = window.as_weak();
        window.on_key(move |key| {
            let Some(w) = weak.upgrade() else { return };

            // Advance the pure chord state machine.
            let (next, action) = {
                let mut pend = pending_chord.lock().unwrap();
                let (next, action) = chords::resolve(*pend, key.as_str());
                *pend = next;
                (next, action)
            };

            // Arm an ~800ms reset while a prefix is pending; otherwise cancel it.
            if matches!(next, chords::Pending::AwaitM | chords::Pending::AwaitF) {
                let pc = Arc::clone(&pending_chord);
                chord_timer.start(
                    TimerMode::SingleShot,
                    Duration::from_millis(800),
                    move || {
                        *pc.lock().unwrap() = chords::Pending::None;
                    },
                );
            } else {
                chord_timer.stop();
            }

            let Some(action) = action else { return };
            let tag_ctx = TagTargetCtx {
                detail: Arc::clone(&detail_ref),
                selected: Arc::clone(&selected_ref),
                lb_index: Arc::clone(&lb_ref),
                state: Arc::clone(&state_ref),
            };
            match action {
                chords::Action::ToggleRail => {
                    w.set_rail_visible(!w.get_rail_visible());
                }
                chords::Action::OpenTagModal => {
                    w.set_tag_modal_open(true);
                }
                chords::Action::PaintBrush(c) => {
                    let tags = brushes_ref.lock().unwrap()[c.index()].clone();
                    apply_tags_to_focused(&weak, &backend_key, &tag_ctx, tags.clone());
                    *mm_ref.lock().unwrap() = tags;
                    push_rail_models(&w, &brushes_ref.lock().unwrap(), &mm_ref.lock().unwrap());
                }
                chords::Action::RepeatLast => {
                    let tags = mm_ref.lock().unwrap().clone();
                    apply_tags_to_focused(&weak, &backend_key, &tag_ctx, tags);
                }
                chords::Action::LoadBrushIntoFilter(c) => {
                    let new_filters = {
                        let tags = brushes_ref.lock().unwrap()[c.index()].clone();
                        let mut f = filters_ref.lock().unwrap();
                        f.tags = tags;
                        push_filter_tags(&w, &f);
                        f.clone()
                    };
                    start_debounce(
                        &timer,
                        weak.clone(),
                        Arc::clone(&state_ref),
                        Arc::clone(&mode_ref),
                        Arc::clone(&grid_gen_ref),
                        backend_key.clone(),
                        new_filters,
                        Arc::clone(&selected_ref),
                    );
                }
                chords::Action::ToggleTagFilter => {
                    let new_filters = {
                        let mut f = filters_ref.lock().unwrap();
                        f.tags_enabled = !f.tags_enabled;
                        push_filter_tags(&w, &f);
                        f.clone()
                    };
                    start_debounce(
                        &timer,
                        weak.clone(),
                        Arc::clone(&state_ref),
                        Arc::clone(&mode_ref),
                        Arc::clone(&grid_gen_ref),
                        backend_key.clone(),
                        new_filters,
                        Arc::clone(&selected_ref),
                    );
                }
            }
        });
    }

    // --- tag-modal-commit callback: apply typed tags to the focused image ---
    {
        let mm_ref = Arc::clone(&mm_buffer);
        let brushes_ref = Arc::clone(&brushes);
        let detail_ref = Arc::clone(&detail);
        let selected_ref = Arc::clone(&selected);
        let lb_ref = Arc::clone(&lb_index);
        let state_ref = Arc::clone(&state);
        let backend_modal = backend.clone();
        let weak = window.as_weak();
        window.on_tag_modal_commit(move |text| {
            let Some(w) = weak.upgrade() else { return };
            let mut tags = Vec::new();
            tagset::set_words(&mut tags, text.as_str());
            let tag_ctx = TagTargetCtx {
                detail: Arc::clone(&detail_ref),
                selected: Arc::clone(&selected_ref),
                lb_index: Arc::clone(&lb_ref),
                state: Arc::clone(&state_ref),
            };
            apply_tags_to_focused(&weak, &backend_modal, &tag_ctx, tags.clone());
            *mm_ref.lock().unwrap() = tags;
            push_rail_models(&w, &brushes_ref.lock().unwrap(), &mm_ref.lock().unwrap());
            w.set_tag_modal_open(false);
        });
    }

    // --- tag-modal-cancel callback: close the modal without applying ---
    {
        let weak = window.as_weak();
        window.on_tag_modal_cancel(move || {
            if let Some(w) = weak.upgrade() {
                w.set_tag_modal_open(false);
            }
        });
    }

    // --- moving-window thumbnail loader ---
    //
    // A single background worker decodes/persists thumbnails (one writer to
    // dodge SQLITE_BUSY); the loader timer below runs on the UI thread, drains
    // decoded bytes into a bounded LRU, and rebuilds the tile model for only the
    // currently-visible window whenever the window or result set changes.
    let (req_tx, req_rx) = mpsc::channel::<loader::ThumbRequest>();
    let (resp_tx, resp_rx) = mpsc::channel::<loader::ThumbResponse>();
    loader::spawn_thumb_worker(backend.clone(), req_rx, resp_tx);

    let loader_timer = Timer::default();
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let grid_gen_ref = Arc::clone(&grid_generation);
        // These are all !Send and owned solely by this UI-thread closure: never
        // wrap them in Arc/Mutex. `last_gen` lets the closure detect a new
        // result set (generation bumped elsewhere) and reset its window state.
        let mut cache = loader::new_cache();
        let mut in_flight = loader::InFlight::default();
        let mut last_range: Option<Range<usize>> = None;
        let mut last_gen: u64 = loader::current_generation(&grid_gen_ref);
        loader_timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return };
            loader_tick(LoaderTick {
                window: &w,
                state_ref: &state_ref,
                grid_gen: &grid_gen_ref,
                req_tx: &req_tx,
                resp_rx: &resp_rx,
                cache: &mut cache,
                in_flight: &mut in_flight,
                last_range: &mut last_range,
                last_gen: &mut last_gen,
            });
        });
    }

    // Keep the model, debounce, and loader timers alive for the event loop.
    let _ = model_timer;
    let _ = debounce_timer;
    let _ = loader_timer;
    let _ = chord_timer;

    // T20: Startup gate — restore the persisted session if one exists, else
    // fall back to the T19 default browse-all. A malformed blob reads as
    // `Ok(None)`; a hard read error is logged and also falls back to default.
    match backend.get_ui_state() {
        Ok(Some(st)) => {
            let ctx = RestoreCtx {
                weak: window.as_weak(),
                state: Arc::clone(&state),
                grid_gen: Arc::clone(&grid_generation),
                backend: backend.clone(),
                selected: Arc::clone(&selected),
                filters: Arc::clone(&filters),
                selected_exts: Arc::clone(&selected_exts),
                search_mode: Arc::clone(&search_mode),
                detail: Arc::clone(&detail),
                restoring: Arc::clone(&restoring),
                all_extensions: &all_extensions,
                size_bounds,
                brushes: Arc::clone(&brushes),
                mm_buffer: Arc::clone(&mm_buffer),
            };
            restore_session(st, ctx);
        }
        Ok(None) => {
            init_fresh_rail(&window, &brushes, &mm_buffer);
            push_filter_tags(&window, &filters.lock().unwrap());
            start_default_browse(
                window.as_weak(),
                Arc::clone(&state),
                Arc::clone(&grid_generation),
                backend.clone(),
                Arc::clone(&selected),
                gui_config.resolved_sort(),
            );
        }
        Err(e) => {
            tracing::warn!("Failed to read persisted session, starting fresh: {e:#}");
            init_fresh_rail(&window, &brushes, &mm_buffer);
            push_filter_tags(&window, &filters.lock().unwrap());
            start_default_browse(
                window.as_weak(),
                Arc::clone(&state),
                Arc::clone(&grid_generation),
                backend.clone(),
                Arc::clone(&selected),
                gui_config.resolved_sort(),
            );
        }
    }

    window.run().context("Event loop failed")?;

    // T20: Persist the session AFTER the event loop returns. `window` and every
    // Arc<Mutex<…>> holder are still in scope; reading the live `grid-viewport-y`
    // captures the final scroll position. Persistence must never crash exit, so a
    // failure is logged and swallowed.
    persist_session(
        &window,
        &state,
        &filters,
        &selected,
        &detail,
        &search_mode,
        &backend,
        &brushes,
        &mm_buffer,
    );

    Ok(())
}

/// Map a [`SortKey`] to its index in the browse-mode sort selector.
///
/// The index order is fixed to match `make_sort_options_model(false)`:
/// `["Name", "Size", "Type"]` → Name=0, Size=1, Type=2.  A future reorder of
/// that model must keep these indices in sync.
fn sort_key_to_browse_index(key: SortKey) -> i32 {
    // Keep in sync with make_sort_options_model(false) = ["Name", "Size", "Type"].
    match key {
        SortKey::Name => 0,
        SortKey::Size => 1,
        SortKey::Type => 2,
    }
}

/// Slint swatch color for a [`BrushColor`].
fn brush_swatch(c: imgfind::colors::BrushColor) -> slint::Color {
    use imgfind::colors::BrushColor;
    match c {
        BrushColor::Red => slint::Color::from_rgb_u8(0xd0, 0x4a, 0x4a),
        BrushColor::Green => slint::Color::from_rgb_u8(0x3a, 0xa6, 0x60),
        BrushColor::Yellow => slint::Color::from_rgb_u8(0xd2, 0xb0, 0x3a),
        BrushColor::Purple => slint::Color::from_rgb_u8(0x9a, 0x5a, 0xc8),
        BrushColor::Blue => slint::Color::from_rgb_u8(0x4a, 0x7c, 0xd0),
    }
}

/// Build a `[string]` Slint model from a slice of owned strings.
fn string_model(items: &[String]) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = items.iter().map(|s| s.as_str().into()).collect();
    ModelRc::new(VecModel::from(rows))
}

/// Push the brush rows and Most-Recent model into the window. Called after any
/// rail edit and during restore/fresh-start setup.
fn push_rail_models(w: &MainWindow, brushes: &[Vec<String>; 5], recent: &[String]) {
    use imgfind::colors::BrushColor;
    let rows: Vec<BrushRow> = BrushColor::ALL
        .iter()
        .map(|&c| {
            let tags = &brushes[c.index()];
            BrushRow {
                letter: c.letter().into(),
                color: brush_swatch(c),
                tags: string_model(tags),
                joined: tags.join(" ").into(),
            }
        })
        .collect();
    w.set_brushes(ModelRc::new(VecModel::from(rows)));
    w.set_recent_tags(string_model(recent));
}

/// Push the tag list for the currently open detail image into the window.
fn push_detail_tags(w: &MainWindow, tags: &[String]) {
    w.set_detail_tags(ModelRc::new(VecModel::from(
        tags.iter()
            .map(|s| SharedString::from(s.as_str()))
            .collect::<Vec<SharedString>>(),
    )));
    w.set_detail_tags_joined(tags.join(" ").into());
}

/// Reflect the tag-filter slice of `filters` into the window: the tag pills,
/// the joined editor text, the AND/OR mode bool, and the master enable. Called
/// after every tag-filter edit and during restore/fresh-start setup.
fn push_filter_tags(w: &MainWindow, f: &Filters) {
    w.set_filter_tags(string_model(&f.tags));
    w.set_filter_tags_joined(f.tags.join(" ").into());
    w.set_tag_match_and(matches!(f.tag_match, TagMatch::AllOf));
    w.set_tags_enabled(f.tags_enabled);
}

/// Show the rail with empty contents on a fresh DB (no persisted session). The
/// derived `UiState::default().rail_visible` is `false`, so the rail must be
/// turned on explicitly here for first-launch visibility.
fn init_fresh_rail(
    window: &MainWindow,
    brushes: &Arc<Mutex<[Vec<String>; 5]>>,
    mm_buffer: &Arc<Mutex<Vec<String>>>,
) {
    let b = brushes.lock().unwrap();
    let recent = mm_buffer.lock().unwrap();
    push_rail_models(window, &b, &recent);
    window.set_rail_visible(true);
}

/// Persist just the rail-related fields by merging them into the stored
/// [`UiState`]. Reads the current persisted state, overwrites the rail fields,
/// and writes it back, so rail edits survive even though the full session is
/// only persisted on exit. Failures are logged and swallowed.
fn persist_rail(
    backend: &Backend,
    brushes: &Arc<Mutex<[Vec<String>; 5]>>,
    mm_buffer: &Arc<Mutex<Vec<String>>>,
    rail_visible: bool,
) {
    let mut st = backend.get_ui_state().ok().flatten().unwrap_or_default();
    {
        let b = brushes.lock().unwrap();
        st.brushes = std::array::from_fn(|i| imgfind::ui_state::TagBrush { tags: b[i].clone() });
    }
    st.recent_tags = mm_buffer.lock().unwrap().clone();
    st.rail_visible = rail_visible;
    if let Err(e) = backend.set_ui_state(&st) {
        tracing::warn!("Failed to persist rail state: {e:#}");
    }
}

/// Trigger the default startup browse-all in the configured sort order.
///
/// Sets `state.sort` to `sort`, pre-seeds the shared selection to index 0 so
/// [`apply_selection_after_results`] re-clamps to it when results land (selecting
/// the first tile for non-empty DBs), then spawns the browse. Does NOT touch the
/// model or lightbox — the window stays in loading state until results arrive.
///
/// **T20 seam:** T20 should call this in the `else`-branch of its
/// "if persisted UiState exists → restore; else → start_default_browse(…)" gate.
/// The exact call site is just before `window.run()` in `main`.
fn start_default_browse(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    selected_ref: Arc<Mutex<Option<usize>>>,
    sort: Sort,
) {
    // Install sort and mark has_searched=true so view_state() returns Results
    // (not Idle) when results arrive, causing apply_fetch_result to call
    // set_total_items and render the grid. Mirror the pattern used by every
    // other browse/search caller (lines ~318, ~1217).
    {
        let mut s = state_ref.lock().unwrap();
        s.sort = sort;
        s.start_search(String::new());
    }
    // Pre-seed the selection to 0; apply_fetch_result re-clamps it to 0 when
    // results are non-empty (reset=false keeps an in-range index). A freshly
    // opened DB with zero images clears it to None via the filter.
    let selected_for_policy = Arc::clone(&selected_ref);
    *selected_ref.lock().unwrap() = Some(0);
    spawn_browse(
        weak,
        state_ref,
        grid_gen,
        backend,
        Filters::default(),
        SelectionPolicy {
            selected: selected_for_policy,
            after: SelectAfter::KeepIndex,
        },
    );
}

/// Shared-state handles `restore_session` needs to rebuild the UI from a
/// persisted [`UiState`]. Bundled into a struct to stay under clippy's
/// argument limit and keep the single call site readable.
struct RestoreCtx<'a> {
    weak: Weak<MainWindow>,
    state: Arc<Mutex<SearchState>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    selected: Arc<Mutex<Option<usize>>>,
    filters: Arc<Mutex<Filters>>,
    selected_exts: Arc<Mutex<HashSet<String>>>,
    search_mode: Arc<Mutex<SearchMode>>,
    detail: Arc<Mutex<Option<DetailState>>>,
    restoring: Arc<AtomicBool>,
    all_extensions: &'a [String],
    size_bounds: (i64, i64),
    brushes: Arc<Mutex<[Vec<String>; 5]>>,
    mm_buffer: Arc<Mutex<Vec<String>>>,
}

/// Whether a persisted [`PersistedMode`] is a search (text or similarity), as
/// opposed to a plain browse. Search modes get a Relevance option in the sort
/// selector and have a relevance ordering to restore.
fn mode_is_search(mode: &PersistedMode) -> bool {
    matches!(mode, PersistedMode::Text(_) | PersistedMode::Similar(_))
}

/// Index of `sort.key` within the sort-selector options for `mode`.
///
/// In search mode the options are `["Relevance", "Name", "Size", "Type"]`, so
/// concrete keys are offset by one and Relevance (index 0) is the default
/// reflection — `UiState` stores only a concrete `Sort`, so we cannot tell
/// "Relevance was selected" apart from a real key and default to Relevance
/// (the restored display order is faithful either way). In browse mode the
/// options are `["Name", "Size", "Type"]` and the key maps directly.
fn sort_to_selector_index(sort: Sort, mode: &PersistedMode) -> i32 {
    if mode_is_search(mode) {
        0
    } else {
        sort_key_to_browse_index(sort.key)
    }
}

/// Map a [`GpsFilter`] back to the `gps-mode` widget int (inverse of the match
/// in [`build_filters`]): Any=0, HasGps=1, NoGps=2.
fn gps_to_mode(gps: GpsFilter) -> i32 {
    match gps {
        GpsFilter::Any => 0,
        GpsFilter::HasGps => 1,
        GpsFilter::NoGps => 2,
    }
}

/// Invert a byte threshold back to a [0, 1] slider fraction (inverse of
/// [`fraction_to_bytes`]). `None` (unbounded) maps to the matching extreme.
fn bytes_to_fraction(bytes: Option<i64>, min: i64, max: i64, is_lo: bool) -> f32 {
    match bytes {
        None => {
            if is_lo {
                0.0
            } else {
                1.0
            }
        }
        Some(b) => {
            if max <= min {
                if is_lo { 0.0 } else { 1.0 }
            } else {
                (((b - min) as f64) / ((max - min) as f64)).clamp(0.0, 1.0) as f32
            }
        }
    }
}

/// Restore the GUI from a persisted [`UiState`] on the main thread, BEFORE the
/// event loop runs. No query is executed: the result list is rehydrated by id
/// (display order preserved) and installed directly, then window properties are
/// set so the loader timer fills thumbnails at the restored scroll position once
/// `run()` starts. Mirrors the install path of `apply_fetch_result` minus the
/// background fetch. Restore side effects can never trigger a re-query: the
/// `restoring` flag is held for the duration and the `search` callback honors it.
fn restore_session(st: UiState, ctx: RestoreCtx<'_>) {
    ctx.restoring.store(true, Ordering::SeqCst);

    let Some(w) = ctx.weak.upgrade() else {
        ctx.restoring.store(false, Ordering::SeqCst);
        return;
    };

    // 1. Result list — rehydrate by id (verbatim display order), install into
    //    SearchState as a completed "search" so view_state() == Results.
    let rows = ctx.backend.rehydrate(&st.result_ids).unwrap_or_default();
    let results_len = rows.len();
    let is_search = mode_is_search(&st.mode);
    {
        let mut s = ctx.state.lock().unwrap();
        s.start_search(String::new());
        s.sort = st.sort;
        if is_search {
            s.relevance_order = rows.clone();
        }
        s.apply_results(rows);
    }

    // Restore the search-mode holder so the filter-bar debounce dispatches the
    // right backend method on the user's next filter change.
    let restored_mode = match &st.mode {
        PersistedMode::Browse => SearchMode::Text(String::new()),
        PersistedMode::Text(q) => SearchMode::Text(q.clone()),
        PersistedMode::Similar(seed_id) => {
            // Resolve the seed id back to its relative path (the holder stores a
            // path). If the row is gone, fall back to browse mode.
            match ctx.backend.rehydrate(&[*seed_id]).ok().and_then(|mut v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0).path)
                }
            }) {
                Some(path) => SearchMode::Similar(path),
                None => SearchMode::Text(String::new()),
            }
        }
    };
    *ctx.search_mode.lock().unwrap() = restored_mode;

    // Install the grid: total-items spans the full set, clearing tiles + bumping
    // the generation makes the loader timer rebuild the visible window.
    w.set_total_items(results_len as i32);
    w.set_tiles(ModelRc::default());
    loader::bump_generation(&ctx.grid_gen);

    // 2. Search text — programmatic set does NOT fire `search` (only `accepted`
    //    on Enter does), and `restoring` guards the callback regardless.
    w.set_query_text(st.search_text.clone().into());

    // 3. Selection — clamp the stored index to the restored length.
    let sel = st.selected_index.filter(|&i| i < results_len);
    *ctx.selected.lock().unwrap() = sel;
    w.set_selected_index(sel.map(|i| i as i32).unwrap_or(-1));

    // 4. Scroll — the stored value is the raw (<= 0) viewport-y; the loader timer
    //    then loads the window at that offset.
    w.set_grid_viewport_y(st.scroll_y);

    // 5. Sort selector — Relevance present iff search mode; index reflects the
    //    stored key (Relevance default for search), direction from the dir.
    w.set_sort_options(make_sort_options_model(is_search));
    w.set_sort_index(sort_to_selector_index(st.sort, &st.mode));
    w.set_sort_desc(matches!(st.sort.dir, SortDir::Desc));

    // 7. Filters (best-effort cosmetic sync; the result list already reflects
    //    them since it is the persisted id set). Set the shared holders and the
    //    filter widgets back to the persisted state.
    {
        let exts: HashSet<String> = st.filters.extensions.iter().cloned().collect();
        *ctx.filters.lock().unwrap() = st.filters.clone();
        *ctx.selected_exts.lock().unwrap() = exts.clone();
        let lo = bytes_to_fraction(
            st.filters.size_min,
            ctx.size_bounds.0,
            ctx.size_bounds.1,
            true,
        );
        let hi = bytes_to_fraction(
            st.filters.size_max,
            ctx.size_bounds.0,
            ctx.size_bounds.1,
            false,
        );
        w.set_size_lo(lo);
        w.set_size_hi(hi);
        w.set_size_label(build_size_label(lo, hi, ctx.size_bounds));
        w.set_type_chips(build_chips_model(ctx.all_extensions, &exts));
        w.set_gps_mode(gps_to_mode(st.filters.gps));
        // Reflect the persisted tag filter into the UI under the `restoring`
        // guard (no query fired); the result list already reflects these tags.
        push_filter_tags(&w, &st.filters);
    }

    // 6. Detail panel — open it for the selected image (best-effort). Mirror
    //    on_tile_selected: show the panel immediately with a placeholder, then
    //    load the 512 thumb + metadata on a background thread.
    if st.detail_open
        && let Some(idx) = sel
    {
        let path = {
            let s = ctx.state.lock().unwrap();
            s.results.get(idx).map(|r| r.path.clone())
        };
        if let Some(path) = path {
            let ds = select(path.clone());
            *ctx.detail.lock().unwrap() = Some(ds.clone());
            let cached = detail_cache::get(&path);
            w.set_detail_open(true);
            w.set_detail_filename(ds.filename.into());
            match &cached {
                Some(img) => w.set_detail_image(img.clone()),
                None => w.set_detail_image(Default::default()),
            }
            w.set_detail_meta("Loading\u{2026}".into());

            let path_for_tags = path.clone();
            spawn_detail_image(ctx.weak.clone(), ctx.backend.clone(), path.clone(), cached);
            spawn_detail_meta(ctx.weak.clone(), ctx.backend.clone(), path.clone());

            // Load tags for the restored detail panel.
            {
                let backend3 = ctx.backend.clone();
                let weak3 = ctx.weak.clone();
                std::thread::spawn(move || {
                    let tags = backend3.tags_for(&path_for_tags).unwrap_or_default();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = weak3.upgrade() {
                            push_detail_tags(&w, &tags);
                        }
                    });
                });
            }
        }
    }

    // 8. Left rail — load the brushes/recent buffer from the persisted state,
    //    push the models, and restore rail visibility.
    {
        let mut b = ctx.brushes.lock().unwrap();
        for (i, slot) in b.iter_mut().enumerate() {
            *slot = st.brushes[i].tags.clone();
        }
        let mut recent = ctx.mm_buffer.lock().unwrap();
        *recent = st.recent_tags.clone();
        push_rail_models(&w, &b, &recent);
    }
    w.set_rail_visible(st.rail_visible);

    ctx.restoring.store(false, Ordering::SeqCst);
}

/// Persist the current session to the DB after the event loop returns. Reads
/// the live window scroll position plus the shared state holders. Any failure
/// is logged and swallowed so it can never crash process exit.
#[allow(clippy::too_many_arguments)]
fn persist_session(
    window: &MainWindow,
    state: &Arc<Mutex<SearchState>>,
    filters: &Arc<Mutex<Filters>>,
    selected: &Arc<Mutex<Option<usize>>>,
    detail: &Arc<Mutex<Option<DetailState>>>,
    search_mode: &Arc<Mutex<SearchMode>>,
    backend: &Backend,
    brushes: &Arc<Mutex<[Vec<String>; 5]>>,
    mm_buffer: &Arc<Mutex<Vec<String>>>,
) {
    let (result_ids, sort) = {
        let s = state.lock().unwrap();
        (s.results.iter().map(|r| r.id).collect::<Vec<i64>>(), s.sort)
    };
    let search_text;
    let mode = {
        let m = search_mode.lock().unwrap().clone();
        match m {
            SearchMode::Text(q) if q.is_empty() => {
                search_text = String::new();
                PersistedMode::Browse
            }
            SearchMode::Text(q) => {
                search_text = q.clone();
                PersistedMode::Text(q)
            }
            SearchMode::Similar(seed_path) => {
                search_text = String::new();
                // Resolve the seed path back to its id; on failure fall back to
                // Browse (the result list restores either way).
                match backend.id_for_rel_path(&seed_path) {
                    Ok(id) => PersistedMode::Similar(id),
                    Err(e) => {
                        tracing::warn!("could not resolve similar-seed id, persisting Browse: {e}");
                        PersistedMode::Browse
                    }
                }
            }
        }
    };

    let st = UiState {
        search_text,
        mode,
        sort,
        filters: filters.lock().unwrap().clone(),
        result_ids,
        selected_index: *selected.lock().unwrap(),
        detail_open: detail.lock().unwrap().is_some(),
        scroll_y: window.get_grid_viewport_y(),
        brushes: {
            let b = brushes.lock().unwrap();
            std::array::from_fn(|i| imgfind::ui_state::TagBrush { tags: b[i].clone() })
        },
        recent_tags: mm_buffer.lock().unwrap().clone(),
        rail_visible: window.get_rail_visible(),
    };

    if let Err(e) = backend.set_ui_state(&st) {
        tracing::warn!("Failed to persist session: {e:#}");
    }
}

/// Restart the debounce timer with a fresh 250 ms single-shot callback.
///
/// Each filter-change event calls this function, which replaces any pending
/// timer fire with a new one carrying the *latest* filter snapshot.  Because
/// `Timer::start` accepts `FnMut + 'static` and we cannot move non-`Copy`
/// values out of an outer `FnMut` callback, we allocate a new closure per
/// restart — which is fine for a 250 ms debounce rate.
#[allow(clippy::too_many_arguments)]
fn start_debounce(
    timer: &Timer,
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    mode_ref: Arc<Mutex<SearchMode>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    filters: Filters,
    selected_ref: Arc<Mutex<Option<usize>>>,
) {
    timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(250),
        move || {
            fire_debounced_query(
                weak.clone(),
                Arc::clone(&state_ref),
                Arc::clone(&mode_ref),
                Arc::clone(&grid_gen),
                backend.clone(),
                filters.clone(),
                Arc::clone(&selected_ref),
            );
        },
    );
}

/// Apply a sort change, using `after` to control post-sort selection.
///
/// Called by both `on_sort_changed` (key change → `SelectAfter::First`) and
/// `on_sort_dir_toggled` (direction flip → `SelectAfter::ById(prev_id)`).
#[allow(clippy::too_many_arguments)]
fn apply_sort_change(
    weak: Weak<MainWindow>,
    mode_ref: Arc<Mutex<SearchMode>>,
    state_ref: Arc<Mutex<SearchState>>,
    filters_ref: Arc<Mutex<Filters>>,
    selected_ref: Arc<Mutex<Option<usize>>>,
    grid_gen_ref: Arc<AtomicU64>,
    backend: Backend,
    after: SelectAfter,
) {
    let Some(w) = weak.upgrade() else { return };
    let idx = w.get_sort_index() as usize;
    let desc = w.get_sort_desc();
    let option_str: String = {
        let model = w.get_sort_options();
        model
            .row_data(idx)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "Name".to_string())
    };
    let dir = if desc { SortDir::Desc } else { SortDir::Asc };
    let mode = mode_ref.lock().unwrap().clone();
    match mode {
        SearchMode::Text(q) if q.is_empty() => {
            // Browse mode: re-issue the browse query with the new sort.
            let sort = Sort {
                key: option_str_to_sort_key(&option_str),
                dir,
            };
            state_ref.lock().unwrap().sort = sort;
            let current_filters = filters_ref.lock().unwrap().clone();
            spawn_browse(
                weak,
                state_ref,
                grid_gen_ref,
                backend,
                current_filters,
                SelectionPolicy {
                    selected: selected_ref,
                    after,
                },
            );
        }
        _ => {
            // Search/similar mode: in-memory resort (no backend round-trip).
            {
                let mut s = state_ref.lock().unwrap();
                if option_str == "Relevance" {
                    s.resort_to_relevance();
                    // Keep sort field as default placeholder — Relevance has
                    // no SortKey equivalent; browse after a clear will reset.
                    s.sort = Sort::default();
                } else {
                    let sort = Sort {
                        key: option_str_to_sort_key(&option_str),
                        dir,
                    };
                    s.resort(&sort);
                }
            }
            // Bump generation so the loader timer rebuilds the tile model.
            loader::bump_generation(&grid_gen_ref);
            let results = state_ref.lock().unwrap().results.clone();
            apply_selection_after_results(&w, &selected_ref, &results, &after);
        }
    }
}

/// Called from all three debounce closures after rebuilding `Filters`.
///
/// Reads `search_mode` and either runs a text search, similarity search, or
/// browse, starting from offset 0.  The filter change is treated as a fresh
/// search (reset for text/browse, re-clamp for similar).
#[allow(clippy::too_many_arguments)]
fn fire_debounced_query(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    mode_ref: Arc<Mutex<SearchMode>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    filters: Filters,
    selected_ref: Arc<Mutex<Option<usize>>>,
) {
    let mode = mode_ref.lock().unwrap().clone();
    match mode {
        SearchMode::Text(query) if !query.is_empty() => {
            state_ref.lock().unwrap().start_search(query.clone());
            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_can_search(false);
            }
            spawn_search(
                weak,
                state_ref,
                grid_gen,
                backend,
                query,
                filters,
                SelectionPolicy {
                    selected: selected_ref,
                    after: SelectAfter::Clear,
                },
            );
        }
        // Text("") = browse mode (or idle — browse with filters anyway).
        // Text("") with default filters — restore idle state, nothing to browse.
        SearchMode::Text(_) if filters == Filters::default() => {
            *selected_ref.lock().unwrap() = None;
            // Bump the generation so the loader timer resets and stops loading.
            loader::bump_generation(&grid_gen);
            if let Some(w) = weak.upgrade() {
                w.set_status("Enter a search query to find images.".into());
                w.set_tiles(ModelRc::default());
                w.set_total_items(0);
                w.set_selected_index(-1);
            }
        }
        // Text("") with active filters — browse with filters.
        SearchMode::Text(_) => {
            state_ref.lock().unwrap().start_search(String::new());
            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_can_search(false);
            }
            spawn_browse(
                weak,
                state_ref,
                grid_gen,
                backend,
                filters,
                SelectionPolicy {
                    selected: selected_ref,
                    after: SelectAfter::Clear,
                },
            );
        }
        SearchMode::Similar(seed) => {
            state_ref.lock().unwrap().start_search(seed.clone());
            if let Some(w) = weak.upgrade() {
                let filename = filename_of(&seed);
                w.set_status(format!("Similar to {filename}").into());
                w.set_can_search(false);
            }
            spawn_similar(
                weak,
                state_ref,
                grid_gen,
                backend,
                seed,
                filters,
                SelectionPolicy {
                    selected: selected_ref,
                    after: SelectAfter::KeepIndex,
                },
            );
        }
    }
}

/// Build the tile model for one render window from already-decoded images.
///
/// `results` is the window slice; `images[i]` is the decoded thumbnail for
/// `results[i]` (looked up from the LRU by the caller), or `None` for a tile
/// whose thumbnail has not yet been decoded — those render with a placeholder
/// image until the loader fills them in. `offset` is the global index of
/// `results[0]` in the full result list, used to set each `Tile.index` for
/// correct absolute positioning in the virtualized grid.
fn build_tiles_model(
    results: &[RowMeta],
    images: Vec<Option<Image>>,
    offset: usize,
) -> ModelRc<Tile> {
    let tiles: Vec<Tile> = results
        .iter()
        .zip(images)
        .enumerate()
        .map(|(i, (r, maybe_img))| {
            let size_kb = r.size.unwrap_or(0) / 1024;
            Tile {
                path: r.path.clone().into(),
                image: maybe_img.unwrap_or_default(),
                size_kb: size_kb as i32,
                // Global index: window offset + position within this slice.
                index: (offset + i) as i32,
            }
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(tiles)))
}

/// Borrowed bundle of the loader timer's UI-thread state, passed to
/// [`loader_tick`] once per tick. Grouping these into a struct keeps the timer
/// closure tidy and the tick function under clippy's argument limit.
struct LoaderTick<'a> {
    window: &'a MainWindow,
    state_ref: &'a Arc<Mutex<SearchState>>,
    grid_gen: &'a Arc<AtomicU64>,
    req_tx: &'a mpsc::Sender<loader::ThumbRequest>,
    resp_rx: &'a mpsc::Receiver<loader::ThumbResponse>,
    cache: &'a mut loader::ThumbCache,
    in_flight: &'a mut loader::InFlight,
    last_range: &'a mut Option<Range<usize>>,
    last_gen: &'a mut u64,
}

/// One tick of the moving-window thumbnail loader, on the UI thread.
///
/// 1. Drain the worker's response channel, decoding current-generation bytes
///    into the LRU (stale-generation responses are dropped).
/// 2. Compute the visible item range from the live scroll geometry.
/// 3. Rebuild the tile model only when the range or generation changed.
/// 4. Request any visible thumbnails not yet cached or in-flight.
///
/// Cheap when idle: an empty channel plus an unchanged range/generation does no
/// work beyond the geometry read.
fn loader_tick(t: LoaderTick<'_>) {
    let current_gen = loader::current_generation(t.grid_gen);

    // A new result set was installed since the last tick: reset window state so
    // the grid rebuilds from the top with this generation's thumbnails.
    let gen_changed = *t.last_gen != current_gen;
    if gen_changed {
        *t.last_gen = current_gen;
        *t.last_range = None;
        t.in_flight.clear();
    }

    // 1. Drain decoded bytes into the cache (drop stale generations). Track
    // whether any current-gen thumbnail landed, so an unchanged window still
    // refreshes to show progressive loads.
    let mut cached_new = false;
    while let Ok((generation, path, result)) = t.resp_rx.try_recv() {
        if generation != current_gen {
            continue;
        }
        t.in_flight.remove(&path);
        match result {
            Ok(bytes) => match loader::decode_thumb_bytes(&bytes) {
                Some(img) => {
                    t.cache.put(path, img);
                    cached_new = true;
                }
                None => tracing::warn!("thumb decode failed for {path}"),
            },
            Err(e) => tracing::warn!("thumbnail fetch failed for {path}: {e}"),
        }
    }

    // 2. Visible window from scroll geometry. viewport-y is <= 0 (more negative
    // scrolling down), so negate it for the pixels-from-top.
    let cols = t.window.get_cols().max(0) as usize;
    let viewport_h = t.window.get_grid_viewport_h();
    let scroll_px = (-t.window.get_grid_viewport_y()).max(0.0);
    let total = t.state_ref.lock().unwrap().results.len();
    let (first_row, last_row) = window::visible_rows(scroll_px, viewport_h, window::TILE_PITCH_Y);
    let range = window::window_range(first_row, last_row, cols, total);

    // 3. Rebuild only when the window/generation changed, or when a freshly
    // decoded thumbnail in the current window needs to appear. Otherwise the
    // tick does no UI work (cheap when idle).
    let range_changed = t.last_range.as_ref() != Some(&range);
    if range_changed || gen_changed || cached_new {
        rebuild_window(t.window, t.state_ref, t.cache, &range);
        if range_changed || gen_changed {
            tracing::debug!(
                start = range.start,
                end = range.end,
                "loader: window changed"
            );
        }
        *t.last_range = Some(range.clone());
    }

    // 4. Request missing thumbnails for the visible window.
    let paths: Vec<String> = {
        let s = t.state_ref.lock().unwrap();
        s.results
            .get(range.clone())
            .map(|slice| slice.iter().map(|r| r.path.clone()).collect())
            .unwrap_or_default()
    };
    for path in paths {
        if t.cache.contains(&path) || t.in_flight.contains(&path) {
            continue;
        }
        t.in_flight.insert(&path);
        // A send error means the worker has gone away (shutdown); ignore it.
        if t.req_tx.send((current_gen, path)).is_err() {
            break;
        }
    }
}

/// Build and install the tile model for `range`, taking each tile's image from
/// the LRU cache (placeholder when absent). `range.start` becomes the tile
/// offset so each tile's global `index` positions it correctly in the grid.
fn rebuild_window(
    window: &MainWindow,
    state_ref: &Arc<Mutex<SearchState>>,
    cache: &mut loader::ThumbCache,
    range: &Range<usize>,
) {
    let slice: Vec<RowMeta> = {
        let s = state_ref.lock().unwrap();
        s.results
            .get(range.clone())
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    };
    let images: Vec<Option<Image>> = slice.iter().map(|r| cache.get(&r.path).cloned()).collect();
    let model = build_tiles_model(&slice, images, range.start);
    window.set_tiles(model);
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

/// Marshal a fetched result set back onto the UI thread: install it into the
/// state, bump the grid generation, set `total-items`, update selection and
/// status. The tile grid itself is NOT built here — the loader timer renders
/// only the visible window once it observes the new generation.
///
/// Bumping `grid_generation` invalidates any in-flight thumbnail responses from
/// the previous result set and signals the loader timer to reset its in-flight
/// set and last-rendered range, rebuilding from the top on its next tick.
fn apply_fetch_result(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    grid_gen: Arc<AtomicU64>,
    res: Result<Vec<RowMeta>>,
    sel: SelectionPolicy,
    is_search: bool,
    // Whether the CLIP model was ready at the moment the background thread
    // finished. For text/similar searches the model is always ready (can_search
    // was false until it became ready). For a startup browse that races the
    // model load, this may be false — in that case leave can_search=false so
    // the model_timer transition is the sole owner of that flag.
    model_ready: bool,
) {
    slint::invoke_from_event_loop(move || {
        let Some(w) = weak.upgrade() else { return };

        let (vs, error_msg, results) = {
            let mut s = state_ref.lock().unwrap();
            match res {
                Ok(rows) => {
                    if is_search {
                        s.relevance_order = rows.clone();
                    }
                    s.apply_results(rows);
                }
                Err(e) => s.apply_error(e.to_string()),
            }
            (s.view_state(), s.error.clone(), s.results.clone())
        };

        // A new result set is installed: bump the generation and clear the
        // visible grid so the loader timer rebuilds the window from the top.
        // `total-items` is always the full result count so the scrollbar spans
        // the complete library even though only a window of tiles is rendered.
        loader::bump_generation(&grid_gen);
        if matches!(vs, ViewState::Results | ViewState::Empty) {
            w.set_tiles(ModelRc::default());
            w.set_total_items(results.len() as i32);
        }

        apply_selection_after_results(&w, &sel.selected, &results, &sel.after);

        w.set_status(status_text_for(vs, error_msg.as_deref()));
        // Only unlock the search box if the CLIP model is already ready.
        // For a startup browse that races the model load, the model_timer
        // is the sole owner of the false→true transition.
        if model_ready {
            w.set_can_search(true);
        }

        // Sync sort selector: search/similar results offer Relevance (default);
        // browse results keep the existing sort options unchanged.
        if is_search && matches!(vs, ViewState::Results | ViewState::Empty) {
            w.set_sort_options(make_sort_options_model(true));
            w.set_sort_index(0); // Relevance selected by default
            w.set_sort_desc(false);
            state_ref.lock().unwrap().sort = Sort::default();
        }
    })
    .ok();
}

/// Spawn a background text-search thread and marshal results back to the UI.
///
/// Only the DB query runs on the worker thread; thumbnails are loaded lazily by
/// the loader timer once results are installed. `sel` controls how the keyboard
/// selection is updated when results land.
fn spawn_search(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    query: String,
    filters: Filters,
    sel: SelectionPolicy,
) {
    std::thread::spawn(move || {
        let res = backend.search(&query, &filters);
        // Text search requires the model to be ready (can_search was false
        // until the model became ready), so model_ready is always true here.
        apply_fetch_result(weak, state_ref, grid_gen, res, sel, true, true);
    });
}

/// Like `spawn_search` but calls the vector-similarity backend.
///
/// The detail panel stays open (callers keep `detail-open` true); this
/// function only updates the tile grid. For similarity searches the seed
/// remains highlighted, so callers should set `sel.reset = false`.
fn spawn_similar(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    seed_path: String,
    filters: Filters,
    sel: SelectionPolicy,
) {
    std::thread::spawn(move || {
        let res = backend.search_similar(&seed_path, &filters);
        // Similarity search uses stored embeddings (no text model needed), but
        // the UI only allows it after the model is ready, so model_ready=true.
        apply_fetch_result(weak, state_ref, grid_gen, res, sel, true, true);
    });
}

/// Browse the full filtered set in the current sort order (no paging).
///
/// Used when the query box is empty but filters are active, and for the
/// filter-bar debounce when `search_mode` is idle/browse.
/// `sel` controls how the keyboard selection is updated when results land.
fn spawn_browse(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    grid_gen: Arc<AtomicU64>,
    backend: Backend,
    filters: Filters,
    sel: SelectionPolicy,
) {
    std::thread::spawn(move || {
        let sort = state_ref.lock().unwrap().sort;
        let res = backend.browse(&filters, &sort);
        // Check model readiness just before marshalling back to the UI thread.
        // For the startup browse this races the model load; the model_timer owns
        // the false→true transition when the model isn't ready yet.
        let model_ready = backend.model_ready();
        apply_fetch_result(weak, state_ref, grid_gen, res, sel, false, model_ready);
    });
}

/// Update the shared grid selection and the Slint `selected-index` property
/// after a page of results is applied.
///
/// `after` controls the selection strategy; see [`SelectAfter`].
fn apply_selection_after_results(
    w: &MainWindow,
    selected_ref: &Arc<Mutex<Option<usize>>>,
    results: &[RowMeta],
    after: &SelectAfter,
) {
    let prev = *selected_ref.lock().unwrap();
    let new_sel = resolve_selection(after, results, prev);
    *selected_ref.lock().unwrap() = new_sel;
    w.set_selected_index(new_sel.map(|i| i as i32).unwrap_or(-1));
}

/// Relative path of the currently-focused image, in priority order: the
/// lightbox image if the lightbox is open, else the detail-panel image if the
/// panel is open, else the selected grid tile. `None` when nothing is focused —
/// tag-paint actions are then no-ops.
fn focused_path(
    detail: &Arc<Mutex<Option<DetailState>>>,
    selected: &Arc<Mutex<Option<usize>>>,
    lb_index: &Arc<Mutex<Option<usize>>>,
    state: &Arc<Mutex<SearchState>>,
) -> Option<String> {
    // Copy the index out before locking `state` so we never hold two locks at
    // once in an order that could deadlock against another path.
    let lb = *lb_index.lock().unwrap();
    if let Some(i) = lb {
        return state.lock().unwrap().results.get(i).map(|r| r.path.clone());
    }
    let detail_path = detail.lock().unwrap().as_ref().map(|d| d.path.clone());
    if let Some(p) = detail_path {
        return Some(p);
    }
    let sel = (*selected.lock().unwrap())?;
    state
        .lock()
        .unwrap()
        .results
        .get(sel)
        .map(|r| r.path.clone())
}

/// Holders needed to resolve the focused image and refresh the detail panel
/// after a tag paint. Bundled to keep [`apply_tags_to_focused`] under clippy's
/// argument limit and the chord call sites readable.
struct TagTargetCtx {
    detail: Arc<Mutex<Option<DetailState>>>,
    selected: Arc<Mutex<Option<usize>>>,
    lb_index: Arc<Mutex<Option<usize>>>,
    state: Arc<Mutex<SearchState>>,
}

/// Apply `tags` to the currently-focused image (see [`focused_path`]).
///
/// No-op when nothing is focused or `tags` is empty. The `Backend::add_tag`
/// writes run on a background thread (never on the UI thread); if the detail
/// panel is currently showing the same image, its tag list is re-fetched and
/// pushed back on the UI thread so the pills update live.
fn apply_tags_to_focused(
    weak: &Weak<MainWindow>,
    backend: &Backend,
    ctx: &TagTargetCtx,
    tags: Vec<String>,
) {
    if tags.is_empty() {
        return;
    }
    let Some(path) = focused_path(&ctx.detail, &ctx.selected, &ctx.lb_index, &ctx.state) else {
        return;
    };
    // Whether the open detail panel is showing this image — decides if we need to
    // re-push its tag pills after the writes land.
    let detail_shows_path = ctx
        .detail
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|d| d.path == path);
    let backend = backend.clone();
    let weak = weak.clone();
    std::thread::spawn(move || {
        for t in &tags {
            if let Err(e) = backend.add_tag(&path, t) {
                tracing::warn!("failed to add tag {t} to {path}: {e}");
            }
        }
        if detail_shows_path {
            let fresh = backend.tags_for(&path).unwrap_or_default();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = weak.upgrade() {
                    push_detail_tags(&w, &fresh);
                }
            });
        }
    });
}

/// Paths of the `n` neighbors around `center` in arc order (closest first),
/// skipping the center itself (which is already being loaded by the display path).
///
/// Returns an empty vec when `results` is empty or `n` is 0.
fn neighbor_paths(results: &[RowMeta], center: usize, n: usize) -> Vec<String> {
    if n == 0 || results.is_empty() {
        return Vec::new();
    }
    // preload_arc returns [center, center+1, center-1, …]; skip index 0 (the focus).
    preload::preload_arc(center, n, results.len())
        .into_iter()
        .skip(1)
        .map(|idx| results[idx].path.clone())
        .collect()
}

/// Spawn a background thread that warms the DB thumbnail cache for each path
/// in `paths` (already in arc order: closest neighbor first).
///
/// The thread stops as soon as it detects that `preload_generation` has advanced
/// beyond `my_gen`, so stale dispatches from a previous focus do not waste I/O.
/// A single thread serializes the SQLite writes and naturally honors the arc order.
fn spawn_preload(
    backend: Backend,
    paths: Vec<String>,
    size: u32,
    preload_generation: Arc<AtomicU64>,
    my_gen: u64,
) {
    if paths.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for path in paths {
            if !is_current_generation(my_gen, preload_generation.load(Ordering::SeqCst)) {
                break;
            }
            if let Err(e) = backend.thumbnail(&path, size) {
                tracing::debug!("preload: thumbnail failed for {path}: {e}");
            }
        }
    });
}

/// 512px is the detail-panel preview size (matches `GUI_THUMBNAIL_SIZES`).
const DETAIL_SIZE: u32 = 512;

/// Load the detail-panel preview image for `path`, setting ONLY `detail_image`
/// (never blocked on metadata).
///
/// On a cache hit (`cached` is `Some`) the image was already applied
/// synchronously by the caller, so this is a no-op. On a miss it spawns a
/// background thread that fetches the 512px JPEG bytes, then posts an
/// `invoke_from_event_loop` that — on the UI thread — decodes the bytes to a
/// `slint::Image`, inserts it into the thread-local detail cache, and sets
/// `detail_image`. The cache and `Image` never leave the UI thread.
fn spawn_detail_image(
    weak: Weak<MainWindow>,
    backend: Backend,
    path: String,
    cached: Option<Image>,
) {
    if cached.is_some() {
        return; // Already displayed synchronously; nothing to fetch.
    }
    std::thread::spawn(move || {
        let thumb_result = backend.thumbnail(&path, DETAIL_SIZE);
        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            match thumb_result {
                Ok(bytes) => match image_util::jpeg_to_slint_image(&bytes) {
                    Ok(img) => {
                        detail_cache::insert(path, img.clone());
                        w.set_detail_image(img);
                    }
                    Err(e) => tracing::warn!("detail thumb decode failed: {e}"),
                },
                Err(e) => tracing::warn!("detail thumbnail failed: {e}"),
            }
        })
        .ok();
    });
}

/// Load the detail-panel metadata for `path` on a background thread and post it
/// to the UI thread, setting ONLY `detail_meta`. Decoupled from the image so a
/// slow metadata read can never delay the cheap cached preview.
fn spawn_detail_meta(weak: Weak<MainWindow>, backend: Backend, path: String) {
    std::thread::spawn(move || {
        let meta_result = backend.metadata(&path);
        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
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
}

/// Preload detail-panel neighbors: fetch each 512px JPEG on a background thread
/// (warming the DB blob cache) and decode + insert it into the thread-local
/// detail cache on the UI thread, WITHOUT changing the visible `detail_image`.
///
/// `paths` is in arc order (closest neighbor first). The background thread stops
/// once `preload_generation` advances past `my_gen`, and each UI-thread decode
/// re-checks the generation so a stale focus's neighbors don't pollute the cache.
fn spawn_detail_preload(
    weak: Weak<MainWindow>,
    backend: Backend,
    paths: Vec<String>,
    preload_generation: Arc<AtomicU64>,
    my_gen: u64,
) {
    if paths.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for path in paths {
            if !is_current_generation(my_gen, preload_generation.load(Ordering::SeqCst)) {
                break;
            }
            let bytes = match backend.thumbnail(&path, DETAIL_SIZE) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!("detail preload: thumbnail failed for {path}: {e}");
                    continue;
                }
            };
            let weak = weak.clone();
            let gen_check = Arc::clone(&preload_generation);
            slint::invoke_from_event_loop(move || {
                // Drop stale decodes; only warm the cache for the current focus.
                if !is_current_generation(my_gen, gen_check.load(Ordering::SeqCst)) {
                    return;
                }
                if weak.upgrade().is_none() {
                    return;
                }
                match image_util::jpeg_to_slint_image(&bytes) {
                    Ok(img) => detail_cache::insert(path, img),
                    Err(e) => tracing::debug!("detail preload decode failed: {e}"),
                }
            })
            .ok();
        }
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

/// True when a background lightbox decode should still be applied: its captured
/// generation must equal the latest generation requested. A stale (superseded) load
/// is dropped so a slow decode can't overwrite an image the user has navigated to.
fn is_current_generation(captured: u64, latest: u64) -> bool {
    captured == latest
}

/// Load the lightbox image for `rel_path` on a background thread, then set the
/// lightbox image and open the overlay on the UI thread.
///
/// Fetches (or generates and persists) the `LIGHTBOX_SIZE`-pixel cached thumbnail
/// via the backend, so RAW files are demosaiced at most once per image — subsequent
/// opens of the same image are served from the DB thumbnail cache.
///
/// The thumbnail bytes are decoded to `DynamicImage` on the background thread so a
/// slow first-time decode never freezes the UI. `slint::Image` is non-Send, so it
/// is constructed inside the `invoke_from_event_loop` closure on the UI thread.
///
/// `generation` is a shared monotonic counter: this call claims the next value and
/// only applies its result if no newer lightbox load has been requested in the
/// meantime (latest-wins), preventing a slow decode from landing after the user has
/// already navigated to another image.
fn load_lightbox_image(
    weak: Weak<MainWindow>,
    backend: Backend,
    rel_path: String,
    generation: Arc<AtomicU64>,
) {
    let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        // Fetch (or generate+persist) the 2048-pixel thumbnail — on this background
        // thread so a slow first-time RAW demosaic never freezes the UI.
        let bytes = match backend.thumbnail(&rel_path, imgfind::thumbnail::LIGHTBOX_SIZE) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Lightbox: failed to load thumbnail for {rel_path}: {e}");
                return;
            }
        };

        // Decode JPEG bytes → DynamicImage here (still off the UI thread).
        // `DynamicImage` is Send; move it to the UI thread and build the (!Send) Image there.
        let img = match image::load_from_memory(&bytes) {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!("Lightbox: failed to decode thumbnail bytes for {rel_path}: {e}");
                return;
            }
        };

        slint::invoke_from_event_loop(move || {
            // Drop this result if a newer lightbox load superseded it while we decoded.
            if !is_current_generation(my_gen, generation.load(Ordering::SeqCst)) {
                return;
            }
            let Some(w) = weak.upgrade() else { return };
            let slint_img = image_util::dynamic_to_slint_image(&img);
            w.set_lightbox_image(slint_img);
            w.set_lightbox_open(true);
        })
        .ok();
    });
}

/// Build a `ModelRc<SharedString>` for the sort selector.
///
/// When `with_relevance` is `true` (search/similar results), "Relevance" is
/// prepended as the first option so it is selected by default.  For browse mode
/// it is omitted because browse results have no relevance ranking.
fn make_sort_options_model(with_relevance: bool) -> ModelRc<SharedString> {
    let mut opts: Vec<SharedString> = Vec::new();
    if with_relevance {
        opts.push("Relevance".into());
    }
    opts.push("Name".into());
    opts.push("Size".into());
    opts.push("Type".into());
    ModelRc::from(Rc::new(VecModel::from(opts)))
}

/// Map a sort-selector option string to a [`SortKey`].
///
/// "Relevance" is handled by the caller before this point; any other unknown
/// string falls back to `SortKey::Name`.
fn option_str_to_sort_key(s: &str) -> SortKey {
    match s {
        "Size" => SortKey::Size,
        "Type" => SortKey::Type,
        _ => SortKey::Name,
    }
}

#[cfg(test)]
mod arg_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn dir_accepts_both_short_and_long() {
        assert_eq!(
            Args::try_parse_from(["imgfind-gui", "-d", "/x"])
                .unwrap()
                .dir,
            Some("/x".to_string())
        );
        assert_eq!(
            Args::try_parse_from(["imgfind-gui", "--dir", "/x"])
                .unwrap()
                .dir,
            Some("/x".to_string())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_size_label, clamp_next, clamp_prev, format_bytes, format_statusline,
        fraction_to_bytes, is_current_generation,
    };
    use imgfind::sort::RowMeta;

    #[test]
    fn latest_generation_applies_stale_is_dropped() {
        assert!(is_current_generation(3, 3)); // newest request -> apply
        assert!(!is_current_generation(2, 3)); // superseded by a newer request -> drop
        assert!(!is_current_generation(1, 5));
    }

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

    // --- format_statusline ---

    #[test]
    fn statusline_normal_shows_count_and_total() {
        use crate::selection::Selection;
        let rows = vec![
            RowMeta { id: 1, path: "a".into(), size: Some(1_000_000), ext: "jpg".into() },
            RowMeta { id: 2, path: "b".into(), size: Some(1_000_000), ext: "jpg".into() },
        ];
        let s = Selection::default();
        let line = format_statusline(&s, &rows);
        assert!(line.starts_with("NORMAL"), "got: {line}");
        assert!(line.contains("2 images"), "got: {line}");
        assert!(line.contains("MB"), "got: {line}");
        assert!(!line.contains("selected"), "got: {line}");
    }

    #[test]
    fn statusline_none_size_counts_as_zero() {
        use crate::selection::Selection;
        let rows = vec![
            RowMeta { id: 1, path: "a".into(), size: None, ext: "jpg".into() },
        ];
        let s = Selection::default();
        let line = format_statusline(&s, &rows);
        assert!(line.contains("1 images"), "got: {line}");
        assert!(line.contains("0 B"), "got: {line}");
    }

    #[test]
    fn statusline_free_shows_selection_stats() {
        use crate::selection::Selection;
        let rows = vec![
            RowMeta { id: 1, path: "a".into(), size: Some(2_000_000), ext: "jpg".into() },
            RowMeta { id: 2, path: "b".into(), size: Some(3_000_000), ext: "jpg".into() },
            RowMeta { id: 3, path: "c".into(), size: Some(4_000_000), ext: "jpg".into() },
        ];
        let mut s = Selection::default();
        s.enter_free();
        s.toggle(0);
        s.toggle(2);
        let line = format_statusline(&s, &rows);
        assert!(line.starts_with("VISUAL (FREE)"), "got: {line}");
        assert!(line.contains("selected 2"), "got: {line}");
    }

    #[test]
    fn statusline_range_label() {
        use crate::selection::Selection;
        let rows = vec![
            RowMeta { id: 1, path: "a".into(), size: Some(1), ext: "jpg".into() },
            RowMeta { id: 2, path: "b".into(), size: Some(1), ext: "jpg".into() },
        ];
        let mut s = Selection::default();
        s.enter_range(0);
        s.cursor_moved(1);
        let line = format_statusline(&s, &rows);
        assert!(line.starts_with("VISUAL (RANGE)"), "got: {line}");
        assert!(line.contains("selected 2"), "got: {line}");
    }

    #[test]
    fn statusline_empty_results() {
        use crate::selection::Selection;
        let s = Selection::default();
        let line = format_statusline(&s, &[]);
        assert!(line.contains("0 images"), "got: {line}");
    }
}

#[cfg(test)]
mod restore_tests {
    use super::{
        PersistedMode, bytes_to_fraction, fraction_to_bytes, gps_to_mode, mode_is_search,
        sort_to_selector_index,
    };
    use imgfind::filters::GpsFilter;
    use imgfind::sort::{Sort, SortDir, SortKey};

    #[test]
    fn mode_is_search_only_for_text_and_similar() {
        assert!(!mode_is_search(&PersistedMode::Browse));
        assert!(mode_is_search(&PersistedMode::Text("cat".into())));
        assert!(mode_is_search(&PersistedMode::Similar(7)));
    }

    #[test]
    fn search_mode_selector_index_is_relevance() {
        // In search mode the selector always reflects Relevance (index 0),
        // regardless of the concrete stored key — the order is faithful anyway.
        let sort = Sort {
            key: SortKey::Size,
            dir: SortDir::Desc,
        };
        assert_eq!(
            sort_to_selector_index(sort, &PersistedMode::Text("x".into())),
            0
        );
        assert_eq!(sort_to_selector_index(sort, &PersistedMode::Similar(1)), 0);
    }

    #[test]
    fn browse_mode_selector_index_maps_key_directly() {
        let mk = |key| Sort {
            key,
            dir: SortDir::Asc,
        };
        assert_eq!(
            sort_to_selector_index(mk(SortKey::Name), &PersistedMode::Browse),
            0
        );
        assert_eq!(
            sort_to_selector_index(mk(SortKey::Size), &PersistedMode::Browse),
            1
        );
        assert_eq!(
            sort_to_selector_index(mk(SortKey::Type), &PersistedMode::Browse),
            2
        );
    }

    #[test]
    fn gps_mode_round_trips() {
        assert_eq!(gps_to_mode(GpsFilter::Any), 0);
        assert_eq!(gps_to_mode(GpsFilter::HasGps), 1);
        assert_eq!(gps_to_mode(GpsFilter::NoGps), 2);
    }

    #[test]
    fn bytes_fraction_inverts_fraction_to_bytes() {
        let (min, max) = (0i64, 1_000_000i64);
        // Unbounded extremes map to the matching slider end.
        assert_eq!(bytes_to_fraction(None, min, max, true), 0.0);
        assert_eq!(bytes_to_fraction(None, min, max, false), 1.0);
        // A concrete byte value round-trips back to its fraction.
        let bytes = fraction_to_bytes(0.5, min, max, true);
        assert!((bytes_to_fraction(bytes, min, max, true) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn bytes_fraction_degenerate_bounds() {
        // max <= min: any concrete value collapses to the slider extreme.
        assert_eq!(bytes_to_fraction(Some(5), 10, 10, true), 0.0);
        assert_eq!(bytes_to_fraction(Some(5), 10, 10, false), 1.0);
    }
}

#[cfg(test)]
mod sort_sel_tests {
    use super::{RowMeta, SelectAfter, resolve_selection};

    fn make_row(id: i64) -> RowMeta {
        RowMeta {
            id,
            path: format!("img{id}.jpg"),
            size: Some(100),
            ext: "jpg".to_string(),
        }
    }

    #[test]
    fn resolve_clear_is_none() {
        let rows = vec![make_row(1), make_row(2)];
        assert_eq!(resolve_selection(&SelectAfter::Clear, &rows, Some(0)), None);
    }

    #[test]
    fn resolve_keep_index_in_range() {
        let rows = vec![make_row(1), make_row(2), make_row(3)];
        assert_eq!(
            resolve_selection(&SelectAfter::KeepIndex, &rows, Some(1)),
            Some(1)
        );
    }

    #[test]
    fn resolve_keep_index_out_of_range() {
        let rows = vec![make_row(1)];
        assert_eq!(
            resolve_selection(&SelectAfter::KeepIndex, &rows, Some(5)),
            None
        );
    }

    #[test]
    fn resolve_keep_index_empty() {
        assert_eq!(
            resolve_selection(&SelectAfter::KeepIndex, &[], Some(0)),
            None
        );
    }

    #[test]
    fn resolve_first_non_empty() {
        let rows = vec![make_row(10), make_row(20)];
        assert_eq!(resolve_selection(&SelectAfter::First, &rows, None), Some(0));
    }

    #[test]
    fn resolve_first_empty() {
        assert_eq!(resolve_selection(&SelectAfter::First, &[], None), None);
    }

    #[test]
    fn resolve_by_id_found() {
        let rows = vec![make_row(10), make_row(20), make_row(30)];
        assert_eq!(
            resolve_selection(&SelectAfter::ById(Some(20)), &rows, None),
            Some(1)
        );
    }

    #[test]
    fn resolve_by_id_not_found() {
        let rows = vec![make_row(10), make_row(20)];
        assert_eq!(
            resolve_selection(&SelectAfter::ById(Some(99)), &rows, None),
            None
        );
    }

    #[test]
    fn resolve_by_id_none() {
        let rows = vec![make_row(10)];
        assert_eq!(
            resolve_selection(&SelectAfter::ById(None), &rows, None),
            None
        );
    }
}
