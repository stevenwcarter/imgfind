# TUI UI Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add render tests for the ratatui TUI — `insta` whole-frame snapshots plus targeted cell/style assertions — covering the chrome and (via a headless halfblocks protocol) the `ratatui-image` results-grid path.

**Architecture:** Render `&mut App` (the `Widget` impl in `src/tui/ui.rs`) into a ratatui `TestBackend`/`Buffer` at a fixed 80×24 size. Three `#[cfg(test)]`-only seams make `App` constructible headlessly: an inert `EventHandler` (no `tokio::spawn`), an `App` test-builder using `Picker::from_fontsize` (not `from_query_stdio`), and an `ImageEntry` builder from a tiny `DynamicImage`. Tests live in a new `#[cfg(test)] mod render_tests`.

**Tech Stack:** ratatui 0.30 (`TestBackend`, `Buffer`, `Cell`), ratatui-image 11 (`Picker::from_fontsize`, `ThreadProtocol`, halfblocks), `insta` 1.48 (snapshots), `image` 0.25 (`DynamicImage`), `tui-input` 0.15 (`Input`).

## Global Constraints

- Rust edition 2024; `cargo clippy -p imgfind --all-targets -- -D warnings` clean; `cargo fmt` clean (format with `cargo fmt -p imgfind -p imgfind-gui`, NOT `cargo fmt --all` — that reaches the sibling `../clipper` repo).
- All test seams are `#[cfg(test)]`-gated: ZERO production runtime/API change.
- Fixed determinism inputs: `Picker::from_fontsize((8, 16))`, `Rect` 80×24, solid-color test images.
- `insta` snapshots committed under `src/tui/snapshots/`.
- These are characterization tests of CURRENT output (regression net for ratatui/ratatui-image upgrades); re-baseline with `cargo insta review`.
- Do NOT modify production rendering behavior. Tests observe; they don't change the UI.

## Key facts about the code under test (verified)

- Render entrypoint: `impl Widget for &mut App` in `src/tui/ui.rs:152` — `fn render(self, area: Rect, buf: &mut Buffer)`. Draws outer border, then `render_images`, `render_pagination`, `render_input`, and (if `show_help`) `render_help_overlay`. Layout has margin 2.
- `App` fields are all `pub` (`src/tui/app.rs`). `App::new` calls `Picker::from_query_stdio()` (NOT usable in tests — bypass it).
- `EventHandler` (`src/tui/event.rs`) has private fields `sender`/`receiver`; `Default` does `tokio::spawn` (NOT usable in tests). An inert constructor must live in `event.rs` to access the private fields.
- `ImageEntry` (`src/tui/app/search.rs:22`): fields `path: String`, `score: f32`, `protocol: ThreadProtocol`, `image: Option<DynamicImage>`, `rx: UnboundedReceiver<ResizeRequest>`, `current_zoom: u8`. Constructed (search.rs:101-107) as:
  ```rust
  let protocol = picker.new_resize_protocol(image.clone());
  let (image_tx, image_rx) = unbounded_channel();
  ImageEntry { path, score, rx: image_rx, current_zoom, image: Some(base), protocol: ThreadProtocol::new(image_tx, Some(protocol)) }
  ```
- `SearchResult` (`src/tui/app/search.rs:12`): `images: Vec<(String, f32, DynamicImage)>`, `result_count: usize`, `query: String`.
- `App::handle_image_resize_requests(&mut self)` (`pub(crate)`, search.rs) drains each entry's `rx`, `resize_encode()`s, and `update_resized_protocol()`s — call it between renders to make halfblock pixels appear.
- `render_image` (`src/tui/ui.rs`) draws the focus border (if focused) and the right-aligned score label REGARDLESS of whether the image protocol has resized yet; only the image pixels depend on the resize cycle.
- `keybindings_help() -> Vec<String>` (`src/tui/app.rs:36`).
- `InputMode` enum (`src/tui/app.rs`): `Normal`, `Editing`. `Input` is `tui_input::Input`.

