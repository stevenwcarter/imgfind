# Grid `gg` / `G` first/last navigation — design

Date: 2026-07-02
Status: Approved

## Summary

Extend the GUI grid's vim-style keyboard navigation with two jump
shortcuts: `gg` moves the highlighted cursor to the **first** tile
(index 0), and `G` (Shift+G) moves it to the **last** tile
(`len - 1`). Scope is the **thumbnail grid only** (including when the
detail panel is open, since that is still the grid FocusScope). The
fullscreen lightbox is intentionally untouched.

## Motivation

The grid already supports vim `h/j/k/l` + arrows (`nav::move_selection`)
and vim-style tag chords (`mm`, `m<color>`, `f<color>`, backtick, `t`).
Jump-to-ends (`gg`/`G`) is the obvious missing pair for anyone with vim
muscle memory navigating a long result list.

## Behavior

- **`gg`** — two-key chord (mirrors the existing `mm` chord): the first
  `g` arms a pending prefix + the shared chord timeout timer; the second
  `g` fires the jump to index 0.
- **`G`** — a single key. Slint reports Shift+G as `event.text == "G"`,
  so uppercase `G` resolves directly to the jump-to-last action with no
  prefix. Lowercase `g` never means "last".
- **Miss/cancel** — any key other than `g` pressed while the `g` prefix
  is pending cancels the prefix and performs no action, and (matching the
  existing `mm`/`AwaitM` behavior) that key is consumed rather than
  falling through to nav. The chord timeout also cancels the prefix, same
  as `m`/`f`.
- **Empty grid** (`len == 0`) — both are no-ops.
- **Visual-selection interaction** — when a Range (`Shift+V`) or Free
  (`v`) selection is active, `gg`/`G` move the cursor through the same
  `Selection::cursor_moved` path that `h/j/k/l` use, so a Range naturally
  **extends** to the first/last tile (vim `Vgg` semantics) and Free mode
  keeps its toggled set while relocating the cursor. No special-casing.
- **Scroll + detail** — the jumped-to tile scrolls into view via the
  existing `changed selected-index` hook in `app.slint`; if the detail
  panel is open it live-updates to the new tile. The statusline refreshes.
  All identical to the current `grid-nav` handler.

## Design

Three touch points, following the established "keyboard state lives in
the pure `chords.rs` machine; Slint forwards keys; `main.rs` performs the
action" pattern.

### 1. `imgfind-gui/src/chords.rs` (pure state machine)

- Add `Pending::AwaitG` variant.
- Add `Action::JumpFirst` and `Action::JumpLast`.
- `resolve` additions:
  - `Pending::None` + `"g"` → `(AwaitG, None)` (safe: bare `g` with no
    prefix is currently a no-op; green painting is `AwaitM`/`AwaitF` + `g`,
    which is unchanged)
  - `Pending::None` + `"G"` → `(None, Some(JumpLast))`
  - `AwaitG` + `"g"` → `(None, Some(JumpFirst))`
  - `AwaitG` + anything else → `(None, None)` (cancel, no action)

  Note `BrushColor::from_letter` is lowercase-only, so `"G"` is inert in
  the paint/filter chords (`AwaitM`/`AwaitF` + `"G"` → cancel), leaving
  `"G"` free to mean jump-to-last only from `Pending::None`.
- Unit tests: `gg` fires `JumpFirst`; `G` fires `JumpLast`; `g` then a
  non-`g` key cancels to `(None, None)`; a bare `g` yields `AwaitG` with
  no action.

The `main.rs` chord dispatcher already arms the timeout timer whenever
`resolve` returns `AwaitM | AwaitF`; extend that guard to include
`AwaitG` so the `g` prefix times out consistently.

### 2. `imgfind-gui/ui/app.slint`

- `is-chord-key(text)` (the callback that gates which keys are forwarded
  to Rust's chord machine) already returns true for `"g"` (it is the
  green brush letter), so only `"G"` needs adding.
- No new key blocks. The existing **grid** chord-forwarding block
  (`if (root.is-chord-key(event.text)) { root.key(event.text); }`)
  already forwards these. Because `g`/`G` are added to `is-chord-key`,
  the **lightbox** branch — which also forwards chord keys — would see
  them too; to keep the Grid-only scope, the lightbox chord-forward must
  be guarded so `g`/`G` are **not** forwarded there (they fall through to
  the lightbox's existing "swallow" behavior, i.e. no-op). Concretely:
  the lightbox forwards a chord key only when it is not `g`/`G`.

  Rationale: `g`/`G` are the only chord keys whose action targets the
  grid cursor; every other chord key (tagging) is meaningful on the
  lightbox image, so only `g`/`G` are excluded there.

### 3. `imgfind-gui/src/main.rs`

- Factor the body of the existing `on_grid_nav` handler — set `selected`,
  call `selection.cursor_moved(i)`, set `selection_dirty`, update
  `selected-index`, live-update the detail panel if open, `push_statusline`
  — into a small shared helper (a closure or free fn) parameterized by the
  **target index** `Option<usize>`. `on_grid_nav` computes its target via
  `nav::move_selection`; the new jump actions compute theirs directly:
  - `JumpFirst` → `Some(0)` when `len > 0`, else `None`.
  - `JumpLast` → `Some(len - 1)` when `len > 0`, else `None`.
- Wire `Action::JumpFirst` / `Action::JumpLast` arms into the existing
  `chords::Action` match in the `window.on_key` dispatcher, calling the
  shared helper.

No new `nav.rs` math is required (targets are trivial), and
`grid_index` newtypes are reused where the helper sets state.

## Non-goals / out of scope

- Lightbox first/last jump (explicitly excluded).
- Numeric counts (`5G`, `3gg`) — not supported; vim-lite only.
- Persistence — selection/cursor remain ephemeral GUI state, unchanged.
- No schema, config, or DB changes.

## Testing

- Pure unit tests in `chords.rs` for the new `resolve` transitions
  (the meaningful branching logic).
- The `main.rs` wiring is thin glue over already-tested primitives
  (`Selection::cursor_moved`, the `grid-nav` state updates); verified by
  building + a manual smoke check driving the GUI (`gg`/`G` move the
  green cursor to first/last and scroll it into view; `Vgg`/`VG` extend a
  range; both no-op on an empty result set).

## Docs

- `CLAUDE.md`: add `gg`/`G` to the keyboard-navigation summary line.
- `USAGE.md`: add `gg`/`G` to the GUI keyboard shortcuts.
