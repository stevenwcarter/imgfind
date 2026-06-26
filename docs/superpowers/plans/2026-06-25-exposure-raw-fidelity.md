# Higher-Fidelity Exposure + Edit-UX Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make exposure edits preserve highlight detail by processing in linear light off the RAW sensor demosaic, add a busy spinner for the now-slower regenerate, and fix Reset to return the slider to neutral 0 EV.

**Architecture:** A new linear edit pipeline (`LinearRgb` + `decode_linear` + `render_edited`) is used **only for non-identity edits**; unedited images keep the existing fast preview path untouched. RAW files demosaic to linear via a custom `rawler` `RawDevelop` that omits the `SRgb` gamma step; exposure is applied in linear light with a soft-knee highlight roll-off, then tonemapped to sRGB 8-bit. The GUI live preview renders through the same function (WYSIWYG), with a busy spinner over the slow demosaic moments.

**Tech Stack:** Rust edition 2024, `image` 0.25 (incl. `Rgb32FImage`/`imageops::resize`), `rawler` 0.7.2 (`imgop::develop`), Slint GUI, `anyhow`, `tracing`.

## Global Constraints

- Rust edition 2024; errors use `anyhow` with `Context`/`with_context`; logging via `tracing`.
- All `Database` methods are `async`; sync callers use `imgfind::block_on(...)`.
- DB image paths are **relative to `Database.parent_dir`**; convert at boundaries.
- Exposure stays EV stops in **[−3.0, +3.0]**, default 0.0 (`ImageEdits { exposure: f32 }` already exists).
- **The thumbnail seam stays single:** all persisted renditions go through `generate_thumbnail_bytes`; the new linear branch lives inside it.
- **Identity edits must never touch the linear path** and must produce byte-identical output to the current fast path (no demosaic, no tonemap).
- New linear pipeline is **linear, scene-referred, sRGB primaries**; highlight roll-off uses a soft knee `HIGHLIGHT_KNEE = 0.8` (continuous value+slope at the knee, asymptotes to 1.0, never hard-clips).
- RAW linear decode demosaics the **sensor** (ignores the embedded preview) and applies EXIF orientation, matching the other decode paths.
- ASCII / Latin-1 only in Slint button/label text (default font tofus symbol glyphs); no focus-stealing `Button` widget in the lightbox sidebar — use the `TouchArea`+`Rectangle`+`Text` idiom.
- rustfmt-clean + clippy-clean (`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`). Dispatch Rust coding to the `rust-developer` agent.

---

### Task 1: Linear color math, `LinearRgb`, and `render_edited` (`src/edits.rs`)

**Files:**
- Modify: `src/edits.rs` (add the linear pipeline alongside the existing `ImageEdits`; keep `apply_adjustments` for now — removed in Task 3)
- Test: inline `#[cfg(test)]` in `src/edits.rs`

**Interfaces:**
- Consumes: `image` crate (`RgbImage`, `Rgb`, `Rgb32FImage`, `imageops`), existing `ImageEdits`.
- Produces:
  - `pub fn srgb_to_linear(c: f32) -> f32` and `pub fn linear_to_srgb(c: f32) -> f32` (normalized [0,1] transfer functions)
  - `pub const HIGHLIGHT_KNEE: f32 = 0.8;`
  - `pub fn tonemap_channel(linear: f32, ev: f32) -> u8`
  - `pub struct LinearRgb(pub image::Rgb32FImage);` with:
    - `pub fn from_srgb8(img: &image::RgbImage) -> LinearRgb`
    - `pub fn from_linear_u16(img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>) -> LinearRgb`
    - `pub fn downscale(&self, max_edge: u32) -> LinearRgb` (preserve aspect; never upscale)
    - `pub fn render(&self, edits: &ImageEdits) -> image::RgbImage`

- [ ] **Step 1: Write the failing tests**

Add to `src/edits.rs`:

