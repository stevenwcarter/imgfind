# bughunt.md — code-health audit findings

Last triage: 2026-06-20 against `main` @ de6dcd24. Toolchain: cargo build --workspace / cargo test --workspace / cargo clippy --workspace --all-targets.

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

## Critical

_(none)_

## High

## Medium

### B9. ANSI escape codes written into file logs (`with_ansi(true)`): `logging init` (src/logging.rs:29)
- Category: observability
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: The non-blocking file appender is configured `with_ansi(true)`, so `log.txt` gets ANSI escapes that corrupt grep/parsers.
- Blast radius: src/logging.rs:29
- Proposed fix: Set `.with_ansi(false)` on the file writer (keep color only for a TTY terminal layer if desired).
- [ ] execute   [ ] skip

### B12. Thumbnail writer-thread panic reported to CLI as `Ok(0)`: `generate_missing_thumbnails_batch` (src/thumbnail.rs:165)
- Category: api-surface
- Impact: 6 (severity 3 × blast-radius 2)
- Effort: M
- Risk: low
- Evidence: The writer thread panics if `Database::new()`/`pool.get()` fail (explicit `panic!` at 75-77); `join()` at 164 logs but the function still returns `Ok(0)`, so the CLI reports success having generated zero thumbnails.
- Blast radius: src/main.rs:284, src/thumbnail.rs:79, src/thumbnail.rs:164
- Proposed fix: Propagate a writer-thread panic/join error as `Err`, or make the writer return a `Result` communicated via a shared channel.
- [ ] execute   [ ] skip


## Low

### B15. Open-in-OS-viewer failure only logged, no UI feedback: `on_tile_open_external` (imgfind-gui/src/main.rs:744)
- Category: api-surface
- Impact: 4 (severity 2 × blast-radius 2)
- Effort: S
- Risk: low
- Evidence: `open::that(&abs)` errors on right-click-open are caught and `tracing::warn!`'d; the GUI shows nothing, so the user gets no indication the open failed.
- Blast radius: imgfind-gui/src/main.rs:744
- Proposed fix: On `Err`, surface a transient UI error (toast/status field).
- [ ] execute   [ ] skip

## Skip (do not re-flag in future runs)
- `SearchState.results` unbounded `Vec<RowMeta>` at imgfind-gui/src/state.rs:19 — search results are 100 by default and only "relevance" is in-memory sorted; O(100) is fine.
