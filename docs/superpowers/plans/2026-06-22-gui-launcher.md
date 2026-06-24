# imgfind Launcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a standalone `imgfind-launcher` desktop binary that is the OS-level graphical front door: pick a recent/other library to open (spawns `imgfind-gui --dir`), or choose a folder to index (spawns `imgfind index` + `imgfind thumbnails --gui-sizes --all` with CLI output streamed live into a log pane).

**Architecture:** A third Cargo workspace crate producing a native Slint app that is a *pure process orchestrator* — it never opens a Turso DB or loads CLIP. It delegates all heavy work to the existing `imgfind` / `imgfind-gui` binaries spawned as child processes, and persists a machine-managed recent-libraries list at `~/.imgfind/recent.json`. Two small pure helpers are added to the `imgfind` library (`find_db_root_upward`, `resolve_sibling_binary`) and a `thumbnails --all` flag to the CLI.

**Tech Stack:** Rust (edition 2024), Slint 1.16 (`slint` + `slint-build`), `rfd` 0.15 (native folder picker), `serde`/`serde_json`, `anyhow`, `tracing`. Spawning via `std::process::Command`.

## Global Constraints

- Rust edition 2024 for every crate; `imgfind-launcher` Cargo.toml sets `edition = "2024"`.
- Errors use `anyhow` with `Context`/`with_context`. Logging via `tracing`; `RUST_LOG` controls verbosity.
- Slint button/label text uses ASCII / Latin-1 glyphs only (e.g. `×`, `<`, `>`), never symbol glyphs like `✕`/`‹›` — the project's default Slint font renders those as tofu.
- No change to `imgfind-gui`'s existing behavior. The launcher is additive.
- The launcher never opens a Turso DB or loads CLIP; all DB/model/index work is delegated to spawned `imgfind` / `imgfind-gui` child processes.
- The library root convention is exactly `<root>/.imgfind/imgfind.db`.
- Code must be clippy-clean and rustfmt-clean (dispatch Rust work to the rust-developer agent).

---

### Task 1: `find_db_root_upward` library helper

**Files:**
- Modify: `src/lib.rs` (add the helper; refactor `get_db_path` walk-up loop to use it)
- Test: `src/lib.rs` (existing `#[cfg(test)] mod tests` — add tests there)

**Interfaces:**
- Produces: `pub fn find_db_root_upward(start: &std::path::Path) -> Option<std::path::PathBuf>` — walks up from `start` (inclusive), returning the first directory that contains `.imgfind/imgfind.db`, else `None`. Never creates anything, never consults `~/.imgfind`.

- [ ] **Step 1: Write the failing tests**

In `src/lib.rs`'s test module (find it with `grep -n "mod tests" src/lib.rs`; if none exists, add `#[cfg(test)] mod tests { use super::*; use std::fs; ... }` at the end of the file), add:

```rust
#[test]
fn find_db_root_upward_finds_at_start() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    fs::create_dir_all(root.join(".imgfind")).unwrap();
    fs::write(root.join(".imgfind").join("imgfind.db"), b"x").unwrap();
    assert_eq!(find_db_root_upward(&root), Some(root.clone()));
}

#[test]
fn find_db_root_upward_finds_at_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("lib");
    let deep = root.join("a").join("b");
    fs::create_dir_all(&deep).unwrap();
    fs::create_dir_all(root.join(".imgfind")).unwrap();
    fs::write(root.join(".imgfind").join("imgfind.db"), b"x").unwrap();
    assert_eq!(find_db_root_upward(&deep), Some(root.clone()));
}

#[test]
fn find_db_root_upward_none_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp.path().join("a").join("b");
    fs::create_dir_all(&deep).unwrap();
    assert_eq!(find_db_root_upward(&deep), None);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib find_db_root_upward`
Expected: FAIL — `cannot find function find_db_root_upward in this scope`.

- [ ] **Step 3: Implement the helper and refactor `get_db_path`**

Add to `src/lib.rs` (near `get_db_path`):

```rust
/// Walk up from `start` (inclusive) and return the first directory that
/// contains a `.imgfind/imgfind.db`, or `None` if no ancestor does. Pure
/// lookup: never creates anything and never falls back to `~/.imgfind`.
pub fn find_db_root_upward(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".imgfind").join("imgfind.db").exists() {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return None,
        }
    }
}
```

Then refactor the walk-up branch of `get_db_path` (the `loop { ... }` over `current_dir`, lib.rs:61-74) to reuse it, preserving the home fallback:

```rust
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;
    if let Some(root) = find_db_root_upward(&current_dir) {
        return Ok(root.join(".imgfind").join("imgfind.db"));
    }

    // Default to ~/.imgfind/imgfind.db
    let home = home_dir().context("Could not find home directory")?;
    let imgfind_dir = home.join(".imgfind");
    fs::create_dir_all(&imgfind_dir)?;
    Ok(imgfind_dir.join("imgfind.db"))
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib find_db_root_upward && cargo test --lib get_db_path`
Expected: PASS. The `get_db_path` tests (if any) still pass, pinning the refactor's home-fallback invariant. If there is no existing `get_db_path` test, that second command runs zero tests — that is fine.