```rust
#[cfg(test)]
mod linear_tests {
    use super::*;

    #[test]
    fn srgb_roundtrips() {
        for &x in &[0.0f32, 0.02, 0.2, 0.5, 0.8, 1.0] {
            let r = linear_to_srgb(srgb_to_linear(x));
            assert!((r - x).abs() < 1e-4, "roundtrip {x} -> {r}");
        }
    }

    #[test]
    fn tonemap_zero_ev_is_srgb_encode() {
        // Below the knee, 0 EV is just linear->sRGB->8bit.
        let got = tonemap_channel(0.2, 0.0);
        let want = (linear_to_srgb(0.2) * 255.0).round() as u8;
        assert_eq!(got, want);
    }

    #[test]
    fn tonemap_highlights_do_not_flatten() {
        // The anti-blowout guarantee: bright values pushed +1 EV stay below 255
        // and remain a strictly increasing gradient (no hard clamp to a flat 255).
        let a = tonemap_channel(0.85, 1.0);
        let b = tonemap_channel(1.5, 1.0);
        let c = tonemap_channel(5.0, 1.0);
        assert!(a < b && b <= c, "monotonic highlights: {a} {b} {c}");
        assert!(a < 255, "0.85 lin @ +1EV must not be pure white, got {a}");
        assert!(b < 255, "1.5 lin @ +1EV must keep headroom, got {b}");
    }

    #[test]
    fn tonemap_knee_is_continuous() {
        // No visible step right at the knee at 0 EV.
        let below = tonemap_channel(HIGHLIGHT_KNEE - 0.01, 0.0) as i32;
        let above = tonemap_channel(HIGHLIGHT_KNEE + 0.01, 0.0) as i32;
        assert!((above - below).abs() <= 2, "knee jump {below}->{above}");
    }

    #[test]
    fn render_brightens_with_exposure() {
        let mut buf = image::Rgb32FImage::new(2, 2);
        for p in buf.pixels_mut() {
            *p = image::Rgb([0.2, 0.2, 0.2]);
        }
        let lin = LinearRgb(buf);
        let dark = lin.render(&ImageEdits { exposure: 0.0 });
        let bright = lin.render(&ImageEdits { exposure: 1.0 });
        assert!(bright.get_pixel(0, 0)[0] > dark.get_pixel(0, 0)[0]);
    }

    #[test]
    fn downscale_preserves_aspect_and_caps_edge() {
        let buf = image::Rgb32FImage::new(400, 200);
        let small = LinearRgb(buf).downscale(100);
        assert_eq!(small.0.width(), 100);
        assert_eq!(small.0.height(), 50);
    }

    #[test]
    fn from_srgb8_then_render_zero_ev_roundtrips() {
        // sRGB8 -> linear -> render(0 EV) returns approximately the input pixels.
        let mut img = image::RgbImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgb([50, 128, 200]));
        let out = LinearRgb::from_srgb8(&img).render(&ImageEdits { exposure: 0.0 });
        let p = out.get_pixel(0, 0);
        for c in 0..3 {
            assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind edits::`
Expected: FAIL — `srgb_to_linear` / `LinearRgb` / `tonemap_channel` not found.

- [ ] **Step 3: Implement the linear pipeline**

Add to `src/edits.rs` (above the existing `apply_adjustments`, which stays untouched for now):

