# GUI Keyboard Selection Modes — Design

Date: 2026-06-20
Status: Approved (brainstormed via `/ship-it --ask`)

## Summary

Add vim-style multi-image selection to the `imgfind-gui` grid so a user can mark
a set of images by keyboard and apply tagging actions to the whole set at once.
Two modes:

- **Range mode** (`Shift+V`) — anchors at the current cursor tile, then as the
  cursor moves the selection becomes the **linear contiguous run** of grid
  indices between anchor and cursor inclusive (crosses row boundaries; it is a
  1-D index range, *not* a 2-D rectangle).
- **Free mode** (`v`) — enters with nothing selected; `Space` toggles the cursor
  tile in/out of the selection so the user can hand-pick scattered images.

While a selection is active, the tag chords that act on images (`mm`, `m<color>`,
and the `t` add-tags modal) apply to **every** selected image. `Esc` clears the
selection and returns to normal mode. A new always-visible **statusline** at the
bottom of the window shows the mode, result-set stats, and (when selecting)
selection stats.

This builds directly on the existing keyboard infrastructure: the single
`app-keys` `capture-key-pressed` FocusScope in `app.slint`, the pure
`nav::move_selection` cursor math, the `chords::resolve` state machine, and the
`apply_tags_to_focused` / `focused_path` tag-write path.

## Goals

- Range and free keyboard selection in the grid, with live visual feedback.
- Apply `mm` / `m<color>` / `t`-modal tagging to a whole selection in one action.
- Always-on statusline with result-set and selection stats.
- Keep selection math pure and unit-tested (sibling to `nav.rs`, `chords.rs`).

## Non-goals

- Mouse-driven multi-select (click-drag, ctrl/shift-click). Keyboard only.
- Persisting a selection across sessions (selection is ephemeral).
- Selection in the lightbox or detail panel (selection is grid-scoped). An open
  lightbox/detail does not clear an existing selection, but `v`/`Shift+V` and the
  range cursor-follow are grid behaviors.
- New tag-write semantics beyond fan-out (still `Backend::add_tag` per path).
- Bulk *untagging* or other batch operations — only tag application.

## Architecture

### 1. Pure selection model — `imgfind-gui/src/selection.rs` (new)

