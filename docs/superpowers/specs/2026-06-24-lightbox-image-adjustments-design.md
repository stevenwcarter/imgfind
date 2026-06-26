# Lightbox image adjustments (exposure) — design

Date: 2026-06-24
Status: Approved (ship-it --ask)

## Goal

Let the user adjust the **exposure** of an image from the GUI lightbox. Edits:

- are toggled on with an **"Edit"** button in the lightbox chrome or the **`e`** key
  (the same key toggles edit mode off);
- update the lightbox preview **live** as the slider moves;
- are persisted on **"Accept Edits"** only, and live **only in the database** — the
  original image file is **never modified**;
- apply to **all image types** (RAW and still formats alike — the adjustment is a
  post-decode pixel transform, so the code path is identical);
- on accept, cause **every already-generated thumbnail for that image to be
  regenerated** with the edit baked in.

Out of scope for this iteration (YAGNI): contrast/highlights/shadows/white-balance,
crop/rotate, edit history/undo stack, export-with-edits. The schema and the
adjustment seam are designed so adding another scalar adjustment later is a column +
a slider, nothing structural.

## Exposure semantics

Photographic EV stops. The control ranges **−3.0 .. +3.0 EV**, default **0.0**
(identity). The transform multiplies each RGB channel by `2^EV` in sRGB space and
clamps to `[0, 255]`; alpha is left untouched. sRGB-space multiply is chosen for
simplicity and speed (true linear-light is not worth the complexity here).

## Decisions (locked during brainstorming)

- **Scope:** all image types, not RAW-only (the transform is post-decode).
- **Exposure model:** EV stops, ±3, sRGB-space multiply.
- **Embeddings:** **not** regenerated on accept. Exposure does not meaningfully
  change semantic content; re-embedding is expensive (loads CLIP) and would shift
  search ranking unexpectedly. Search keeps the original vector.
- **Discard model:** explicit **Accept Edits** (persist + regenerate) and **Reset**
  (slider back to last-accepted). Toggling edit mode off (`e` again) or pressing
  **Esc** in edit mode **discards** un-accepted slider movement and reverts the
  preview to the last-accepted state. Only **Accept** writes to the DB. Esc in edit
  mode exits *edit mode only*; a second Esc closes the lightbox.

## Data model & storage

New migration **004** (`src/schema.rs`), `LATEST_MIGRATION_VERSION → 4`:

```sql
CREATE TABLE IF NOT EXISTS image_edits (
    image_id   INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
    exposure   REAL NOT NULL DEFAULT 0.0,   -- EV stops, -3.0 .. +3.0
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
)
```

One row per edited image. **Absence of a row = no edits (identity).** Cascade-delete
keeps it consistent with image removal. The migration runner is idempotent
(`CREATE TABLE IF NOT EXISTS`); follow the existing migration-add pattern
(`migration_004_image_edits`, an `if current < 4 { ... }` block, bump the constant).

### Core type & transform — `src/edits.rs` (new)

```rust
pub struct ImageEdits { pub exposure: f32 }   // exposure in EV stops

impl ImageEdits {
    pub const EXPOSURE_MIN: f32 = -3.0;
    pub const EXPOSURE_MAX: f32 =  3.0;
    pub fn identity() -> Self;        // exposure 0.0
    pub fn is_identity(&self) -> bool; // exposure == 0.0 (within epsilon)
    pub fn clamped(self) -> Self;     // clamp exposure to [MIN, MAX]
}

/// Pure, deterministic. Identity edits return `img` unchanged (no copy work).
pub fn apply_adjustments(img: DynamicImage, edits: &ImageEdits) -> DynamicImage;
```

`apply_adjustments` for a non-identity exposure: factor `f = 2^EV`; for each pixel,
`channel = clamp(round(channel as f32 * f), 0, 255)` on R/G/B, alpha copied. Operate
on an `RgbaImage`/`RgbImage` buffer (match the existing `image_util` conversion
style). Keep it allocation-light but correctness-first; this is unit-tested.

## Where edits get applied (the seam)

Edits are **baked at the thumbnail/decode seam** so every rendition an edited image
produces already contains the adjustment, including renditions generated *after*
accept (e.g. a `FullSize` first decoded on a later zoom).

- `Database` gains `get_image_edits(path: &RelativePath) -> Result<ImageEdits>`
  (returns identity when no row) and `set_image_edits(path, &ImageEdits)` (upsert).
  Internally resolve `path → images.id`, mirroring the existing tag methods.
- `thumbnail::get_or_generate_thumbnail(db, filepath, hash, spec)` already holds
  `db`. On a cache miss it fetches the image's edits (by `filepath`) and passes them
  to generation. `generate_thumbnail_bytes` gains an `edits: &ImageEdits` parameter
  and calls `apply_adjustments(decoded, edits)` **after decode, before resize/encode**.
  This covers grid (300), detail (512), lightbox base (2048), and `FullSize`.
- Batch generation (`generate_missing_thumbnails_batch`) likewise looks up edits per
  image before generating (identity for the common case — short-circuits).

**Known limitation (documented):** the thumbnail cache is content-hash-keyed, so two
byte-identical duplicate files share one baked rendition. Edits are per-image
(per-`image_id`), so editing one duplicate changes the shared thumbnail. Acceptable
for v1 — a pre-existing property of the content-keyed cache; noted in `CLAUDE.md`.

### Invariants this feature depends on

- **Thumbnail generation always routes through `generate_thumbnail_bytes`**, and the
  only persistence path is `get_or_generate_thumbnail` / the batch generator. If a
  new code path decodes-and-caches a thumbnail without going through these, it would
  bypass edits. Pinned by tests that assert a thumbnail generated for an image with a
  non-identity edit differs from the unedited rendition.
