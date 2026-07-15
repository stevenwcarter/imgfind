# Telnet Search Server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `imgfind telnet` subcommand that serves a plaintext telnet search server: users log in, type a natural-language query, and get the top matching image rendered as ANSI truecolor ASCII art, with multiple simultaneous connections sharing one loaded CLIP model.

**Architecture:** A new `src/telnet/` module split into pure, testable units (`protocol`, `render`, `auth`) plus an async `session` state machine and a `server` that owns one shared embedder worker thread. A DB migration adds a `telnet_users` table. The CLI gains `telnet` (start server) and `telnet-user` (manage accounts) subcommands. Reuses existing `SearchEngine`, `decode_image`, thumbnail cache, `Database`, and model-loading code.

**Tech Stack:** Rust 2024, tokio (already full-featured), turso (SQLite), `image` crate, `clipper` (CLIP), new deps `argon2` + `rpassword`.

**Design spec:** `docs/superpowers/specs/2026-07-14-telnet-search-server-design.md` — read it first.

## Global Constraints

- Rust edition 2024. Errors use `anyhow` (`Context`/`with_context`). Logging via `tracing`.
- Imports: prefer `use crate::name` over inline qualified paths (repo convention).
- Image paths in the DB are **relative to the DB parent dir**; convert with `relative_to_abs_path` before filesystem access. `Database.parent_dir` is the base.
- All `Database` methods are `async`; sync callers use `imgfind::block_on(..)`. New DB row-reads use the existing private helpers `col_text(&row, idx, "field")?` / `col_i64(&row, idx, "field")?` (already in `src/database.rs`, in scope inside the `impl Database` block).
- Telnet output uses **CRLF** (`\r\n`) line endings; ANSI truecolor is `\x1b[38;2;R;G;Bm` (fg) / `\x1b[48;2;R;G;Bm` (bg); reset `\x1b[0m`.
- Default bind `127.0.0.1`, default port `2323`, default `--max-connections 16`.
- Passwords are **Argon2-hashed** (never stored or logged in plaintext).
- Build/test: `cargo build --workspace`, `cargo test --workspace`. Run a single module with e.g. `cargo test telnet::protocol`.

---

### Task 1: Dependencies + `telnet::auth` (Argon2 password hashing)

**Files:**
- Modify: `Cargo.toml` (root — add `argon2`, `rpassword`)
- Create: `src/telnet/mod.rs`
- Create: `src/telnet/auth.rs`
- Modify: `src/lib.rs` (add `pub mod telnet;`)

**Interfaces:**
- Produces: `imgfind::telnet::auth::hash_password(plain: &str) -> anyhow::Result<String>` (returns a PHC string); `imgfind::telnet::auth::verify_password(plain: &str, phc: &str) -> bool`.

- [ ] **Step 1: Add dependencies.** In root `Cargo.toml` under `[dependencies]` add:

```toml
argon2 = "0.5"
rpassword = "7"
```

- [ ] **Step 2: Declare the module.** In `src/lib.rs`, add alongside the other `pub mod` lines (keep alphabetical-ish grouping, near `pub mod search;`):

```rust
pub mod telnet;
```

- [ ] **Step 3: Create `src/telnet/mod.rs`:**

```rust
//! Telnet search server: plaintext TCP front-end that returns the top image
//! for a natural-language query as ANSI truecolor ASCII art.
//!
//! See `docs/superpowers/specs/2026-07-14-telnet-search-server-design.md`.

pub mod auth;
```

- [ ] **Step 4: Write the failing test.** Create `src/telnet/auth.rs` with only the tests:

```rust
//! Argon2 password hashing helpers for telnet accounts.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_not_plaintext_and_verifies() {
        let hash = hash_password("hunter2").unwrap();
        assert_ne!(hash, "hunter2");
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password("hunter2", &hash));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let hash = hash_password("hunter2").unwrap();
        assert!(!verify_password("hunter3", &hash));
    }

    #[test]
    fn malformed_hash_never_panics() {
        assert!(!verify_password("anything", "not-a-valid-phc-string"));
    }

    #[test]
    fn two_hashes_of_same_password_differ() {
        // Random salt => different PHC strings, both verify.
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a));
        assert!(verify_password("same", &b));
    }
}
```

- [ ] **Step 5: Run test to verify it fails.** Run: `cargo test telnet::auth`. Expected: FAIL (compile error — `hash_password`/`verify_password` not defined).

- [ ] **Step 6: Implement.** Add above the `#[cfg(test)]` block in `src/telnet/auth.rs`:

```rust
use anyhow::{Result, anyhow};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

/// Hash a plaintext password into an Argon2 PHC string (random salt).
pub fn hash_password(plain: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let phc = Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow!("failed to hash password: {e}"))?
        .to_string();
    Ok(phc)
}

/// Verify a plaintext password against a stored PHC string.
/// Returns `false` for any mismatch or malformed hash — never panics.
pub fn verify_password(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}
```

- [ ] **Step 7: Run tests to verify they pass.** Run: `cargo test telnet::auth`. Expected: PASS (4 tests).

- [ ] **Step 8: Commit.**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/telnet/mod.rs src/telnet/auth.rs
git commit -m "feat(telnet): add argon2 password hashing for telnet accounts"
```

---

### Task 2: `telnet::protocol` (IAC/NAWS parser + negotiation)

**Files:**
- Create: `src/telnet/protocol.rs`
- Modify: `src/telnet/mod.rs` (add `pub mod protocol;`)

**Interfaces:**
- Produces:
  - `enum TelnetEvent { Data(u8), WindowSize { cols: u16, rows: u16 }, Negotiation }`
  - `struct TelnetParser` with `TelnetParser::new() -> Self` and `fn feed(&mut self, bytes: &[u8]) -> Vec<TelnetEvent>`
  - `fn initial_negotiation() -> Vec<u8>`

- [ ] **Step 1: Register the module.** In `src/telnet/mod.rs` add:

```rust
pub mod protocol;
```

- [ ] **Step 2: Write the failing tests.** Create `src/telnet/protocol.rs` with tests only:

```rust
//! Pure telnet byte-stream parser: strips IAC negotiation, surfaces NAWS
//! window-size updates, and yields real user-input bytes.

#[cfg(test)]
mod tests {
    use super::*;

    // Telnet command bytes for building test inputs.
    const IAC: u8 = 255;
    const DO: u8 = 253;
    const WILL: u8 = 251;
    const SB: u8 = 250;
    const SE: u8 = 240;
    const NAWS: u8 = 31;

    #[test]
    fn plain_data_passes_through() {
        let mut p = TelnetParser::new();
        let events = p.feed(b"hi");
        assert_eq!(events, vec![TelnetEvent::Data(b'h'), TelnetEvent::Data(b'i')]);
    }

