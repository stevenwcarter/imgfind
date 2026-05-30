# Code-Health Fixes — Design Spec

Date: 2026-05-29
Branch: `code-health/2026-05-29`
Source audit: `bughunt.md` (strip each item on fix, in the same commit)

## Scope

Surgical fixes for the user-selected subset of the audit. **In scope:** S1, S2, P1, P2, C1, C2, C3, C4, C5. **Out of scope** (remain in `bughunt.md`): P3, P4, C6, C7, D1–D8, O1–O3, R1, R2.

No rewrites. Each finding is an independent, small change with its own commit. Tests added only where they catch real bugs (S1 containment, C4 empty-list).

---

## Findings & approach

### S1 — Path-traversal containment (`src/api/search.rs`, `file` handler)

The `/api/v1/search/file/{*filename}` handler joins the unsanitized wildcard to `context.basepath` and reads it, allowing `../../../etc/passwd` to escape.

**Fix:** Before reading, resolve containment:
1. Compute `base = canonicalize(context.basepath)` (fall back to the raw basepath if canonicalize fails).
2. Compute the target as `base.join(filename)`, then **lexically normalize** it (resolve `.`/`..` components without touching the filesystem, so non-existent-but-safe paths still work) — or canonicalize the target and compare. Reject if the normalized target does not start with `base`.
3. On rejection, return `StatusCode::NOT_FOUND` (404 — don't leak existence), not a panic.

Helper: a small pure `fn path_is_contained(base: &Path, candidate: &Path) -> bool` (or `fn safe_join(base, filename) -> Option<PathBuf>`) so it's unit-testable. Lexical normalization must treat a leading `/` in `filename` and any `..` that climbs above `base` as rejection.

**Test:** unit tests for the helper — `foo/bar.jpg` allowed; `../../etc/passwd`, `/etc/passwd`, `a/../../b` rejected.

### S2 — Default-bind to localhost (`src/main.rs`, `Serve` subcommand + `serve`)

Server currently binds `0.0.0.0` unconditionally.

**Fix:** Add `#[arg(long, default_value = "127.0.0.1")] host: String` to the `Serve` command. Pass it into `serve(db, dir, host, port)` and bind `format!("{host}:{port}")`. Public exposure becomes opt-in via `--host 0.0.0.0`. Merged with C3 (use `?` on bind).

### P1 — Cache the CLIP model in the server (`src/context.rs`, `src/main.rs serve`, `src/api/search.rs`)

`/search` reloads `ClipEmbedder` per request.

**Fix:**
- Add `pub embedder: Arc<ClipEmbedder>` to `GraphQLContext`; construct it once in `serve()` and pass to `GraphQLContext::new`.
- In the `search` handler, use `context.embedder` instead of `ClipEmbedder::new(...)`.
- `ClipEmbedder` methods are all `&self`, so `Arc` sharing is sufficient. If the type proves not `Send + Sync` when shared across handlers, fall back to wrapping the embedding call in `tokio::task::spawn_blocking` with the `Arc` cloned in (note for implementer; do not expand scope otherwise).

### P2 — Remove the DB `Mutex` (`src/context.rs`, `src/graphql.rs`, `src/api/search.rs`, `src/database.rs`, `src/thumbnail.rs`)

`GraphQLContext.db: Arc<Mutex<Database>>` serializes all server DB access despite the r2d2 pool.

**Fix:**
- Change the field to `pub db: Database` (it is `Clone + Send + Sync` via the pool). Update `GraphQLContext::new`. Remove/replace the `get_db()` accessor.
- Relax `Database::insert_thumbnail` from `&mut self` to `&self`, and `thumbnail::get_or_generate_thumbnail`'s `db: &mut Database` to `db: &Database` (both only call `self.pool.get()` — no true mutation of the struct).
- Delete the three `.lock().unwrap()` sites (`graphql.rs` `images_by_bounds`; `api/search.rs` `thumb`, `search`). Access `context.db` directly.
- This also covers P2's secondary benefit: no more poison-panic on a poisoned mutex.

Note: this is the one change that touches multiple files in concert; land it as a single commit so the build stays green.

### C1 — `get_db_path(Some(dir))` returns Err, not panic (`src/lib.rs`)

Replace `panic!("No database found in this directory")` with `return Err(anyhow!("No database found in {dir}"))`.

### C2 — `current_dir().unwrap()` (`src/lib.rs`)

Replace with `?` (the fn already returns `Result`).

### C3 — `serve()` error propagation + stale comment (`src/main.rs`)

Replace `.unwrap()`/`.expect()` on `TcpListener::bind` and `server.await` with `?` / contextual errors. Keep the Ctrl-C handler's `.expect()` only if there's no clean alternative, otherwise log. Delete the line-197 "Placeholder for future server implementation" comment. (Bind address comes from S2.)

### C4 — Focus underflow on empty image list (`src/tui/app/focus.rs`)

`calculate_new_focus_index` underflows (`images_len - 1` on `u8`) and `% 0` when `images_len == 0`.

**Fix:** at the top of `calculate_new_focus_index`, `if images_len == 0 { return 0; }`. Existing tests are untouched (one-way rule). **Add** one characterization test asserting `calculate_new_focus_index(0, 0, <each dir>) == 0` does not panic.

### C5 — Zoom path graceful errors (`src/tui/app/zoom.rs`)

Four `.expect()` calls crash the TUI on a missing/unreadable image.

**Fix:**
- `self.images.get(zoom_index)` miss → `tracing::warn!` and `return` (no zoom), instead of `.expect("image not found")`.
- Inside the spawned task: `ImageReader::open(...).decode()` failures → `tracing::warn!("failed to open/decode {path}: {e}")` and `return` (don't construct/send an entry), replacing the two `.expect()`s.
- `zoom_tx.send(...)` failure (receiver dropped during shutdown) → `tracing::debug!` instead of `.expect("Could not send image entry")`.

---

## Verification

Per change: `cargo build`, `cargo clippy --all-targets`, `cargo test`. The frontend is not touched in this scope, so no `site` rebuild is required; however the server binary embeds `site/build` via rust-embed, which already exists in the working tree, so `cargo build` succeeds.

Milestone: full `cargo test` green after the TUI changes and after the P2 multi-file change. Final: full test suite green.

## Commit convention

One commit per finding: `fix(<area>): <summary> [<finding-id>]`, and strip that finding's section from `bughunt.md` in the same commit. The P2 change is a single commit spanning its files.
