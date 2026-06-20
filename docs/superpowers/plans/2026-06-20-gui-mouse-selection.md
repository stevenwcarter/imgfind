# GUI Mouse Selection + Peek Arrows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add shift/ctrl-click selection, brush-swatch-click tagging, and peekable edge tabs (toggle rail / detail) to the imgfind-gui grid.

**Architecture:** Extend the pure `selection.rs` model with `range_to`/`ctrl_toggle`; capture click modifiers in Slint via `pointer-event` down → root bools read in `clicked`; route a `tile-clicked(index, shift, ctrl)` callback to a Rust handler that branches plain/shift/ctrl. Swatch click reuses the `PaintBrush` apply path (factored into a shared helper). Two overlay tabs toggle the rail and detail panels.

**Tech Stack:** Rust (edition 2024), Slint 1.x.

## Global Constraints

- Rust edition 2024; `cargo clippy --workspace --all-targets -- -D warnings` clean and `cargo fmt --all` clean (dispatch Rust work to the `rust-developer` agent).
- Errors use `anyhow`; logging via `tracing`.
- Cursor (`selected-index` / `selected_ref: Arc<Mutex<Option<usize>>>`) and the marked set (`selection_ref: Arc<Mutex<selection::Selection>>`) are DISTINCT; green cursor wins ties.
- Do NOT hold a Mutex guard across any Slint `set_*`/`invoke_*` call or across `std::thread::spawn` (UI-thread reentrancy deadlock). Copy values out / scope the guard, then call.
- ASCII-only UI text/glyphs (Slint default font tofus symbol glyphs; use `>` / `<`, not chevrons).
- Selection stays ephemeral + grid-only; the existing `selection_dirty` flag drives yellow-border refresh on the loader tick, the green cursor updates immediately via `set_selected_index`.
- The `brushes` Slint model is index-aligned with `imgfind::colors::BrushColor::ALL`.
- Pure logic gets unit tests; GUI wiring is verified manually.
- Consult the `slint` skill before editing `app.slint` (FocusScope/pointer-event/glyph gotchas).

**Binding names in `main.rs` (use the actual in-scope name at each site):** the cursor holder is `selected_ref` inside handlers (clone of `selected`); the multi-selection is `selection_ref` (clone of `selection`); the dirty flag clone is `selection_dirty_ref` (clone of `selection_dirty`); the search state is `state_ref`. `push_statusline(&w, &selection_ref, &state_ref)` re-pushes the statusline. `SelectionHandles { selected, selection, dirty }` (main.rs:96) bundles the three; `.clear()` clears+dirties.

---

### Task 1: `selection.rs` — `range_to` + `ctrl_toggle`

**Files:**
- Modify: `imgfind-gui/src/selection.rs`
- Test: inline `#[cfg(test)] mod tests` in `selection.rs`

**Interfaces:**
- Consumes: existing `Selection` internals (`mode`, `set`, `enter_range`, `cursor_moved`).
- Produces:
  - `pub fn range_to(&mut self, anchor: usize, clicked: usize)`
  - `pub fn ctrl_toggle(&mut self, clicked: usize)`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `selection.rs`:

```rust
#[test]
fn range_to_forward() {
    let mut s = Selection::default();
    s.range_to(2, 8);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(s.mode(), SelectionMode::Range { anchor: 2 });
}

#[test]
fn range_to_backward() {
    let mut s = Selection::default();
    s.range_to(8, 2);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(s.mode(), SelectionMode::Range { anchor: 8 });
}

#[test]
fn range_to_onto_anchor() {
    let mut s = Selection::default();
    s.range_to(5, 5);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![5]);
}

#[test]
fn range_to_replaces_prior_selection() {
    let mut s = Selection::default();
    s.enter_free();
    s.toggle(1);
    s.toggle(9);
    s.range_to(3, 5); // fresh range; old free set gone
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![3, 4, 5]);
    assert_eq!(s.mode(), SelectionMode::Range { anchor: 3 });
}

#[test]
fn ctrl_toggle_from_normal_selects_only_clicked() {
    let mut s = Selection::default();
    s.ctrl_toggle(4);
    assert_eq!(s.mode(), SelectionMode::Free);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![4]);
}

#[test]
fn ctrl_toggle_from_free_adds_then_removes() {
    let mut s = Selection::default();
    s.enter_free();
    s.toggle(2);
    s.ctrl_toggle(9);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![2, 9]);
    s.ctrl_toggle(2);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![9]);
}

#[test]
fn ctrl_toggle_from_range_converts_to_free_keeping_set() {
    let mut s = Selection::default();
    s.range_to(2, 4); // Range {2,3,4}
    s.ctrl_toggle(9);
    assert_eq!(s.mode(), SelectionMode::Free);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![2, 3, 4, 9]);
    s.ctrl_toggle(3);
    assert_eq!(s.set().iter().copied().collect::<Vec<_>>(), vec![2, 4, 9]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui selection::`