    #[test]
    fn naws_subnegotiation_yields_window_size() {
        // IAC SB NAWS 0 120 0 40 IAC SE  => 120 cols, 40 rows
        let mut p = TelnetParser::new();
        let bytes = [IAC, SB, NAWS, 0, 120, 0, 40, IAC, SE];
        let events = p.feed(&bytes);
        assert_eq!(events, vec![TelnetEvent::WindowSize { cols: 120, rows: 40 }]);
    }

    #[test]
    fn sequence_split_across_feeds_is_handled() {
        let mut p = TelnetParser::new();
        // Feed a WILL negotiation one byte at a time; expect no Data events.
        assert!(p.feed(&[IAC]).is_empty());
        let ev = p.feed(&[WILL]);
        assert!(ev.iter().all(|e| !matches!(e, TelnetEvent::Data(_))));
        let ev = p.feed(&[NAWS]);
        assert_eq!(ev, vec![TelnetEvent::Negotiation]);
        // Then real data still flows.
        assert_eq!(p.feed(b"x"), vec![TelnetEvent::Data(b'x')]);
    }

    #[test]
    fn escaped_iac_is_a_single_data_byte() {
        let mut p = TelnetParser::new();
        // IAC IAC => literal 0xFF data byte.
        assert_eq!(p.feed(&[IAC, IAC]), vec![TelnetEvent::Data(0xFF)]);
    }

    #[test]
    fn do_command_consumes_option_byte() {
        let mut p = TelnetParser::new();
        // IAC DO NAWS then a data byte.
        let ev = p.feed(&[IAC, DO, NAWS, b'z']);
        assert_eq!(ev, vec![TelnetEvent::Negotiation, TelnetEvent::Data(b'z')]);
    }

    #[test]
    fn initial_negotiation_requests_echo_sga_naws() {
        let bytes = initial_negotiation();
        // Must contain IAC WILL ECHO, IAC WILL SGA, IAC DO NAWS.
        assert!(bytes.windows(3).any(|w| w == [IAC, WILL, 1]));   // ECHO=1
        assert!(bytes.windows(3).any(|w| w == [IAC, WILL, 3]));   // SGA=3
        assert!(bytes.windows(3).any(|w| w == [IAC, DO, NAWS]));  // NAWS=31
    }
}
```

- [ ] **Step 3: Run tests to verify they fail.** Run: `cargo test telnet::protocol`. Expected: FAIL (types not defined).

- [ ] **Step 4: Implement.** Add above the tests in `src/telnet/protocol.rs`:

```rust
// Telnet command bytes (RFC 854 / 1073).
const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_NAWS: u8 = 31;

/// One parsed event from the client byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelnetEvent {
    /// A real user-input byte.
    Data(u8),
    /// A NAWS window-size update.
    WindowSize { cols: u16, rows: u16 },
    /// An observed WILL/WONT/DO/DONT negotiation (we don't act on these).
    Negotiation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Normal data flow.
    Data,
    /// Saw IAC, waiting for the command byte.
    Iac,
    /// Saw IAC + (WILL/WONT/DO/DONT), waiting for the option byte.
    Negotiate,
    /// Inside a subnegotiation (after IAC SB), collecting bytes until IAC SE.
    Subneg,
    /// Inside a subnegotiation and just saw IAC (waiting for SE or escaped IAC).
    SubnegIac,
}

/// Incremental telnet parser. Feed it raw socket bytes; get clean events.
pub struct TelnetParser {
    state: State,
    sb: Vec<u8>,
}

impl TelnetParser {
    pub fn new() -> Self {
        Self { state: State::Data, sb: Vec::new() }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<TelnetEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                State::Data => {
                    if b == IAC {
                        self.state = State::Iac;
                    } else {
                        out.push(TelnetEvent::Data(b));
                    }
                }
                State::Iac => match b {
                    IAC => {
                        // Escaped literal 0xFF.
                        out.push(TelnetEvent::Data(0xFF));
                        self.state = State::Data;
                    }
                    SB => {
                        self.sb.clear();
                        self.state = State::Subneg;
                    }
                    WILL | WONT | DO | DONT => {
                        self.state = State::Negotiate;
                    }
                    _ => {
                        // Standalone command (e.g. GA, NOP) — ignore.
                        self.state = State::Data;
                    }
                },
                State::Negotiate => {
                    // b is the option byte; we don't act on peer negotiation.
                    let _ = b;
                    out.push(TelnetEvent::Negotiation);
                    self.state = State::Data;
                }
                State::Subneg => {
                    if b == IAC {
                        self.state = State::SubnegIac;
                    } else {
                        self.sb.push(b);
                    }
                }
                State::SubnegIac => match b {
                    IAC => {
                        // Escaped 0xFF inside subnegotiation payload.
                        self.sb.push(0xFF);
                        self.state = State::Subneg;
                    }
                    SE => {
                        if let Some(ev) = parse_subneg(&self.sb) {
                            out.push(ev);
                        }
                        self.sb.clear();
                        self.state = State::Data;
                    }
                    _ => {
                        // Unexpected; abandon subnegotiation.
                        self.sb.clear();
                        self.state = State::Data;
                    }
                },
            }
        }
        out
    }
}

