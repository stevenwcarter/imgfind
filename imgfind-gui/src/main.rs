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

    // State shared with background threads via Arc<Mutex<_>>.
    let state: Arc<Mutex<SearchState>> = Arc::new(Mutex::new(SearchState::new()));

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

    // --- tile-clicked callback (no-op until Task 6 lightbox) ---
    window.on_tile_clicked(|_index| {
        // Lightbox is Task 6; nothing to do here yet.
    });

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

    // Keep the model timer alive for the entire event loop.
    let _ = model_timer;

    window.run().context("Event loop failed")?;
    Ok(())
}

/// Build a `ModelRc<Tile>` from the current state, decoding thumbnails via the backend.
fn build_tiles_model(results: &[SearchResult], backend: &Backend) -> ModelRc<Tile> {
    let tiles: Vec<Tile> = results
        .iter()
        .map(|r| {
            let img = backend
                .thumbnail(&r.path, 300)
                .ok()
                .and_then(|bytes| image_util::jpeg_to_slint_image(&bytes).ok())
                .unwrap_or_default();
            let size_kb = r.file_size.unwrap_or(0) / 1024;
            Tile { path: r.path.clone().into(), image: img, size_kb: size_kb as i32 }
        })
        .collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(tiles)))
}

fn status_text_for(vs: ViewState) -> &'static str {
    match vs {
        ViewState::Idle => "Enter a search query to find images.",
        ViewState::Loading => "Searching\u{2026}",
        ViewState::Error => "",
        ViewState::Empty => "No images found.",
        ViewState::Results => "",
    }
}

/// Spawn a background search thread and marshal results back to the UI thread.
fn spawn_search(
    weak: Weak<MainWindow>,
    state_ref: Arc<Mutex<SearchState>>,
    backend: Backend,
    query: String,
    offset: usize,
) {
    std::thread::spawn(move || {
        let res = backend.search(&query, offset);
        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };

            let results_snapshot = {
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
            let (vs, has_more, error_msg, results) = results_snapshot;

            // Rebuild tiles on success.
            if matches!(vs, ViewState::Results | ViewState::Empty) {
                let model = build_tiles_model(&results, &backend);
                w.set_tiles(model);
            }

            let status_text: slint::SharedString = match vs {
                ViewState::Error => error_msg.as_deref().unwrap_or("Unknown error").into(),
                other => status_text_for(other).into(),
            };
            w.set_status(status_text);
            w.set_show_load_more(has_more && vs == ViewState::Results);
            w.set_can_search(true);
        })
        .ok();
    });
}