Expected: FAIL to compile — `range_to` / `ctrl_toggle` not found.

- [ ] **Step 3: Write the implementation**

Add to `impl Selection` (the `#[allow(dead_code)]`-tagged impl block):

```rust
/// Mouse shift-click: a fresh Range anchored at `anchor`, spanning to `clicked`.
pub fn range_to(&mut self, anchor: usize, clicked: usize) {
    self.enter_range(anchor);
    self.cursor_moved(clicked);
}

/// Mouse ctrl-click: toggle `clicked` in a free/discrete selection.
/// Normal -> Free {clicked}; Free -> toggle; Range -> become Free (keep set), toggle.
pub fn ctrl_toggle(&mut self, clicked: usize) {
    match self.mode {
        SelectionMode::Normal => {
            self.mode = SelectionMode::Free;
            self.set.clear();
            self.set.insert(clicked);
        }
        SelectionMode::Free | SelectionMode::Range { .. } => {
            self.mode = SelectionMode::Free; // Range conversion keeps self.set
            if !self.set.remove(&clicked) {
                self.set.insert(clicked);
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui selection::`
Expected: PASS (all selection tests, old + new).

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/src/selection.rs
git commit -m "feat(gui): selection range_to + ctrl_toggle for mouse clicks"
```

---

### Task 2: Modifier-aware tile clicks (shift/ctrl/plain)

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (root props + callback + grid tile `TouchArea`)
- Modify: `imgfind-gui/src/main.rs` (`on_tile_clicked` handler)

**Interfaces:**
- Consumes: `selection::range_to`, `selection::ctrl_toggle` (Task 1); existing `on_tile_selected` callback, `push_statusline`, `selected_ref`/`selection_ref`/`selection_dirty`/`state_ref`.
- Produces: Slint `callback tile-clicked(int, bool, bool)` + props `click-shift`/`click-ctrl`; Rust `window.on_tile_clicked(...)`.

- [ ] **Step 1: Slint — add props + callback**

Near the other selection callbacks/props in `app.slint` (around `tile-selected`/`selected-index`):

```slint
// Transient modifier state: set on left-button pointer-down, read in `clicked`.
in-out property <bool> click-shift;
in-out property <bool> click-ctrl;
// Mouse click on a tile, carrying the captured modifiers.
callback tile-clicked(int, bool, bool);   // index, shift, ctrl
```

- [ ] **Step 2: Slint — rewrite the grid tile `TouchArea`**

Replace the current handler block (app.slint:656-669) with:

```slint
TouchArea {
    width: 100%;
    height: 100%;
    pointer-event(e) => {
        if (e.kind == PointerEventKind.down && e.button == PointerEventButton.left) {
            root.click-shift = e.modifiers.shift;
            root.click-ctrl = e.modifiers.control;
        }
        if (e.button == PointerEventButton.right && e.kind == PointerEventKind.up) {
            root.tile-open-external(tile.index);
        }
    }
    clicked => {
        root.tile-clicked(tile.index, root.click-shift, root.click-ctrl);
        app-keys.focus();
    }
    double-clicked => { root.tile-activated(tile.index); }
}
```

- [ ] **Step 3: Rust — implement `on_tile_clicked`**

Register near the existing `on_tile_selected` handler. Clone the Arcs it needs into the closure (`selected`, `selection`, `selection_dirty`, `state`). Plain click clears the selection then re-enters the detail-opening path; shift/ctrl update selection + cursor only (no detail open).

```rust
window.on_tile_clicked({
    let selection = Arc::clone(&selection);
    let selected = Arc::clone(&selected);
    let selection_dirty = Arc::clone(&selection_dirty);
    let state_ref = Arc::clone(&state);
    let weak = window.as_weak();
    move |index, shift, ctrl| {
        let idx = index as usize;
        let Some(w) = weak.upgrade() else { return };
        if ctrl {
            selection.lock().unwrap().ctrl_toggle(idx);
            *selected.lock().unwrap() = Some(idx);
            selection_dirty.store(true, Ordering::Relaxed);
            w.set_selected_index(index);
            push_statusline(&w, &selection, &state_ref);
        } else if shift {
            let anchor = selected.lock().unwrap().unwrap_or(idx);
            selection.lock().unwrap().range_to(anchor, idx);
            *selected.lock().unwrap() = Some(idx);
            selection_dirty.store(true, Ordering::Relaxed);
            w.set_selected_index(index);
            push_statusline(&w, &selection, &state_ref);
        } else {
            // Plain click: collapse any selection to this tile, then open detail.
            selection.lock().unwrap().clear();
            selection_dirty.store(true, Ordering::Relaxed);
            w.invoke_tile_selected(index); // sets cursor, opens detail, loads, restatuses
        }
    }
});
```

(Use the actual in-scope Arc names. Each `selection.lock()` guard is a single
statement that drops before the next `w.*` call — no guard across a Slint call.)

- [ ] **Step 4: Build + tests**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; all tests pass.

- [ ] **Step 5: Manual smoke (if a DB is available) + clippy + commit**

Run the GUI. Verify: plain click opens detail and clears any selection; after focusing a tile, shift-click selects the contiguous run (yellow) with the green cursor on the clicked tile and does NOT open detail; ctrl-click with nothing selected selects only the clicked tile (cursor not auto-added), further ctrl-clicks add/remove, ctrl-click during a Range keeps the run and adds the tile. If no display, rely on build/tests + reading and say so.

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): shift/ctrl/plain mouse selection on grid tiles"
```

