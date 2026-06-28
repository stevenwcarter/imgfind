# bughunt.md — code-health audit findings

Last triage: 2026-06-28 against `main` @ b8fdb85f. Toolchain: cargo build --workspace / cargo test --workspace / cargo clippy --workspace --all-targets.

> **For future sessions reading this file:** when you fix an item listed
> here, strip it from this file in the same commit that fixes it. The list
> is intended to reflect open issues only; resolved items shouldn't linger.
> This keeps the file's signal-to-noise high for the next audit pass.

## How to use this file
- Check `[x] execute` on items to fix this batch.
- Check `[x] skip` on items to never re-flag (the skill records them in user memory).
- Items left unchecked stay in bughunt.md for the next run.
- Ranking is impact = severity × blast-radius (effort is shown separately, never folded into the rank).
- When ready, run `/code-health --execute`.

_IDs continue from B16: B1–B15 were addressed/triaged in prior passes (B12/B15 fixed this session; B4/B9 skipped). Security (Lens 1) returned no findings — path conversions use `strip_prefix` guards, SQL table names are charset-validated before interpolation, process spawning uses separate `.arg()` calls, no secrets/unsafe._

## Critical

_(none)_

## High

_(none)_

## Medium

### B17. Thumbnail DB write failure is swallowed — silent lost work and a possible infinite loop under `--all`: `flush` in `generate_missing_thumbnails_batch` (src/thumbnail.rs:80)
- Category: correctness
- Impact: 9 (severity 3 × blast-radius 3)
- Effort: M
- Risk: high (the write-failure path has no test)
- Evidence: the writer's `flush` closure calls `insert_thumbnails_batch` and on `Err` only `tracing::error!`s (thumbnail.rs:85-87); the generated JPEG bytes are dropped. (Correction to the raw triage note: the success counter is *not* inflated — failures simply aren't counted — so the count is correct, but the work is silently lost.) The function still returns `Ok`, so the CLI reports success. Worse, `thumbnails --all` loops batches "until no missing thumbnails remain" (per CLAUDE.md / main.rs); under a persistent write failure (disk full, read-only DB) the missing count never decreases → the loop regenerates forever, burning CPU with no progress. This is the write-error gap deliberately scoped *out* of B12 (which fixed the writer-thread *panic* join); it is now its own finding.
- Blast radius: src/thumbnail.rs:73-100, :140-147; src/main.rs (the `thumbnails --all` loop)
- Proposed fix: surface a flush failure so the batch function returns `Err` (e.g. record a flush-error flag/`Result` shared with the writer thread and check it after join), so the CLI reports failure and `--all` aborts instead of looping. At minimum, break the `--all` loop when a batch makes zero forward progress.
- [ ] execute   [ ] skip

### B20. Directory-walk errors are silently skipped during indexing — no diagnostic trail for unindexed files: `index_directory` (src/main.rs:447)
- Category: observability
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: the index file-walk discards `WalkDir` errors with `Err(_) => continue` (main.rs ~:444-448). Permission-denied dirs, broken symlinks, and transient I/O errors cause whole subtrees to be skipped with zero logging, so a user wondering why images are missing from search has no breadcrumb even at `RUST_LOG=debug`.
- Blast radius: src/main.rs:444-452
- Proposed fix: `Err(e) => { tracing::debug!("skipped path on walk error: {e}"); continue; }` (debug level keeps normal output clean while giving operators a diagnostic trail).
- [x] execute   [ ] skip

## Low

### B27. `thumbnails --size` accepts an unbounded `u32` → OOM/extreme slowness: `resolve_thumbnail_sizes` (src/main.rs:1040)
- Category: api-surface
- Impact: 2 (severity 2 × blast-radius 1)
- Effort: S
- Risk: low
- Evidence: `--size` is validated to reject 0 but has no upper bound; `--size 4294967295` reaches `image::resize` and tries to allocate/process an absurd rendition (OOM or effectively hangs). Self-inflicted, but a trivial guard prevents a foot-gun.
- Blast radius: src/main.rs:1040-1053; src/thumbnail.rs (resize call)
- Proposed fix: cap accepted sizes at a sane maximum (e.g. ≤ 8192, comfortably above `LIGHTBOX_SIZE` 2048) in `resolve_thumbnail_sizes`, `bail!`ing on larger.
- User Note: allow up to 100 megapixels for now (normal-ish aspect ratio, just pick something in the right ballpark)
- [x] execute   [ ] skip

### B28. Indexing progress-bar template `.unwrap()` on a constant string: `index_directory` (src/main.rs:496)
- Category: api-surface
- Impact: 1 (severity 1 × blast-radius 1)
- Effort: S
- Risk: low
- Evidence: `ProgressStyle::default_bar().template(...).unwrap()` on a hardcoded template — valid today, but a future edit that breaks the template string panics the index command instead of degrading. Near-cosmetic; listed for completeness.
- Blast radius: src/main.rs:492-500
- Proposed fix: `.expect("progress bar template is a valid constant")` for a clear message, or fall back to the default style on parse error.
- [x] execute   [ ] skip

## Skip (do not re-flag in future runs)
- `SearchState.results` unbounded `Vec<RowMeta>` at imgfind-gui/src/state.rs:19 — search results are 100 by default and only "relevance" is in-memory sorted; O(100) is fine.
- ANSI escape codes in file logs via `with_ansi(true)` at src/logging.rs:29 (B9) — skipped 2026-06-28.
- Migration 005 non-idempotent `ADD COLUMN`s at src/schema.rs:376 (B16) — skipped 2026-06-28 (latent DB-bricking risk on an interrupted v4→v5 migration; accepted for now).
