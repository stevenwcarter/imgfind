# Cursor-Relative Zoom (TUI) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make mouse-wheel zoom in the TUI anchor on the cursor — the original-image pixel under the cursor stays under the cursor as zoom changes.

**Architecture:** A pure normalized-coordinate helper (`focal_for_zoom_at_cursor`) computes the new crop-window center from the cursor position and zoom transition; `zoom_center` crops around that focal point; `App` carries `zoom_focal` + the captured on-screen `zoomed_image_rect`; the scroll handlers feed the cursor in.

**Tech Stack:** Rust (edition 2024), ratatui 0.29, ratatui-image, crossterm `MouseEvent`, the `image` crate. Verify with `cargo build`, `cargo clippy --all-targets`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-05-29-cursor-relative-zoom-design.md`. Build green after every task. Commits `feat(tui)`/`test(tui)`.

**File map:**
- `src/tui/app/zoom.rs` — `zoom_center` (gains focal), new pure `focal_for_zoom_at_cursor` + its tests, `handle_zoom_image` passes focal.
- `src/tui/app.rs` — `App` struct fields, `App::new`, the `ZoomIn`/`ZoomOut`/`ZoomReset`/`ZoomImage` handler arms.
- `src/tui/ui.rs` — `render_image` returns `Result<Rect>`; `render_images` captures the rect.
- `src/tui/widget/image.rs` — `render_image` lives here (returns `Result<Rect>`).
- `bughunt.md` — narrow D7.

> Note on `render_image`: it is defined in `src/tui/widget/image.rs` and re-exported via `src/tui/widget/mod.rs` as `render_image`. `ui.rs` calls it. Confirm the exact location with `grep -rn "fn render_image" src/tui/` before editing.

---

## Task 1: Pure focal helper `focal_for_zoom_at_cursor`

**Files:**
- Modify: `src/tui/app/zoom.rs` (add the function near the top, after the existing `use` lines and before `zoom_center`, plus a `#[cfg(test)] mod tests` at the bottom)

- [ ] **Step 1: Add the imports needed for the helper + tests**

At the top of `src/tui/app/zoom.rs`, ensure `ratatui::layout::Rect` is imported. The current imports are:
```rust
use image::{DynamicImage, GenericImageView, ImageReader};
use ratatui_image::{FilterType, thread::ThreadProtocol};
use tokio::sync::mpsc::unbounded_channel;
use tracing::{debug, warn};
```
Add this line:
```rust
use ratatui::layout::Rect;
```

- [ ] **Step 2: Write the failing tests**

