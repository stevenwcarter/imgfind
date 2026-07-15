# Telnet search server with ASCII-art results — design

**Date:** 2026-07-14
**Status:** Approved, ready for implementation
**Crate:** `imgfind` (root binary/library)

## What this is

An experimental `imgfind telnet` subcommand that starts a plaintext telnet
server. A user telnets in, logs in, types a natural-language search query, and
receives the **top matching image rendered as color ASCII art** (ANSI truecolor
half-blocks) filling their terminal. Pressing any key returns to the search box;
pressing Esc in the search box dismisses it and redraws the current art. Multiple
clients can connect and search simultaneously.

This is a fun/experimental feature. It reuses the existing library search,
decode, thumbnail-cache, DB, and model-loading code — it adds a network front
end and a renderer, nothing more.

## Goals

- `imgfind telnet` starts a TCP/telnet listener; each connection is an
  independent session.
- Login is required by default (accounts stored in the DB, Argon2-hashed
  passwords); `--no-auth` runs it open.
- Search returns the top image as color half-block ASCII art sized to the
  client's real terminal (via telnet NAWS negotiation).
- Interaction: any key on the results screen → search box; Esc in the search box
  → dismiss and redraw current art; Enter → run a new search.
- Multiple simultaneous connections, sharing one loaded CLIP model.

## Non-goals

- Encryption/TLS. Telnet is plaintext by design; default bind is localhost.
- Telnet AUTHENTICATION option (RFC 2941) — not practically supported by clients.
- Top-N cycling, pagination, or any GUI/TUI parity. Top result only.
- Persisting per-connection UI state.

## CLI surface (`src/main.rs`)

New subcommands on the `Commands` enum:

- `Telnet`:
  - `-d, --dir <DIR>` — DB directory (walk-up/global resolution, same as other
    commands).
  - `--bind <ADDR>` — bind address, default `127.0.0.1`. Use `0.0.0.0` to expose
    to the LAN deliberately.
  - `--port <PORT>` — default `2323` (unprivileged; avoids privileged 23).
  - `--no-auth` — run without login (open server).
  - `--max-connections <N>` — cap concurrent connections, default `16`.
- `TelnetUser` with a nested subcommand:
  - `add <NAME>` — prompt for a password **without echo** (`rpassword`), store an
    Argon2 hash in the DB. Errors if the username already exists.
  - `list` — list usernames (no hashes).
  - `remove <NAME>` — delete a user.
  - `-d, --dir <DIR>` — DB directory.

Startup rule: if auth is enabled (not `--no-auth`) and the DB has **zero** telnet
users, `imgfind telnet` refuses to start with a message telling the operator to
run `imgfind telnet-user add <name>` first (or pass `--no-auth`).

## Module layout: `src/telnet/`

Exposed from `src/lib.rs` as `pub mod telnet;`. Split into small, independently
testable units:

### `protocol.rs` — pure, unit-tested

A telnet byte-stream state machine, no I/O.

- Command constants: `IAC=255`, `DONT=254`, `DO=253`, `WONT=252`, `WILL=251`,
  `SB=250`, `SE=240`; options `ECHO=1`, `SGA=3` (suppress-go-ahead), `NAWS=31`.
- `struct TelnetParser` with `feed(&mut self, bytes: &[u8]) -> Vec<TelnetEvent>`
  where `TelnetEvent` is one of `Data(u8)` (a real user-input byte),
  `WindowSize { cols: u16, rows: u16 }` (from a NAWS `SB … SE` subnegotiation),
  or `Negotiation { .. }` (observed WILL/WONT/DO/DONT — mostly ignored, but the
  parser must consume them correctly). The parser must correctly handle
  sequences split across multiple `feed` calls (TCP does not preserve message
  boundaries) and the `IAC IAC` escape (a literal 0xFF data byte).
- Free functions building the bytes we send:
  `initial_negotiation() -> Vec<u8>` emitting `IAC WILL ECHO`, `IAC WILL SGA`,
  `IAC DO NAWS`.

Tests: parse a plain data run; parse a NAWS subnegotiation into the right
cols/rows; parse a negotiation sequence delivered one byte per `feed` call;
`IAC IAC` yields a single 0xFF `Data` byte.

