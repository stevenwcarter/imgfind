# GUI Lightbox Zoom & Pan Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add scroll-wheel zoom and click-drag pan to the GUI lightbox (ported from `~/src/utmost`), with progressive full-native-resolution loading, behind a type-safe `ThumbnailSpec` rendition selector.

**Architecture:** First promote the thumbnail "size" parameter to a `ThumbnailSpec` enum (`ScaleSize(ThumbnailSize)` | `FullSize`) threaded through the cache accessors, so the full-resolution rendition is a first-class case stored under `thumbnails.size = 0`. Then build the lightbox: pure zoom/pan math helpers (TDD), Slint markup (two conditional images + chrome bars + scroll/drag handlers), Rust callback wiring, and a generation-guarded background full-res decode that hot-swaps sharper pixels without disturbing the user's zoom/pan window.

**Tech Stack:** Rust (edition 2024), Slint UI, `turso` async SQLite, `image` crate, `anyhow`.

## Global Constraints

- Rust edition 2024; `anyhow` with `Context`/`with_context` for errors; `tracing` for logging.
- ASCII / Latin-1 glyphs only in Slint text (`×`, `<`, `>`) — symbol glyphs tofu in the default font.
- Zoom range is `0.1..=8.0` (10%–800%). Lightbox navigation **clamps** at the ends (no wrap).
- The `thumbnails (image_hash, size)` schema is unchanged; **no new migration**. `FullSize` encodes as `size = 0`; a scaled size is never `0`.
- Zoom/pan/fit state is GUI-runtime only — **not** persisted to `ui_state`.
- Run `cargo fmt --all` and `cargo clippy --workspace --all-targets` clean before each commit. Dispatch Rust coding to the `rust-developer` agent.

---

### Task 1: `ThumbnailSpec` enum

**Files:**
- Modify: `src/units.rs` (add enum after `ThumbnailSize`, ~line 55; add tests in the `tests` module)
- Modify: `src/lib.rs:48` (re-export)

**Interfaces:**
- Consumes: existing `ThumbnailSize(pub u32)` with `.get() -> u32`.
- Produces:
  - `enum ThumbnailSpec { ScaleSize(ThumbnailSize), FullSize }` (derives `Debug, Clone, Copy, PartialEq, Eq, Hash`)
  - `ThumbnailSpec::to_db_size(self) -> u32` — `FullSize` → `0`, `ScaleSize(px)` → `px.get()`
  - `ThumbnailSpec::from_db_size(n: u32) -> ThumbnailSpec` — `0` → `FullSize`, `n` → `ScaleSize(ThumbnailSize(n))`
  - `impl From<ThumbnailSize> for ThumbnailSpec` — wraps as `ScaleSize`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/units.rs`:

```rust
#[test]
fn thumbnail_spec_db_size_round_trips() {
    use ThumbnailSpec::*;
    assert_eq!(FullSize.to_db_size(), 0);
    assert_eq!(ScaleSize(ThumbnailSize(2048)).to_db_size(), 2048);
    // round-trip both ways
    assert_eq!(ThumbnailSpec::from_db_size(0), FullSize);
    assert_eq!(
        ThumbnailSpec::from_db_size(300),
        ScaleSize(ThumbnailSize(300))
    );
    for spec in [FullSize, ScaleSize(ThumbnailSize(512))] {
        assert_eq!(ThumbnailSpec::from_db_size(spec.to_db_size()), spec);
    }
}

#[test]
fn thumbnail_size_converts_to_scale_spec() {
    let spec: ThumbnailSpec = ThumbnailSize(300).into();
    assert_eq!(spec, ThumbnailSpec::ScaleSize(ThumbnailSize(300)));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind units::tests::thumbnail_spec_db_size_round_trips units::tests::thumbnail_size_converts_to_scale_spec 2>&1 | tail -20`
Expected: FAIL — `cannot find type ThumbnailSpec`.

- [ ] **Step 3: Implement the enum**

Add after the `ThumbnailSize` impl block (~line 55) in `src/units.rs`:

```rust
/// Which rendition of an image to fetch / generate / store. The DB
/// `thumbnails.size` column encodes a `ScaleSize` as its pixel value and
/// `FullSize` as the sentinel `0`. That encoding lives only in
/// `to_db_size`/`from_db_size`, so the `0` can never leak into application
/// logic — callers always pass and match on the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbnailSpec {
    /// A scaled thumbnail with the given long-edge target (e.g. 300/512/2048).
    ScaleSize(ThumbnailSize),
    /// The original, full-resolution rendition.
    FullSize,
}

impl ThumbnailSpec {
    /// On-disk `thumbnails.size` value. `FullSize` → 0; `ScaleSize(px)` → px.
    pub const fn to_db_size(self) -> u32 {
        match self {
            ThumbnailSpec::ScaleSize(px) => px.get(),
            ThumbnailSpec::FullSize => 0,
        }
    }

    /// Inverse of [`to_db_size`](Self::to_db_size). `0` → `FullSize`, else
    /// `ScaleSize`.
    pub const fn from_db_size(n: u32) -> Self {
        match n {
            0 => ThumbnailSpec::FullSize,
            n => ThumbnailSpec::ScaleSize(ThumbnailSize(n)),
        }
    }
}

