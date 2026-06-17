slint::include_modules!();

mod backend;
mod detail;
mod image_util;
mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{ModelRc, Timer, TimerMode, VecModel, Weak};

use backend::Backend;
use detail::{DetailState, filename_of, format_metadata, select};
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

fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let args = Args::parse();
    let backend = Backend::open(args.dir.as_deref()).context("Failed to open imgfind database")?;
    backend.start_loading_model();

    let window = MainWindow::new().context("Failed to create window")?;

    // Initial UI state: model loading, search disabled.
    window.set_can_search(false);
    window.set_status("Loading model...".into());
    window.set_show_load_more(false);
    window.set_tiles(ModelRc::default());
    window.set_lightbox_open(false);
    window.set_detail_open(false);

    // State shared with background threads via Arc<Mutex<_>>.
    let state: Arc<Mutex<SearchState>> = Arc::new(Mutex::new(SearchState::new()));

    // Current lightbox index. None when the lightbox is closed.
    let lb_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

    // Currently selected detail image. None when the panel is closed.
    let detail: Arc<Mutex<Option<DetailState>>> = Arc::new(Mutex::new(None));

    // Tracks whether the tile grid was populated by a text or similarity search,
    // so `on_load_more` knows which backend method to call.
    let search_mode: Arc<Mutex<SearchMode>> = Arc::new(Mutex::new(SearchMode::Text(String::new())));

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
        let backend_search = backend.clone();
        window.on_search(move |query| {
            let query = query.trim().to_string();
            if query.is_empty() {
                // Clearing the search box resets everything, including the
                // detail panel and lightbox (both could hold stale state).
                *state_ref.lock().unwrap() = SearchState::new();
                *detail_ref.lock().unwrap() = None;
                *lb_ref.lock().unwrap() = None;
                if let Some(w) = weak.upgrade() {
                    w.set_status("Enter a search query to find images.".into());
                    w.set_show_load_more(false);
                    w.set_tiles(ModelRc::default());
                    w.set_detail_open(false);
                    w.set_lightbox_open(false);
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
            );
        });
    }

    // --- load-more callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let mode_ref = Arc::clone(&search_mode);
        let backend_more = backend.clone();
        window.on_load_more(move || {
            let offset = state_ref.lock().unwrap().next_offset();
            let mode = mode_ref.lock().unwrap().clone();

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
                    );
                }
                SearchMode::Similar(seed) if !seed.is_empty() => {
                    spawn_similar(
                        weak.clone(),
                        Arc::clone(&state_ref),
                        backend_more.clone(),
                        seed,
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
        let backend_sim = backend.clone();
        window.on_search_similar(move || {
            let seed_path = {
                let d = detail_ref.lock().unwrap();
                d.as_ref().map(|ds| ds.path.clone())
            };
            let Some(seed_path) = seed_path else { return };

            let filename = filename_of(&seed_path);
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

    // Keep the model timer alive for the entire event loop.
    let _ = model_timer;

    window.run().context("Event loop failed")?;
    Ok(())
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
) {
    std::thread::spawn(move || {
        let res = backend.search(&query, offset, &imgfind::filters::Filters::default());

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
) {
    std::thread::spawn(move || {
        let res = backend.search_similar(&seed_path, offset, &imgfind::filters::Filters::default());

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
    use super::{clamp_next, clamp_prev};

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
}