- **`apply_adjustments` is identity for `ImageEdits::identity()`** — pinned by a test,
  because every un-edited image relies on it being a no-op (and short-circuit).

## Live preview in the lightbox

Entering edit mode decodes a **fresh, unedited** base on a background thread (via the
existing decode helpers; reuse the 2048 lightbox size) — *not* the baked thumbnail —
so live edits never compound on an already-baked rendition. Each slider tick
re-applies the **full** current edit set to that in-memory base and swaps the pixels
into `lightbox-image`, generation-guarded exactly like the existing full-res swap; no
DB writes. The exposure multiply on a ~2048px image is fast; apply on a worker thread
with latest-wins (reuse the generation/guard pattern in `main.rs`) so the slider
stays responsive.

The pure clamp/step math for the exposure slider (clamping to [−3, +3], readout
formatting) lives in a small testable helper alongside the existing `zoompan.rs`
style (e.g. `imgfind-gui/src/edits_ui.rs`) so it is unit-tested off the UI thread.

## UI (Slint lightbox) — `imgfind-gui/ui/app.slint`

- An **"Edit"** button in the lightbox chrome (bottom bar, near Fit/1:1) and the
  **`e`** key both toggle edit mode. `e` is suppressed while typing, consistent with
  the existing chord handling in the lightbox `FocusScope`.
- Edit mode shows a **right-side sidebar** overlay inside the lightbox: title
  "Adjustments", an **Exposure** `Slider` (minimum −3, maximum 3, value bound to
  `edit-exposure`) with a live numeric readout (e.g. `+1.30 EV`), a **Reset** button
  (slider back to last-accepted value), and an **"Accept Edits"** button at the
  bottom. The image area shrinks to make room for the sidebar (does not overlay the
  image), mirroring the detail-panel reflow approach.
- New `MainWindow` properties: `in-out property <bool> edit-mode`,
  `in-out property <float> edit-exposure`.
- New callbacks: `callback edit-toggle()`, `callback edit-exposure-changed(float)`,
  `callback edit-reset()`, `callback edit-accept()`.
- Use ASCII/Latin-1 glyphs only for any button text (Slint default font tofus symbol
  glyphs — established project constraint).

### Interaction wiring (`imgfind-gui/src/main.rs`)

- `edit-toggle()`: flip `edit-mode`. On enter: set `edit-exposure` from the stored
  edits for the current image, kick off the unedited-base decode, apply current edits
  to the preview. On exit: discard un-accepted changes, reset `edit-exposure` to
  last-accepted, restore the normal (baked) lightbox image.
- `edit-exposure-changed(v)`: clamp via the `edits_ui` helper, update `edit-exposure`,
  re-render the live preview from the in-memory unedited base (latest-wins guard).
- `edit-reset()`: set `edit-exposure` to the last-accepted value and re-render preview.
- `e` key handler in the lightbox `FocusScope` calls `edit-toggle()`; Esc handling
  gains a branch: if `edit-mode`, exit edit mode (discard) instead of closing the
  lightbox.
- Lightbox navigation (prev/next) while in edit mode: simplest correct behavior —
  **exit edit mode (discard) on navigate**, then navigate. (Avoids ambiguous
  cross-image pending edits.) Documented in the plan.

## Accept flow

`edit-accept()` runs on a background thread (existing `block_on` pattern):

1. Clamp and **upsert** the `image_edits` row (`set_image_edits`).
2. Look up every `size` currently cached in `thumbnails` for this image's **hash**,
   and **regenerate each** with edits baked, overwriting via `insert_thumbnail`. Add
   `Database::get_thumbnail_sizes(hash) -> Vec<u32>` (distinct sizes present) to drive
   this; regenerate by calling generation with the freshly-decoded source + edits.
3. **Refresh the in-memory grid LRU** entry for this image so the grid/detail tiles
   show the new pixels immediately (evict the cached decode and re-request the visible
   thumbnail through the normal worker path).
4. Stay in edit mode is **not** required — accept exits edit mode and the normal
   lightbox view now shows the baked (edited) rendition. The `edit-exposure` becomes
   the new last-accepted value.

If no thumbnails exist yet for the image (unlikely in the GUI, since the lightbox
itself generates the 2048), step 2 is a no-op and future generation bakes edits
anyway.

## Testing

Core (`src/edits.rs`, `src/database.rs`, `src/thumbnail.rs`):

- `apply_adjustments`: identity is a no-op (returns equal pixels / short-circuits);
  +EV brightens and clamps a near-white pixel to 255; −EV darkens; mid-gray at +1 EV
  doubles (clamped); alpha preserved on RGBA input; EV at the ±3 boundaries.
- `ImageEdits`: `is_identity`, `clamped` to range, `identity()`.
- `Database`: `set_image_edits` upsert (insert then update same row), `get_image_edits`
  returns identity when absent and the stored value when present;
  `get_thumbnail_sizes` returns the distinct sizes present for a hash.
- `thumbnail`: a thumbnail generated for an image with a non-identity edit differs
  from the unedited rendition (pins the seam invariant); identity edit equals the
  unedited rendition.

GUI helper (`imgfind-gui/src/edits_ui.rs`):

- exposure clamp to [−3, 3], readout formatting (sign, 2 decimals), reset value math.

## Docs

Update `CLAUDE.md`: note the new `image_edits` table + migration 004, the
`src/edits.rs` adjustment seam (edits baked at thumbnail/decode generation, never
mutating originals), the lightbox edit mode (`e` / Edit button, Accept/Reset), and
the content-hash-keyed duplicate limitation. Link this spec.