Add at the bottom of `src/tui/app/zoom.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn rect() -> Rect {
        Rect {
            x: 10,
            y: 5,
            width: 100,
            height: 50,
        }
    }

    fn approx(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-4, "expected {b}, got {a}");
    }

    #[test]
    fn new_zoom_one_resets_to_center() {
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 60, 30, 2, (0.75, 0.25), 1);
        approx(fx, 0.5);
        approx(fy, 0.5);
    }

    #[test]
    fn cursor_at_center_keeps_center() {
        // cursor at the rect center, zooming 1 -> 2, focal was centered
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 60, 30, 1, (0.5, 0.5), 2);
        approx(fx, 0.5);
        approx(fy, 0.5);
    }

    #[test]
    fn cursor_at_right_edge_anchors_right() {
        // cursor at far right/bottom of rect (u=v=1), zoom 1 -> 2
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 110, 55, 1, (0.5, 0.5), 2);
        // s_new = 0.5 -> focal clamps to [0.25, 0.75]; right/bottom anchor -> 0.75
        approx(fx, 0.75);
        approx(fy, 0.75);
    }

    #[test]
    fn repeated_zoom_keeps_right_edge_anchored() {
        // after the previous step we are at zoom 2, focal 0.75; scroll again at right edge to zoom 3
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 110, 55, 2, (0.75, 0.75), 3);
        // s_new = 1/3 -> focal clamps to [1/6, 5/6]; right anchor -> 5/6
        approx(fx, 5.0 / 6.0);
        approx(fy, 5.0 / 6.0);
    }

    #[test]
    fn cursor_left_of_rect_clamps_to_left() {
        // cursor column/row below rect origin -> u=v=0
        let (fx, fy) = focal_for_zoom_at_cursor(rect(), 0, 0, 1, (0.5, 0.5), 2);
        // left/top anchor -> focal clamps to 0.25
        approx(fx, 0.25);
        approx(fy, 0.25);
    }

    #[test]
    fn zero_width_rect_uses_center_axis() {
        let r = Rect { x: 0, y: 0, width: 0, height: 50 };
        let (fx, _fy) = focal_for_zoom_at_cursor(r, 5, 25, 1, (0.5, 0.5), 2);
        // width 0 -> u defaults to 0.5 -> fx stays 0.5
        approx(fx, 0.5);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail to compile**

Run: `cargo test focal_for_zoom_at_cursor`
Expected: FAIL — `cannot find function focal_for_zoom_at_cursor`.

- [ ] **Step 4: Implement the helper**

Add this function in `src/tui/app/zoom.rs` (above `zoom_center`):
```rust
/// Compute the new crop-window center (focal point, in original-image normalized
/// [0,1] coordinates) so that the pixel currently under the cursor stays under the
/// cursor when zooming from `old_zoom` to `new_zoom`.
///
/// `rect` is the on-screen rect the zoomed image is drawn into; `col`/`row` are the
/// cursor's terminal cell. Works in normalized coordinates, so image dimensions cancel.
pub fn focal_for_zoom_at_cursor(
    rect: Rect,
    col: u16,
    row: u16,
    old_zoom: u8,
    old_focal: (f32, f32),
    new_zoom: u8,
) -> (f32, f32) {
    if new_zoom <= 1 {
        return (0.5, 0.5);
    }

    let u = if rect.width == 0 {
        0.5
    } else {
        (col.saturating_sub(rect.x) as f32 / rect.width as f32).clamp(0.0, 1.0)
    };
    let v = if rect.height == 0 {
        0.5
    } else {
        (row.saturating_sub(rect.y) as f32 / rect.height as f32).clamp(0.0, 1.0)
    };

    let s_old = 1.0 / old_zoom.max(1) as f32;
    let s_new = 1.0 / new_zoom as f32;

    let (fx_old, fy_old) = old_focal;
    let o_old_x = (fx_old - s_old / 2.0).clamp(0.0, 1.0 - s_old);
    let o_old_y = (fy_old - s_old / 2.0).clamp(0.0, 1.0 - s_old);

    // Original-image point currently under the cursor.
    let po_x = o_old_x + u * s_old;
    let po_y = o_old_y + v * s_old;

    // New crop origin that keeps that point at the same screen position (u, v).
    let o_new_x = po_x - u * s_new;
    let o_new_y = po_y - v * s_new;

    let fx_new = (o_new_x + s_new / 2.0).clamp(s_new / 2.0, 1.0 - s_new / 2.0);
    let fy_new = (o_new_y + s_new / 2.0).clamp(s_new / 2.0, 1.0 - s_new / 2.0);

    (fx_new, fy_new)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test focal_for_zoom_at_cursor`
Expected: all 6 PASS.

- [ ] **Step 6: Build + clippy**

Run: `cargo build && cargo clippy --all-targets`
Expected: clean (pre-existing `mouse_event` unused warnings in `tui/app.rs` are fine — they're resolved in Task 5).

- [ ] **Step 7: Commit**

```bash
git add src/tui/app/zoom.rs
git commit -m "feat(tui): add pure focal_for_zoom_at_cursor helper [D7]"
```

---

## Task 2: `zoom_center` takes a focal point

**Files:**
- Modify: `src/tui/app/zoom.rs` (`zoom_center` signature + body; add a test)

- [ ] **Step 1: Write the failing test**

Add these two tests inside the existing `#[cfg(test)] mod tests` block in `src/tui/app/zoom.rs`:
```rust
    #[test]
    fn zoom_one_returns_same_dimensions() {
        let img = DynamicImage::new_rgb8(64, 48);
        let out = zoom_center(&img, 1, (0.5, 0.5));
        assert_eq!(out.dimensions(), (64, 48));
    }

    #[test]
    fn off_center_focal_stays_in_bounds() {
        // A focal at the corner must not panic and must keep output dims == input dims.
        let img = DynamicImage::new_rgb8(64, 48);
        let out = zoom_center(&img, 3, (0.95, 0.05));
        assert_eq!(out.dimensions(), (64, 48));
    }
```
(`DynamicImage` and `GenericImageView` — for `.dimensions()` — are already imported at the top of the file.)

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test zoom_center 2>&1 | head -20` (or `cargo test off_center_focal_stays_in_bounds`)
Expected: FAIL to compile — `zoom_center` takes 2 args, not 3.

- [ ] **Step 3: Update `zoom_center`**

Replace the existing function:
```rust
pub fn zoom_center(img: &DynamicImage, zoom: u8) -> DynamicImage {
    let zoom = zoom.clamp(1, 4);
    let (w, h) = img.dimensions();

    let new_w = (w as f32 / zoom as f32) as u32;
    let new_h = (h as f32 / zoom as f32) as u32;

    let x = (w - new_w) / 2;
    let y = (h - new_h) / 2;

    let cropped = img.crop_imm(x, y, new_w, new_h);

    cropped.resize_exact(w, h, FilterType::Lanczos3)
}
```
with:
```rust
pub fn zoom_center(img: &DynamicImage, zoom: u8, focal: (f32, f32)) -> DynamicImage {
    let zoom = zoom.clamp(1, 4);
    let (w, h) = img.dimensions();

    if zoom == 1 {
        return img.clone();
    }

    let new_w = (w as f32 / zoom as f32) as u32;
    let new_h = (h as f32 / zoom as f32) as u32;

    // Crop top-left so the window is centered on the focal point, clamped in bounds.
    let cx = focal.0 * w as f32;
    let cy = focal.1 * h as f32;
    let x = (cx - new_w as f32 / 2.0)
        .round()
        .clamp(0.0, (w - new_w) as f32) as u32;
    let y = (cy - new_h as f32 / 2.0)
        .round()
        .clamp(0.0, (h - new_h) as f32) as u32;

    let cropped = img.crop_imm(x, y, new_w, new_h);

    cropped.resize_exact(w, h, FilterType::Lanczos3)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib zoom`
Expected: the two new tests PASS. (The call site in `handle_zoom_image` still passes 2 args and will now fail to COMPILE — that's expected and fixed in Task 5. To keep this task's build green, also do Step 5 below in the same commit.)

- [ ] **Step 5: Update the single call site so the crate compiles**

In `src/tui/app/zoom.rs`, `handle_zoom_image` currently calls:
```rust
                    let image = zoom_center(&base_image, zoom_level);
```
Change it to pass the captured focal (the `App` field is added in Task 3; for now thread it through a local captured before the spawn). Replace the line that captures locals before `tokio::spawn` — find:
```rust
                let zoom_tx = self.zoom_tx.clone();
                let picker = self.picker.clone();
```
and add a focal capture right after it:
```rust
                let zoom_tx = self.zoom_tx.clone();
                let picker = self.picker.clone();
                let focal = self.zoom_focal;
```
Then inside the spawned task change:
```rust
                    let image = zoom_center(&base_image, zoom_level);
```
to:
```rust
                    let image = zoom_center(&base_image, zoom_level, focal);
```

> This references `self.zoom_focal`, which does not exist yet. **Do Task 3 (App field) before compiling/committing this task.** Because Tasks 2 and 3 are mutually dependent for compilation, implement Task 3's struct field + `App::new` initializer now (it's a 2-line addition shown in Task 3) so the crate compiles, then commit Tasks 2+3 together. If you prefer strict separation, instead temporarily use `(0.5, 0.5)` for `focal` here, commit Task 2, then replace with `self.zoom_focal` in Task 3. Pick one; do not leave the crate uncompilable between commits.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app/zoom.rs src/tui/app.rs
git commit -m "feat(tui): zoom_center crops around a focal point [D7]"
```

---

## Task 3: `App` state — `zoom_focal` and `zoomed_image_rect`

**Files:**
- Modify: `src/tui/app.rs` (struct definition + `App::new`)

> If you already added `zoom_focal` while doing Task 2, just add `zoomed_image_rect` here and fold into the same commit.

- [ ] **Step 1: Import `Rect` in `app.rs`**

`src/tui/app.rs` imports ratatui types. Add `Rect` — find the ratatui import block:
```rust
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
};
```
Change to:
```rust
use ratatui::{
    DefaultTerminal,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};
```

- [ ] **Step 2: Add the struct fields**

In the `pub struct App { ... }` definition, add these two fields (e.g. just after `pub zoom_level: u8,`):
```rust
    /// Crop-window center for the zoomed image, in original-image normalized [0,1] coords.
    pub zoom_focal: (f32, f32),
    /// On-screen rect of the displayed zoomed image, captured during render.
    pub zoomed_image_rect: Option<Rect>,
```

- [ ] **Step 3: Initialize them in `App::new`**

In `App::new`, the returned `Self { ... }` literal sets `zoom_level: 1,` among others. Add:
```rust
            zoom_focal: (0.5, 0.5),
            zoomed_image_rect: None,
```

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles (now that `self.zoom_focal` from Task 2 resolves). Pre-existing `mouse_event` warnings remain (fixed in Task 5).

- [ ] **Step 5: Commit (if not already folded into Task 2)**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add zoom_focal and zoomed_image_rect to App [D7]"
```

---

## Task 4: `render_image` returns the drawn rect; capture it

**Files:**
- Modify: `src/tui/widget/image.rs` (`render_image` returns `Result<Rect>`)
- Modify: `src/tui/ui.rs` (`render_images` stores the rect)

- [ ] **Step 1: Change `render_image` return type**

In `src/tui/widget/image.rs`, the function is:
```rust
pub fn render_image(
    index: u8,
    focused_image_index: u8,
    image_entry: &mut ImageEntry,
    area: Rect,
    buf: &mut Buffer,
) -> Result<()> {
    let image_area = image_entry
        .protocol
        .size_for(Resize::Scale(None), area)
        .context("could not find size for image")?;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    if index == focused_image_index {
        block.render(area, buf);
    }
    let center = center(
        inner,
        Constraint::Length(image_area.width),
        Constraint::Length(image_area.height),
    );
    let image = StatefulImage::new().resize(Resize::Scale(Some(FilterType::CatmullRom)));
    image.render(center, buf, &mut image_entry.protocol);

    Ok(())
}
```
Change the return type `-> Result<()>` to `-> Result<Rect>` and the final `Ok(())` to `Ok(center)`:
```rust
) -> Result<Rect> {
```
…and:
```rust
    let image = StatefulImage::new().resize(Resize::Scale(Some(FilterType::CatmullRom)));
    image.render(center, buf, &mut image_entry.protocol);

    Ok(center)
}
```

- [ ] **Step 2: Update `render_images` in `ui.rs` to capture the rect**

In `src/tui/ui.rs`, the `render_images` method currently is:
```rust
    fn render_images(&mut self, area: Rect, buf: &mut Buffer) {
        if let Some(image) = self.zoomed_image.as_mut() {
            Clear.render(area, buf);
            if let Err(e) = render_image(0, 9, image, area, buf) {
                error!("Failed to render zoomed image: {}", e);
            }
        } else {
            let nines = nine_block(area);

            for (index, area) in nines.into_iter().enumerate() {
```
Replace the `if let Some(image) = ... { ... } else {` opening (through the `let nines` line) with:
```rust
    fn render_images(&mut self, area: Rect, buf: &mut Buffer) {
        if self.zoomed_image.is_some() {
            Clear.render(area, buf);
            let result = render_image(0, 9, self.zoomed_image.as_mut().unwrap(), area, buf);
            match result {
                Ok(rect) => self.zoomed_image_rect = Some(rect),
                Err(e) => error!("Failed to render zoomed image: {}", e),
            }
        } else {
            self.zoomed_image_rect = None;
            let nines = nine_block(area);

            for (index, area) in nines.into_iter().enumerate() {
```
(The rest of the `else` body — the mouse-click hit-test and the grid `render_image` loop — is unchanged. The grid loop's `if let Err(e) = render_image(...)` still works because `Result<Rect>` still pattern-matches `Err(e)`.)

- [ ] **Step 3: Build + clippy**

Run: `cargo build && cargo clippy --all-targets`
Expected: clean compile. If clippy flags the grid loop's `let Err(e) = render_image(...)` as "unused `Ok` value", that's not an error; leave it. Pre-existing `mouse_event` warnings remain (fixed next task).

- [ ] **Step 4: Commit**

```bash
git add src/tui/widget/image.rs src/tui/ui.rs
git commit -m "feat(tui): capture displayed zoomed-image rect during render [D7]"
```

---

## Task 5: Wire scroll/reset/select handlers to the focal point

**Files:**
- Modify: `src/tui/app.rs` (`ZoomIn`, `ZoomOut`, `ZoomReset`, `ZoomImage` arms)

Context — the current arms in `handle_event`:
```rust
                AppEvent::ZoomIn(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        self.zoom_level = self.zoom_level.saturating_add(1).clamp(1, 4);
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
                AppEvent::ZoomOut(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        self.zoom_level = self.zoom_level.saturating_sub(1).clamp(1, 4);
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
                AppEvent::ZoomReset => {
                    self.zoom_level = 1;
                    self.handle_zoom_image(self.zoomed_image_index);
                }
```
…and later:
```rust
                AppEvent::ZoomImage(zoom) => {
                    self.handle_zoom_image(zoom);
                }
```

- [ ] **Step 1: Rewrite `ZoomIn` to use the cursor**

Replace the `ZoomIn` arm with:
```rust
                AppEvent::ZoomIn(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        let new_zoom = self.zoom_level.saturating_add(1).clamp(1, 4);
                        if let Some(rect) = self.zoomed_image_rect {
                            self.zoom_focal = crate::tui::app::zoom::focal_for_zoom_at_cursor(
                                rect,
                                mouse_event.column,
                                mouse_event.row,
                                self.zoom_level,
                                self.zoom_focal,
                                new_zoom,
                            );
                        }
                        self.zoom_level = new_zoom;
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
```

> The path `crate::tui::app::zoom::focal_for_zoom_at_cursor` assumes the `zoom` submodule is reachable. `app.rs` already declares `mod zoom;` (private). To call the helper from within `app.rs`, either (a) refer to it as `zoom::focal_for_zoom_at_cursor(...)` since `mod zoom;` is in scope in `app.rs`, or (b) add `use zoom::focal_for_zoom_at_cursor;` near the other `use` lines. **Use option (b):** add `use zoom::focal_for_zoom_at_cursor;` to `app.rs`'s imports and then call `focal_for_zoom_at_cursor(...)` unqualified in all three arms below. Verify `mod zoom;` exists in `app.rs` (it's declared alongside `mod focus; mod input; mod search;`).

Given option (b), the `ZoomIn` arm body becomes:
```rust
                AppEvent::ZoomIn(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        let new_zoom = self.zoom_level.saturating_add(1).clamp(1, 4);
                        if let Some(rect) = self.zoomed_image_rect {
                            self.zoom_focal = focal_for_zoom_at_cursor(
                                rect,
                                mouse_event.column,
                                mouse_event.row,
                                self.zoom_level,
                                self.zoom_focal,
                                new_zoom,
                            );
                        }
                        self.zoom_level = new_zoom;
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
```

- [ ] **Step 2: Add the import**

Near the top of `src/tui/app.rs`, where submodule re-exports live (the file already has `pub use focus::FocusDirection;` etc.), add:
```rust
use zoom::focal_for_zoom_at_cursor;
```

- [ ] **Step 3: Rewrite `ZoomOut` to use the cursor**

Replace the `ZoomOut` arm with:
```rust
                AppEvent::ZoomOut(mouse_event) => {
                    if self.zoomed_image.is_some() {
                        let new_zoom = self.zoom_level.saturating_sub(1).clamp(1, 4);
                        if let Some(rect) = self.zoomed_image_rect {
                            self.zoom_focal = focal_for_zoom_at_cursor(
                                rect,
                                mouse_event.column,
                                mouse_event.row,
                                self.zoom_level,
                                self.zoom_focal,
                                new_zoom,
                            );
                        }
                        self.zoom_level = new_zoom;
                        self.handle_zoom_image(self.zoomed_image_index);
                    }
                }
```

- [ ] **Step 4: Reset focal on `ZoomReset`**

Replace the `ZoomReset` arm with:
```rust
                AppEvent::ZoomReset => {
                    self.zoom_level = 1;
                    self.zoom_focal = (0.5, 0.5);
                    self.handle_zoom_image(self.zoomed_image_index);
                }
```

- [ ] **Step 5: Reset focal on fresh image select / un-zoom**

Replace the `ZoomImage` arm with:
```rust
                AppEvent::ZoomImage(zoom) => {
                    self.zoom_focal = (0.5, 0.5);
                    self.handle_zoom_image(zoom);
                }
```

- [ ] **Step 6: Build + clippy + test**

Run: `cargo build && cargo clippy --all-targets && cargo test`
Expected: clean compile — the previously-unused `mouse_event` bindings in `ZoomIn`/`ZoomOut` are now used, so those two warnings disappear. All tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): zoom toward cursor on scroll, reset focal on select/reset [D7]"
```

---

## Task 6: Narrow the D7 finding in `bughunt.md`

**Files:**
- Modify: `bughunt.md`

- [ ] **Step 1: Edit the D7 entry**

The current D7 entry reads:
```markdown
### D7 — Unused vars/params `[LOW]`
`src/tui/app.rs:143,149` — `mouse_event` unused in `ZoomIn`/`ZoomOut`. `src/tui/ui.rs:63` — `_x` cursor calc computed but never applied (cursor position not set).
```
Replace it with (the `mouse_event` half is now resolved; keep only the still-open `ui.rs` half):
```markdown
### D7 — Unused vars/params `[LOW]`
`src/tui/ui.rs:63` — `_x` cursor calc computed but never applied (cursor position not set). (The `ZoomIn`/`ZoomOut` `mouse_event` params are now used — cursor-relative zoom, 2026-05-29.)
```

- [ ] **Step 2: Commit**

```bash
git add bughunt.md
git commit -m "docs: narrow D7 to remaining ui.rs cursor remnant [D7]"
```

---

## Final verification

- [ ] `cargo build && cargo clippy --all-targets && cargo test` — expect green, and the two `mouse_event` unused warnings gone.
- [ ] `cargo test focal_for_zoom_at_cursor zoom_center` — focal + crop tests pass.
- [ ] Confirm no remaining 2-arg `zoom_center(` call: `grep -rn "zoom_center(" src/` shows only the 3-arg call and the definition.
- [ ] Manual (optional, needs a real terminal + indexed images): `cargo run -- tui`, zoom an image, scroll the wheel over a corner — that corner should stay under the cursor as it magnifies.
