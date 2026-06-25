# Lightbox Image Adjustments (Exposure) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an exposure adjustment editor to the GUI lightbox whose edits live only in the DB (never touching the original file) and are baked into all regenerated thumbnails.

**Architecture:** A pure `apply_adjustments(DynamicImage, &ImageEdits)` transform is applied at the thumbnail/decode generation seam; per-image edits are stored in a new `image_edits` table; the lightbox gains an edit mode with a live preview (re-applying edits to a freshly-decoded unedited base in memory) and an Accept flow that persists edits and regenerates every cached thumbnail size for the image.

**Tech Stack:** Rust (edition 2024), `image` 0.25, `turso` (async SQLite), Slint GUI, `anyhow`, `tracing`. Core crate `imgfind`; GUI crate `imgfind-gui`.

## Global Constraints

- Rust edition 2024; errors use `anyhow` with `Context`/`with_context`; logging via `tracing`.
- All `Database` methods are `async`; sync callers use `imgfind::block_on(...)`.
- Image paths in the DB are **relative to `Database.parent_dir`**; convert at boundaries (`RelativePath`/`AbsolutePath`, `abs_to_relative_path`/`relative_to_abs_path`).
- Exposure is EV stops in **[−3.0, +3.0]**, default 0.0; transform = per-RGB-channel `clamp(round(c * 2^EV), 0, 255)`, alpha untouched, sRGB space.
- **Identity edits (`exposure == 0`) must be a true no-op** in `apply_adjustments` (short-circuit, return input unchanged).
- Slint button/label text must use **ASCII / Latin-1 only** (default font tofus symbol glyphs).
- Migration runner is idempotent (`CREATE TABLE IF NOT EXISTS`); stamp `LATEST_MIGRATION_VERSION` only after all migrations succeed.
- Original image files are **never** modified.
- Rust code must be clippy-clean and rustfmt-clean (dispatch Rust coding to the `rust-developer` agent).
- Run a single test module with e.g. `cargo test -p imgfind edits::` ; whole workspace with `cargo test --workspace`.

---

### Task 1: `ImageEdits` type and `apply_adjustments` transform

**Files:**
- Create: `src/edits.rs`
- Modify: `src/lib.rs` (add `pub mod edits;`)
- Test: inline `#[cfg(test)]` module in `src/edits.rs`

**Interfaces:**
- Consumes: `image::DynamicImage` (from the `image` crate, already a dependency).
- Produces:
  - `pub struct ImageEdits { pub exposure: f32 }`
  - `impl ImageEdits`: `pub const EXPOSURE_MIN: f32 = -3.0;`, `pub const EXPOSURE_MAX: f32 = 3.0;`, `pub fn identity() -> Self`, `pub fn is_identity(&self) -> bool`, `pub fn clamped(self) -> Self`
  - `pub fn apply_adjustments(img: image::DynamicImage, edits: &ImageEdits) -> image::DynamicImage`
  - `ImageEdits` derives `Debug, Clone, Copy, PartialEq`.

- [ ] **Step 1: Write the failing tests**

Add to `src/edits.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgba, RgbaImage};

    fn solid(w: u32, h: u32, px: [u8; 4]) -> DynamicImage {
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba(px)))
    }

    #[test]
    fn identity_is_noop() {
        let img = solid(2, 2, [100, 110, 120, 255]);
        let out = apply_adjustments(img.clone(), &ImageEdits::identity());
        assert_eq!(img.to_rgba8(), out.to_rgba8());
    }

    #[test]
    fn is_identity_detects_zero() {
        assert!(ImageEdits::identity().is_identity());
        assert!(!ImageEdits { exposure: 0.5 }.is_identity());
    }

    #[test]
    fn plus_one_ev_doubles_midtone() {
        // 2^1 = 2, so 100 -> 200, alpha unchanged.
        let out = apply_adjustments(solid(1, 1, [100, 50, 25, 200]), &ImageEdits { exposure: 1.0 });
        assert_eq!(out.to_rgba8().get_pixel(0, 0), &image::Rgba([200, 100, 50, 200]));
    }

    #[test]
    fn plus_ev_clamps_to_255() {
        let out = apply_adjustments(solid(1, 1, [200, 200, 200, 255]), &ImageEdits { exposure: 2.0 });
        assert_eq!(out.to_rgba8().get_pixel(0, 0), &image::Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn minus_one_ev_halves_midtone() {
        let out = apply_adjustments(solid(1, 1, [100, 80, 40, 255]), &ImageEdits { exposure: -1.0 });
        assert_eq!(out.to_rgba8().get_pixel(0, 0), &image::Rgba([50, 40, 20, 255]));
    }

    #[test]
    fn clamped_bounds_exposure() {
        assert_eq!(ImageEdits { exposure: 9.0 }.clamped().exposure, ImageEdits::EXPOSURE_MAX);
        assert_eq!(ImageEdits { exposure: -9.0 }.clamped().exposure, ImageEdits::EXPOSURE_MIN);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind edits::`
