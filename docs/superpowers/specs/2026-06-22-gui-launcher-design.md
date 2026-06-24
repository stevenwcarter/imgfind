# imgfind Launcher — Design

**Date:** 2026-06-22
**Status:** Approved (brainstorming → spec)
**Topic:** A standalone desktop "front door" for imgfind that picks a library to open and can index new folders.

## Problem

`imgfind-gui` today launches straight into the image grid, resolving its database by walking
up from the current directory (or falling back to `~/.imgfind/imgfind.db`). That works when
the GUI is launched from a terminal inside a project, but it is a poor **OS-level application
entry point**: when a user double-clicks an installed app icon there is no meaningful cwd, so
the GUI cannot know *which* library the user wants.

We want a separate, installable desktop application — the **launcher** — that is the graphical
entry point for users who are not on the command line. It lets the user:

1. See a list of **recently-opened libraries** and open one.
2. Open an arbitrary existing library folder via a native folder picker.
3. **Choose a folder to index** (creating a new library there, or adding to an existing one),
   kicking off the indexing + thumbnail-generation operations with the CLI's status output
   shown live in a **log pane**.

## Non-goals

- No change whatsoever to `imgfind-gui`'s existing behavior. Bare `imgfind-gui` still walks up
  from cwd / falls back to the global DB; `--dir DIR` still opens that library directly. The
  launcher is additive and never alters those paths.
- The launcher does **not** open a Turso database or load a CLIP model itself. It is a pure
  process orchestrator (Approach A from brainstorming). Richer recents (image counts, cover
  thumbnails) by linking the `imgfind` library is explicitly deferred.
- No map view, no search, no in-app browsing — the launcher hands off to `imgfind-gui` for all
  of that.

## Architecture

### New workspace crate: `imgfind-launcher`

A third workspace member (`members = [".", "imgfind-gui", "imgfind-launcher"]`) producing the
`imgfind-launcher` binary — a native Slint desktop app, mirroring `imgfind-gui`'s crate shape
(`slint` + `slint-build`, `anyhow`, `tracing`/`tracing-subscriber`, `clap`, `rfd`, plus a path
dependency on the `imgfind` library for the small pure helpers below). It is `edition = "2024"`.

The launcher is a **pure orchestrator**: every heavyweight operation is delegated to the
existing `imgfind` / `imgfind-gui` binaries spawned as child processes. This keeps the launcher
small, crash-isolated from indexing, and means it reuses the real, tested engines untouched.

### Binary resolution

The launcher must locate the sibling `imgfind` and `imgfind-gui` executables. It reuses the
exact strategy already in `imgfind`'s `launch_gui` (`src/main.rs`): **sibling-of-current-exe
first, then bare name on `PATH`**. This logic is extracted into a small shared helper so it is
not duplicated three ways:

- Add `imgfind::resolve_sibling_binary(name: &str) -> std::ffi::OsString` to `src/lib.rs`
  (sibling-of-`current_exe` if it exists, else `name` verbatim for PATH lookup, honoring
  `std::env::consts::EXE_SUFFIX`). Refactor `launch_gui` to call it. The launcher calls it for
  both `imgfind` and `imgfind-gui`.

### Library-root resolution helper

The launcher needs to answer "does this folder, or any ancestor, already contain an imgfind
library?" for the ask-per-folder flow, and "what is the resulting library root?" to record in
recents and offer an Open button. The walk-up logic currently lives inline in `get_db_path`.
Extract a pure, non-creating helper:

- Add `imgfind::find_db_root_upward(start: &Path) -> Option<PathBuf>` to `src/lib.rs`: walk up
  from `start` (inclusive) returning the **first directory containing `.imgfind/imgfind.db`**,
  or `None`. It never touches `~/.imgfind` and never creates anything. Refactor `get_db_path`'s
  walk-up branch to use it (behavior-preserving: `get_db_path(None)` still falls back to the
  home DB when the helper returns `None`).
- Unit-tested with `tempfile`: finds a DB at `start`; finds one at an ancestor; returns `None`
  when no ancestor has one; is unaffected by `~/.imgfind`.

## Recent-libraries store

A machine-managed JSON file, separate from the hand-edited `config.toml`, at
`~/.imgfind/recent.json`:

```jsonc
{
  "version": 1,
  "entries": [
    { "root": "/home/steve/Pictures", "last_opened": 1781827380 }
  ]
}
```

- `root` is the **library root** (the directory containing `.imgfind`), stored canonicalized
  and absolute.
- `last_opened` is **Unix epoch seconds** (`SystemTime::now()` since `UNIX_EPOCH`) — avoids
  adding a date/time dependency; the UI renders a coarse relative hint ("today", "3d ago") from
  it, and it doubles as the sort key.
- Ordered **most-recent-first**; opening or indexing a root moves it to the front (dedup by
  canonical path).
- Capped at **20** entries (oldest dropped).
- On load, entries whose `root/.imgfind/imgfind.db` no longer exists are pruned and the file is
  rewritten. (A transient prune is fine — the launcher only shows libraries that currently
  exist.)
