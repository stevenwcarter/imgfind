# EXIF Orientation in the Decode Seam — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `decode_image` return upright images by reading the EXIF `Orientation` tag and applying it, for RAW and non-RAW uniformly.

**Architecture:** Add a `read_exif_orientation(path) -> Option<u8>` helper (kamadak-exif, already a dep) to `src/decode.rs`, and apply it in `decode_image` via `image::metadata::Orientation::from_exif` + `DynamicImage::apply_orientation` — one unbranched step on the output of both decode paths. No CLI/schema changes; fix-forward only.

**Tech Stack:** Rust 2024, `image` 0.25.6 (`metadata::Orientation`, `apply_orientation`), `kamadak-exif` 0.6.1 (`Tag::Orientation`, `Value::get_uint`).

## Global Constraints

- Rust edition 2024; `cargo clippy --workspace --features tui -- -D warnings` clean; `cargo fmt --check` clean; pristine test output.
- Errors use `anyhow` with `.context()`/`.with_context()`. EXIF-orientation reading is **best-effort**: a read failure or missing tag must NEVER fail the decode — return `None` and apply no rotation.
- Uniform: orientation applies to RAW and non-RAW. No per-format branching of the orientation step.
- Fix-forward only: no CLI flags, no schema change, no thumbnail regeneration, no re-embedding.
- Verified API facts (do not re-derive): `image::metadata::Orientation::from_exif(u8) -> Option<Orientation>` (6 → `Rotate90`); `DynamicImage::apply_orientation(&mut self, Orientation)` (Rotate90 swaps a WxH landscape to HxW); `kamadak-exif` `Reader::read_from_container`, `field.value.get_uint(0) -> Option<u32>`, `Tag::Orientation` = 0x112.

---

### Task 1: Read and apply EXIF orientation in `decode_image`

**Files:**
- Modify: `src/decode.rs` (add `read_exif_orientation`; update `decode_image`; add tests + a test-only oriented-JPEG generator)

**Interfaces:**
- Consumes: existing `decode_raw`, `is_raw_extension` (unchanged).
- Produces: `decode_image` now returns orientation-corrected images; new private `fn read_exif_orientation(path: &Path) -> Option<u8>`.

- [ ] **Step 1: Write the failing test (load-bearing) + the no-op test + a test helper**

Add to the `#[cfg(test)] mod tests` in `src/decode.rs`:

```rust
/// Build JPEG bytes for a `w`x`h` image carrying an EXIF Orientation tag.
/// The `image` JPEG encoder does not emit EXIF, so we splice a minimal big-endian
/// EXIF APP1 segment (single SHORT Orientation entry) right after the SOI marker.
fn jpeg_with_orientation(w: u32, h: u32, exif_orientation: u8) -> Vec<u8> {
    use image::{ImageBuffer, ImageFormat, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(w, h, |x, _| Rgb([(x * 40) as u8, 100, 150]));
    let mut jpeg = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut jpeg), ImageFormat::Jpeg)
        .expect("encode jpeg");

    // APP1: marker FFE1, length 0x0022 (34), "Exif\0\0", TIFF(MM, 0x002A, IFD0@8),
    // 1 entry: tag 0x0112 (Orientation), type 0x0003 (SHORT), count 1,
    // value left-justified big-endian SHORT = 00 <orient> 00 00; then next-IFD = 0.
    let app1: Vec<u8> = vec![
        0xFF, 0xE1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0x00, 0x00, 0x4D, 0x4D, 0x00, 0x2A, 0x00,
        0x00, 0x00, 0x08, 0x00, 0x01, 0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00,
        exif_orientation, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    // Splice the APP1 segment in immediately after the 2-byte SOI (FFD8).
    let mut out = Vec::with_capacity(jpeg.len() + app1.len());
    out.extend_from_slice(&jpeg[..2]);
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

struct TempFile(std::path::PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn decode_image_applies_exif_orientation() {
    // 4x2 landscape tagged Orientation=6 (Rotate90 CW) -> upright should be 2x4.
    let bytes = jpeg_with_orientation(4, 2, 6);
    let path = std::env::temp_dir().join(format!("imgfind_orient6_{}.jpg", std::process::id()));
    std::fs::write(&path, &bytes).expect("write temp jpeg");
    let _cleanup = TempFile(path.clone());

    let decoded = decode_image(&path).expect("decode_image");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (2, 4),
        "Orientation=6 must rotate the 4x2 image to 2x4"
    );
}

#[test]
fn decode_image_without_orientation_tag_is_unchanged() {
    // Same image, no EXIF tag at all -> stays 4x2.
    use image::{ImageBuffer, ImageFormat, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(4, 2, |x, _| Rgb([(x * 40) as u8, 100, 150]));
    let path = std::env::temp_dir().join(format!("imgfind_noorient_{}.jpg", std::process::id()));
    image::DynamicImage::ImageRgb8(img)
        .save_with_format(&path, ImageFormat::Jpeg)
        .expect("save jpeg");
    let _cleanup = TempFile(path.clone());

    let decoded = decode_image(&path).expect("decode_image");
    assert_eq!((decoded.width(), decoded.height()), (4, 2));
}
```