A pure, fully unit-tested module holding selection state and the index math. No
I/O, no Slint, no locks (the caller wraps it in the existing `Arc<Mutex<…>>`
pattern).

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionMode {
    Normal,
    Range { anchor: usize },
    Free,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Selection {
    mode: SelectionMode,      // Default = Normal
    set: BTreeSet<usize>,     // selected indices into the result set
}
```

Methods (all pure):

- `enter_range(&mut self, cursor: usize)` — `mode = Range { anchor: cursor }`,
  `set = {cursor}`.
- `enter_free(&mut self)` — `mode = Free`, `set` cleared (nothing selected yet).
- `cursor_moved(&mut self, cursor: usize)` —
  - `Range { anchor }`: `set = (min(anchor,cursor)..=max(anchor,cursor))`.
  - `Free` / `Normal`: no-op.
- `toggle(&mut self, cursor: usize)` — `Free` only: insert/remove `cursor`.
  No-op in `Range`/`Normal`.
- `clear(&mut self)` — `mode = Normal`, `set` emptied.
- `is_active(&self) -> bool` — `mode != Normal`.
- `contains(&self, i: usize) -> bool` — membership for border rendering.
- `selected_indices(&self) -> impl Iterator<Item = usize>` / `len()` — for the
  statusline and tag fan-out.
- `mode(&self) -> SelectionMode`.

`SelectionMode` derives `Default` as `Normal`.

Note: `cursor_moved` materializes the full range into `set` on every move. The
window of rendered tiles is bounded (a few hundred at most), and the full result
set is already held in memory, so a `BTreeSet` spanning the whole result set is
cheap and keeps rendering a simple membership test.

### 2. State holders in `main.rs`

Add one holder alongside the existing `selected: Arc<Mutex<Option<usize>>>`
(the cursor) and `pending_chord`:

```rust
let selection: Arc<Mutex<selection::Selection>> = Arc::new(Mutex::new(Selection::default()));
```

The cursor (`selected: Arc<Mutex<Option<usize>>>` and the Slint `selected-index`
property) is unchanged — it remains the single highlighted tile. `Selection` is
the *marked set*, a separate concept.

### 3. Slint surface — `app.slint`

**`Tile` struct** gains a field:

```
export struct Tile {
    path: string,
    image: image,
    size-kb: int,
    index: int,
    selected: bool,   // NEW — true when this tile is in the marked set
}
```

**New callbacks / properties on `MainWindow`:**

- `callback selection-enter-range();`
- `callback selection-enter-free();`
- `callback selection-toggle();`  // Free-mode Space
- `in property <string> statusline;` // always-visible bottom bar text

**Border rendering** (the grid tile `Rectangle`) changes from the current
blue-cursor-only logic to:

```
border-width: (tile.index == root.selected-index || tile.selected) ? 3px : 0px;
border-color: tile.index == root.selected-index
    ? #3ad17a       // cursor: green (replaces old #5b8cff blue), wins ties
    : #f2c94c;      // selected-set member: yellow
```

**Key handling** in the `app-keys` `capture-key-pressed`, grid branch (after the
search/modal/suppressed/lightbox guards, integrated with the existing chord and
nav branches):

- `v` with **no** shift → `root.selection-enter-free(); accept`.
- `v`/`V` with shift (`event.modifiers.shift`) → `root.selection-enter-range(); accept`.
- `Space` (`" "`): if a selection is active in `Free` mode → `selection-toggle()`;
  else (Normal) → existing `grid-open-lightbox()`. (Rust decides which by reading
  the selection mode; Slint forwards a single `selection-toggle()` when active —
  see wiring below.)
- `Esc`: if selection active → clear selection (handled before `detail-close`).
- `h/j/k/l`/arrows: unchanged call to `grid-nav`; Rust's `grid-nav` handler also
  advances `Selection::cursor_moved` so a Range re-materializes live.

Because Slint cannot read the Rust-side selection mode synchronously, the
**Space** and **Esc** disambiguation is done in Rust:

- Add `callback grid-space();` and route `Space` (in the non-lightbox grid
  branch) to it. The Rust `on_grid_space` handler checks the selection: if
  `Free`, toggle; otherwise open the lightbox.
- `Esc` in the grid branch: add the selection-clear check to the existing
  Rust-driven close logic. Introduce `callback grid-escape();` (or extend an
  existing one) whose handler clears the selection when active, else falls back
  to `detail-close`. The markup keeps its existing `lightbox`/`search`/`modal`
  Esc guards; only the final grid-branch `Esc` is redirected to `grid-escape()`.

This keeps every mode decision in testable Rust and leaves `.slint` declarative.

### 4. Tag application to a selection — `main.rs`

Add `apply_tags_to_selection(weak, backend, ctx, sel_paths, tags)` mirroring
`apply_tags_to_focused`:

- Resolve the selected indices → relative paths from `SearchState.results`.
- Spawn one background thread; for each path, for each tag, `Backend::add_tag`.
- If the detail panel is showing one of the affected paths, re-fetch and push its
  tags on the UI thread (same as the single-image path).

Chord dispatch (`Action::PaintBrush`, `Action::RepeatLast`) and the
`tag-modal-commit` handler branch:

```
if selection.is_active() && !selection.is_empty() {
    apply_tags_to_selection(...)   // fan out to the whole set
} else {
    apply_tags_to_focused(...)     // unchanged single-image behavior
}
```

Per the approved decision, **selection and mode persist after apply** — only
`Esc` clears them — so a user can stack `mr` then `mg` then `t` on the same set.

The `f`-prefixed filter chords (`LoadBrushIntoFilter`, `ToggleTagFilter`) are
unaffected: they manipulate the tag filter, not images.

### 5. Statusline — `main.rs` + `app.slint`

A new `statusline` string property, rendered in an always-visible bar at the very
bottom of the window (below the main `VerticalLayout`/grid row, inside `app-keys`
so it spans the rail+main columns). Distinct from the existing transient `status`
text above the grid (which stays for "Loading model…" messages).

A pure helper `format_statusline(...)` in `main.rs` (unit-tested) builds the
string from:

- total result count (`results.len()`),
- total disk size (`Σ results[i].size`, via the existing `format_bytes`),
- selection mode label,
- when active: selected count and selected disk size.

Format:

- Normal: `NORMAL · {n} images · {total_size}`
- Free:   `VISUAL (FREE) · {n} images · {total_size} ┃ selected {k} · {sel_size}`
- Range:  `VISUAL (RANGE) · {n} images · {total_size} ┃ selected {k} · {sel_size}`

(`·` U+00B7 and `┃` U+2503 — verify glyph coverage in Slint's default font during
implementation; per the project's known tofu issue, fall back to ASCII `-` and
`|` if either renders as tofu. See memory: Slint default-font glyph coverage.)

The statusline is recomputed and pushed whenever the result set changes, the
selection changes, or the mode changes — i.e. from the same code paths that
already refresh tiles and `selected-index`. Sizes are summed from `RowMeta.size`
(`Option<i64>`; treat `None` as 0).

## Data flow

```
key press (app-keys capture-key-pressed)
  ├─ Shift+V → selection-enter-range  → Rust: Selection::enter_range(cursor)
  ├─ v       → selection-enter-free   → Rust: Selection::enter_free()
  ├─ Space   → grid-space             → Rust: if Free toggle(cursor) else open lightbox
  ├─ h/j/k/l → grid-nav               → Rust: move cursor; Selection::cursor_moved(cursor)
  ├─ Esc     → grid-escape            → Rust: if active Selection::clear() else detail-close
  ├─ m<c>/mm → key → chords::resolve  → Rust: if active apply_to_selection else apply_to_focused
  └─ t       → OpenTagModal; commit   → Rust: same active/focused branch
                                          │
  every state change ─────────────────────┘
       └─ rebuild Tile model (set `selected` per tile) + set selected-index
          + set statusline   (all on the UI thread)
```

## Testing

Pure unit tests (no GUI harness needed), following the `nav.rs`/`chords.rs`
pattern:

**`selection.rs`:**
- `enter_range` seeds anchor and selects only the anchor.
- `cursor_moved` forward: anchor=2, cursor=8 → set = {2,3,4,5,6,7,8} (contiguous,
  crosses rows).
- `cursor_moved` backward: anchor=8, cursor=2 → same set {2..=8} (min/max).
- `cursor_moved` back onto anchor → set = {anchor}.
- `enter_free` selects nothing; `toggle` adds then removes the same index;
  `toggle` of several distinct indices accumulates a sparse set.
- `toggle` is a no-op in `Range`/`Normal`; `cursor_moved` is a no-op in
  `Free`/`Normal`.
- `clear` returns to `Normal` with empty set; `is_active` reflects mode.
- Re-entering a mode resets the set (enter_range after a free selection, etc.).

**`format_statusline`:**
- Normal with N images and known total size → `NORMAL · N images · …`.
- Free/Range with a selection → includes `selected k · …` and the right label.
- `None` sizes contribute 0 to both totals.
- Zero results → sensible `NORMAL · 0 images · 0 B`.

**Manual / integration (documented in the plan, run via the slint skill's app
launch):**
- Shift+V then move highlights a live-growing yellow run with a green cursor.
- v then Space hand-picks scattered tiles.
- `mr`/`mm`/`t` on a selection tags every selected image (verify via the detail
  panel / `status` and a follow-up tag-filter).
- Esc clears selection and restores `NORMAL`; Space in normal mode still opens
  the lightbox.

## Invariants this feature depends on

- **Cursor vs selection are distinct.** `selected-index` (Slint) / `selected`
  (Rust `Option<usize>`) is the single cursor; `Selection.set` is the marked set.
  Border rendering and tag fan-out must read the correct one. (Test: border logic
  renders green for cursor even when the cursor tile is also in the set.)
- **Tile model is rebuilt on every cursor move and selection change**, so the
  per-tile `selected` flag and `statusline` stay in sync with `Selection`. If a
  future change stops rebuilding tiles on cursor move, the live range highlight
  breaks — pin with the manual range-grow check.
- **`results` indices are stable for the lifetime of a selection.** A new
  search/browse/sort/similar that replaces `results` must `clear()` the selection
  (indices would otherwise dangle). Enumerate the result-replacing call sites
  (search, browse, sort change, search-similar, filter change) and clear the
  selection in each.
- **Relative-path invariant** (`abs_to_relative_path` / DB stores relative
  paths): `apply_tags_to_selection` resolves indices to the same relative
  `RowMeta.path` strings that `apply_tags_to_focused` already uses — no new
  path conversion.