```rust
/// sRGB transfer function (IEC 61966-2-1) on normalized [0,1]: encoded -> linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function on normalized [0,1]: linear -> encoded.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Knee above which highlights roll off toward 1.0 instead of clipping.
pub const HIGHLIGHT_KNEE: f32 = 0.8;

/// Soft-knee highlight roll-off in linear light. Identity below the knee;
/// above it, a rational shoulder that is continuous in value and slope at the
/// knee and asymptotes to 1.0 (never hard-clips), so highlight gradients survive.
fn highlight_rolloff(v: f32) -> f32 {
    let k = HIGHLIGHT_KNEE;
    if v <= k {
        v
    } else {
        1.0 - (1.0 - k) * (1.0 - k) / (v - 2.0 * k + 1.0)
    }
}

/// Map one linear channel through exposure -> highlight roll-off -> sRGB gamma -> 8-bit.
pub fn tonemap_channel(linear: f32, ev: f32) -> u8 {
    let exposed = linear.max(0.0) * 2f32.powf(ev);
    let rolled = highlight_rolloff(exposed).clamp(0.0, 1.0);
    (linear_to_srgb(rolled) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A linear-light, scene-referred RGB image (sRGB primaries). Values are nominally
/// in [0,1] with sensor white at ~1.0; RAW highlight headroom lives just below 1.0.
pub struct LinearRgb(pub image::Rgb32FImage);

impl LinearRgb {
    /// Convert an 8-bit sRGB image to linear light.
    pub fn from_srgb8(img: &image::RgbImage) -> LinearRgb {
        let mut out = image::Rgb32FImage::new(img.width(), img.height());
        for (o, p) in out.pixels_mut().zip(img.pixels()) {
            *o = image::Rgb([
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
            ]);
        }
        LinearRgb(out)
    }

    /// Wrap a *linear* 16-bit RGB image (e.g. rawler develop without the sRGB step).
    pub fn from_linear_u16(img: &image::ImageBuffer<image::Rgb<u16>, Vec<u16>>) -> LinearRgb {
        let mut out = image::Rgb32FImage::new(img.width(), img.height());
        for (o, p) in out.pixels_mut().zip(img.pixels()) {
            *o = image::Rgb([
                p[0] as f32 / 65535.0,
                p[1] as f32 / 65535.0,
                p[2] as f32 / 65535.0,
            ]);
        }
        LinearRgb(out)
    }

    /// Downscale so the longest edge is at most `max_edge` (preserve aspect, never upscale).
    pub fn downscale(&self, max_edge: u32) -> LinearRgb {
        let (w, h) = (self.0.width(), self.0.height());
        let long = w.max(h);
        if long <= max_edge || long == 0 {
            return LinearRgb(self.0.clone());
        }
        let scale = max_edge as f32 / long as f32;
        let nw = (w as f32 * scale).round().max(1.0) as u32;
        let nh = (h as f32 * scale).round().max(1.0) as u32;
        LinearRgb(image::imageops::resize(
            &self.0,
            nw,
            nh,
            image::imageops::FilterType::Lanczos3,
        ))
    }

    /// Apply exposure + highlight roll-off + sRGB gamma, producing an 8-bit image.
    pub fn render(&self, edits: &ImageEdits) -> image::RgbImage {
        let ev = edits.clamped().exposure;
        let mut out = image::RgbImage::new(self.0.width(), self.0.height());
        for (o, p) in out.pixels_mut().zip(self.0.pixels()) {
            *o = image::Rgb([
                tonemap_channel(p[0], ev),
                tonemap_channel(p[1], ev),
                tonemap_channel(p[2], ev),
            ]);
        }
        out
    }
}
```

> Note: `image::Rgb32FImage` is `ImageBuffer<Rgb<f32>, Vec<f32>>`; `imageops::resize` supports it. If a trait bound complains, confirm the `image` feature set — `Rgb32FImage` is in default `image` 0.25.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind edits::`
Expected: PASS (existing `apply_adjustments` tests still pass too).

- [ ] **Step 5: Commit**

```bash
git add src/edits.rs
git commit -m "feat(edits): linear-light exposure pipeline with soft-knee highlight rolloff"
```

---

### Task 2: `decode_linear` — RAW demosaic to linear (`src/decode.rs`)

**Files:**
- Modify: `src/decode.rs` (add `decode_linear` + `decode_raw_linear`; new `rawler` imports)
- Test: inline `#[cfg(test)]` in `src/decode.rs`

**Interfaces:**
- Consumes: `crate::edits::LinearRgb` (Task 1); existing `decode_image`, `apply_exif_orientation`, `is_raw_extension`, `RAW_EXTENSIONS`; `rawler::imgop::develop::{ProcessingStep, RawDevelop}`, `rawler::decoders::RawDecodeParams`, `rawler::rawsource::RawSource`, `rawler::get_decoder`.
- Produces: `pub fn decode_linear(path: &std::path::Path) -> anyhow::Result<crate::edits::LinearRgb>`.

- [ ] **Step 1: Write the failing test (non-RAW branch)**

Add to `src/decode.rs`:

