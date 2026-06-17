# TUI UI tests (ratatui render tests) — design

**Date:** 2026-06-16
**Branch:** `tui-ui-tests`
**Status:** Approved (brainstorming → spec)

## Summary

Add automated UI tests for the ratatui TUI by rendering the `App` widget into an
in-memory ratatui `TestBackend` and asserting the result. Two complementary
styles: **`insta` whole-frame snapshots** (a regression safety net) plus
**targeted behavioral/style assertions** on individual buffer cells. The results
grid — including the `ratatui-image` code path — is exercised deterministically
via a headless **halfblocks** protocol, so the image-rendering code touched by
the recent ratatui 0.29→0.30 / ratatui-image 8→11 upgrade is finally covered.

These are **characterization tests of the current output**: they pin today's
rendering so a future upgrade or refactor that changes the UI fails loudly.
`cargo insta review` re-baselines intentional changes.

## Motivation

The Slint migration required a major ratatui-stack upgrade (ratatui 0.29→0.30,
crossterm 0.28→0.29, ratatui-image 8→11, tui-input 0.14→0.15). The TUI has logic
tests (`app.rs`, `focus.rs`, `zoom.rs`) but **no render tests**, so the upgrade's
API migrations (`size_for(Rect)`→`size_for(Size)`, layout `.as_ref()` removal)
were verified only by "it compiles" + manual reasoning. This adds the missing
render coverage.

## Decisions (from brainstorming Q&A)

1. **Goal:** both — whole-frame snapshot safety net AND targeted behavioral
   assertions.
2. **Image path:** headless **halfblocks** protocol (`Picker::from_fontsize`),
   rendering tiny in-test images to deterministic Unicode cells, so the real
   `render_image` path (focus border, similarity-score label, grid layout) is
   covered. (Not: real terminal protocols, which need terminal support.)
3. **Tooling:** `insta` for whole-frame snapshots (per the user's standard crate
   menu) + direct ratatui `Buffer` cell access for color/style assertions
   (snapshot text does not capture color).

## Constraints discovered (drive the design)

- **`App::new(db)` calls `Picker::from_query_stdio()`** (`src/tui/app.rs:104`),
  which queries the terminal over stdio — unusable headless. Tests must build an
  `App` WITHOUT calling `App::new`.
- **`EventHandler::default()` calls `tokio::spawn`** (`src/tui/event.rs:67`) to
  run a crossterm `EventStream` reader — requires a runtime and touches stdin.
  Tests need an `EventHandler` that does not spawn.
- **All `App` fields are `pub`** (`src/tui/app.rs`), so a same-crate test can
  construct one; but the 25-field literal is brittle, so it's centralized in one
  builder.
- **Render entrypoint is `impl Widget for &mut App`** (`src/tui/ui.rs:152`) —
  public via the `Widget` trait, callable from a sibling test module.
- **`render_image` errors are logged, not panicked** (`src/tui/ui.rs:105,133`),
  so a missing protocol degrades gracefully — but with the halfblocks builder we
  supply real protocols.

## Test seams (ALL `#[cfg(test)]`-gated — zero production runtime impact)

1. **App test-builder** — e.g. `App::test(...)` or a free `fn test_app(...)` in
   the test module, gated `#[cfg(test)]`. Builds `App` with:
   - `picker: Picker::from_fontsize((W, H))` (halfblocks; deterministic; no
     terminal query),
   - `db`: a temp `Database` (the render path does not query it; a freshly
     created temp DB satisfies the field),
   - `events`: the inert `EventHandler` (seam 2),
   - all other fields defaulted, with crafted overrides per test (search_result,
     images, input, input_mode, page, focused_image_index, zoomed_image,
     show_help, is_searching).
2. **Inert `EventHandler`** — a `#[cfg(test)]` constructor (e.g.
   `EventHandler::inert()`) that builds the channel pair WITHOUT `tokio::spawn`,
   so render tests need no tokio runtime and never read stdin.
3. **`ImageEntry` builder** — a `#[cfg(test)]` helper that builds an `ImageEntry`
   from a tiny solid-color `image::DynamicImage` via
   `picker.new_resize_protocol(img)` plus a fixed `score`, to populate
   `App.images` / `App.zoomed_image` deterministically.