> If any ratatui-image 11 API name below differs at compile time (`Picker::from_fontsize`, `new_resize_protocol`, `ThreadProtocol::new`, `ResizeRequest::resize_encode`, `update_resized_protocol`), mirror the exact form used in `src/tui/app/search.rs` / `src/tui/app/zoom.rs`, or consult ratatui-image 0.11/`crate` docs via the context7 MCP tool. The behavior contract (a built protocol that renders halfblock cells after one resize cycle) is what must hold.

---

## File Structure

- `Cargo.toml` — add `[dev-dependencies]` with `insta`.
- `src/tui/event.rs` — add `#[cfg(test)] impl EventHandler { fn inert() -> Self }`.
- `src/tui/mod.rs` — add `#[cfg(test)] mod render_tests;`.
- `src/tui/render_tests.rs` (new) — the test-builder helpers + all render tests.
- `src/tui/snapshots/` (new dir) — `insta` snapshot files (committed).

---

## Task 1: Test harness, seams, and the first (idle) snapshot

**Files:**
- Modify: `Cargo.toml` (add `[dev-dependencies] insta`)
- Modify: `src/tui/event.rs` (inert `EventHandler`)
- Modify: `src/tui/mod.rs` (declare `render_tests`)
- Create: `src/tui/render_tests.rs`

**Interfaces:**
- Produces (all `#[cfg(test)]`):
  - `EventHandler::inert() -> EventHandler` (in `event.rs`)
  - in `render_tests.rs`: `fn temp_db() -> (Database, std::path::PathBuf)`, `fn test_app(db: Database) -> App`, `fn render_to_string(app: &mut App, w: u16, h: u16) -> String`.

- [ ] **Step 1: Add `insta` dev-dependency**

In `Cargo.toml`, add a new section (the crate currently has none):

```toml
[dev-dependencies]
insta = "1.48"
```

- [ ] **Step 2: Add the inert `EventHandler` constructor**

In `src/tui/event.rs`, add inside an `impl EventHandler { … }` block (the fields `sender`/`receiver` are private to this module, so this MUST live here):

```rust
#[cfg(test)]
impl EventHandler {
    /// Test-only constructor: builds the channel pair WITHOUT spawning the
    /// crossterm reader task, so render tests need no tokio runtime and never
    /// touch stdin.
    pub(crate) fn inert() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }
}
```

- [ ] **Step 3: Declare the test module**

In `src/tui/mod.rs`, add after the existing `mod` lines:

```rust
#[cfg(test)]
mod render_tests;
```

- [ ] **Step 4: Write the harness helpers + the first failing test**

Create `src/tui/render_tests.rs`:

```rust
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

/// Build an `App` headlessly: a `from_fontsize` (halfblocks) picker instead of
/// `from_query_stdio`, an inert event handler (no spawn), and defaulted state.
/// Tests mutate the returned `App`'s public fields to set up each scenario.
fn test_app(db: Database) -> App {
    let (search_tx, search_rx) = unbounded_channel();
    let (zoom_tx, zoom_rx) = unbounded_channel();
    App {
        db,
        picker: Picker::from_fontsize((8, 16)),
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
```

> If the `App` struct literal misses or misnames a field, fix it against `src/tui/app.rs`'s `pub struct App` (every field is `pub`). If `InputMode` is not at `super::app::InputMode`, adjust the import to its real path.

- [ ] **Step 5: Run the test — it will create the snapshot (first run "fails" pending acceptance)**

Run: `cargo test -p imgfind --lib tui::render_tests::renders_idle_frame`
Expected: insta reports a NEW snapshot (test fails as "snapshot assertion" on first run, writing `src/tui/snapshots/imgfind__tui__render_tests__idle_frame.snap.new`). The `assert!(out.contains("imgfind-cli"))` must pass first; if THAT fails, the harness is wrong — fix before accepting the snapshot.

- [ ] **Step 6: Review and accept the snapshot**

Inspect the `.snap.new` file — confirm it shows the bordered `imgfind-cli` frame with an empty input box and no results. If correct, accept it:

Run: `cargo insta accept` (or rename `.snap.new` → `.snap`)
Then re-run: `cargo test -p imgfind --lib tui::render_tests::renders_idle_frame`
Expected: PASS.

- [ ] **Step 7: clippy + fmt**

