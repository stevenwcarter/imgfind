# GUI Mouse Selection + Peek Arrows — Design

Date: 2026-06-20
Status: Approved (brainstormed via `/ship-it --ask`)

## Summary

Add mouse affordances to the imgfind-gui grid that complement the existing
keyboard selection modes (range `Shift+V` / free `v`):

1. **Shift-click** a tile selects the linear contiguous run between the current
   cursor tile and the clicked tile — identical to `Shift+V` then navigating to
   that tile (a fresh Range anchored at the current cursor).
2. **Ctrl-click** a tile toggles it in/out of the selection. From a non-selected
   state it starts a free/discrete selection containing **only** the clicked
   tile (the cursor tile is *not* auto-included). If a Range selection is active,
   ctrl-click converts it to a free selection (keeping the already-selected
   items) and toggles the clicked tile.
3. **Plain click** collapses any active selection to the single clicked tile and
   opens the detail panel (unchanged detail behavior).
4. **Clicking a brush color swatch** in the left rail applies that brush's tags
   to the current selection (or, if nothing is selected, the focused/cursor
   tile) — the mouse equivalent of the `m<color>` chord.
5. **Peek arrows** — two small clickable tabs at the left and right window edges
   that open/close the brush rail (left) and the detail panel (right). ~30px
   tall, vertically centered, with a hover effect.

This builds on `imgfind-gui/src/selection.rs`, the grid tile `TouchArea` in
`app.slint`, the rail brush swatches, and the existing tag-apply path
(`selected_paths` + `apply_tags_to_selection`/`apply_tags_to_focused`).

## Goals

- Shift-click range select and ctrl-click toggle, matching the keyboard model.
- Swatch click applies a brush's tags to the selection or focused tile.
- Peek arrows to toggle the rail and detail panels by mouse, with hover feedback.
- Keep selection math pure and unit-tested (extend `selection.rs`).

## Non-goals

- Click-drag rubber-band selection (keyboard + click gestures only).
- A mouse affordance for the "Most Recent" (`mm`) buffer — only the 5 color
  swatches are clickable (YAGNI).
- Persisting selection or panel-open state beyond what already persists
  (`rail_visible` already persists; detail-open does not, and stays that way).
- Changing the keyboard behavior of `` ` `` (still toggles the rail) — the left
  peek arrow is an additional affordance, not a replacement.

## Architecture

### 1. Pure selection model — `imgfind-gui/src/selection.rs` (two new methods)

Both pure, unit-tested, reusing the existing internals.

```rust
/// Mouse shift-click: a fresh Range anchored at `anchor`, spanning to `clicked`.
/// Equivalent to `enter_range(anchor); cursor_moved(clicked)`.
pub fn range_to(&mut self, anchor: usize, clicked: usize) {
    self.enter_range(anchor);
    self.cursor_moved(clicked);
}

/// Mouse ctrl-click: toggle `clicked` in a free/discrete selection.
/// - Normal  -> mode = Free, set = {clicked}
/// - Free    -> toggle `clicked`
/// - Range   -> switch to Free KEEPING the current set, then toggle `clicked`
pub fn ctrl_toggle(&mut self, clicked: usize) {
    match self.mode {
        SelectionMode::Normal => {
            self.mode = SelectionMode::Free;
            self.set.clear();
            self.set.insert(clicked);
        }
        SelectionMode::Free => {
            // reuse existing toggle semantics
            if !self.set.remove(&clicked) {
                self.set.insert(clicked);
            }
        }
        SelectionMode::Range { .. } => {
            self.mode = SelectionMode::Free; // keep self.set as-is
            if !self.set.remove(&clicked) {
                self.set.insert(clicked);
            }
        }
    }
}
```

Note: `toggle()` already early-returns unless `mode == Free`, so `ctrl_toggle`
implements the insert/remove inline (it must run for the just-switched Free
mode and for the Range-conversion case). Alternatively `ctrl_toggle` may set
`mode = Free` first and then call `self.toggle(clicked)` — pick whichever keeps
the duplication out; the test cases below pin the behavior either way.

### 2. Modifier capture in Slint — `app.slint`

Slint's `TouchArea.clicked` carries no modifier info, but `pointer-event(e)`
exposes `e.modifiers`. Capture the modifier state on pointer **down** into two
root bool properties, then read them in `clicked`:

```slint
// New root properties (transient; set on pointer-down, read on clicked):
in-out property <bool> click-shift;
in-out property <bool> click-ctrl;