```rust
#[cfg(test)]
mod linear_decode_tests {
    use super::*;

    #[test]
    fn decode_linear_nonraw_roundtrips_at_zero_ev() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.png");
        let mut img = image::RgbImage::new(4, 4);
        for p in img.pixels_mut() {
            *p = image::Rgb([40, 130, 210]);
        }
        img.save(&path).unwrap();

        let lin = decode_linear(&path).unwrap();
        let out = lin.render(&crate::edits::ImageEdits { exposure: 0.0 });
        let p = out.get_pixel(0, 0);
        for c in 0..3 {
            assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
        }
    }
}
```

> RAW decoding is exercised by integration/manual verification (Task 6) — unit-testing it needs a committed RAW fixture, which the repo may not have. If `tests/` or a fixtures dir already contains a small RAW, add a `decode_linear` smoke test for it; otherwise rely on the non-RAW test plus manual RAW verification and say so in your report.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p imgfind decode::`
Expected: FAIL — `decode_linear` not found.

- [ ] **Step 3: Implement `decode_linear`**

Add to `src/decode.rs` (match the existing `rawler` import style already used by `decode_raw`):

```rust
use rawler::imgop::develop::{ProcessingStep, RawDevelop};

/// Decode `path` to a linear-light RGB image for high-fidelity editing.
///
/// RAW files are demosaiced from the **sensor** (ignoring the embedded preview)
/// via a custom `RawDevelop` that omits the final sRGB gamma step, so highlight
/// headroom above the camera-JPEG white point is preserved. Non-RAW files are
/// decoded normally and converted sRGB -> linear. EXIF orientation is applied.
pub fn decode_linear(path: &std::path::Path) -> anyhow::Result<crate::edits::LinearRgb> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if is_raw_extension(&ext) {
        decode_raw_linear(path)
    } else {
        let img = decode_image(path)?; // already EXIF-oriented sRGB
        Ok(crate::edits::LinearRgb::from_srgb8(&img.to_rgb8()))
    }
}

fn decode_raw_linear(path: &std::path::Path) -> anyhow::Result<crate::edits::LinearRgb> {
    use anyhow::Context;
    let source = rawler::rawsource::RawSource::new(path)
        .with_context(|| format!("open RAW source: {}", path.display()))?;
    let decoder = rawler::get_decoder(&source).context("get RAW decoder")?;
    let params = rawler::decoders::RawDecodeParams::default();
    let raw = decoder
        .raw_image(&source, &params, false)
        .context("decode RAW sensor image")?;
    // Develop to LINEAR: same steps as the default pipeline minus the final SRgb gamma.
    let develop = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
            ProcessingStep::WhiteBalance,
            ProcessingStep::Calibrate,
            ProcessingStep::CropDefault,
        ],
    };
    let intermediate = develop
        .develop_intermediate(&raw)
        .context("rawler develop_intermediate (linear)")?;
    let mut dynimg = intermediate
        .to_dynamic_image()
        .context("rawler intermediate produced no image")?;
    apply_exif_orientation(&mut dynimg, path);
    Ok(crate::edits::LinearRgb::from_linear_u16(&dynimg.to_rgb16()))
}
```

> Match the actual import paths used by the existing `decode_raw` (it already imports `RawDecodeParams`, `RawDevelop`, `RawSource` — reuse those imports rather than fully-qualifying if they're already in scope). The `false` arg to `raw_image` mirrors existing usage.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind decode::` then `cargo build -p imgfind`.
Expected: PASS / builds.

- [ ] **Step 5: Commit**

```bash
git add src/decode.rs
git commit -m "feat(decode): decode_linear — RAW sensor demosaic to linear light"
```

---

### Task 3: Route the thumbnail seam through the linear pipeline (`src/thumbnail.rs`, `src/edits.rs`)

**Files:**
- Modify: `src/thumbnail.rs` (`generate_thumbnail_bytes` branches identity vs non-identity)
- Modify: `src/edits.rs` (remove now-unused `apply_adjustments` + its old tests)
- Test: inline `#[cfg(test)]` in `src/thumbnail.rs`