Rationale: these are the minimal, test-only seams that make the existing
(already `pub`-fielded) `App` renderable headless. They add no production API and
no runtime behavior. This is the "targeted testability improvement" a careful
developer makes in code they're testing.

## What gets tested

All tests render `&mut App` into a fixed **80×24** `TestBackend` (or a directly
constructed `Buffer` of that `Rect`) for determinism.

### A. Whole-frame `insta` snapshots (text via the backend's `Display`)

One snapshot per state; committed under `src/tui/snapshots/`:

- **Normal / idle** — empty input, no results.
- **Editing mode** — input contains a query string.
- **Help overlay open** (`show_help = true`).
- **Empty results** — `search_result` present with `result_count = 0`.
- **Results grid** — `search_result` with several results + `App.images`
  populated with halfblock protocols; exercises pagination text, the
  nine-block grid layout, per-cell score labels, and the focused-cell border.
- **Zoomed image** — `zoomed_image` set; exercises the zoom render + hint line.

### B. Behavioral / style assertions (direct `buf[(x, y)]`)

Snapshots are text-only and miss color; these pin the styles/specific text:

- Editing mode: the input field's border style is **yellow**
  (`InputMode::Editing`).
- Results grid: the **focused** image cell (`focused_image_index`) has the
  yellow rounded border; a non-focused cell does not.
- Pagination: the line reads `Page 1/1 (N results)` for a crafted
  `result_count` (and correct page math for a multi-page case, e.g. 20 results
  → `Page 1/3`).
- Help overlay: the rendered buffer contains a known keybinding row (e.g. the
  `e` edit-search entry from `keybindings_help()`).
- Score label: the formatted score (e.g. `0.123`) appears right-aligned on the
  cell's bottom row.

## Determinism

- Fixed `Picker::from_fontsize` value (e.g. `(8, 16)`), fixed `80×24` area, and
  fixed solid-color test images → stable halfblock cell output across runs.
- No reliance on the real terminal, env, time, or RNG.
- `insta` snapshots are reviewed/committed; `cargo insta test` /
  `cargo insta review` is the re-baseline workflow.

## Placement & tooling

- New file `src/tui/render_tests.rs`, declared in `src/tui/mod.rs` as
  `#[cfg(test)] mod render_tests;`.
- The `#[cfg(test)]` seams (`EventHandler::inert`, the `App`/`ImageEntry`
  builders) live next to their types (`event.rs`, `app.rs`) gated by
  `#[cfg(test)]`, or in the test module if they need no private access — the
  plan picks the lowest-visibility option that compiles.
- Add `insta` to `[dev-dependencies]` (a new section; the crate currently has
  none).

## Invariants this feature depends on

- **Render entrypoint stays `Widget for &mut App`** — if rendering moves to a
  different entrypoint, the test-builder call site updates with it.
- **Halfblocks is the `from_fontsize` default protocol** — the headless image
  cells are Unicode half-block glyphs. If ratatui-image changes this default,
  snapshots change (which is the intended signal), and the builder may need an
  explicit protocol-type selection.
- **`render_image` reads only `width`/`height` from `size_for`** — preserved by
  the upgrade; the snapshot of the grid pins the resulting layout.

## Out of scope

- Real terminal image protocols (sixel, kitty, iterm) — require terminal
  support; halfblocks is the deterministic headless stand-in.
- Event-loop / key-and-mouse input handling behavior — partly covered already by
  `app.rs`/`focus.rs`/`zoom.rs` logic tests; not re-done here.
- The Slint GUI (separate crate, already has its own tests).
- Performance/benchmark tests.

## Risks

- **Snapshot brittleness:** halfblock output is sensitive to ratatui-image
  internals — intentional for upgrade regressions, but expect to `cargo insta
  review` after deliberate dependency bumps. Mitigated by keeping snapshots small
  (few images, small grid) and pairing them with stable behavioral assertions.
- **`EventHandler` field coupling:** if `App` later requires a live event task at
  construction, the inert seam needs revisiting; today rendering does not touch
  `events`.
- **tokio runtime:** render tests must NOT require one; the inert `EventHandler`
  ensures this. If any test does need async, it uses `#[tokio::test]` explicitly.
