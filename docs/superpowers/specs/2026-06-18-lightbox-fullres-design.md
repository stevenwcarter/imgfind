# Full-resolution RAW in the GUI lightbox

**Date:** 2026-06-18
**Status:** Approved
**Branch:** `lightbox-fullres`

## Goal

When a RAW file is opened in the GUI lightbox, show a **≥2000px, correctly-oriented**
image instead of the tiny embedded thumbnail it currently displays.

## Root cause

The lightbox bypasses the RAW decode seam entirely. `load_lightbox_image`
(`imgfind-gui/src/main.rs:1019`) does:

```rust
let bytes = std::fs::read(&abs)?;            // original file bytes
image_util::jpeg_to_slint_image(&bytes)        // image::load_from_memory(bytes)
```

For a RAW file, `image::load_from_memory` cannot decode the sensor data; for a
TIFF-based DNG/NEF it decodes a small embedded **thumbnail IFD** (the low-res raster),
and for some formats it fails. It also bypasses the EXIF-orientation step added to
`decode_image`, so RAW/oriented images in the lightbox are low-res *and* possibly
rotated. Two further problems in the same function:

- The decode runs **inside `invoke_from_event_loop`** (the UI thread), despite the
  comment claiming otherwise — so a slow decode freezes the window. This matters once
  RAW demosaic (1–3s) can run here.

## Design

### 1. New `imgfind::decode::decode_full_image`

A full-resolution sibling of `decode_image` in `src/decode.rs`:

```rust
/// Decode an image at full/high resolution for full-screen viewing.
/// Non-RAW: the original (already full-res). RAW: the largest embedded preview if its
/// long edge is >= FULL_RAW_MIN_LONG_EDGE, else a full sensor demosaic. EXIF orientation
/// applied. Use this for the lightbox; use `decode_image` for thumbnails/embeddings.
pub fn decode_full_image(path: &Path) -> Result<image::DynamicImage>
```

- **non-RAW:** `image::open(path)` (the file is already full resolution).
- **RAW:** `decode_raw_full(path)` — see below.
- **orientation:** applies the shared orientation step (see refactor).

### 2. `decode_raw_full` + threshold

```rust
const FULL_RAW_MIN_LONG_EDGE: u32 = 2000;

/// True when an image of these dimensions is large enough to use as-is for full view.
fn preview_meets_full_threshold(width: u32, height: u32) -> bool {
    width.max(height) >= FULL_RAW_MIN_LONG_EDGE
}
```

`decode_raw_full(path)`:
1. Open the rawler `RawSource` + `get_decoder` + `RawDecodeParams::default()` (same
   setup as `decode_raw`).
2. `best_preview` = `full_image(..)?` if `Some`, else `preview_image(..)?` (largest
   embedded preview, or `None`).
3. If `best_preview` is `Some(img)` and `preview_meets_full_threshold(img.dims)` →
   return it (fast common path).
4. Otherwise demosaic: `raw_image(..,false)?` → `RawDevelop::default().develop_intermediate(..)?`
   → `to_dynamic_image()`. Return the demosaiced image.
5. **Graceful fallback:** if demosaic fails (`Err`/`None`), return `best_preview` if we
   have one (a small image beats no image); only error if there is no preview either.

`decode_image` (the fast path) is **unchanged** — thumbnails/embeddings keep using the
small preview; only the lightbox calls `decode_full_image`.

### 3. Shared orientation helper (refactor)

Extract the orientation block currently inline in `decode_image` (`src/decode.rs:73-79`)
into:

```rust
/// Apply the file's EXIF orientation to `img` in place (best-effort; no-op if absent).
fn apply_exif_orientation(img: &mut image::DynamicImage, path: &Path) {
    if let Some(o) = read_exif_orientation(path).and_then(image::metadata::Orientation::from_exif) {
        img.apply_orientation(o);
    }
}
```

Both `decode_image` and `decode_full_image` call it. Behaviour of `decode_image` is
unchanged (characterization tests guard this).

### 4. Lightbox: decode off the UI thread (`imgfind-gui`)

Rewrite `load_lightbox_image` so the **decode runs on the background thread** and only a
cheap pixel hand-off happens on the UI thread:

```rust
fn load_lightbox_image(weak, backend, rel_path) {
    std::thread::spawn(move || {
        let abs = backend.abs_path(&rel_path);
        let img = match imgfind::decode::decode_full_image(&abs) {   // heavy work, bg thread
            Ok(i) => i,
            Err(e) => { tracing::warn!("Lightbox: failed to decode {abs:?}: {e}"); return; }
        };
        // DynamicImage is Send -> move it to the UI thread; build the (!Send) slint Image there.
        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            let slint_img = image_util::dynamic_to_slint_image(&img); // to_rgba8: a few ms
            w.set_lightbox_image(slint_img);
            w.set_lightbox_open(true);
        }).ok();
    });
}
```

The dominant cost (RAW demosaic) now runs off the UI thread; only the `to_rgba8`/buffer
copy (milliseconds, even for a 4000px image) remains on it. `slint::Image` is `!Send`, so
it must be constructed in the UI closure; `DynamicImage` is `Send` and moves in fine.

New `image_util::dynamic_to_slint_image(&DynamicImage) -> slint::Image` (`to_rgba8` →
`SharedPixelBuffer::clone_from_slice` → `Image::from_rgba8`); refactor the existing
`jpeg_to_slint_image` to `load_from_memory` then delegate to it (DRY).

## Scope

- **GUI lightbox only.** Grid thumbnails, TUI, embeddings, and `search --display` keep
  using `decode_image` (small previews are correct + fast there).
- The detail-panel seed thumbnail is unchanged (it's a thumbnail, not the full view).

## Invariants this feature depends on

1. **`decode_image`'s output is unchanged by the orientation refactor.** *Test:*
   existing `decode_image_*` tests stay green (characterization).
2. **rawler `full_image`/`preview_image` expose `.dimensions()` on the returned
   `DynamicImage`** so the threshold can be checked without demosaicing. *Test:* the
   pure `preview_meets_full_threshold` test + integration decode.
3. **`DynamicImage` is `Send`** (safe to move to the UI closure); **`slint::Image` is
   `!Send`** (must be built in the closure). *Verified by compilation.*

## Testing

- **Pure (unit):** `preview_meets_full_threshold` — `(2000,100)→true`, `(1999,1999)→false`,
  `(100,2500)→true` (long edge), `(3000,2000)→true`. This pins the ≥2000 decision.
- **Integration (`decode_full_image`):** over the committed `tests/fixtures/sample.dng`,
  assert a non-empty image is returned (it decodes — either preview or demosaic path).
  Note: the fixture's embedded preview size is unknown; the test asserts success + a
  positive size, not a specific ≥2000 (the threshold path itself is covered by the pure
  test). If the implementer confirms the fixture demosaics to ≥2000, tighten the assert.
- **Non-RAW parity:** `decode_full_image` on a generated still yields the same dimensions
  as `image::open` (full-res, no shrink).
- **`image_util`:** `dynamic_to_slint_image` preserves dimensions (small generated image).
- **Regression:** existing decode/orientation/thumbnail tests stay green.

## Out of scope

- Caching full-res decodes (re-decoded per lightbox open; acceptable).
- A configurable threshold (hard-coded const).
- Changing thumbnail/TUI/CLI display resolution.

## Documentation

- `CLAUDE.md` decode-seam bullet: note `decode_full_image` (RAW ≥2000px preview-or-demosaic)
  is used by the GUI lightbox, vs `decode_image` for thumbnails/embeddings.
