# `imgfind gui` subcommand + `imgfind-gui -d` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `imgfind gui [ARGS]` subcommand that forwards args to the `imgfind-gui` binary, and give `imgfind-gui` a `-d` short alias for `--dir`.

**Architecture:** `imgfind-gui`'s `dir` arg gains `short`. Base `imgfind` gets a `Gui { args: Vec<OsString> }` subcommand using clap's `trailing_var_arg` + `allow_hyphen_values` so all args after `gui` are captured raw; a thin `launch_gui` helper spawns `imgfind-gui` (sibling of `current_exe`, else PATH), blocks, and propagates the exit code.

**Tech Stack:** Rust 2024, clap derive (`trailing_var_arg`, `allow_hyphen_values`), `std::process::Command`, `anyhow`.

## Global Constraints

- Rust edition 2024; `cargo clippy --workspace --features tui -- -D warnings` clean; `cargo fmt --check` clean; pristine test output.
- Errors use `anyhow` with `.context()`/`.with_context()`.
- Subcommand form only (`imgfind gui …`); no `--gui`/`-g` flag.
- Block model: spawn + wait + propagate child exit code (`status.code().unwrap_or(1)`).
- Binary discovery: sibling of `current_exe()` named `imgfind-gui{EXE_SUFFIX}` if it exists, else `imgfind-gui` via PATH.
- Verified API facts: clap supports `Vec<std::ffi::OsString>` positional args; `trailing_var_arg = true` requires the field be the last positional (no `short`/`long`); `allow_hyphen_values = true` lets `-d`/`--dir` be captured as values rather than parsed as `imgfind` options.

---

### Task 1: `imgfind-gui -d` alias + `imgfind gui` passthrough subcommand

**Files:**
- Modify: `imgfind-gui/src/main.rs` (the `Args` struct `dir` field; add a test module)
- Modify: `src/main.rs` (add `Gui` variant to `Commands` ~line 32-72; add dispatch arm ~line 174; add `launch_gui`; add a test module)

**Interfaces:**
- Produces: `Commands::Gui { args: Vec<std::ffi::OsString> }`; `fn launch_gui(args: &[std::ffi::OsString]) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing tests**

In `imgfind-gui/src/main.rs`, add (or extend the existing `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod arg_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn dir_accepts_both_short_and_long() {
        assert_eq!(
            Args::try_parse_from(["imgfind-gui", "-d", "/x"]).unwrap().dir,
            Some("/x".to_string())
        );
        assert_eq!(
            Args::try_parse_from(["imgfind-gui", "--dir", "/x"]).unwrap().dir,
            Some("/x".to_string())
        );
    }
}
```

In `src/main.rs`, add a test module (place near the bottom, sibling to any existing tests):

```rust
#[cfg(test)]
mod gui_cli_tests {
    use super::*;
    use clap::Parser;
    use std::ffi::OsString;

    #[test]
    fn gui_captures_trailing_args_including_hyphens() {
        let cli = Cli::try_parse_from(["imgfind", "gui", "--dir", "x", "-d", "y", "--whatever"])
            .expect("parse");
        match cli.command {
            Commands::Gui { args } => {
                let want: Vec<OsString> =
                    ["--dir", "x", "-d", "y", "--whatever"].iter().map(OsString::from).collect();
                assert_eq!(args, want);
            }
            _ => panic!("expected Gui subcommand"),
        }
    }

    #[test]
    fn gui_with_no_args_is_empty() {
        let cli = Cli::try_parse_from(["imgfind", "gui"]).expect("parse");
        assert!(matches!(cli.command, Commands::Gui { args } if args.is_empty()));
    }

