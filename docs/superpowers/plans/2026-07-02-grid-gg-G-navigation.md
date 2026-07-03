# Grid `gg` / `G` first/last navigation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add vim-style `gg` (jump to first tile) and `G` (jump to last tile) to the GUI thumbnail grid.

**Architecture:** `gg` is a two-key chord resolved by the existing pure `chords.rs` state machine (mirrors `mm`); `G` is a single key resolved directly. `app.slint` forwards the keys; `main.rs` performs the jump via a shared helper that reuses the exact grid-nav state updates (cursor move, selection extend, scroll-into-view, statusline). Grid-only scope; the lightbox is excluded.

**Tech Stack:** Rust (edition 2024), Slint UI, `parking_lot::Mutex`, existing `nav`/`selection`/`chords` GUI modules.

## Global Constraints

- Scope is the **grid only**. The lightbox must NOT gain `gg`/`G` behavior.
- `BrushColor::from_letter` is lowercase-only; `"G"` is inert in paint/filter chords. Bare `g` (no prefix) is currently a no-op, so repurposing it to arm `gg` is safe; green painting (`m g` / `f g`) must stay working.
- Empty grid (`len == 0`) → both jumps are no-ops.
- Ephemeral GUI state only — no schema, config, or persistence changes.
- Build/test with `cargo test -p imgfind-gui` and `cargo build -p imgfind-gui`.

---

### Task 1: `chords.rs` — `AwaitG` state + `JumpFirst`/`JumpLast` actions