Expected: FAIL — `apply_adjustments`/`ImageEdits` not found (module not yet declared).

- [ ] **Step 3: Implement `src/edits.rs`**

```rust
//! Per-image, non-destructive adjustments (currently exposure in EV stops).
//!
//! Edits live only in the DB and are baked into thumbnails at the generation
//! seam; the original file is never modified. See
//! `docs/superpowers/specs/2026-06-24-lightbox-image-adjustments-design.md`.

use image::DynamicImage;

/// Adjustments applied to a single image. Identity (`exposure == 0`) is a no-op.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageEdits {
    /// Exposure in photographic EV stops; output channel = input * 2^exposure.
    pub exposure: f32,
}

impl ImageEdits {
    pub const EXPOSURE_MIN: f32 = -3.0;
    pub const EXPOSURE_MAX: f32 = 3.0;

    pub fn identity() -> Self {
        Self { exposure: 0.0 }
    }

    /// True when no adjustment would alter the image.
    pub fn is_identity(&self) -> bool {
        self.exposure.abs() < f32::EPSILON
    }

    /// Clamp exposure into the supported range.
    pub fn clamped(self) -> Self {
        Self {
            exposure: self.exposure.clamp(Self::EXPOSURE_MIN, Self::EXPOSURE_MAX),
        }
    }
}

impl Default for ImageEdits {
    fn default() -> Self {
        Self::identity()
    }
}

/// Apply `edits` to `img`, returning the adjusted image.
///
/// Pure and deterministic. Identity edits return `img` unchanged (no copy).
pub fn apply_adjustments(img: DynamicImage, edits: &ImageEdits) -> DynamicImage {
    let edits = edits.clamped();
    if edits.is_identity() {
        return img;
    }
    let factor = 2f32.powf(edits.exposure);
    let mut buf = img.to_rgba8();
    for px in buf.pixels_mut() {
        for c in 0..3 {
            px.0[c] = (px.0[c] as f32 * factor).round().clamp(0.0, 255.0) as u8;
        }
    }
    DynamicImage::ImageRgba8(buf)
}
```

Add `pub mod edits;` to `src/lib.rs` next to the other module declarations.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind edits::`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add src/edits.rs src/lib.rs
git commit -m "feat(edits): ImageEdits type and apply_adjustments exposure transform"
```

---

### Task 2: Migration 004 — `image_edits` table

**Files:**
- Modify: `src/schema.rs` (add `migration_004_image_edits`, call it in `run_migrations`, bump `LATEST_MIGRATION_VERSION` to 4)
- Test: inline `#[cfg(test)]` in `src/schema.rs` (follow the existing migration test pattern if present) OR a focused test in `src/database.rs` Task 3. If `src/schema.rs` has no test harness, the table is exercised by Task 3's DB tests — in that case this task's verification is "migrations run and `LATEST_MIGRATION_VERSION == 4`".

**Interfaces:**
- Consumes: existing `run_migrations(conn: &turso::Connection)` and the `images` table (`images.id`).
- Produces: an `image_edits(image_id PK → images.id ON DELETE CASCADE, exposure REAL NOT NULL DEFAULT 0.0, updated_at DATETIME)` table available after migrations; `LATEST_MIGRATION_VERSION == 4`.

- [ ] **Step 1: Inspect the existing migration pattern**

Read `src/schema.rs` around `run_migrations` and `migration_003_*`. Mirror the exact style (async fn, `conn.execute(...).await?`, the `if current < N { ... }` block ordering, and the version stamp at the end).