- [ ] **Step 2: Run tests to verify the load-bearing one fails**

Run: `cargo test --lib decode::tests::decode_image_applies_exif_orientation -- --nocapture`
Expected: FAIL — without the fix, `image::open` ignores orientation, so dimensions are `(4, 2)`, not `(2, 4)`.
(`decode_image_without_orientation_tag_is_unchanged` should already PASS — it's the inert-case guard.)

- [ ] **Step 3: Add `read_exif_orientation` and apply it in `decode_image`**

In `src/decode.rs`, add the helper (place it near `decode_raw`):

```rust
/// Read the EXIF Orientation tag (0x0112, primary IFD) as its raw 1–8 value.
/// Best-effort: returns `None` on any read failure or when the tag is absent —
/// never propagates an error, so a missing/!broken EXIF block can't fail a decode.
fn read_exif_orientation(path: &Path) -> Option<u8> {
    use exif::{In, Reader, Tag};

    let file = std::fs::File::open(path).ok()?;
    let mut bufreader = std::io::BufReader::new(&file);
    let reader = Reader::new().read_from_container(&mut bufreader).ok()?;
    let field = reader.get_field(Tag::Orientation, In::PRIMARY)?;
    u8::try_from(field.value.get_uint(0)?).ok()
}
```

Update `decode_image` to apply it to both branches' output:

```rust
/// Decode any supported still or RAW image to a `DynamicImage`, corrected for
/// EXIF orientation (0x0112) so the result is upright.
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

    // Apply EXIF orientation uniformly (RAW preview / non-RAW alike). Best-effort:
    // no tag or unreadable EXIF leaves the image as-decoded.
    if let Some(orientation) =
        read_exif_orientation(path).and_then(image::metadata::Orientation::from_exif)
    {
        img.apply_orientation(orientation);
    }

    Ok(img)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib decode:: -- --nocapture`
Expected: PASS — `decode_image_applies_exif_orientation` now yields `(2, 4)`; the no-tag test stays `(4, 2)`; the existing classification/RAW-fixture tests stay green (the DNG fixture is Orientation=1, a no-op).

- [ ] **Step 5: Full gate**

Run:
```bash
cargo test --workspace --features tui
cargo clippy --workspace --features tui -- -D warnings
cargo fmt --check
```
Expected: all pass; no warnings; no diff. (Existing thumbnail/metadata tests unaffected.)

- [ ] **Step 6: Commit**

```bash
git add src/decode.rs
git commit -m "feat(decode): apply EXIF orientation to decoded images (RAW + non-RAW)"
```

---

### Task 2: Documentation

**Files:**
- Modify: `CLAUDE.md` (the `Decode seam` architecture bullet)

- [ ] **Step 1: Update the decode-seam bullet**

In `CLAUDE.md`, extend the `Decode seam (src/decode.rs)` bullet to note orientation handling, e.g. append:

> `decode_image` also applies the EXIF `Orientation` tag (0x0112, read via `kamadak-exif`) to the decoded image — RAW previews and oriented JPEGs alike come out upright. Fix-forward only: existing cached thumbnails are not regenerated. See `docs/superpowers/specs/2026-06-17-exif-orientation-design.md`.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: note EXIF orientation handling in the decode seam"
```

---

## Self-Review

**Spec coverage:**
- Uniform orientation in `decode_image` (both paths) — Task 1 Step 3. ✓
- `read_exif_orientation` via kamadak-exif — Task 1 Step 3. ✓
- Apply via `image::metadata::Orientation::from_exif` + `apply_orientation` — Task 1 Step 3. ✓
- Best-effort (never fails decode) — `read_exif_orientation` returns `Option`, applied only when `Some`. ✓
- Load-bearing test: oriented JPEG → dimension swap — Task 1 Step 1 (`decode_image_applies_exif_orientation`). ✓
- No-op guard (no tag → unchanged) — Task 1 Step 1 (`decode_image_without_orientation_tag_is_unchanged`). ✓
- Fix-forward only, no CLI/schema — no such tasks; Global Constraints. ✓
- Metadata dimensions now reflect oriented image — automatic (extract_image_metadata uses decode_image); covered by the dimension-swap behavior. ✓
- Docs — Task 2. ✓

**Placeholder scan:** No TBD/TODO; all code is concrete, including the exact EXIF byte layout. The spec's "commit a fixture OR generate" is resolved to in-test generation (deterministic, no binary blob, no external tooling) — an improvement that still satisfies the load-bearing requirement.

**Type consistency:** `read_exif_orientation(&Path) -> Option<u8>`; `Orientation::from_exif(u8) -> Option<Orientation>`; `apply_orientation(&mut self, Orientation)` — consistent throughout. `decode_image` signature unchanged (`&Path -> Result<DynamicImage>`), so all existing call sites compile untouched.