// New callback carrying the resolved modifiers:
callback tile-clicked(int, bool, bool);   // index, shift, ctrl
```

Grid tile `TouchArea` (currently `clicked => { root.tile-selected(tile.index); … }`):

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

`double-clicked` (lightbox activation) is unchanged. (Implementation note:
verify against the Slint skill that `pointer-event` `down` fires before
`clicked` so the captured modifier state is current — this is the documented
ordering; if a platform delivers them out of order, fall back to handling the
modified click directly in `pointer-event` left-`up`.)

### 3. Click handler — `main.rs` `on_tile_clicked`

A single handler branches on the modifiers. It owns clones of `selection`,
`selected` (cursor), `state`, `selection_dirty`, and a `Weak<MainWindow>`.

- **plain** (`!shift && !ctrl`): `selection.clear(); selection_dirty = true;` then
  re-enter the existing detail-opening path via `w.invoke_tile_selected(index)`
  (sets cursor, opens detail, loads image/meta/tags, refreshes statusline). This
  preserves today's single-click-opens-detail behavior while collapsing the
  selection.
- **shift**: read the current cursor (`selected` holder); `anchor = cursor.unwrap_or(index)`;
  `selection.range_to(anchor, index)`; set cursor to `index` (update `selected`
  holder + `set_selected_index(index)`); `selection_dirty = true`; `push_statusline`.
  Does **not** open the detail panel.
- **ctrl**: `selection.ctrl_toggle(index)`; set cursor to `index`;
  `selection_dirty = true`; `push_statusline`. Does **not** open the detail panel.

Lock discipline: copy the cursor value out, drop the guard, then mutate
`selection`; never hold a guard across `invoke_*`/`set_*` (same rule the keyboard
handlers follow). The yellow selection borders refresh on the next loader tick
via `selection_dirty` (the existing mechanism); the green cursor updates
immediately via `set_selected_index`.

### 4. Swatch click — `app.slint` + `main.rs`

**Markup:** add a `TouchArea` (with `has-hover`) over each brush color circle in
the rail (`for brush[i] in root.brushes`, the 20×20 `Rectangle` at
app.slint:330). Hover feedback: a light border when `swatch-touch.has-hover`.

```slint
Rectangle {
    width: 20px; height: 20px; border-radius: 10px;
    background: brush.color;
    border-width: swatch-touch.has-hover ? 2px : 0px;
    border-color: #ffffff;
    Text { text: brush.letter; /* unchanged */ }
    swatch-touch := TouchArea {
        clicked => { root.brush-swatch-clicked(i); app-keys.focus(); }
    }
}
```

New callback: `callback brush-swatch-clicked(int);` (the brush/rail index `i`,
index-aligned with `BrushColor::ALL`).

**Rust:** factor the existing `Action::PaintBrush` body (main.rs:1629-1639) into a
shared helper so the chord and the swatch use one code path (avoids duplicated
logic a reviewer would flag):

```rust
// Apply brush `idx`'s tags to the active selection, or the focused tile if none.
// Updates the Most Recent (mm) buffer and re-pushes the rail models, exactly as
// the m<color> chord does.
fn paint_brush_by_index(idx: usize, ctx: &PaintCtx) { /* moved-from PaintBrush body */ }
```

`Action::PaintBrush(c)` becomes `paint_brush_by_index(c.index(), &ctx)`, and the
`on_brush_swatch_clicked(i)` handler calls `paint_brush_by_index(i as usize, &ctx)`.
(`PaintCtx` bundles the Arcs the body needs — `brushes`, `selection`, `state`,
`mm`, the backend, `TagTargetCtx`, weak window — to stay under clippy's
`too_many_arguments`; reuse/extend the existing bundles where possible.)

### 5. Peek arrows — `app.slint` + `main.rs`

Two overlay tabs drawn after the main `HorizontalLayout` (so they paint above the
grid) and before the lightbox overlay (so the lightbox still covers them).

Geometry: width ~18px, height 30px, `y: (parent.height - 30px) / 2` (vertically
centered). ASCII glyphs only (`>` / `<` — chevron symbols tofu in Slint's default
font). Hover: background lightens via the tab's `has-hover`.

**Left tab (rail):**
- `x: root.rail-visible ? rail-width : 0px` (rail-width = the rail Rectangle's
  240px). When rail closed it sits at the screen's left edge showing `>`; when
  open it sits at the rail's right edge showing `<`.
- `clicked => { root.toggle-rail(); }` (reuses the existing `toggle-rail`
  callback used by `` ` `` and the chord `Action::ToggleRail`).

