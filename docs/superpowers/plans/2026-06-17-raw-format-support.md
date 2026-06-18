# RAW Format Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make imgfind discover and fully process camera RAW files (NEF, DNG, ORF, and every other format the decoder supports) through the existing embed/thumbnail/EXIF/display pipeline.

**Architecture:** Introduce one decode seam — `src/decode.rs::decode_image(path) -> Result<DynamicImage>` — that routes RAW extensions to `rawler` (largest embedded preview first, full demosaic via `RawDevelop` as fallback) and everything else to today's `image::open`. Every existing pixel-decode call site is rewired through this seam, and the scanner allowlist is replaced by the seam's `is_supported_extension`.

**Tech Stack:** Rust (edition 2024), `rawler` 0.7.x (pure-Rust multi-format RAW decoder, dnglab project), existing `image` 0.25, `kamadak-exif`, `clipper` (local).

## Global Constraints

- Rust edition 2024; code must be `cargo clippy`-clean and `cargo fmt`-clean.
- Errors use `anyhow` with `.context()`/`.with_context()` (project convention).
- Non-RAW decode behavior must be **byte-for-byte unchanged** (the seam only adds a RAW branch).
- RAW coverage lives in exactly one list: `decode::RAW_EXTENSIONS`. "All formats the decoder supports."
- A file that fails to decode is logged + counted as an error and skipped — never aborts a batch (matches current corrupt-file handling).
- `rawler` 0.7.x exact entry-point names/signatures (`get_decoder`, `RawSource`/`RawDecodeParams`, `full_image`/`preview_image`/`raw_image`, the `RawDevelop` builder, and how to obtain a `DynamicImage` from the developed result) **must be pinned against docs.rs/source for the resolved 0.7.x version at implementation time** — do not ship guessed signatures. The behavior (preview → develop fallback, returning `image::DynamicImage`) is fixed; the exact calls are an implementation detail to verify.

---

### Task 1: Decode seam — extension tables, classification, non-RAW dispatch

Creates the module and its pure, fully-testable surface. RAW decoding is stubbed here (returns an explanatory error) so this task compiles and is fully green **without** the `rawler` dependency. Task 2 fills in the RAW branch.

**Files:**
- Create: `src/decode.rs`
- Modify: `src/lib.rs` (add `pub mod decode;`)
- Test: inline `#[cfg(test)] mod tests` in `src/decode.rs`

**Interfaces:**
- Produces:
  - `pub const STILL_EXTENSIONS: &[&str]`
  - `pub const RAW_EXTENSIONS: &[&str]`
  - `pub fn is_raw_extension(ext: &str) -> bool` (case-insensitive)
  - `pub fn is_supported_extension(ext: &str) -> bool` (case-insensitive; union)
  - `pub fn decode_image(path: &std::path::Path) -> anyhow::Result<image::DynamicImage>`
  - `fn decode_raw(path: &std::path::Path) -> anyhow::Result<image::DynamicImage>` (private; stub in this task)

- [ ] **Step 1: Write the failing tests**

Add to `src/decode.rs` (the file does not exist yet — create it with just the test module + minimal signatures so it compiles to a failing state; or create the full module per Step 3 and confirm tests fail before that). Tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_still_extensions_case_insensitively() {
        assert!(is_supported_extension("jpg"));
        assert!(is_supported_extension("JPG"));
        assert!(is_supported_extension("Png"));
        assert!(!is_raw_extension("jpg"));
    }

    #[test]
    fn recognizes_raw_extensions_case_insensitively() {
        assert!(is_raw_extension("nef"));
        assert!(is_raw_extension("NEF"));
        assert!(is_raw_extension("dng"));
        assert!(is_raw_extension("orf"));
        assert!(is_supported_extension("nef")); // union includes raw
    }

    #[test]
    fn rejects_unknown_extension() {
        assert!(!is_supported_extension("txt"));
        assert!(!is_raw_extension("txt"));
        assert!(!is_supported_extension(""));
    }

    #[test]
    fn decode_image_non_raw_matches_image_open() {
        // Use an existing repo image asset as a known-good still.
        let p = std::path::Path::new("clipper-cat.jpg"); // adjust to a real committed image if needed
        if p.exists() {
            let via_seam = decode_image(p).unwrap();
            let via_image = image::open(p).unwrap();
            assert_eq!(via_seam.width(), via_image.width());
            assert_eq!(via_seam.height(), via_image.height());
        }
    }
}
```

> If no committed still image exists at the repo root for the equivalence test, the implementer should point it at any small committed image (check `assets/`, `tests/`, examples) or write a 2×2 PNG to a tempfile first. The test must exercise a real non-RAW decode.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib decode::`
Expected: FAIL — `decode` module / functions not defined.