impl From<ThumbnailSize> for ThumbnailSpec {
    fn from(size: ThumbnailSize) -> Self {
        ThumbnailSpec::ScaleSize(size)
    }
}
```

Update the re-export in `src/lib.rs:48`:

```rust
pub use units::{EmbeddingDim, FileSize, MaxK, ThumbnailSize, ThumbnailSpec};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind units:: 2>&1 | tail -20`
Expected: PASS (all `units::tests`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p imgfind --all-targets 2>&1 | tail -5
git add src/units.rs src/lib.rs
git commit -m "feat(units): ThumbnailSpec enum (ScaleSize | FullSize)"
```

---

### Task 2: DB cache accessors take `ThumbnailSpec`

**Files:**
- Modify: `src/database.rs:1044-1087` (`insert_thumbnail`, `get_thumbnail`)
- Test: `src/database.rs` (add a test near the other DB tests, ~line 2160)

**Interfaces:**
- Consumes: `ThumbnailSpec`, `ThumbnailSpec::to_db_size` from Task 1.
- Produces:
  - `Database::insert_thumbnail(&self, image_hash: &str, spec: impl Into<ThumbnailSpec>, thumbnail_data: &[u8]) -> Result<()>`
  - `Database::get_thumbnail(&self, image_hash: &str, spec: impl Into<ThumbnailSpec>) -> Result<Vec<u8>>`
  - (`impl Into` means existing `ThumbnailSize` call sites compile unchanged via `From`.)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/database.rs` (use the existing test DB helper pattern in that module — mirror a nearby thumbnail/`Database::new` test for setup):

```rust
#[test]
fn full_size_thumbnail_round_trips_and_is_distinct_from_scaled() {
    use crate::{ThumbnailSize, ThumbnailSpec};
    let db_path = unique_test_db_path(); // existing helper in this module
    let db = block_on(Database::new(&db_path)).expect("create db");

    block_on(db.insert_thumbnail("h", ThumbnailSpec::FullSize, &[1, 2, 3])).unwrap();
    block_on(db.insert_thumbnail("h", ThumbnailSize(2048), &[9, 9])).unwrap();

    // FullSize stored under size=0, retrievable by the enum (not the integer).
    assert_eq!(
        block_on(db.get_thumbnail("h", ThumbnailSpec::FullSize)).unwrap(),
        vec![1, 2, 3]
    );
    // Distinct from the scaled row for the same hash — no key collision.
    assert_eq!(
        block_on(db.get_thumbnail("h", ThumbnailSize(2048))).unwrap(),
        vec![9, 9]
    );
}
```

> If the module's test-DB helper has a different name, use that one; the assertion logic is what matters.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind full_size_thumbnail_round_trips 2>&1 | tail -20`
Expected: FAIL — `insert_thumbnail` doesn't accept `ThumbnailSpec`.

- [ ] **Step 3: Change the two accessors**

`src/database.rs` `insert_thumbnail` (line ~1044):

```rust
pub async fn insert_thumbnail(
    &self,
    image_hash: &str,
    spec: impl Into<ThumbnailSpec>,
    thumbnail_data: &[u8],
) -> Result<()> {
    let size_col = i64::from(spec.into().to_db_size());
    let conn = self
        .pool
        .get()
        .await
        .context("Failed to get DB connection to insert thumbnail")?;
    conn.execute(
        "INSERT OR REPLACE INTO thumbnails (image_hash, size, thumbnail_data) \
         VALUES (?1, ?2, ?3)",
        (image_hash.to_string(), size_col, thumbnail_data.to_vec()),
    )
    .await
    .context("failed to insert or replace")?;
    Ok(())
}
```

`get_thumbnail` (line ~1070):

```rust
pub async fn get_thumbnail(
    &self,
    image_hash: &str,
    spec: impl Into<ThumbnailSpec>,
) -> Result<Vec<u8>> {
    let size_col = i64::from(spec.into().to_db_size());
    let conn = self
        .pool
        .get()
        .await
        .context("Failed to get DB connection to get thumbnail")?;
    let mut rows = conn
        .query(
            "SELECT thumbnail_data FROM thumbnails WHERE image_hash = ?1 AND size = ?2",
            (image_hash.to_string(), size_col),
        )
        .await?;
    let row = rows.next().await?.context("no thumbnail row")?;
    match row.get_value(0)? {
        Value::Blob(b) => Ok(b),
        _ => anyhow::bail!("thumbnail_data is not a blob"),
    }
}
```

Add `ThumbnailSpec` to the imports at `src/database.rs:5`:

```rust
    AbsolutePath, EmbeddingDim, MaxK, RelativePath, ThumbnailSize, ThumbnailSpec, db_pool,
    get_db_parent_dir,
```

