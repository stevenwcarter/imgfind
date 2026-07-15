# Handoff — Telnet Search Server (for the executing agent)

You are picking up an approved, committed plan to implement on branch
**`feat/telnet-search-server`**. This note tells you how to run it and the
gotchas the plan author (a different Claude instance) already knows about.

## Start here

1. `git fetch && git checkout feat/telnet-search-server && git pull` (this branch
   already contains the spec + plan commits; base your work on top of them).
2. Read, in order:
   - `docs/superpowers/specs/2026-07-14-telnet-search-server-design.md` (the design)
   - `docs/superpowers/plans/2026-07-14-telnet-search-server.md` (the task list)
3. Execute the plan **task by task** with **superpowers:subagent-driven-development**
   (fresh subagent per task, review between tasks). Tasks 1–4 and 6's pure
   helpers are TDD with real tests; 5, 7, 8, 9 are wiring/docs/integration.

## Environment prerequisites

- This repo builds against a **sibling `../clipper` crate** (path dependency) —
  it must be checked out next to `imgfind` or the workspace won't build. Confirm
  `../clipper` exists before starting.
- **GPG-signed commits**: this repo signs commits. If the GPG agent locks
  mid-session and a commit fails with a signing error, stop and ask the operator
  to unlock the agent (this happened during planning). Keep the plan's commit
  messages; append the repo's `Co-Authored-By` trailer if that's the convention
  in recent history (`git log` to check).
- Manual end-to-end testing needs a directory with an indexed `.imgfind` DB that
  **has embeddings** (run `imgfind index` then `imgfind process`), plus a
  `telnet` client installed.

## Non-obvious things the plan already accounts for (don't re-derive)

- **Multi-connection is real telnet**: it's a plain `TcpListener` with one tokio
  task per connection. No special telnet feature is needed.
- **"Press any key" requires char-at-a-time mode**: the server sends
  `IAC WILL ECHO` + `IAC WILL SGA` on connect (`protocol::initial_negotiation`).
  Without this, most clients line-buffer and keystrokes won't arrive until Enter.
  The server owns echo — that's also how the password prompt is hidden.
- **One shared embedder**: the CLIP model loads **once** on a dedicated OS thread
  that owns it (so it needs no `Sync` bound) and answers embed requests over a
  channel. Do **not** load a model per connection.
- **Paths are DB-relative**: convert with `relative_to_abs_path(rel, &db.parent_dir)`
  before decoding an image. This is the #1 mistake in this codebase.

## Verification points the plan flags (confirm against the code, don't assume)

The plan's code blocks were written from reading the codebase but a few API
shapes must be confirmed as you implement — each is called out inline in the
relevant task, but collected here:

1. **turso `conn.execute` return type** (Task 4, `remove_telnet_user`): the plan
   assumes it returns an affected-row count. Compare to the existing
   `DELETE FROM favorites` call in `src/database.rs` and adapt the return.
2. **`DistanceThreshold` / `MaxK` constructors** (Task 7): the plan uses tuple
   form `DistanceThreshold(1.3)` / `MaxK(200)`. Confirm against `src/units.rs`
   and existing call sites in `src/database.rs` tests.
3. **`db.active_model().name`** (Task 7): confirm `active_model()` returns a
   struct with a `.name` field — it's used this way in `search_images`
   (`src/main.rs`).
4. **DB test harness idiom** (Task 4): the plan's test uses an inline tempfile
   setup; if the existing `#[cfg(test)]` block in `src/database.rs` has a shared
   helper (e.g. `test_db()`), prefer that for consistency.
5. **`col_text` / `col_i64`** are private helpers already in `src/database.rs`;
   the new methods live in the same `impl Database` block, so they're in scope.

## Definition of done

- `cargo build --workspace` and `cargo test --workspace` are green (the Task 8
  loopback test stays `#[ignore]`d — run it manually with `--ignored` once to
  confirm the real drive).
- Manual end-to-end (Task 7 Step 5) works: add a user, start the server, telnet
  in, log in, search → color art, any-key → search box, Esc → redraw art, and
  **two simultaneous sessions** both work.
- Docs updated (Task 9).
- Then run **superpowers:requesting-code-review**, address findings, and
  **superpowers:finishing-a-development-branch** to integrate (the operator will
  choose merge vs PR). Do not merge to `main` without the operator's go-ahead.

## If you get stuck

- BLOCKED on an API shape you can't confirm from the code → search the codebase
  for the nearest existing call site (the plan names one for each flagged point).
- BLOCKED on model download / no indexed fixture for the integration test →
  keep it `#[ignore]`d, note it, and proceed; it's not a release blocker.
- Genuine design ambiguity the spec's "Open implementer choices" section doesn't
  cover → make the smallest reasonable choice, document it in the code, and note
  it in your review request. Don't stall.
