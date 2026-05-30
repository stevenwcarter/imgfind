# Cursor-Relative Zoom (TUI) — Design Spec

Date: 2026-05-29
Branch: `tui/cursor-relative-zoom`
Addresses: the `mouse_event`-unused half of audit finding **D7** (`src/tui/app.rs` `ZoomIn`/`ZoomOut`). The unrelated `ui.rs:63` input-cursor `_x` note in D7 is **out of scope** and stays in `bughunt.md`.

## Goal

When the user scrolls the mouse wheel over the zoomed image in the TUI, zoom **toward the cursor** ("anchor under cursor"): the original-image pixel under the cursor stays under the cursor as zoom level changes. Today `ZoomIn`/`ZoomOut` ignore the `MouseEvent` and `zoom_center` always crops the image center.

## Current behavior (verified)

- `App` holds `zoom_level: u8` (clamped 1–4) and renders a single `zoomed_image` overlay via `render_image` (`ui.rs:render_images`).
- `zoom.rs::zoom_center(img, zoom)` crops a `(w/zoom, h/zoom)` window from the **center** and `resize_exact`s back to `(w, h)`.
- Mouse scroll → `AppEvent::ZoomIn(mouse_event)`/`ZoomOut(mouse_event)` → bumps `zoom_level`, calls `handle_zoom_image(self.zoomed_image_index)`; `mouse_event` is dropped.
- The on-screen rect where the zoomed image draws is computed inside `render_image` (the centered `image_area` rect) and discarded.

## Design

### New `App` state
- `zoom_focal: (f32, f32)` — the crop-window **center** in original-image normalized coordinates `[0,1]`. Default `(0.5, 0.5)`.
- `zoomed_image_rect: Option<Rect>` — the on-screen rect of the displayed zoomed image, captured each render. `None` until first zoomed render.

Both initialized in `App::new` (`zoom_focal = (0.5, 0.5)`, `zoomed_image_rect = None`). Requires `use ratatui::layout::Rect` in `app.rs`.

### `zoom_center` gains a focal parameter
`fn zoom_center(img: &DynamicImage, zoom: u8, focal: (f32, f32)) -> DynamicImage`:
- `zoom == 1` → return the full image unchanged (focal irrelevant).
- Else crop size `new_w = w/zoom`, `new_h = h/zoom`. Crop top-left `x = round(focal.0*w - new_w/2)`, `y = round(focal.1*h - new_h/2)`, each clamped to `[0, w-new_w]` / `[0, h-new_h]`. `crop_imm` then `resize_exact(w, h, Lanczos3)` (unchanged filter).

### Pure focal-update helper (the core, TDD'd)
`fn focal_for_zoom_at_cursor(rect: Rect, col: u16, row: u16, old_zoom: u8, old_focal: (f32,f32), new_zoom: u8) -> (f32,f32)`

Works entirely in normalized coordinates (image dims cancel — crop window is `1/zoom` wide/tall in each axis):
1. `new_zoom <= 1` → return `(0.5, 0.5)`.
2. Cursor position within the displayed rect: `u = clamp((col - rect.x)/rect.width, 0, 1)`, `v = clamp((row - rect.y)/rect.height, 0, 1)` (use `saturating_sub`; if `rect.width/height == 0` use `0.5`).
3. Old crop: `s_old = 1/old_zoom`, origin `o_old = clamp(old_focal - s_old/2, 0, 1 - s_old)` per axis.
4. Original point under cursor: `po = o_old + (u,v) * s_old`.
5. New crop: `s_new = 1/new_zoom`. Solve for origin keeping `po` at screen-norm `(u,v)`: `o_new = po - (u,v) * s_new`.
6. New focal = `o_new + s_new/2`, clamped per axis to `[s_new/2, 1 - s_new/2]`.

This is the function unit tests target (center-stays-center, right-edge anchor, repeated-zoom anchor at edge, `new_zoom==1` reset).

### Wiring
- `app.rs` `ZoomIn`/`ZoomOut`: compute `new_zoom` (saturating ±1, clamp 1–4); if `zoomed_image_rect` is `Some`, set `self.zoom_focal = focal_for_zoom_at_cursor(rect, mouse_event.column, mouse_event.row, self.zoom_level, self.zoom_focal, new_zoom)`; set `self.zoom_level = new_zoom`; `handle_zoom_image(self.zoomed_image_index)`.
- `app.rs` `ZoomReset`: set `zoom_level = 1`, `zoom_focal = (0.5, 0.5)`, re-render.
- `app.rs` `ZoomImage(zoom)` arm: reset `zoom_focal = (0.5, 0.5)` before `handle_zoom_image(zoom)` (fresh image select / un-zoom starts centered). Note: scroll handlers call `handle_zoom_image` directly, **not** via `ZoomImage`, so this reset never clobbers a scroll focal.
- `zoom.rs` `handle_zoom_image`: capture `let focal = self.zoom_focal;` before the spawn; pass it to `zoom_center(&base_image, zoom_level, focal)`.
- `ui.rs` `render_image`: return `Result<Rect>` (the centered `center` rect it draws into). Grid callers already pattern-match `Err(_)` and ignore `Ok`, so they're unaffected. `render_images` zoomed branch stores the returned rect into `self.zoomed_image_rect` (compute the `Result` first, then assign after the `as_mut` borrow ends, to satisfy the borrow checker); the grid branch sets `self.zoomed_image_rect = None`.

## Out of scope
- Max zoom level (stays 1–4).
- Keyboard/digit zoom focal (always centered — they carry no cursor).
- The `ui.rs:63` `_x` input-cursor remnant (separate D7 sub-item, left documented).

## Testing
- Unit tests on `focal_for_zoom_at_cursor` (pure): center→center, right/top edge anchor, two-step repeated zoom keeps the edge anchored, `new_zoom==1` → `(0.5,0.5)`, cursor-outside-rect clamps.
- Unit test on `zoom_center` focal clamping: `zoom==1` returns same dimensions; an off-center focal near an edge produces an in-bounds crop (assert output dims == input dims, no panic).
- `cargo build`, `cargo clippy --all-targets`, `cargo test` green. TUI interaction itself is manual.

## Commit convention
TDD per change; commits `feat(tui): <summary>` / `test(tui): ...`. Narrow the D7 entry in `bughunt.md` (remove the resolved `app.rs:143,149 mouse_event` sub-item) in the final wiring commit.