(`get_images_without_thumbnails` and `count_images_without_thumbnails` keep `ThumbnailSize` — they are scaled-only batch helpers and must never request `FullSize`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind 2>&1 | tail -20`
Expected: PASS, including the new test and all existing thumbnail tests (which pass `ThumbnailSize` via `From`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p imgfind --all-targets 2>&1 | tail -5
git add src/database.rs
git commit -m "feat(db): thumbnail cache accessors take ThumbnailSpec; FullSize=0"
```

---

### Task 3: Full-resolution generation path in `thumbnail.rs`

**Files:**
- Modify: `src/thumbnail.rs:126-184` (`generate_thumbnail_bytes`, `generate_and_store_thumbnail`, `get_or_generate_thumbnail`)
- Test: `src/thumbnail.rs` (add to the `tests` module)

**Interfaces:**
- Consumes: `ThumbnailSpec`, `crate::decode::{decode_image, decode_full_image}`.
- Produces:
  - `generate_thumbnail_bytes(filepath: &str, spec: ThumbnailSpec) -> Result<Vec<u8>>`
  - `get_or_generate_thumbnail(db: &Database, filepath: &str, hash: &str, spec: impl Into<ThumbnailSpec>) -> Result<Vec<u8>>`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/thumbnail.rs` (reuse `temp_db_path()` and the 8×8 PNG fixture pattern already in that module):

```rust
/// `get_or_generate_thumbnail(FullSize)` persists a row under size=0 and the
/// returned bytes decode to the original (un-downscaled) dimensions.
#[test]
fn get_or_generate_full_size_persists_size_zero() {
    use crate::ThumbnailSpec;
    let db_path = temp_db_path();
    let db = block_on(Database::new(&db_path)).expect("create test db");
    let parent_dir = db_path.parent().unwrap().parent().unwrap();

    // 64×40 so the original is larger than a tiny thumbnail and clearly "full".
    let img_path = parent_dir.join("full_fixture.png");
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(64, 40, Rgb([10, 20, 30]));
    img.save(&img_path).unwrap();
    let abs_path = img_path.to_str().unwrap();
    let hash = "full_size_hash";

    assert!(block_on(db.get_thumbnail(hash, ThumbnailSpec::FullSize)).is_err());

    let bytes =
        get_or_generate_thumbnail(&db, abs_path, hash, ThumbnailSpec::FullSize).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!(
        (decoded.width(), decoded.height()),
        (64, 40),
        "FullSize must preserve original dimensions (no downscale)"
    );

    // Persisted under size=0 and retrievable as FullSize.
    assert!(block_on(db.get_thumbnail(hash, ThumbnailSpec::FullSize)).is_ok());
    let _ = std::fs::remove_dir_all(parent_dir);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p imgfind get_or_generate_full_size 2>&1 | tail -20`
Expected: FAIL — signatures don't accept `ThumbnailSpec`.

- [ ] **Step 3: Implement the FullSize branch**

`src/thumbnail.rs` — add `ThumbnailSpec` to the import on line 1:

```rust
use crate::{ThumbnailSize, ThumbnailSpec, database::Database, get_db_path};
```

Replace `generate_thumbnail_bytes` (line 126):

```rust
// Generate JPEG bytes for a thumbnail rendition (pure aside from file IO).
// `ScaleSize` downscales via the fast decode path; `FullSize` uses the
// RAW-aware full-resolution decode and encodes at native dimensions.
fn generate_thumbnail_bytes(filepath: &str, spec: ThumbnailSpec) -> Result<Vec<u8>> {
    let path = std::path::Path::new(filepath);
    let out_image = match spec {
        ThumbnailSpec::ScaleSize(size) => {
            let image = crate::decode::decode_image(path)
                .with_context(|| format!("Failed to decode image: {}", filepath))?;
            let px = size.get();
            image.resize(px, px, image::imageops::FilterType::Lanczos3)
        }
        ThumbnailSpec::FullSize => crate::decode::decode_full_image(path)
            .with_context(|| format!("Failed to decode full image: {}", filepath))?,
    };

    let mut bytes: Vec<u8> = Vec::new();
    out_image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("Failed to encode thumbnail as JPEG")?;
    Ok(bytes)
}
```

`generate_and_store_thumbnail` (line 141) — keep its `ThumbnailSize` signature (batch path is scaled-only) but wrap when calling the now-`ThumbnailSpec` helper:

```rust
fn generate_and_store_thumbnail(
    filepath: &str,
    hash: &str,
    size: ThumbnailSize,
    tx: &Sender<(String, u32, Vec<u8>)>,
) -> Result<()> {
    let bytes = generate_thumbnail_bytes(filepath, ThumbnailSpec::ScaleSize(size))?;
    tx.send((hash.to_string(), size.get(), bytes))
        .context("Failed to send thumbnail bytes over channel")?;
    Ok(())
}
```

Replace `get_or_generate_thumbnail` (line 166):

```rust
pub fn get_or_generate_thumbnail(
    db: &Database,
    filepath: &str,
    hash: &str,
    spec: impl Into<ThumbnailSpec>,
) -> Result<Vec<u8>> {
    let spec = spec.into();
    // First, try the database cache.
    if let Ok(thumbnail_data) = block_on(db.get_thumbnail(hash, spec)) {
        return Ok(thumbnail_data);
    }

    // Miss: generate, persist, return.
    let bytes = generate_thumbnail_bytes(filepath, spec)?;
    block_on(db.insert_thumbnail(hash, spec, &bytes))
        .context("Failed to store thumbnail in database")?;
    block_on(db.get_thumbnail(hash, spec))
        .context("Failed to retrieve newly generated thumbnail")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind 2>&1 | tail -20`
Expected: PASS — new FullSize test plus existing thumbnail tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p imgfind --all-targets 2>&1 | tail -5
git add src/thumbnail.rs
git commit -m "feat(thumbnail): FullSize rendition via decode_full_image"
```

---

### Task 4: Pure zoom/pan math helpers

**Files:**
- Create: `imgfind-gui/src/zoompan.rs`
- Modify: `imgfind-gui/src/main.rs` (add `mod zoompan;` near the other `mod` declarations)

**Interfaces:**
- Produces (all pure, no Slint types — plain `f32`):
  - `const ZOOM_MIN: f32 = 0.1; const ZOOM_MAX: f32 = 8.0; const ZOOM_STEP: f32 = 1.25;`
  - `clamp_zoom(z: f32) -> f32` — clamp to `[ZOOM_MIN, ZOOM_MAX]`
  - `zoom_in(z: f32) -> f32` — `clamp_zoom(z * ZOOM_STEP)`
  - `zoom_out(z: f32) -> f32` — `clamp_zoom(z / ZOOM_STEP)`
  - `wheel_zoom(z: f32, delta_px: f32) -> f32` — `clamp_zoom(z * 1.1f32.powf(delta_px / 60.0))`
  - `clamp_pan(image_len: f32, viewport_len: f32, pan: f32) -> f32` — when the image is larger than the viewport, clamp `pan` so an edge can't pull inside the viewport (allowed range derived below); when the image fits, returns `0.0` (centered).

- [ ] **Step 1: Write the failing tests**

Create `imgfind-gui/src/zoompan.rs`:

```rust
//! Pure zoom/pan math for the lightbox. No Slint types so it unit-tests cleanly;
//! `app.slint` mirrors the same clamp formula for live pan during a drag.

pub const ZOOM_MIN: f32 = 0.1;
pub const ZOOM_MAX: f32 = 8.0;
pub const ZOOM_STEP: f32 = 1.25;

// (implementations added in Step 3)

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "{a} !~= {b}");
    }

    #[test]
    fn zoom_clamps_to_range() {
        approx(clamp_zoom(0.001), ZOOM_MIN);
        approx(clamp_zoom(100.0), ZOOM_MAX);
        approx(clamp_zoom(1.0), 1.0);
    }

    #[test]
    fn zoom_in_out_step_and_clamp() {
        approx(zoom_in(1.0), 1.25);
        approx(zoom_out(1.0), 0.8);
        approx(zoom_in(ZOOM_MAX), ZOOM_MAX); // already at ceiling
        approx(zoom_out(ZOOM_MIN), ZOOM_MIN); // already at floor
    }

    #[test]
    fn wheel_zoom_sign_and_clamp() {
        assert!(wheel_zoom(1.0, 60.0) > 1.0, "positive delta zooms in");
        assert!(wheel_zoom(1.0, -60.0) < 1.0, "negative delta zooms out");
        approx(wheel_zoom(1.0, 0.0), 1.0);
        approx(wheel_zoom(ZOOM_MAX, 600.0), ZOOM_MAX); // clamped
    }

    #[test]
    fn pan_centers_when_image_fits() {
        approx(clamp_pan(100.0, 200.0, 50.0), 0.0);
        approx(clamp_pan(200.0, 200.0, 50.0), 0.0);
    }

    #[test]
    fn pan_clamps_to_image_edges() {
        // image 400, viewport 200 → half-overhang 100 → pan ∈ [-100, 100].
        approx(clamp_pan(400.0, 200.0, 0.0), 0.0);
        approx(clamp_pan(400.0, 200.0, 999.0), 100.0);
        approx(clamp_pan(400.0, 200.0, -999.0), -100.0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui zoompan 2>&1 | tail -20`
Expected: FAIL — functions not defined (and `mod zoompan;` missing).

- [ ] **Step 3: Implement the helpers**

Add the function bodies above the `#[cfg(test)]` block in `imgfind-gui/src/zoompan.rs`:

```rust
pub fn clamp_zoom(z: f32) -> f32 {
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

pub fn zoom_in(z: f32) -> f32 {
    clamp_zoom(z * ZOOM_STEP)
}

pub fn zoom_out(z: f32) -> f32 {
    clamp_zoom(z / ZOOM_STEP)
}

/// Wheel/trackpad zoom: ~60px per notch, 1.1 base for smooth perceived steps.
pub fn wheel_zoom(z: f32, delta_px: f32) -> f32 {
    clamp_zoom(z * 1.1f32.powf(delta_px / 60.0))
}

/// Clamp a pan offset so a too-large image can't be dragged off the viewport.
/// When the image fits within the viewport it is centered (pan = 0).
pub fn clamp_pan(image_len: f32, viewport_len: f32, pan: f32) -> f32 {
    let overhang = (image_len - viewport_len) / 2.0;
    if overhang <= 0.0 {
        0.0
    } else {
        pan.clamp(-overhang, overhang)
    }
}
```

Add to `imgfind-gui/src/main.rs` near the other module declarations:

```rust
mod zoompan;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui zoompan 2>&1 | tail -20`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p imgfind-gui --all-targets 2>&1 | tail -5
git add imgfind-gui/src/zoompan.rs imgfind-gui/src/main.rs
git commit -m "feat(gui): pure lightbox zoom/pan math helpers"
```

---

### Task 5: Slint lightbox markup — two-image body, chrome, scroll/drag/keyboard

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (lightbox block lines ~851–923; add `MainWindow` properties/callbacks near the other lightbox declarations; extend the `app-keys` `capture-key-pressed` lightbox branch ~lines 239–261)

**Interfaces:**
- Consumes (set by Task 6): `lightbox-image`, `lightbox-open`, and the new props below.
- Produces (new `MainWindow` surface used by Task 6/7):
  - in props: `lightbox-filename: string`, `lightbox-index1: int`, `lightbox-total: int`, `lightbox-zoom: float` (default 1.0), `lightbox-fit: bool` (default true), `lightbox-fit-scale: float`
  - callbacks: `lightbox-zoom-changed(float)`, `lightbox-zoom-fit()` (existing `lightbox-prev/next/close` reused)

This task is verified by a clean `cargo build` (Slint codegen) since the markup has no unit harness. Unhandled new callbacks are legal in Slint (they no-op until Task 6 wires them).

- [ ] **Step 1: Add properties and callbacks**

In the `MainWindow` declaration in `imgfind-gui/ui/app.slint`, near the existing `lightbox-*` properties/callbacks, add:

```slint
in property <string> lightbox-filename;
in property <int> lightbox-index1;
in property <int> lightbox-total;
in property <float> lightbox-zoom: 1.0;
in property <bool> lightbox-fit: true;
in property <float> lightbox-fit-scale: 1.0;
callback lightbox-zoom-changed(float);
callback lightbox-zoom-fit();
```

- [ ] **Step 2: Replace the lightbox body with two-image + chrome**

Replace the lightbox block (lines ~855–923, the `if root.lightbox-open: Rectangle { ... }`) with:

```slint
if root.lightbox-open: Rectangle {
    width: root.width;
    height: root.height;

    // Local pan offset (px), reset on navigation / entering fit.
    property <length> pan-x: 0px;
    property <length> pan-y: 0px;

    Rectangle {
        background: #000000ee;
        width: 100%;
        height: 100%;
    }

    // Content area between the 36px top bar and 80px bottom bar.
    content := Rectangle {
        x: 0;
        y: 36px;
        width: parent.width;
        height: parent.height - 36px - 80px;
        clip: true;

        // Fit view (default): centered, image-fit contain. Click closes.
        if root.lightbox-fit: Image {
            source: root.lightbox-image;
            image-fit: contain;
            width: parent.width;
            height: parent.height;
        }

        // Zoomed view: explicit size = source px * zoom, pan clamped so an edge
        // can't pull inside the viewport (mirrors zoompan::clamp_pan).
        if !root.lightbox-fit: Image {
            source: root.lightbox-image;
            width: self.source.width * 1px * root.lightbox-zoom;
            height: self.source.height * 1px * root.lightbox-zoom;
            x: self.width > parent.width
                ? clamp((parent.width - self.width) / 2 + content-root.pan-x,
                        parent.width - self.width, 0px)
                : (parent.width - self.width) / 2;
            y: self.height > parent.height
                ? clamp((parent.height - self.height) / 2 + content-root.pan-y,
                        parent.height - self.height, 0px)
                : (parent.height - self.height) / 2;
        }

        // Wheel zoom + click-drag pan. Below the nav arrows in z-order.
        drag-touch := TouchArea {
            width: 100%;
            height: 100%;
            mouse-cursor: root.lightbox-fit
                ? MouseCursor.default
                : (self.pressed ? MouseCursor.grabbing : MouseCursor.grab);

            property <length> start-pan-x;
            property <length> start-pan-y;

            scroll-event(e) => {
                // Anchor at fit-scale on the first tick out of fit so size
                // doesn't jump, then apply the wheel factor.
                if (root.lightbox-fit) {
                    root.lightbox-zoom-changed(root.lightbox-fit-scale);
                }
                root.lightbox-zoom-changed(
                    clamp(root.lightbox-zoom * pow(1.1, e.delta-y / 60px), 0.1, 8.0));
                return accept;
            }
            changed pressed => {
                if (self.pressed) {
                    self.start-pan-x = content-root.pan-x;
                    self.start-pan-y = content-root.pan-y;
                }
            }
            moved => {
                if (!root.lightbox-fit) {
                    content-root.pan-x = self.start-pan-x + (self.mouse-x - self.pressed-x);
                    content-root.pan-y = self.start-pan-y + (self.mouse-y - self.pressed-y);
                }
            }
            clicked => {
                if (root.lightbox-fit) { root.lightbox-close(); }
            }
        }

        // Prev / next arrows pinned to the content edges.
        prev-touch := TouchArea {
            width: 60px;
            height: 100%;
            x: 0;
            clicked => { root.lightbox-prev(); }
            Rectangle {
                background: prev-touch.has-hover ? #00000080 : transparent;
                Text {
                    text: "<";
                    color: white;
                    font-size: 28px;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
            }
        }
        next-touch := TouchArea {
            width: 60px;
            height: 100%;
            x: parent.width - self.width;
            clicked => { root.lightbox-next(); }
            Rectangle {
                background: next-touch.has-hover ? #00000080 : transparent;
                Text {
                    text: ">";
                    color: white;
                    font-size: 28px;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
            }
        }
    }

    // Reset pan on navigation and when (re-)entering fit.
    changed lightbox-index1 => {
        content-root.pan-x = 0px;
        content-root.pan-y = 0px;
    }
    changed lightbox-fit => {
        if (root.lightbox-fit) {
            content-root.pan-x = 0px;
            content-root.pan-y = 0px;
        }
    }

    // Top bar: filename · counter · close.
    Rectangle {
        x: 0; y: 0; width: parent.width; height: 36px;
        background: #000000cc;
        HorizontalLayout {
            padding-left: 12px;
            padding-right: 12px;
            spacing: 12px;
            Text {
                text: root.lightbox-filename;
                color: white;
                vertical-alignment: center;
                overflow: elide;
                horizontal-stretch: 1;
            }
            Text {
                text: root.lightbox-index1 + " of " + root.lightbox-total;
                color: #cccccc;
                vertical-alignment: center;
            }
            close-touch := TouchArea {
                width: 28px;
                clicked => { root.lightbox-close(); }
                Text {
                    text: "×";
                    color: close-touch.has-hover ? white : #cccccc;
                    font-size: 22px;
                    horizontal-alignment: center;
                    vertical-alignment: center;
                }
            }
        }
    }

    // Bottom bar: Fit button · zoom slider · percentage.
    Rectangle {
        x: 0; y: parent.height - 80px; width: parent.width; height: 80px;
        background: #000000cc;
        HorizontalLayout {
            padding: 16px;
            spacing: 16px;
            alignment: center;
            fit-touch := TouchArea {
                width: 60px;
                clicked => { root.lightbox-zoom-fit(); }
                Rectangle {
                    background: fit-touch.has-hover ? #ffffff22 : #ffffff11;
                    border-radius: 4px;
                    Text {
                        text: "Fit";
                        color: white;
                        horizontal-alignment: center;
                        vertical-alignment: center;
                    }
                }
            }
            zoom-slider := Slider {
                minimum: 0.1;
                maximum: 8.0;
                value: root.lightbox-zoom;
                width: 240px;
                changed(v) => { root.lightbox-zoom-changed(v); }
            }
            Text {
                text: round((root.lightbox-fit ? root.lightbox-fit-scale : root.lightbox-zoom) * 100) + "%";
                color: white;
                vertical-alignment: center;
                width: 56px;
            }
        }
    }
}
```

> Naming note: give the outer lightbox Rectangle the id `content-root := Rectangle` (replace `if root.lightbox-open: Rectangle {` with `if root.lightbox-open: content-root := Rectangle {`) so the `pan-x`/`pan-y` references resolve. Confirm `Slider`'s `changed(v)` callback name against the project's Slint version; if it differs, use the version's value-changed callback (e.g. `changed => root.lightbox-zoom-changed(self.value)`).

- [ ] **Step 3: Extend the keyboard branch**

In the `app-keys` `capture-key-pressed` lightbox branch (~lines 239–261), add before it returns, alongside the existing h/l/arrow/Esc handling:

```slint
if (root.lightbox-open && event.text == "0") {
    root.lightbox-zoom-fit();
    return accept;
}
if (root.lightbox-open && (event.text == "+" || event.text == "=")) {
    root.lightbox-zoom-changed(min(root.lightbox-zoom * 1.25, 8.0));
    return accept;
}
if (root.lightbox-open && event.text == "-") {
    root.lightbox-zoom-changed(max(root.lightbox-zoom / 1.25, 0.1));
    return accept;
}
```

- [ ] **Step 4: Build to verify Slint codegen + layout compile**

Run: `cargo build -p imgfind-gui 2>&1 | tail -20`
Expected: builds clean (warnings about unused generated setters are fine until Task 6).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add imgfind-gui/ui/app.slint
git commit -m "feat(gui): lightbox zoom/pan markup + chrome bars"
```

---

### Task 6: Wire Rust callbacks, zoom/fit state, and per-image chrome

**Files:**
- Modify: `imgfind-gui/src/main.rs` (lightbox state near lines ~355–356; `on_lightbox_*` callbacks ~951–1072; open/load path ~3443–3483; `Backend::thumbnail` call sites)
- Modify: `imgfind-gui/src/backend.rs:149` (`thumbnail` accepts `ThumbnailSpec`)

**Interfaces:**
- Consumes: `zoompan::{clamp_zoom, zoom_in, zoom_out}`, the Slint props/callbacks from Task 5, `ThumbnailSpec` (Task 1).
- Produces:
  - `Backend::thumbnail(&self, rel_path: &str, spec: impl Into<ThumbnailSpec>) -> Result<Vec<u8>>`
  - lightbox zoom/fit state `lb_zoom: Arc<Mutex<f32>>`, `lb_fit: Arc<Mutex<bool>>`
  - handlers `on_lightbox_zoom_changed`, `on_lightbox_zoom_fit`; reset-on-open/nav; per-image `lightbox-filename`/`-index1`/`-total`/`-fit-scale` population
  - a shared helper `apply_lightbox_view(&MainWindow, zoom: f32, fit: bool)` that writes `set_lightbox_zoom`/`set_lightbox_fit`

- [ ] **Step 1: Widen `Backend::thumbnail`**

`imgfind-gui/src/backend.rs:149`:

```rust
pub fn thumbnail(&self, rel_path: &str, spec: impl Into<ThumbnailSpec>) -> Result<Vec<u8>> {
    let hash = imgfind::block_on(self.db.get_image_hash(&Self::rel(rel_path)))
        .with_context(|| format!("No hash for {rel_path}"))?;
    let abs = self.abs_path(rel_path);
    let abs_str = abs.to_string_lossy();
    get_or_generate_thumbnail(&self.db, &abs_str, &hash, spec)
        .with_context(|| format!("Failed to load thumbnail for {rel_path}"))
}
```

Add `ThumbnailSpec` to the `imgfind::` import at `imgfind-gui/src/backend.rs:19`:

```rust
    AbsolutePath, FileSize, RelativePath, ThumbnailSize, ThumbnailSpec, get_db_path,
    relative_to_abs_path,
```

(All existing callers pass `ThumbnailSize` and compile via `From`.)

- [ ] **Step 2: Add zoom/fit state**

Near `lb_index`/`lb_generation` (~line 355) in `imgfind-gui/src/main.rs`:

```rust
// Lightbox view state (GUI-runtime only; never persisted to ui_state).
let lb_zoom: Arc<Mutex<f32>> = Arc::new(Mutex::new(1.0));
let lb_fit: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
```

Add a small helper (free fn near the other lightbox helpers, ~line 3408):

```rust
/// Push zoom/fit to the UI in one place so every entry point stays consistent.
fn apply_lightbox_view(w: &MainWindow, zoom: f32, fit: bool) {
    w.set_lightbox_zoom(zoom);
    w.set_lightbox_fit(fit);
}
```

- [ ] **Step 3: Reset view + populate chrome on open and navigation**

In `load_lightbox_image` (~3443), after `w.set_lightbox_open(true)` set the per-image chrome and computed fit-scale. Extend the function to take the row index + total + dimensions, OR (simpler, less churn) set chrome from the callbacks that already know the index. Implement in the callbacks:

In `on_lightbox_prev` / `on_lightbox_next` / the open path, after computing `new_idx` and before/after `load_lightbox_image(...)`, add:

```rust
// Reset zoom/fit on every open and navigation (utmost parity).
*lb_zoom_ref.lock() = 1.0;
*lb_fit_ref.lock() = true;
if let Some(w) = weak.upgrade() {
    apply_lightbox_view(&w, 1.0, true);
    // Per-image chrome.
    let results = state_ref.lock();
    let total = results.results().len() as i32;
    if let Some(row) = results.results().get(new_idx) {
        let fname = std::path::Path::new(&row.path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| row.path.clone());
        w.set_lightbox_filename(fname.into());
    }
    w.set_lightbox_index1((new_idx as i32) + 1);
    w.set_lightbox_total(total);
}
```

(Clone `lb_zoom`/`lb_fit` into each callback closure as `lb_zoom_ref`/`lb_fit_ref`, mirroring how `lb_ref`/`selected_ref` are already cloned.)

For `lightbox-fit-scale`: compute from the displayed thumbnail dimensions and the content viewport once the image is set. Since the content area is `window - chrome`, compute in `load_lightbox_image` after decoding (we have `img.width()/height()`):

```rust
// fit-scale = min(viewport_w / img_w, viewport_h / img_h), matching
// image-fit:contain, so the first wheel tick out of fit doesn't jump.
let (iw, ih) = (img.width() as f32, img.height() as f32);
let vw = w.get_width() as f32; // content ≈ window; minus chrome is close enough
let vh = (w.get_height() as f32) - 36.0 - 80.0;
let fit_scale = (vw / iw).min(vh / ih);
w.set_lightbox_fit_scale(fit_scale);
```

- [ ] **Step 4: Implement the zoom callbacks**

Add alongside the other `on_lightbox_*` registrations (~line 1072):

```rust
{
    let weak = window.as_weak();
    let lb_zoom_ref = Arc::clone(&lb_zoom);
    let lb_fit_ref = Arc::clone(&lb_fit);
    window.on_lightbox_zoom_changed(move |z| {
        let zc = zoompan::clamp_zoom(z);
        *lb_zoom_ref.lock() = zc;
        *lb_fit_ref.lock() = false; // any explicit zoom leaves fit
        if let Some(w) = weak.upgrade() {
            apply_lightbox_view(&w, zc, false);
        }
    });
}
{
    let weak = window.as_weak();
    let lb_zoom_ref = Arc::clone(&lb_zoom);
    let lb_fit_ref = Arc::clone(&lb_fit);
    window.on_lightbox_zoom_fit(move || {
        *lb_zoom_ref.lock() = 1.0;
        *lb_fit_ref.lock() = true;
        if let Some(w) = weak.upgrade() {
            apply_lightbox_view(&w, 1.0, true);
        }
    });
}
```

- [ ] **Step 5: Build + run existing tests**

Run: `cargo build -p imgfind-gui 2>&1 | tail -20 && cargo test -p imgfind-gui 2>&1 | tail -10`
Expected: builds clean; tests pass.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy -p imgfind-gui --all-targets 2>&1 | tail -5
git add imgfind-gui/src/main.rs imgfind-gui/src/backend.rs
git commit -m "feat(gui): wire lightbox zoom/fit state, callbacks, and chrome"
```

---

### Task 7: Progressive full-resolution hot-swap

**Files:**
- Modify: `imgfind-gui/src/main.rs` (full-res generation counter near `lb_generation`; trigger inside `on_lightbox_zoom_changed`; new `load_lightbox_fullres` helper near `load_lightbox_image`)

**Interfaces:**
- Consumes: `Backend::thumbnail(rel, ThumbnailSpec::FullSize)` (Task 6/3), the lightbox `lb_index`/`state` (current row path), the `lightbox-image` setter.
- Produces: `lb_fullres_generation: Arc<AtomicU64>`; `load_lightbox_fullres(weak, backend, rel, generation)` that swaps in full-res pixels **without** touching zoom/fit/pan.

- [ ] **Step 1: Add the full-res generation counter**

Near `lb_generation` (~line 356) in `imgfind-gui/src/main.rs`:

```rust
// Bumped on every navigation/open so a late full-res decode for a now-stale
// image is dropped (separate from lb_generation, which guards the base load).
let lb_fullres_generation: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
```

Bump it wherever `lb_generation` is bumped (open + each prev/next): add `lb_fullres_generation.fetch_add(1, Ordering::SeqCst);` next to the existing base-load dispatch in `on_lightbox_prev`/`on_lightbox_next`/open path. (Clone it into those closures as `lb_fullres_ref`.)

- [ ] **Step 2: Add the full-res loader**

Near `load_lightbox_image` (~3443):

```rust
/// Decode (cache-first) the full-resolution rendition off-thread and swap it in
/// WITHOUT disturbing the current zoom/fit/pan — only the pixels sharpen. Guarded
/// by `generation` so a decode that finishes after the user navigates is dropped.
fn load_lightbox_fullres(
    weak: Weak<MainWindow>,
    backend: Backend,
    rel_path: String,
    generation: Arc<AtomicU64>,
) {
    let my_gen = generation.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let bytes = match backend.thumbnail(&rel_path, imgfind::ThumbnailSpec::FullSize) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Lightbox full-res: {e:?}");
                return;
            }
        };
        let img = match image::load_from_memory(&bytes) {
            Ok(img) => img,
            Err(e) => {
                tracing::warn!("Lightbox full-res decode: {e:?}");
                return;
            }
        };
        slint::invoke_from_event_loop(move || {
            if my_gen != generation.load(Ordering::SeqCst) {
                return; // user navigated; drop stale full-res
            }
            let Some(w) = weak.upgrade() else { return };
            // Swap pixels only — leave zoom/fit/pan untouched.
            w.set_lightbox_image(image_util::dynamic_to_slint_image(&img));
        })
        .ok();
    });
}
```

- [ ] **Step 3: Trigger on first zoom past fit**

In `on_lightbox_zoom_changed` (Task 6 Step 4), after leaving fit, kick off the full-res load for the current image. Add inside that closure (it already has `weak`; also clone in `backend`, `state`, `lb_index`, and `lb_fullres_generation` as refs):

```rust
// On entering zoom, upgrade to full-res in the background (cache-first).
let rel = state_ref
    .lock()
    .results()
    .get(lb_ref.lock().unwrap_or(0))
    .map(|r| r.path.clone());
if let Some(rel) = rel {
    load_lightbox_fullres(
        weak.clone(),
        backend_ref.clone(),
        rel,
        Arc::clone(&lb_fullres_ref),
    );
}
```

> Idempotency: `Backend::thumbnail(FullSize)` is cache-first, so repeated zoom ticks that re-trigger this are cheap (a DB hit returning the same bytes); the generation guard prevents a stale swap. No extra "already loaded" flag needed.

- [ ] **Step 4: Build + manual smoke**

Run: `cargo build -p imgfind-gui 2>&1 | tail -20`
Expected: builds clean.

Manual (document result in the commit, not automated): `cargo run -p imgfind-gui -- --dir <a dir with an indexed DB>`, open a large image in the lightbox, scroll to zoom — image is instantly zoomable (2048px), sharpens shortly after; drag pans within bounds; `0`/Fit returns to fit; `+`/`-` step; arrows/h/l navigate and reset zoom.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p imgfind-gui --all-targets 2>&1 | tail -5
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): progressive full-res hot-swap on lightbox zoom"
```

---

### Task 8: Final review, docs, finish branch

**Files:**
- Modify: `CLAUDE.md` (lightbox description in the Native GUI section + the thumbnails/`ThumbnailSpec` note)

- [ ] **Step 1: Full workspace verification**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets 2>&1 | tail -10 && cargo test --workspace 2>&1 | tail -15`
Expected: fmt clean, no clippy warnings, all tests pass.

- [ ] **Step 2: Update `CLAUDE.md`**

In the Native GUI lightbox sentence, note that the lightbox now supports scroll-wheel zoom (10–800%), click-drag pan when zoomed, `+`/`-`/`0` keys and a Fit button + zoom slider, with progressive resolution: it shows the 2048px thumbnail instantly then hot-swaps a background-decoded full-native-resolution rendition without disturbing the zoom/pan window. In the `thumbnails` storage bullet, note that renditions are selected by the `ThumbnailSpec` enum (`ScaleSize` | `FullSize`); `FullSize` is the original resolution, persisted under `size = 0` (no migration). Add a pointer to `docs/superpowers/specs/2026-06-21-gui-lightbox-zoom-pan-design.md`.

- [ ] **Step 3: Commit docs**

```bash
git add CLAUDE.md
git commit -m "docs: lightbox zoom/pan + ThumbnailSpec"
```

- [ ] **Step 4: Finish the branch** via `superpowers:finishing-a-development-branch`.

---

## Self-Review

- **Spec coverage:** ThumbnailSpec enum (Task 1) ✓; DB seam (Task 2) ✓; FullSize decode path (Task 3) ✓; zoom/pan math (Task 4) ✓; Slint markup + chrome + scroll/drag/keyboard (Task 5) ✓; Rust wiring + reset + chrome population (Task 6) ✓; progressive hot-swap preserving zoom/pan (Task 7) ✓; no-wrap nav preserved (unchanged) ✓; not persisted to ui_state (Task 6 comment) ✓; docs (Task 8) ✓; tests for to_db_size/from_db_size + FullSize round-trip + FullSize generate + zoom/pan helpers (Tasks 1–4) ✓.
- **Placeholders:** none — every code step has concrete code; manual GUI smoke in Task 7 is explicitly labeled non-automated.
- **Type consistency:** `ThumbnailSpec`/`ThumbnailSize`, `to_db_size`/`from_db_size`, `clamp_zoom`/`zoom_in`/`zoom_out`/`wheel_zoom`/`clamp_pan`, `apply_lightbox_view`, `load_lightbox_fullres`, `lb_zoom`/`lb_fit`/`lb_fullres_generation`, and the Slint props/callbacks (`lightbox-zoom`/`-fit`/`-fit-scale`/`-filename`/`-index1`/`-total`, `lightbox-zoom-changed`/`-zoom-fit`) are used consistently across tasks.
- **Known verification points** flagged inline for the implementer: the `Slider` `changed` callback spelling and the `content-root` id wiring in Slint (Task 5); the exact closure-clone names for `state`/`backend`/`lb_index` in the GUI callbacks (Tasks 6–7) match the existing `lb_ref`/`selected_ref` pattern.