**Interfaces:**
- Consumes: `crate::decode::decode_linear` (Task 2), `crate::edits::LinearRgb` (Task 1), existing `decode_image`/`decode_full_image`, `ThumbnailSpec`, `ImageEdits`.
- Produces: unchanged signature `generate_thumbnail_bytes(filepath: &str, spec: ThumbnailSpec, edits: &ImageEdits) -> Result<Vec<u8>>`; behavior: identity → existing fast path (byte-identical), non-identity → linear pipeline.

- [ ] **Step 1: Write/adjust the failing tests**

In `src/thumbnail.rs` keep the existing `edited_thumbnail_differs_from_unedited` test and add:

```rust
#[test]
fn identity_thumbnail_matches_plain_decode() {
    // Identity edits must take the fast path: bytes equal a direct decode+resize+encode.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.png");
    image::RgbImage::from_pixel(80, 80, image::Rgb([90, 90, 90]))
        .save(&path)
        .unwrap();
    let p = path.to_str().unwrap();

    let via_seam = generate_thumbnail_bytes(
        p,
        ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
        &ImageEdits::identity(),
    )
    .unwrap();

    // Reference: the exact fast-path operations.
    let img = crate::decode::decode_image(std::path::Path::new(p)).unwrap();
    let resized = img.resize(64, 64, image::imageops::FilterType::Lanczos3);
    let mut want = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut want), image::ImageFormat::Jpeg)
        .unwrap();

    assert_eq!(via_seam, want, "identity must be byte-identical to the fast path");
}

#[test]
fn highlight_edit_preserves_more_than_hard_clamp() {
    // A bright image pushed +2 EV through the linear path must NOT be a single
    // flat 255 block — decode the regenerated JPEG and assert it isn't all 255.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bright.png");
    // A gradient near white so a hard clamp would flatten it.
    let mut img = image::RgbImage::new(64, 1);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        let v = 200 + (x as u32 * 55 / 63) as u8; // 200..=255
        *px = image::Rgb([v, v, v]);
    }
    img.save(&path).unwrap();
    let bytes = generate_thumbnail_bytes(
        path.to_str().unwrap(),
        ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
        &ImageEdits { exposure: 2.0 },
    )
    .unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
    let all_white = decoded.pixels().all(|p| p[0] == 255);
    assert!(!all_white, "highlights flattened to pure white (blowout)");
}
```

- [ ] **Step 2: Run to verify the new tests fail**

Run: `cargo test -p imgfind thumbnail::`
Expected: `highlight_edit_preserves_more_than_hard_clamp` FAILS against the current hard-clamp `apply_adjustments` (it blows to 255); `identity_thumbnail_matches_plain_decode` may already pass.

- [ ] **Step 3: Branch the seam on identity**

In `generate_thumbnail_bytes`, replace the body that decodes + `apply_adjustments` + resizes with:

```rust
fn generate_thumbnail_bytes(
    filepath: &str,
    spec: ThumbnailSpec,
    edits: &ImageEdits,
) -> Result<Vec<u8>> {
    let path = std::path::Path::new(filepath);
    let out_image: image::DynamicImage = if edits.is_identity() {
        // Fast path (unchanged): no demosaic, no tonemap.
        match spec {
            ThumbnailSpec::ScaleSize(size) => {
                let img = crate::decode::decode_image(path)
                    .with_context(|| format!("Failed to decode image: {filepath}"))?;
                let px = size.get();
                img.resize(px, px, image::imageops::FilterType::Lanczos3)
            }
            ThumbnailSpec::FullSize => crate::decode::decode_full_image(path)
                .with_context(|| format!("Failed to decode full image: {filepath}"))?,
        }
    } else {
        // High-fidelity path: linear decode -> downscale in linear -> tonemap.
        let linear = crate::decode::decode_linear(path)
            .with_context(|| format!("Failed to decode (linear) image: {filepath}"))?;
        let sized = match spec {
            ThumbnailSpec::ScaleSize(size) => linear.downscale(size.get()),
            ThumbnailSpec::FullSize => linear,
        };
        image::DynamicImage::ImageRgb8(sized.render(edits))
    };

    let mut bytes: Vec<u8> = Vec::new();
    out_image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("Failed to encode thumbnail as JPEG")?;
    Ok(bytes)
}
```

