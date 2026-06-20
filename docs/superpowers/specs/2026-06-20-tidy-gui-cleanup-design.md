# Tidy execution spec — GUI cleanup (2026-06-20)

Execution batch for `/tidy --execute`: the nine `[x]`-checked findings in
`TIDY.md` (the four `[ ]` items stay deferred). One commit per finding,
`tidy(<lens>): <summary> [T<n>]`, stripping the finding's bullet from `TIDY.md`
in the same commit. Lint after each; full `cargo test --workspace` at the T5
milestone and at the end. Branch: `tidy/2026-06-20`.

## T1 — colors index-order test is partial (`src/colors.rs`)
`index_is_stable_order` pins only `Red=0`/`Blue=4`. Add asserts for
`Green=1`, `Yellow=2`, `Purple=3`. Test-only.
Commit: `tidy(tests): assert all five BrushColor indices [T1]`

## T2 — redundant `.trim()` in `add_words` (`imgfind-gui/src/tagset.rs`)
`.trim()` is a no-op after `split_whitespace()`. Drop the `let w = w.trim();`
line; keep the `!w.is_empty()` guard (scope-minimal). Update the doc comment
("trimming and skipping duplicates" → "skipping duplicates").
Commit: `tidy(idioms): drop redundant trim in add_words [T2]`

## T3 — detail loads have no rapid-nav guard (`imgfind-gui/src/main.rs`)
The detail-panel image/meta/tags background loads can paint image A's data onto
image B after a fast A→B nav. Fix with a **path-match** guard (the idiomatic
choice here — `apply_tags_to_*` already use it, and `detail: Mutex<Option<
DetailState>>` is the existing source of truth; no new counter to keep in sync
across the two open sites).

- Add a pure predicate `fn detail_shows(detail: &Option<DetailState>, path:
  &str) -> bool` and a unit test for it (characterizes the guard kernel).
- Thread `detail: Arc<Mutex<Option<DetailState>>>` into `spawn_detail_image`
  and `spawn_detail_meta`; on the UI thread, gate `set_detail_image` /
  `set_detail_meta` behind `detail_shows(&detail.lock().unwrap(), &path)`.
  In `spawn_detail_image`, still decode + `detail_cache::insert` (caching a
  neighbor is desirable); only the visible `set_detail_image` is gated.
- Gate the two inline tag loads (interactive `on_tile_selected` ~line 616 and
  the restore path ~line 2308) behind the same predicate before
  `push_detail_tags`.
Commit: `tidy(fix): guard detail loads against rapid-nav races [T3]`

## T4 — no symmetric range→free reset test (`imgfind-gui/src/selection.rs`)
`re_entering_mode_resets_set` covers free→range. Add the symmetric
range→free case asserting the set empties and mode is `Free`.
Commit: `tidy(tests): add range→free reset test [T4]`

## T5 — statusline test asserts count, not byte total (`imgfind-gui/src/main.rs`)
`statusline_free_shows_selection_stats` toggles indices 0 (2 MB) + 2 (4 MB) =
6,000,000 bytes. Add `assert!(line.contains(&format_bytes(6_000_000)))`.
Commit: `tidy(tests): assert selected byte total in statusline [T5]`
**Milestone: run `cargo test --workspace` after this commit.**

## T6 — apply_tags detail-gate divergence (`imgfind-gui/src/main.rs`)
`apply_tags_to_focused` snapshots a `bool`; `apply_tags_to_selection` carries an
`Option<String>` + in-thread membership. DRY: extract the spawn-write-then-
detail-refresh tail into `apply_tags_to_paths(.., paths: Vec<String>, tags)`
(the Option-membership form). `apply_tags_to_focused` computes its single path
and delegates with `vec![path]` (`paths.contains(&dp)` ⟺ `dp == path`).
Rename the call site in `paint_brush_by_index` accordingly. Private fns only —
no public API change.
Commit: `tidy(duplication): share detail-refresh tail in tag apply [T6]`

## T7 — selection tests bypass `set_of` (`imgfind-gui/src/selection.rs`)
`range_to_*` / `ctrl_toggle_*` tests inline `s.set().iter().copied().collect::
<Vec<_>>()`. Replace each with the existing `set_of(&s)` helper.
Commit: `tidy(tests): use set_of helper in selection tests [T7]`

## T8 — stale comment above the tile `clicked` block (`imgfind-gui/ui/app.slint`)
The "Return keyboard focus…" comment now sits above a handler whose primary job
is `root.tile-clicked(...)`. Refresh it to describe both the dispatch and the
focus return.
Commit: `tidy(comments): refresh stale tile-clicked comment [T8]`

## T9 — `paint_brush_by_index` unchecked index (`imgfind-gui/src/main.rs`)
`ctx.brushes.lock().unwrap()[idx]` → `let Some(tags) = ctx.brushes.lock()
.unwrap().get(idx).cloned() else { return; };`. Unreachable today; defensive.
Commit: `tidy(fix): bounds-check brush index with get [T9]`

**Final: run `cargo test --workspace` once more; report green.**
