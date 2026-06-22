# GUI Lightbox Zoom & Pan — Design

**Date:** 2026-06-21
**Status:** Approved
**Crate:** `imgfind-gui` (Slint desktop GUI) + `imgfind` core (thumbnail/units seam)

## Summary

Add scroll-wheel **zoom** and click-drag **pan** to the GUI lightbox, plus the
chrome to drive it (Fit button, zoom slider, live percentage, a top bar with
filename / counter / close). The behavior is ported to be *essentially
identical* to the lightbox in `~/src/utmost`
(`docs/superpowers/specs/2026-05-18-gui-preview-lightbox-design.md` and
`2026-05-18-gui-lightbox-pan-design.md` there).

The lightbox currently shows a single 2048px cached thumbnail (`image-fit:
contain`) with prev/next navigation only. Zooming a 2048px image past ~100% on a
large display looks soft. To keep zoom **instant** yet eventually **crisp**, the
lightbox uses **progressive resolution**: it zooms the already-loaded 2048px
thumbnail immediately, decodes the **full native-resolution** rendition in the
background (cache-checked first, then persisted), and hot-swaps the sharper
pixels in **without disturbing the user's current zoom/pan window**.

As part of this work, the stringly-/sentinel-prone "thumbnail size" parameter is
promoted from a bare pixel newtype to an enum so the full-resolution rendition is
a first-class, type-safe case (no magic `0` leaking into logic).

## Goals

- Scroll-wheel / trackpad zoom over the lightbox image (10%–800%).
- Click-drag to pan when zoomed beyond fit; image edges clamp to the viewport.
- Keyboard: `+`/`=` zoom in, `-` zoom out, `0` fit. (Existing Esc / arrows / h /
  l unchanged.)
- Chrome: top bar (filename · "n of N" · close `×`), bottom bar (Fit button ·
  zoom slider 10–800% · live percentage).
- Progressive resolution: instant zoom on the 2048px thumb, background full-res
  decode, cache-first, persisted, hot-swap preserving zoom/pan.
- Type-safe rendition selector (`ThumbnailSpec` enum) replacing the bare size.

## Non-goals

- No wrap-around in lightbox navigation. imgfind clamps at the ends (consistent
  with the grid cursor); utmost wraps, but we keep imgfind's clamp.
- No zoom/pan in the **detail panel** (lightbox only).
- No change to the grid, detail-panel, or CLI thumbnail *sizes* (still
  300/512/2048); only the *type* threading them changes.
- No new schema migration (the `thumbnails (image_hash, size)` table is reused;
  `size = 0` is the on-disk encoding of `FullSize`).

## Type model: `ThumbnailSpec`

Today `src/units.rs` has a pixel newtype:

```rust
pub struct ThumbnailSize(pub u32);   // a fixed long-edge in pixels
```

This stays. We add an enum that becomes the "which rendition" parameter wherever
thumbnails are requested or stored:

```rust
/// Which rendition of an image to fetch/generate/store. The DB `thumbnails.size`
/// column encodes a `ScaleSize` as its pixel value and `FullSize` as the
/// sentinel `0`; that encoding lives only in `to_db_size`/`from_db_size`, so the
/// `0` can never leak into application logic.
pub enum ThumbnailSpec {
    ScaleSize(ThumbnailSize),  // a scaled thumbnail, e.g. 300 / 512 / 2048
    FullSize,                  // original full-resolution rendition
}

impl ThumbnailSpec {
    /// On-disk `thumbnails.size` value. `FullSize` → 0; `ScaleSize(px)` → px.
    pub const fn to_db_size(self) -> u32 { ... }
    /// Inverse of `to_db_size`. 0 → `FullSize`; n → `ScaleSize(ThumbnailSize(n))`.
    pub const fn from_db_size(n: u32) -> Self { ... }
}

impl From<ThumbnailSize> for ThumbnailSpec {
    fn from(s: ThumbnailSize) -> Self { ThumbnailSpec::ScaleSize(s) }
}
```

Naming decision: the pixel newtype keeps the name `ThumbnailSize` (recently
introduced, used widely); the enum is `ThumbnailSpec`, so `ScaleSize(ThumbnailSize(300))`
reads cleanly.

### Invariants this feature depends on

- `thumbnails (image_hash, size)` has **no CHECK constraint** on `size` (verified
  in `src/schema.rs` migration 001: `size INTEGER NOT NULL`, `UNIQUE(image_hash,
  size)`), so `0` is a storable, never-otherwise-produced value. A scaled size of
  `0` is meaningless and never requested, so `0` unambiguously denotes `FullSize`.
