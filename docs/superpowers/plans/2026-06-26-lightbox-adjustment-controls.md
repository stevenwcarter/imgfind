# Lightbox Adjustment Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expand lightbox edit mode from one Exposure slider to six Lightroom-Basic-style adjustments (exposure, saturation, blacks, whites, brightness, contrast), each with a per-control reset button, and fix Esc-twice so leaving edit mode then pressing Esc closes the lightbox.

**Architecture:** Extend the existing non-destructive `ImageEdits` struct + linear-light render pipeline in `src/edits.rs` (hybrid: exposure/saturation in linear light, tonal controls in display space after gamma). Persist five new REAL columns via migration 005. Drive six sliders through generic index-keyed Slint callbacks; gather the full `ImageEdits` from the window for the WYSIWYG live preview and Accept.

**Tech Stack:** Rust (edition 2024), `image` crate, `turso` (async SQLite), Slint 1.x, anyhow, tracing.

## Global Constraints

- Rust edition 2024; code must be `cargo clippy --workspace --all-targets` clean and `cargo fmt --all --check` clean.
- All Rust coding tasks go to the `rust-developer` agent.
- Edits are **non-destructive** (baked at the thumbnail seam; original file never written).
- **Identity invariant:** all six controls neutral (0) ⇒ `ImageEdits::is_identity() == true` ⇒ fast byte-identical thumbnail path. Must stay pinned by tests.
- Slint button/label text is **ASCII/Latin-1 only** (default font tofus symbol glyphs).
- The live preview must use the same `LinearRgb::render` that the thumbnails bake (WYSIWYG).
- Control ranges: Exposure ±3 (EV); saturation/blacks/whites/brightness/contrast ±100. Neutral = 0 for all.
- Pipeline order (per pixel): exposure (lin) → saturation (lin) → highlight roll-off (lin) → sRGB gamma → blacks → whites → brightness → contrast → 8-bit.
- Constants: `BLACK_STRENGTH = 0.5`, `WHITE_STRENGTH = 0.5`, `HIGHLIGHT_KNEE = 0.8` (existing).

---

### Task 1: Expand `ImageEdits` struct + identity/clamp