- [ ] **Step 2: Add the migration function**

```rust
async fn migration_004_image_edits(conn: &turso::Connection) -> anyhow::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS image_edits (
            image_id   INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
            exposure   REAL NOT NULL DEFAULT 0.0,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
        (),
    )
    .await?;
    Ok(())
}
```

- [ ] **Step 3: Wire it into `run_migrations` and bump the version**

In `run_migrations`, after the `if current < 3 { migration_003_*(conn).await? }` block, add:

```rust
    if current < 4 {
        migration_004_image_edits(conn).await?;
    }
```

Change `pub const LATEST_MIGRATION_VERSION: i32 = 3;` to `= 4;`.

- [ ] **Step 4: Verify it builds and existing schema tests pass**

Run: `cargo test -p imgfind schema::`
Expected: PASS (existing migration tests still green; version is now 4). If there are no `schema::` tests, run `cargo build -p imgfind` and rely on Task 3 to exercise the table.

- [ ] **Step 5: Commit**

```bash
git add src/schema.rs
git commit -m "feat(schema): migration 004 adds image_edits table"
```

---

### Task 3: `Database` accessors for edits and thumbnail sizes

**Files:**
- Modify: `src/database.rs` (add three async methods)
- Test: inline `#[cfg(test)]` in `src/database.rs` (follow the existing DB test pattern — look for how other tests build a temp `Database`)

**Interfaces:**
- Consumes: `ImageEdits` from `crate::edits` (Task 1); migration 004 table (Task 2); existing helpers `col_i64`, `col_text`, the pool, `RelativePath`, and the path→id resolution used by the tag methods (`tag_image`).
- Produces:
  - `pub async fn get_image_edits(&self, path: &RelativePath) -> anyhow::Result<ImageEdits>` — returns `ImageEdits::identity()` when no row exists.
  - `pub async fn set_image_edits(&self, path: &RelativePath, edits: &ImageEdits) -> anyhow::Result<()>` — upsert keyed on `image_id`.
  - `pub async fn get_thumbnail_sizes(&self, image_hash: &str) -> anyhow::Result<Vec<u32>>` — distinct `size` values present in `thumbnails` for the hash.

- [ ] **Step 1: Write the failing tests**

Mirror the existing DB-test setup in `src/database.rs` (temp dir + `Database` + an inserted image). Add:

```rust
#[tokio::test]
async fn image_edits_upsert_and_read() {
    let (db, rel) = test_db_with_one_image().await; // reuse/define per existing pattern
    // Absent => identity
    assert!(db.get_image_edits(&rel).await.unwrap().is_identity());
    // Insert
    db.set_image_edits(&rel, &ImageEdits { exposure: 1.5 }).await.unwrap();
    assert_eq!(db.get_image_edits(&rel).await.unwrap().exposure, 1.5);
    // Update same row (no duplicate)
    db.set_image_edits(&rel, &ImageEdits { exposure: -0.75 }).await.unwrap();
    assert_eq!(db.get_image_edits(&rel).await.unwrap().exposure, -0.75);
}

#[tokio::test]
async fn thumbnail_sizes_lists_distinct() {
    let (db, _rel, hash) = test_db_with_one_image_hash().await;
    db.insert_thumbnail(&hash, imgfind::thumbnail::ThumbnailSize(300), &[1, 2, 3]).await.unwrap();
    db.insert_thumbnail(&hash, imgfind::thumbnail::ThumbnailSize(512), &[4, 5, 6]).await.unwrap();
    let mut sizes = db.get_thumbnail_sizes(&hash).await.unwrap();
    sizes.sort_unstable();
    assert_eq!(sizes, vec![300, 512]);
}
```

> Implementer note: adapt the test fixtures to the existing helpers in `src/database.rs` tests. If a one-image fixture helper does not exist, create a small local one. Use the crate-internal paths (`crate::...`) rather than `imgfind::...` inside the crate if that matches the file's existing test style.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind database::`
Expected: FAIL — `get_image_edits`/`set_image_edits`/`get_thumbnail_sizes` not found.

- [ ] **Step 3: Implement the methods**

Follow the path→id resolution used by `tag_image`/`untag_image`. Sketch:

```rust
pub async fn get_image_edits(&self, path: &RelativePath) -> anyhow::Result<ImageEdits> {
    let conn = self.pool.get().await.context("get connection")?;
    let mut rows = conn
        .query(
            "SELECT e.exposure FROM image_edits e
             JOIN images i ON i.id = e.image_id
             WHERE i.path = ?1",
            (path.as_str().into_owned(),),
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(ImageEdits { exposure: col_f64(&row, 0, "exposure")? as f32 }.clamped()),
        None => Ok(ImageEdits::identity()),
    }
}

pub async fn set_image_edits(&self, path: &RelativePath, edits: &ImageEdits) -> anyhow::Result<()> {
    let edits = edits.clamped();
    let conn = self.pool.get().await.context("get connection")?;
    conn.execute(
        "INSERT INTO image_edits (image_id, exposure, updated_at)
         SELECT i.id, ?2, CURRENT_TIMESTAMP FROM images i WHERE i.path = ?1
         ON CONFLICT(image_id) DO UPDATE SET exposure = excluded.exposure, updated_at = CURRENT_TIMESTAMP",
        (path.as_str().into_owned(), edits.exposure as f64),
    )
    .await?;
    Ok(())
}

pub async fn get_thumbnail_sizes(&self, image_hash: &str) -> anyhow::Result<Vec<u32>> {
    let conn = self.pool.get().await.context("get connection")?;
    let mut rows = conn
        .query(
            "SELECT DISTINCT size FROM thumbnails WHERE image_hash = ?1",
            (image_hash.to_string(),),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(col_i64(&row, 0, "size")? as u32);
    }
    Ok(out)
}
```

> If a `col_f64` helper does not exist, use the existing `col_opt_f64` and `.context(...)?`, or add a small `col_f64` mirroring `col_i64`. Verify the turso parameter-binding style against neighboring methods (some use `Value::...`); match what compiles in this file.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind database::`
Expected: PASS (new tests green, existing DB tests unaffected).

- [ ] **Step 5: Commit**

```bash
git add src/database.rs
git commit -m "feat(db): get/set image edits and list thumbnail sizes"
```

---

### Task 4: Bake edits into thumbnail generation

**Files:**
- Modify: `src/thumbnail.rs` (`generate_thumbnail_bytes`, `get_or_generate_thumbnail`, `generate_missing_thumbnails_batch`)
- Test: inline `#[cfg(test)]` in `src/thumbnail.rs`