Run: `cargo clippy -p imgfind --all-targets -- -D warnings` → clean.
Run: `cargo fmt -p imgfind -p imgfind-gui`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/tui/event.rs src/tui/mod.rs src/tui/render_tests.rs src/tui/snapshots/
git commit -m "test(tui): render-test harness + seams + idle-frame snapshot"
```

---

## Task 2: No-image state snapshots + behavioral assertions

**Files:**
- Modify: `src/tui/render_tests.rs`
- Create: snapshot files under `src/tui/snapshots/`

**Interfaces:**
- Consumes: `test_app`, `render_to_string`, `temp_db`, `W`, `H` from Task 1.
- Produces: nothing new for later tasks (adds tests only).

- [ ] **Step 1: Write the failing tests**

Append to `src/tui/render_tests.rs` (inside the same file; add imports `use ratatui::style::Color;` and `use crate::tui::app::search::SearchResult;` near the top, adjusting the `SearchResult` path to its real module):

```rust
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
    assert!(has_yellow, "input border should be yellow in editing mode");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn renders_help_overlay_with_keybindings() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    app.show_help = true;
    let out = render_to_string(&mut app, W, H);
    assert!(out.contains("Keybindings"), "help overlay title should render");
    // A known key from keybindings_help() (the `e` edit-search entry).
    assert!(out.contains('e'), "help overlay should list keybindings");
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
    assert!(out.contains("Page 1/1 (0 results)"), "empty pagination line");
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
    assert!(out.contains("Page 1/3 (20 results)"), "multi-page pagination");
    let _ = std::fs::remove_dir_all(root);
}
```

> `Input::new(String)` is the tui-input 0.15 constructor; if it differs, set the value via the real API. The `border_y`/yellow-scan assumes the input block is the bottom `Length(3)` region from `build_layout` (margin 2). If the yellow scan finds nothing, print the buffer and locate the actual border row, then fix the coordinate — do not weaken the assertion to always-pass.

- [ ] **Step 2: Run tests; the snapshot tests create `.snap.new`, the assertions must pass**

Run: `cargo test -p imgfind --lib tui::render_tests`
Expected: the four new tests' `assert!`/style checks PASS; the three with `insta::assert_snapshot!` report NEW snapshots (fail pending acceptance). `pagination_reports_multiple_pages` (no snapshot) PASSES outright. If a non-snapshot assertion fails, fix the test/coordinate before accepting snapshots.

- [ ] **Step 3: Accept snapshots and re-run**

Run: `cargo insta accept`
Run: `cargo test -p imgfind --lib tui::render_tests`
Expected: all PASS.

- [ ] **Step 4: clippy + fmt**

Run: `cargo clippy -p imgfind --all-targets -- -D warnings` → clean. Run `cargo fmt -p imgfind -p imgfind-gui`.

- [ ] **Step 5: Commit**

```bash
git add src/tui/render_tests.rs src/tui/snapshots/
git commit -m "test(tui): editing/help/empty-results snapshots + pagination + input-color asserts"
```

---

## Task 3: `ImageEntry` builder + results-grid chrome (single render)

**Files:**
- Modify: `src/tui/render_tests.rs`
- Create: snapshot file(s) under `src/tui/snapshots/`

**Interfaces:**
- Consumes: `test_app`, `temp_db`, `W`, `H`.
- Produces: `fn test_image_entry(picker: &mut Picker, path: &str, score: f32, rgb: [u8; 3]) -> ImageEntry` (used by Task 4 too).

- [ ] **Step 1: Write the builder + failing chrome tests**

Append to `src/tui/render_tests.rs` (add imports: `use ratatui_image::thread::ThreadProtocol;`, `use crate::tui::app::search::ImageEntry;`, `use image::{DynamicImage, RgbImage};` — adjust paths to match the real modules):

```rust
/// Build an `ImageEntry` from a tiny solid-color image, mirroring the
/// production construction in `app/search.rs` (a `ThreadProtocol` wrapping a
/// `new_resize_protocol`, with the paired resize-request receiver stored).
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