- Lives in a new module `imgfind-launcher/src/recents.rs` as a pure, serde-backed `Recents`
  type with `load() -> Recents`, `record(&mut self, root: &Path)`, and `save(&self) -> Result<()>`.
  Pure ordering/dedup/cap logic is unit-tested without touching the real home directory (the
  path is injected; tests use a `tempfile` dir). Corrupt/missing JSON loads as empty (logged at
  `warn`), never an error — the launcher must always start.

This file is written **only by the launcher**. Libraries opened directly via the CLI/`imgfind-gui`
do not appear here; that is acceptable and matches the "launcher is its own front door" model.

## UI (Slint)

`imgfind-launcher/ui/launcher.slint` — a single `MainWindow inherits Window` with two logical
areas. Follows the existing imgfind-gui Slint conventions (callbacks declared on the window,
properties set from Rust, models for lists, dark chrome consistent with the GUI). Use only
ASCII / Latin-1 glyphs in button text (per the project's known Slint default-font tofu issue —
e.g. `×`, `<`, `>`, not `✕`/`‹›`).

### Home view

- **Header**: app title + a short subtitle ("Choose a library to open").
- **Recent libraries list** (`ListView` over a `[RecentRow]` model): each row shows the library
  name (the root's final path component) and the abbreviated root path (`~/…`) plus a relative
  "last opened" hint. A row is openable by click of its **Open** button or double-click.
- **Buttons**:
  - **Open other folder…** → native folder picker (`rfd::FileDialog::pick_folder`). The chosen
    folder is validated with `find_db_root_upward`; if a library root is found it is opened,
    otherwise an inline message offers to index it instead (routes into the Index view with the
    folder pre-filled).
  - **Index a folder…** → switches to the Index view (below).
- Empty state: when there are no recents, the list area shows guidance pointing at the two
  buttons.

### Index view

Reached from "Index a folder…" (or the "index it instead" affordance). Flow:

1. **Folder pick** (`rfd` folder picker) if not already chosen.
2. **Ask-per-folder dialog** (driven by `find_db_root_upward(folder)`):
   - If an existing library root `X` is found (the folder itself or an ancestor), offer two
     choices:
     - **Add to existing library** at `X` — index the picked folder into `X`'s DB.
     - **Create a new library here** — create a fresh `.imgfind` inside the picked folder.
   - If none is found, default directly to **Create a new library here** (no dialog needed; a
     one-line note states a new library will be created).
3. **Run**: spawn the index then thumbnails children (see "Indexing execution"), streaming
   output into the **log pane** — a read-only, monospace, auto-scrolling text area. A status
   line shows the current phase ("Indexing…", "Generating thumbnails…", "Done", or "Failed").
4. On success, the resolved library root is recorded in recents and an **Open this library**
   button appears (spawns `imgfind-gui --dir <root>`). On failure (non-zero child exit) the
   status line shows "Failed" and the log retains the error output.
- The **Index** / **Run** button is disabled while a job is in flight; only one indexing job
  runs at a time. The window stays responsive throughout (work is on a background thread; log
  lines marshal to the UI via `slint::invoke_from_event_loop`).

## Indexing execution

Given a picked `folder` and a choice of target library root `root`:

- **Create a new library here** (`root == folder`, no existing `.imgfind` reused): spawn
  `imgfind index --root` with the child's **working directory set to `folder`** (so the new
  `.imgfind/imgfind.db` is created inside `folder`; `--dir` defaults to `.`).
- **Add to existing library** (`root` is `folder` or an ancestor that already has `.imgfind`):
  spawn `imgfind index` with working directory `folder` and **no** `--root`, so the indexer
  walks up and indexes into `root`'s existing DB.
- Then, regardless of choice, spawn **`imgfind thumbnails --gui-sizes --all`** with working
  directory `folder` to pre-generate the three GUI thumbnail sizes (300/512/2048) for every
  image, so first view in the GUI is instant.

### New CLI flag: `thumbnails --all`

The existing `thumbnails` command generates `--count` (default 50) per size and prints how many
remain. For an unattended launcher run we need "generate everything," and relying on a magic
huge `--count` is fragile. Add an `--all` flag to the `Thumbnails` subcommand:

- When `--all` is set, for each requested size loop the batch generator until
  `count_images_without_thumbnails(size)` reports `0` (or a batch generates `0`, guarding
  against an image that can never be thumbnailed — e.g. a permanently-undecodable file — so the
  loop always terminates). `--count` becomes the per-iteration batch size (keeping memory
  bounded).
- Without `--all`, behavior is exactly as today.
- This is genuinely useful beyond the launcher (one-shot "finish all thumbnails"). The
  loop-termination logic (stop on zero-remaining **or** zero-progress) is covered by a test that
  pins both exit conditions.

### Streaming the CLI output to the log pane