---

### Task 3: Brush swatch click → apply tags

**Files:**
- Modify: `imgfind-gui/src/main.rs` (factor `paint_brush_by_index`; `on_brush_swatch_clicked`)
- Modify: `imgfind-gui/ui/app.slint` (swatch `TouchArea` + hover; `brush-swatch-clicked` callback)

**Interfaces:**
- Consumes: existing `Action::PaintBrush` body (main.rs:1629-1639), `selected_paths`, `apply_tags_to_selection`, `apply_tags_to_focused`, `push_rail_models`.
- Produces: Slint `callback brush-swatch-clicked(int)`; Rust `fn paint_brush_by_index(...)` + `window.on_brush_swatch_clicked(...)`.

- [ ] **Step 1: Rust — factor the PaintBrush body into a shared helper**

Extract the body of `Action::PaintBrush(c)` (main.rs:1629-1639) into a free function that both the chord arm and the swatch handler call. Bundle the Arcs it needs into a small struct to stay under `too_many_arguments`:

```rust
/// Holders needed to apply a brush's tags by index (shared by the m<color>
/// chord and the rail color-swatch click).
struct PaintCtx {
    brushes: Arc<Mutex<[Vec<String>; 5]>>,
    mm: Arc<Mutex<Vec<String>>>,
    selection: Arc<Mutex<selection::Selection>>,
    state: Arc<Mutex<SearchState>>,
    backend: Backend,
    tag_ctx: TagTargetCtx,
    weak: Weak<MainWindow>,
}

/// Apply brush `idx`'s tags to the active selection (or focused tile if none),
/// update the Most Recent buffer, and re-push the rail models.
fn paint_brush_by_index(idx: usize, ctx: &PaintCtx) {
    let Some(w) = ctx.weak.upgrade() else { return };
    let tags = ctx.brushes.lock().unwrap()[idx].clone();
    let paths = selected_paths(&ctx.selection, &ctx.state);
    if paths.is_empty() {
        apply_tags_to_focused(&ctx.weak, &ctx.backend, &ctx.tag_ctx, tags.clone());
    } else {
        apply_tags_to_selection(&ctx.weak, &ctx.backend, &ctx.tag_ctx, paths, tags.clone());
    }
    *ctx.mm.lock().unwrap() = tags;
    push_rail_models(&w, &ctx.brushes.lock().unwrap(), &ctx.mm.lock().unwrap());
}
```

Then in the chord dispatch replace the `Action::PaintBrush(c)` arm body with a
call. The chord closure must build/borrow a `PaintCtx` (or construct one inline
from its already-cloned Arcs). Keep `TagTargetCtx` construction as it is and move
it into the `PaintCtx`:

```rust
chords::Action::PaintBrush(c) => {
    paint_brush_by_index(c.index(), &paint_ctx);
}
```