- [ ] **Step 3: Write the module**

`src/decode.rs`:

```rust
//! Single decode seam for every pixel-decode in imgfind.
//!
//! RAW files (by extension) are decoded via `rawler` — largest embedded preview
//! first, full demosaic as a fallback (see `decode_raw`, Task 2). Every other
//! extension uses the `image` crate exactly as before.

use anyhow::{Context, Result};
use std::path::Path;

/// Lowercased extensions the `image` crate decodes (imgfind's historical set).
pub const STILL_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp"];

/// Lowercased RAW extensions `rawler` can decode. This is the single, explicit
/// place where RAW format coverage is declared.
pub const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", // Nikon
    "dng", // Adobe / generic
    "orf", // Olympus
    "cr2", "cr3", "crw", // Canon
    "arw", "sr2", "srf", // Sony
    "raf", // Fujifilm
    "rw2", // Panasonic
    "pef", // Pentax
    "srw", // Samsung
    "erf", // Epson
    "mrw", // Minolta
    "raw", "rwl", // Leica / misc
    "iiq", "3fr", "fff", // Phase One / Hasselblad
    "mef", "mos", "kdc", "dcr", // Mamiya / Leaf / Kodak
];

/// True if `ext` (with or without case) is a RAW format we decode via rawler.
pub fn is_raw_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    RAW_EXTENSIONS.contains(&ext.as_str())
}

/// True if `ext` is any image format the scanner should pick up (still or RAW).
pub fn is_supported_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    STILL_EXTENSIONS.contains(&ext.as_str()) || RAW_EXTENSIONS.contains(&ext.as_str())
}

/// Decode any supported still or RAW image to a `DynamicImage`.
pub fn decode_image(path: &Path) -> Result<image::DynamicImage> {
    let is_raw = path
        .extension()
        .and_then(|e| e.to_str())
        .map(is_raw_extension)
        .unwrap_or(false);

    if is_raw {
        decode_raw(path)
    } else {
        image::open(path).with_context(|| format!("decoding image {}", path.display()))
    }
}

/// Decode a RAW file via rawler (preview → demosaic fallback). Filled in Task 2.
fn decode_raw(path: &Path) -> Result<image::DynamicImage> {
    anyhow::bail!("RAW decoding not yet implemented for {}", path.display())
}
```

Add to `src/lib.rs` alongside the other `pub mod` lines:

```rust
pub mod decode;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib decode::`
Expected: PASS (the non-RAW equivalence test runs against a real image; classification tests pass).

- [ ] **Step 5: Verify clean**

Run: `cargo clippy --lib 2>&1 | tail -5 && cargo fmt --check`
Expected: no warnings, no diff.

- [ ] **Step 6: Commit**

```bash
git add src/decode.rs src/lib.rs
git commit -m "feat(decode): add decode seam with extension classification (non-RAW path)"
```

---

### Task 2: Implement RAW decode via rawler (preview → demosaic fallback)

**Files:**
- Modify: `Cargo.toml` (add `rawler`)
- Modify: `src/decode.rs` (replace the `decode_raw` stub)
- Create: `tests/fixtures/` (RAW sample — see Step 1)
- Test: inline integration test in `src/decode.rs`

**Interfaces:**
- Consumes: `is_raw_extension`, `decode_image` from Task 1.
- Produces: a working `decode_raw(path) -> Result<DynamicImage>`.

- [ ] **Step 1: Obtain a RAW fixture and write the failing integration test**

Source one **small** real RAW file and commit it at `tests/fixtures/sample.dng` (a small DNG is preferred — open spec, broadly decodable, usually the smallest real RAW). Good sources: a phone/camera DNG you own, or the smallest sample from a permissively-licensed RAW sample set. Keep it small (ideally < 5 MB).

Add to `src/decode.rs` test module:

```rust
const RAW_FIXTURE: &str = "tests/fixtures/sample.dng";

#[test]
fn decodes_real_raw_to_nonempty_image() {
    let p = std::path::Path::new(RAW_FIXTURE);
    if !p.exists() {
        eprintln!("skipping: RAW fixture {RAW_FIXTURE} not present");
        return;
    }
    let img = decode_image(p).expect("RAW decode should succeed");
    assert!(img.width() > 0 && img.height() > 0);
}
```

