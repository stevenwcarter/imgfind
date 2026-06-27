# Lightbox adjustment controls — contrast, brightness, blacks, whites, saturation

**Date:** 2026-06-26
**Status:** Approved (brainstorm)
**Supersedes/extends:** `2026-06-24-lightbox-image-adjustments-design.md`,
`2026-06-25-exposure-raw-fidelity-design.md`

## Summary

Expand the lightbox edit mode from a single **Exposure** control to a
Lightroom-Basic-style panel of six non-destructive adjustments:

| Control     | Range        | Neutral | Stage          |
|-------------|--------------|---------|----------------|
| Exposure    | −3…+3 EV     | 0       | linear light   |
| Saturation  | −100…+100    | 0       | linear light   |
| Blacks      | −100…+100    | 0       | display (post-gamma) |
| Whites      | −100…+100    | 0       | display (post-gamma) |
| Brightness  | −100…+100    | 0       | display (post-gamma) |
| Contrast    | −100…+100    | 0       | display (post-gamma) |

Each control gets its **own per-control reset button** (resets that one control
to neutral) in addition to the existing global **Reset** (resets all six) and
**Accept Edits** (persists + rebakes thumbnails). All edits remain
**non-destructive** (baked at the thumbnail-generation seam; the original file is
never modified) and **identity-preserving**: when all six controls are neutral,
`ImageEdits::is_identity()` is `true`, so un-edited images keep the fast
byte-identical decode path.

A separate, smaller fix: pressing **Esc** twice in the lightbox (once to leave
edit mode, once to close the lightbox) must return to the grid. Today the second
Esc is swallowed because leaving edit mode loses keyboard focus.

## Goals

- Six adjustments behaving like Lightroom's Basic panel (region-weighted Blacks/
  Whites with soft falloff; midtone Brightness; pivoted Contrast; chroma
  Saturation), layered on the existing linear-light exposure pipeline.
- Per-control reset + global reset + accept, WYSIWYG live preview.
- Esc-twice closes the lightbox after editing.

## Non-goals

- Tone curve, HSL, white balance, vibrance, local adjustments, histogram.
- Per-control keyboard shortcuts (only the existing `e` toggle + Esc).
- Re-generating embeddings on edit (unchanged: edits never touch embeddings).

## Render pipeline (`src/edits.rs`)

The existing pipeline is exposure (×2^EV) → highlight roll-off → sRGB gamma →
8-bit, anchored in linear light. The new pipeline is a **hybrid**: physically
multiplicative ops stay in linear light; tonal/perceptual ops happen in display
space after gamma, where they look correct.

Per pixel, in order:

**Linear-light stage** (channels `r,g,b` ≥ 0, sensor white ≈ 1.0):
1. **Exposure:** `c *= 2^EV` for each channel (existing).
2. **Saturation:** `Y = 0.2126·r + 0.7152·g + 0.0722·b` (Rec.709 linear luma);
   `f = 1 + saturation/100` (so −100 → `f=0` grayscale, 0 → `f=1`, +100 → `f=2`);
   `c' = (Y + f·(c − Y)).max(0.0)` for each channel.

**Per-channel display stage** (after roll-off + gamma):
3. **Highlight roll-off:** existing soft knee (`HIGHLIGHT_KNEE = 0.8`), unchanged,
   then `.clamp(0,1)`.
4. **Gamma:** `d = linear_to_srgb(c)` → display value `d ∈ [0,1]`.
5. **Blacks:** `d += (blacks/100) · BLACK_STRENGTH · shadow_weight(d)`.
6. **Whites:** `d += (whites/100) · WHITE_STRENGTH · highlight_weight(d)`.
7. **Brightness:** `d = d.powf(2f32.powf(-brightness/100.0))` (midtone gamma;
   endpoints 0 and 1 fixed; +brightness brightens mids).
8. **Contrast:** `d = 0.5 + (d − 0.5) · (1.0 + contrast/100.0)` (linear pivot at
   mid-gray; factor ∈ [0,2]).
9. **Output:** `(d.clamp(0,1) · 255).round() as u8`.

Weights (smoothstep `s(a,b,x) = t·t·(3−2t)`, `t = clamp((x−a)/(b−a), 0, 1)`):
- `shadow_weight(d) = 1.0 − s(0.0, 0.5, d)` — 1 in deep shadow, 0 by mid-gray.
- `highlight_weight(d) = s(0.5, 1.0, d)` — 0 below mid, 1 at white.

Constants: `BLACK_STRENGTH = 0.5`, `WHITE_STRENGTH = 0.5` (full-slider shift of
±0.5 display units at the extreme, tapering to 0 across the midtones).

### Function decomposition (all pure, all unit-tested)

- `apply_saturation(r, g, b, saturation) -> (f32, f32, f32)` (linear).
- `shadow_weight(d) -> f32`, `highlight_weight(d) -> f32`, `smoothstep(a,b,x) -> f32`.
- `apply_blacks(d, blacks)`, `apply_whites(d, whites)`, `apply_brightness(d,
  brightness)`, `apply_contrast(d, contrast)` — each `f32 -> f32` on display `d`.