- `decode_full_image` (in `src/decode.rs`) already yields full-resolution,
  EXIF-oriented, RAW-aware pixels — it is the producer for `FullSize`.

### Threading the type

`size: ThumbnailSize` → `spec: ThumbnailSpec` (or accept `impl Into<ThumbnailSpec>`)
at:

- `imgfind::thumbnail`: `get_or_generate_thumbnail`, `generate_thumbnail_bytes`,
  `generate_missing_thumbnails_batch`; consts `LIGHTBOX_SIZE`, `GUI_THUMBNAIL_SIZES`
  become `ThumbnailSpec::ScaleSize(...)`.
- `imgfind::database::Database`: `get_thumbnail`, `insert_thumbnail`,
  `get_images_without_thumbnails`, `count_images_without_thumbnails` — convert to
  the `u32` column value via `to_db_size` at the SQL boundary only.
- `imgfind-gui::backend::Backend::thumbnail`, `loader.rs`, `main.rs`
  (`DETAIL_SIZE`, preload `spawn_preload`).
- CLI `src/main.rs` `resolve_thumbnail_sizes` (the `--size`/`--gui-sizes` flags)
  still parse pixel integers into `ThumbnailSize`, wrapped as `ScaleSize`.

`generate_thumbnail_bytes` switches decode path on the spec: `ScaleSize(px)`
keeps the current `decode_image` + downscale; `FullSize` uses `decode_full_image`
and encodes at native resolution (JPEG, quality matching the existing encoder).

## Lightbox state (Rust, `imgfind-gui/src/main.rs`)

Alongside `lb_index` / `lb_generation`:

- `lb_zoom: Arc<Mutex<f32>>` — user zoom factor, clamped `0.1..=8.0`. (Mirrors
  Slint's `zoom`.)
- `lb_fit: Arc<Mutex<bool>>` — `true` = fit-to-window (default).
- A separate **full-res generation** counter (`lb_fullres_generation:
  AtomicU64`) so a background full-res decode only hot-swaps if the user hasn't
  since navigated to another image.

Both `zoom`/`fit` reset to default (`zoom = 1.0`, `fit = true`) on open and on
every prev/next, exactly like utmost. Pan offset lives in Slint
(`pan-x`/`pan-y`) and resets on navigation and on entering fit mode.

These are GUI-runtime only; **not** persisted to `ui_state` (matches the
existing lightbox, which persists only `selected_index`).

## Slint (`imgfind-gui/ui/app.slint`, lightbox block ~851–923)

Replace the single `Image` with the utmost two-image + chrome structure inside
the `if root.lightbox-open` Rectangle:

New properties on `MainWindow`:

```slint
in property <string> lightbox-filename;
in property <int>    lightbox-index1;   // 1-based position
in property <int>    lightbox-total;
in property <float>  lightbox-zoom: 1.0;
in property <bool>   lightbox-fit: true;
in property <float>  lightbox-fit-scale;   // what image-fit:contain yields
property <length>    lightbox-pan-x: 0px;
property <length>    lightbox-pan-y: 0px;
```

New callbacks:

```slint
callback lightbox-zoom-changed(float);  // absolute zoom, Rust clamps
callback lightbox-zoom-fit();
```

Body (inside a `clip: true` content Rectangle between the bars):

- `if lightbox-fit`: `Image { image-fit: contain; … }` centered (today's look).
- `if !lightbox-fit`: `Image` sized `source.width*1px*zoom × source.height*1px*zoom`,
  positioned with clamped pan so edges can't pull off-screen (utmost's clamp
  formula).
- A full-area `drag_touch := TouchArea` (below the nav arrows in z-order) that:
  - `scroll-event(e)`: if currently `fit`, first emit `lightbox-zoom-changed(fit-scale)`
    so the first wheel tick doesn't jump size; then
    `lightbox-zoom-changed(clamp(zoom * pow(1.1, e.delta-y / 60px), 0.1, 8.0))`;
    `return accept`.
  - drag-to-pan: capture `start-pan-x/y` on `changed pressed`, accumulate
    `(mouse - pressed)` deltas in `moved` (only when `!fit`).
  - `mouse-cursor`: `default` in fit, else `grab` / `grabbing` (pressed).
- Pan reset: `changed lightbox-index1 => { pan = 0 }`,
  `changed lightbox-fit => { if fit { pan = 0 } }`.

Top bar: filename (left), "`{index1} of {total}`" (center), close `×` (right, a
TouchArea). Bottom bar: `Fit` button (TouchArea → `lightbox-zoom-fit()`), a
`Slider` (min 0.1, max 8.0, value `lightbox-zoom`, `changed => lightbox-zoom-changed(self.value)`),
and a `Text` showing `round(displayed-zoom * 100)%`. ASCII glyphs only (`×`, `<`,
`>`) — symbol glyphs tofu in the default Slint font.

Keyboard: extend `app-keys` `capture-key-pressed` lightbox branch with `+`/`=`
(zoom × 1.25, cap 8.0), `-` (÷ 1.25, floor 0.1), `0` (fit). Existing Esc / arrows
/ h / l unchanged.

## Progressive resolution flow

1. **Open / navigate**: load the 2048px (`ScaleSize(LIGHTBOX)`) thumbnail as today
   (instant from cache). Reset zoom/fit/pan.
2. **First zoom past fit** (fit flips to false via wheel/slider/`+`): if a
   full-res rendition for the current image isn't already shown, spawn a
   background thread that calls `Backend::thumbnail(rel, ThumbnailSpec::FullSize)`
   — which cache-checks `thumbnails(hash, 0)`, and on miss decodes via
   `decode_full_image`, encodes JPEG, and persists under `size = 0`.
3. On completion, `invoke_from_event_loop`: if `lb_fullres_generation` is still
   current (user hasn't navigated away), `set_lightbox_image(full_res)` —
   **without touching** `lightbox-zoom` / `lightbox-fit` / `lightbox-pan-*`, so
   the visible window is preserved; only the pixels sharpen.
4. Navigating away bumps both `lb_generation` and `lb_fullres_generation`, so a
   late full-res decode for the previous image is dropped.

Triggering on first-zoom (not on open) keeps `FullSize` blobs bounded to images
the user actually inspects closely.

## Testing

Pure logic, table-driven (no GUI harness):

- `ThumbnailSpec::to_db_size` / `from_db_size` round-trip, incl. `FullSize ↔ 0`
  and `ScaleSize(px) ↔ px`; assert `from_db_size(0) == FullSize`.
- `ThumbnailSpec` cache round-trip through the DB: `insert_thumbnail(FullSize)`
  then `get_thumbnail(FullSize)` returns the bytes and is distinct from a
  `ScaleSize(2048)` row for the same hash (pins that `0` doesn't collide).
- Per the project's spec/test discipline: a `get_or_generate_thumbnail(FullSize)`
  test asserting a `size = 0` row is created and that `decode_full_image` is the
  path taken (the load-bearing test that pins the `FullSize` producer).
- Zoom math helpers (extract small pure fns in Rust where the clamp / `1.25`
  step / wheel-factor logic can live and be unit-tested): clamp to `0.1..=8.0`;
  `+`/`-` step; wheel `pow(1.1, delta/60)` factor sign.
- Pan-clamp helper: given image/viewport dimensions and a raw pan offset, the
  clamped offset never pulls an edge inside the viewport; centered when the image
  is smaller than the viewport.
- Existing thumbnail/backend tests updated to the new `ThumbnailSpec` signatures
  (keep coverage of the scaled path).

## Files touched

- `src/units.rs` — add `ThumbnailSpec` enum + conversions + tests.
- `src/thumbnail.rs` — thread `ThumbnailSpec`; `FullSize` decode path; consts.
- `src/database.rs` — `*_thumbnail*` methods take `ThumbnailSpec`, map at SQL.
- `src/main.rs` (CLI) — `resolve_thumbnail_sizes` wraps into `ScaleSize`.
- `imgfind-gui/src/backend.rs` — `thumbnail` takes `ThumbnailSpec`.
- `imgfind-gui/src/loader.rs` — grid loader uses `ScaleSize(300)`.
- `imgfind-gui/src/main.rs` — lightbox zoom/pan state, callbacks, progressive
  full-res load, pure zoom/pan helpers + tests; `DETAIL_SIZE`/preload updates.
- `imgfind-gui/ui/app.slint` — lightbox two-image body, chrome bars, scroll/drag
  handlers, keyboard.
- `CLAUDE.md` — document lightbox zoom/pan + `ThumbnailSpec`/`FullSize` cache.

## Risks

- **Full-res memory**: a 6000×4000 image decoded at native size is large; it
  lives only while shown (bounded by the lightbox showing one at a time + the
  generation guard dropping stale decodes). Acceptable.
- **Slint `scroll-event` availability**: confirmed via utmost's working
  implementation using the same Slint major version pattern; if the API differs,
  fall back to `TouchArea` pointer handling. (Verify early in implementation.)
