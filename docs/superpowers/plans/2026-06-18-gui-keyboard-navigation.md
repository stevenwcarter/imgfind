# GUI Keyboard Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add vim-style + arrow-key navigation with a visible selection to the imgfind-gui thumbnail grid, and wire the lightbox so navigating it mirrors the grid selection.

**Architecture:** A pure, unit-tested Rust index function (`nav::move_selection`) computes the next selected index; `imgfind-gui/src/main.rs` owns a shared `selected: Arc<Mutex<Option<usize>>>` and wires new Slint callbacks to it (matching the existing `Arc<Mutex<…>>` + callback pattern); `imgfind-gui/ui/app.slint` carries the declarative visuals (highlight border, FocusScope key handling, scroll-into-view, lightbox vim keys).

**Tech Stack:** Rust (edition 2024), Slint, the existing `imgfind-gui` crate. No new dependencies.

## Global Constraints

- Rust edition 2024; code must be `cargo clippy --workspace` and `cargo fmt --all` clean.
- No new crate dependencies.
- Slint button/label text must use ASCII / Latin-1 glyphs only (the default font tofus symbol glyphs — see existing `×` comment in `app.slint`). N/A here unless adding visible glyph text.
- `selected` (Rust), `lb_index` (Rust), and the Slint `selected-index` property all index the **same** list: the current `state.results` / grid order. Any code mutating the result set keeps them consistent.
- Left/right navigation **clamps** at the global first/last tile (NO wrap); this is the deliberate divergence from utmost. Up/down clamp at the top/bottom rows.
- Direction encoding for the `grid-nav` callback: `0=Left, 1=Right, 2=Up, 3=Down`.

---

### Task 1: Pure navigation function `nav::move_selection`

**Files:**
- Create: `imgfind-gui/src/nav.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod nav;` near the other module declarations)
- Test: inline `#[cfg(test)]` module in `imgfind-gui/src/nav.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum NavDir { Left, Right, Up, Down }`
  - `impl NavDir { pub fn from_i32(v: i32) -> Option<NavDir> }` (0=Left,1=Right,2=Up,3=Down; other → None)
  - `pub fn move_selection(cur: Option<usize>, dir: NavDir, cols: usize, len: usize) -> Option<usize>`

- [ ] **Step 1: Write the failing tests**