    #[test]
    fn existing_subcommand_still_parses() {
        let cli = Cli::try_parse_from(["imgfind", "tui"]).expect("parse");
        assert!(matches!(cli.command, Commands::Tui { .. }));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p imgfind-gui arg_tests` and `cargo test -p imgfind gui_cli_tests`
Expected: FAIL — `imgfind-gui` `-d` is unknown (only `--dir` today); `Commands::Gui` does not exist yet (won't compile). A compile failure here is the expected RED.

- [ ] **Step 3: Add `-d` to `imgfind-gui`**

In `imgfind-gui/src/main.rs`, change the `Args.dir` field:

```rust
    /// Directory to search for an imgfind database (walks up from here).
    #[arg(short, long)]
    dir: Option<String>,
```

- [ ] **Step 4: Add the `Gui` subcommand + dispatch + launcher in `src/main.rs`**

Add the variant to `enum Commands` (e.g. right after the `Tui { … }` variant, ~line 72):

```rust
    /// Launch the native desktop GUI (forwards remaining args to imgfind-gui)
    Gui {
        /// Arguments passed through to imgfind-gui (e.g. -d / --dir DIR)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
```

Add the dispatch arm in the `match cli.command { … }` block (~line 174, alongside the other arms):

```rust
        Commands::Gui { args } => launch_gui(&args)?,
```

Add the launcher function (top-level `fn` in `src/main.rs`, e.g. near the other helper fns):

```rust
/// Launch the imgfind-gui binary with `args`, blocking until it exits and
/// propagating its exit code, so `imgfind gui ARGS` behaves like `imgfind-gui ARGS`.
fn launch_gui(args: &[std::ffi::OsString]) -> anyhow::Result<()> {
    use std::ffi::OsString;

    // Prefer a sibling of the current executable (install.sh + cargo target layout);
    // otherwise rely on PATH.
    let sibling = std::env::current_exe().ok().and_then(|p| {
        let cand = p.with_file_name(format!("imgfind-gui{}", std::env::consts::EXE_SUFFIX));
        cand.exists().then_some(cand.into_os_string())
    });
    let program = sibling.unwrap_or_else(|| OsString::from("imgfind-gui"));

    let status = std::process::Command::new(&program)
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "failed to launch imgfind-gui ({}); is it installed / on PATH?",
                program.to_string_lossy()
            )
        })?;
    std::process::exit(status.code().unwrap_or(1));
}
```

Ensure `anyhow::Context` is in scope for `.with_context` (it is used elsewhere in `main.rs`; if the import is local, rely on the existing top-level `use anyhow::...`).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p imgfind-gui arg_tests` and `cargo test -p imgfind gui_cli_tests`
Expected: PASS — `-d`/`--dir` both yield `Some("/x")`; `gui` captures `["--dir","x","-d","y","--whatever"]`; empty case empty; `tui` still parses.

- [ ] **Step 6: Manual smoke (non-blocking on tests) + full gate**

```bash
cargo build --workspace
./target/debug/imgfind gui --help   # should hand off to imgfind-gui (its help) or print gui usage; must NOT error as an unknown imgfind flag
cargo test --workspace --features tui
cargo clippy --workspace --features tui -- -D warnings
cargo fmt --check
```
Expected: builds; `imgfind gui --help` reaches the GUI binary (or prints subcommand usage) rather than erroring; all tests pass; no warnings; no fmt diff.

- [ ] **Step 7: Commit**

```bash
git add imgfind-gui/src/main.rs src/main.rs
git commit -m "feat(cli): add 'imgfind gui' passthrough subcommand and imgfind-gui -d alias"
```

---

### Task 2: Documentation

**Files:**
- Modify: `README.md`, `USAGE.md`, `CLAUDE.md`

- [ ] **Step 1: Update docs**

- `README.md` and `USAGE.md`: document `imgfind gui [ARGS]` (launches the native GUI, forwarding args to `imgfind-gui`; e.g. `imgfind gui -d ~/pics`) and note `imgfind-gui` accepts `-d` as well as `--dir`. Place near the existing command listing / GUI section, matching each file's style.
- `CLAUDE.md`: add `gui` to the CLI commands line (currently lists `index`, `search`, `metadata`, `tui`, `thumbnails`, `clean`, `status`, `config {…}`), noting it forwards remaining args to `imgfind-gui`. The GUI run note can mention `imgfind gui [-d DIR]` as the installed-binary equivalent of `cargo run -p imgfind-gui -- [--dir DIR]`. Link the spec `docs/superpowers/specs/2026-06-17-gui-subcommand-design.md`.

- [ ] **Step 2: Commit**

```bash
git add README.md USAGE.md CLAUDE.md
git commit -m "docs: document 'imgfind gui' subcommand and imgfind-gui -d"
```

---

## Self-Review

**Spec coverage:**
- `imgfind-gui -d`/`--dir` — Task 1 Step 3 + test Step 1. ✓
- `imgfind gui` passthrough subcommand (trailing_var_arg + allow_hyphen_values) — Task 1 Step 4 + test Step 1 (load-bearing capture test). ✓
- `launch_gui` block model + exit-code propagation + sibling/PATH discovery — Task 1 Step 4. ✓
- Edge cases (no args → empty; binary not found → context error; signal → exit 1) — covered by `launch_gui` + the empty-args test. ✓
- Invariant 1 (trailing capture incl hyphens) test — Task 1 Step 1 (`gui_captures_trailing_args_including_hyphens`). ✓
- Invariant 2 (`-d` ≡ `--dir`) test — Task 1 Step 1 (`dir_accepts_both_short_and_long`). ✓
- No-regression guard (`tui` still parses) — Task 1 Step 1. ✓
- Docs — Task 2. ✓

**Placeholder scan:** No TBD/TODO; all code concrete. The launcher spawn is intentionally not unit-tested (thin I/O over a real binary) — covered by the Step 6 manual smoke, disclosed in the spec.

**Type consistency:** `Commands::Gui { args: Vec<std::ffi::OsString> }` and `launch_gui(args: &[std::ffi::OsString]) -> Result<()>` used consistently; `Args.dir: Option<String>` unchanged in type (only `short` added), so existing `imgfind-gui` `--dir` handling is untouched.
