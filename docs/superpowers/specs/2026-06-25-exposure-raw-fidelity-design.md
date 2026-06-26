# Higher-fidelity exposure + edit-UX polish — design

Date: 2026-06-25
Status: Approved (ship-it --ask)
Follows: `docs/superpowers/specs/2026-06-24-lightbox-image-adjustments-design.md`

## Problem

The exposure editor shipped in the previous iteration has three issues the user hit:

1. **Highlights blow out.** Exposure is `8-bit pixel · 2^EV` hard-clamped to 255, applied
   to the camera's **already-tonemapped embedded JPEG preview** (for RAW). The RAW
   sensor's highlight headroom is never used, so bumping exposure clips highlights
   that a real RAW editor preserves.
2. **No feedback on Accept.** Accepting edits on a large file takes a few seconds
   (thumbnail regeneration) with no indication anything is happening.
3. **Reset is broken.** The Reset button does not move the slider back to neutral.

## Goals / decisions (locked during brainstorming)

- **RAW fidelity:** full sensor demosaic to **linear** float, exposure in linear light
  with a highlight roll-off, then tonemap — genuinely recovers highlight detail. The
  user accepted that regenerating an *edited* RAW is slower (full demosaic); unedited
  browsing stays on the fast preview path.
- **Busy indicator:** a simple animated **spinner + "Saving…/Preparing…"** (the demosaic
  is one blocking step, so a percentage bar would be dishonest).