### `render.rs` — pure, unit-tested

`render_halfblock(img: &image::DynamicImage, cols: u16, rows: u16) -> String`.

- Resize the image to fit `cols × (rows*2)` pixels preserving aspect ratio (each
  character cell is one column wide and two pixels tall).
- For each cell, top pixel → ANSI truecolor **foreground**, bottom pixel →
  **background**, glyph `▀` (U+2580 upper half block). Emit
  `\x1b[38;2;{r};{g};{b}m\x1b[48;2;{r};{g};{b}m▀`, reset at end of each row
  (`\x1b[0m`) plus `\r\n` line endings (telnet clients need CRLF).
- Rows with an odd final pixel row use black (or the same pixel) for the missing
  bottom.

Tests: a 2×2 solid-red image at `cols=1,rows=1` produces one cell with the
expected fg/bg escape and `▀`; output line count matches `rows`; aspect-ratio fit
never exceeds the requested bounds.

### `auth.rs` — pure, unit-tested

Argon2 password helpers (no DB access):

- `hash_password(plain: &str) -> Result<String>` — returns a PHC string using
  `argon2` with a random salt (`argon2::Argon2::default()` +
  `password_hash::SaltString`).
- `verify_password(plain: &str, phc: &str) -> bool`.

Tests: hash ≠ plaintext; `verify_password(pw, hash(pw))` is true; wrong password
is false; a malformed hash string returns false (never panics).

### `session.rs` — per-connection async state machine

Owns one `TcpStream` (split read/write halves). Uses `protocol::TelnetParser` to
turn incoming bytes into events, and holds the current window size (default
`80×24` until the first NAWS event).

State machine:

1. **Negotiate** — write `protocol::initial_negotiation()`. Begin reading; apply
   NAWS updates whenever they arrive (window size can change at any time).
2. **Login** (skipped when `--no-auth`): render `Username:` (echo typed chars),
   read a line; render `Password:` (do **not** echo typed chars — the server owns
   echo via `WILL ECHO`), read a line; look the user up and `verify_password`.
   Wrong credentials → error line + retry. **3 failures → close the connection.**
3. **Search box** — clear screen, draw a prompt box, echo typed characters so the
   user sees the query. **Enter** submits; **Esc** dismisses (see transitions);
   Backspace edits.
4. **Run search** — send the query to the shared embedder worker over a channel,
   await the embedding, call `SearchEngine::search(..)`, take the top result.
   Resolve its relative path via `relative_to_abs_path`, decode via the thumbnail
   cache / `decode_image`, `render_halfblock` to the current window size.
5. **Results** — clear screen, draw the art + a one-line caption
   (`{filename} · {NN}% match`) and a hint line
   (`any key: search · Esc: dismiss`). Match percent is derived from cosine
   distance `d` as `round((1 - d/2) * 100)` clamped to `[0,100]`.

Transitions:

- Results, **any key** → Search box (dialog).
- Search box, **Esc** → if art exists, dismiss and redraw the current art;
  if no art yet (fresh session), show a hint and stay (or a `Goodbye` + close is
  acceptable — implementer's choice, documented).
- Search box, **Enter** with empty query → ignore (stay).
- Search returns **no matches** → a friendly "no matches for …" screen; any key
  returns to the search box.
- Top image fails to decode → fall through to the next result; if none decode,
  show the no-matches screen.

Factor the non-I/O decision logic (what the next state is given an input byte and
current state, and caption/percent formatting) into pure helpers so they can be
unit-tested without a socket.

### `server.rs` — listener, embedder worker, wiring

- `run_server(db, bind, port, auth: AuthMode, max_conns) -> Result<()>`.
- **Embedder worker**: on startup, resolve the active model
  (`db.active_model()`), load `ClipEmbedder::from_model(&name, false)` **once** on
  a dedicated thread. The worker owns the embedder (never shared, so no `Sync`
  bound needed) and serves requests over an `mpsc` channel of
  `(query: String, reply: oneshot::Sender<Result<Vec<f32>>>)`. Each connection
  sends a request and awaits its reply, so all connections share one loaded
  model and embeddings are serialized. If model load fails, the server exits with
  a clear error before accepting connections.
- **Accept loop**: `tokio::net::TcpListener`, a `tokio::sync::Semaphore` sized to
  `max_conns` for back-pressure, `tokio::spawn` one `session::run(..)` per
  connection with a clone of the DB pool and the embed-request sender. Log
  connect/disconnect at `info`.

## Database changes

### Migration 007 (`src/schema.rs`)

- Bump `LATEST_MIGRATION_VERSION` to `7`.
- Add migration 007 (version-gated, `CREATE TABLE IF NOT EXISTS`):
  ```sql
  CREATE TABLE IF NOT EXISTS telnet_users (
      id            INTEGER PRIMARY KEY AUTOINCREMENT,
      username      TEXT NOT NULL UNIQUE,
      password_hash TEXT NOT NULL,
      created_at    TEXT NOT NULL DEFAULT (datetime('now'))
  );
  ```

### `src/database.rs` async methods

- `add_telnet_user(&self, username: &str, password_hash: &str) -> Result<()>` —
  insert; surface a clear error on UNIQUE violation.
- `get_telnet_user(&self, username: &str) -> Result<Option<TelnetUser>>` — where
  `TelnetUser { username, password_hash }`.
- `list_telnet_users(&self) -> Result<Vec<String>>`.
- `remove_telnet_user(&self, username: &str) -> Result<bool>` — true if a row was
  deleted.
- `count_telnet_users(&self) -> Result<usize>`.

Hashing lives in `telnet::auth`; the DB only stores/loads the PHC string.

## Dependencies

Add to root `Cargo.toml` (both pure Rust, no C):

- `argon2` (password hashing) and its `password-hash` companion for
  `SaltString`/`PasswordHash` (typically re-exported by `argon2`).
- `rpassword` (hidden password entry in `telnet-user add`).

No new feature flag — the telnet module compiles in by default; it pulls no heavy
deps (image/tokio already present).

## Concurrency model

- One tokio task per connection → genuinely simultaneous, independent sessions.
- One embedder worker task owns the single loaded model; connections request
  embeddings over a channel (serialized CPU work, shared model, no per-connection
  model load).
- A semaphore bounds concurrent connections (`--max-connections`).
- Per-connection output is independent ANSI (clear + cursor-home + full redraw on
  each state change).

## Security notes

- Plaintext protocol; default bind `127.0.0.1`. `--bind 0.0.0.0` is an explicit
  opt-in to LAN exposure.
- Passwords are Argon2-hashed at rest; plaintext is never stored and the password
  prompt is never echoed (CLI: `rpassword`; telnet: server-owned echo).
- 3 failed login attempts drop the connection.
- Connection cap limits resource exhaustion.

## Testing (TDD)

Pure units get real tests written first:

- `protocol.rs` — data runs, NAWS subnegotiation → cols/rows, byte-split
  sequences, `IAC IAC` escape.
- `render.rs` — solid-color image → expected escape + `▀`, output dimensions,
  aspect-fit bounds.
- `auth.rs` — hash≠plaintext, verify round-trip, wrong password, malformed hash.
- `session.rs` pure helpers — state transitions for (any key / Esc / Enter /
  empty query), caption + percent formatting.

Integration: a lightweight loopback test connects a TCP client to the listener
with `--no-auth` and drives negotiate → search → art, asserting the response
contains ANSI truecolor + `▀`. Keep it minimal (uses the default model; may be
`#[ignore]`d if model download is impractical in CI, mirroring existing
model-dependent tests in `processing.rs`).

## Documentation

- `CLAUDE.md` — new `telnet` module, `telnet` / `telnet-user` subcommands,
  migration 007 / `LATEST_MIGRATION_VERSION = 7`, `telnet_users` table, new deps,
  and a pointer to this spec.
- `USAGE.md` — usage for `telnet` and `telnet-user`.
- `README.md` — a short mention of the fun telnet mode.

## Open implementer choices (documented, low-risk)

- Exact box-drawing/prompt styling of the search box and login screens.
- Behavior of Esc in the fresh (no-art-yet) search box: show a hint and stay, or
  `Goodbye` and close. Pick one and note it.
- Whether to serve the top image from the persisted thumbnail (fast) or a fresh
  decode; prefer the thumbnail cache for speed, falling back to `decode_image`.