(Where `paint_ctx` is assembled from the Arcs the `on_key` closure already
clones — `brushes_ref`, `mm_ref`, `selection_ref`, `state_ref`, `backend_key`,
the `tag_ctx`, and `weak`. Build it once per dispatch or hold it for the
closure; whichever keeps borrows clean. `RepeatLast` keeps its own body — it
applies the mm buffer, not a brush index.)

- [ ] **Step 2: Build to confirm the refactor is behavior-preserving**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; tests pass (no behavior change yet — the chord still paints).

- [ ] **Step 3: Slint — swatch callback + TouchArea + hover**

Add the callback near the other rail callbacks:

```slint
callback brush-swatch-clicked(int);
```

Modify the brush color circle `Rectangle` (app.slint:330-344) to add hover border + a `TouchArea`:

```slint
Rectangle {
    width: 20px;
    height: 20px;
    border-radius: 10px;
    background: brush.color;
    border-width: swatch-touch.has-hover ? 2px : 0px;
    border-color: #ffffff;
    Text {
        text: brush.letter;
        color: #fff;
        font-size: 12px;
        horizontal-alignment: center;
        vertical-alignment: center;
        width: 100%;
        height: 100%;
    }
    swatch-touch := TouchArea {
        clicked => { root.brush-swatch-clicked(i); app-keys.focus(); }
    }
}
```

- [ ] **Step 4: Rust — wire `on_brush_swatch_clicked`**

```rust
window.on_brush_swatch_clicked({
    let paint_ctx = /* a PaintCtx built from cloned Arcs */;
    move |i| {
        paint_brush_by_index(i as usize, &paint_ctx);
    }
});
```

(Build a `PaintCtx` from fresh Arc clones for this closure; `Backend` and the
Arcs are all `Clone`.)

- [ ] **Step 5: Build + tests**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; tests pass.

- [ ] **Step 6: Manual smoke + clippy + commit**

Run the GUI: with several tiles selected, click a rail color swatch → all selected images get that brush's tags (verify via a tag filter / detail panel); with no selection, it tags the cursor item; the swatch shows a hover border. The `m<color>` chord still works identically (shared path).

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/src/main.rs imgfind-gui/ui/app.slint
git commit -m "feat(gui): click a brush swatch to apply its tags to the selection"
```

---

### Task 4: Peek arrows (toggle rail / detail)

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (two overlay tabs; `toggle-detail` callback)
- Modify: `imgfind-gui/src/main.rs` (`on_toggle_detail` handler)

**Interfaces:**
- Consumes: existing `toggle-rail` callback, `rail-visible`/`detail-open` props, `on_tile_selected`/`detail-close`, `selected_ref`/`state_ref`.
- Produces: Slint `callback toggle-detail()` + two overlay `TouchArea` tabs; Rust `window.on_toggle_detail(...)`.

- [ ] **Step 1: Slint — add the `toggle-detail` callback**

```slint
callback toggle-detail();
```

- [ ] **Step 2: Slint — add the two overlay tabs**

Place these as children of the `app-keys` FocusScope, AFTER the main
`HorizontalLayout`/`VerticalLayout` block and BEFORE the lightbox overlay (so
they paint above the grid but under the lightbox). Both are vertically centered,
30px tall, ASCII glyphs, hover-lightened. Bind rail width (240px) and detail
width (340px) as literals matching the panels.

```slint
// Left peek tab: toggle the brush rail.
Rectangle {
    width: 18px;
    height: 30px;
    x: root.rail-visible ? 240px : 0px;
    y: (parent.height - self.height) / 2;
    background: left-tab.has-hover ? #3a4255 : #2d3446;
    border-radius: 4px;
    Text {
        text: root.rail-visible ? "<" : ">";
        color: #e0e4ed;
        horizontal-alignment: center;
        vertical-alignment: center;
        width: 100%;
        height: 100%;
    }
    left-tab := TouchArea {
        clicked => { root.toggle-rail(); app-keys.focus(); }
    }
}

