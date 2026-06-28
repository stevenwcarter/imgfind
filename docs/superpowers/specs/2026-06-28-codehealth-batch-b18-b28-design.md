# code-health execution: B18–B28 (11 findings)

Date: 2026-06-28. Source: `bughunt.md` `[x] execute` items.
B16 skipped (recorded in memory). B17 left unchecked (stays open).
Toolchain: `cargo build --workspace`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets`, `cargo fmt --all`.

One commit per finding (`fix(<category>): <summary> [B<n>]`), finding stripped
in that same commit. Sequential, grouped by file so same-file findings are
consecutive and each commit stages a clean single-finding change. Full workspace
test suite at the 5-finding milestone and at the end.

Findings wanting a test (land with one): B19, B21, B22, B27. The logging /
hygiene one-liners (B18, B20, B23, B24, B26, B28) and the B25 micro-refactor are
verified by build+clippy+existing suite (their behavior isn't unit-testable in
isolation, or the change is a pure simplification).

## Execution order

1. **B19** (`src/config.rs`) — validate `distance_threshold` is finite and
   `>= 0.0` (reject or clamp) at the config boundary. Test: a config with NaN/inf
   threshold is rejected (or clamped) — round-trip unit test.
2. **B21** (`src/database.rs`) — `find_similar_to_path`: collapse the id-lookup +
   embedding-lookup into one JOIN-on-path query. Test: existing similar-search
   tests still pass + a focused test that the JOIN returns the seed embedding.
3. **B22** (`src/lib.rs`) — reject a relative path containing
   `Component::ParentDir`/`RootDir` on read (in `to_absolute` or at the row
   boundary). Test: a `../escape` relative path yields an error, a normal path
   resolves.
4. **B18** (`imgfind-gui/src/meta_cache.rs`) — both `.lock().unwrap()` →
   `.lock().unwrap_or_else(|e| e.into_inner())` so a poisoned lock recovers
   instead of crashing the UI.
5. **B26** (`src/thumbnail.rs`) — replace the writer-DB-open `panic!` with
   `return Err(e).context("writer thread failed to open database")`.
   *(milestone: full `cargo test --workspace` here.)*
6. **B24** (`imgfind-gui/src/main.rs`) — `match` the `rehydrate` result and
   `tracing::warn!` before the empty-Vec fallback (keep graceful fallback).
7. **B23** (`imgfind-gui/src/backend.rs`) — `tracing::info!("CLIP model loaded:
   {model_name}")` on the success path.
8. **B25** (`imgfind-gui/src/backend.rs` + `src/tui/app/search.rs`) — stop
   materializing a `SearchConfig` per search just to read the two hardcoded
   constants; pass the values directly / build once. *(after B23 — same file.)*
9. **B20** (`src/main.rs`) — index walk error arm: `Err(e) => { tracing::debug!(
   "skipped path on walk error: {e}"); continue; }`.
10. **B27** (`src/main.rs`) — cap `--size` in `resolve_thumbnail_sizes`.
    **User note: allow up to ~100 megapixels (normal-ish aspect ratio).** `--size`
    is a bounding-box long-edge in px; pick a clean constant whose square/normal
    output lands ~100 MP — use `MAX_THUMBNAIL_SIZE = 12_000` (≈100 MP at a 3:2
    photo; 12000×8000 = 96 MP) and `bail!` above it. Test: a size above the cap is
    rejected, a size at/below passes (and the existing GUI sizes still resolve).
    *(after B20 — same file.)*
11. **B28** (`src/main.rs`) — progress-bar template `.unwrap()` →
    `.expect("progress bar template is a valid constant")`. *(after B27.)*
    *(final: full `cargo test --workspace`.)*

## Verification (controller, per finding)
- Affected crate builds; new test (if any) passes.
- `cargo clippy --workspace --all-targets` clean and `cargo fmt --all --check`
  clean before each commit.
- Full `cargo test --workspace` at the milestone (after B26) and after B28.