**Files:**
- Modify: `imgfind-gui/src/chords.rs`
- Test: `imgfind-gui/src/chords.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `chords::Pending::AwaitG`; `chords::Action::JumpFirst`; `chords::Action::JumpLast`. `resolve` gains transitions: `None+"g"→(AwaitG,None)`, `None+"G"→(None,Some(JumpLast))`, `AwaitG+"g"→(None,Some(JumpFirst))`, `AwaitG+other→(None,None)`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `imgfind-gui/src/chords.rs`:

```rust
    #[test]
    fn g_arms_prefix_no_action() {
        assert_eq!(resolve(Pending::None, "g"), (Pending::AwaitG, None));
    }

    #[test]
    fn gg_jumps_first() {
        let (p, _) = resolve(Pending::None, "g");
        assert_eq!(
            resolve(p, "g"),
            (Pending::None, Some(Action::JumpFirst))
        );
    }

    #[test]
    fn shift_g_jumps_last() {
        assert_eq!(
            resolve(Pending::None, "G"),
            (Pending::None, Some(Action::JumpLast))
        );
    }

    #[test]
    fn g_then_other_cancels_no_action() {
        let (p, _) = resolve(Pending::None, "g");
        assert_eq!(resolve(p, "j"), (Pending::None, None));
    }

    #[test]
    fn green_brush_still_paints_after_m() {
        // Regression: `g` after `m` must still mean the green brush.
        let (p, _) = resolve(Pending::None, "m");
        assert_eq!(
            resolve(p, "g"),
            (Pending::None, Some(Action::PaintBrush(BrushColor::Green)))
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui chords:: 2>&1 | tail -20`
Expected: compile error — `no variant AwaitG` / `no variant JumpFirst`.

- [ ] **Step 3: Add the variants and transitions**

In `imgfind-gui/src/chords.rs`, add `AwaitG` to `Pending`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pending {
    #[default]
    None,
    AwaitM,
    AwaitF,
    AwaitG,
}
```

Add the two actions to `Action`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ToggleRail,
    OpenTagModal,
    PaintBrush(BrushColor),
    RepeatLast,
    LoadBrushIntoFilter(BrushColor),
    ToggleTagFilter,
    JumpFirst,
    JumpLast,
}
```

In `resolve`, extend the `Pending::None` arm (add `g`/`G` lines alongside the existing `m`/`f`) and add a new `AwaitG` arm:

```rust
        Pending::None => match key {
            "`" => (Pending::None, Some(Action::ToggleRail)),
            "t" => (Pending::None, Some(Action::OpenTagModal)),
            "m" => (Pending::AwaitM, None),
            "f" => (Pending::AwaitF, None),
            "g" => (Pending::AwaitG, None),
            "G" => (Pending::None, Some(Action::JumpLast)),
            _ => (Pending::None, None),
        },
```

Add after the `Pending::AwaitF` arm (before the closing brace of the outer match):

```rust
        Pending::AwaitG => match key {
            "g" => (Pending::None, Some(Action::JumpFirst)),
            _ => (Pending::None, None),
        },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui chords:: 2>&1 | tail -20`
Expected: all `chords::` tests PASS (including the pre-existing ones and `green_brush_still_paints_after_m`).

- [ ] **Step 5: Commit**

```bash
git add imgfind-gui/src/chords.rs
git commit -m "feat(gui): gg/G chord resolution in the chord state machine"
```

---

### Task 2: Wire `gg`/`G` into the grid (main.rs + app.slint) + docs

**Files:**
- Modify: `imgfind-gui/src/main.rs` (grid-nav handler, chord-timer guard, `on_key` action match, new helper fn)
- Modify: `imgfind-gui/ui/app.slint` (`is-chord-key`, lightbox chord-forward guard)
- Modify: `CLAUDE.md`, `USAGE.md`

**Interfaces:**
- Consumes: `chords::Action::JumpFirst`, `chords::Action::JumpLast`, `chords::Pending::AwaitG` (Task 1).
- Produces: free fn `apply_grid_target(target: Option<usize>, selected: &Arc<Mutex<Option<usize>>>, selection: &Arc<Mutex<selection::Selection>>, selection_dirty: &Arc<AtomicBool>, state: &Arc<Mutex<SearchState>>, w: &MainWindow)`.

- [ ] **Step 1: Add the shared `apply_grid_target` helper**

In `imgfind-gui/src/main.rs`, add this free function near `push_statusline` (around line 3708). It is the extracted body of the current `on_grid_nav` handler, parameterized by an absolute target index:

```rust
/// Apply a grid cursor move to `target` (None = clear selection): updates the
/// selected index, moves the multi-selection cursor (extends a Range/Free
/// selection), marks the selection dirty, syncs `selected-index` (scrolls the
/// tile into view via the `changed selected-index` hook), live-updates the
/// detail panel if open, and refreshes the statusline. Shared by keyboard
/// hjkl/arrow nav and the gg/G first/last jumps.
fn apply_grid_target(
    target: Option<usize>,
    selected: &Arc<Mutex<Option<usize>>>,
    selection: &Arc<Mutex<selection::Selection>>,
    selection_dirty: &Arc<AtomicBool>,
    state: &Arc<Mutex<SearchState>>,
    w: &MainWindow,
) {
    *selected.lock() = target;
    // Move the multi-selection cursor so a Range selection grows/shrinks live
    // (a no-op in Normal mode). Guard dropped before any invoke_*/set_*.
    if let Some(i) = target {
        selection.lock().cursor_moved(i);
    }
    selection_dirty.store(true, Ordering::Relaxed);
    w.set_selected_index(target.map(|i| i as i32).unwrap_or(-1));
    if let Some(i) = target
        && w.get_detail_open()
    {
        w.invoke_tile_selected(i as i32);
    }
    push_statusline(w, selection, state);
}
```

- [ ] **Step 2: Refactor `on_grid_nav` to call the helper**

Replace the body of `window.on_grid_nav(...)` (currently lines ~1787-1817) below the `move_selection` call so it delegates. The full replacement closure body:

```rust
        window.on_grid_nav(move |dir_i, cols_i| {
            let Some(dir) = nav::NavDir::from_i32(dir_i) else {
                return;
            };
            let len = state_ref.lock().results().len();
            let cur = *selected_ref.lock();
            let new = nav::move_selection(
                cur.map(grid_index::CursorIndex),
                dir,
                grid_index::GridCols(cols_i.max(0) as usize),
                grid_index::ItemCount(len),
            );
            if let Some(w) = weak.upgrade() {
                apply_grid_target(
                    new,
                    &selected_ref,
                    &selection_ref,
                    &selection_dirty_ref,
                    &state_ref,
                    &w,
                );
            }
        });
```

- [ ] **Step 3: Capture `selection_dirty` in the `on_key` closure**

In the `window.on_key` setup block (the `let ... = Arc::clone(...)` list at ~2461-2477), add a clone so the action match can call the helper:

```rust
        let selection_dirty_ref = Arc::clone(&selection_dirty);
```

- [ ] **Step 4: Extend the chord-timer guard to include `AwaitG`**

In `on_key`, change the timer-arm condition (~line 2490) so the `g` prefix times out like `m`/`f`:

```rust
            if matches!(
                next,
                chords::Pending::AwaitM | chords::Pending::AwaitF | chords::Pending::AwaitG
            ) {
```

- [ ] **Step 5: Handle the two new actions in the `on_key` match**

Add these arms to the `match action { ... }` block (alongside the existing `chords::Action::*` arms, e.g. after `ToggleTagFilter`):

```rust
                chords::Action::JumpFirst | chords::Action::JumpLast => {
                    let len = state_ref.lock().results().len();
                    let target = if len == 0 {
                        None
                    } else if matches!(action, chords::Action::JumpFirst) {
                        Some(0)
                    } else {
                        Some(len - 1)
                    };
                    apply_grid_target(
                        target,
                        &selected_ref,
                        &selection_ref,
                        &selection_dirty_ref,
                        &state_ref,
                        &w,
                    );
                }
```

- [ ] **Step 6: Forward `G` from Slint; exclude `g`/`G` from the lightbox**

In `imgfind-gui/ui/app.slint`, add `"G"` to `is-chord-key` (line ~280); `"g"` is already present:

```slint
    pure function is-chord-key(t: string) -> bool {
        return t == "`" || t == "t" || t == "m" || t == "f"
            || t == "r" || t == "g" || t == "y" || t == "p" || t == "b"
            || t == "G";
    }
```

In the **lightbox** branch's chord-forward (line ~479), guard out `g`/`G` so the jump only works in the grid (they fall through to the lightbox's swallow-all `return accept`):

```slint
                if (root.is-chord-key(event.text) && event.text != "g" && event.text != "G") {
                    root.key(event.text);
                    return accept;
                }
```

Leave the **grid** chord-forward (line ~523) unchanged — it already forwards `g`/`G`.

- [ ] **Step 7: Build and smoke-check**

Run: `cargo build -p imgfind-gui 2>&1 | tail -15`
Expected: builds clean (no errors; no new warnings about unused `selection_dirty_ref`).

Run: `cargo test -p imgfind-gui 2>&1 | tail -15`
Expected: all tests PASS.

- [ ] **Step 8: Update docs**

In `CLAUDE.md`, in the GUI keyboard-navigation summary (the sentence describing vim `h/j/k/l` and arrow keys under the Native GUI section), add a clause noting `gg` jumps to the first tile and `G` to the last.

In `USAGE.md`, add `gg` / `G` to the GUI keyboard-shortcuts list next to the `h/j/k/l` navigation entry.

- [ ] **Step 9: Commit**

```bash
git add imgfind-gui/src/main.rs imgfind-gui/ui/app.slint CLAUDE.md USAGE.md
git commit -m "feat(gui): gg/G jump grid cursor to first/last tile"
```

---

## Self-Review

- **Spec coverage:** `gg`/`G` resolution (Task 1); grid-only scope via lightbox guard (Task 2 Step 6); visual-selection extension via `cursor_moved` in the shared helper (Task 2 Step 1); scroll-into-view + detail live-update + statusline reused from grid-nav (helper); empty-grid no-op (Task 2 Step 5); green-brush regression guard (Task 1 test); docs (Task 2 Step 8). All covered.
- **Placeholder scan:** none — every code step is concrete.
- **Type consistency:** helper signature uses the verified real types (`Arc<Mutex<Option<usize>>>`, `Arc<Mutex<selection::Selection>>`, `Arc<AtomicBool>`, `Arc<Mutex<SearchState>>`, `MainWindow`); `apply_grid_target` is defined once and called from both sites; `selection_dirty_ref` is the established name for the `on_key` clone.