- `channel_to_display(linear_exposed: f32, edits: &ImageEdits) -> u8` — chains
  roll-off → gamma → blacks → whites → brightness → contrast → 8-bit (no exposure;
  exposure and saturation are applied at the pixel level before this).
- `tonemap_channel(linear, ev) -> u8` is **retained** as a thin wrapper
  (`channel_to_display(linear · 2^ev, &ImageEdits::identity())` — exposure is
  pre-multiplied into the argument and the display controls are all neutral), so
  the existing `linear_tests` keep passing unchanged.
- `LinearRgb::render(&ImageEdits) -> RgbImage` orchestrates the per-pixel loop:
  exposure → saturation → per-channel `channel_to_display`.

### Tests (`src/edits.rs`)

Keep all existing tests. Add:
- `all_neutral_is_identity` — `ImageEdits` with every field 0 → `is_identity()`.
- `render_all_neutral_roundtrips_srgb8` — sRGB8 → linear → `render(neutral)` ≈ input
  (the byte-identical-path guarantee, pinned even though the fast path bypasses it).
- Per control, on a mid-gray patch: **direction** (e.g. +contrast widens the gap
  between a light and dark sample; +brightness raises mids; +saturation increases
  channel spread; −saturation → all channels equal; +blacks lifts a shadow sample;
  +whites lifts a highlight sample) and **neutral = no-op** (value 0 leaves the
  pixel unchanged within ±1/255).
- `apply_contrast`/`apply_brightness`/`apply_blacks`/`apply_whites` **monotonic**
  in `d` and **fix endpoints** where applicable (brightness/contrast keep 0→0,
  1→1 only for brightness; contrast maps 0.5→0.5).
- `shadow_weight`/`highlight_weight` are 1/0 at their anchors and in `[0,1]`.

## Data model

`ImageEdits` (`src/edits.rs`) gains five `f32` fields:
`contrast, brightness, blacks, whites, saturation` (all default 0.0). Update:
- `identity()` — all zero.
- `is_identity()` — every field `< f32::EPSILON` in absolute value.
- `clamped()` — exposure to ±3; the other five to ±100.
- Add `const ADJ_MIN: f32 = -100.0; const ADJ_MAX: f32 = 100.0;`.

### Migration 005 (`src/schema.rs`)

`migration_005_edit_controls` runs five `ALTER TABLE image_edits ADD COLUMN <x>
REAL NOT NULL DEFAULT 0.0` (contrast, brightness, blacks, whites, saturation).
Bump `LATEST_MIGRATION_VERSION = 5` and add the gated call
(`if current < 5 { migration_005… }`) before the version stamp. The runner is
strictly version-gated, so the non-idempotent `ADD COLUMN` runs exactly once.
Add a test asserting the new columns exist after `run_migrations` (and that the
existing idempotency test still passes — second `run_migrations` is a no-op).

### `database.rs`

- `get_image_edits` — `SELECT e.exposure, e.contrast, e.brightness, e.blacks,
  e.whites, e.saturation …`; build all six fields; keep `.clamped()`.
- `set_image_edits` — extend the INSERT column list, the `SELECT … ?n` params, and
  the `ON CONFLICT … DO UPDATE SET` list to all six columns.
- Extend the existing `image_edits_upsert_and_read` test to round-trip all six
  fields (set non-neutral values, read back, assert each).

## GUI

### Pure helpers (`imgfind-gui/src/edits_ui.rs`)

Introduce an `EditControl` enum to key the generic Slint callbacks and centralize
per-control range/format:

```rust
pub enum EditControl { Exposure, Saturation, Blacks, Whites, Brightness, Contrast }
```

- `from_i32(i32) -> Option<EditControl>` / `to_i32`.
- `clamp(self, v: f32) -> f32` — Exposure → ±3; others → ±100.
- `neutral(self) -> f32` — 0.0 for all (kept as a method for clarity/future-proofing).
- `format(self, v: f32) -> String` — Exposure → existing `"+1.30 EV"` form; the
  other five → signed integer-ish `"+45"`, `"0"`, `"-30"` (no unit).
- Keep `clamp_exposure`/`format_exposure` (delegate to `EditControl::Exposure`)
  so existing call sites and tests are untouched.
- Unit tests: `clamp` per control hits the right bounds; `format` matches the
  expected strings (EV vs. unit-less); `from_i32`/`to_i32` round-trip.

### Slint markup (`imgfind-gui/ui/app.slint`)

Per control, add a property + label property:
`edit-contrast/-brightness/-blacks/-whites/-saturation : float` and their
`-label : string`. Replace the per-control changed/reset callbacks with two
**generic** callbacks keyed by control index:
`callback edit-control-changed(int, float);` and `callback edit-control-reset(int);`
(Exposure migrates onto the same pair; the legacy `edit-exposure-changed`/
`edit-reset` names are removed or re-pointed.) The global reset + accept keep
their own callbacks (`edit-reset-all()`, `edit-accept()`).

