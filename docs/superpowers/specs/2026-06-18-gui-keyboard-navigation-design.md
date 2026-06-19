# GUI keyboard navigation — design

Date: 2026-06-18
Crate: `imgfind-gui` (Slint desktop GUI)
Status: approved

## Problem

The native GUI is missing the keyboard quality-of-life it should have:

1. **Grid has no keyboard navigation and no notion of a selected tile.** Selection
   today is implicit — a single click opens the detail panel, a double click
   opens the lightbox, but nothing is "highlighted" and the keyboard can't move
   between tiles.
2. **The lightbox only navigates with the arrow keys**, not the vim `h`/`l`
   keys, and when you close it you land back wherever you were with no selection
   carried over.

This spec adds vim-style + arrow-key navigation to the grid with a visible
selection, and wires the lightbox so navigating it mirrors the grid selection —
so closing the lightbox leaves you on the last-viewed image, selected.

A prior, more mature version of grid navigation exists in the sibling project
`~/src/utmost` (`crates/utmost-gui`, `gallery_move` in `view_model.rs` + the
`gallery-nav` callback in `detail.slint`). We follow its shape but with one
deliberate behavioral difference (see Invariants / wrap rules).

## Goals

- Grid: `h`/`j`/`k`/`l` **and** the Left/Down/Up/Right arrow keys move a
  visibly-highlighted selection.
- Left/right move linearly through the result list, so they cross row
  boundaries: right at the end of a row lands on the first tile of the next row,
  left at the start of a row lands on the last tile of the previous row.
- **The global beginning and end do NOT wrap.** Left on the very first tile
  stays put; right on the very last tile stays put. (This is the one place we
  diverge from utmost, which wraps first↔last.)
- Up/down move by one row and clamp at the top and bottom rows (no vertical
  wrap, no horizontal column change).
- The selected tile is always scrolled into view.
- `Enter` opens the detail panel for the selected tile. `Space` opens the
  lightbox for the selected tile. `Esc` closes the detail panel **without**
  clearing the selection.
- If the detail panel is already open, moving the selection live-updates the
  panel to the newly-selected tile. If it is closed, navigation only moves the
  highlight (panel stays closed until `Enter` or a click).
- After a search returns results, keyboard focus moves to the grid so the keys
  work immediately. Clicking the search box returns focus to it for typing.
- Lightbox: `h`/`l` step prev/next alongside the existing Left/Right arrows.
  Every lightbox step mirrors the grid `selected-index`, so `Esc` (close) drops
  the user back onto the last-viewed tile, already selected and scrolled into
  view.

## Non-goals

- No change to mouse behaviour (single-click → detail, double-click → lightbox,
  right-click → OS viewer all stay).
- No change to the lightbox zoom/pan (there is none today), filter bar, or
  search-similar flow.
- No `Tab`-based focus ring, no multi-select, no keyboard control of the filter
  bar. YAGNI.

## Architecture

The work splits cleanly into a **tested pure-Rust index function**, **Rust
state/callback wiring** (matching the existing `Arc<Mutex<…>>` pattern in
`imgfind-gui/src/main.rs`), and **declarative Slint visuals**.

### 1. Pure navigation function (`imgfind-gui/src/nav.rs`, new)

```rust
pub enum NavDir { Left, Right, Up, Down }

/// Compute the new selected index given the current selection, a direction,
/// the column count, and the total number of tiles.
///
/// - `cur == None`  -> 0 (first key selects the first tile)
/// - Left/Right     -> linear ±1, clamped to [0, len-1] (crosses rows, no global wrap)
/// - Up/Down        -> ±cols, clamped so it never leaves [0, len-1] and never
///                     moves when already in the top/bottom row for that column
/// Returns `None` when `len == 0`.
pub fn move_selection(cur: Option<usize>, dir: NavDir, cols: usize, len: usize) -> Option<usize>;
```

Semantics (mirroring utmost's `gallery_move`, except Left/Right clamp instead of
wrap):

| current | dir   | result |
|---------|-------|--------|
| `None`  | any   | `0` |
| `i`     | Left  | `i.saturating_sub(1)` |
| `i`     | Right | `min(i+1, len-1)` |
| `i`     | Up    | `if i < cols { i } else { i - cols }` |
| `i`     | Down  | `if i + cols >= len { i } else { i + cols }` |

`cols` is coerced to `>= 1` inside the function. This is the load-bearing
branchy logic and is fully unit-tested (see Testing).

### 2. Rust state & callbacks (`imgfind-gui/src/main.rs`)

- **New shared state:** `selected: Arc<Mutex<Option<usize>>>` — the index into
  the current `state.results` (the same index space `lb_index` already uses).
- **New Slint callback `grid-nav(int /*dir*/, int /*cols*/)`** (direction encoded
  as 0=Left,1=Right,2=Up,3=Down to keep the Slint signature trivial; a small
  `NavDir::from_i32` maps it). Handler:
  1. reads `state.results.len()` and current `selected`,
  2. calls `nav::move_selection`,
  3. stores the new index in `selected` and sets the Slint `selected-index`
     property,
  4. if `detail-open` is currently true, refreshes the detail panel for the new
     tile (reuse the existing `tile-selected` code path / `select(path)` +
     async thumbnail+metadata load).
- **`tile-selected` (existing, single-click):** also set `selected` +
  `selected-index` so mouse and keyboard share one selection.
- **New callback `grid-open-detail()`** (Enter): if a tile is selected, run the
  same logic as `tile-selected(selected)` to open/refresh the panel.
- **New callback `grid-open-lightbox()`** (Space): if a tile is selected, run the
  same logic as `tile-activated(selected)` to open the lightbox at that index
  (sets `lb_index = selected`).
