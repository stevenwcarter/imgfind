//! Render tests for the TUI. These render `&mut App` into an in-memory
//! ratatui backend and assert the output — `insta` snapshots for whole-frame
//! regression coverage plus targeted cell/style assertions. All construction
//! goes through `#[cfg(test)]` seams so no real terminal/runtime is needed.

use std::sync::atomic::{AtomicU32, Ordering};

use image::{DynamicImage, RgbImage};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui_image::picker::Picker;
use ratatui_image::thread::ThreadProtocol;
use tokio::sync::mpsc::unbounded_channel;
use tui_input::Input;

use super::app::{App, InputMode};
use super::event::EventHandler;
use crate::database::Database;
use crate::tui::app::{ImageEntry, SearchResult};

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

#[test]
fn renders_editing_mode_with_yellow_input_border() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    app.input_mode = InputMode::Editing;
    app.input = Input::new("sunset".to_string());

    // Snapshot (text) + style assertion (color).
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&mut app, frame.area()))
        .expect("draw");
    let buf = terminal.backend().buffer().clone();

    // The query text is visible.
    let text = format!("{}", terminal.backend());
    assert!(text.contains("sunset"), "typed query should render");
    insta::assert_snapshot!("editing_mode", text);

    // The input box border is yellow in editing mode. The input box is the
    // bottom 3-row block (layout margin 2, last constraint Length(3)); its top
    // border row is at y = H - 2 - 3 = 19. Scan that row for a yellow cell.
    let border_y = H - 2 - 3;
    let has_yellow = (0..W).any(|x| buf[(x, border_y)].fg == Color::Yellow);
    if !has_yellow {
        // Diagnostic: print the buffer and inspect nearby rows so we can find
        // the actual yellow-border row if the layout shifts.
        eprintln!("--- buffer text ---\n{text}");
        for y in 0..H {
            for x in 0..W {
                let cell = &buf[(x, y)];
                if cell.fg == Color::Yellow {
                    eprintln!("yellow cell at ({x}, {y}): {:?}", cell.symbol());
                }
            }
        }
    }
    assert!(has_yellow, "input border should be yellow in editing mode");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn renders_help_overlay_with_keybindings() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    app.show_help = true;
    let out = render_to_string(&mut app, W, H);
    assert!(
        out.contains("Keybindings"),
        "help overlay title should render"
    );
    // A multi-char keybinding token from keybindings_help() that does not
    // appear in the frame title or border chrome.
    assert!(out.contains("h/j/k/l"), "help overlay should list the focus keys");
    insta::assert_snapshot!("help_overlay", out);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn renders_empty_results_pagination() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    app.search_result = Some(SearchResult {
        images: Vec::new(),
        result_count: 0,
        query: "nothing".to_string(),
    });
    let out = render_to_string(&mut app, W, H);
    // total_pages = 0.div_ceil(9) = 0; rendered as max(1) => "Page 1/1".
    assert!(
        out.contains("Page 1/1 (0 results)"),
        "empty pagination line"
    );
    insta::assert_snapshot!("empty_results", out);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn pagination_reports_multiple_pages() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    app.search_result = Some(SearchResult {
        images: Vec::new(),
        result_count: 20,
        query: "many".to_string(),
    });
    // 20 results, 9 per page => 3 pages.
    let out = render_to_string(&mut app, W, H);
    assert!(
        out.contains("Page 1/3 (20 results)"),
        "multi-page pagination"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// Build an `ImageEntry` from a tiny solid-color image, mirroring the
/// production construction in `app/search.rs`: a `ThreadProtocol` wrapping a
/// `new_resize_protocol`, with the paired resize-request receiver stored.
fn test_image_entry(picker: &mut Picker, path: &str, score: f32, rgb: [u8; 3]) -> ImageEntry {
    let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 16, image::Rgb(rgb)));
    let protocol = picker.new_resize_protocol(img.clone());
    let (image_tx, image_rx) = unbounded_channel();
    ImageEntry {
        path: path.to_string(),
        score,
        rx: image_rx,
        current_zoom: 1,
        image: Some(img),
        protocol: ThreadProtocol::new(image_tx, Some(protocol)),
    }
}

/// Verifies that the results grid renders score labels, pagination, and the
/// yellow focus border for the focused cell, all from a single draw call
/// (no resize dance needed — `render_image` draws the border around the image
/// area regardless of whether pixels have been resized yet).
#[test]
fn results_grid_shows_score_label_and_focus_border() {
    let (db, root) = temp_db();
    let mut app = test_app(db);

    // Build entries using the app's own picker (Picker: Clone).
    let mut picker = app.picker.clone();
    app.images = vec![
        test_image_entry(&mut picker, "a.jpg", 0.123, [200, 30, 30]),
        test_image_entry(&mut picker, "b.jpg", 0.456, [30, 200, 30]),
    ];
    app.search_result = Some(SearchResult {
        images: vec![
            (
                "a.jpg".into(),
                0.123,
                DynamicImage::ImageRgb8(RgbImage::new(1, 1)),
            ),
            (
                "b.jpg".into(),
                0.456,
                DynamicImage::ImageRgb8(RgbImage::new(1, 1)),
            ),
        ],
        result_count: 2,
        query: "things".to_string(),
    });
    app.focused_image_index = 0;

    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&mut app, frame.area()))
        .expect("draw");
    let text = format!("{}", terminal.backend());
    let buf = terminal.backend().buffer().clone();

    // Score labels render overlaid on each cell regardless of resize state.
    assert!(text.contains("0.123"), "focused score label should render");
    assert!(text.contains("0.456"), "second score label should render");

    // Pagination reflects the 2 results (full string also contains "[H prev | L next]").
    assert!(
        text.contains("Page 1/1 (2 results)"),
        "grid pagination should reflect result count"
    );

    // The focused cell (index 0) has a yellow border. `render_image` draws a
    // Block::bordered with fg(Color::Yellow) only when index == focused_image_index,
    // so at least one cell in the grid area must carry Yellow foreground.
    let grid_has_yellow = (0..W).any(|x| (0..H).any(|y| buf[(x, y)].fg == Color::Yellow));
    if !grid_has_yellow {
        eprintln!("--- buffer text ---\n{text}");
        for y in 0..H {
            for x in 0..W {
                let cell = &buf[(x, y)];
                if cell.fg != Color::Reset {
                    eprintln!("fg={:?} at ({x},{y}): {:?}", cell.fg, cell.symbol());
                }
            }
        }
    }
    assert!(
        grid_has_yellow,
        "focused image cell should have a yellow border"
    );

    let _ = std::fs::remove_dir_all(root);
}