**Files:**
- Modify: `src/edits.rs` (struct `ImageEdits` lines ~7-39)
- Test: `src/edits.rs` (`#[cfg(test)] mod linear_tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `ImageEdits { exposure, saturation, blacks, whites, brightness, contrast: f32 }`; `ImageEdits::identity()`, `is_identity()`, `clamped()`; `const ADJ_MIN: f32 = -100.0; const ADJ_MAX: f32 = 100.0;` (existing `EXPOSURE_MIN/MAX` retained).

- [ ] **Step 1: Write failing tests**

Add to `linear_tests`:

```rust
#[test]
fn all_neutral_is_identity() {
    assert!(ImageEdits::identity().is_identity());
    assert!(
        ImageEdits {
            exposure: 0.0,
            saturation: 0.0,
            blacks: 0.0,
            whites: 0.0,
            brightness: 0.0,
            contrast: 0.0,
        }
        .is_identity()
    );
}

#[test]
fn any_nonzero_control_is_not_identity() {
    for e in [
        ImageEdits { saturation: 10.0, ..ImageEdits::identity() },
        ImageEdits { blacks: -5.0, ..ImageEdits::identity() },
        ImageEdits { whites: 5.0, ..ImageEdits::identity() },
        ImageEdits { brightness: 1.0, ..ImageEdits::identity() },
        ImageEdits { contrast: -1.0, ..ImageEdits::identity() },
    ] {
        assert!(!e.is_identity());
    }
}

#[test]
fn clamp_bounds_each_control() {
    let c = ImageEdits {
        exposure: 9.0,
        saturation: 999.0,
        blacks: -999.0,
        whites: 999.0,
        brightness: -999.0,
        contrast: 999.0,
    }
    .clamped();
    assert_eq!(c.exposure, 3.0);
    assert_eq!(c.saturation, 100.0);
    assert_eq!(c.blacks, -100.0);
    assert_eq!(c.whites, 100.0);
    assert_eq!(c.brightness, -100.0);
    assert_eq!(c.contrast, 100.0);
}
```

Every existing struct literal `ImageEdits { exposure: X }` in the test module must be updated to `ImageEdits { exposure: X, ..ImageEdits::identity() }` so they still compile.

- [ ] **Step 2: Run tests, verify they fail to compile** (missing fields)

Run: `cargo test -p imgfind edits::linear_tests 2>&1 | head -30`
Expected: compile error — unknown fields `saturation`/etc.

- [ ] **Step 3: Implement the struct expansion**

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageEdits {
    /// Exposure in photographic EV stops; linear gain = 2^exposure.
    pub exposure: f32,
    /// Saturation, -100..=100 (0 neutral; -100 grayscale, +100 doubles chroma).
    pub saturation: f32,
    /// Blacks, -100..=100 (shadow-weighted lift/drop).
    pub blacks: f32,
    /// Whites, -100..=100 (highlight-weighted lift/drop).
    pub whites: f32,
    /// Brightness, -100..=100 (midtone gamma lift).
    pub brightness: f32,
    /// Contrast, -100..=100 (S-pivot at mid-gray).
    pub contrast: f32,
}

impl ImageEdits {
    pub const EXPOSURE_MIN: f32 = -3.0;
    pub const EXPOSURE_MAX: f32 = 3.0;
    pub const ADJ_MIN: f32 = -100.0;
    pub const ADJ_MAX: f32 = 100.0;

    pub fn identity() -> Self {
        Self {
            exposure: 0.0,
            saturation: 0.0,
            blacks: 0.0,
            whites: 0.0,
            brightness: 0.0,
            contrast: 0.0,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.exposure.abs() < f32::EPSILON
            && self.saturation.abs() < f32::EPSILON
            && self.blacks.abs() < f32::EPSILON
            && self.whites.abs() < f32::EPSILON
            && self.brightness.abs() < f32::EPSILON
            && self.contrast.abs() < f32::EPSILON
    }

    pub fn clamped(self) -> Self {
        Self {
            exposure: self.exposure.clamp(Self::EXPOSURE_MIN, Self::EXPOSURE_MAX),
            saturation: self.saturation.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            blacks: self.blacks.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            whites: self.whites.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            brightness: self.brightness.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
            contrast: self.contrast.clamp(Self::ADJ_MIN, Self::ADJ_MAX),
        }
    }
}
```

This will break callers that construct `ImageEdits { exposure }` outside the test module (in `database.rs`, `main.rs`). Those are fixed in their own tasks; for now `cargo test -p imgfind edits` must compile (the lib `edits.rs` + its tests). If `database.rs` literals break the lib build, update them minimally to `..ImageEdits::identity()` here (they are touched fully in Task 3).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p imgfind edits::linear_tests 2>&1 | tail -20`
Expected: PASS (existing + new).

- [ ] **Step 5: Commit**

```bash
git add src/edits.rs
git commit -m "feat(edits): expand ImageEdits to six adjustment controls"
```

---

### Task 2: Render pipeline — saturation + display-stage controls

**Files:**
- Modify: `src/edits.rs` (`tonemap_channel`, `LinearRgb::render`, add helpers)
- Test: `src/edits.rs` (`linear_tests`)

**Interfaces:**
- Consumes: `ImageEdits` (Task 1).
- Produces pure fns: `smoothstep(a,b,x)->f32`, `shadow_weight(d)->f32`, `highlight_weight(d)->f32`, `apply_saturation(r,g,b,sat)->(f32,f32,f32)`, `apply_blacks(d,blacks)->f32`, `apply_whites(d,whites)->f32`, `apply_brightness(d,brightness)->f32`, `apply_contrast(d,contrast)->f32`, `channel_to_display(linear_exposed:f32, edits:&ImageEdits)->u8`. `LinearRgb::render` now applies the full pipeline. `tonemap_channel(linear,ev)->u8` retained (wrapper).

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn smoothstep_anchors() {
    assert_eq!(smoothstep(0.0, 0.5, -1.0), 0.0);
    assert_eq!(smoothstep(0.0, 0.5, 1.0), 1.0);
    let mid = smoothstep(0.0, 1.0, 0.5);
    assert!((mid - 0.5).abs() < 1e-6);
}

#[test]
fn weights_in_range_and_at_anchors() {
    assert!((shadow_weight(0.0) - 1.0).abs() < 1e-6);
    assert!(shadow_weight(0.6) < 1e-6);
    assert!(highlight_weight(0.4) < 1e-6);
    assert!((highlight_weight(1.0) - 1.0).abs() < 1e-6);
    for d in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        assert!((0.0..=1.0).contains(&shadow_weight(d)));
        assert!((0.0..=1.0).contains(&highlight_weight(d)));
    }
}

#[test]
fn display_controls_neutral_are_noops() {
    for d in [0.0f32, 0.2, 0.5, 0.8, 1.0] {
        assert!((apply_blacks(d, 0.0) - d).abs() < 1e-6);
        assert!((apply_whites(d, 0.0) - d).abs() < 1e-6);
        assert!((apply_brightness(d, 0.0) - d).abs() < 1e-6);
        assert!((apply_contrast(d, 0.0) - d).abs() < 1e-6);
    }
}

#[test]
fn brightness_raises_midtones_endpoints_fixed() {
    assert!(apply_brightness(0.5, 50.0) > 0.5);
    assert!(apply_brightness(0.5, -50.0) < 0.5);
    assert!((apply_brightness(0.0, 50.0)).abs() < 1e-6);
    assert!((apply_brightness(1.0, 50.0) - 1.0).abs() < 1e-6);
}

#[test]
fn contrast_pivots_at_mid_gray() {
    assert!((apply_contrast(0.5, 80.0) - 0.5).abs() < 1e-6);
    assert!(apply_contrast(0.8, 80.0) > 0.8);
    assert!(apply_contrast(0.2, 80.0) < 0.2);
    // negative contrast pulls toward 0.5
    assert!(apply_contrast(0.8, -80.0) < 0.8 && apply_contrast(0.8, -80.0) > 0.5);
}

#[test]
fn blacks_lift_shadows_whites_lift_highlights() {
    assert!(apply_blacks(0.05, 60.0) > 0.05);
    assert!(apply_blacks(0.05, -60.0) < 0.05);
    assert!(apply_whites(0.95, 60.0) > 0.95);
    assert!(apply_whites(0.95, -60.0) < 0.95);
    // midtone barely moved by either
    assert!((apply_blacks(0.5, 100.0) - 0.5).abs() < 0.05);
    assert!((apply_whites(0.5, 100.0) - 0.5).abs() < 0.05);
}

#[test]
fn saturation_extremes() {
    // -100 => all channels collapse to luma (equal)
    let (r, g, b) = apply_saturation(0.8, 0.2, 0.1, -100.0);
    assert!((r - g).abs() < 1e-6 && (g - b).abs() < 1e-6);
    // +100 widens the spread vs neutral
    let (r1, _, b1) = apply_saturation(0.8, 0.2, 0.1, 0.0);
    let (r2, _, b2) = apply_saturation(0.8, 0.2, 0.1, 100.0);
    assert!((r2 - b2).abs() > (r1 - b1).abs());
}

#[test]
fn render_all_neutral_roundtrips_srgb8() {
    let mut img = image::RgbImage::new(1, 1);
    img.put_pixel(0, 0, image::Rgb([50, 128, 200]));
    let out = LinearRgb::from_srgb8(&img).render(&ImageEdits::identity());
    let p = out.get_pixel(0, 0);
    for c in 0..3 {
        assert!((p[c] as i32 - img.get_pixel(0, 0)[c] as i32).abs() <= 1);
    }
}
```

- [ ] **Step 2: Run tests, verify fail** (undefined fns)

Run: `cargo test -p imgfind edits::linear_tests 2>&1 | head -30`
Expected: compile errors — `smoothstep` / `apply_*` not found.

- [ ] **Step 3: Implement helpers + rewire render**

```rust
/// GLSL-style smoothstep: 0 below `a`, 1 above `b`, smooth Hermite between.
fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    if (b - a).abs() < f32::EPSILON {
        return if x < a { 0.0 } else { 1.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 1.0 in deep shadow, tapering to 0.0 by mid-gray.
fn shadow_weight(d: f32) -> f32 {
    1.0 - smoothstep(0.0, 0.5, d)
}

/// 0.0 below mid-gray, rising to 1.0 at white.
fn highlight_weight(d: f32) -> f32 {
    smoothstep(0.5, 1.0, d)
}

/// Scale chroma around Rec.709 linear luma. `sat` in -100..=100.
pub fn apply_saturation(r: f32, g: f32, b: f32, sat: f32) -> (f32, f32, f32) {
    let f = 1.0 + sat / 100.0;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    (
        (y + f * (r - y)).max(0.0),
        (y + f * (g - y)).max(0.0),
        (y + f * (b - y)).max(0.0),
    )
}

/// Shadow-weighted lift/drop on a display value. `blacks` in -100..=100.
pub fn apply_blacks(d: f32, blacks: f32) -> f32 {
    d + (blacks / 100.0) * BLACK_STRENGTH * shadow_weight(d)
}

/// Highlight-weighted lift/drop on a display value. `whites` in -100..=100.
pub fn apply_whites(d: f32, whites: f32) -> f32 {
    d + (whites / 100.0) * WHITE_STRENGTH * highlight_weight(d)
}

/// Midtone gamma lift; endpoints 0 and 1 fixed. `brightness` in -100..=100.
pub fn apply_brightness(d: f32, brightness: f32) -> f32 {
    let gamma = 2f32.powf(-brightness / 100.0);
    d.clamp(0.0, 1.0).powf(gamma)
}

/// Linear contrast pivoted at mid-gray. `contrast` in -100..=100.
pub fn apply_contrast(d: f32, contrast: f32) -> f32 {
    0.5 + (d - 0.5) * (1.0 + contrast / 100.0)
}

/// Map one already-exposed linear channel through roll-off → gamma → the
/// display-space tone controls → 8-bit. Exposure and saturation are applied at
/// the pixel level before this (saturation is cross-channel).
pub fn channel_to_display(linear_exposed: f32, edits: &ImageEdits) -> u8 {
    let rolled = highlight_rolloff(linear_exposed.max(0.0)).clamp(0.0, 1.0);
    let mut d = linear_to_srgb(rolled);
    d = apply_blacks(d, edits.blacks);
    d = apply_whites(d, edits.whites);
    d = apply_brightness(d, edits.brightness);
    d = apply_contrast(d, edits.contrast);
    (d.clamp(0.0, 1.0) * 255.0).round().clamp(0.0, 255.0) as u8
}
```

Add the strength constants near `HIGHLIGHT_KNEE`:

```rust
/// Full-slider display-space shift for Blacks/Whites at the extreme (tapers to 0 across mids).
pub const BLACK_STRENGTH: f32 = 0.5;
pub const WHITE_STRENGTH: f32 = 0.5;
```

Rewrite `tonemap_channel` as a wrapper (preserves existing tests):

```rust
/// Exposure → highlight roll-off → sRGB gamma → 8-bit (no other controls).
pub fn tonemap_channel(linear: f32, ev: f32) -> u8 {
    channel_to_display(linear.max(0.0) * 2f32.powf(ev), &ImageEdits::identity())
}
```

Rewrite `LinearRgb::render`:

```rust
pub fn render(&self, edits: &ImageEdits) -> image::RgbImage {
    let e = edits.clamped();
    let gain = 2f32.powf(e.exposure);
    let mut out = image::RgbImage::new(self.0.width(), self.0.height());
    for (o, p) in out.pixels_mut().zip(self.0.pixels()) {
        let (r, g, b) = apply_saturation(
            p[0] * gain,
            p[1] * gain,
            p[2] * gain,
            e.saturation,
        );
        *o = image::Rgb([
            channel_to_display(r, &e),
            channel_to_display(g, &e),
            channel_to_display(b, &e),
        ]);
    }
    out
}
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p imgfind edits 2>&1 | tail -20`
Expected: PASS (all old `linear_tests` + new). Note `render_brightens_with_exposure`, `tonemap_*` still pass.

- [ ] **Step 5: Commit**

```bash
git add src/edits.rs
git commit -m "feat(edits): hybrid pipeline for saturation/blacks/whites/brightness/contrast"
```

---

### Task 3: DB — migration 005 + get/set all six fields

**Files:**
- Modify: `src/schema.rs` (`run_migrations`, `LATEST_MIGRATION_VERSION`, add `migration_005_edit_controls`, tests)
- Modify: `src/database.rs` (`get_image_edits` ~572, `set_image_edits` ~596, test `image_edits_upsert_and_read` ~3043)

**Interfaces:**
- Consumes: `ImageEdits` (Task 1).
- Produces: `get_image_edits`/`set_image_edits` read/write all six columns; `LATEST_MIGRATION_VERSION = 5`.

- [ ] **Step 1: Write failing tests**

In `src/schema.rs` tests, add:

```rust
async fn column_exists(conn: &turso::Connection, table: &str, col: &str) -> bool {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await
        .unwrap();
    while let Some(row) = rows.next().await.unwrap() {
        let name: String = row.get_value(1).unwrap().as_text().unwrap().clone();
        if name == col {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn migration_005_adds_edit_control_columns() {
    let conn = mem().await;
    run_migrations(&conn).await.unwrap();
    for c in ["contrast", "brightness", "blacks", "whites", "saturation"] {
        assert!(column_exists(&conn, "image_edits", c).await, "missing column {c}");
    }
}
```

In `src/database.rs` `image_edits_upsert_and_read`, extend to set/read all six (replace the body's writes/reads):

```rust
db.set_image_edits(
    &rel_path,
    &ImageEdits {
        exposure: 1.5,
        saturation: 40.0,
        blacks: -20.0,
        whites: 15.0,
        brightness: 10.0,
        contrast: -5.0,
    },
)
.await
.unwrap();
let got = db.get_image_edits(&rel_path).await.unwrap();
assert_eq!(got.exposure, 1.5);
assert_eq!(got.saturation, 40.0);
assert_eq!(got.blacks, -20.0);
assert_eq!(got.whites, 15.0);
assert_eq!(got.brightness, 10.0);
assert_eq!(got.contrast, -5.0);
```

(Adjust `as_text()`/`get_value` calls in `column_exists` to match the turso row API used elsewhere in the file — check `table_exists`/`col_f64` for the exact accessor; use whatever idiom reads a TEXT column there.)

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p imgfind schema::tests::migration_005 database::tests::image_edits 2>&1 | head -30`
Expected: FAIL — columns missing / fields unknown.

- [ ] **Step 3: Implement migration + bump version**

In `src/schema.rs`, set `LATEST_MIGRATION_VERSION = 5`. Add gated call after the migration-004 block:

```rust
if current < 5 {
    migration_005_edit_controls(conn)
        .await
        .context("migration 5 (edit control columns)")?;
}
```

Add the migration fn after `migration_004_image_edits`:

```rust
/// Migration 5: add the five extra adjustment columns to image_edits.
async fn migration_005_edit_controls(conn: &turso::Connection) -> Result<()> {
    for col in ["contrast", "brightness", "blacks", "whites", "saturation"] {
        conn.execute(
            &format!("ALTER TABLE image_edits ADD COLUMN {col} REAL NOT NULL DEFAULT 0.0"),
            (),
        )
        .await
        .with_context(|| format!("add column {col} to image_edits"))?;
    }
    Ok(())
}
```

- [ ] **Step 4: Update `get_image_edits` / `set_image_edits` in `src/database.rs`**

`get_image_edits` query + mapping:

```rust
let mut rows = conn
    .query(
        "SELECT e.exposure, e.saturation, e.blacks, e.whites, e.brightness, e.contrast
         FROM image_edits e
         JOIN images i ON i.id = e.image_id
         WHERE i.path = ?1",
        (path.as_str().into_owned(),),
    )
    .await?;
match rows.next().await? {
    Some(row) => Ok(crate::edits::ImageEdits {
        exposure: col_f64(&row, 0, "exposure")? as f32,
        saturation: col_f64(&row, 1, "saturation")? as f32,
        blacks: col_f64(&row, 2, "blacks")? as f32,
        whites: col_f64(&row, 3, "whites")? as f32,
        brightness: col_f64(&row, 4, "brightness")? as f32,
        contrast: col_f64(&row, 5, "contrast")? as f32,
    }
    .clamped()),
    None => Ok(crate::edits::ImageEdits::identity()),
}
```

`set_image_edits` (after `let edits = edits.clamped();`):

```rust
conn.execute(
    "INSERT INTO image_edits (image_id, exposure, saturation, blacks, whites, brightness, contrast, updated_at)
     SELECT i.id, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP FROM images i WHERE i.path = ?1
     ON CONFLICT(image_id) DO UPDATE SET
       exposure = excluded.exposure,
       saturation = excluded.saturation,
       blacks = excluded.blacks,
       whites = excluded.whites,
       brightness = excluded.brightness,
       contrast = excluded.contrast,
       updated_at = CURRENT_TIMESTAMP",
    (
        path.as_str().into_owned(),
        edits.exposure as f64,
        edits.saturation as f64,
        edits.blacks as f64,
        edits.whites as f64,
        edits.brightness as f64,
        edits.contrast as f64,
    ),
)
.await?;
```

- [ ] **Step 5: Run tests, verify pass**

Run: `cargo test -p imgfind schema:: database::tests::image_edits 2>&1 | tail -20`
Expected: PASS, including the existing `migrations_are_idempotent_and_create_tables`.

- [ ] **Step 6: Commit**

```bash
git add src/schema.rs src/database.rs
git commit -m "feat(db): migration 005 + persist all six adjustment fields"
```

---

### Task 4: GUI pure helpers — `EditControl` + per-control clamp/format

**Files:**
- Modify: `imgfind-gui/src/edits_ui.rs`
- Test: `imgfind-gui/src/edits_ui.rs` (`mod tests`)

**Interfaces:**
- Consumes: `imgfind::edits::ImageEdits` (Task 1).
- Produces: `enum EditControl { Exposure, Saturation, Blacks, Whites, Brightness, Contrast }` with `from_i32(i32)->Option<Self>`, `to_i32(self)->i32`, `clamp(self,f32)->f32`, `neutral(self)->f32`, `format(self,f32)->String`. Keep `clamp_exposure`/`format_exposure` delegating to `EditControl::Exposure`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn control_index_roundtrip() {
    for c in [
        EditControl::Exposure,
        EditControl::Saturation,
        EditControl::Blacks,
        EditControl::Whites,
        EditControl::Brightness,
        EditControl::Contrast,
    ] {
        assert_eq!(EditControl::from_i32(c.to_i32()), Some(c));
    }
    assert_eq!(EditControl::from_i32(99), None);
}

#[test]
fn clamp_per_control_bounds() {
    assert_eq!(EditControl::Exposure.clamp(9.0), 3.0);
    assert_eq!(EditControl::Exposure.clamp(-9.0), -3.0);
    assert_eq!(EditControl::Contrast.clamp(999.0), 100.0);
    assert_eq!(EditControl::Saturation.clamp(-999.0), -100.0);
}

#[test]
fn format_exposure_has_ev_others_unitless() {
    assert_eq!(EditControl::Exposure.format(1.3), "+1.30 EV");
    assert_eq!(EditControl::Contrast.format(45.0), "+45");
    assert_eq!(EditControl::Blacks.format(-30.0), "-30");
    assert_eq!(EditControl::Whites.format(0.0), "0");
}
```

Keep the existing `clamp_to_range` / `format_has_sign_and_two_decimals` tests.

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p imgfind-gui edits_ui 2>&1 | head -30`
Expected: FAIL — `EditControl` undefined.

- [ ] **Step 3: Implement**

```rust
use imgfind::edits::ImageEdits;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditControl {
    Exposure,
    Saturation,
    Blacks,
    Whites,
    Brightness,
    Contrast,
}

impl EditControl {
    pub fn to_i32(self) -> i32 {
        match self {
            EditControl::Exposure => 0,
            EditControl::Saturation => 1,
            EditControl::Blacks => 2,
            EditControl::Whites => 3,
            EditControl::Brightness => 4,
            EditControl::Contrast => 5,
        }
    }

    pub fn from_i32(i: i32) -> Option<Self> {
        Some(match i {
            0 => EditControl::Exposure,
            1 => EditControl::Saturation,
            2 => EditControl::Blacks,
            3 => EditControl::Whites,
            4 => EditControl::Brightness,
            5 => EditControl::Contrast,
            _ => return None,
        })
    }

    pub fn neutral(self) -> f32 {
        0.0
    }

    pub fn clamp(self, v: f32) -> f32 {
        match self {
            EditControl::Exposure => v.clamp(ImageEdits::EXPOSURE_MIN, ImageEdits::EXPOSURE_MAX),
            _ => v.clamp(ImageEdits::ADJ_MIN, ImageEdits::ADJ_MAX),
        }
    }

    pub fn format(self, v: f32) -> String {
        match self {
            EditControl::Exposure => format_exposure(v),
            _ => {
                let n = v.round() as i32;
                if n > 0 {
                    format!("+{n}")
                } else {
                    format!("{n}")
                }
            }
        }
    }
}

/// Clamp an exposure value to the supported EV range.
pub fn clamp_exposure(v: f32) -> f32 {
    EditControl::Exposure.clamp(v)
}
```

Keep `format_exposure` as-is (used by `EditControl::Exposure.format`).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p imgfind-gui edits_ui 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/edits_ui.rs
git commit -m "feat(gui): EditControl enum with per-control clamp/format"
```

---

### Task 5: Slint markup — six control rows, per-control reset, sidebar resize, Esc fix

**Files:**
- Modify: `imgfind-gui/ui/app.slint` (root props/callbacks ~65-78; sidebar ~1156-1273; content-shrink ~951; lightbox Esc branch ~280-289)

**Interfaces:**
- Consumes: nothing (markup).
- Produces (Slint-generated Rust API the wiring task uses): properties `edit-exposure`, `edit-saturation`, `edit-blacks`, `edit-whites`, `edit-brightness`, `edit-contrast` (`float`, in-out) and matching `-label` (`string`); callbacks `edit-control-changed(int, float)`, `edit-control-reset(int)`, `edit-reset-all()`, `edit-accept()`, `edit-toggle()`. Indices match `EditControl::to_i32` (0 Exposure … 5 Contrast).

- [ ] **Step 1: Replace the per-control props/callbacks block (~65-78)**

```slint
// Lightbox edit-mode (image adjustments).
in-out property <bool> edit-mode;
in-out property <float> edit-exposure;
in-out property <float> edit-saturation;
in-out property <float> edit-blacks;
in-out property <float> edit-whites;
in-out property <float> edit-brightness;
in-out property <float> edit-contrast;
in property <string> edit-exposure-label: "0.00 EV";
in property <string> edit-saturation-label: "0";
in property <string> edit-blacks-label: "0";
in property <string> edit-whites-label: "0";
in property <string> edit-brightness-label: "0";
in property <string> edit-contrast-label: "0";
in property <bool> edit-busy;
in property <string> edit-busy-label: "Working...";
callback edit-control-changed(int, float);
callback edit-control-reset(int);
callback edit-reset-all();
callback edit-accept();
```

(Keep `edit-toggle()` wherever it is declared.)

- [ ] **Step 2: Add an `AdjustRow` component** (top of file, near other component defs)

```slint
component AdjustRow inherits HorizontalLayout {
    in property <int> index;
    in property <string> label;
    in property <string> value-label;
    in property <float> minimum;
    in property <float> maximum;
    in-out property <float> value;
    in property <bool> busy;
    callback changed(float);
    callback reset();
    spacing: 8px;
    // Per-control reset (ASCII-only text; "0" = reset-to-neutral).
    reset-btn := TouchArea {
        width: 30px;
        clicked => { if (!root.busy) { root.reset(); } }
        Rectangle {
            background: root.busy ? #ffffff08 : (reset-btn.has-hover ? #ffffff22 : #ffffff11);
            border-radius: 4px;
            Text {
                text: "0";
                color: root.busy ? #666666 : white;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
        }
    }
    VerticalLayout {
        spacing: 2px;
        horizontal-stretch: 1;
        Text {
            text: root.label + ": " + root.value-label;
            color: #cccccc;
            font-size: 12px;
        }
        Slider {
            minimum: root.minimum;
            maximum: root.maximum;
            value <=> root.value;
            changed(v) => { root.changed(v); }
        }
    }
}
```

Note: the `value <=> root.value` two-way binding lets a Rust write to the
window property reseat the thumb (per-control + global reset).

- [ ] **Step 3: Replace the sidebar control area** (the Exposure `Text`+`Slider`+`HorizontalLayout{Reset}` block, ~1173-1209) with six rows + a global Reset, keeping the `Adjustments` header, busy spinner, spacer, and Accept untouched:

```slint
AdjustRow {
    index: 0; label: "Exposure"; value-label: root.edit-exposure-label;
    minimum: -3.0; maximum: 3.0; value <=> root.edit-exposure; busy: root.edit-busy;
    changed(v) => { root.edit-control-changed(0, v); }
    reset => { root.edit-control-reset(0); }
}
AdjustRow {
    index: 1; label: "Saturation"; value-label: root.edit-saturation-label;
    minimum: -100.0; maximum: 100.0; value <=> root.edit-saturation; busy: root.edit-busy;
    changed(v) => { root.edit-control-changed(1, v); }
    reset => { root.edit-control-reset(1); }
}
AdjustRow {
    index: 2; label: "Blacks"; value-label: root.edit-blacks-label;
    minimum: -100.0; maximum: 100.0; value <=> root.edit-blacks; busy: root.edit-busy;
    changed(v) => { root.edit-control-changed(2, v); }
    reset => { root.edit-control-reset(2); }
}
AdjustRow {
    index: 3; label: "Whites"; value-label: root.edit-whites-label;
    minimum: -100.0; maximum: 100.0; value <=> root.edit-whites; busy: root.edit-busy;
    changed(v) => { root.edit-control-changed(3, v); }
    reset => { root.edit-control-reset(3); }
}
AdjustRow {
    index: 4; label: "Brightness"; value-label: root.edit-brightness-label;
    minimum: -100.0; maximum: 100.0; value <=> root.edit-brightness; busy: root.edit-busy;
    changed(v) => { root.edit-control-changed(4, v); }
    reset => { root.edit-control-reset(4); }
}
AdjustRow {
    index: 5; label: "Contrast"; value-label: root.edit-contrast-label;
    minimum: -100.0; maximum: 100.0; value <=> root.edit-contrast; busy: root.edit-busy;
    changed(v) => { root.edit-control-changed(5, v); }
    reset => { root.edit-control-reset(5); }
}
// Global reset (all controls to neutral).
HorizontalLayout {
    reset-all-touch := TouchArea {
        height: 28px;
        clicked => { if (!root.edit-busy) { root.edit-reset-all(); } }
        Rectangle {
            background: root.edit-busy ? #ffffff08 : (reset-all-touch.has-hover ? #ffffff22 : #ffffff11);
            border-radius: 4px;
            Text {
                text: "Reset All";
                color: root.edit-busy ? #666666 : white;
                horizontal-alignment: center;
                vertical-alignment: center;
            }
        }
    }
}
```

- [ ] **Step 4: Widen the sidebar 240px → 280px**

In the sidebar `Rectangle` (~1162): `width: 280px;`. In the content-area shrink (~951): `width: parent.width - (root.edit-mode ? 280px : 0px);`. Search the file for any other `240px` tied to edit-mode and update consistently.

- [ ] **Step 5: Esc fix in the lightbox key branch** (~280-289)

```slint
if (event.text == Key.Escape) {
    if (root.edit-mode) {
        root.edit-toggle();
        app-keys.focus();
    } else {
        root.lightbox-close();
    }
    return accept;
}
```

- [ ] **Step 6: Verify the GUI compiles**

Run: `cargo build -p imgfind-gui 2>&1 | tail -25`
Expected: builds. Unwired new callbacks are fine (handlers are optional in Slint); the old `edit-exposure-changed`/`edit-reset` handlers in `main.rs` will fail to compile if they reference removed callbacks — if so, this task may leave `main.rs` temporarily referencing them; resolve by completing Task 6 in the same review cycle. Prefer: keep `main.rs` building by leaving its old `on_edit_exposure_changed`/`on_edit_reset` closures only if those callbacks still exist. Since this task removes them, **comment out** the now-dangling `main.rs` handler registrations with a `// rewired in Task 6` note so the crate builds, then Task 6 replaces them.

- [ ] **Step 7: Commit**

```bash
git add imgfind-gui/ui/app.slint imgfind-gui/src/main.rs
git commit -m "feat(gui): six adjustment rows with per-control reset; Esc-twice focus fix"
```

---

### Task 6: GUI wiring — generic control callbacks, full-edits preview, reset-all, accept

**Files:**
- Modify: `imgfind-gui/src/main.rs` (edit callbacks ~1400-1634; `set_edit_exposure`/`render_edit_preview` ~4172-4211; `lb_last_accepted_exposure` decl ~367)

**Interfaces:**
- Consumes: `EditControl` (Task 4), Slint props/callbacks (Task 5), `ImageEdits` (Task 1), `Backend::image_edits`/`save_edits_and_regenerate` (unchanged).
- Produces: window handlers `on_edit_control_changed`, `on_edit_control_reset`, `on_edit_reset_all`, `on_edit_accept`, updated `on_edit_toggle`; helpers `edits_from_window(&MainWindow)->ImageEdits`, `set_edit_control(&MainWindow, EditControl, f32)`; `render_edit_preview` taking `&ImageEdits`.

- [ ] **Step 1: Add helpers** (near `set_edit_exposure`, ~4172). Replace `set_edit_exposure` usage with the generic setter; keep a thin `set_edit_exposure` if other call sites use it, else remove.

```rust
fn set_edit_control(w: &MainWindow, control: edits_ui::EditControl, v: f32) {
    use edits_ui::EditControl::*;
    let label: slint::SharedString = control.format(v).into();
    match control {
        Exposure => { w.set_edit_exposure(v); w.set_edit_exposure_label(label); }
        Saturation => { w.set_edit_saturation(v); w.set_edit_saturation_label(label); }
        Blacks => { w.set_edit_blacks(v); w.set_edit_blacks_label(label); }
        Whites => { w.set_edit_whites(v); w.set_edit_whites_label(label); }
        Brightness => { w.set_edit_brightness(v); w.set_edit_brightness_label(label); }
        Contrast => { w.set_edit_contrast(v); w.set_edit_contrast_label(label); }
    }
}

fn edits_from_window(w: &MainWindow) -> imgfind::edits::ImageEdits {
    imgfind::edits::ImageEdits {
        exposure: w.get_edit_exposure(),
        saturation: w.get_edit_saturation(),
        blacks: w.get_edit_blacks(),
        whites: w.get_edit_whites(),
        brightness: w.get_edit_brightness(),
        contrast: w.get_edit_contrast(),
    }
    .clamped()
}
```

- [ ] **Step 2: Change `render_edit_preview` to take full edits**

```rust
fn render_edit_preview(
    weak: Weak<MainWindow>,
    base: Arc<Mutex<Option<imgfind::edits::LinearRgb>>>,
    generation: Arc<AtomicU64>,
    edits: imgfind::edits::ImageEdits,
) {
    let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
    let Some(base_img) = base.lock().clone() else { return };
    std::thread::spawn(move || {
        let rgb = base_img.render(&edits.clamped());
        let adjusted = image::DynamicImage::ImageRgb8(rgb);
        slint::invoke_from_event_loop(move || {
            if !is_current_generation(my_gen, generation.load(Ordering::SeqCst)) { return; }
            let Some(w) = weak.upgrade() else { return };
            w.set_lightbox_image(image_util::dynamic_to_slint_image(&adjusted));
        })
        .ok();
    });
}
```

- [ ] **Step 3: Replace the `lb_last_accepted_exposure` state with full edits** (~367)

```rust
let lb_last_accepted_edits: Arc<Mutex<imgfind::edits::ImageEdits>> =
    Arc::new(Mutex::new(imgfind::edits::ImageEdits::identity()));
```

Update every clone/use (`last_accepted_ref`, `last_accepted_a`) to this type.

- [ ] **Step 4: Replace the changed/reset/reset-all handlers** (was `on_edit_exposure_changed` + `on_edit_reset`)

```rust
// edit-control-changed: live-preview a slider move for any control.
{
    let weak = window.as_weak();
    let edit_base_ref = Arc::clone(&lb_edit_base);
    let edit_gen_ref = Arc::clone(&lb_edit_generation);
    window.on_edit_control_changed(move |idx, v| {
        let Some(control) = edits_ui::EditControl::from_i32(idx) else { return };
        let v = control.clamp(v);
        if let Some(w) = weak.upgrade() {
            set_edit_control(&w, control, v);
            render_edit_preview(
                weak.clone(),
                Arc::clone(&edit_base_ref),
                Arc::clone(&edit_gen_ref),
                edits_from_window(&w),
            );
        }
    });
}
// edit-control-reset: reset one control to neutral, then re-render.
{
    let weak = window.as_weak();
    let edit_base_ref = Arc::clone(&lb_edit_base);
    let edit_gen_ref = Arc::clone(&lb_edit_generation);
    window.on_edit_control_reset(move |idx| {
        let Some(control) = edits_ui::EditControl::from_i32(idx) else { return };
        if let Some(w) = weak.upgrade() {
            set_edit_control(&w, control, control.neutral());
            render_edit_preview(
                weak.clone(),
                Arc::clone(&edit_base_ref),
                Arc::clone(&edit_gen_ref),
                edits_from_window(&w),
            );
        }
    });
}
// edit-reset-all: every control to neutral, then re-render.
{
    let weak = window.as_weak();
    let edit_base_ref = Arc::clone(&lb_edit_base);
    let edit_gen_ref = Arc::clone(&lb_edit_generation);
    window.on_edit_reset_all(move || {
        if let Some(w) = weak.upgrade() {
            use edits_ui::EditControl::*;
            for c in [Exposure, Saturation, Blacks, Whites, Brightness, Contrast] {
                set_edit_control(&w, c, c.neutral());
            }
            render_edit_preview(
                weak.clone(),
                Arc::clone(&edit_base_ref),
                Arc::clone(&edit_gen_ref),
                edits_from_window(&w),
            );
        }
    });
}
```

- [ ] **Step 5: Update `on_edit_toggle`** — ENTER seeds all six from stored edits; EXIT restores all six from `last_accepted`.

ENTER (replace the exposure-only seeding ~1455-1465):

```rust
let stored = backend_edit
    .image_edits(&rel)
    .unwrap_or_else(|e| {
        tracing::warn!("edit-mode: failed to read edits for {rel}: {e:#}");
        imgfind::edits::ImageEdits::identity()
    })
    .clamped();
*last_accepted_ref.lock() = stored;
{
    use edits_ui::EditControl::*;
    set_edit_control(&w, Exposure, stored.exposure);
    set_edit_control(&w, Saturation, stored.saturation);
    set_edit_control(&w, Blacks, stored.blacks);
    set_edit_control(&w, Whites, stored.whites);
    set_edit_control(&w, Brightness, stored.brightness);
    set_edit_control(&w, Contrast, stored.contrast);
}
w.set_edit_mode(true);
```

In the decode-thread completion where it calls `render_edit_preview(..., w.get_edit_exposure())`, change to `edits_from_window(&w)`.

EXIT (discard) branch (~1430-1436):

```rust
let restore = *last_accepted_ref.lock();
{
    use edits_ui::EditControl::*;
    set_edit_control(&w, Exposure, restore.exposure);
    set_edit_control(&w, Saturation, restore.saturation);
    set_edit_control(&w, Blacks, restore.blacks);
    set_edit_control(&w, Whites, restore.whites);
    set_edit_control(&w, Brightness, restore.brightness);
    set_edit_control(&w, Contrast, restore.contrast);
}
*edit_base_ref.lock() = None;
edit_gen_ref.fetch_add(1, Ordering::SeqCst);
w.set_edit_mode(false);
```

- [ ] **Step 6: Update `on_edit_accept`** (~1582, 1598-1611) to persist the full edits:

```rust
let edits = edits_from_window(&w);
// ... in the worker thread, replace `ImageEdits { exposure }`:
if let Err(e) = backend_a.save_edits_and_regenerate(&rel, &edits) { /* unchanged */ }
// ... on success:
*last_accepted_a.lock() = edits;
```

(`edits` is `Copy`; move/clone into the thread as the surrounding code does for `exposure`.)

- [ ] **Step 7: Build + test**

Run: `cargo build -p imgfind-gui 2>&1 | tail -25 && cargo test -p imgfind-gui 2>&1 | tail -15`
Expected: builds clean; `edits_ui` tests pass.

- [ ] **Step 8: Commit**

```bash
git add imgfind-gui/src/main.rs
git commit -m "feat(gui): wire six adjustment controls, full-edits live preview, reset-all"
```

---

### Task 7: Workspace verification + docs

**Files:**
- Modify: `CLAUDE.md` (Image adjustments + Native GUI edit-mode descriptions)

- [ ] **Step 1: Full workspace gate**

Run: `cargo test --workspace 2>&1 | tail -25`
Expected: all pass.

Run: `cargo clippy --workspace --all-targets 2>&1 | tail -15`
Expected: no warnings.

Run: `cargo fmt --all --check`
Expected: clean (no output).

- [ ] **Step 2: Manual verification** (via `/run` or `/verify`)

Open the GUI on a library with at least one image: open lightbox → `e` → drag each of the six sliders (live preview updates per slider) → click a per-control `0` button (only that control zeroes) → click `Reset All` (all zero) → `Accept Edits` (spinner, then grid/detail/lightbox show the baked edit) → reopen edit mode (persisted values restored). Press Esc once (leaves edit mode), Esc again (returns to grid).

- [ ] **Step 3: Update `CLAUDE.md`**

In the **Image adjustments (`src/edits.rs`)** bullet, change "Currently exposure only" / "exposure (`× 2^EV`)" wording to: six controls — exposure (EV) and saturation in linear light; blacks, whites, brightness, contrast in display space after gamma; identity (all six neutral) preserves the fast byte-identical path. In the **Native GUI** edit-mode description, replace "an **Exposure** slider (±3 EV)" with the six-slider panel, each row having a per-control reset (`0`) button plus a global **Reset All** and **Accept Edits**; note the Esc-twice behavior (Esc leaves edit mode, a second Esc closes the lightbox). Reference `docs/superpowers/specs/2026-06-26-lightbox-adjustment-controls-design.md`. Update the `image_edits` schema bullet to list the new columns (migration 005; `LATEST_MIGRATION_VERSION = 5`).

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document six-control adjustments + Esc-twice lightbox close"
```

---

## Self-Review

**Spec coverage:**
- Six controls + ranges/pipeline → Tasks 1, 2. ✓
- Per-control reset + global reset → Tasks 5, 6. ✓
- Identity invariant pinned → Tasks 1, 2 (`all_neutral_is_identity`, `render_all_neutral_roundtrips_srgb8`). ✓
- Migration 005 + persistence → Task 3. ✓
- GUI helpers (`EditControl`) → Task 4. ✓
- Slint markup + sidebar resize → Task 5. ✓
- Live preview uses full edits (WYSIWYG) → Task 6 (`edits_from_window` → `render_edit_preview` → `LinearRgb::render`). ✓
- Esc-twice fix → Task 5 (markup `app-keys.focus()`). ✓
- Docs → Task 7. ✓

**Type consistency:** `EditControl` index mapping (0..5) is identical in Task 4 (`to_i32`), Task 5 (markup indices), and Task 6 (`from_i32`). `render_edit_preview` signature changes to `edits: ImageEdits` in Task 6 and all call sites are updated there. `lb_last_accepted_edits` type change is applied to all clones in Task 6.

**Placeholder scan:** No TBD/TODO; every code step shows complete code. The only soft spot is the turso row accessor in Task 3 `column_exists` — flagged inline to match the file's existing idiom (`col_f64`/`table_exists`).
