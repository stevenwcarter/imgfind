# Apply EXIF orientation in the decode seam

**Date:** 2026-06-17
**Status:** Approved (via `/ship-it --ask`)
**Branch:** `exif-orientation`

## Goal

Decoded images come out **upright**. Today the shared decode seam ignores the EXIF
`Orientation` tag (0x0112), so any image whose pixels are stored in a non-default
orientation renders rotated/flipped. This is most visible on **RAW previews** (the
camera-rendered embedded JPEG is returned un-rotated by rawler), but it affects
**regular JPEGs** carrying an orientation tag too (`image::open` does not auto-apply
orientation). Fix it once, uniformly, in `decode_image`.

## Background

- Both decode paths skip orientation: `image::open` (non-RAW) and rawler's
  `full_image`/`preview_image` (RAW) return pixels as-stored, ignoring 0x0112.
- We already depend on `kamadak-exif`, which reads `Tag::Orientation` from RAW (TIFF
  container) and JPEG alike via `read_from_container` (see `database.rs:1380`).
- `image` 0.25.6 provides the application mechanism, no manual pixel math:
  - `image::metadata::Orientation::from_exif(exif: u8) -> Option<Orientation>` maps the
    EXIF value (1–8) to the enum (1=Normal … 6=Rotate90 … 8=Rotate270, plus flips).
  - `DynamicImage::apply_orientation(&mut self, Orientation)` performs the transform.

## Design

Add to `src/decode.rs`:

```rust
/// Read the EXIF Orientation tag (0x0112, primary IFD) as its raw 1–8 value.
/// Returns None if the file has no readable EXIF or no orientation entry.
fn read_exif_orientation(path: &Path) -> Option<u8>
```

It opens the file, runs `kamadak-exif`'s `Reader::read_from_container`, and reads
`Tag::Orientation` from `In::PRIMARY` as a `u16`, narrowed to `u8` (valid range 1–8).

`decode_image` applies it to the result of **both** branches:

```rust
pub fn decode_image(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = /* …unchanged extension check… */;
    let mut img = if is_raw { decode_raw(path)? } else { image::open(path).with_context(…)? };
    if let Some(o) = read_exif_orientation(path).and_then(image::metadata::Orientation::from_exif) {
        img.apply_orientation(o);
    }
    Ok(img)
}
```

One unbranched orientation step; no per-format special-casing. Because every consumer
(indexing/embedding, thumbnails, TUI zoom, metadata dimensions, `search --display`)
routes through `decode_image`, all of them get upright images automatically.

## Scope (decided)

- **Uniform:** applies to RAW and non-RAW.
- **Fix-forward only:** no CLI flags, no schema changes, no thumbnail regeneration
  command, no re-embedding. Newly decoded images (live displays, newly-generated
  thumbnails, newly-indexed embeddings) are correct; **existing cached thumbnails keep
  their current orientation** until they happen to be regenerated. Live displays
  (TUI zoom, `search --display`) are correct immediately since they decode on the fly.

## Edge cases / error handling

- **No EXIF / no orientation tag / unreadable EXIF:** `read_exif_orientation` returns
  `None`; no rotation applied (the common case for many PNGs etc.). EXIF read failure
  must never fail the decode — it's best-effort, exactly like the existing metadata read.
- **Orientation = 1 (Normal):** `apply_orientation` is a no-op; fine to apply
  unconditionally when `Some`.
- **Dimensions metadata:** `extract_image_metadata` reads dimensions via `decode_image`,
  which now returns the *oriented* image — so stored width/height match the upright
  orientation (a portrait photo stored landscape+tag now records portrait dimensions).
  This is the correct, consistent result.

## Invariants this feature depends on

Per project spec-discipline, record what this relies on so a later change can grep for it:

1. **`kamadak-exif` reads `Tag::Orientation` from both RAW (TIFF) and JPEG containers.**
   *Test:* integration test asserting a known-oriented JPEG yields the expected value
   path (covered by the dimension-swap test below).
2. **rawler's embedded preview is returned un-rotated** (pixels as-stored), so applying
   the RAW file's 0x0112 tag is correct and does not double-rotate. *Verify in
   implementation against a real oriented RAW if available; the fixture DNG (LG-H815)
   is orientation=1, so it exercises the no-op path only — see Testing.*
3. **`image::metadata::Orientation::from_exif` + `DynamicImage::apply_orientation`**
   implement the standard EXIF orientation transforms. *Test:* dimension-swap test.

## Testing

- **Load-bearing integration test (required):** commit a tiny JPEG fixture with EXIF
  `Orientation = 6` (Rotate90) and **asymmetric dimensions** (e.g. 4×2). Assert
  `decode_image(fixture)` returns an image whose dimensions are **swapped** (2×4) —
  proving the seam actually applies the rotation end-to-end. A pure mapping test is
  insufficient (it wouldn't prove `decode_image` calls the transform), so it is not a
  substitute.
  - Fixture creation (resolve in plan): prefer generating/committing a real JPEG that
    carries an APP1 EXIF orientation tag (e.g. via `exiftool` offline if available, or a
    minimal hand-assembled EXIF/JPEG written in a small build/test helper). The fixture
    must be a *real* image with a *real* orientation tag — do not fake it.
- **No-op test:** an image with no orientation tag (or tag=1) decodes to unchanged
  dimensions (regression guard that the step is inert when it should be).
- **Regression:** existing decode/thumbnail/metadata tests stay green (the existing DNG
  fixture is orientation=1, so its assertions are unchanged).

## Out of scope

- Regenerating or invalidating existing cached thumbnails.
- Re-embedding already-indexed images.
- Any CLI/command or schema change.

## Documentation

- `CLAUDE.md` decode-seam bullet: note that `decode_image` applies EXIF orientation
  (0x0112) to the decoded image for RAW and non-RAW uniformly.