Create `imgfind-gui/src/nav.rs` with the test module first (the function/enum will not exist yet, so it won't compile — that is the failing state):

```rust
//! Pure grid-navigation index math for the GUI thumbnail grid.
//!
//! Mirrors the behaviour of utmost's `gallery_move`, EXCEPT Left/Right clamp at
//! the global first/last tile instead of wrapping (see the design spec
//! 2026-06-18-gui-keyboard-navigation-design.md).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

impl NavDir {
    pub fn from_i32(v: i32) -> Option<NavDir> {
        match v {
            0 => Some(NavDir::Left),
            1 => Some(NavDir::Right),
            2 => Some(NavDir::Up),
            3 => Some(NavDir::Down),
            _ => None,
        }
    }
}

/// Compute the new selected index.
///
/// - `len == 0` -> `None`
/// - `cur == None` -> `Some(0)` (first key selects the first tile)
/// - Left/Right -> linear ±1, clamped to `[0, len-1]` (crosses rows, no global wrap)
/// - Up/Down -> ±cols, clamped so it never leaves the grid and never moves when
///   already in the top/bottom row for that column
///
/// `cols` is coerced to at least 1.
pub fn move_selection(cur: Option<usize>, dir: NavDir, cols: usize, len: usize) -> Option<usize> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 3-column grid with 8 tiles (indices 0..=7); bottom row [6,7] is partial.
    const COLS: usize = 3;
    const LEN: usize = 8;

    #[test]
    fn no_selection_any_direction_selects_first() {
        for dir in [NavDir::Left, NavDir::Right, NavDir::Up, NavDir::Down] {
            assert_eq!(move_selection(None, dir, COLS, LEN), Some(0));
        }
    }

    #[test]
    fn left_crosses_row_boundary() {
        // index 3 is the first tile of row 1; left -> 2, the last tile of row 0.
        assert_eq!(move_selection(Some(3), NavDir::Left, COLS, LEN), Some(2));
    }

    #[test]
    fn left_at_first_tile_stays_no_global_wrap() {
        assert_eq!(move_selection(Some(0), NavDir::Left, COLS, LEN), Some(0));
    }

    #[test]
    fn right_crosses_row_boundary() {
        // index 2 is the last tile of row 0; right -> 3, the first tile of row 1.
        assert_eq!(move_selection(Some(2), NavDir::Right, COLS, LEN), Some(3));
    }

    #[test]
    fn right_at_last_tile_stays_no_global_wrap() {
        assert_eq!(move_selection(Some(LEN - 1), NavDir::Right, COLS, LEN), Some(LEN - 1));
    }

    #[test]
    fn up_from_top_row_stays() {
        assert_eq!(move_selection(Some(1), NavDir::Up, COLS, LEN), Some(1));
    }

    #[test]
    fn up_from_second_row_moves_up_one_row() {
        assert_eq!(move_selection(Some(4), NavDir::Up, COLS, LEN), Some(1));
    }

    #[test]
    fn down_from_bottom_row_stays() {
        // index 7 is in the bottom (partial) row; down stays.
        assert_eq!(move_selection(Some(7), NavDir::Down, COLS, LEN), Some(7));
    }

    #[test]
    fn down_into_partial_bottom_row_clamps() {
        // index 5 (row 1, col 2); +cols = 8 which is >= len, so it clamps (stays).
        assert_eq!(move_selection(Some(5), NavDir::Down, COLS, LEN), Some(5));
    }

    #[test]
    fn down_from_top_row_moves_down_one_row() {
        assert_eq!(move_selection(Some(0), NavDir::Down, COLS, LEN), Some(3));
    }

    #[test]
    fn zero_cols_treated_as_one() {
        // cols 0 must not panic / divide by zero; behaves as a single column.
        assert_eq!(move_selection(Some(0), NavDir::Down, 0, LEN), Some(1));
        assert_eq!(move_selection(Some(LEN - 1), NavDir::Down, 0, LEN), Some(LEN - 1));
    }

    #[test]
    fn empty_grid_returns_none() {
        assert_eq!(move_selection(Some(0), NavDir::Right, COLS, 0), None);
        assert_eq!(move_selection(None, NavDir::Right, COLS, 0), None);
    }

    #[test]
    fn single_column_up_down_behave_like_prev_next() {
        // cols == 1: up/down move by one and clamp.
        assert_eq!(move_selection(Some(2), NavDir::Up, 1, 5), Some(1));
        assert_eq!(move_selection(Some(2), NavDir::Down, 1, 5), Some(3));
        assert_eq!(move_selection(Some(0), NavDir::Up, 1, 5), Some(0));
        assert_eq!(move_selection(Some(4), NavDir::Down, 1, 5), Some(4));
    }
}
```

Add `mod nav;` to `imgfind-gui/src/main.rs` alongside the other `mod` declarations (e.g. near `mod backend;` / `mod state;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui nav::`
Expected: compile/run failure — `move_selection` hits `todo!()` (panics) so tests fail.

- [ ] **Step 3: Implement `move_selection`**

Replace the `todo!()` body:

```rust
pub fn move_selection(cur: Option<usize>, dir: NavDir, cols: usize, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cols = cols.max(1);
    let i = match cur {
        None => return Some(0),
        Some(i) => i.min(len - 1),
    };
    let new = match dir {
        NavDir::Left => i.saturating_sub(1),
        NavDir::Right => (i + 1).min(len - 1),
        NavDir::Up => {
            if i < cols {
                i
            } else {
                i - cols
            }
        }
        NavDir::Down => {
            if i + cols >= len {
                i
            } else {
                i + cols
            }
        }
    };
    Some(new)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui nav::`
Expected: PASS (all `nav::tests::*`).

- [ ] **Step 5: Lint & format**

Run: `cargo clippy -p imgfind-gui --all-targets && cargo fmt --all`
Expected: no warnings, no diff after fmt.

- [ ] **Step 6: Commit**

```bash
git add imgfind-gui/src/nav.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): pure move_selection grid-navigation index math + tests"
```

---

### Task 2: Slint grid selection — `selected-index` property, highlight, FocusScope, scroll-into-view

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (grid area `~lines 58-262`, lightbox `~lines 330-399`)

**Interfaces:**
- Consumes: nothing from Rust yet (callbacks are declared here, handled in Task 3).
- Produces (the Slint API the generated Rust bindings will expose, consumed in Task 3):
  - `in property <int> selected-index` (default `-1`)
  - `in property <bool> grid-focus-pulse`
  - `callback grid-nav(int /*dir*/, int /*cols*/)`
  - `callback grid-open-detail()`
  - `callback grid-open-lightbox()`

This is a UI-markup task with no Rust unit test; verification is "the workspace still compiles" (Slint markup is compiled by the `build.rs`), and the new callbacks are wired in Task 3. Keep the callbacks as no-ops at the markup level (they only emit).

- [ ] **Step 1: Add properties and callbacks to the root component**

In `imgfind-gui/ui/app.slint`, near the existing `in property`/`callback` declarations (the block around lines 40-56), add:

```slint
// Keyboard-navigation selection: index into `tiles` of the highlighted tile,
// or -1 for none. Driven by Rust (mouse click + grid-nav handler).
in property <int> selected-index: -1;
// Toggled by Rust after a search returns results, to move keyboard focus to
// the grid so hjkl/arrows work immediately.
in property <bool> grid-focus-pulse;

// dir encoding: 0=Left, 1=Right, 2=Up, 3=Down. Second arg is the live column count.
callback grid-nav(int, int);
callback grid-open-detail();
callback grid-open-lightbox();
```

- [ ] **Step 2: Highlight the selected tile**

In the grid `for tile[i] in root.tiles: Rectangle { … }` (around lines 226-253), add a border keyed on the selection. Add these two property lines to that Rectangle (alongside `border-radius: 6px;`):

```slint
border-width: i == root.selected-index ? 3px : 0px;
border-color: #5b8cff;
```

- [ ] **Step 3: Wrap the grid area in a FocusScope and name the ScrollView**

Currently `grid-area := Rectangle { … ScrollView { … } }` (lines 216-262). Give the `ScrollView` a name and add a sibling `FocusScope` that captures the keys. The FocusScope must overlay the ScrollView so it can hold focus while the ScrollView still scrolls/clicks.

Replace the `grid-area` Rectangle body so it contains both a named ScrollView and a FocusScope. Concretely, change `ScrollView {` to `grid-scroll := ScrollView {`, and wrap the keyboard handling in a `grid-focus := FocusScope` placed as the first child of `grid-area` (a FocusScope with no visual children still receives keys when focused):

```slint
grid-area := Rectangle {
    horizontal-stretch: 1;
    background: transparent;

    grid-focus := FocusScope {
        init => { self.focus(); }
        // Re-grab focus whenever Rust pulses grid-focus-pulse (after a search).
        property <bool> pulse <=> root.grid-focus-pulse;
        changed pulse => { self.focus(); }

        key-pressed(event) => {
            if (event.text == "h" || event.text == "H" || event.text == Key.LeftArrow) {
                root.grid-nav(0, root.cols);
                return accept;
            }
            if (event.text == "l" || event.text == "L" || event.text == Key.RightArrow) {
                root.grid-nav(1, root.cols);
                return accept;
            }
            if (event.text == "k" || event.text == "K" || event.text == Key.UpArrow) {
                root.grid-nav(2, root.cols);
                return accept;
            }
            if (event.text == "j" || event.text == "J" || event.text == Key.DownArrow) {
                root.grid-nav(3, root.cols);
                return accept;
            }
            if (event.text == Key.Return) {
                root.grid-open-detail();
                return accept;
            }
            if (event.text == " ") {
                root.grid-open-lightbox();
                return accept;
            }
            if (event.text == Key.Escape && root.detail-open) {
                root.detail-close();
                return accept;
            }
            return reject;
        }
    }

    grid-scroll := ScrollView {
        width: 100%;
        height: 100%;
        viewport-height: root.grid-height + (root.show-load-more ? 60px : 0px);
        viewport-width: root.cols * root.tile-stride;

        // … existing `for tile[i]` and `if root.show-load-more` children unchanged …
    }
}
```

(Keep the existing `for tile[i] …` and `if root.show-load-more …` children exactly as they are, now inside `grid-scroll`.)

- [ ] **Step 4: Scroll the selected tile into view**

Add a `changed selected-index` handler on the root component (or on `grid-area`) that clamps `grid-scroll.viewport-y`. Add this block inside the root component body (top-level, e.g. just after the property declarations). It reads the selected tile's row and adjusts the viewport so the tile is fully visible:

```slint
// Keep the selected tile within the scroll viewport.
changed selected-index => {
    if (root.selected-index >= 0) {
        // Tile top/bottom in viewport coordinates.
        property <length> tile-y: floor(root.selected-index / root.cols) * root.tile-stride;
        property <length> tile-bottom: tile-y + root.tile-size;
        // grid-scroll.viewport-y is <= 0 (content shifted up). Visible window is
        // [-viewport-y, -viewport-y + visible-height].
        property <length> top: -grid-scroll.viewport-y;
        property <length> bottom: top + grid-scroll.visible-height;
        if (tile-y < top) {
            grid-scroll.viewport-y = -tile-y;
        } else if (tile-bottom > bottom) {
            grid-scroll.viewport-y = -(tile-bottom - grid-scroll.visible-height);
        }
    }
}
```

> Note: Slint does not allow `property` declarations inside a `changed` callback body. If the compiler rejects the inline `property` lines, inline the expressions directly (compute `tile-y`, `top`, `bottom` as sub-expressions in the `if` conditions and assignments) rather than naming them. Keep the same logic.

- [ ] **Step 5: Add vim h/l to the lightbox FocusScope**

In the lightbox `lb-keys := FocusScope { key-pressed(event) => { … } }` (around lines 369-384), extend the existing arrow handlers so `h`/`l` also work. Change the `LeftArrow` branch condition to also match `"h"`/`"H"` and the `RightArrow` branch to also match `"l"`/`"L"`:

```slint
} else if (event.text == Key.LeftArrow || event.text == "h" || event.text == "H") {
    root.lightbox-prev();
    accept
} else if (event.text == Key.RightArrow || event.text == "l" || event.text == "L") {
    root.lightbox-next();
    accept
}
```

- [ ] **Step 6: Verify the workspace compiles**

Run: `cargo build -p imgfind-gui`
Expected: builds successfully (Slint `build.rs` compiles the markup; any syntax error fails here). Resolve any Slint syntax issues (esp. the Step 4 `property`-in-`changed` note) before committing.

- [ ] **Step 7: Lint & format**

Run: `cargo clippy -p imgfind-gui --all-targets && cargo fmt --all`
Expected: no warnings, no diff.

- [ ] **Step 8: Commit**

```bash
git add imgfind-gui/ui/app.slint
git commit -m "feat(gui): grid selection highlight, FocusScope nav keys, scroll-into-view, lightbox h/l"
```

---

### Task 3: Rust wiring — selection state, grid-nav / open-detail / open-lightbox handlers, lightbox mirroring, focus pulse

**Files:**
- Modify: `imgfind-gui/src/main.rs`

**Interfaces:**
- Consumes (from Task 1): `nav::{NavDir, move_selection}`.
- Consumes (from Task 2): generated Slint setters/handlers `set_selected_index(i32)`, `set_grid_focus_pulse(bool)`, `on_grid_nav(|dir, cols| …)`, `on_grid_open_detail(|| …)`, `on_grid_open_lightbox(|| …)`.
- Produces: fully wired keyboard navigation.

This task is Rust glue against Slint's event loop; it has no standalone unit test (the testable logic lives in `nav::move_selection`, Task 1). Verification is `cargo build`/`clippy` plus the manual run in Task 4. Each step shows the exact code.

- [ ] **Step 1: Add the shared `selected` state**

Near where `lb_index` is declared (main.rs ~line 163) add a sibling:

```rust
// Index into `state.results` of the keyboard/mouse-selected tile (mirrors the
// Slint `selected-index` property). Shares the index space with `lb_index`.
let selected: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
```

- [ ] **Step 2: Make `tile-selected` (mouse click) also set the selection**

In the existing `window.on_tile_selected(move |index| { … })` handler (~lines 354-400), at the top set the shared selection and the Slint property so mouse and keyboard share one highlight. Add (cloning `selected` into the closure as needed, following the existing clone pattern):

```rust
*selected_ref.lock().unwrap() = Some(index as usize);
if let Some(w) = weak.upgrade() {
    w.set_selected_index(index);
}
```

(Add a `let selected_ref = selected.clone();` before the closure, matching the existing `state_ref`/`detail_ref` clone style.)

- [ ] **Step 3: Wire `grid-nav`**

After the existing callback wiring, add the `grid-nav` handler. It computes the new index with the pure function, updates state + the Slint property, and — only if the detail panel is currently open — refreshes the panel for the new tile by invoking the same path as a click. The simplest correct approach: compute the new index, store it, set the property, and if `detail-open`, call the tile-selected logic. To avoid duplicating the detail-load code, factor the click body into a reusable closure/fn OR re-invoke via `window.invoke_tile_selected(new_idx)`.

Use `invoke_tile_selected` to reuse Task-2/existing logic without duplication:

```rust
{
    let state_ref = state.clone();
    let selected_ref = selected.clone();
    let weak = window.as_weak();
    window.on_grid_nav(move |dir_i, cols_i| {
        let Some(dir) = nav::NavDir::from_i32(dir_i) else { return };
        let len = state_ref.lock().unwrap().results.len();
        let cur = *selected_ref.lock().unwrap();
        let new = nav::move_selection(cur, dir, cols_i.max(0) as usize, len);
        *selected_ref.lock().unwrap() = new;
        if let Some(w) = weak.upgrade() {
            w.set_selected_index(new.map(|i| i as i32).unwrap_or(-1));
            // Live-update the detail panel only if it is already open.
            if let Some(i) = new {
                if w.get_detail_open() {
                    w.invoke_tile_selected(i as i32);
                }
            }
        }
    });
}
```

> Note on `invoke_tile_selected`: re-invoking the click handler re-sets the selection (Step 2) to the same index — harmless. If the generated API name differs, use the actual generated `invoke_<callback>` for the `tile-selected` callback. Verify the exact generated method name with `cargo build` errors and adjust.

- [ ] **Step 4: Wire `grid-open-detail` (Enter) and `grid-open-lightbox` (Space)**

```rust
{
    let selected_ref = selected.clone();
    let weak = window.as_weak();
    window.on_grid_open_detail(move || {
        if let (Some(i), Some(w)) = (*selected_ref.lock().unwrap(), weak.upgrade()) {
            w.invoke_tile_selected(i as i32);
        }
    });
}
{
    let selected_ref = selected.clone();
    let weak = window.as_weak();
    window.on_grid_open_lightbox(move || {
        if let (Some(i), Some(w)) = (*selected_ref.lock().unwrap(), weak.upgrade()) {
            w.invoke_tile_activated(i as i32);
        }
    });
}
```

(`tile-activated` is the existing double-click → lightbox callback; re-invoking it opens the lightbox at index `i` and sets `lb_index`.)

- [ ] **Step 5: Mirror lightbox navigation into the grid selection**

In the existing `on_lightbox_prev` and `on_lightbox_next` handlers (~lines 547-604), after the new `lb_index` is computed and stored, also update `selected` + the Slint `selected-index`. Add (with `selected` cloned into each closure):

```rust
// Mirror into the grid selection so closing the lightbox lands on this tile.
*selected_ref.lock().unwrap() = Some(new_idx);
if let Some(w) = weak.upgrade() {
    w.set_selected_index(new_idx as i32);
}
```

Use the same `new_idx` the handler already computed for `lb_index`. (If the handler stores into `lb_index` via a local, reuse that value.)

- [ ] **Step 6: Seed selection when the lightbox opens from a double-click**

In `on_tile_activated` (the double-click → lightbox handler), ensure `selected` + `selected-index` are set to the activated index too (so a subsequent Esc keeps it selected):

```rust
*selected_ref.lock().unwrap() = Some(index as usize);
if let Some(w) = weak.upgrade() {
    w.set_selected_index(index);
}
```

- [ ] **Step 7: Pulse grid focus after a search returns results; reset selection on a new search**

Find where search results are applied to the grid (the handler that sets `window.set_tiles(...)` after a text search completes). After setting non-empty tiles for a **new text search**, reset selection and pulse focus:

```rust
*selected_ref.lock().unwrap() = None;
if let Some(w) = weak.upgrade() {
    w.set_selected_index(-1);
    if !results_empty {
        w.set_grid_focus_pulse(!w.get_grid_focus_pulse());
    }
}
```

For `search-similar` and `load-more` result appliers: do NOT reset the selection to `None`; instead re-clamp it — if the stored `selected` index is now `>= results.len()`, set it to `None`/`-1`, otherwise leave it. Pulse focus the same way for `search-similar` (it replaces the grid); for `load-more` (appends), pulsing focus is optional — leave focus as-is to avoid yanking it mid-scroll.

> If determining `results_empty` is awkward at the call site, derive it from the results vector length you already have when calling `set_tiles`.

- [ ] **Step 8: Build, lint, format**

Run: `cargo build -p imgfind-gui && cargo clippy -p imgfind-gui --all-targets && cargo fmt --all`
Expected: builds clean, no clippy warnings, no fmt diff. Fix any generated-method-name mismatches (`invoke_tile_selected`, `invoke_tile_activated`, `get_detail_open`, `get_grid_focus_pulse`) revealed by the compiler.

- [ ] **Step 9: Run the full test suite**

Run: `cargo test --workspace`
Expected: PASS (includes Task 1's `nav::` tests; nothing else should regress).

- [ ] **Step 10: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): wire grid keyboard nav, Enter/Space actions, lightbox↔selection mirroring, focus pulse"
```

---

### Task 4: Manual verification + docs

**Files:**
- Modify: `CLAUDE.md` (Native GUI bullet)

**Interfaces:**
- Consumes: the finished feature.
- Produces: a documented, manually-verified feature.

- [ ] **Step 1: Manual run-through**

Run: `cargo run -p imgfind-gui -- --dir <a directory with an imgfind DB>`
Verify each behaviour:
- After a search, hjkl/arrows move a highlighted border; focus is on the grid without clicking.
- Right at a row end jumps to the next row's first tile; left at a row start jumps to the previous row's last tile.
- Left on the first tile and right on the last tile do nothing (no wrap).
- Up/down move by a row and stop at the top/bottom.
- The selected tile scrolls into view when it moves off-screen.
- `Enter` opens the detail panel for the selected tile; with the panel open, moving the selection live-updates it; `Esc` closes the panel and keeps the selection highlighted.
- `Space` opens the lightbox at the selected tile.
- In the lightbox, `h`/`l` (and arrows) step prev/next; `Esc` closes and the grid shows the last-viewed tile selected and scrolled into view.
- Clicking the search box lets you type again (focus returns to it).

If any behaviour is wrong, fix it (loop back to the relevant task's code) before committing docs.

- [ ] **Step 2: Update CLAUDE.md**

In `CLAUDE.md`, in the **Native GUI** bullet, add a sentence describing keyboard navigation and link the spec:

```
Keyboard navigation: in the grid, vim `h/j/k/l` and arrow keys move a highlighted selection (left/right cross rows, clamped at the global ends; up/down clamp at top/bottom rows), `Enter` opens the detail panel, `Space` opens the lightbox, `Esc` closes the panel keeping the selection; the grid grabs focus after a search. In the lightbox, `h`/`l` join the arrows for prev/next and mirror the grid selection so closing returns to the last-viewed tile. See `docs/superpowers/specs/2026-06-18-gui-keyboard-navigation-design.md`.
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: note GUI grid + lightbox keyboard navigation"
```

---

## Self-Review

**Spec coverage:**
- Grid hjkl/arrow nav with highlight → Task 1 (math) + Task 2 (highlight, FocusScope) + Task 3 (wiring). ✓
- Left/right cross rows, clamp at global ends → Task 1 `move_selection` + tests. ✓
- Up/down clamp at rows → Task 1 + tests. ✓
- Scroll-into-view → Task 2 Step 4. ✓
- Enter→panel, Space→lightbox, Esc→close keeping selection → Task 2 (keys) + Task 3 (handlers). ✓
- Detail panel live-updates only if open → Task 3 Step 3 (`get_detail_open()` guard). ✓
- Focus to grid after search → Task 2 (pulse observer) + Task 3 Step 7. ✓
- Lightbox h/l + mirror selection so Esc lands on last-viewed → Task 2 Step 5 + Task 3 Steps 5-6. ✓
- Docs → Task 4. ✓

**Placeholder scan:** No TBD/TODO; every code step shows code. The two "verify the generated method name" notes are real guidance (Slint's generated API names depend on the markup), not placeholders — they instruct using compiler feedback to confirm exact names.

**Type consistency:** `move_selection(cur: Option<usize>, dir: NavDir, cols: usize, len: usize) -> Option<usize>` and `NavDir::from_i32` are used identically in Tasks 1 and 3. The Slint `selected-index` (i32 in generated Rust), `grid-focus-pulse` (bool), and callbacks `grid-nav(int,int)`, `grid-open-detail()`, `grid-open-lightbox()` declared in Task 2 match their consumption in Task 3. Selection is stored as `Option<usize>` in Rust and surfaced as `i32` (`-1` for none) to Slint consistently.
