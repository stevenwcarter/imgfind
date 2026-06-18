# Full-resolution RAW in the GUI lightbox — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a ≥2000px, correctly-oriented image for RAW files in the GUI lightbox instead of the tiny embedded thumbnail, without freezing the window.

**Architecture:** Add `imgfind::decode::decode_full_image` (RAW: largest preview if its long edge ≥ 2000px, else full demosaic; non-RAW: the original; EXIF orientation applied via a shared helper refactored out of `decode_image`). The GUI lightbox decodes through it on a background thread and hands finished pixels to the UI thread.

**Tech Stack:** Rust 2024, `rawler` 0.7.2 (`full_image`/`preview_image`/`raw_image`/`RawDevelop`), `image` 0.25.6, `slint` (`SharedPixelBuffer`, `Image::from_rgba8`), `anyhow`.

## Global Constraints

- Rust 2024; `cargo clippy --workspace --features tui -- -D warnings` clean; `cargo fmt --check` clean; pristine test output.
- Errors use `anyhow` with `.context()`/`.with_context()`. EXIF-orientation reading stays best-effort (never fails a decode).
- `decode_image` (the fast path used by thumbnails/embeddings/TUI/`search --display`) must stay behaviorally unchanged — only its inline orientation block moves into a shared helper.
- RAW full-res threshold: `const FULL_RAW_MIN_LONG_EDGE: u32 = 2000;` — compare the **long edge** (`width.max(height) >= 2000`).
- Scope: GUI lightbox only. No thumbnail/TUI/CLI resolution change.
- Verified facts: `DynamicImage` is `Send`; `slint::Image` is `!Send` (build it inside the UI closure). `rawler` `full_image`/`preview_image` return `Result<Option<DynamicImage>>`; the returned `DynamicImage` exposes `.width()/.height()`.

---

### Task 1: `decode_full_image` + shared orientation helper (`src/decode.rs`)

**Files:**
- Modify: `src/decode.rs` (refactor orientation into a helper; add const + threshold fn + `decode_full_image` + `decode_raw_full`; add tests)

**Interfaces:**
- Consumes: existing `read_exif_orientation`, `is_raw_extension`.
- Produces: `pub fn decode_full_image(path: &Path) -> anyhow::Result<image::DynamicImage>`; `fn preview_meets_full_threshold(width: u32, height: u32) -> bool`; `const FULL_RAW_MIN_LONG_EDGE: u32`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `src/decode.rs`:

```rust
#[test]
fn preview_threshold_uses_long_edge() {
    assert!(preview_meets_full_threshold(2000, 100));
    assert!(preview_meets_full_threshold(100, 2500));
    assert!(preview_meets_full_threshold(3000, 2000));
    assert!(!preview_meets_full_threshold(1999, 1999));
}

#[test]
fn decode_full_image_decodes_raw_fixture() {
    let p = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.dng"));
    let img = decode_full_image(p).expect("decode_full_image on sample.dng");
    assert!(img.width() > 0 && img.height() > 0);
}

#[test]
fn decode_full_image_non_raw_matches_image_open() {
    use image::{ImageBuffer, Rgb};
    let buf: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(6, 4, |x, _| Rgb([(x * 30) as u8, 80, 120]));
    let path = std::env::temp_dir().join(format!("imgfind_full_{}.png", std::process::id()));
    buf.save(&path).expect("save");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _c = Cleanup(path.clone());

    let via_full = decode_full_image(&path).expect("decode_full_image");
    let via_open = image::open(&path).expect("image::open");
    assert_eq!((via_full.width(), via_full.height()), (via_open.width(), via_open.height()));
    assert_eq!((via_full.width(), via_full.height()), (6, 4)); // full-res, no shrink
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib decode::tests::preview_threshold_uses_long_edge decode::tests::decode_full_image`
Expected: FAIL (won't compile: `preview_meets_full_threshold` / `decode_full_image` undefined). That compile failure is the expected RED.

- [ ] **Step 3: Refactor orientation into a shared helper**

In `src/decode.rs`, add the helper and make `decode_image` call it. Replace the inline block at the current `decode_image` (lines ~73-79):

```rust
/// Apply the file's EXIF orientation to `img` in place (best-effort; no-op if absent).
fn apply_exif_orientation(img: &mut image::DynamicImage, path: &Path) {
    if let Some(orientation) =
        read_exif_orientation(path).and_then(image::metadata::Orientation::from_exif)
    {
        img.apply_orientation(orientation);
    }
}
```

`decode_image` body becomes:

```rust
pub fn decode_image(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(is_raw_extension)
        .unwrap_or(false);

    let mut img = if is_raw {
        decode_raw(path)?
    } else {
        image::open(path).with_context(|| format!("decoding image {}", path.display()))?
    };

    apply_exif_orientation(&mut img, path);
    Ok(img)
}
```

(Behavior identical — the existing `decode_image_applies_exif_orientation` / `_without_orientation_tag_is_unchanged` tests are the characterization guard.)

- [ ] **Step 4: Add the threshold + full-resolution decoders**

Add near the top of the module (after the extension consts) the threshold:

```rust
/// Long-edge pixel floor for a RAW embedded preview to be used as-is for full-screen
/// viewing; below this we demosaic the sensor for full native resolution.
const FULL_RAW_MIN_LONG_EDGE: u32 = 2000;

/// True when an image of these dimensions is large enough to use as-is for full view.
fn preview_meets_full_threshold(width: u32, height: u32) -> bool {
    width.max(height) >= FULL_RAW_MIN_LONG_EDGE
}
```

Add the public entry point and the RAW helper:

```rust
/// Decode an image at full/high resolution for full-screen viewing (the GUI lightbox).
/// Non-RAW: the original (already full-res). RAW: the largest embedded preview if its
/// long edge is >= `FULL_RAW_MIN_LONG_EDGE`, else a full sensor demosaic. EXIF
/// orientation applied. For thumbnails/embeddings use the faster `decode_image` instead.
pub fn decode_full_image(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(is_raw_extension)
        .unwrap_or(false);

    let mut img = if is_raw {
        decode_raw_full(path)?
    } else {
        image::open(path).with_context(|| format!("decoding image {}", path.display()))?
    };

    apply_exif_orientation(&mut img, path);
    Ok(img)
}

/// Decode a RAW at full resolution: the largest embedded preview when its long edge is
/// >= `FULL_RAW_MIN_LONG_EDGE`, else a full sensor demosaic. If demosaic fails, falls
/// back to the (small) preview rather than erroring.
fn decode_raw_full(path: &Path) -> Result<image::DynamicImage> {
    use rawler::decoders::RawDecodeParams;
    use rawler::imgop::develop::RawDevelop;
    use rawler::rawsource::RawSource;

    let source =
        RawSource::new(path).with_context(|| format!("opening RAW file {}", path.display()))?;
    let decoder = rawler::get_decoder(&source)
        .with_context(|| format!("no RAW decoder for {}", path.display()))?;
    let params = RawDecodeParams::default();

    // Largest embedded preview: full_image preferred, else preview_image.
    let best_preview = match decoder
        .full_image(&source, &params)
        .with_context(|| format!("reading embedded full image for {}", path.display()))?
    {
        Some(img) => Some(img),
        None => decoder
            .preview_image(&source, &params)
            .with_context(|| format!("reading embedded preview for {}", path.display()))?,
    };

    let preview_big_enough = best_preview
        .as_ref()
        .map(|img| preview_meets_full_threshold(img.width(), img.height()))
        .unwrap_or(false);
    if preview_big_enough {
        return Ok(best_preview.expect("checked Some above"));
    }

    // Preview too small or absent: demosaic the sensor to full native resolution.
    let demosaic = (|| -> Result<image::DynamicImage> {
        let raw = decoder
            .raw_image(&source, &params, false)
            .with_context(|| format!("decoding RAW sensor data for {}", path.display()))?;
        let intermediate = RawDevelop::default()
            .develop_intermediate(&raw)
            .with_context(|| format!("developing RAW image {}", path.display()))?;
        intermediate
            .to_dynamic_image()
            .with_context(|| format!("converting developed RAW to image for {}", path.display()))
    })();

    match demosaic {
        Ok(img) => Ok(img),
        // A small preview beats nothing; only error if we have no preview at all.
        Err(e) => best_preview.ok_or(e),
    }
}
```

- [ ] **Step 5: Run tests + full gate**

Run:
```bash
cargo test --lib decode::
cargo test --workspace --features tui
cargo clippy --workspace --features tui -- -D warnings
cargo fmt --check
```
Expected: PASS — threshold cases correct; `decode_full_image(sample.dng)` non-empty; non-RAW matches `image::open` at 6×4; existing `decode_image_*` characterization tests stay green; no warnings; no fmt diff.

- [ ] **Step 6: Commit**

```bash
git add src/decode.rs
git commit -m "feat(decode): add decode_full_image (RAW >=2000px preview-or-demosaic) + shared orientation helper"
```

---

### Task 2: Lightbox decodes full-res off the UI thread (`imgfind-gui`)

**Files:**
- Modify: `imgfind-gui/src/image_util.rs` (add `dynamic_to_slint_image`; refactor `jpeg_to_slint_image` to delegate; add a test)
- Modify: `imgfind-gui/src/main.rs` (`load_lightbox_image` ~1019-1045)

**Interfaces:**
- Consumes: `imgfind::decode::decode_full_image` (Task 1); `image_util::dynamic_to_slint_image`.

- [ ] **Step 1: Write the failing test for the image_util helper**

Add to `#[cfg(test)] mod tests` in `imgfind-gui/src/image_util.rs`:

```rust
#[test]
fn dynamic_to_slint_image_preserves_dimensions() {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        3,
        2,
        image::Rgba([10, 20, 30, 255]),
    ));
    let slint_img = dynamic_to_slint_image(&img);
    assert_eq!(slint_img.size().width, 3);
    assert_eq!(slint_img.size().height, 2);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p imgfind-gui image_util::tests::dynamic_to_slint_image_preserves_dimensions`
Expected: FAIL (won't compile: `dynamic_to_slint_image` undefined).

- [ ] **Step 3: Add `dynamic_to_slint_image` and refactor `jpeg_to_slint_image`**

In `imgfind-gui/src/image_util.rs`:

```rust
//! Decode image bytes / decoded images into a Slint image.

use anyhow::{Context, Result};
use image::DynamicImage;
use slint::{Image, SharedPixelBuffer};

/// Convert an already-decoded `DynamicImage` into a Slint `Image`.
pub fn dynamic_to_slint_image(img: &DynamicImage) -> Image {
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let buffer = SharedPixelBuffer::clone_from_slice(rgba.as_raw(), w, h);
    Image::from_rgba8(buffer)
}

pub fn jpeg_to_slint_image(bytes: &[u8]) -> Result<Image> {
    let img = image::load_from_memory(bytes).context("Failed to decode image bytes")?;
    Ok(dynamic_to_slint_image(&img))
}
```

(Keep the existing `jpeg_to_slint_image` tests; they still pass via delegation.)

- [ ] **Step 4: Rewrite `load_lightbox_image` to decode full-res off the UI thread**

In `imgfind-gui/src/main.rs`, replace the body of `load_lightbox_image` (~1019-1045):

```rust
fn load_lightbox_image(weak: Weak<MainWindow>, backend: Backend, rel_path: String) {
    std::thread::spawn(move || {
        let abs = backend.abs_path(&rel_path);
        // Full-resolution, RAW-aware, orientation-corrected decode — on this background
        // thread so a slow RAW demosaic never freezes the UI.
        let img = match imgfind::decode::decode_full_image(&abs) {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!("Lightbox: failed to decode {abs:?}: {e}");
                return;
            }
        };

        // `DynamicImage` is Send; move it to the UI thread and build the (!Send) Image there.
        slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            let slint_img = image_util::dynamic_to_slint_image(&img);
            w.set_lightbox_image(slint_img);
            w.set_lightbox_open(true);
        })
        .ok();
    });
}
```

- [ ] **Step 5: Build, gate, and smoke**

Run:
```bash
cargo build -p imgfind-gui
cargo test --workspace --features tui
cargo clippy --workspace --features tui -- -D warnings
cargo fmt --check
```
Expected: builds; image_util test passes; full suite green; no warnings; no fmt diff. (The lightbox threading itself is UI behavior — not unit-tested here; the decode path is covered by Task 1's tests.)

- [ ] **Step 6: Commit**

```bash
git add imgfind-gui/src/image_util.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): lightbox decodes full-resolution RAW off the UI thread"
```

---

### Task 3: Documentation

**Files:**
- Modify: `CLAUDE.md` (decode-seam bullet)

- [ ] **Step 1: Update the decode-seam bullet**

In `CLAUDE.md`, extend the `Decode seam (src/decode.rs)` bullet to note the full-res path, e.g. append:

> A separate `decode_full_image` is used by the GUI lightbox for full-screen viewing: for RAW it uses the largest embedded preview when its long edge is ≥ 2000px, else demosaics the sensor for full resolution (thumbnails/embeddings keep using the faster `decode_image`). See `docs/superpowers/specs/2026-06-18-lightbox-fullres-design.md`.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: note decode_full_image for the GUI lightbox"
```

---

## Self-Review

**Spec coverage:**
- `decode_full_image` (RAW preview≥2000 else demosaic; non-RAW full; orientation) — Task 1 Step 4. ✓
- Threshold const + `preview_meets_full_threshold` (long edge) — Task 1 Step 4 + test Step 1. ✓
- Graceful fallback (demosaic fail → small preview) — Task 1 Step 4 (`best_preview.ok_or(e)`). ✓
- Shared orientation helper; `decode_image` unchanged — Task 1 Step 3 (characterization tests). ✓
- Lightbox routes through it, decode off UI thread — Task 2 Step 4. ✓
- `image_util` DynamicImage→Image helper — Task 2 Step 3 + test Step 1. ✓
- Scope (lightbox only) — no other call sites touched. ✓
- Tests (pure threshold; integration non-empty; non-RAW parity; image_util dims) — Tasks 1 & 2. ✓
- Docs — Task 3. ✓

**Placeholder scan:** No TBD/TODO; all code concrete. The integration test asserts non-empty rather than a specific ≥2000 because the fixture's preview size is unknown (disclosed in the spec); the ≥2000 decision is pinned by the pure test.

**Type consistency:** `decode_full_image(&Path) -> Result<DynamicImage>`, `preview_meets_full_threshold(u32,u32) -> bool`, `apply_exif_orientation(&mut DynamicImage, &Path)`, `dynamic_to_slint_image(&DynamicImage) -> slint::Image` — consistent across tasks. `decode_image` signature unchanged.
