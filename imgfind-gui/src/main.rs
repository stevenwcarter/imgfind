slint::include_modules!();

mod backend;
mod image_util;
mod state;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use slint::{ModelRc, Timer, TimerMode, VecModel, Weak};

use backend::Backend;
use state::{SearchResult, SearchState, ViewState};

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
    let backend =
        Backend::open(args.dir.as_deref()).context("Failed to open imgfind database")?;
    backend.start_loading_model();

    let window = MainWindow::new().context("Failed to create window")?;

    // Initial UI state: model loading, search disabled.
    window.set_can_search(false);
    window.set_status("Loading model...".into());
    window.set_show_load_more(false);
    window.set_tiles(ModelRc::default());
    window.set_lightbox_open(false);

    // State shared with background threads via Arc<Mutex<_>>.
    let state: Arc<Mutex<SearchState>> = Arc::new(Mutex::new(SearchState::new()));

    // Current lightbox index. None when the lightbox is closed.
    let lb_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

    // Poll for model readiness every 250 ms.
    let model_timer = Timer::default();
    {
        let weak = window.as_weak();
        let backend_poll = backend.clone();
        model_timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            if backend_poll.model_ready()
                && let Some(w) = weak.upgrade()
                && !w.get_can_search()
            {
                w.set_can_search(true);
                // Clear loading status only when no search is in progress.
                if w.get_status() == "Loading model..." {
                    w.set_status("Enter a search query to find images.".into());
                }
            }
        });
    }

    // --- search callback ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let backend_search = backend.clone();
        window.on_search(move |query| {
            let query = query.trim().to_string();
            if query.is_empty() {
                *state_ref.lock().unwrap() = SearchState::new();
                if let Some(w) = weak.upgrade() {
                    w.set_status("Enter a search query to find images.".into());
                    w.set_show_load_more(false);
                    w.set_tiles(ModelRc::default());
                }
                return;
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
        let backend_more = backend.clone();
        window.on_load_more(move || {
            let (query, offset) = {
                let s = state_ref.lock().unwrap();
                (s.committed_query.clone(), s.next_offset())
            };
            if query.is_empty() {
                return;
            }
            if let Some(w) = weak.upgrade() {
                w.set_status("Searching\u{2026}".into());
                w.set_show_load_more(false);
                w.set_can_search(false);
            }

            spawn_search(
                weak.clone(),
                Arc::clone(&state_ref),
                backend_more.clone(),
                query,
                offset,
            );
        });
    }

    // --- tile-clicked callback: open lightbox at the clicked index ---
    {
        let weak = window.as_weak();
        let state_ref = Arc::clone(&state);
        let lb_ref = Arc::clone(&lb_index);
        let backend_lb = backend.clone();
        window.on_tile_clicked(move |index| {
            let idx = index as usize;
            let path = state_ref.lock().unwrap().results.get(idx).map(|r| r.path.clone());
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
                // Clamp to [0, len): no wrap, no panic at the start.
                let next = current.saturating_sub(1);
                *guard = Some(next);
                next
            };
            let path =
                state_ref.lock().unwrap().results.get(new_idx).map(|r| r.path.clone());
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
                // Clamp to [0, len): no wrap, no panic at the end.
                let next = if len == 0 { 0 } else { (current + 1).min(len - 1) };
                *guard = Some(next);
                (next, len)
            };
            if len == 0 {
                return;
            }
            let path =
                state_ref.lock().unwrap().results.get(new_idx).map(|r| r.path.clone());
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
            Tile { path: r.path.clone().into(), image: img, size_kb: size_kb as i32 }
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
        let res = backend.search(&query, offset);

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
    /// Lightbox index-clamp logic: saturating_sub never goes below 0,
    /// min(len-1) never exceeds the last valid index.
    #[test]
    fn lightbox_index_clamp_prev_at_zero_stays_zero() {
        let current: usize = 0;
        assert_eq!(current.saturating_sub(1), 0);
    }

    #[test]
    fn lightbox_index_clamp_prev_advances_backward() {
        let current: usize = 5;
        assert_eq!(current.saturating_sub(1), 4);
    }

    #[test]
    fn lightbox_index_clamp_next_at_last_stays_last() {
        let len: usize = 10;
        let current: usize = 9;
        assert_eq!((current + 1).min(len - 1), 9);
    }

    #[test]
    fn lightbox_index_clamp_next_advances_forward() {
        let len: usize = 10;
        let current: usize = 3;
        assert_eq!((current + 1).min(len - 1), 4);
    }
}
