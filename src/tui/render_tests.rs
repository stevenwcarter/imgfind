//! Render tests for the TUI. These render `&mut App` into an in-memory
//! ratatui backend and assert the output — `insta` snapshots for whole-frame
//! regression coverage plus targeted cell/style assertions. All construction
//! goes through `#[cfg(test)]` seams so no real terminal/runtime is needed.

use std::sync::atomic::{AtomicU32, Ordering};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui_image::picker::Picker;
use tokio::sync::mpsc::unbounded_channel;
use tui_input::Input;

use super::app::{App, InputMode};
use super::event::EventHandler;
use crate::database::Database;

/// Fixed terminal size for all render tests.
const W: u16 = 80;
const H: u16 = 24;

/// Unique temp database (mirrors the layout `Database::new` requires).
fn temp_db() -> (Database, std::path::PathBuf) {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("imgfind_tui_test_{}_{n}", std::process::id()));
    let db_path = root.join(".imgfind").join("imgfind.db");
    let db = Database::new(&db_path).expect("create temp db");
    (db, root)
}

/// Build an `App` headlessly: a `halfblocks` picker instead of
/// `from_query_stdio`, an inert event handler (no spawn), and defaulted state.
/// Tests mutate the returned `App`'s public fields to set up each scenario.
fn test_app(db: Database) -> App {
    let (search_tx, search_rx) = unbounded_channel();
    let (zoom_tx, zoom_rx) = unbounded_channel();
    App {
        db,
        picker: Picker::halfblocks(),
        images: Vec::new(),
        running: true,
        zoomed_image_index: None,
        zoomed_image: None,
        zoom_level: 1,
        zoom_focal: (0.5, 0.5),
        zoomed_image_rect: None,
        focused_image_index: 0,
        input: Input::default(),
        page: 0,
        input_mode: InputMode::Normal,
        last_search: None,
        search_result: None,
        events: EventHandler::inert(),
        search_rx,
        search_tx,
        zoom_rx,
        zoom_tx,
        current_search_task: None,
        is_searching: false,
        mouse_click: None,
        show_help: false,
    }
}

/// Render the app once into a `TestBackend` and return its text grid (no color).
fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&mut *app, frame.area()))
        .expect("draw");
    format!("{}", terminal.backend())
}

#[test]
fn renders_idle_frame() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    let out = render_to_string(&mut app, W, H);
    // Idle frame shows the outer title and the empty input box, no results.
    assert!(out.contains("imgfind-cli"), "outer title should render");
    insta::assert_snapshot!("idle_frame", out);
    let _ = std::fs::remove_dir_all(root);
}