> Fixture fallback (per spec): if you genuinely cannot source a small, licensable real RAW file to commit, keep this test but leave the fixture absent so it self-skips (as written above), AND additionally add an `#[ignore]`-marked variant documenting the expected path, so the intent is recorded. Do **not** fabricate a fake RAW — it would validate nothing. The Task 1 classification/dispatch unit tests remain the always-on guard.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib decode::decodes_real_raw -- --nocapture`
Expected: FAIL with the stub's "RAW decoding not yet implemented" (if fixture present), or self-skip (if absent — in which case proceed; the integration coverage is gated on fixture availability and the implementation is still required and exercised manually).

- [ ] **Step 3: Add the rawler dependency**

In `Cargo.toml` `[dependencies]`:

```toml
rawler = "0.7"
```

Run `cargo build` once to resolve the exact 0.7.x version, then **open docs.rs for that exact version** (or `cargo doc --open -p rawler`) and confirm the real signatures for: obtaining a decoder from a path/source, `RawDecodeParams`, `full_image` / `preview_image`, `raw_image`, and `rawler::imgop::develop::RawDevelop` → `DynamicImage`.

- [ ] **Step 4: Implement `decode_raw`**

Replace the stub. The **logic is fixed**; adjust the exact rawler calls to the resolved version's API (this is the one place where API-pinning per Global Constraints applies):

```rust
fn decode_raw(path: &Path) -> Result<image::DynamicImage> {
    // Build a rawler decoder for this file. (Pin exact constructor against the
    // resolved rawler 0.7.x API — RawSource/BufReader + get_decoder.)
    let raw_source = rawler::rawsource::RawSource::new(path)
        .with_context(|| format!("opening RAW file {}", path.display()))?;
    let decoder = rawler::get_decoder(&raw_source)
        .with_context(|| format!("no RAW decoder for {}", path.display()))?;
    let params = rawler::decoders::RawDecodeParams::default();

    // 1) Largest embedded preview (camera-rendered JPEG) — fast common path.
    if let Ok(Some(img)) = decoder.full_image(&raw_source, &params) {
        return Ok(img);
    }
    if let Ok(Some(img)) = decoder.preview_image(&raw_source, &params) {
        return Ok(img);
    }

    // 2) Fallback: demosaic the raw sensor data to sRGB.
    let raw_image = decoder
        .raw_image(&raw_source, &params, false)
        .with_context(|| format!("decoding RAW sensor data for {}", path.display()))?;
    let developed = rawler::imgop::develop::RawDevelop::default()
        .develop_intermediate(&raw_image)
        .with_context(|| format!("developing RAW image {}", path.display()))?;
    let img = developed
        .to_dynamic_image()
        .with_context(|| format!("converting developed RAW to image for {}", path.display()))?;
    Ok(img)
}
```

> The method names above (`full_image`, `preview_image`, `raw_image`, `RawDevelop`, `develop_intermediate`, `to_dynamic_image`) match the rawler trait surface confirmed during design, but argument counts/borrow forms differ between 0.7.x patch releases — adapt to compile against the resolved version. Keep the preview-first → develop-fallback order and the `DynamicImage` return type.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib decode::decodes_real_raw -- --nocapture`
Expected: PASS (fixture present) — a non-empty image is produced.

- [ ] **Step 6: Verify clean**