Build the six rows from a single reusable sub-component, e.g.
`AdjustRow { in property index; in property label; in property value-label;
in-out property <float> value; in property min; in property max; }`, laid out as:

```
HorizontalLayout {                      // one row per control
  reset-btn (≈30px, ASCII-safe text)    // per-control reset → edit-control-reset(index)
  VerticalLayout {                       // stretch
    Text  { text: label + ": " + value-label; }
    Slider { minimum: min; maximum: max; value <=> root.<prop>;
             changed(v) => root.edit-control-changed(index, v); }
  }
}
```

The two-way `value <=> root.<prop>` binding is what lets a Rust write (per-control
or global reset) physically reseat the thumb, mirroring today's Exposure Reset.
The per-control reset button text must be **ASCII/Latin-1 only** (the default
Slint font tofus symbol glyphs — see memory `slint-default-font-glyph-coverage`);
use a compact label such as `"0"` (reset-to-zero) rather than a ↺ glyph.

Widen the sidebar from 240px to **280px** so each row fits `[reset] [label/slider]`;
the lightbox content area already reflows by the sidebar width, so update both the
sidebar `width` and the `parent.width - (edit-mode ? 280px : 0px)` content shrink.
Reuse the existing busy spinner / Accept button verbatim. All controls are gated
by `edit-busy` (clicks ignored + dimmed) exactly like Reset/Accept today.

### Esc fix (`app.slint`)

In the lightbox Esc branch, when `edit-mode` is true, after `root.edit-toggle()`
call `app-keys.focus()` so keyboard focus returns to the capture FocusScope.
Today, leaving edit mode removes the sidebar (and the focused Slider) from the
tree, dropping focus to null, so the next Esc reaches no FocusScope. Restoring
focus makes the second Esc close the lightbox.

```slint
if (root.edit-mode) {
    root.edit-toggle();
    app-keys.focus();        // <-- the fix
} else {
    root.lightbox-close();
}
```

### Wiring (`imgfind-gui/src/main.rs`, `backend.rs`)

- `edits_from_window(&MainWindow) -> ImageEdits` — read all six window properties
  into an `ImageEdits` (clamped). Used by the live preview and Accept so the
  whole control set is honored, not just exposure.
- `set_edit_control(&MainWindow, EditControl, f32)` — set the matching property +
  its formatted label (generalizes today's `set_edit_exposure`).
- `render_edit_preview(...)` now takes the full `&ImageEdits` (gathered via
  `edits_from_window`) instead of a lone `exposure: f32`; the generation guard,
  background thread, and latest-wins logic are unchanged. The render uses the same
  `LinearRgb::render` the thumbnails bake → WYSIWYG.
- `on_edit_control_changed(idx, v)` — resolve `EditControl::from_i32`, clamp,
  `set_edit_control`, then `render_edit_preview(edits_from_window(...))`.
- `on_edit_control_reset(idx)` — set that control to its neutral, then re-render
  (same path as a slider change, so a slow in-flight multiply can't land after).
- `on_edit_reset_all()` — set every control to neutral, then re-render (replaces
  today's exposure-only `edit-reset`).
- `on_edit_toggle` ENTER — read stored `ImageEdits`, seed every slider+label via
  `set_edit_control`, store `last_accepted` as the **full** `ImageEdits` (the
  Reset/Accept discard baseline becomes the whole struct, not just exposure).
  EXIT (discard) — restore every control from `last_accepted`.
- `on_edit_accept` — persist `edits_from_window(...)` (all six) via
  `save_edits_and_regenerate`; rebake + evict unchanged.
- `lb_last_accepted_exposure: Mutex<f32>` becomes
  `lb_last_accepted_edits: Mutex<ImageEdits>`.

No `backend.rs` signature changes are required beyond `image_edits`/
`save_edits_and_regenerate` already taking a full `ImageEdits` (they do).

## Verification

- `cargo test --workspace` — covers edits math, DB round-trip, migration, edits_ui.
- `cargo clippy --workspace --all-targets` and `cargo fmt --all --check` clean.
- Manual (via `/run` or `/verify`): open lightbox → `e` → drag each slider (live
  preview updates) → per-control reset zeroes just that control → global Reset
  zeroes all → Accept rebakes (grid/detail/lightbox reflect the edit) → reopen
  shows persisted values. Esc once leaves edit mode, Esc again returns to grid.

## Invariants this feature depends on

- **Neutral edits ⇒ identity ⇒ fast byte-identical thumbnail path.** Pinned by
  `all_neutral_is_identity` + `render_all_neutral_roundtrips_srgb8`. Any future
  control added to `ImageEdits` must extend `is_identity()` and these tests, or an
  un-edited image would silently route through the slow linear pipeline.
- **Edits are baked only at the thumbnail seam; the original file is never
  written.** Unchanged from the prior specs.
- **The live preview uses the same `LinearRgb::render` as the bake**, so the
  preview and the accepted rendition match (WYSIWYG). Any divergence between the
  preview path and the bake path breaks this.