Then in `src/edits.rs` **remove** the now-unused `apply_adjustments` function and its `#[cfg(test)] mod tests` block that tested it (the `linear_tests` module from Task 1 stays). Confirm no other references remain: `grep -rn apply_adjustments src/ imgfind-gui/`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind thumbnail::` then `cargo test --workspace`.
Expected: PASS, including `highlight_edit_preserves_more_than_hard_clamp` and `identity_thumbnail_matches_plain_decode`. Fix any `apply_adjustments` references the removal surfaced.

- [ ] **Step 5: Commit**

```bash
git add src/thumbnail.rs src/edits.rs
git commit -m "feat(thumbnail): route non-identity edits through the linear pipeline; drop 8-bit apply_adjustments"
```

---

### Task 4: GUI live preview via the linear pipeline (`imgfind-gui/src/backend.rs`, `imgfind-gui/src/main.rs`)

**Files:**
- Modify: `imgfind-gui/src/backend.rs` (`decode_lightbox_base` returns `LinearRgb`)
- Modify: `imgfind-gui/src/main.rs` (edit base type + live-preview render)
- Test: none new (integration/manual); rely on the core tests from Tasks 1–3.

**Interfaces:**
- Consumes: `imgfind::edits::LinearRgb` + `imgfind::decode::decode_linear` (Tasks 1–2), `imgfind::thumbnail::LIGHTBOX_SIZE`, existing `image_util::dynamic_to_slint_image`, the existing edit-mode state (`lb_edit_base`, `lb_edit_generation`), `render_edit_preview` helper, generation-guard idiom.
- Produces: live preview rendered through `LinearRgb::render` so it matches the baked thumbnails.

- [ ] **Step 1: Change `decode_lightbox_base` to return `LinearRgb`**

In `imgfind-gui/src/backend.rs`, replace the body of `decode_lightbox_base` so it returns the linear base downscaled to the lightbox size:

```rust
/// Decode the UNEDITED original of `rel_path` to a LINEAR base at the lightbox
/// long-edge size, for live edit-mode preview. RAW files demosaic the sensor
/// (slow) so the preview shows the same highlight headroom the baked thumbnails will.
pub fn decode_lightbox_base(&self, rel_path: &str) -> Result<imgfind::edits::LinearRgb> {
    let abs = self.abs_path(rel_path);
    let linear = imgfind::decode::decode_linear(&abs)
        .with_context(|| format!("decode (linear) original for edit preview: {rel_path}"))?;
    Ok(linear.downscale(imgfind::thumbnail::LIGHTBOX_SIZE.get()))
}
```

- [ ] **Step 2: Update the edit-base type and live render in `main.rs`**

Find the edit-base state (declared `let lb_edit_base: Arc<Mutex<Option<image::DynamicImage>>>`) and change its type to `Arc<Mutex<Option<imgfind::edits::LinearRgb>>>`. In `render_edit_preview` (and any place that applies the slider value to the base), replace the old `apply_adjustments(base.clone(), &ImageEdits{..})` with:

```rust
let rgb = base.render(&imgfind::edits::ImageEdits { exposure });
let dynimg = image::DynamicImage::ImageRgb8(rgb);
// ... existing dynamic_to_slint_image(&dynimg) + set_lightbox_image on the UI thread
```

`base` is the `LinearRgb`; `render` is cheap relative to the decode. Keep the existing generation-guard / off-UI-thread structure (do the `render` on the worker thread, set the `slint::Image` on the UI thread). The decode of the base on edit-entry already runs on a background thread — leave that threading intact (Task 5 adds the spinner around it).

- [ ] **Step 3: Build and basic check**

Run: `cargo build -p imgfind-gui` then `cargo clippy -p imgfind-gui --all-targets -- -D warnings`.
Expected: builds clean. Fix any type mismatches from the `DynamicImage` → `LinearRgb` change (e.g. `*lb_edit_base.lock() = Some(linear_base)` where the decode result is now `LinearRgb`).

- [ ] **Step 4: Commit**

```bash
git add imgfind-gui/src/backend.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): render live edit preview through the linear pipeline"
```

---

### Task 5: Fix Reset (→ neutral 0 EV) and add the busy spinner (`imgfind-gui/ui/app.slint`, `imgfind-gui/src/main.rs`)

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (slider binding fix; spinner markup; busy property)
- Modify: `imgfind-gui/src/main.rs` (reset → 0; set/clear `edit-busy` around the slow decodes)
- Test: none (UI). Verified by build + manual run.

**Interfaces:**
- Consumes: existing `edit-mode`, `edit-exposure`, `edit-exposure-label`, callbacks `edit-toggle/exposure-changed/reset/accept`; the edit-entry decode and accept threads from Task 4 / prior feature; `edits_ui::format_exposure`.
- Produces: working Reset; `in property <bool> edit-busy;`, `in property <string> edit-busy-label;` driving a spinner; disabled Reset/Accept while busy.

- [ ] **Step 1: Fix the slider so a Rust write reseats the thumb**

In `app.slint`, the sidebar `Slider` currently has one-way `value: root.edit-exposure;`. Make the thumb follow Rust writes by using a two-way binding:

```slint
Slider {
    minimum: -3.0;
    maximum: 3.0;
    value <=> root.edit-exposure;
    changed(v) => { root.edit-exposure-changed(v); }
}
```

> If `value <=> root.edit-exposure` causes a feedback loop with `changed` (it should not — `changed` only fires on user interaction), fall back to keeping `value: root.edit-exposure` but verify on the running GUI that `edit-reset()` writing `edit-exposure` moves the thumb. The deliverable is: clicking Reset visibly returns the thumb to center. The implementer MUST confirm this by running the GUI.

- [ ] **Step 2: Make `edit-reset` set neutral 0 and re-render**

In `main.rs`, the `on_edit_reset` handler: set exposure to `0.0`, update the label, and re-render the preview. Match the existing handler style:

```rust
window.on_edit_reset(move || {
    let Some(w) = weak.upgrade() else { return };
    w.set_edit_exposure(0.0);
    w.set_edit_exposure_label(edits_ui::format_exposure(0.0).into());
    // re-render the live preview at 0 EV (reuse the same path edit-exposure-changed uses)
    // e.g. call the shared render_edit_preview(... exposure = 0.0 ...) helper.
});
```

> Use the existing `render_edit_preview` helper / generation-guard path so Reset’s preview update goes through the same latest-wins machinery as slider drags.

- [ ] **Step 3: Add the busy property + spinner markup**

In `app.slint` `MainWindow`, near the other `edit-*` declarations add:

```slint
in property <bool> edit-busy;
in property <string> edit-busy-label: "Working...";
```

In the adjustments sidebar, add a spinner shown while busy (ASCII label, a rotating rectangle — no glyphs):

```slint
if root.edit-busy : VerticalLayout {
    spacing: 8px;
    Rectangle {
        width: 24px; height: 24px;
        spin := Rectangle {
            width: 24px; height: 24px;
            border-width: 3px;
            border-color: #888888;
            border-radius: 12px;
            // a single bright arc segment to make rotation visible
            Rectangle { width: 6px; height: 6px; x: 9px; y: -1px; background: white; border-radius: 3px; }
            rotation-angle: rot.angle;
        }
    }
    Text { text: root.edit-busy-label; color: #cccccc; font-size: 12px; }
}
// continuous rotation driver
rot := Rectangle {
    property <angle> angle;
    animate angle { duration: 900ms; iteration-count: -1; easing: linear; }
    init => { self.angle = 360deg; }
}
```

> If this exact spinner construction fights Slint’s layout/animation rules, use any equivalent that visibly animates while `edit-busy` and shows `edit-busy-label` — the requirement is a moving indicator, ASCII-only text, no focus stealing. Disable the Reset/Accept `TouchArea`s while busy (e.g. gate their `clicked` on `!root.edit-busy`, and dim them).

- [ ] **Step 4: Drive `edit-busy` from Rust around the slow work**

In `main.rs`:
- **Edit-entry decode** (where `decode_lightbox_base` is spawned on entering edit mode): set `w.set_edit_busy(true)` + `w.set_edit_busy_label("Preparing...".into())` before spawning; clear `set_edit_busy(false)` in the `invoke_from_event_loop` completion after the base is stored / preview rendered.
- **Accept** (`on_edit_accept`): set `set_edit_busy(true)` + label `"Saving..."` before `std::thread::spawn`; clear it in the existing `invoke_from_event_loop` completion (alongside `set_edit_mode(false)`). On the error-return path, also clear busy (don’t leave the spinner stuck) — handle by routing the error case through an `invoke_from_event_loop` that clears busy and logs.

> Keep the existing generation guards and `shown_fullres` reset from the prior feature intact.

- [ ] **Step 5: Build + manual verify**

Run: `cargo build -p imgfind-gui` and `cargo clippy -p imgfind-gui --all-targets -- -D warnings`.
Manual (document result): run the GUI on a library with a RAW; enter edit mode (spinner shows while the RAW demosaics), drag exposure (highlights roll off, not blown), click Reset (thumb returns to center, preview resets), click Accept (spinner "Saving...", then closes and the grid tile updates).

- [ ] **Step 6: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): fix Reset to neutral 0 EV and add a busy spinner for slow edits"
```

---

### Task 6: Workspace verification + docs

**Files:**
- Modify: `CLAUDE.md`
- No new tests.

- [ ] **Step 1: Full workspace check**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean/green. Fix anything that fails.

- [ ] **Step 2: Manual RAW smoke (best-effort, document result)**

Build and run the GUI against a library containing a RAW and a JPEG:
```bash
cargo run -p imgfind-gui -- --dir <a-test-library>
```
Verify: highlight detail is preserved when bumping exposure on a RAW (vs the old blowout); the spinner appears during the demosaic on edit-entry and during Accept; Reset returns to neutral. If no RAW library is available, note that the RAW path was verified only by the linear-pipeline unit tests and code review.

- [ ] **Step 3: Update `CLAUDE.md`**

Update the **Image adjustments** note (added by the prior feature) to reflect:
- exposure now uses a **linear-light pipeline with soft-knee highlight roll-off**, applied **only for non-identity edits** (unedited images keep the fast preview path);
- RAW files demosaic the **sensor** to linear via a custom `rawler` `RawDevelop` that omits the `SRgb` step (`src/decode.rs` `decode_linear`/`decode_raw_linear`), recovering highlight detail the embedded preview clipped; non-RAW convert sRGB→linear;
- the live preview renders through the same `LinearRgb::render` (WYSIWYG), with a busy spinner over the slow demosaic (edit-entry + Accept);
- Reset returns the slider to **neutral 0 EV**;
- link `docs/superpowers/specs/2026-06-25-exposure-raw-fidelity-design.md`.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: linear high-fidelity exposure pipeline, spinner, reset-to-neutral"
```

---

## Self-Review notes

- **Spec coverage:** linear math + roll-off (Task 1) · RAW linear demosaic (Task 2) · seam branch identity vs non-identity + remove old multiply (Task 3) · live preview WYSIWYG (Task 4) · Reset→0 + spinner (Task 5) · verify + docs (Task 6). The load-bearing anti-blowout test is in Task 1 (`tonemap_highlights_do_not_flatten`) and Task 3 (`highlight_edit_preserves_more_than_hard_clamp`). Identity-unchanged invariant pinned in Task 3.
- **Type consistency:** `LinearRgb(image::Rgb32FImage)`, `LinearRgb::{from_srgb8, from_linear_u16, downscale, render}`, `tonemap_channel(f32,f32)->u8`, `srgb_to_linear`/`linear_to_srgb`, `decode_linear(&Path)->Result<LinearRgb>` — used consistently across tasks. `decode_lightbox_base` return type changes to `LinearRgb` (Task 4), consumed by the edit-base `Arc<Mutex<Option<LinearRgb>>>`.
- **Adaptation points (flagged inline, not placeholders):** exact `rawler` import reuse, the Slint slider two-way-vs-one-way reset behavior (must be confirmed on the running GUI), and the precise spinner markup are matched to the existing code/runtime by the implementer; each step says so.
```
