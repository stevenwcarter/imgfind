# GUI Keyboard Selection Modes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add vim-style range (`Shift+V`) and free (`v`) keyboard multi-selection to the imgfind-gui grid, apply tag chords/modal to the whole selection, and show an always-visible statusline.

**Architecture:** A new pure `selection.rs` module holds the selection state machine and index math (sibling to `nav.rs`/`chords.rs`). A per-tile `selected` bool flows into the existing windowed tile rebuild; a `selection_dirty` flag forces the loader tick to rebuild so yellow borders track the set (the tick owns the decoded-image cache). The green cursor and the statusline update instantly from key handlers. Tag application fans out from the single-image `apply_tags_to_focused` to a new `apply_tags_to_selection`.

**Tech Stack:** Rust (edition 2024), Slint 1.x, `rusqlite`, the existing `imgfind` core crate.

## Global Constraints

- Rust edition 2024; code must be `cargo clippy` + `cargo fmt` clean (dispatch Rust coding to the `rust-developer` agent per the user's global rules).
- Errors use `anyhow` with `Context`/`with_context`; logging via `tracing`.
- Cursor (`selected-index` / `Arc<Mutex<Option<usize>>>` named `selected`) and the marked set (`Selection.set`) are **distinct** concepts — never conflate them.
- Selection is grid-scoped and ephemeral (NOT persisted to `ui_state`).
- Relative-path invariant: tag writes use `RowMeta.path` strings directly (already relative), via `Backend::add_tag` — no new path conversion.
- Slint default-font glyph caution: any non-ASCII glyph in UI text must be verified to render (no tofu); fall back to ASCII if unsure (see the Slint glyph-coverage memory).
- All pure logic gets unit tests; GUI wiring is verified manually (documented at the end).

---

### Task 1: Pure selection model (`selection.rs`)

**Files:**
- Create: `imgfind-gui/src/selection.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod selection;` near the other `mod` lines at the top, e.g. after `mod nav;`)
- Test: inline `#[cfg(test)] mod tests` in `selection.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum SelectionMode { Normal, Range { anchor: usize }, Free }` (derives `Clone, Copy, Debug, PartialEq, Eq, Default`; `#[default] Normal`)
  - `pub struct Selection { mode: SelectionMode, set: BTreeSet<usize> }` (derives `Clone, Debug, PartialEq, Eq, Default`)
  - `pub fn enter_range(&mut self, cursor: usize)`
  - `pub fn enter_free(&mut self)`
  - `pub fn cursor_moved(&mut self, cursor: usize)`
  - `pub fn toggle(&mut self, cursor: usize)`
  - `pub fn clear(&mut self)`
  - `pub fn is_active(&self) -> bool`
  - `pub fn is_empty(&self) -> bool`
  - `pub fn contains(&self, i: usize) -> bool`
  - `pub fn set(&self) -> &BTreeSet<usize>`
  - `pub fn mode(&self) -> SelectionMode`

- [ ] **Step 1: Write the failing tests**

Create `imgfind-gui/src/selection.rs` with only the test module first (it will not compile until Step 3 adds the types — that is the intended red state):