Run: `cargo clippy --lib 2>&1 | tail -5 && cargo fmt --check`
Expected: no warnings, no diff.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/decode.rs tests/fixtures/
git commit -m "feat(decode): decode RAW via rawler (embedded preview, demosaic fallback)"
```

---

### Task 3: Route scanner + core decode sites through the seam

Rewires discovery and the three core decode sites (embedding, thumbnail, metadata-dimensions) — plus the TUI site — to the seam. After this, indexing a directory of RAW files produces vectors, thumbnails, and metadata.

**Files:**
- Modify: `src/main.rs` (scanner allowlist ~line 371 & 415-421; embedding decode ~line 515)
- Modify: `src/thumbnail.rs` (`generate_thumbnail_bytes` ~line 165-178)
- Modify: `src/database.rs` (`extract_image_metadata` dimensions ~line 1369-1375)
- Modify: `src/tui/app/zoom.rs` (decode ~line 135-147)
- Test: integration test for metadata over the RAW fixture (inline in `database.rs` tests, or `tests/`)

**Interfaces:**
- Consumes: `imgfind::decode::{decode_image, is_supported_extension}` from Tasks 1–2.

- [ ] **Step 1: Write the failing metadata integration test**

Add a test asserting RAW metadata extraction populates dimensions + EXIF (invariants 4 & 5). Place in `src/database.rs` test module:

```rust
#[test]
fn extracts_metadata_from_raw_fixture() {
    let fixture = "tests/fixtures/sample.dng";
    if !std::path::Path::new(fixture).exists() {
        eprintln!("skipping: RAW fixture {fixture} not present");
        return;
    }
    let md = extract_image_metadata(fixture).expect("metadata extraction");
    assert!(md.width.is_some() && md.height.is_some(), "dimensions populated");
    // At least one EXIF identity field should be present in a real RAW.
    assert!(
        md.camera_make.is_some() || md.camera_model.is_some() || md.datetime_taken.is_some(),
        "some EXIF field populated"
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --lib database::extracts_metadata_from_raw -- --nocapture`
Expected: FAIL — `extract_image_metadata` still uses `image::open`, so dimensions are `None` for the RAW fixture (test asserts `Some`).

- [ ] **Step 3: Rewire `extract_image_metadata` dimensions**

In `src/database.rs`, replace the dimensions block (~1369-1375):

```rust
    // Get image dimensions (RAW-aware via the decode seam; best-effort).
    if let Ok(img) = crate::decode::decode_image(std::path::Path::new(file_path)) {
        let (width, height) = img.dimensions();
        metadata.width = Some(width);
        metadata.height = Some(height);
    }
```

Remove the now-unused `use image::ImageReader as ImgReader;` import in that function if it is no longer referenced.

- [ ] **Step 4: Rewire the scanner allowlist**

In `src/main.rs`, delete the local `image_extensions` HashSet (~371-374) and replace the membership check (~415-421) with:

```rust
        // Check if it's a supported image (still or RAW) by extension.
        if let Some(ext_str) = path.extension().and_then(|e| e.to_str())
            && imgfind::decode::is_supported_extension(ext_str)
        {
            image_files.push(path.to_path_buf());
        }
```

(Use the crate path that matches `main.rs`'s existing imports — `imgfind::decode::…` or `crate::decode::…`.) Remove the now-unused `HashSet` import if nothing else uses it.

- [ ] **Step 5: Rewire the embedding decode**

In `src/main.rs` (~515), replace `image::open(abs_path)` with:

```rust
            let img = match imgfind::decode::decode_image(abs_path) {
```

(The `match` arms and error handling stay identical.)

- [ ] **Step 6: Rewire the thumbnail decode**

In `src/thumbnail.rs` `generate_thumbnail_bytes` (~166-169), replace:

```rust
    let image = crate::decode::decode_image(std::path::Path::new(filepath))
        .with_context(|| format!("Failed to decode image: {}", filepath))?;
```

Drop the now-unused `use image::ImageReader;` if nothing else in the file uses it.

- [ ] **Step 7: Rewire the TUI decode**

In `src/tui/app/zoom.rs` (~135-147), replace the `ImageReader::open(&image_path)` → `decode()` block with:

```rust
                        match crate::decode::decode_image(std::path::Path::new(&image_path)) {
                            Ok(img) => img,
                            Err(e) => {
                                warn!("failed to decode image {image_path}: {e}");
                                return;
                            }
                        }
```

Remove the now-unused `ImageReader` import in that file if unreferenced.

- [ ] **Step 8: Run tests + clippy across the workspace (incl. tui feature)**

Run:
```bash
cargo test --workspace
cargo test --features tui --lib tui:: 2>&1 | tail -5   # ensure tui still compiles & passes
cargo clippy --workspace --features tui 2>&1 | tail -5
cargo fmt --check
```
Expected: all pass; the metadata RAW test now passes (fixture present); no clippy warnings; no fmt diff.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs src/thumbnail.rs src/database.rs src/tui/app/zoom.rs
git commit -m "feat: route scanner and decode sites through the RAW-aware decode seam"
```

---

### Task 4: RAW-aware inline terminal display (`search --display`)

`print_image` streams raw *file bytes* to the terminal via iterm2img; a RAW file would render as garbage. Decode RAW through the seam and re-encode to JPEG before display; non-RAW is unchanged (original bytes streamed as today).

**Files:**
- Modify: `src/main.rs` (`print_image` ~806-817)

**Interfaces:**
- Consumes: `imgfind::decode::{decode_image, is_raw_extension}`.

- [ ] **Step 1: Implement RAW branch in `print_image`**

Replace `print_image`:

```rust
fn print_image(path: &str) -> Result<()> {
    let is_raw = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(imgfind::decode::is_raw_extension)
        .unwrap_or(false);

    let bytes = if is_raw {
        // Terminals can't render RAW; decode via the seam and re-encode to JPEG.
        let img = imgfind::decode::decode_image(std::path::Path::new(path))
            .context("Failed to decode RAW image for display")?;
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .context("Failed to encode RAW preview as JPEG for display")?;
        buf
    } else {
        std::fs::read(path).context("Failed to read image file for display")?
    };

    let image = iterm2img::from_bytes(bytes)
        .width_auto()
        .height_percent(33)
        .preserve_aspect_ratio(true)
        .inline(true)
        .build();
    println!("{}", image);

    Ok(())
}
```

Ensure `image::ImageFormat` / `std::io::Cursor` are reachable (import or fully-qualify as shown).

- [ ] **Step 2: Verify build + clean**

Run: `cargo build 2>&1 | tail -5 && cargo clippy 2>&1 | tail -5 && cargo fmt --check`
Expected: builds; no warnings; no fmt diff. (No unit test — this is terminal-escape output; covered by manual `search --display` against a RAW file. Behavior for non-RAW is unchanged.)

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat(display): decode RAW to JPEG for inline terminal display"
```

---

### Task 5: Documentation

**Files:**
- Modify: `README.md` (supported formats ~129-134)
- Modify: `USAGE.md` (supported formats ~194-199)
- Modify: `CLAUDE.md` (architecture notes)

- [ ] **Step 1: Update supported-formats lists**

In `README.md` and `USAGE.md`, add RAW formats to the supported-formats list, e.g.:

> **RAW (camera):** Nikon (.nef, .nrw), Adobe/generic (.dng), Olympus (.orf), Canon (.cr2, .cr3), Sony (.arw), Fujifilm (.raf), Panasonic (.rw2), and more — decoded via embedded preview with full-demosaic fallback.

- [ ] **Step 2: Update CLAUDE.md architecture notes**

Add a bullet under Architecture noting the decode seam:

> - **Decode seam (`src/decode.rs`)** — every pixel-decode (indexing, thumbnails, TUI, metadata dimensions, inline display) routes through `decode_image`. RAW files (extensions in `RAW_EXTENSIONS`) are decoded via `rawler` — largest embedded preview first, full demosaic (`RawDevelop`) as fallback; all other formats use the `image` crate. See `docs/superpowers/specs/2026-06-17-raw-format-support-design.md`.

- [ ] **Step 3: Commit**

```bash
git add README.md USAGE.md CLAUDE.md
git commit -m "docs: document RAW format support and the decode seam"
```

---

## Self-Review

**Spec coverage:**
- Discovery (allowlist → `is_supported_extension`) — Task 3 Step 4. ✓
- Decode strategy (preview → demosaic fallback) — Task 2. ✓
- rawler engine — Task 2. ✓
- One decode seam — Task 1. ✓
- 5 call sites (embedding, thumbnail, TUI, metadata-dims, print_image) — Tasks 3 & 4. ✓
- EXIF already works; only dimensions need seam — Task 3 Step 3 + metadata test. ✓
- Error handling (skip + count, no batch abort) — preserved in Task 3 Step 5 (match arms unchanged); Global Constraints. ✓
- Invariants 1–5 with tests — Task 1 (1,2,3), Task 2 (4), Task 3 (5). ✓
- Testing (unit classification always-on; integration gated on fixture) — Tasks 1 & 2. ✓
- Docs — Task 5. ✓

**Placeholder scan:** rawler API code is concrete with an explicit, bounded "pin to resolved 0.7.x" instruction (legitimate external-crate verification, not a TODO). Fixture has a defined fallback. No "add error handling"/"TBD" placeholders.

**Type consistency:** `decode_image(&Path) -> Result<DynamicImage>` and `is_supported_extension`/`is_raw_extension(&str) -> bool` used identically across all tasks. ✓