**Interfaces:**
- Consumes: `apply_adjustments` + `ImageEdits` (Task 1); `Database::get_image_edits` (Task 3); existing `decode_image`/`decode_full_image`, `ThumbnailSpec`.
- Produces:
  - `generate_thumbnail_bytes(filepath: &str, spec: ThumbnailSpec, edits: &ImageEdits) -> Result<Vec<u8>>` (new `edits` param; apply after decode, before resize/encode).
  - `get_or_generate_thumbnail` unchanged signature, but on cache-miss it fetches edits via `db.get_image_edits(...)` (convert `filepath` → `RelativePath` using the DB's `parent_dir`) and passes them to generation.
  - `generate_missing_thumbnails_batch` fetches edits per image before generating.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn edited_thumbnail_differs_from_unedited() {
    // Build a small temp image file, generate at 64px with identity vs +2 EV.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.png");
    image::RgbImage::from_pixel(80, 80, image::Rgb([100, 100, 100]))
        .save(&path)
        .unwrap();
    let p = path.to_str().unwrap();
    let plain = generate_thumbnail_bytes(p, ThumbnailSpec::ScaleSize(ThumbnailSize(64)), &ImageEdits::identity()).unwrap();
    let bright = generate_thumbnail_bytes(p, ThumbnailSpec::ScaleSize(ThumbnailSize(64)), &ImageEdits { exposure: 2.0 }).unwrap();
    assert_ne!(plain, bright, "exposure edit must change generated thumbnail bytes");

    // And identity equals a second identity render (determinism + true no-op).
    let plain2 = generate_thumbnail_bytes(p, ThumbnailSpec::ScaleSize(ThumbnailSize(64)), &ImageEdits::identity()).unwrap();
    assert_eq!(plain, plain2);
}
```

> Ensure `tempfile` is available as a dev-dependency (it is used by existing tests — confirm in `Cargo.toml`; if not, add it under `[dev-dependencies]`).

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p imgfind thumbnail::`
Expected: FAIL — `generate_thumbnail_bytes` arity mismatch (no `edits` param yet).

- [ ] **Step 3: Thread `edits` through generation**

In `generate_thumbnail_bytes`, after decoding (`decode_image` for `ScaleSize`, `decode_full_image` for `FullSize`) and **before** resize/encode:

```rust
let img = crate::edits::apply_adjustments(img, edits);
```

Update the signature to take `edits: &ImageEdits`. In `get_or_generate_thumbnail`, on cache miss:

```rust
let rel = crate::abs_to_relative_path(/* filepath */, &db.parent_dir)?; // match existing conversion helper
let edits = imgfind::block_on(db.get_image_edits(&rel))?;               // or .await if already async context
let bytes = generate_thumbnail_bytes(filepath, spec, &edits)?;
```

> Use whichever path-conversion helper the file already imports; `filepath` here is the absolute path string the callers pass. In `generate_missing_thumbnails_batch`, fetch edits per `(path, hash)` before the rayon generation (identity is the cheap common case — `apply_adjustments` short-circuits). Keep the DB read off the rayon workers (fetch on the coordinating thread, move the `ImageEdits` `Copy` value into the worker).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind thumbnail::`
Expected: PASS. Then `cargo test --workspace` to catch any call-site arity breaks (fix them by passing `&ImageEdits::identity()` only where a caller genuinely has no DB/edits context; prefer routing through `get_or_generate_thumbnail`).

- [ ] **Step 5: Commit**

```bash
git add src/thumbnail.rs Cargo.toml
git commit -m "feat(thumbnail): bake image edits into generated thumbnails"
```

---

### Task 5: Accept flow — persist edits + regenerate cached thumbnails

**Files:**
- Modify: `src/thumbnail.rs` OR `src/lib.rs` — add a reusable `regenerate_thumbnails_for_image(db, abs_path, hash, edits)` helper (place it next to thumbnail code).
- Test: inline `#[cfg(test)]` in the file the helper lives in.

**Interfaces:**
- Consumes: `Database::get_thumbnail_sizes`, `insert_thumbnail` (Task 3); `generate_thumbnail_bytes` (Task 4); `ThumbnailSpec`/`ThumbnailSize`; `to_db_size`/`from_db_size`.
- Produces:
  - `pub fn regenerate_thumbnails_for_image(db: &Database, abs_path: &str, hash: &str, edits: &ImageEdits) -> anyhow::Result<usize>` — for each size currently cached for `hash`, regenerate with `edits` baked and overwrite via `insert_thumbnail`; returns the count regenerated. Size `0` maps to `ThumbnailSpec::FullSize`, others to `ScaleSize`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn regenerate_overwrites_existing_sizes_with_edits() {
    // temp Database + one image file + cached identity thumbnails at 64 and 0 (FullSize).
    // After regenerate with +2 EV, the stored bytes for each size must change.
    // (Use block_on for the async db calls, mirroring existing sync tests.)
    // ... build db, insert image row + hash, seed identity thumbnails ...
    let before_64 = imgfind::block_on(db.get_thumbnail(&hash, ThumbnailSize(64))).unwrap();
    let n = regenerate_thumbnails_for_image(&db, abs, &hash, &ImageEdits { exposure: 2.0 }).unwrap();
    assert!(n >= 1);
    let after_64 = imgfind::block_on(db.get_thumbnail(&hash, ThumbnailSize(64))).unwrap();
    assert_ne!(before_64, after_64);
}
```

> Implementer: adapt fixtures to existing sync test patterns in the crate. Seed thumbnails with `block_on(db.insert_thumbnail(...))`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p imgfind thumbnail::regenerate`
Expected: FAIL — function not found.

- [ ] **Step 3: Implement the helper**

```rust
pub fn regenerate_thumbnails_for_image(
    db: &Database,
    abs_path: &str,
    hash: &str,
    edits: &ImageEdits,
) -> anyhow::Result<usize> {
    let sizes = crate::block_on(db.get_thumbnail_sizes(hash))?;
    let mut count = 0;
    for size in sizes {
        let spec = if size == 0 {
            ThumbnailSpec::FullSize
        } else {
            ThumbnailSpec::ScaleSize(ThumbnailSize(size))
        };
        let bytes = generate_thumbnail_bytes(abs_path, spec, edits)?;
        crate::block_on(db.insert_thumbnail(hash, spec, &bytes))?;
        count += 1;
    }
    Ok(count)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind thumbnail::` then `cargo test --workspace`.
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/thumbnail.rs
git commit -m "feat(thumbnail): regenerate all cached sizes for an edited image"
```

---

### Task 6: Lightbox edit-mode UI (Slint)

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (lightbox component: properties, callbacks, Edit button, sidebar)
- Test: none (markup). Verified by `cargo build -p imgfind-gui` (Slint compiles markup at build time) and Task 8 manual run.

**Interfaces:**
- Consumes: existing lightbox component, `lightbox-image`, chrome bars.
- Produces (new `MainWindow` members the Rust side in Task 7 will bind):
  - `in-out property <bool> edit-mode;`
  - `in-out property <float> edit-exposure;`
  - `callback edit-toggle();`
  - `callback edit-exposure-changed(float);`
  - `callback edit-reset();`
  - `callback edit-accept();`

- [ ] **Step 1: Add properties and callbacks**

In the `MainWindow` declaration block alongside the existing `lightbox-*` properties/callbacks, add the six members above.

- [ ] **Step 2: Add the "Edit" toggle button to the lightbox bottom bar**

Next to the Fit / 1:1 buttons, add a button whose text is `"Edit"` (ASCII only). Its `clicked` calls `root.edit-toggle()`. Give it a pressed/active look when `root.edit-mode` is true (e.g. background highlight).

- [ ] **Step 3: Add the right-side adjustments sidebar (shown only in edit mode)**

```slint
if root.lightbox-open && root.edit-mode : Rectangle {
    x: parent.width - self.width;
    width: 240px;
    height: parent.height;
    background: #1e1e1eee;
    VerticalLayout {
        padding: 16px; spacing: 12px;
        Text { text: "Adjustments"; color: white; font-size: 16px; }
        Text { text: "Exposure: " + round(root.edit-exposure * 100) / 100 + " EV"; color: #ccc; }
        Slider {
            minimum: -3.0; maximum: 3.0; value: root.edit-exposure;
            changed(v) => { root.edit-exposure-changed(v); }
        }
        HorizontalLayout {
            spacing: 8px;
            Button { text: "Reset"; clicked => { root.edit-reset(); } }
        }
        Rectangle { } // spacer
        Button { text: "Accept Edits"; clicked => { root.edit-accept(); } }
    }
}
```

> Match the existing lightbox styling idioms (the codebase hand-rolls TouchArea-based buttons in places — follow whatever the neighboring lightbox buttons do; the snippet above is the intent, adapt to local component conventions). The image/Flickable area should reduce its effective width when `edit-mode` so the sidebar does not overlay the image (mirror the detail-panel reflow if the lightbox supports it; if a simple overlay is materially simpler and the image stays usable, an overlay panel anchored right is acceptable — note the choice in the commit).

- [ ] **Step 4: Build to compile the markup**

Run: `cargo build -p imgfind-gui`
Expected: builds (Slint validates the markup; property/callback names must match Task 7 bindings).

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/ui/app.slint
git commit -m "feat(gui): lightbox edit-mode sidebar and Edit toggle (markup)"
```

---

### Task 7: Lightbox edit-mode wiring + live preview + accept (Rust)

**Files:**
- Modify: `imgfind-gui/src/main.rs` (callback handlers, edit-mode state, live preview, `e`/Esc keys)
- Create: `imgfind-gui/src/edits_ui.rs` (pure slider/readout helpers) + declare `mod edits_ui;`
- Test: inline `#[cfg(test)]` in `imgfind-gui/src/edits_ui.rs`

**Interfaces:**
- Consumes: Task 6 properties/callbacks; `imgfind::edits::{ImageEdits, apply_adjustments}`; `Database::{get_image_edits, set_image_edits}` (Task 3); `imgfind::thumbnail::regenerate_thumbnails_for_image` (Task 5); existing `load_lightbox_image`, `image_util::dynamic_to_slint_image`, the generation-guard pattern (`lb_generation`), `block_on`, `decode_full_image`.
- Produces: a working edit mode. New pure helpers in `edits_ui.rs`:
  - `pub fn clamp_exposure(v: f32) -> f32`
  - `pub fn format_exposure(v: f32) -> String` (e.g. `"+1.30 EV"`, `"0.00 EV"`, `"-0.75 EV"`)

- [ ] **Step 1: Write failing tests for the pure helpers**

In `imgfind-gui/src/edits_ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clamp_to_range() {
        assert_eq!(clamp_exposure(9.0), 3.0);
        assert_eq!(clamp_exposure(-9.0), -3.0);
        assert_eq!(clamp_exposure(1.2), 1.2);
    }
    #[test]
    fn format_has_sign_and_two_decimals() {
        assert_eq!(format_exposure(1.3), "+1.30 EV");
        assert_eq!(format_exposure(0.0), "0.00 EV");
        assert_eq!(format_exposure(-0.75), "-0.75 EV");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p imgfind-gui edits_ui::`
Expected: FAIL — module/functions not found.

- [ ] **Step 3: Implement `edits_ui.rs` helpers**

```rust
//! Pure helpers for the lightbox exposure control (UI-thread-free, unit-tested).
use imgfind::edits::ImageEdits;

pub fn clamp_exposure(v: f32) -> f32 {
    v.clamp(ImageEdits::EXPOSURE_MIN, ImageEdits::EXPOSURE_MAX)
}

pub fn format_exposure(v: f32) -> String {
    if v > 0.0 {
        format!("+{v:.2} EV")
    } else if v < 0.0 {
        format!("{v:.2} EV")
    } else {
        "0.00 EV".to_string()
    }
}
```

Add `mod edits_ui;` near the other `mod` declarations in `imgfind-gui/src/main.rs`.

- [ ] **Step 4: Wire the callbacks and keys in `main.rs`**

Add edit-mode runtime state near the lightbox state (around the `lb_generation`/`lb_shown_fullres` declarations):

```rust
// Unedited base for live edit preview (decoded fresh on entering edit mode).
let lb_edit_base: Arc<Mutex<Option<image::DynamicImage>>> = Arc::new(Mutex::new(None));
// Last-accepted exposure for the current image (for Reset / discard-on-exit).
let lb_last_accepted_exposure: Arc<Mutex<f32>> = Arc::new(Mutex::new(0.0));
```

Implement handlers (sketch — adapt to the existing closure/`Weak`/`backend` capture style used by the neighboring lightbox callbacks):

- `on_edit_toggle`: flip `edit-mode`.
  - **Entering:** read stored edits for the current image (`block_on(db.get_image_edits(rel))`), set `edit-exposure` and `lb_last_accepted_exposure`, spawn a background decode of the **unedited** base (`decode_full_image(abs)` or the 2048 path used by `load_lightbox_image`, but **without** baking edits — decode the original directly), store it in `lb_edit_base`, then render the live preview with the current exposure.
  - **Exiting (discard):** set `edit-exposure` back to `*lb_last_accepted_exposure`, clear `lb_edit_base`, and restore the normal baked lightbox image via the existing `load_lightbox_image(...)` path.
- `on_edit_exposure_changed(v)`: `let v = edits_ui::clamp_exposure(v);` set `edit-exposure`; render live preview = `apply_adjustments(base.clone(), &ImageEdits{exposure:v})` → `dynamic_to_slint_image` → `set_lightbox_image`, guarded latest-wins (bump/check a generation counter, or run the multiply on a worker thread and drop stale results).
- `on_edit_reset`: set `edit-exposure` = `*lb_last_accepted_exposure`; re-render preview.
- `on_edit_accept`: on a background thread —
  ```rust
  let edits = ImageEdits { exposure: edits_ui::clamp_exposure(current_exposure) };
  block_on(db.set_image_edits(&rel, &edits))?;
  imgfind::thumbnail::regenerate_thumbnails_for_image(&db, &abs, &hash, &edits)?;
  ```
  then on the UI thread: update `*lb_last_accepted_exposure = edits.exposure`, exit edit mode, evict this image's entry from the grid decode LRU and re-request its visible thumbnail (reuse the existing grid-refresh/`request`/invalidate path), and restore the normal lightbox image (now baked).
- **Keyboard:** in the lightbox `FocusScope` key handler, map `e` (when not typing) to `edit-toggle()`. In the Esc branch: `if edit-mode { edit-toggle() /* discard */ } else { lightbox-close() }`.
- **Navigation guard:** in `on_lightbox_prev`/`on_lightbox_next`, if `edit-mode` is set, exit edit mode (discard) first, then navigate.

> Reuse the existing helpers for: resolving the current image's `rel`/`abs`/`hash` (the lightbox already tracks `current_lightbox_index` into the row list — pull path+hash the same way `load_lightbox_image`/`load_lightbox_fullres` do), the generation-guard idiom, and grid LRU eviction. Do not invent new infra where an existing path exists.

- [ ] **Step 5: Run helper tests + build the GUI**

Run: `cargo test -p imgfind-gui edits_ui::` then `cargo build -p imgfind-gui`.
Expected: helper tests PASS; GUI builds with all callbacks bound (no missing-callback Slint panic at build/codegen).

- [ ] **Step 6: Commit**

```bash
git add imgfind-gui/src/main.rs imgfind-gui/src/edits_ui.rs
git commit -m "feat(gui): lightbox exposure edit mode with live preview and accept"
```

---

### Task 8: Workspace verification, docs, manual smoke

**Files:**
- Modify: `CLAUDE.md` (document the feature)
- No new tests; this task is integration + docs.

**Interfaces:** none new.

- [ ] **Step 1: Full workspace check**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: all clean/green. Fix any fmt/clippy/test issues.

- [ ] **Step 2: Manual smoke (best-effort, document result)**

Build and run the GUI against a test library with at least one RAW and one JPEG:
```bash
cargo run -p imgfind-gui -- --dir <a-test-library>
```
Open the lightbox, press `e`, drag exposure (preview brightens/darkens live), click "Accept Edits", close and reopen the lightbox and check the grid tile — the image now shows the edit (thumbnails regenerated). Press `e`, move the slider, press Esc — change is discarded. If no test library is available, note that manual verification was deferred and rely on the automated tests.

- [ ] **Step 3: Update `CLAUDE.md`**

Add to the architecture/storage notes:
- new `image_edits` table (migration 004) and `LATEST_MIGRATION_VERSION = 4`;
- `src/edits.rs` `ImageEdits` + `apply_adjustments`, baked at the thumbnail/decode generation seam (originals never modified);
- lightbox **edit mode**: `e` / "Edit" toggles a right sidebar with an Exposure slider (±3 EV), **Reset**, and **Accept Edits** (persists to `image_edits` and regenerates every cached thumbnail size via `regenerate_thumbnails_for_image`); Esc/`e` discards un-accepted changes;
- the content-hash-keyed duplicate limitation (byte-identical duplicates share a baked rendition);
- link `docs/superpowers/specs/2026-06-24-lightbox-image-adjustments-design.md`.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document lightbox exposure adjustments feature"
```

---

## Self-Review notes

- **Spec coverage:** storage (Task 2/3) · pure transform (Task 1) · all-types via post-decode seam (Task 4) · live preview (Task 7) · Accept regenerates all cached sizes (Task 5/7) · Reset/discard model (Task 7) · embeddings untouched (no task touches embeddings — correct) · `e`/Edit toggle + sidebar (Task 6/7) · docs (Task 8). All covered.
- **Type consistency:** `ImageEdits { exposure: f32 }`, `apply_adjustments(DynamicImage, &ImageEdits)`, `get_image_edits`/`set_image_edits(&RelativePath, &ImageEdits)`, `get_thumbnail_sizes(&str) -> Vec<u32>`, `regenerate_thumbnails_for_image(&Database, &str, &str, &ImageEdits) -> usize`, `clamp_exposure`/`format_exposure` — names used consistently across tasks.
- **Known adaptation points (flagged inline, not placeholders):** exact turso binding style, path-conversion helper names, DB test-fixture helpers, and lightbox button styling must match existing code; each task tells the implementer to mirror the neighboring pattern.
```