- [ ] **Step 5: Verify clean build**

Run: `cargo clippy --lib 2>&1 | tail -5`
Expected: no warnings on the changed lines.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs
git commit -m "feat(lib): add find_db_root_upward; reuse it in get_db_path walk-up"
```

---

### Task 2: `resolve_sibling_binary` library helper

**Files:**
- Modify: `src/lib.rs` (add helper + pure core)
- Modify: `src/main.rs` (refactor `launch_gui` to call it, main.rs:350-371)
- Test: `src/lib.rs` test module

**Interfaces:**
- Produces: `pub fn resolve_sibling_binary(name: &str) -> std::ffi::OsString` — returns the path to a sibling-of-current-exe executable named `name` (+ platform `EXE_SUFFIX`) if it exists, else the bare `name` for a `PATH` lookup.
- Internal pure core (testable): `fn sibling_binary_from(current_exe: Option<&Path>, name: &str, exists: impl Fn(&Path) -> bool) -> OsString`.

- [ ] **Step 1: Write the failing tests**

In `src/lib.rs` test module:

```rust
#[test]
fn sibling_binary_prefers_existing_sibling() {
    let exe = PathBuf::from("/opt/app/imgfind");
    let got = sibling_binary_from(Some(&exe), "imgfind-gui", |_p| true);
    let want = PathBuf::from(format!(
        "/opt/app/imgfind-gui{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert_eq!(got, want.into_os_string());
}

#[test]
fn sibling_binary_falls_back_to_bare_name_when_missing() {
    let exe = PathBuf::from("/opt/app/imgfind");
    let got = sibling_binary_from(Some(&exe), "imgfind-gui", |_p| false);
    assert_eq!(got, std::ffi::OsString::from("imgfind-gui"));
}

#[test]
fn sibling_binary_bare_name_when_no_current_exe() {
    let got = sibling_binary_from(None, "imgfind-gui", |_p| true);
    assert_eq!(got, std::ffi::OsString::from("imgfind-gui"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib sibling_binary`
Expected: FAIL — `cannot find function sibling_binary_from`.

- [ ] **Step 3: Implement helper + pure core**

Add to `src/lib.rs`:

```rust
use std::ffi::OsString;

/// Resolve a sibling executable: prefer one next to the current executable
/// (the `install.sh` / cargo target layout), else fall back to the bare name
/// for a `PATH` lookup. Used to spawn `imgfind` / `imgfind-gui` from sibling
/// binaries (e.g. the launcher, `imgfind gui`).
pub fn resolve_sibling_binary(name: &str) -> OsString {
    sibling_binary_from(std::env::current_exe().ok().as_deref(), name, |p| {
        p.exists()
    })
}

fn sibling_binary_from(
    current_exe: Option<&Path>,
    name: &str,
    exists: impl Fn(&Path) -> bool,
) -> OsString {
    if let Some(exe) = current_exe {
        let cand = exe.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if exists(&cand) {
            return cand.into_os_string();
        }
    }
    OsString::from(name)
}
```

(If `use std::ffi::OsString;` collides with an existing import, reuse the existing one.)

- [ ] **Step 4: Refactor `launch_gui` to use it**

In `src/main.rs`, replace the `sibling`/`program` block (lines 351-359) with:

```rust
    let program = imgfind::resolve_sibling_binary("imgfind-gui");
```

Remove the now-unused `use std::ffi::OsString;` inside `launch_gui` if it becomes dead (keep it only if still referenced). Leave the rest of `launch_gui` unchanged.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib sibling_binary && cargo build --bin imgfind`
Expected: tests PASS; binary builds.

- [ ] **Step 6: Commit**

```bash
git add src/lib.rs src/main.rs
git commit -m "feat(lib): add resolve_sibling_binary; use it in launch_gui"
```

---

### Task 3: `thumbnails --all` (loop-until-complete)

**Files:**
- Modify: `src/thumbnail.rs` (add `ThumbnailBatcher` trait, `run_until_complete`, `generate_all_missing_thumbnails`)
- Modify: `src/main.rs` (`Thumbnails` clap variant: add `--all`; dispatch)
- Test: `src/thumbnail.rs` test module

**Interfaces:**
- Produces: `pub fn generate_all_missing_thumbnails(db: &mut Database, size: ThumbnailSize, batch: usize) -> Result<usize>` — generates every missing thumbnail of `size`, looping batches of `batch` until none remain or a batch makes zero progress; returns total generated.
- Consumes (Task 1/2 unrelated): nothing.

- [ ] **Step 1: Write the failing tests (pure control loop)**

Add to `src/thumbnail.rs` test module (`mod tests` at line 194):

```rust
/// Scripted fake batcher: `remaining` and `generated` are popped front-to-back;
/// when a sequence is exhausted its last value repeats.
struct FakeBatcher {
    remaining: std::cell::RefCell<Vec<usize>>,
    generated: std::cell::RefCell<Vec<usize>>,
}
impl FakeBatcher {
    fn pop(seq: &std::cell::RefCell<Vec<usize>>) -> usize {
        let mut v = seq.borrow_mut();
        if v.len() > 1 { v.remove(0) } else { v[0] }
    }
}
impl ThumbnailBatcher for FakeBatcher {
    fn remaining(&mut self) -> Result<usize> { Ok(Self::pop(&self.remaining)) }
    fn generate_batch(&mut self) -> Result<usize> { Ok(Self::pop(&self.generated)) }
}

#[test]
fn run_until_complete_stops_when_none_remain() {
    let mut b = FakeBatcher {
        remaining: std::cell::RefCell::new(vec![5, 0]),
        generated: std::cell::RefCell::new(vec![5]),
    };
    assert_eq!(run_until_complete(&mut b).unwrap(), 5);
}

#[test]
fn run_until_complete_stops_on_zero_progress() {
    // 2 images remain forever (undecodable); each batch generates 0.
    let mut b = FakeBatcher {
        remaining: std::cell::RefCell::new(vec![2]),
        generated: std::cell::RefCell::new(vec![0]),
    };
    // Must terminate (not hang) and report zero generated.
    assert_eq!(run_until_complete(&mut b).unwrap(), 0);
}

#[test]
fn run_until_complete_sums_then_stops_on_zero_progress() {
    let mut b = FakeBatcher {
        remaining: std::cell::RefCell::new(vec![4, 2, 2]),
        generated: std::cell::RefCell::new(vec![2, 0]),
    };
    assert_eq!(run_until_complete(&mut b).unwrap(), 2);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib run_until_complete`
Expected: FAIL — `cannot find trait ThumbnailBatcher` / `cannot find function run_until_complete`.

- [ ] **Step 3: Implement trait, loop, and real wrapper**

Add to `src/thumbnail.rs` (outside the test module):

```rust
/// Abstraction over "how many thumbnails are missing" and "generate one batch",
/// so the loop control in `run_until_complete` can be tested with a fake.
pub trait ThumbnailBatcher {
    fn remaining(&mut self) -> Result<usize>;
    fn generate_batch(&mut self) -> Result<usize>;
}

/// Drive batched generation to completion. Stops when nothing remains, OR when a
/// batch makes zero forward progress (guards against permanently-undecodable
/// images so the loop can never run forever). Returns the total generated.
pub fn run_until_complete(b: &mut impl ThumbnailBatcher) -> Result<usize> {
    let mut total = 0usize;
    loop {
        if b.remaining()? == 0 {
            break;
        }
        let generated = b.generate_batch()?;
        total += generated;
        if generated == 0 {
            break;
        }
    }
    Ok(total)
}

/// Real `ThumbnailBatcher` over a database + target size + per-batch count.
struct DbThumbnailBatcher<'a> {
    db: &'a mut Database,
    size: ThumbnailSize,
    batch: usize,
}
impl ThumbnailBatcher for DbThumbnailBatcher<'_> {
    fn remaining(&mut self) -> Result<usize> {
        block_on(self.db.count_images_without_thumbnails(self.size))
    }
    fn generate_batch(&mut self) -> Result<usize> {
        generate_missing_thumbnails_batch(self.db, self.size, self.batch)
    }
}

/// Generate *every* missing thumbnail of `size`, in batches of `batch`.
pub fn generate_all_missing_thumbnails(
    db: &mut Database,
    size: ThumbnailSize,
    batch: usize,
) -> Result<usize> {
    let mut b = DbThumbnailBatcher { db, size, batch };
    run_until_complete(&mut b)
}
```

- [ ] **Step 4: Wire `--all` into the CLI**

In `src/main.rs`, add to the `Thumbnails` variant (after `gui_sizes`, around line 132):

```rust
        /// Generate ALL missing thumbnails for each requested size (loops in
        /// batches of --count until none remain), instead of a single batch.
        #[arg(long)]
        all: bool,
```

Update the dispatch arm (lines 280-290) to:

```rust
        Commands::Thumbnails {
            size,
            count,
            gui_sizes,
            all,
        } => {
            let db_path = get_db_path(None)?;
            let mut db = imgfind::block_on(Database::new(&db_path))?;
            for s in resolve_thumbnail_sizes(&size, gui_sizes)? {
                if all {
                    let n = imgfind::thumbnail::generate_all_missing_thumbnails(&mut db, s, count)?;
                    println!("Generated {} thumbnails of size {}px (all)", n, s.get());
                } else {
                    generate_thumbnails_batch(&mut db, s, count)?;
                }
            }
        }
```

- [ ] **Step 5: Run tests and build**

Run: `cargo test --lib run_until_complete && cargo build --bin imgfind`
Expected: tests PASS; binary builds. Sanity-check help: `cargo run --bin imgfind -- thumbnails --help | grep -- --all` shows the new flag.

- [ ] **Step 6: Commit**

```bash
git add src/thumbnail.rs src/main.rs
git commit -m "feat(cli): thumbnails --all loops batches until every size is complete"
```

---

### Task 4: Scaffold the `imgfind-launcher` crate

**Files:**
- Modify: `Cargo.toml` (workspace `members`)
- Create: `imgfind-launcher/Cargo.toml`
- Create: `imgfind-launcher/build.rs`
- Create: `imgfind-launcher/ui/launcher.slint`
- Create: `imgfind-launcher/src/main.rs`

**Interfaces:**
- Produces: a buildable `imgfind-launcher` binary that opens an empty titled window. Later tasks add modules `recents`, `runner` and the real UI.

- [ ] **Step 1: Add the workspace member**

Edit `Cargo.toml` line 2:

```toml
members = [".", "imgfind-gui", "imgfind-launcher"]
```

- [ ] **Step 2: Create the crate manifest**

Create `imgfind-launcher/Cargo.toml`:

```toml
[package]
name = "imgfind-launcher"
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"
description = "Desktop launcher for imgfind: pick a library to open or index a new folder."
repository = "https://github.com/stevenwcarter/imgfind"
readme = "../README.md"

[dependencies]
imgfind = { path = ".." }
slint = "1.16"
rfd = "0.15"
clap = { version = "4.0", features = ["derive"] }
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
dirs = "6.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[build-dependencies]
slint-build = "1.16"
```

- [ ] **Step 3: Create `build.rs`**

Create `imgfind-launcher/build.rs`:

```rust
fn main() {
    slint_build::compile("ui/launcher.slint").expect("Slint build failed");
}
```

- [ ] **Step 4: Create a minimal Slint window**

Create `imgfind-launcher/ui/launcher.slint`:

```slint
export component MainWindow inherits Window {
    title: "imgfind";
    preferred-width: 760px;
    preferred-height: 560px;

    Text {
        text: "imgfind launcher";
        font-size: 20px;
    }
}
```

- [ ] **Step 5: Create a minimal main**

Create `imgfind-launcher/src/main.rs`:

```rust
slint::include_modules!();

use anyhow::Result;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let window = MainWindow::new()?;
    window.run()?;
    Ok(())
}
```

- [ ] **Step 6: Build the crate**

Run: `cargo build -p imgfind-launcher`
Expected: builds cleanly. (Do not run `window.run()` in CI/headless; building is the gate here.)

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml imgfind-launcher/
git commit -m "feat(launcher): scaffold imgfind-launcher crate with empty Slint window"
```

---

### Task 5: Recents store (`recents.rs`)

**Files:**
- Create: `imgfind-launcher/src/recents.rs`
- Modify: `imgfind-launcher/src/main.rs` (add `mod recents;`)
- Test: `imgfind-launcher/src/recents.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub struct RecentEntry { pub root: PathBuf, pub last_opened: u64 }` (serde)
  - `pub struct Recents { pub entries: Vec<RecentEntry> }`
  - `pub fn default_recents_path() -> Option<PathBuf>` → `~/.imgfind/recent.json`
  - `Recents::load_from(path: &Path) -> Recents` (missing/corrupt → empty, never errors)
  - `Recents::prune_missing(&mut self)` (drops entries whose `root/.imgfind/imgfind.db` is gone)
  - `Recents::record(&mut self, root: &Path, now: u64)` (canonicalizes if possible, dedups, moves to front, caps at 20)
  - `Recents::save_to(&self, path: &Path) -> Result<()>`
  - `pub const MAX_RECENTS: usize = 20;`

- [ ] **Step 1: Write the failing tests**

Create `imgfind-launcher/src/recents.rs` with the test module first (so the build sees the symbols once implemented):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_lib(parent: &std::path::Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        fs::create_dir_all(root.join(".imgfind")).unwrap();
        fs::write(root.join(".imgfind").join("imgfind.db"), b"x").unwrap();
        root
    }

    #[test]
    fn record_moves_to_front_and_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_lib(tmp.path(), "a");
        let b = make_lib(tmp.path(), "b");
        let mut r = Recents::default();
        r.record(&a, 1);
        r.record(&b, 2);
        r.record(&a, 3); // a back to front, no dup
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].root, a.canonicalize().unwrap());
        assert_eq!(r.entries[1].root, b.canonicalize().unwrap());
    }

    #[test]
    fn record_caps_at_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Recents::default();
        for i in 0..(MAX_RECENTS + 5) {
            let root = make_lib(tmp.path(), &format!("lib{i}"));
            r.record(&root, i as u64);
        }
        assert_eq!(r.entries.len(), MAX_RECENTS);
    }

    #[test]
    fn prune_drops_missing_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_lib(tmp.path(), "a");
        let mut r = Recents::default();
        r.record(&a, 1);
        // Push a bogus entry that does not exist.
        r.entries.push(RecentEntry { root: tmp.path().join("gone"), last_opened: 2 });
        r.prune_missing();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].root, a.canonicalize().unwrap());
    }

    #[test]
    fn load_missing_or_corrupt_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        assert!(Recents::load_from(&missing).entries.is_empty());
        let corrupt = tmp.path().join("bad.json");
        fs::write(&corrupt, b"{ not json").unwrap();
        assert!(Recents::load_from(&corrupt).entries.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_lib(tmp.path(), "a");
        let mut r = Recents::default();
        r.record(&a, 7);
        let path = tmp.path().join("recent.json");
        r.save_to(&path).unwrap();
        let loaded = Recents::load_from(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].last_opened, 7);
    }
}
```

Add `tempfile` as a dev-dependency: append to `imgfind-launcher/Cargo.toml`:

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind-launcher recents`
Expected: FAIL — types/functions not found.

- [ ] **Step 3: Implement the store**

Prepend to `imgfind-launcher/src/recents.rs` (above the test module):

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum number of remembered libraries.
pub const MAX_RECENTS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub root: PathBuf,
    /// Unix epoch seconds of the last open/index.
    pub last_opened: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Recents {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<RecentEntry>,
}

fn one() -> u32 {
    1
}

impl Recents {
    /// Load from `path`; a missing or corrupt file yields an empty store
    /// (logged) — the launcher must always start.
    pub fn load_from(path: &Path) -> Recents {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("ignoring corrupt {}: {e}", path.display());
                Recents::default()
            }),
            Err(_) => Recents::default(),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serializing recents")?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Drop entries whose library DB no longer exists.
    pub fn prune_missing(&mut self) {
        self.entries
            .retain(|e| e.root.join(".imgfind").join("imgfind.db").exists());
    }

    /// Record `root` as most-recently-used: canonicalize, dedup, move to front,
    /// cap at `MAX_RECENTS`.
    pub fn record(&mut self, root: &Path, now: u64) {
        let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.entries.retain(|e| e.root != key);
        self.entries.insert(
            0,
            RecentEntry {
                root: key,
                last_opened: now,
            },
        );
        self.entries.truncate(MAX_RECENTS);
    }
}

/// `~/.imgfind/recent.json`.
pub fn default_recents_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".imgfind").join("recent.json"))
}

/// Unix epoch seconds now (saturating to 0 before the epoch).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
```

Add `mod recents;` to `imgfind-launcher/src/main.rs` (below `slint::include_modules!();`).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p imgfind-launcher recents`
Expected: PASS (all 5 tests).

- [ ] **Step 5: Commit**

```bash
git add imgfind-launcher/
git commit -m "feat(launcher): recents store (load/prune/record/save) with tests"
```

---

### Task 6: Index command planning + streaming runner (`runner.rs`)

**Files:**
- Create: `imgfind-launcher/src/runner.rs`
- Modify: `imgfind-launcher/src/main.rs` (add `mod runner;`)
- Test: `imgfind-launcher/src/runner.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  - `pub enum ChildKind { Imgfind }`
  - `pub struct ChildCommandSpec { pub kind: ChildKind, pub args: Vec<String>, pub cwd: PathBuf }`
  - `pub fn plan(folder: &Path, create_new: bool) -> Vec<ChildCommandSpec>` — builds the two-step argv (index then thumbnails). `create_new == true` ⇒ index gets `--root`; both commands run with `cwd = folder`.
  - `pub fn run_plan(specs: &[ChildCommandSpec], mut on_line: impl FnMut(String) + Send) -> Result<()>` — spawns each spec sequentially, streaming merged stdout+stderr line-by-line to `on_line`, stopping on the first non-zero exit. (Glue; verified manually.)

- [ ] **Step 1: Write the failing tests for `plan`**

Create `imgfind-launcher/src/runner.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn plan_create_new_uses_root_then_thumbnails_all() {
        let folder = Path::new("/data/photos");
        let specs = plan(folder, true);
        assert_eq!(specs.len(), 2);
        // index --root, cwd = folder
        assert_eq!(specs[0].args, vec!["index", "--root"]);
        assert_eq!(specs[0].cwd, folder);
        // thumbnails --gui-sizes --all, cwd = folder
        assert_eq!(specs[1].args, vec!["thumbnails", "--gui-sizes", "--all"]);
        assert_eq!(specs[1].cwd, folder);
    }

    #[test]
    fn plan_existing_omits_root() {
        let folder = Path::new("/data/photos/sub");
        let specs = plan(folder, false);
        assert_eq!(specs[0].args, vec!["index"]);
        assert_eq!(specs[0].cwd, folder);
        assert_eq!(specs[1].args, vec!["thumbnails", "--gui-sizes", "--all"]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p imgfind-launcher plan_`
Expected: FAIL — `plan` not found.

- [ ] **Step 3: Implement `plan` and `run_plan`**

Prepend to `imgfind-launcher/src/runner.rs`:

```rust
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildKind {
    Imgfind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildCommandSpec {
    pub kind: ChildKind,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Build the index→thumbnails plan for indexing `folder`.
///
/// `create_new` decides whether a fresh library is created *inside* `folder`
/// (`index --root`, exploiting that `--root` creates the DB in the process cwd)
/// or whether indexing walks up into an existing ancestor library (`index`).
/// Both steps run with `cwd = folder`, and thumbnails pre-generates all GUI
/// sizes so first view is instant.
pub fn plan(folder: &Path, create_new: bool) -> Vec<ChildCommandSpec> {
    let mut index_args = vec!["index".to_string()];
    if create_new {
        index_args.push("--root".to_string());
    }
    vec![
        ChildCommandSpec { kind: ChildKind::Imgfind, args: index_args, cwd: folder.to_path_buf() },
        ChildCommandSpec {
            kind: ChildKind::Imgfind,
            args: vec!["thumbnails".into(), "--gui-sizes".into(), "--all".into()],
            cwd: folder.to_path_buf(),
        },
    ]
}

/// Spawn each spec sequentially, streaming merged stdout+stderr line-by-line to
/// `on_line`. Stops at the first non-zero exit. `RUST_LOG=info` is set on the
/// child unless already set in this process's environment, so the live `tracing`
/// progress lines reach the log pane (indicatif bars auto-hide when piped).
pub fn run_plan(specs: &[ChildCommandSpec], mut on_line: impl FnMut(String) + Send) -> Result<()> {
    for spec in specs {
        let program = match spec.kind {
            ChildKind::Imgfind => imgfind::resolve_sibling_binary("imgfind"),
        };
        on_line(format!("$ imgfind {}", spec.args.join(" ")));

        let mut cmd = Command::new(&program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if std::env::var_os("RUST_LOG").is_none() {
            cmd.env("RUST_LOG", "info");
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", program.to_string_lossy()))?;

        // Drain stderr on a thread; read stdout on this thread; interleave lines.
        let stderr = child.stderr.take().context("child stderr")?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let tx_err = tx.clone();
        let err_thread = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx_err.send(line);
            }
        });
        if let Some(stdout) = child.stdout.take() {
            let tx_out = tx.clone();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        }
        drop(tx);
        for line in rx {
            on_line(line);
        }
        let _ = err_thread.join();

        let status = child.wait().context("waiting for child")?;
        if !status.success() {
            on_line(format!("(exited with {})", status));
            bail!("command failed: imgfind {}", spec.args.join(" "));
        }
    }
    Ok(())
}
```

Add `mod runner;` to `imgfind-launcher/src/main.rs`.

- [ ] **Step 4: Run the tests + build**

Run: `cargo test -p imgfind-launcher plan_ && cargo build -p imgfind-launcher`
Expected: tests PASS; crate builds.

- [ ] **Step 5: Commit**

```bash
git add imgfind-launcher/
git commit -m "feat(launcher): index plan builder + streaming child-process runner"
```

---

### Task 7: Launcher UI + wiring

**Files:**
- Modify: `imgfind-launcher/ui/launcher.slint` (full two-view UI)
- Modify: `imgfind-launcher/src/main.rs` (clap args, recents load, window callbacks, rfd pickers, spawn `imgfind-gui`, background index run streaming to log)

**Interfaces:**
- Consumes: `imgfind::find_db_root_upward`, `imgfind::resolve_sibling_binary` (Tasks 1-2); `recents::{Recents, default_recents_path, now_secs}` (Task 5); `runner::{plan, run_plan}` (Task 6).

This task is verified manually (Slint UI + process spawning). Use the `slint` skill for layout/focus idioms.

- [ ] **Step 1: Define the full Slint UI**

Replace `imgfind-launcher/ui/launcher.slint` with a two-view window. Required structs / properties / callbacks (names are the contract used by `main.rs` — keep them exact):

```slint
export struct RecentRow {
    name: string,
    path: string,
    when: string,
    root: string,
}

export component MainWindow inherits Window {
    title: "imgfind";
    preferred-width: 820px;
    preferred-height: 600px;
    background: #1e1e1e;

    // Model + state driven from Rust.
    in property <[RecentRow]> recents;
    in property <string> view: "home";        // "home" | "index"
    in property <string> status-line;          // index phase / result
    in property <string> log-text;             // accumulated log
    in property <bool> indexing: false;        // disables controls while running
    in property <bool> can-open-indexed: false;
    in property <string> ask-existing-root;    // non-empty => offer add-to-existing

    // Events handled in Rust.
    callback open-root(string);     // root path
    callback open-other();          // pick an existing library folder
    callback start-index();         // go to index view, pick a folder
    callback confirm-index(bool);   // true => create-new, false => add-to-existing
    callback back-home();
    callback open-indexed();

    // ... layout: when view == "home" show header + recents ListView (each row
    // has an "Open" TouchArea calling open-root(row.root)), plus buttons
    // "Open other folder..." (open-other) and "Index a folder..." (start-index).
    // when view == "index" show: the folder + the ask block (if
    // ask-existing-root != "" show two buttons calling confirm-index(false) /
    // confirm-index(true), else a single "Create library here" calling
    // confirm-index(true)); a read-only scrolling log (TextInput read-only:true
    // or a Text in a Flickable bound to log-text); the status-line; a "Back"
    // button (back-home) and an "Open this library" button visible when
    // can-open-indexed, calling open-indexed(). Use ASCII glyphs only.
}
```

Implement the layout per the slint skill (VerticalLayout/HorizontalLayout, ListView for recents, a Flickable+Text or read-only TextInput for the log). Buttons disabled via `enabled: !root.indexing`.

- [ ] **Step 2: Wire `main.rs`**

Rewrite `imgfind-launcher/src/main.rs` to:

1. Parse clap `Args {}` (no args needed yet; keep a struct for future `--dir`).
2. Load + prune recents: `let recents_path = recents::default_recents_path();` load, `prune_missing()`, `save_to` (best-effort), and build the `RecentRow` model (name = root's final component, path = home-abbreviated, when = coarse relative from `last_opened` via `now_secs()`, root = root string).
3. Create `MainWindow`, set `recents` model.
4. Callback `on_open_root`: record in recents + save, then spawn `imgfind-gui --dir <root>` via `imgfind::resolve_sibling_binary("imgfind-gui")` (`Command::new(prog).arg("--dir").arg(root).spawn()`), and close the launcher (`slint::quit_event_loop()` or `window.hide()`).
5. Callback `on_open_other`: `rfd::FileDialog::new().pick_folder()`; if `Some(folder)`, `imgfind::find_db_root_upward(&folder)`: if `Some(root)` → same as open_root; else set `view = "index"`, stash the folder, and set `ask-existing-root = ""` (no ancestor → create-new only).
6. Callback `on_start_index`: `pick_folder()`; on `Some(folder)` stash it, compute `find_db_root_upward(&folder)` and set `ask-existing-root` to the ancestor root string (or `""`), set `view = "index"`.
7. Callback `on_confirm_index(create_new)`: set `indexing = true`, clear `log-text`, status "Indexing…". On a background thread (`std::thread::spawn`) build `runner::plan(&folder, create_new)` and call `runner::run_plan(&specs, on_line)` where `on_line` marshals each line to the UI via `slint::invoke_from_event_loop` appending to `log-text`. On success: record the resolved root (folder if create_new, else the ancestor root) in recents + save, set status "Done", `can-open-indexed = true`, `indexing = false`. On error: status "Failed", `indexing = false`. Use a `Weak<MainWindow>` for marshaling. Stash `folder`/resolved root in `Rc<RefCell<...>>` shared with the callbacks.
8. Callback `on_back_home`: `view = "home"`, reset index-view state.
9. Callback `on_open_indexed`: spawn `imgfind-gui --dir <resolved root>` and quit.

Follow the imgfind-gui patterns for `Weak`/`invoke_from_event_loop` (see `imgfind-gui/src/main.rs`). Keep all DB-free — the launcher only spawns processes and reads/writes `recent.json`.

- [ ] **Step 3: Build**

Run: `cargo build -p imgfind-launcher && cargo clippy -p imgfind-launcher 2>&1 | tail -5`
Expected: builds, clippy-clean.

- [ ] **Step 4: Manual smoke test**

Build the workspace (`cargo build --release --workspace`). From a directory that has an indexed library, run `./target/release/imgfind-launcher`:
- The recent list shows entries (after one open) and "Open" launches `imgfind-gui` on that library.
- "Index a folder…" → pick a folder with no `.imgfind` → "Create library here" runs indexing, the log pane streams lines, status reaches "Done", and "Open this library" launches the GUI on it.
- Pick a subfolder of an existing library → the add-to-existing vs create-new choice appears.

Document the manual result in the commit message / review notes.

- [ ] **Step 5: Commit**

```bash
git add imgfind-launcher/
git commit -m "feat(launcher): two-view UI — open recent/other library, index a folder with live log"
```

---

### Task 8: Packaging (install.sh + .desktop)

**Files:**
- Modify: `install.sh`
- Create: `packaging/imgfind-launcher.desktop`

- [ ] **Step 1: Create the desktop entry**

Create `packaging/imgfind-launcher.desktop`:

```ini
[Desktop Entry]
Type=Application
Name=imgfind
GenericName=Semantic Image Search
Comment=Find your images by natural-language search
Exec=imgfind-launcher
Terminal=false
Categories=Graphics;Photography;
```

- [ ] **Step 2: Install the launcher binary + desktop entry**

In `install.sh`, after the GUI-binary block (line 37), add:

```bash
# Copy launcher binary if present
LAUNCHER_BINARY="$PROJECT_DIR/target/release/imgfind-launcher"
if [ -f "$LAUNCHER_BINARY" ]; then
    echo "📦 Installing imgfind-launcher to $LOCAL_BIN..."
    cp "$LAUNCHER_BINARY" "$LOCAL_BIN/imgfind-launcher"
    chmod +x "$LOCAL_BIN/imgfind-launcher"

    # Install a desktop entry so it shows up in the OS application menu.
    DESKTOP_DIR="$HOME/.local/share/applications"
    mkdir -p "$DESKTOP_DIR"
    DESKTOP_SRC="$PROJECT_DIR/packaging/imgfind-launcher.desktop"
    if [ -f "$DESKTOP_SRC" ]; then
        echo "🖥  Installing desktop entry to $DESKTOP_DIR..."
        sed "s|^Exec=imgfind-launcher\$|Exec=$LOCAL_BIN/imgfind-launcher|" \
            "$DESKTOP_SRC" > "$DESKTOP_DIR/imgfind-launcher.desktop"
    fi
fi
```

Also add a line to the quick-start output (after line 48):

```bash
echo "    imgfind-launcher        # graphical launcher (or find 'imgfind' in your app menu)"
```

- [ ] **Step 3: Verify the script parses**

Run: `bash -n install.sh && echo OK`
Expected: `OK` (no syntax errors). Optionally run `./install.sh` after a release build and confirm both files are copied.

- [ ] **Step 4: Commit**

```bash
git add install.sh packaging/imgfind-launcher.desktop
git commit -m "build: install imgfind-launcher binary + Linux .desktop entry"
```

---

### Task 9: Documentation + final review + finish branch

**Files:**
- Modify: `CLAUDE.md` (workspace table + a Launcher bullet)
- Modify: `README.md` and/or `USAGE.md` (mention the launcher)

- [ ] **Step 1: Update CLAUDE.md**

In the Workspace table, add a row:

```markdown
| `imgfind-launcher` | `imgfind-launcher` | Desktop launcher: pick a library to open, or index a folder |
```

Add a short architecture bullet near the GUI section:

```markdown
- **Launcher (`imgfind-launcher/`)** — standalone Slint desktop "front door" (the OS app-menu entry point). Pure process orchestrator: it never opens a DB or loads CLIP. Lists recently-opened libraries from `~/.imgfind/recent.json` and opens one by spawning `imgfind-gui --dir <root>`; "Index a folder…" picks a folder (`rfd`), asks new-vs-existing library (`imgfind::find_db_root_upward`), and spawns `imgfind index` + `imgfind thumbnails --gui-sizes --all` as children, streaming their output into a log pane (`RUST_LOG=info` so live progress shows). Sibling binaries resolved via `imgfind::resolve_sibling_binary`. Leaves `imgfind-gui`'s walk-up behavior untouched. See `docs/superpowers/specs/2026-06-22-gui-launcher-design.md`.
```

Note the new `thumbnails --all` flag in the CLI-commands paragraph (one clause).

- [ ] **Step 2: Update README/USAGE**

Add a brief "Launcher" note to `README.md` (how to start it: `imgfind-launcher` or the app menu) and document `thumbnails --all` in `USAGE.md`.

- [ ] **Step 3: Full workspace verification**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets 2>&1 | tail -15 && cargo test --workspace`
Expected: fmt clean, no clippy warnings, all tests pass.

- [ ] **Step 4: Commit docs**

```bash
git add CLAUDE.md README.md USAGE.md
git commit -m "docs: document imgfind-launcher and thumbnails --all"
```

- [ ] **Step 5: Final review + finish branch**

Dispatch the final code-reviewer over the whole branch, address any Critical/Important findings, then invoke `superpowers:finishing-a-development-branch`.

---

## Self-Review

**Spec coverage:**
- New `imgfind-launcher` crate (orchestrator) → Tasks 4-7. ✓
- Recents at `~/.imgfind/recent.json`, most-recent-first, cap 20, prune missing, corrupt→empty → Task 5. ✓
- Open recent / open other folder (validate `.imgfind`) → Task 7 (steps 4-5). ✓
- Index a folder, ask-per-folder new-vs-existing via walk-up → Tasks 1 (helper) + 7 (steps 6-7). ✓
- Spawn `imgfind index` (+`--root` for new) then `thumbnails --gui-sizes --all`, cwd=folder → Task 6 `plan`. ✓
- `thumbnails --all` loop-until-complete → Task 3. ✓
- Stream child stdout+stderr to a log pane, `RUST_LOG=info` default → Task 6 `run_plan` + Task 7 UI. ✓
- Sibling-binary resolution shared helper → Task 2. ✓
- install.sh + Linux `.desktop` → Task 8. ✓
- Docs → Task 9. ✓
- Invariants pinned by tests: `.imgfind/imgfind.db` marker (Task 1 tests), `--root`+cwd pairing (Task 6 `plan` tests), get_db_path home fallback (Task 1 step 4). ✓

**Placeholder scan:** No TBD/TODO; the only "implement per the slint skill" is the *visual layout* of Task 7 (a manual UI task), whose logic interfaces (struct/property/callback names, Rust behavior) are fully specified. ✓

**Type consistency:** `find_db_root_upward(&Path)->Option<PathBuf>`, `resolve_sibling_binary(&str)->OsString`, `Recents::{load_from,save_to,prune_missing,record,default}`, `runner::plan(&Path,bool)->Vec<ChildCommandSpec>`, `run_plan(&[ChildCommandSpec], FnMut(String))` — names used identically across Tasks 5-7. ✓