- **Reset → neutral 0 EV** (matches a RAW editor's "reset to default"); reverting to the
  last *accepted* value remains what Esc / toggle-off does.

Out of scope (YAGNI): additional sliders (contrast/highlights/shadows/WB), curve UI,
per-channel controls, GPU. Exposure stays the only control; the pipeline is structured
so a future scalar adjustment is additive.

## Feasibility (verified against installed `rawler 0.7.2`)

`rawler::imgop::develop` exposes (all public):
- `enum ProcessingStep { Rescale, Demosaic, CropActiveArea, WhiteBalance, Calibrate, CropDefault, SRgb }`
- `struct RawDevelop { pub steps: Vec<ProcessingStep> }` and `develop_intermediate(&RawImage) -> Result<Intermediate>`
- `Intermediate::to_dynamic_image(self) -> Option<DynamicImage>`

`develop_intermediate` runs the steps in order; the float RGB is linear, scene-referred
(sRGB primaries), normalized so sensor black→0 / white→1, **until the final `SRgb` step
applies gamma**. Building a `RawDevelop` whose `steps` **omit `SRgb`** yields linear
float; `to_dynamic_image()` then produces a **linear `Rgb16`** `DynamicImage` (it only
scales f32→u16, no gamma). The camera JPEG's white sits below sensor saturation, so this
linear data holds the highlight detail the preview clipped. This is the unlock.

## Architecture

### New core: a linear edit pipeline (`src/edits.rs` + `src/decode.rs`)

Used **only when `ImageEdits` is non-identity**. Unedited images keep the existing fast
`decode_image`/`decode_full_image` + identity short-circuit path entirely unchanged
(preserves browse speed and the "identity is a no-op" invariant).

1. **`LinearRgb`** — a small owned float buffer:
   ```rust
   pub struct LinearRgb { pub data: Vec<f32>, pub width: u32, pub height: u32 } // RGB, linear, sRGB primaries
   impl LinearRgb {
       pub fn downscale(&self, max_edge: u32) -> LinearRgb; // box/triangle filter in linear
   }
   ```

2. **`decode_linear(path: &Path) -> Result<LinearRgb>`** (in `src/decode.rs`, beside the
   other decode fns):
   - **RAW** (`is_raw_extension`): `decode_raw_linear(path)` — `RawSource` → `get_decoder`
     → `raw_image(&source, &params, false)` → custom
     `RawDevelop { steps: vec![Rescale, Demosaic, CropActiveArea, WhiteBalance, Calibrate, CropDefault] }`
     `.develop_intermediate(&raw)?.to_dynamic_image()` → a **linear `Rgb16`**
     `DynamicImage`. Apply the existing EXIF `apply_exif_orientation` to it (same as the
     other decode paths). Convert `Rgb16` → `LinearRgb` (`u16 / 65535.0`). **Always
     demosaics** (ignores the embedded preview) so highlight headroom is present.
   - **Non-RAW:** `decode_image(path)` (already EXIF-oriented) → `to_rgb8` → per channel
     `srgb_to_linear(u8/255.0)` → `LinearRgb`.

3. **`render_edited(linear: &LinearRgb, edits: &ImageEdits) -> image::RgbImage`** (pure,
   in `src/edits.rs`): for each pixel/channel
   `tonemap_channel(linear_value, ev)`:
   - exposure in linear: `v = linear_value * 2^ev`
   - **soft-knee highlight roll-off** toward a ceiling of 1.0 (knee `KNEE = 0.8`): values
     `≤ KNEE` pass through unchanged; values above compress smoothly so the function is
     continuous and monotonic and asymptotes to 1.0 (never hard-clips). Concretely a
     rational/`hermite` shoulder, e.g. for `v > KNEE`:
     `1 - (1-KNEE) * (1-KNEE) / (v - 2*KNEE + 1)` (continuous value & slope at `v=KNEE`,
     → 1.0 as `v→∞`). Shadows/midtones below the knee are untouched, so exposure stays
     WYSIWYG there.
   - sRGB gamma encode `linear_to_srgb(.)`, scale to `u8` with round + clamp.

   The pure scalar `tonemap_channel(f32, f32) -> u8` and the `srgb_to_linear` /
   `linear_to_srgb` helpers live here and are unit-tested.

### Thumbnail seam (`src/thumbnail.rs`)

`generate_thumbnail_bytes(filepath, spec, edits)` branches on `edits.is_identity()`:
- **identity:** current behavior exactly (`decode_image`/`decode_full_image` →
  `apply_adjustments` no-op → resize/encode). Unchanged.
- **non-identity:** `decode_linear(path)` → `downscale` to the target long edge
  (`ScaleSize(px)` → px; `FullSize` → native, no downscale) **in linear** →
  `render_edited` → JPEG encode. The downscale happens before tonemap (cheaper, and
  resizing in linear is correct).

`apply_adjustments` (old 8-bit multiply) is **removed**; its only callers were this seam
and the old live preview, both replaced by the linear path. The identity short-circuit
moves to the branch above (identity never calls `render_edited`). Its tests are replaced
by `render_edited`/`tonemap_channel` tests.

`regenerate_thumbnails_for_image` is unchanged (it already routes through
`generate_thumbnail_bytes`).

### Live preview (`imgfind-gui/src/backend.rs` + `main.rs`)

- `Backend::decode_lightbox_base` returns a **`LinearRgb`** downscaled to `LIGHTBOX_SIZE`
  (was a tonemapped `DynamicImage`). For RAW this is the slow demosaic.
- Entering edit mode decodes that linear base on a background thread (busy spinner up),
  stores it in `lb_edit_base: Arc<Mutex<Option<LinearRgb>>>`. Each slider tick runs
  `render_edited(&base, &ImageEdits{exposure})` off the UI thread (existing
  generation-guard latest-wins) → `slint::Image` set on the UI thread. Matches the baked
  result exactly (same `render_edited`).
- The exposure multiply now runs on a float buffer; a 2048² RGB f32 base is ~50 MB held
  transiently during edit mode — acceptable.

### Reset fix (`imgfind-gui/ui/app.slint` + `main.rs`)

The `Slider`'s one-way `value: root.edit-exposure` does not reseat the thumb when Rust
writes the property. Fix: make the slider reflect a Rust-owned exposure reliably — bind
the `Slider.value` to `root.edit-exposure` AND ensure Rust sets `edit-exposure` (the
property the slider reads) on reset; if Slint still doesn't reseat the thumb under a pure
`value:` binding, switch to an explicit two-way (`value <=> root.edit-exposure`) or drive
it via a redundant `in property` the slider reads. `edit-reset()` sets exposure to
**0.0**, updates `edit-exposure-label` via `edits_ui::format_exposure(0.0)`, and
re-renders the live preview. Verified by running the GUI (the binding behavior is a Slint
runtime detail — the implementer confirms the thumb actually moves).

### Busy indicator (`imgfind-gui/ui/app.slint` + `main.rs`)

- New `in property <bool> edit-busy;` and `in property <string> edit-busy-label;` on
  `MainWindow`.
- Sidebar shows, when `edit-busy`, an animated spinner (a `Rectangle`/arc with a
  continuous `rotation-angle` `animate` loop — ASCII/no glyphs) plus `edit-busy-label`
  ("Preparing…" on RAW edit-entry decode, "Saving…" on Accept). Reset/Accept TouchAreas
  are disabled (ignore clicks / dimmed) while `edit-busy`.
- Rust sets `edit-busy=true` + label before spawning the background thread (edit-entry
  decode, and accept), clears it in the `invoke_from_event_loop` completion. For non-RAW
  edit-entry the decode is fast; the spinner may barely flash — fine.

## Invariants this feature depends on

- **The thumbnail seam stays single.** All persisted renditions still go through
  `generate_thumbnail_bytes`; the new linear branch lives inside it. Pinned by a test
  asserting an edited (non-identity) thumbnail differs from the unedited rendition (kept
  from the prior feature, still valid).
- **Identity edits never touch the linear path.** `generate_thumbnail_bytes` with
  `ImageEdits::identity()` produces byte-identical output to the pre-change fast path
  (no demosaic, no tonemap). Pinned by a test: identity thumbnail bytes unchanged.
- **Live preview == baked result.** Both call `render_edited` on a `LinearRgb` base of
  the same source; pinned conceptually (same function) — a test renders a small synthetic
  `LinearRgb` through `render_edited` and asserts the anti-blowout property.

## Highlight roll-off — the load-bearing behavior

The core fidelity guarantee is: **a bright (near- or above-white in linear) region pushed
up in exposure must not flatten to a single 255 value — it must retain a monotonic
gradient.** This is the test that must exist (and is exactly the kind of test the project
conventions say not to skip): feed `render_edited` a `LinearRgb` ramp of increasing
highlight values at +1 EV and assert the output is strictly monotonic and `< 255` until
the input is far past the knee (only true saturation reaches 255). Hard-clamp `2^EV`
multiply would fail this; the soft-knee passes.

## Performance

- Unedited browse/index: unchanged (fast preview path; identity short-circuit before any
  demosaic).
- Editing/Accepting a RAW: one full sensor demosaic (seconds for large files), shared
  across all regenerated sizes within a single Accept by decoding `LinearRgb` once and
  downscaling per size. (Implementation note: `regenerate_thumbnails_for_image` may decode
  the linear base once and reuse it across sizes rather than calling
  `generate_thumbnail_bytes` per size — an allowed internal optimization as long as the
  seam invariants hold; if kept simple/per-size, document the redundant decode.)

## Testing summary

- `src/edits.rs` pure: `tonemap_channel` (monotonic, anti-blowout ramp, knee continuity,
  0 EV ≈ identity round-trip via srgb_to_linear∘linear_to_srgb), `srgb_to_linear` /
  `linear_to_srgb` round-trip, `render_edited` on a synthetic `LinearRgb` (brightens,
  preserves highlight gradient, alpha n/a).
- `src/thumbnail.rs`: identity output unchanged vs fast path; non-identity differs;
  (RAW linear-develop exercised via decode tests if a RAW fixture is available, else
  integration/manual).
- GUI: `edits_ui` helpers already tested; live-preview threading + reset + spinner are
  integration- and manually-verified (run the GUI on a RAW).

## Docs

Update `CLAUDE.md`: the exposure edit now uses a **linear, highlight-preserving** pipeline
(RAW: full demosaic via a custom `RawDevelop` without the `SRgb` step → linear → exposure
+ soft-knee roll-off + tonemap; non-RAW: sRGB→linear→same), applied only for non-identity
edits; the live preview and busy spinner; Reset → neutral 0 EV. Link this spec.