**Right tab (detail):**
- Pinned to the right edge: `x: parent.width - (root.detail-open ? detail-width : 0px) - self.width`
  (detail-width = the detail panel's 340px). When detail closed it sits at the
  screen's right edge showing `<`; when open it sits at the panel's left edge
  showing `>`.
- `clicked => { root.toggle-detail(); }` — new callback.

New callback + Rust handler `on_toggle_detail`:

```rust
window.on_toggle_detail(move || {
    let Some(w) = weak.upgrade() else { return };
    if w.get_detail_open() {
        w.invoke_detail_close();          // existing close path
    } else {
        // Focus the first tile if nothing is focused, then open detail for the cursor.
        let idx = selected.lock().unwrap().unwrap_or(0);
        // (guard dropped before invoke)
        if /* results non-empty */ { w.invoke_tile_selected(idx as i32); }
    }
});
```

`invoke_tile_selected` already sets the cursor, opens the detail panel, and loads
the image/metadata/tags, so the right tab "opens detail for the cursor item
(focusing the first tile if none)" with no new detail logic. If the result set is
empty, opening is a no-op.

## Data flow

```
left-button down on tile  -> pointer-event: capture click-shift / click-ctrl
left-button click on tile -> tile-clicked(index, shift, ctrl)
    plain -> selection.clear(); dirty; invoke_tile_selected(index)  (opens detail)
    shift -> selection.range_to(cursor, index); cursor=index; dirty; push_statusline
    ctrl  -> selection.ctrl_toggle(index);      cursor=index; dirty; push_statusline

swatch click -> brush-swatch-clicked(i) -> paint_brush_by_index(i)
    -> selected_paths(): non-empty -> apply_tags_to_selection
                          empty     -> apply_tags_to_focused
    -> update mm buffer + push_rail_models

left tab  click -> toggle-rail()   (existing)
right tab click -> toggle-detail()  -> open(cursor)/close detail

selection change -> selection_dirty -> loader tick rebuilds tiles (yellow borders)
                  + push_statusline (counts)
cursor change    -> set_selected_index (green border, immediate)
```

## Testing

**Pure unit tests (`selection.rs`):**
- `range_to` forward: `range_to(2, 8)` → set {2..=8}, mode Range{anchor:2}.
- `range_to` backward: `range_to(8, 2)` → set {2..=8}, mode Range{anchor:8}.
- `range_to` onto anchor: `range_to(5, 5)` → set {5}.
- `range_to` replaces a prior selection (call after a free selection → only the
  new run remains).
- `ctrl_toggle` from Normal: → mode Free, set {clicked} (cursor NOT included —
  the model has no cursor, so this is inherently satisfied; the handler test
  below covers "cursor not auto-included").
- `ctrl_toggle` from Free: adds a new index; a second `ctrl_toggle` of the same
  index removes it.
- `ctrl_toggle` from Range: mode becomes Free, the prior contiguous set is kept,
  and the clicked index is toggled into/out of it (e.g. Range {2,3,4} +
  ctrl_toggle(9) → Free {2,3,4,9}; ctrl_toggle(3) → Free {2,4,9}).

**Manual / integration (documented in the plan):**
- Plain click collapses a multi-selection to one tile and opens detail.
- Shift-click after focusing a tile selects the contiguous run (yellow) with a
  green cursor on the clicked tile; matches `Shift+V`+navigate.
- Ctrl-click with nothing selected selects only the clicked tile (cursor tile not
  added); further ctrl-clicks add/remove; ctrl-click during a Range keeps the
  run and adds the clicked tile.
- Swatch click tags all selected (or the focused tile if none); mm buffer + rail
  update; with no selection it tags the cursor item.
- Left tab toggles the rail (open/close), tracking the rail edge; right tab opens
  detail for the cursor item (focuses first tile if none) and closes it; both
  show a hover effect; glyphs render (no tofu).

## Invariants this feature depends on

- **`pointer-event` down precedes `clicked`** so the captured modifier state is
  current when `clicked` reads it. (Pin: shift-click and ctrl-click manual checks
  — if they behave like plain clicks, the ordering assumption broke; fall back to
  dispatching the modified action from `pointer-event` left-`up`.)
- **`on_tile_selected` opens the detail panel and is safe to invoke for the plain
  click and the right-tab open.** Shift/ctrl click must NOT route through it (they
  would wrongly open detail). (Pin: shift/ctrl click leave the detail panel
  state unchanged.)
- **Cursor vs marked-set stay distinct** (cursor = green via `selected-index`;
  set = yellow via `Tile.selected`). Mouse handlers update both correctly; green
  wins ties (existing border expression).
- **Results indices stable for the selection's lifetime** — already enforced:
  every result-replacement site clears the selection (from the prior feature).
  Mouse selection introduces no new result-mutating path, so the invariant holds.
- **`brushes` model is index-aligned with `BrushColor::ALL`** so a swatch index
  `i` maps to the correct brush tags (the rail already relies on this via
  `push_rail_models`).
- **Relative-path invariant** — swatch tagging reuses `selected_paths` /
  `apply_tags_to_*`, which already operate on `RowMeta.path` (relative); no new
  path conversion.