The launcher spawns each child with `Stdio::piped()` on **both stdout and stderr**, and sets
`RUST_LOG=info` in the child environment **unless `RUST_LOG` is already set in the launcher's
environment** (honoring a user override). Rationale: indexing's live progress is drawn by
`indicatif`, which auto-hides its bars when stdout is not a TTY (i.e. when piped), so without
`RUST_LOG` the pane would show almost nothing until the final summary. With `RUST_LOG=info`
the `tracing`/`log` per-batch lines stream into the pane, matching "the status updates normally
printed to the CLI."

A background thread reads child stdout and stderr line-by-line (one reader thread per stream)
and forwards each line to the UI via `invoke_from_event_loop`, appending to the log model. The
worker waits for child exit and reports the exit status (success/failure) back to the UI. The
two phases (index, then thumbnails) run sequentially; thumbnails only starts if indexing
succeeded.

### Log-line plumbing module

`imgfind-launcher/src/runner.rs` owns process spawning and streaming. It exposes a small,
testable seam:

- A pure `IndexPlan` builder: `fn plan(folder: &Path, root: &Path) -> Vec<ChildCommandSpec>`
  where `ChildCommandSpec { program_kind: Imgfind, args: Vec<String>, cwd: PathBuf, use_root: bool }`.
  This computes the exact argv + cwd + `--root` choice from `(folder, root)` **without spawning
  anything**, so the command-construction logic (the part most likely to be wrong) is unit-tested:
  - new library (`root == folder`, folder has no `.imgfind`) → `index --root` then
    `thumbnails --gui-sizes --all`, both cwd=folder.
  - existing ancestor (`root` ≠ `folder`) → `index` (no `--root`) then `thumbnails …`, cwd=folder.
- The actual spawn/stream loop (`run_plan`) is thin glue over `std::process::Command` and is not
  unit-tested directly (it shells out); its correctness rests on the tested `plan` + manual
  verification.

## Packaging / install

- `install.sh`: build `--workspace` already produces `target/release/imgfind-launcher`; copy it
  to `~/.local/bin/imgfind-launcher` alongside the other two binaries (guarded by an existence
  check like the GUI binary), and mention it in the quick-start output.
- **Linux `.desktop` entry**: ship `packaging/imgfind-launcher.desktop` and have `install.sh`
  install it to `~/.local/share/applications/imgfind-launcher.desktop` (with `Exec` pointing at
  `~/.local/bin/imgfind-launcher`, `Terminal=false`, a sensible `Name`/`Comment`/`Categories=Graphics;Photography;`).
  This makes it launchable from the OS application menu — the "launch like any other application"
  requirement. No icon asset is in scope yet (use a stock/no icon); an icon can be added later.

## Testing strategy

Per the project's TDD discipline, tests target the pure seams that carry the logic; the
shell-out/Slint glue is verified manually.

- `imgfind::find_db_root_upward` — unit tests (tempfile): finds at start; finds at ancestor;
  `None` when absent; ignores `~/.imgfind`. Plus a characterization test that
  `get_db_path(None)` still returns the home fallback when no ancestor DB exists (pins the
  refactor's invariant at the seam it crosses).
- `imgfind::resolve_sibling_binary` — unit test that it appends `EXE_SUFFIX` and prefers a
  sibling path that exists, falling back to the bare name otherwise.
- `thumbnails --all` loop — test both termination conditions: stops when no images remain, and
  stops when a batch makes zero progress (cannot-thumbnail image), without infinite-looping.
- `recents` — unit tests (injected path / tempfile): record bumps to front and dedups by
  canonical path; cap at 20 drops oldest; load prunes missing roots; corrupt/missing file loads
  as empty.
- `runner::plan` — unit tests: new-library and add-to-existing produce the expected argv, cwd,
  and `--root` presence for both the index and thumbnails commands.

## Invariants this feature depends on

- **`.imgfind/imgfind.db` is the sole library marker.** `find_db_root_upward` and recents-prune
  both rely on this exact relative path. If the DB location convention ever changes, both must
  change with it. (Pinned by the `find_db_root_upward` tests.)
- **`imgfind index --root` creates the DB in the process's cwd** (not in `--dir`). The launcher
  sets the child cwd to the picked folder to exploit this; the "new library here" `runner::plan`
  test pins the `--root` + cwd pairing.
- **`indicatif` hides progress bars when stdout is not a TTY**, which is why the launcher relies
  on `RUST_LOG=info` for live log content rather than on the progress bars. (Behavioral
  assumption of the `image`/`indicatif` stack; documented here so a future change to how the CLI
  reports progress is known to affect the launcher's log richness.)

## Out of scope / future

- Library-linked recents with image counts and cover thumbnails (Approach B).
- A bundled app icon and richer desktop integration (mimetypes, macOS `.app`/Windows shortcut).
- In-launcher search or browsing — the GUI owns that.
- Cancelling an in-flight indexing job from the launcher (first cut: disable controls until it
  finishes; the user can close the window / kill the process).