- **Lightbox prev/next handlers (existing):** after computing the new
  `lb_index`, also write it to `selected` + the Slint `selected-index` so the
  grid mirrors the lightbox. (Esc/close already keeps `lb_index`; selection now
  reflects it.)
- **Focus to grid after search:** when results are applied after a search
  (`search`/`search-similar`/`load-more` result handlers that set `tiles`), pulse
  a `grid-focus` property so the grid FocusScope re-grabs focus. When results are
  empty, do not pulse (leave focus in the search box).
- **Reset selection** to `None` / `-1` whenever a brand-new result set replaces
  the grid (new text search). `search-similar`/`load-more` preserve the existing
  selection index if still in range, else clear it.

### 3. Slint visuals (`imgfind-gui/ui/app.slint`)

- **New `in property <int> selected-index: -1;`** and **`in property <bool>
  grid-focus-pulse;`** (toggled by Rust).
- **Highlight:** each grid tile gets `border-width: i == root.selected-index ?
  3px : 0px;` and `border-color: #5b8cff;` (a visible accent that reads against
  the `#2d3446` tile background; exact value chosen during implementation).
- **FocusScope around the grid area** handling `key-pressed`:
  - `h`/`H`/`Key.LeftArrow`  → `grid-nav(0, cols)`
  - `l`/`L`/`Key.RightArrow` → `grid-nav(1, cols)`
  - `k`/`K`/`Key.UpArrow`    → `grid-nav(2, cols)`
  - `j`/`J`/`Key.DownArrow`  → `grid-nav(3, cols)`
  - `Key.Return`             → `grid-open-detail()`
  - `" "` (space)            → `grid-open-lightbox()`
  - `Key.Escape`             → `detail-close()` (only when `detail-open`)
  - else → reject
  The FocusScope self-focuses on init and re-focuses when `grid-focus-pulse`
  changes (the `changed` pattern utmost uses for cross-scope focus).
- **Scroll-into-view:** name the `ScrollView`, and on `changed selected-index`
  compute the selected tile's `y` (`floor(idx/cols) * tile-stride`) and clamp the
  ScrollView's `viewport-y` so the tile's full height is within
  `[0, visible-height)`. Pixel math stays in Slint, where the lengths live.
- **Lightbox FocusScope (`lb-keys`):** add `event.text == "h" || "H"` →
  `lightbox-prev()` and `"l" || "L"` → `lightbox-next()` next to the existing
  arrow handlers.

## Data flow

```
search returns results ─► Rust sets `tiles`, resets `selected`=None,
                          pulses `grid-focus-pulse` ─► grid FocusScope focuses

key h/j/k/l/arrow ─► Slint grid-nav(dir,cols) ─► Rust move_selection
        ─► sets `selected` + `selected-index`
        ─► (if detail-open) refresh detail panel
        ─► Slint `changed selected-index` clamps ScrollView viewport-y

Enter  ─► grid-open-detail()  ─► open/refresh detail panel for `selected`
Space  ─► grid-open-lightbox()─► tile-activated(selected); lb_index = selected
Esc    ─► detail-close() (selection preserved)

lightbox h/l/arrows ─► lightbox-prev/next ─► Rust updates lb_index
        ─► also writes `selected` + `selected-index` (grid mirrors)
lightbox Esc ─► close; grid already shows last-viewed tile selected
```

## Invariants this feature depends on

- **`selected`, `lb_index`, and the Slint `selected-index` all index the same
  list** (`state.results`, the current grid order). Any code that mutates the
  result set must keep them consistent (reset or re-clamp together). Tested via
  the lightbox-mirrors-selection path.
- **`cols` reported by Slint to `grid-nav` matches the actual on-screen column
  count** used to lay out tiles (`max(1, floor((grid-area.width-16px)/tile-stride))`).
  Both derive from the same Slint `cols` property, so they cannot drift.
- **Left/right clamp, not wrap, at the global ends** — the explicit divergence
  from utmost. Pinned by `move_selection` tests for index 0 + Left and
  index len-1 + Right.

## Testing

Unit tests for `nav::move_selection` (pure, no Slint) — these are the
load-bearing tests:

- no selection + each direction → `Some(0)`
- left from a mid-row tile crosses to the previous row's last tile
- left from index 0 **stays at 0** (no global wrap)  ← divergence from utmost
- right from the last tile **stays at last** (no global wrap)
- right from a row-end crosses to the next row's first tile
- up from the top row stays (per-column clamp)
- down from the bottom row stays
- down from a full row into a partial bottom row clamps to a valid index
- `cols == 0` is treated as `1` (no panic / no div-by-zero)
- `len == 0` → `None`
- single-column grid (`cols == 1`): up/down behave like left/right (±1 clamped)

Slint-side behaviour (highlight, scroll-into-view, focus pulse, lightbox `h`/`l`,
selection mirroring) is verified manually by running the GUI — Slint markup has
no unit-test harness in this project. The Rust callback wiring that has logic
(detail refresh on nav, selection/lb_index mirroring) is exercised through the
pure function plus a manual run.

## Files touched

- `imgfind-gui/src/nav.rs` — **new**, `NavDir` + `move_selection` + tests.
- `imgfind-gui/src/main.rs` — new shared `selected` state; `grid-nav`,
  `grid-open-detail`, `grid-open-lightbox` handlers; selection/focus updates in
  `tile-selected`, lightbox prev/next, and the search/similar/load-more result
  appliers; declare `mod nav;`.
- `imgfind-gui/ui/app.slint` — `selected-index` + `grid-focus-pulse` properties,
  tile highlight border, grid FocusScope, ScrollView scroll-into-view, lightbox
  `h`/`l` keys, the three new callbacks.
- `CLAUDE.md` — one line under the Native GUI section noting grid/lightbox
  keyboard navigation + this spec link.
```