```rust
//! Pure selection state + grid-index math for the GUI multi-select modes.
//! Range mode materializes the linear contiguous index run between anchor and
//! cursor (crosses row boundaries — NOT a 2-D rectangle). Free mode toggles
//! individual indices. No I/O, no Slint, no locks.

use std::collections::BTreeSet;

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(s: &Selection) -> Vec<usize> {
        s.set().iter().copied().collect()
    }

    #[test]
    fn enter_range_seeds_anchor_only() {
        let mut s = Selection::default();
        s.enter_range(5);
        assert!(s.is_active());
        assert_eq!(s.mode(), SelectionMode::Range { anchor: 5 });
        assert_eq!(set_of(&s), vec![5]);
    }

    #[test]
    fn range_forward_is_contiguous_run() {
        let mut s = Selection::default();
        s.enter_range(2);
        s.cursor_moved(8);
        assert_eq!(set_of(&s), vec![2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn range_backward_same_set() {
        let mut s = Selection::default();
        s.enter_range(8);
        s.cursor_moved(2);
        assert_eq!(set_of(&s), vec![2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn range_back_onto_anchor_collapses() {
        let mut s = Selection::default();
        s.enter_range(4);
        s.cursor_moved(7);
        s.cursor_moved(4);
        assert_eq!(set_of(&s), vec![4]);
    }

    #[test]
    fn free_starts_empty_and_toggles() {
        let mut s = Selection::default();
        s.enter_free();
        assert!(s.is_active());
        assert!(s.is_empty());
        s.toggle(3);
        s.toggle(9);
        assert_eq!(set_of(&s), vec![3, 9]);
        s.toggle(3);
        assert_eq!(set_of(&s), vec![9]);
    }

    #[test]
    fn cursor_moved_noop_in_free_and_normal() {
        let mut s = Selection::default();
        s.cursor_moved(5); // Normal
        assert!(s.is_empty());
        s.enter_free();
        s.toggle(2);
        s.cursor_moved(7); // Free: must not change the set
        assert_eq!(set_of(&s), vec![2]);
    }

    #[test]
    fn toggle_noop_in_range_and_normal() {
        let mut s = Selection::default();
        s.toggle(1); // Normal
        assert!(s.is_empty());
        s.enter_range(4);
        s.toggle(9); // Range: must not add
        assert_eq!(set_of(&s), vec![4]);
    }

    #[test]
    fn clear_resets_to_normal_empty() {
        let mut s = Selection::default();
        s.enter_range(4);
        s.cursor_moved(6);
        s.clear();
        assert!(!s.is_active());
        assert_eq!(s.mode(), SelectionMode::Normal);
        assert!(s.is_empty());
    }

    #[test]
    fn re_entering_mode_resets_set() {
        let mut s = Selection::default();
        s.enter_free();
        s.toggle(1);
        s.toggle(2);
        s.enter_range(7); // re-enter: anchor only
        assert_eq!(set_of(&s), vec![7]);
    }

    #[test]
    fn contains_reflects_set() {
        let mut s = Selection::default();
        s.enter_range(2);
        s.cursor_moved(4);
        assert!(s.contains(3));
        assert!(!s.contains(5));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui selection::`
Expected: FAIL to compile — `Selection` / `SelectionMode` not found.

- [ ] **Step 3: Write the minimal implementation**

Insert above the `#[cfg(test)]` block:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionMode {
    #[default]
    Normal,
    Range {
        anchor: usize,
    },
    Free,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    mode: SelectionMode,
    set: BTreeSet<usize>,
}

impl Selection {
    pub fn enter_range(&mut self, cursor: usize) {
        self.mode = SelectionMode::Range { anchor: cursor };
        self.set.clear();
        self.set.insert(cursor);
    }

    pub fn enter_free(&mut self) {
        self.mode = SelectionMode::Free;
        self.set.clear();
    }

    pub fn cursor_moved(&mut self, cursor: usize) {
        if let SelectionMode::Range { anchor } = self.mode {
            let (lo, hi) = (anchor.min(cursor), anchor.max(cursor));
            self.set = (lo..=hi).collect();
        }
    }

    pub fn toggle(&mut self, cursor: usize) {
        if self.mode == SelectionMode::Free && !self.set.remove(&cursor) {
            self.set.insert(cursor);
        }
    }

    pub fn clear(&mut self) {
        self.mode = SelectionMode::Normal;
        self.set.clear();
    }