#[test]
fn results_grid_shows_score_label_and_focus_border() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    // Two results; build entries with the app's own picker.
    let mut picker = app.picker.clone();
    app.images = vec![
        test_image_entry(&mut picker, "a.jpg", 0.123, [200, 30, 30]),
        test_image_entry(&mut picker, "b.jpg", 0.456, [30, 200, 30]),
    ];
    app.search_result = Some(SearchResult {
        images: vec![
            ("a.jpg".into(), 0.123, DynamicImage::ImageRgb8(RgbImage::new(1, 1))),
            ("b.jpg".into(), 0.456, DynamicImage::ImageRgb8(RgbImage::new(1, 1))),
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

    // Score labels render around the image regardless of resize state.
    assert!(text.contains("0.123"), "focused score label should render");
    assert!(text.contains("0.456"), "second score label should render");
    // Pagination reflects the 2 results.
    assert!(text.contains("Page 1/1 (2 results)"), "grid pagination");
    // The focused cell (index 0) has a yellow border somewhere in the grid area.
    let grid_has_yellow = (0..W).any(|x| (0..H).any(|y| buf[(x, y)].fg == Color::Yellow));
    assert!(grid_has_yellow, "focused image cell should have a yellow border");

    let _ = std::fs::remove_dir_all(root);
}
```

> `app.picker.clone()` assumes `Picker: Clone` (it is in ratatui-image 11). If not, build a fresh `Picker::from_fontsize((8, 16))` for the entries — it must match the app's picker font size. `new_resize_protocol` may take `&mut self`; mirror search.rs (which calls it on `self.picker`). If `RgbImage::from_pixel`/`image::Rgb` names differ, use the `image` 0.25 equivalents.

- [ ] **Step 2: Run; fix any coordinate/API issues, confirm assertions pass**

Run: `cargo test -p imgfind --lib tui::render_tests::results_grid_shows_score_label_and_focus_border`
Expected: PASS (no snapshot in this test). If the yellow-border scan fails, print the buffer, confirm `render_image` draws the focused border, and correct the scan; do not weaken it to pass trivially.

- [ ] **Step 3: clippy + fmt + commit**

```bash
cargo clippy -p imgfind --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add src/tui/render_tests.rs
git commit -m "test(tui): results-grid chrome — score labels, focus border, pagination"
```

---

## Task 4: Image-pixel rendering (resize cycle) + zoom view snapshots

**Files:**
- Modify: `src/tui/render_tests.rs`
- Create: snapshot files under `src/tui/snapshots/`

**Interfaces:**
- Consumes: `test_app`, `temp_db`, `test_image_entry`, `App::handle_image_resize_requests`.

> This task exercises the actual halfblock image pixels, which require a
> render → drain-resize → render cycle (the `ThreadProtocol` defers encoding).
> This is the most upgrade-sensitive and most brittle coverage. If, after
> mirroring production and consulting ratatui-image 11 docs via context7, the
> halfblock cells cannot be made deterministic headless, STOP and report
> DONE_WITH_CONCERNS: keep the grid-CHROME coverage from Task 3 (which does not
> need the resize cycle) and document the limitation in the report rather than
> committing a flaky snapshot.

- [ ] **Step 1: Write the failing image-pixel snapshot test**

Append to `src/tui/render_tests.rs`:

```rust
/// Render once (queues resize requests on each entry's rx), process the
/// resize requests (encode the halfblock protocol), then render again so the
/// image cells contain stable halfblock glyphs.
fn render_grid_with_pixels_to_string(app: &mut App) -> String {
    let backend = TestBackend::new(W, H);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| frame.render_widget(&mut *app, frame.area()))
        .expect("first draw queues resize");
    app.handle_image_resize_requests();
    terminal
        .draw(|frame| frame.render_widget(&mut *app, frame.area()))
        .expect("second draw renders pixels");
    format!("{}", terminal.backend())
}

#[test]
fn results_grid_renders_image_pixels() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    let mut picker = app.picker.clone();
    app.images = vec![test_image_entry(&mut picker, "a.jpg", 0.123, [200, 30, 30])];
    app.search_result = Some(SearchResult {
        images: vec![("a.jpg".into(), 0.123, DynamicImage::ImageRgb8(RgbImage::new(1, 1)))],
        result_count: 1,
        query: "red".to_string(),
    });
    let out = render_grid_with_pixels_to_string(&mut app);
    // The score label still renders; the snapshot also captures image cells.
    assert!(out.contains("0.123"), "score label present");
    insta::assert_snapshot!("results_grid_pixels", out);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn renders_zoomed_image() {
    let (db, root) = temp_db();
    let mut app = test_app(db);
    let mut picker = app.picker.clone();
    app.zoomed_image_index = Some(0);
    app.zoomed_image = Some(test_image_entry(&mut picker, "z.jpg", 0.999, [30, 30, 200]));
    let out = render_grid_with_pixels_to_string(&mut app);
    insta::assert_snapshot!("zoomed_image", out);
    let _ = std::fs::remove_dir_all(root);
}
```

> `handle_image_resize_requests` is `pub(crate)` — callable from this in-crate test module. If the second render still shows no image cells, the resize path may need the area to be known on the first render (it is — the grid cell rect drives the ResizeRequest). Consult ratatui-image 11 `ThreadProtocol`/`ResizeRequest` docs via context7 if the encode call name differs. Do NOT commit a snapshot that varies between runs.

- [ ] **Step 2: Run; verify determinism (run twice, output identical)**

Run: `cargo test -p imgfind --lib tui::render_tests::results_grid_renders_image_pixels` (twice)
Expected: identical `.snap.new` both times (insta is deterministic if the render is). If the two runs differ, the snapshot is flaky — follow the DONE_WITH_CONCERNS guidance above instead of committing it.

- [ ] **Step 3: Accept snapshots, re-run all TUI render tests**

Run: `cargo insta accept`
Run: `cargo test -p imgfind --lib tui::render_tests`
Expected: all PASS.

- [ ] **Step 4: clippy + fmt + commit**

```bash
cargo clippy -p imgfind --all-targets -- -D warnings
cargo fmt -p imgfind -p imgfind-gui
git add src/tui/render_tests.rs src/tui/snapshots/
git commit -m "test(tui): halfblock image-pixel grid + zoomed-image snapshots"
```

---

## Self-Review

**Spec coverage:**
- insta whole-frame snapshots (idle/editing/help/empty/grid/zoom) → Tasks 1,2,4. ✓
- Behavioral/style asserts (yellow input border, focus border, pagination text, help keybinding, score label) → Tasks 2,3. ✓
- Headless halfblocks image path → Tasks 3 (chrome) + 4 (pixels). ✓
- Test seams (inert EventHandler, App builder, ImageEntry builder), all `#[cfg(test)]` → Tasks 1,3. ✓
- `insta` dev-dep; snapshots under `src/tui/snapshots/`; tests in `#[cfg(test)] mod render_tests` → Task 1. ✓
- Determinism (from_fontsize, 80×24, solid images) → Task 1 constants + builders. ✓
- Out-of-scope items (real protocols, event-loop, mouse) → not implemented (correct). ✓

**Placeholder scan:** No TBD/"handle edge cases"; API-uncertain spots carry an explicit "mirror production / consult context7" instruction with a behavior contract, and Task 4 has a concrete DONE_WITH_CONCERNS fallback rather than a vague hope. ✓

**Type consistency:** `test_app(db: Database) -> App`, `test_image_entry(&mut Picker, &str, f32, [u8;3]) -> ImageEntry`, `render_to_string(&mut App,u16,u16)->String`, `render_grid_with_pixels_to_string(&mut App)->String`, `EventHandler::inert()->EventHandler` — names consistent across tasks. `SearchResult { images, result_count, query }` and `ImageEntry { path, score, rx, current_zoom, image, protocol }` match the verified definitions. ✓

**Risk flag for implementer:** ratatui-image 11 method names (`from_fontsize`, `new_resize_protocol`, `ThreadProtocol::new`, resize-encode) and `Picker: Clone` are the main uncertainties — every use site says to mirror `src/tui/app/search.rs`/`zoom.rs` or consult context7. The single highest-risk item (deterministic halfblock pixels in Task 4) is isolated in its own task with a graceful fallback so Tasks 1-3 deliver value regardless.
