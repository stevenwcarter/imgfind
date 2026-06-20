# GUI "Clear text search" button — design

## Problem

In the native GUI, a semantic text search narrows the grid. There is currently
no explicit way to drop *just* the text query and fall back to the broader set
defined by the remaining (size / type / GPS / tag) filters. The search field's
built-in inner **X** only clears the field's text so the user can retype — it
does not re-run anything, by design.

Worse, the only path that re-runs on an empty query is `on_search`, which fires
**only on Enter** (`LineEdit.accepted`). And when the query is empty *and* no
other filters are active, `on_search` shows an idle "Enter a search query"
prompt instead of the full library.

## Goal

Add a dedicated **"Clear text search"** button to the **left** of the search
input. Clicking it drops the text query and shows the full set of images that
match the other filters — and with no other filters active, the entire library
(same as a fresh launch). The inner X is unchanged (text-clear for retyping
only).

## Decisions (from brainstorming)

- **New button, left of the input.** The inner native X stays a pure text-clear;
  the new button is the explicit "drop the query, go back to browsing" action.
- **Clearing always browses.** No "idle prompt" special-case. With no other
  filters, `Filters::default()` → `browse_all` → all images.
- **Approach A** (dedicated `clear-search` callback + a shared Rust helper),
  chosen over reusing `search("")` (couples to the typing entrypoint) and over
  auto-refreshing on `edited` (would make the inner X jump back to browse,
  against intent).

## Design

### Slint (`imgfind-gui/ui/app.slint`)

- Declare a `callback clear-search();`.
- In the search `HorizontalLayout` (currently just `search-input`), add a
  `Button` **before** `search-input`:
  - `text: "Clear text search";` (ASCII/Latin-1 only — Slint default-font glyph
    safety, per project memory).
  - `enabled: root.query-text != "";` — disabled when there is nothing to clear
    (i.e. while already browsing).
  - `clicked => { root.clear-search(); app-keys.focus(); }` — return focus to the
    grid keys after, matching the existing `accepted` handler.
- The `search-input` LineEdit is otherwise untouched (still `text <=>
  root.query-text`, `accepted(text) => root.search(text)`). The inner X clears
  `query-text` via the two-way binding and fires nothing — grid stays put.

### Rust (`imgfind-gui/src/main.rs`)

**New shared helper** `clear_to_browse(...)` containing the empty-query browse
logic (extracted from `on_search`'s current filters-active branch, generalized):

- `*lb = None; *detail = None;` and `*mode = SearchMode::Text(String::new())`.
- Reset sort for browse: `state.sort = Sort::default()` then
  `state.start_search(String::new())`. Resetting sort is **load-bearing** — a
  prior search may have left sort = Relevance, which is invalid for browse; the
  old filters-active clear branch failed to reset it (latent bug, fixed here).
- On the UI thread: `set_lightbox_open(false)`, `set_detail_open(false)`,
  `set_status("Searching…")`, `set_can_search(false)`, and reset the sort
  selector to browse mode: `set_sort_options(make_sort_options_model(false))`,
  `set_sort_index(0)`, `set_sort_desc(false)`.
- `spawn_browse(weak, state, grid_gen, backend, filters, sel)` with
  `SelectionPolicy { after: SelectAfter::Clear, .. }` (drops any multi-select,
  since the result list is replaced).
- Takes `filters: Filters` (the caller passes the current filter state, so
  default filters → all images and active filters → the filtered set).

**`on_search` empty-query branch** collapses to a single call: when `query`
is empty, call `clear_to_browse(.., current_filters, ..)` and return. The
`current_filters == Filters::default()` idle special-case is **removed** — the
"Enter a search query" idle state no longer appears on clear; an empty library
simply browses to an empty grid (whatever status `apply_fetch_result` sets).
The `restoring` guard at the top of `on_search` is unchanged.

**New `on_clear_search` handler:** set `query-text` to empty
(`w.set_query_text("".into())`) then call the same `clear_to_browse(..)` with
the current filters. Mirrors the on_search empty path; the only extra step is
emptying the field (the button doesn't go through the LineEdit).

## Invariants this feature depends on

- **`browse_all(&Filters::default(), ..)` returns every image** (the "show all
  on no-filter clear" behavior). Already characterized by the existing
  `browse_all_sorts_by_*` tests (all use `Filters::default()` and assert the
  full row set). A focused test `browse_all_default_filters_returns_all` is
  added to make the dependency explicit and greppable.

## Testing

- **DB (unit):** add `browse_all_default_filters_returns_all` in
  `src/database.rs` tests — insert N images, assert `browse_all(&Filters::
  default(), &Sort::default())` returns all N.
- **GUI wiring (manual-smoke):** the button → `clear-search` → `clear_to_browse`
  path and the "inner X clears text only" behavior are interactive and verified
  manually, consistent with the other GUI interaction features (selection,
  mouse) which carry manual-smoke residuals. Smoke checklist:
  1. Run a text search → grid narrows; "Clear text search" becomes enabled.
  2. Click it → grid returns to browse (all images, or the filtered set if
     size/type/GPS/tag filters are set); Relevance disappears from the sort
     selector; button greys out.
  3. With filters set, repeat → returns to the filtered (not full) set.
  4. Inner X with text present → field empties, grid unchanged, no re-query.

## Out of scope

- Changing the inner X behavior.
- Live-refresh-on-edit.
- Persisting any new state (the button derives enabled-ness from `query-text`).