    pub fn is_active(&self) -> bool {
        self.mode != SelectionMode::Normal
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn contains(&self, i: usize) -> bool {
        self.set.contains(&i)
    }

    pub fn set(&self) -> &BTreeSet<usize> {
        &self.set
    }

    pub fn mode(&self) -> SelectionMode {
        self.mode
    }
}
```

Add `mod selection;` to `main.rs` alongside the existing module declarations (e.g. right after `mod nav;`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui selection::`
Expected: PASS (all selection tests green).

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`
(If `mod selection;` triggers dead-code warnings because nothing outside tests calls it yet, prefer `#[allow(dead_code)]` on the impl block with a `// wired in Task 3/4` comment over leaving warnings.)

```bash
git add imgfind-gui/src/selection.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): pure selection state machine (range/free index math)"
```

---

### Task 2: Statusline formatter (`format_statusline`)

**Files:**
- Modify: `imgfind-gui/src/main.rs` (add a free function near the other pure helpers like `format_bytes`, plus tests)
- Test: extend the existing `#[cfg(test)]` tests in `main.rs` (search for `mod tests` / existing `format_bytes` tests; add there)

**Interfaces:**
- Consumes: `selection::Selection`, `selection::SelectionMode`, `imgfind::sort::RowMeta`, the existing `fn format_bytes(bytes: i64) -> String`.
- Produces: `fn format_statusline(sel: &selection::Selection, results: &[RowMeta]) -> String`

- [ ] **Step 1: Write the failing tests**

Add to the `main.rs` test module (use `RowMeta { id, path, size, ext }`; only `size` matters here):

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui statusline`
Expected: FAIL to compile — `format_statusline` not found.

- [ ] **Step 3: Write the minimal implementation**

Add near `format_bytes` in `main.rs`. Use ASCII separators ` | ` for the selection split and ` - ` between stats to avoid any glyph risk (the spec's `·`/`┃` are replaced with ASCII per the glyph-coverage constraint):

```rust
/// Build the always-visible bottom statusline from the current selection and
/// the full result set. ASCII separators only (Slint default-font glyph safety).
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui statusline`
Expected: PASS.

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): pure statusline formatter (result-set + selection stats)"
```

---

### Task 3: Slint surface + tile/statusline plumbing (build stays green)

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (`Tile` struct, `MainWindow` properties/callbacks, tile border colors, bottom statusline bar)
- Modify: `imgfind-gui/src/main.rs` (`Tile` construction at ~2305, `build_tiles_model` signature, `rebuild_window` signature, `LoaderTick` struct + tick gate, create the `selection` holder + `selection_dirty` flag, push statusline once at startup)

**Interfaces:**
- Consumes: `selection::Selection` (Task 1), `format_statusline` (Task 2).
- Produces (for Task 4):
  - Slint callbacks `selection-enter-range()`, `selection-enter-free()`, `grid-space()`, `grid-escape()`; property `statusline` (string).
  - Rust: `let selection: Arc<Mutex<selection::Selection>>`; `let selection_dirty: Arc<AtomicBool>`; helper `fn push_statusline(w: &MainWindow, selection: &Arc<Mutex<selection::Selection>>, state: &Arc<Mutex<SearchState>>)`.
  - `build_tiles_model(results, images, offset, selected: &BTreeSet<usize>)` and `rebuild_window(window, state_ref, cache, range, selected: &BTreeSet<usize>)`.

This task wires plumbing with an always-empty selection, so the app compiles and shows a `NORMAL` statusline with correct counts; no keys are bound yet.

- [ ] **Step 1: Slint — `Tile` struct gains `selected`**

In `imgfind-gui/ui/app.slint`, the `export struct Tile`:

```slint
export struct Tile {
    path: string,
    image: image,
    size-kb: int,
    index: int,
    selected: bool,
}
```

- [ ] **Step 2: Slint — properties, callbacks, border colors, statusline bar**

Add near the other selection/nav declarations (around `selected-index` / `grid-nav`):

```slint
in property <string> statusline;
callback selection-enter-range();
callback selection-enter-free();
callback grid-space();
callback grid-escape();
```

Change the grid tile border (currently blue-cursor-only) to green cursor / yellow set:

```slint
border-width: (tile.index == root.selected-index || tile.selected) ? 3px : 0px;
border-color: tile.index == root.selected-index ? #3ad17a : #f2c94c;
```

Add an always-visible statusline bar as the **last** child of the `app-keys` FocusScope (after the `HorizontalLayout` that holds rail + main column, so it spans the full width at the bottom). Keep it inside `app-keys` but outside the lightbox overlay:

```slint
Rectangle {
    // anchored to the bottom; full width
    y: parent.height - self.height;
    width: parent.width;
    height: 24px;
    background: #161a22;
    HorizontalLayout {
        padding-left: 10px;
        padding-right: 10px;
        Text {
            text: root.statusline;
            color: #9aa4b2;
            font-size: 12px;
            vertical-alignment: center;
        }
    }
}
```

(If absolute `y` positioning fights the existing `HorizontalLayout`, instead wrap the existing rail+main `HorizontalLayout` and this bar in a `VerticalLayout` so the bar sits below naturally — pick whichever keeps the grid from overlapping the bar. Verify visually in Step 6. Reserve ~24px so the grid doesn't hide under it.)

- [ ] **Step 3: Rust — thread the selection set through the tile builder**

In `main.rs`:

1. Add imports: `use std::collections::BTreeSet;` and `use std::sync::atomic::{AtomicBool, Ordering};` (check whether `AtomicU64`/`Ordering` are already imported — extend, don't duplicate).
2. Change `build_tiles_model` to accept `selected: &BTreeSet<usize>` and set the field:

```rust
fn build_tiles_model(
    results: &[RowMeta],
    images: Vec<Option<Image>>,
    offset: usize,
    selected: &BTreeSet<usize>,
) -> ModelRc<Tile> {
    let tiles: Vec<Tile> = results
        .iter()
        .zip(images)
        .enumerate()
        .map(|(i, (r, maybe_img))| {
            let size_kb = r.size.unwrap_or(0) / 1024;
            let index = offset + i;
            Tile {
                path: r.path.clone().into(),
                image: maybe_img.unwrap_or_default(),
                size_kb: size_kb as i32,
                index: index as i32,
                selected: selected.contains(&index),
            }
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(tiles)))
}
```

3. Change `rebuild_window` to accept and forward `selected: &BTreeSet<usize>`:

```rust
fn rebuild_window(
    window: &MainWindow,
    state_ref: &Arc<Mutex<SearchState>>,
    cache: &mut loader::ThumbCache,
    range: &Range<usize>,
    selected: &BTreeSet<usize>,
) {
    // ... unchanged slice/images ...
    let model = build_tiles_model(&slice, images, range.start, selected);
    window.set_tiles(model);
}
```

- [ ] **Step 4: Rust — create holders, extend `LoaderTick`, force-rebuild on dirty**

1. Where the other UI-thread holders are created (near `selected: Arc<Mutex<Option<usize>>>` and `pending_chord`), add:

```rust
let selection: Arc<Mutex<selection::Selection>> = Arc::new(Mutex::new(selection::Selection::default()));
let selection_dirty: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
```

2. Add a `selection` field (clone of the Arc) and a `selection_dirty` field to the `LoaderTick<'a>` struct, and populate them where `LoaderTick { .. }` is constructed for the timer.
3. In `loader_tick`, read+forward the set and consume the dirty flag so a selection change forces one rebuild even when the window is unchanged:

```rust
let selection_changed = t.selection_dirty.swap(false, Ordering::Relaxed);
let range_changed = t.last_range.as_ref() != Some(&range);
if range_changed || gen_changed || cached_new || selection_changed {
    let sel = t.selection.lock().unwrap().set().clone();
    rebuild_window(t.window, t.state_ref, t.cache, &range, &sel);
    // ... existing logging + *t.last_range = Some(range.clone());
}
```

4. Add the statusline helper and call it once after initial results load (and anywhere the result set is first installed at startup):

```rust
fn push_statusline(
    w: &MainWindow,
    selection: &Arc<Mutex<selection::Selection>>,
    state: &Arc<Mutex<SearchState>>,
) {
    let sel = selection.lock().unwrap();
    let s = state.lock().unwrap();
    w.set_statusline(format_statusline(&sel, &s.results).into());
}
```

5. Update the other `rebuild_window`/`build_tiles_model` call sites (the empty `set_tiles(ModelRc::default())` calls need NO change; only the real `rebuild_window` call in the tick passes the set). Pass `&selection.lock().unwrap().set().clone()`-style data only at the tick site already handled in Step 4.3.

- [ ] **Step 5: Build + existing tests**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; all prior tests still pass.

- [ ] **Step 6: Manual smoke + clippy + commit**

Run the GUI (`cargo run -p imgfind-gui -- --dir <a-dir-with-an-imgfind-db>`); confirm: statusline bar visible at the bottom reading `NORMAL - N images - <size>`; grid cursor border is now **green**; no tile overlap with the bar; no tofu in the statusline. Use the `slint` skill if focus/layout misbehaves.

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): tile selected flag, green cursor, bottom statusline (plumbing)"
```

---

### Task 4: Wire selection key handlers

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (key routing in `app-keys` `capture-key-pressed`)
- Modify: `imgfind-gui/src/main.rs` (handlers + clear-on-result-replacement)

**Interfaces:**
- Consumes: everything from Tasks 1–3 (`selection` holder, `selection_dirty`, `push_statusline`, the four new callbacks).
- Produces: live range/free selection behavior; selection cleared whenever `results` is replaced.

- [ ] **Step 1: Slint — route the new keys (grid branch only)**

In `capture-key-pressed`, in the **grid** branch (after the lightbox/modal/suppressed guards, before/with the existing nav `if`s):

- Replace the `Space` handler:

```slint
if (event.text == " ") {
    root.grid-space();
    return accept;
}
```

- Replace the existing grid `Escape` handler (currently `if (event.text == Key.Escape && root.detail-open)`):

```slint
if (event.text == Key.Escape) {
    root.grid-escape();
    return accept;
}
```

- Add `v` / `Shift+V` handling. `v` arrives as `event.text == "v"`; shift makes it `"V"` and/or sets `event.modifiers.shift`. Branch on shift first:

```slint
if (event.text == "v" || event.text == "V") {
    if (event.modifiers.shift) {
        root.selection-enter-range();
    } else {
        root.selection-enter-free();
    }
    return accept;
}
```

Place the `v`/`V` block **before** the `is-chord-key`/nav blocks. (`v` is not a chord or nav key, so order is safe, but keep it explicit.) Leave the lightbox branch unchanged — selection is grid-only.

- [ ] **Step 2: Rust — `on_selection_enter_range` / `on_selection_enter_free`**

Each reads the current cursor (`selected`), mutates `selection`, marks dirty, pushes statusline. Range needs a cursor; if none, seed cursor to 0 first (so `Shift+V` on a fresh grid anchors the first tile). Clone Arcs as needed:

```rust
window.on_selection_enter_range({
    let selection = Arc::clone(&selection);
    let selected = Arc::clone(&selected);
    let selection_dirty = Arc::clone(&selection_dirty);
    let state_ref = Arc::clone(&state);
    let weak = window.as_weak();
    move || {
        let cur = selected.lock().unwrap().unwrap_or(0);
        *selected.lock().unwrap() = Some(cur);
        selection.lock().unwrap().enter_range(cur);
        selection_dirty.store(true, Ordering::Relaxed);
        if let Some(w) = weak.upgrade() {
            w.set_selected_index(cur as i32);
            push_statusline(&w, &selection, &state_ref);
        }
    }
});

window.on_selection_enter_free({
    let selection = Arc::clone(&selection);
    let selection_dirty = Arc::clone(&selection_dirty);
    let state_ref = Arc::clone(&state);
    let weak = window.as_weak();
    move || {
        selection.lock().unwrap().enter_free();
        selection_dirty.store(true, Ordering::Relaxed);
        if let Some(w) = weak.upgrade() {
            push_statusline(&w, &selection, &state_ref);
        }
    }
});
```

(Match the exact name of the existing search-state Arc — in `on_grid_nav` it is `state_ref`/`selected_ref`; use the actual binding names in scope.)

- [ ] **Step 3: Rust — `on_grid_space` (toggle in Free, else lightbox)**

```rust
window.on_grid_space({
    let selection = Arc::clone(&selection);
    let selected = Arc::clone(&selected);
    let selection_dirty = Arc::clone(&selection_dirty);
    let state_ref = Arc::clone(&state);
    let weak = window.as_weak();
    move || {
        let is_free = matches!(selection.lock().unwrap().mode(), selection::SelectionMode::Free);
        if is_free {
            if let Some(cur) = *selected.lock().unwrap() {
                selection.lock().unwrap().toggle(cur);
                selection_dirty.store(true, Ordering::Relaxed);
                if let Some(w) = weak.upgrade() {
                    push_statusline(&w, &selection, &state_ref);
                }
            }
        } else if let Some(w) = weak.upgrade() {
            w.invoke_grid_open_lightbox();
        }
    }
});
```

(Confirm the lightbox-open callback name; if `grid-open-lightbox` is itself only a callback with a Rust handler, factor the lightbox-open body into a helper both call, or `w.invoke_grid_open_lightbox()` to re-enter it. Use whichever the codebase already exposes.)

- [ ] **Step 4: Rust — `on_grid_escape` (clear selection, else close detail)**

```rust
window.on_grid_escape({
    let selection = Arc::clone(&selection);
    let selection_dirty = Arc::clone(&selection_dirty);
    let state_ref = Arc::clone(&state);
    let weak = window.as_weak();
    move || {
        let was_active = selection.lock().unwrap().is_active();
        if was_active {
            selection.lock().unwrap().clear();
            selection_dirty.store(true, Ordering::Relaxed);
            if let Some(w) = weak.upgrade() {
                push_statusline(&w, &selection, &state_ref);
            }
        } else if let Some(w) = weak.upgrade() {
            if w.get_detail_open() {
                w.invoke_detail_close();
            }
        }
    }
});
```

- [ ] **Step 5: Rust — range follows the cursor in `on_grid_nav`**

In the existing `on_grid_nav` handler (around line 898), after computing `new` and storing it into `selected`, add the selection follow + statusline + dirty (clone the needed Arcs into that closure):

```rust
// after: *selected_ref.lock().unwrap() = new;
if let Some(i) = new {
    selection.lock().unwrap().cursor_moved(i);
}
selection_dirty.store(true, Ordering::Relaxed);
if let Some(w) = weak.upgrade() {
    // ... existing set_selected_index + detail refresh ...
    push_statusline(&w, &selection, &state_ref);
}
```

- [ ] **Step 6: Rust — clear selection when the result set is replaced**

Search/browse/sort/similar/filter all rebuild `SearchState.results`; a stale selection would index dangling rows. At each result-replacement site (the same places that call `w.set_selected_index(-1)` or reset `selected` — grep for `set_selected_index(-1)` and the search/browse/sort/filter/similar result installers), add:

```rust
selection.lock().unwrap().clear();
selection_dirty.store(true, Ordering::Relaxed);
// and refresh the statusline for the new result set after results are installed:
push_statusline(&w, &selection, &state_ref);
```

Ensure `push_statusline` is also called after each new result set is installed so the counts/total update. (These sites are: initial load, `on_search`, browse/`apply_sort_change`, `search-similar`, and `filters-changed`. Verify by grepping `set_selected_index(-1)` — lines ~389, ~2241, and the search/similar handlers.)

- [ ] **Step 7: Build + tests**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; tests pass.

- [ ] **Step 8: Manual verification + clippy + commit**

Run the GUI. Verify:
- `Shift+V` then `h/j/k/l`/arrows grows a live yellow contiguous run with a green cursor; statusline shows `VISUAL (RANGE) - … | selected K - …`.
- `v` then `Space` hand-picks scattered tiles (re-press deselects); statusline `VISUAL (FREE)`.
- `Esc` clears selection and returns to `NORMAL`; `Space` in normal mode still opens the lightbox; `Enter` still opens detail.
- Running a new search/sort clears the selection.

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): wire range/free selection keys + live statusline"
```

---

### Task 5: Apply tag chords/modal to the whole selection

**Files:**
- Modify: `imgfind-gui/src/main.rs` (`apply_tags_to_selection` + branch the chord/modal dispatch)

**Interfaces:**
- Consumes: `selection` holder, `TagTargetCtx`, `apply_tags_to_focused`, `SearchState.results`, `Backend::add_tag`/`tags_for`.
- Produces: `fn apply_tags_to_selection(weak, backend, ctx, sel_paths: Vec<String>, tags: Vec<String>)`.

- [ ] **Step 1: Implement `apply_tags_to_selection`**

Mirror `apply_tags_to_focused` (line ~2651) but over many paths. Resolve indices→paths from `results` at the call site (so the fn takes the already-resolved `Vec<String>`):

```rust
/// Apply `tags` to every path in `sel_paths` (background thread, batched writes).
/// If the detail panel shows one of the affected paths, re-push its pills.
fn apply_tags_to_selection(
    weak: &Weak<MainWindow>,
    backend: &Backend,
    ctx: &TagTargetCtx,
    sel_paths: Vec<String>,
    tags: Vec<String>,
) {
    if tags.is_empty() || sel_paths.is_empty() {
        return;
    }
    let detail_path = ctx.detail.lock().unwrap().as_ref().map(|d| d.path.clone());
    let backend = backend.clone();
    let weak = weak.clone();
    std::thread::spawn(move || {
        for path in &sel_paths {
            for t in &tags {
                if let Err(e) = backend.add_tag(path, t) {
                    tracing::warn!("failed to add tag {t} to {path}: {e}");
                }
            }
        }
        if let Some(dp) = detail_path {
            if sel_paths.contains(&dp) {
                let fresh = backend.tags_for(&dp).unwrap_or_default();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = weak.upgrade() {
                        push_detail_tags(&w, &fresh);
                    }
                });
            }
        }
    });
}
```

- [ ] **Step 2: Branch the chord dispatch (`PaintBrush`, `RepeatLast`)**

Where `apply_tags_to_focused(&weak, &backend_key, &tag_ctx, tags…)` is called for `Action::PaintBrush(c)` and `Action::RepeatLast` (lines ~1425–1434), wrap with a selection check. Add a small helper to resolve selected paths to keep both call sites DRY:

```rust
fn selected_paths(
    selection: &Arc<Mutex<selection::Selection>>,
    state: &Arc<Mutex<SearchState>>,
) -> Vec<String> {
    let sel = selection.lock().unwrap();
    if !sel.is_active() || sel.is_empty() {
        return Vec::new();
    }
    let s = state.lock().unwrap();
    sel.set()
        .iter()
        .filter_map(|&i| s.results.get(i).map(|r| r.path.clone()))
        .collect()
}
```

Then at each tag-applying chord site:

```rust
let paths = selected_paths(&selection, &state);
if paths.is_empty() {
    apply_tags_to_focused(&weak, &backend_key, &tag_ctx, tags.clone());
} else {
    apply_tags_to_selection(&weak, &backend_key, &tag_ctx, paths, tags.clone());
}
```

(`selection` must be cloned into the `on_key` closure that owns the chord dispatch — add `let selection = Arc::clone(&selection);` to that closure's capture block, and `let state = Arc::clone(&state);` if not already captured.)

Per the approved decision, do **not** clear the selection after applying — leave mode/set intact.

- [ ] **Step 3: Branch the tag-modal commit**

In `on_tag_modal_commit` (line ~1486–1496), apply the same branch:

```rust
let paths = selected_paths(&selection, &state);
if paths.is_empty() {
    apply_tags_to_focused(&weak, &backend_modal, &tag_ctx, tags.clone());
} else {
    apply_tags_to_selection(&weak, &backend_modal, &tag_ctx, paths, tags.clone());
}
```

(Capture `selection`/`state` Arc clones into the modal-commit closure.)

- [ ] **Step 4: Build + tests**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; tests pass.

- [ ] **Step 5: Manual verification + clippy + commit**

Run the GUI:
- Select several images (range or free), press `mr` (or another configured brush) → every selected image gets the brush's tags. Confirm via the detail panel and/or by enabling a tag filter for one of those tags.
- With a selection active, `t` → type tags → Enter applies to all selected.
- Selection persists after apply (apply a second brush to the same set); `Esc` clears.
- With NO selection, `mr`/`t` still tag only the focused image (regression check).

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): fan out tag chords/modal to the whole selection"
```

---

### Task 6: Documentation

**Files:**
- Modify: `CLAUDE.md` (GUI section — selection modes, statusline, new module/key list)

**Interfaces:** none (docs only).

- [ ] **Step 1: Update `CLAUDE.md`**

In the Native GUI bullet (the Tagging/keyboard area), add a concise sentence describing the new behavior and a spec link. Suggested addition:

> **Keyboard selection** — `Shift+V` enters range-select (anchors the cursor; movement materializes the linear contiguous index run, crossing rows), `v` enters free-select (`Space` toggles the cursor tile). The cursor tile shows a **green** border, selected tiles **yellow**. While a selection is active, the tag chords (`mm`/`m<color>`) and the `t` modal apply to **all** selected images; selection persists after apply, `Esc` clears it. An always-visible **statusline** at the bottom shows mode, result count + total size, and (when selecting) selected count + size. Selection is grid-only and ephemeral (not persisted). New module `imgfind-gui/src/selection.rs` (pure `Selection`/`SelectionMode` state machine). See `docs/superpowers/specs/2026-06-20-gui-keyboard-selection-modes-design.md`.

Also update the "Keyboard navigation" sentence to note the cursor border is now green (was blue).

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document GUI keyboard selection modes + statusline"
```

---

## Self-Review

**Spec coverage:**
- Range mode (Shift+V, linear contiguous, live) → Tasks 1 (math), 3 (render), 4 (keys). ✓
- Free mode (v, Space toggle) → Tasks 1, 4. ✓
- Apply mm/m<color>/t to whole selection → Task 5. ✓
- Esc clears → Task 4 (`on_grid_escape`). ✓
- Cursor green / selected yellow → Task 3. ✓
- Always-visible statusline w/ result + selection stats → Tasks 2, 3, 4. ✓
- Selection cleared on result-set replacement (invariant) → Task 4 Step 6. ✓
- Distinct cursor vs set, relative-path tag writes, ephemeral selection → constraints honored across tasks. ✓

**Placeholder scan:** No TBD/TODO; every code step shows code; commands have expected output. ✓

**Type consistency:** `Selection`/`SelectionMode` method names match between Task 1 definitions and Tasks 3–5 uses (`enter_range`, `enter_free`, `cursor_moved`, `toggle`, `clear`, `is_active`, `is_empty`, `contains`, `set`, `mode`). `build_tiles_model`/`rebuild_window` signature change is applied at definition and the single real call site. `format_statusline(&Selection, &[RowMeta])` consistent. ✓

**Note for implementers:** exact Arc binding names (`state` vs `state_ref`, `selected` vs `selected_ref`) vary by closure scope in `main.rs`; use the names actually in scope at each call site — the plan's snippets show intent, not verbatim variable names.