// Right peek tab: toggle the detail panel.
Rectangle {
    width: 18px;
    height: 30px;
    x: parent.width - (root.detail-open ? 340px : 0px) - self.width;
    y: (parent.height - self.height) / 2;
    background: right-tab.has-hover ? #3a4255 : #2d3446;
    border-radius: 4px;
    Text {
        text: root.detail-open ? ">" : "<";
        color: #e0e4ed;
        horizontal-alignment: center;
        vertical-alignment: center;
        width: 100%;
        height: 100%;
    }
    right-tab := TouchArea {
        clicked => { root.toggle-detail(); app-keys.focus(); }
    }
}
```

(If the tabs' absolute `x`/`y` need to resolve against the window, ensure
`parent` is the FocusScope/window-sized container; adjust the parent reference so
`parent.width`/`parent.height` are the full window — verify visually. Consult the
slint skill if z-order or positioning misbehaves.)

- [ ] **Step 3: Rust — implement `on_toggle_detail`**

```rust
window.on_toggle_detail({
    let selected = Arc::clone(&selected);
    let state_ref = Arc::clone(&state);
    let weak = window.as_weak();
    move || {
        let Some(w) = weak.upgrade() else { return };
        if w.get_detail_open() {
            w.invoke_detail_close();
        } else {
            let empty = state_ref.lock().unwrap().results.is_empty();
            if empty {
                return;
            }
            let idx = selected.lock().unwrap().unwrap_or(0);
            w.invoke_tile_selected(idx as i32); // focuses + opens + loads detail
        }
    }
});
```

(Drop the `state_ref`/`selected` guards before the `w.invoke_*` calls — the
snippet copies `empty`/`idx` out first.)

- [ ] **Step 4: Build + tests**

Run: `cargo build -p imgfind-gui && cargo test -p imgfind-gui`
Expected: compiles; tests pass.

- [ ] **Step 5: Manual smoke + clippy + commit**

Run the GUI: the left tab toggles the rail (sitting at the screen edge when
closed, at the rail's right edge when open, glyph flips `>`/`<`); the right tab
opens the detail panel for the cursor item (focusing the first tile if none) and
closes it (glyph flips `<`/`>`); both lighten on hover; no tofu; tabs don't
overlap the statusline. Backtick `` ` `` still toggles the rail.

Run: `cargo clippy -p imgfind-gui --all-targets -- -D warnings && cargo fmt -p imgfind-gui`

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): peek tabs to toggle the rail and detail panels"
```

---

### Task 5: Documentation

**Files:**
- Modify: `CLAUDE.md` (Native GUI section)

- [ ] **Step 1: Update `CLAUDE.md`**

In the Native GUI bullet, add a concise sentence after the existing keyboard-selection description:

> **Mouse selection** — shift-click selects the contiguous run between the cursor tile and the clicked tile (like `Shift+V`+navigate); ctrl-click toggles a tile in/out of a free selection (from nothing it selects only the clicked tile, not the cursor; a Range converts to free, keeping its items); plain click collapses the selection and opens the detail panel. Clicking a rail **color swatch** applies that brush's tags to the selection (or the cursor tile if none) — the mouse equivalent of `m<color>`. **Peek tabs** at the left/right window edges (vertically centered, hover-highlight) toggle the brush rail and the detail panel. See `docs/superpowers/specs/2026-06-20-gui-mouse-selection-design.md`.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document GUI mouse selection, swatch tagging, peek tabs"
```

---

## Self-Review

**Spec coverage:**
- Shift-click range (re-anchor at cursor) → Task 1 (`range_to`) + Task 2. ✓
- Ctrl-click toggle + convert range to free, no cursor auto-include → Task 1 (`ctrl_toggle`) + Task 2. ✓
- Plain click collapses selection + opens detail → Task 2. ✓
- Swatch click applies brush tags to selection/focused → Task 3. ✓
- Peek arrows toggle rail + detail, ~30px, centered, hover → Task 4. ✓
- Docs → Task 5. ✓
- Invariants (pointer-event ordering, distinct cursor/set, no lock across Slint call, index-aligned brushes, relative paths) → honored in Tasks 2-4 constraints. ✓

**Placeholder scan:** No TBD/TODO; each code step shows code; commands have expected output. The `PaintCtx`/`paint_ctx` assembly is described (build from the closure's cloned Arcs) rather than pinned to exact pre-existing variable names because those are scope-local — the implementer reads the in-scope names. ✓

**Type consistency:** `range_to(anchor, clicked)`, `ctrl_toggle(clicked)`, `tile-clicked(int,bool,bool)`, `brush-swatch-clicked(int)`, `toggle-detail()`, `paint_brush_by_index(idx, &PaintCtx)` are consistent across tasks. `invoke_tile_selected`/`invoke_detail_close`/`get_detail_open`/`set_selected_index` match existing Slint-generated method names. ✓

**Note for implementers:** exact Arc binding names (`selected` vs `selected_ref`, `state` vs `state_ref`) vary by closure scope; use the names in scope. Build a fresh `PaintCtx`/handle clones per closure.