impl Default for TelnetParser {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a completed subnegotiation payload (option byte + data).
fn parse_subneg(sb: &[u8]) -> Option<TelnetEvent> {
    if sb.first() == Some(&OPT_NAWS) && sb.len() >= 5 {
        let cols = u16::from_be_bytes([sb[1], sb[2]]);
        let rows = u16::from_be_bytes([sb[3], sb[4]]);
        return Some(TelnetEvent::WindowSize { cols, rows });
    }
    None
}

/// Bytes the server sends on connect: take over echo, suppress go-ahead,
/// and ask the client for its window size.
pub fn initial_negotiation() -> Vec<u8> {
    vec![
        IAC, WILL, OPT_ECHO,
        IAC, WILL, OPT_SGA,
        IAC, DO, OPT_NAWS,
    ]
}
```

- [ ] **Step 5: Run tests to verify they pass.** Run: `cargo test telnet::protocol`. Expected: PASS (6 tests).

- [ ] **Step 6: Commit.**

```bash
git add src/telnet/mod.rs src/telnet/protocol.rs
git commit -m "feat(telnet): IAC/NAWS telnet protocol parser"
```

---

### Task 3: `telnet::render` (color half-block ASCII art)

**Files:**
- Create: `src/telnet/render.rs`
- Modify: `src/telnet/mod.rs` (add `pub mod render;`)

**Interfaces:**
- Produces: `fn render_halfblock(img: &image::DynamicImage, cols: u16, rows: u16) -> String`

- [ ] **Step 1: Register the module.** In `src/telnet/mod.rs` add:

```rust
pub mod render;
```

- [ ] **Step 2: Write the failing tests.** Create `src/telnet/render.rs` with tests only:

```rust
//! Render a decoded image to ANSI truecolor half-block "ASCII art".

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, image::Rgb(rgb)))
    }

    #[test]
    fn single_cell_solid_red_has_fg_bg_and_halfblock() {
        let img = solid(4, 4, [255, 0, 0]);
        let out = render_halfblock(&img, 1, 1);
        // Truecolor fg + bg for red, upper-half-block glyph, CRLF line ending.
        assert!(out.contains("\u{1b}[38;2;255;0;0m"));
        assert!(out.contains("\u{1b}[48;2;255;0;0m"));
        assert!(out.contains('\u{2580}')); // ▀
        assert!(out.contains("\r\n"));
    }

    #[test]
    fn output_row_count_matches_requested_rows() {
        let img = solid(10, 10, [10, 20, 30]);
        let out = render_halfblock(&img, 8, 5);
        // One CRLF per rendered row.
        assert_eq!(out.matches("\r\n").count(), 5);
    }

    #[test]
    fn aspect_fit_never_exceeds_requested_bounds() {
        // Wide image into a square budget: width fills, height <= budget.
        let img = solid(200, 50, [0, 0, 0]);
        let out = render_halfblock(&img, 40, 40);
        assert!(out.matches("\r\n").count() <= 40);
    }

    #[test]
    fn ends_with_reset() {
        let img = solid(4, 4, [1, 2, 3]);
        let out = render_halfblock(&img, 2, 2);
        assert!(out.trim_end().ends_with("\u{1b}[0m") || out.contains("\u{1b}[0m"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail.** Run: `cargo test telnet::render`. Expected: FAIL (`render_halfblock` not defined).

- [ ] **Step 4: Implement.** Add above the tests in `src/telnet/render.rs`:

```rust
use image::imageops::FilterType;

/// Render `img` as ANSI truecolor half-blocks that fit within `cols × rows`
/// character cells (each cell is 1px wide and 2px tall). Preserves aspect
/// ratio; lines end with CRLF for telnet clients.
pub fn render_halfblock(img: &image::DynamicImage, cols: u16, rows: u16) -> String {
    let cols = cols.max(1) as u32;
    let rows = rows.max(1) as u32;
    // Target pixel grid: cols wide, rows*2 tall (two vertical pixels per cell).
    let target_w = cols;
    let target_h = rows * 2;

    // Fit within the target while preserving aspect ratio.
    let (iw, ih) = (img.width().max(1), img.height().max(1));
    let scale = (target_w as f32 / iw as f32).min(target_h as f32 / ih as f32);
    let out_w = ((iw as f32 * scale).round() as u32).clamp(1, target_w);
    let out_h = ((ih as f32 * scale).round() as u32).clamp(1, target_h);

    let rgb = img
        .resize_exact(out_w, out_h, FilterType::Triangle)
        .to_rgb8();

    let mut s = String::new();
    let mut y = 0;
    while y < out_h {
        for x in 0..out_w {
            let top = rgb.get_pixel(x, y).0;
            let bottom = if y + 1 < out_h {
                rgb.get_pixel(x, y + 1).0
            } else {
                [0, 0, 0]
            };
            s.push_str(&format!(
                "\u{1b}[38;2;{};{};{}m\u{1b}[48;2;{};{};{}m\u{2580}",
                top[0], top[1], top[2], bottom[0], bottom[1], bottom[2]
            ));
        }
        s.push_str("\u{1b}[0m\r\n");
        y += 2;
    }
    s
}
```

- [ ] **Step 5: Run tests to verify they pass.** Run: `cargo test telnet::render`. Expected: PASS (4 tests).

Note (documented implementer choice from the spec): serving from the persisted thumbnail cache vs a fresh decode is decided in the session task; `render_halfblock` itself is source-agnostic.

- [ ] **Step 6: Commit.**

```bash
git add src/telnet/mod.rs src/telnet/render.rs
git commit -m "feat(telnet): color half-block ASCII art renderer"
```

---

### Task 4: DB migration 007 + `telnet_users` methods

**Files:**
- Modify: `src/schema.rs` (bump `LATEST_MIGRATION_VERSION`, add migration 007)
- Modify: `src/database.rs` (add `TelnetUser` struct + 5 async methods)
- Test: inline `#[cfg(test)]` in `src/database.rs` (follow existing DB test style — build a temp DB)

**Interfaces:**
- Produces on `Database`:
  - `async fn add_telnet_user(&self, username: &str, password_hash: &str) -> Result<()>`
  - `async fn get_telnet_user(&self, username: &str) -> Result<Option<TelnetUser>>`
  - `async fn list_telnet_users(&self) -> Result<Vec<String>>`
  - `async fn remove_telnet_user(&self, username: &str) -> Result<bool>`
  - `async fn count_telnet_users(&self) -> Result<usize>`
  - `pub struct TelnetUser { pub username: String, pub password_hash: String }`

- [ ] **Step 1: Bump the version.** In `src/schema.rs`, change:

```rust
pub const LATEST_MIGRATION_VERSION: i32 = 6;
```
to
```rust
pub const LATEST_MIGRATION_VERSION: i32 = 7;
```

- [ ] **Step 2: Wire migration 007 into the runner.** In `src/schema.rs` `run_migrations`, after the `if current < 6 { … }` block and before the `if current < LATEST_MIGRATION_VERSION` stamp block, add:

```rust
    if current < 7 {
        migration_007_telnet_users(conn)
            .await
            .context("migration 7 (telnet users)")?;
    }
```

- [ ] **Step 3: Add the migration function.** In `src/schema.rs`, near the other `migration_00*` functions, add:

```rust
/// Migration 7: telnet user accounts (username + Argon2 password hash).
async fn migration_007_telnet_users(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS telnet_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            username TEXT NOT NULL UNIQUE, \
            password_hash TEXT NOT NULL, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create telnet_users")?;
    Ok(())
}
```

- [ ] **Step 4: Write the failing test.** In `src/database.rs`, find the existing `#[cfg(test)] mod tests` block. Add a test that mirrors the existing temp-DB setup (look at a nearby test such as the tag or favorites tests for the exact `Database::new` + tempfile idiom; reuse whatever helper those tests use). Add:

```rust
    #[tokio::test]
    async fn telnet_users_crud_roundtrip() {
        // Use the same temp-DB construction the other DB tests use.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join(".imgfind").join("imgfind.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let db = Database::new(&db_path).await.unwrap();

        assert_eq!(db.count_telnet_users().await.unwrap(), 0);
        db.add_telnet_user("alice", "phc-hash-a").await.unwrap();
        db.add_telnet_user("bob", "phc-hash-b").await.unwrap();
        assert_eq!(db.count_telnet_users().await.unwrap(), 2);

        let mut names = db.list_telnet_users().await.unwrap();
        names.sort();
        assert_eq!(names, vec!["alice".to_string(), "bob".to_string()]);

        let alice = db.get_telnet_user("alice").await.unwrap().unwrap();
        assert_eq!(alice.username, "alice");
        assert_eq!(alice.password_hash, "phc-hash-a");
        assert!(db.get_telnet_user("nobody").await.unwrap().is_none());

        // Duplicate username errors.
        assert!(db.add_telnet_user("alice", "x").await.is_err());

        assert!(db.remove_telnet_user("alice").await.unwrap());
        assert!(!db.remove_telnet_user("alice").await.unwrap());
        assert_eq!(db.count_telnet_users().await.unwrap(), 1);
    }
```

> If the existing DB tests use a shared helper (e.g. `test_db().await`) instead of the inline tempfile setup above, use that helper for consistency — check the top of the `mod tests` block first.

- [ ] **Step 5: Run test to verify it fails.** Run: `cargo test telnet_users_crud_roundtrip`. Expected: FAIL (methods/`TelnetUser` not defined).

- [ ] **Step 6: Implement.** In `src/database.rs`, add the struct near the other row structs (top of file area where structs like `ImageSearchResult` live):

```rust
/// A telnet account row.
pub struct TelnetUser {
    pub username: String,
    pub password_hash: String,
}
```

Then inside `impl Database`, add the methods (mirroring the `list_tags`/`list_models` style, using `col_text`):

```rust
    /// Insert a telnet user. Errors on duplicate username (UNIQUE constraint).
    pub async fn add_telnet_user(&self, username: &str, password_hash: &str) -> Result<()> {
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT INTO telnet_users (username, password_hash) VALUES (?1, ?2)",
            (username.to_string(), password_hash.to_string()),
        )
        .await
        .with_context(|| format!("add telnet user '{username}'"))?;
        Ok(())
    }

    /// Look up a telnet user by username.
    pub async fn get_telnet_user(&self, username: &str) -> Result<Option<TelnetUser>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT username, password_hash FROM telnet_users WHERE username = ?1",
                (username.to_string(),),
            )
            .await?;
        if let Some(row) = rows.next().await? {
            Ok(Some(TelnetUser {
                username: col_text(&row, 0, "username")?,
                password_hash: col_text(&row, 1, "password_hash")?,
            }))
        } else {
            Ok(None)
        }
    }

    /// List all telnet usernames, ordered.
    pub async fn list_telnet_users(&self) -> Result<Vec<String>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT username FROM telnet_users ORDER BY username", ())
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(col_text(&row, 0, "username")?);
        }
        Ok(out)
    }

    /// Remove a telnet user. Returns true if a row was deleted.
    pub async fn remove_telnet_user(&self, username: &str) -> Result<bool> {
        let conn = self.pool.get().await.context("get connection")?;
        let affected = conn
            .execute(
                "DELETE FROM telnet_users WHERE username = ?1",
                (username.to_string(),),
            )
            .await
            .with_context(|| format!("remove telnet user '{username}'"))?;
        Ok(affected > 0)
    }

    /// Count telnet users.
    pub async fn count_telnet_users(&self) -> Result<usize> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT COUNT(*) FROM telnet_users", ())
            .await?;
        if let Some(row) = rows.next().await? {
            Ok(col_i64(&row, 0, "count")? as usize)
        } else {
            Ok(0)
        }
    }
```

> Note: `conn.execute` in turso returns the affected-row count (`u64`); if the local signature differs, adapt the `remove_telnet_user` return accordingly (compare against the `DELETE FROM favorites` call already in this file for the exact type).

- [ ] **Step 7: Run tests to verify they pass.** Run: `cargo test telnet_users_crud_roundtrip`. Expected: PASS. Then `cargo test --workspace` to confirm the migration bump didn't break other schema tests.

- [ ] **Step 8: Commit.**

```bash
git add src/schema.rs src/database.rs
git commit -m "feat(telnet): telnet_users table (migration 007) + DB accessors"
```

---

### Task 5: CLI `telnet-user` subcommands (add / list / remove)

**Files:**
- Modify: `src/main.rs` (add `TelnetUser` command + nested action enum + handler)

**Interfaces:**
- Consumes: `Database::{add_telnet_user, list_telnet_users, remove_telnet_user}`, `imgfind::telnet::auth::hash_password`, `imgfind::block_on`, `get_db_path`.
- Produces: CLI `imgfind telnet-user add <NAME> [-d DIR]`, `imgfind telnet-user list [-d DIR]`, `imgfind telnet-user remove <NAME> [-d DIR]`.

- [ ] **Step 1: Add the command variant.** In `src/main.rs`, in the `enum Commands`, add:

```rust
    /// Manage telnet server accounts
    TelnetUser {
        #[command(subcommand)]
        action: TelnetUserAction,
        /// Database directory (walk-up/global resolution)
        #[arg(short, long)]
        dir: Option<String>,
    },
```

- [ ] **Step 2: Add the action enum.** In `src/main.rs`, near the other subcommand enums (e.g. `ModelsAction`), add:

```rust
#[derive(Subcommand)]
enum TelnetUserAction {
    /// Add a user (prompts for a password, stored Argon2-hashed)
    Add {
        /// Username
        name: String,
    },
    /// List usernames
    List,
    /// Remove a user
    Remove {
        /// Username
        name: String,
    },
}
```

- [ ] **Step 3: Add the handler.** In `src/main.rs`, in the `match cli.command` block, add an arm (mirror the `Commands::Models` arm's DB setup):

```rust
        Commands::TelnetUser { action, dir } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = imgfind::block_on(Database::new(&db_path))?;
            match action {
                TelnetUserAction::Add { name } => {
                    let pw = rpassword::prompt_password(format!("Password for '{name}': "))
                        .context("failed to read password")?;
                    let confirm = rpassword::prompt_password("Confirm password: ")
                        .context("failed to read password")?;
                    if pw != confirm {
                        anyhow::bail!("passwords do not match");
                    }
                    if pw.is_empty() {
                        anyhow::bail!("password must not be empty");
                    }
                    let hash = imgfind::telnet::auth::hash_password(&pw)?;
                    imgfind::block_on(db.add_telnet_user(&name, &hash))
                        .with_context(|| format!("failed to add user '{name}' (already exists?)"))?;
                    println!("Added telnet user '{name}'.");
                }
                TelnetUserAction::List => {
                    let users = imgfind::block_on(db.list_telnet_users())?;
                    if users.is_empty() {
                        println!("No telnet users. Add one with: imgfind telnet-user add <name>");
                    } else {
                        for u in users {
                            println!("{u}");
                        }
                    }
                }
                TelnetUserAction::Remove { name } => {
                    let removed = imgfind::block_on(db.remove_telnet_user(&name))?;
                    if removed {
                        println!("Removed telnet user '{name}'.");
                    } else {
                        println!("No such telnet user '{name}'.");
                    }
                }
            }
        }
```

- [ ] **Step 4: Build & manually verify.** Run: `cargo build`. Then:

```bash
cargo run -- telnet-user add tester   # type a password twice at the prompt
cargo run -- telnet-user list         # should print: tester
cargo run -- telnet-user remove tester
```

Expected: add prints "Added telnet user 'tester'.", list shows it, remove confirms. (This uses whatever DB `get_db_path` resolves — run from a directory with an `.imgfind` DB, or use `-d`.)

- [ ] **Step 5: Commit.**

```bash
git add src/main.rs
git commit -m "feat(telnet): telnet-user add/list/remove CLI subcommands"
```

---

### Task 6: `telnet::session` — pure state helpers + async runner

**Files:**
- Create: `src/telnet/session.rs`
- Modify: `src/telnet/mod.rs` (add `pub mod session;`)

**Interfaces:**
- Consumes: `crate::telnet::protocol::{TelnetParser, TelnetEvent, initial_negotiation}`, `crate::telnet::render::render_halfblock`, `crate::telnet::auth::verify_password`, `crate::database::Database`, `crate::search::SearchEngine`, `crate::relative_to_abs_path`, `crate::decode::decode_image`, `crate::units::{DistanceThreshold, MaxK}`. Embedder access is via an `EmbedHandle` (defined in Task 7) — for this task, define the trait/closure boundary so `session::run` takes something it can call to embed text.
- Produces:
  - `fn match_percent(distance: f32) -> u8` (pure)
  - `enum Screen { Login, SearchBox, Results, NoResults }` and `fn next_screen_on_key(current: Screen, byte: u8, has_art: bool) -> Screen` (pure)
  - `fn caption(filename: &str, percent: u8) -> String` (pure)
  - `async fn run(stream: tokio::net::TcpStream, ctx: SessionCtx) -> anyhow::Result<()>` where `SessionCtx` bundles `Arc<Database>`, an embed sender, `auth: bool`, threshold/max_k.

- [ ] **Step 1: Register the module.** In `src/telnet/mod.rs` add:

```rust
pub mod session;
```

- [ ] **Step 2: Write the failing tests (pure helpers).** Create `src/telnet/session.rs` with tests only:

```rust
//! Per-connection telnet session: negotiate, login, search, render.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_percent_maps_distance_to_0_100() {
        assert_eq!(match_percent(0.0), 100); // identical
        assert_eq!(match_percent(2.0), 0);   // opposite
        assert_eq!(match_percent(1.0), 50);  // orthogonal
        // Clamps out-of-range distances.
        assert_eq!(match_percent(-0.5), 100);
        assert_eq!(match_percent(3.0), 0);
    }

    #[test]
    fn any_key_on_results_opens_search_box() {
        assert_eq!(next_screen_on_key(Screen::Results, b'x', true), Screen::SearchBox);
        assert_eq!(next_screen_on_key(Screen::Results, b' ', true), Screen::SearchBox);
    }

    #[test]
    fn esc_in_search_box_returns_to_results_when_art_exists() {
        // ESC = 0x1b
        assert_eq!(next_screen_on_key(Screen::SearchBox, 0x1b, true), Screen::Results);
    }

    #[test]
    fn esc_in_search_box_with_no_art_stays_in_search_box() {
        assert_eq!(next_screen_on_key(Screen::SearchBox, 0x1b, false), Screen::SearchBox);
    }

    #[test]
    fn any_key_on_no_results_opens_search_box() {
        assert_eq!(next_screen_on_key(Screen::NoResults, b'k', false), Screen::SearchBox);
    }

    #[test]
    fn caption_includes_filename_and_percent() {
        let c = caption("beach.jpg", 92);
        assert!(c.contains("beach.jpg"));
        assert!(c.contains("92%"));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail.** Run: `cargo test telnet::session`. Expected: FAIL.

- [ ] **Step 4: Implement the pure helpers.** Add above the tests in `src/telnet/session.rs`:

```rust
/// Which screen the client is currently looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    SearchBox,
    Results,
    NoResults,
}

/// Map a cosine distance in [0, 2] to a 0–100 "match" percentage.
pub fn match_percent(distance: f32) -> u8 {
    let pct = ((1.0 - distance / 2.0) * 100.0).round();
    pct.clamp(0.0, 100.0) as u8
}

/// Given the current screen and a pressed byte, decide the next screen.
/// `has_art` is whether a result is currently rendered (affects Esc).
pub fn next_screen_on_key(current: Screen, byte: u8, has_art: bool) -> Screen {
    const ESC: u8 = 0x1b;
    match current {
        Screen::Results => Screen::SearchBox,
        Screen::NoResults => Screen::SearchBox,
        Screen::SearchBox => {
            if byte == ESC {
                if has_art { Screen::Results } else { Screen::SearchBox }
            } else {
                Screen::SearchBox
            }
        }
        Screen::Login => Screen::Login,
    }
}

/// One-line caption under the art.
pub fn caption(filename: &str, percent: u8) -> String {
    format!("{filename} \u{00b7} {percent}% match")
}
```

- [ ] **Step 5: Run tests to verify they pass.** Run: `cargo test telnet::session`. Expected: PASS (6 tests).

- [ ] **Step 6: Commit the pure helpers.**

```bash
git add src/telnet/mod.rs src/telnet/session.rs
git commit -m "feat(telnet): pure session-state helpers (screen transitions, match %)"
```

- [ ] **Step 7: Implement the async session runner.** This is I/O glue (no unit test; covered by the integration test in Task 8). Add to `src/telnet/session.rs`. Read the design spec's "Connection lifecycle" section and implement:

```rust
use std::sync::Arc;
use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::database::Database;
use crate::decode::decode_image;
use crate::relative_to_abs_path;
use crate::search::SearchEngine;
use crate::telnet::protocol::{TelnetEvent, TelnetParser, initial_negotiation};
use crate::telnet::render::render_halfblock;
use crate::units::{DistanceThreshold, MaxK};

/// A request to the shared embedder worker (defined/wired in server.rs).
pub struct EmbedRequest {
    pub query: String,
    pub reply: oneshot::Sender<Result<Vec<f32>>>,
}

/// Everything a session needs, cloned per connection.
pub struct SessionCtx {
    pub db: Arc<Database>,
    pub embed_tx: mpsc::Sender<EmbedRequest>,
    pub auth: bool,
    pub threshold: DistanceThreshold,
    pub max_k: MaxK,
}

const ESC: u8 = 0x1b;
const CR: u8 = b'\r';
const LF: u8 = b'\n';
const BS: u8 = 0x08;
const DEL: u8 = 0x7f;

/// Drive one connection to completion. Errors are logged by the caller.
pub async fn run(mut stream: TcpStream, ctx: SessionCtx) -> Result<()> {
    stream.write_all(&initial_negotiation()).await?;
    stream.flush().await?;

    let mut parser = TelnetParser::new();
    let (mut cols, mut rows): (u16, u16) = (80, 24);
    let mut buf = [0u8; 1024];

    // --- Helper: read the next batch of TelnetEvents (updates window size). ---
    // Inlined below via a loop because closures can't easily borrow `parser`
    // and `stream` at once across awaits.

    // --- Login ---
    if ctx.auth {
        let mut attempts = 0;
        loop {
            write_str(&mut stream, "\x1b[2J\x1b[H\r\nimgfind telnet\r\nUsername: ").await?;
            let username = read_line(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows, true).await?;
            write_str(&mut stream, "\r\nPassword: ").await?;
            let password = read_line(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows, false).await?;

            let ok = match ctx.db.get_telnet_user(username.trim()).await? {
                Some(u) => crate::telnet::auth::verify_password(&password, &u.password_hash),
                None => false,
            };
            if ok {
                break;
            }
            attempts += 1;
            if attempts >= 3 {
                write_str(&mut stream, "\r\nToo many failed attempts. Goodbye.\r\n").await?;
                return Ok(());
            }
            write_str(&mut stream, "\r\nInvalid credentials.\r\n").await?;
        }
    }

    // --- Search / results loop ---
    let mut current_art: Option<String> = None; // full screen (art + caption)
    loop {
        // Draw the search box.
        write_str(
            &mut stream,
            "\x1b[2J\x1b[H\r\nSearch (Enter to run, Esc to dismiss):\r\n> ",
        )
        .await?;
        // Read a query line; Esc during input dismisses to art if present.
        let query = match read_query(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await? {
            QueryOutcome::Submit(q) => q,
            QueryOutcome::Dismiss => {
                if let Some(art) = &current_art {
                    write_str(&mut stream, art).await?;
                    // Wait for any key, then loop back to the search box.
                    wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
                }
                continue;
            }
            QueryOutcome::Closed => return Ok(()),
        };
        if query.trim().is_empty() {
            continue;
        }

        // Embed via the shared worker.
        let (tx, rx) = oneshot::channel();
        ctx.embed_tx
            .send(EmbedRequest { query: query.trim().to_string(), reply: tx })
            .await
            .context("embed worker gone")?;
        let embedding = match rx.await.context("embed worker dropped reply")? {
            Ok(e) => e,
            Err(e) => {
                write_str(&mut stream, &format!("\r\nSearch error: {e}\r\n")).await?;
                continue;
            }
        };

        // Search top result.
        let engine = SearchEngine::new(&ctx.db);
        let results = engine.search(&embedding, 5, ctx.threshold, ctx.max_k).await?;

        // Find the first result whose image decodes.
        let mut shown: Option<(String, f32, image::DynamicImage)> = None;
        for (rel, dist) in &results {
            let abs = relative_to_abs_path(std::path::Path::new(rel), &ctx.db.parent_dir);
            let decoded = tokio::task::spawn_blocking({
                let abs = abs.clone();
                move || decode_image(&abs)
            })
            .await;
            if let Ok(Ok(img)) = decoded {
                shown = Some((rel.clone(), *dist, img));
                break;
            }
        }

        match shown {
            Some((rel, dist, img)) => {
                let art = render_halfblock(&img, cols, rows.saturating_sub(2).max(1));
                let filename = std::path::Path::new(&rel)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| rel.clone());
                let pct = match_percent(dist);
                let screen = format!(
                    "\x1b[2J\x1b[H{art}\x1b[0m\r\n{}\r\n(any key: search \u{00b7} Esc: dismiss)",
                    caption(&filename, pct)
                );
                write_str(&mut stream, &screen).await?;
                current_art = Some(screen);
                // Any key returns to the search box (top of loop).
                wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
            }
            None => {
                write_str(
                    &mut stream,
                    &format!("\x1b[2J\x1b[H\r\nNo matches for \"{}\".\r\n(any key: search)", query.trim()),
                )
                .await?;
                wait_any_key(&mut stream, &mut parser, &mut buf, &mut cols, &mut rows).await?;
            }
        }
    }
}

enum QueryOutcome {
    Submit(String),
    Dismiss,
    Closed,
}

async fn write_str(stream: &mut TcpStream, s: &str) -> Result<()> {
    stream.write_all(s.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Pump one socket read through the parser, applying NAWS updates, returning
/// the data bytes seen. Returns `Ok(None)` on EOF (connection closed).
async fn pump(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
) -> Result<Option<Vec<u8>>> {
    let n = stream.read(buf).await?;
    if n == 0 {
        return Ok(None);
    }
    let mut data = Vec::new();
    for ev in parser.feed(&buf[..n]) {
        match ev {
            TelnetEvent::Data(b) => data.push(b),
            TelnetEvent::WindowSize { cols: c, rows: r } => {
                if c > 0 { *cols = c; }
                if r > 0 { *rows = r; }
            }
            TelnetEvent::Negotiation => {}
        }
    }
    Ok(Some(data))
}

/// Read a line terminated by CR (or LF). If `echo`, echo typed chars back.
async fn read_line(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
    echo: bool,
) -> Result<String> {
    let mut line = String::new();
    loop {
        let data = match pump(stream, parser, buf, cols, rows).await? {
            Some(d) => d,
            None => return Ok(line), // EOF
        };
        for b in data {
            match b {
                CR | LF => return Ok(line),
                BS | DEL => {
                    if line.pop().is_some() && echo {
                        write_str(stream, "\x08 \x08").await?;
                    }
                }
                0 => {}
                _ => {
                    line.push(b as char);
                    if echo {
                        stream.write_all(&[b]).await?;
                        stream.flush().await?;
                    }
                }
            }
        }
    }
}

/// Like `read_line` but Esc yields `Dismiss` and EOF yields `Closed`.
async fn read_query(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
) -> Result<QueryOutcome> {
    let mut line = String::new();
    loop {
        let data = match pump(stream, parser, buf, cols, rows).await? {
            Some(d) => d,
            None => return Ok(QueryOutcome::Closed),
        };
        for b in data {
            match b {
                ESC => return Ok(QueryOutcome::Dismiss),
                CR | LF => return Ok(QueryOutcome::Submit(line)),
                BS | DEL => {
                    if line.pop().is_some() {
                        write_str(stream, "\x08 \x08").await?;
                    }
                }
                0 => {}
                _ => {
                    line.push(b as char);
                    stream.write_all(&[b]).await?;
                    stream.flush().await?;
                }
            }
        }
    }
}

/// Block until any single key arrives (or EOF).
async fn wait_any_key(
    stream: &mut TcpStream,
    parser: &mut TelnetParser,
    buf: &mut [u8],
    cols: &mut u16,
    rows: &mut u16,
) -> Result<()> {
    loop {
        match pump(stream, parser, buf, cols, rows).await? {
            None => return Ok(()),           // EOF: let the outer loop end on next read
            Some(d) if !d.is_empty() => return Ok(()),
            Some(_) => continue,             // only negotiation/NAWS arrived; keep waiting
        }
    }
}
```

- [ ] **Step 8: Build.** Run: `cargo build`. Expected: compiles (the `EmbedRequest`/`SessionCtx` types are consumed by Task 7). Fix any borrow/type issues the compiler flags — the pure helpers already pass their tests, so keep those intact.

- [ ] **Step 9: Commit.**

```bash
git add src/telnet/session.rs
git commit -m "feat(telnet): async per-connection session runner (login, search, render loop)"
```

---

### Task 7: `telnet::server` (embedder worker + listener) + `Telnet` CLI command

**Files:**
- Create: `src/telnet/server.rs`
- Modify: `src/telnet/mod.rs` (add `pub mod server;`)
- Modify: `src/main.rs` (add `Telnet` command + handler)

**Interfaces:**
- Consumes: `session::{run, SessionCtx, EmbedRequest}`, `Database`, `crate::models::ensure_and_activate_model` (via `db.active_model()`), `clipper::ClipEmbedder`, `crate::units::{DistanceThreshold, MaxK}`.
- Produces: `async fn run_server(db: Database, bind: std::net::IpAddr, port: u16, auth: bool, max_connections: usize) -> Result<()>`.

- [ ] **Step 1: Register the module.** In `src/telnet/mod.rs` add:

```rust
pub mod server;
```

- [ ] **Step 2: Implement the server.** Create `src/telnet/server.rs`:

```rust
//! Telnet server: TCP accept loop + a single shared CLIP embedder worker.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc, oneshot};
use tracing::{error, info};

use crate::database::Database;
use crate::telnet::session::{self, EmbedRequest, SessionCtx};
use crate::units::{DistanceThreshold, MaxK};

/// Start the telnet server and run until the listener errors.
pub async fn run_server(
    db: Database,
    bind: IpAddr,
    port: u16,
    auth: bool,
    max_connections: usize,
) -> Result<()> {
    // Resolve the active model name up front (fail fast with a clear message).
    let model_name = db.active_model().await.context("no active model")?.name;

    // Spawn the embedder worker on a dedicated OS thread. It owns the loaded
    // model (never shared, so no Sync bound), and serves embed requests over
    // a channel — all connections share this one loaded model.
    let (embed_tx, mut embed_rx) = mpsc::channel::<EmbedRequest>(64);
    let (ready_tx, ready_rx) = oneshot::channel::<Result<()>>();
    std::thread::spawn(move || {
        let embedder = match clipper::ClipEmbedder::from_model(&model_name, false)
            .context("failed to load CLIP model for telnet server")
        {
            Ok(e) => {
                let _ = ready_tx.send(Ok(()));
                e
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        while let Some(req) = embed_rx.blocking_recv() {
            let result = embedder
                .get_text_embedding(&req.query)
                .context("failed to embed query");
            let _ = req.reply.send(result);
        }
    });
    // Propagate a model-load failure before binding.
    ready_rx.await.context("embed worker died")??;
    info!("telnet: CLIP model '{}' loaded", db.active_model().await?.name);

    let addr = SocketAddr::new(bind, port);
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind telnet listener on {addr}"))?;
    info!("telnet server listening on {addr} (auth={auth}, max={max_connections})");

    let db = Arc::new(db);
    let sem = Arc::new(Semaphore::new(max_connections));

    loop {
        let (stream, peer) = listener.accept().await.context("accept failed")?;
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                info!("telnet: refusing {peer} (connection cap reached)");
                drop(stream);
                continue;
            }
        };
        let ctx = SessionCtx {
            db: db.clone(),
            embed_tx: embed_tx.clone(),
            auth,
            threshold: DistanceThreshold(1.3),
            max_k: MaxK(200),
        };
        info!("telnet: connection from {peer}");
        tokio::spawn(async move {
            let _permit = permit; // released on task end
            if let Err(e) = session::run(stream, ctx).await {
                error!("telnet: session {peer} ended with error: {e}");
            } else {
                info!("telnet: {peer} disconnected");
            }
        });
    }
}
```

> Verify the exact constructors for `DistanceThreshold`/`MaxK` (tuple vs `::new`) against `src/units.rs` and existing call sites in `src/database.rs` tests; adjust if they are newtypes with a different constructor. Verify `db.active_model()` returns a struct with a `.name` field (used in `search_images` in `main.rs`).

- [ ] **Step 3: Add the `Telnet` CLI command.** In `src/main.rs` `enum Commands`, add:

```rust
    /// Start a telnet search server (plaintext; localhost by default)
    Telnet {
        /// Database directory (walk-up/global resolution)
        #[arg(short, long)]
        dir: Option<String>,
        /// Bind address (use 0.0.0.0 to expose to your LAN)
        #[arg(long, default_value = "127.0.0.1")]
        bind: std::net::IpAddr,
        /// Port to listen on
        #[arg(long, default_value_t = 2323)]
        port: u16,
        /// Run without login (open server)
        #[arg(long)]
        no_auth: bool,
        /// Maximum simultaneous connections
        #[arg(long, default_value_t = 16)]
        max_connections: usize,
    },
```

- [ ] **Step 4: Add the handler.** In `src/main.rs` `match cli.command`, add:

```rust
        Commands::Telnet {
            dir,
            bind,
            port,
            no_auth,
            max_connections,
        } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = imgfind::block_on(Database::new(&db_path))?;
            let auth = !no_auth;
            if auth {
                let count = imgfind::block_on(db.count_telnet_users())?;
                if count == 0 {
                    anyhow::bail!(
                        "no telnet users exist. Add one with `imgfind telnet-user add <name>`, \
                         or pass --no-auth to run without login."
                    );
                }
            }
            println!("Starting telnet server on {bind}:{port} (auth={auth}). Ctrl-C to stop.");
            imgfind::block_on(imgfind::telnet::server::run_server(
                db,
                bind,
                port,
                auth,
                max_connections,
            ))?;
        }
```

- [ ] **Step 5: Build & manual smoke test.** Run: `cargo build`. Then in a directory with an indexed `.imgfind` DB (with embeddings):

```bash
# terminal A (open mode for a quick smoke test):
cargo run -- telnet --no-auth
# terminal B:
telnet 127.0.0.1 2323
# type a query, press Enter -> ASCII art; press a key -> search box; Esc -> art.
```

Expected: art renders in color; interaction matches the spec. (With auth: first `cargo run -- telnet-user add me`, then `cargo run -- telnet` and log in.)

- [ ] **Step 6: Commit.**

```bash
git add src/telnet/mod.rs src/telnet/server.rs src/main.rs
git commit -m "feat(telnet): TCP server with shared embedder worker + telnet CLI command"
```

---

### Task 8: Loopback integration test (no-auth path)

**Files:**
- Create: `tests/telnet_loopback.rs`

**Interfaces:**
- Consumes: `imgfind::telnet::server::run_server`, `imgfind::database::Database`. Uses a real TCP client (`std::net::TcpStream` or `tokio::net::TcpStream`).

- [ ] **Step 1: Write the test.** Create `tests/telnet_loopback.rs`. This test needs an indexed DB with at least one embedded image and loads the default CLIP model, so mark it `#[ignore]` (mirrors the model-dependent tests in `src/processing.rs` that are gated for the same reason). Document how to run it.

```rust
//! Loopback smoke test for the telnet server. Requires the default CLIP model
//! (downloaded on first use) and is therefore #[ignore]d by default.
//!
//! Run with: cargo test --test telnet_loopback -- --ignored --nocapture

use std::io::{Read, Write};
use std::time::Duration;

#[ignore]
#[test]
fn telnet_no_auth_search_returns_ansi_art() {
    // Build a temp DB, index a tiny generated image, embed it, then start the
    // server on an ephemeral port and drive one search over a TCP socket.
    //
    // Implementation notes for the engineer:
    // 1. Create a tempdir with `.imgfind/imgfind.db` via `Database::new`.
    // 2. Write a small solid-color PNG into the tempdir, index it, and run the
    //    processing pipeline (or insert an image + embedding directly) so a
    //    search can return it. Reuse helpers from `src/processing.rs` tests if
    //    accessible, otherwise index via the library API.
    // 3. Bind the server on 127.0.0.1:0 is not exposed by run_server (fixed
    //    port), so pass a high fixed port (e.g. 12323) and connect to it.
    //    Spawn run_server on a std::thread with its own runtime via
    //    imgfind::block_on, or a #[tokio::test] with tokio::spawn.
    // 4. Connect a client, send b"cat\r", read the response with a short
    //    read timeout, and assert it contains the truecolor prefix
    //    "\x1b[38;2;" and the half-block char "\u{2580}".
    //
    // Keep the assertion minimal and robust:
    let _ = (Duration::from_millis(500), b"cat\r");
    // assert!(response.contains("\u{1b}[38;2;"));
    // assert!(response.contains('\u{2580}'));
}
```

> This task's deliverable is a runnable (ignored) integration test that, when run manually with `--ignored`, exercises negotiate → search → art end-to-end. If wiring a full index in-test proves heavy, the acceptable minimum is: start `run_server` against a pre-built fixture DB path from an env var (`IMGFIND_TEST_DB`), connect, search, and assert the ANSI art bytes. Document whichever approach you choose in the test's doc comment. Do **not** leave the test asserting nothing — either implement the real drive or convert it to assert on a smaller seam (e.g. `render_halfblock` over a fixture image piped through the session helpers).

- [ ] **Step 2: Verify it compiles and is ignored by default.** Run: `cargo test --test telnet_loopback`. Expected: builds, reports the test as ignored (0 run). Then optionally `cargo test --test telnet_loopback -- --ignored` locally to confirm the real drive works.

- [ ] **Step 3: Commit.**

```bash
git add tests/telnet_loopback.rs
git commit -m "test(telnet): loopback integration smoke test (ignored; needs model)"
```

---

### Task 9: Documentation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `USAGE.md`
- Modify: `README.md`

- [ ] **Step 1: Update `CLAUDE.md`.** In the "CLI commands" paragraph, add `telnet` and `telnet-user` to the command list. In the "Architecture" section, add a bullet:

```markdown
- **Telnet server (`src/telnet/`)** — an experimental `imgfind telnet` plaintext telnet search server. `protocol.rs` (pure IAC/NAWS parser), `render.rs` (color truecolor half-block ASCII art), `auth.rs` (Argon2 password hashing), `session.rs` (per-connection async state machine: negotiate → login → search → art; pure transition helpers unit-tested), `server.rs` (TCP accept loop + one shared embedder worker thread owning the loaded `ClipEmbedder`, so all connections share one model). Accounts live in the `telnet_users` table (migration 007); manage with `imgfind telnet-user add|list|remove`. Login required by default (`--no-auth` opts out); default bind `127.0.0.1:2323`. See `docs/superpowers/specs/2026-07-14-telnet-search-server-design.md`.
```

Also update the schema/migration description: bump the migration list to note `007 adds telnet_users` and `LATEST_MIGRATION_VERSION = 7`.

- [ ] **Step 2: Update `USAGE.md`.** Add a section documenting:

```markdown
## Telnet search server (experimental)

Start a plaintext telnet server that returns the top match as color ASCII art:

    imgfind telnet-user add alice     # create an account (prompts for password)
    imgfind telnet                    # start (localhost:2323, login required)
    imgfind telnet --no-auth          # open server, no login
    imgfind telnet --bind 0.0.0.0 --port 2323   # expose to the LAN (plaintext!)

Connect with any telnet client:

    telnet 127.0.0.1 2323

Type a query and press Enter to see the top image as ASCII art. Press any key to
search again; press Esc in the search box to dismiss it and redraw the art.
Manage accounts with `imgfind telnet-user list` / `imgfind telnet-user remove <name>`.

Telnet is unencrypted; the default bind is localhost. Multiple clients can
connect at once (they share one loaded CLIP model).
```

- [ ] **Step 3: Update `README.md`.** Add a short bullet under features mentioning the experimental telnet mode with a one-line usage pointer to `USAGE.md`.

- [ ] **Step 4: Commit.**

```bash
git add CLAUDE.md USAGE.md README.md
git commit -m "docs(telnet): document telnet server and account management"
```

---

## Final verification (after all tasks)

- [ ] Run `cargo build --workspace` — clean build.
- [ ] Run `cargo test --workspace` — all tests pass (the loopback test stays ignored).
- [ ] Run `cargo clippy --workspace` (or `just` equivalent if present) — no new warnings on the added code.
- [ ] Manual end-to-end per Task 7 Step 5 in a real indexed library: add a user, start the server, `telnet` in, log in, search, verify color art + any-key + Esc behavior, and confirm two simultaneous `telnet` sessions both work.
- [ ] Confirm docs (`CLAUDE.md`, `USAGE.md`, `README.md`) match the shipped behavior.

## Self-review notes (author)

- **Spec coverage:** CLI (Tasks 5, 7) · module layout protocol/render/auth/session/server (Tasks 2, 3, 1, 6, 7) · migration 007 + DB methods (Task 4) · deps (Task 1) · concurrency/embedder worker (Task 7) · security: localhost default, Argon2, 3-strike login, conn cap (Tasks 5, 6, 7) · testing (Tasks 1–3, 6, 8) · docs (Task 9). All spec sections map to a task.
- **Type consistency:** `EmbedRequest`/`SessionCtx` defined in Task 6, consumed in Task 7. `match_percent`/`caption`/`Screen`/`next_screen_on_key` defined and used in Task 6. `TelnetUser` + 5 DB methods defined in Task 4, consumed in Tasks 5, 6, 7. `hash_password`/`verify_password` defined in Task 1, consumed in Tasks 5, 6.
- **Known verification points flagged inline** (do not skip): turso `conn.execute` return type for `remove_telnet_user`; `DistanceThreshold`/`MaxK` constructor form; `db.active_model().name`; the existing DB test harness idiom for Task 4's test.
