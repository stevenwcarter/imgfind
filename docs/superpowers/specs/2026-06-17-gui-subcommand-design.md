# `imgfind gui` subcommand + `imgfind-gui -d`

**Date:** 2026-06-17
**Status:** Approved (via `/ship-it --ask`)
**Branch:** `gui-subcommand`

## Goal

Two CLI-parity improvements:

1. **`imgfind-gui` accepts `-d` as well as `--dir`**, mirroring the base `imgfind`
   interface (whose dir flags are `-d`/`--dir`).
2. **`imgfind gui [ARGS…]`** launches the `imgfind-gui` binary, forwarding `ARGS`
   verbatim. Implemented as a real subcommand (sibling of the existing `imgfind tui`
   subcommand) rather than a `--gui` flag, so it is idiomatic, discoverable in
   `--help`, and lets clap capture the passthrough args natively.

## Background

- `imgfind-gui`'s arg is `#[arg(long)] dir: Option<String>` (`imgfind-gui/src/main.rs`)
  — only `--dir` today.
- Base `imgfind` is entirely subcommand-driven (`Index`, `Search`, `Tui`, …; enum
  `Commands` at `src/main.rs:32`, dispatched at `src/main.rs:174`). There is no `--gui`
  handling today, and the top-level `Cli` requires a subcommand. `tui` is already a
  subcommand, so `gui` is its natural sibling.

## Design

### 1. `imgfind-gui` `-d`/`--dir`

In `imgfind-gui/src/main.rs`, change the `Args` field:

```rust
#[arg(short, long)]
dir: Option<String>,
```

`short` derives `-d` from the field name. No other change; behavior is identical, just
an added alias.

### 2. `imgfind gui` subcommand (passthrough launcher)

Add a variant to `enum Commands` in `src/main.rs`:

```rust
/// Launch the native desktop GUI (forwards remaining args to imgfind-gui)
Gui {
    /// Arguments passed through to imgfind-gui (e.g. -d / --dir DIR)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<std::ffi::OsString>,
},
```

`trailing_var_arg` + `allow_hyphen_values` make clap capture everything after `gui`
raw — including `-d`, `--dir`, and any future `imgfind-gui` flag — into `args` without
`imgfind` trying to validate them. So `imgfind gui --dir x -d y` ⇒
`args == ["--dir", "x", "-d", "y"]`.

Add a match arm at the dispatch (`src/main.rs:174`) that launches the GUI:

```rust
Commands::Gui { args } => launch_gui(&args)?,
```

And the launcher (thin I/O):

```rust
/// Launch the imgfind-gui binary with `args`, blocking until it exits and
/// propagating its exit code (so `imgfind gui ARGS` behaves like `imgfind-gui ARGS`).
fn launch_gui(args: &[std::ffi::OsString]) -> Result<()> {
    use std::ffi::OsString;

    // Prefer a sibling of the current executable (matches install.sh + cargo target
    // layout); otherwise rely on PATH.
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

**Process model:** block (spawn + wait) and propagate the child's exit code. Ctrl-C in
the terminal reaches the child via the shared foreground process group. `imgfind gui ARGS`
is behaviorally equivalent to `imgfind-gui ARGS`.

**Help behavior:** `imgfind --help` lists `gui` with its doc comment. For
`imgfind gui --help`, clap may either forward `--help` to `imgfind-gui` (its own help)
or print the `gui` subcommand's brief usage, depending on clap's handling of `--help`
vs. `allow_hyphen_values`; either outcome is acceptable. The load-bearing, tested
behavior is that ordinary args (`-d`, `--dir`, arbitrary values) are captured and
forwarded — not the `--help` special case.

## Edge cases / error handling

- **`imgfind gui` with no args** → `args` is empty → launches `imgfind-gui` with no args
  (resolves DB via its normal walk-up/global logic).
- **`imgfind-gui` not found** (not built / not on PATH) → `Command::status()` errors;
  `launch_gui` returns a clear context message ("is it installed / on PATH?") and
  `imgfind` exits non-zero.
- **Child killed by signal** → `status.code()` is `None` → exit `1`.

## Invariants this feature depends on

1. **clap captures all trailing args (incl. hyphen-prefixed) into `Gui.args`** rather
   than parsing them as `imgfind` options. *Test:* parse `["imgfind","gui","--dir","x",
   "-d","y","--whatever"]` and assert `args == ["--dir","x","-d","y","--whatever"]`.
2. **`imgfind-gui` parses `-d` to the same field as `--dir`.** *Test:* parse
   `["imgfind-gui","-d","/x"]` and `["imgfind-gui","--dir","/x"]`; both yield
   `dir == Some("/x")`.

## Testing

- **Base `imgfind` (clap, pure):** in `src/main.rs` tests, assert the trailing-var-arg
  capture from invariant 1 (the load-bearing behavior — proves passthrough works for
  `-d`/`--dir`/arbitrary flags). Also assert a normal subcommand (e.g.
  `["imgfind","tui"]`) still parses, guarding no regression.
- **`imgfind-gui` (clap, pure):** in `imgfind-gui/src/main.rs` tests, assert `-d` and
  `--dir` both populate `dir` (invariant 2).
- **Launcher I/O** (`launch_gui` spawn/exit): thin shell-out over a real binary; not
  unit-tested — smoke-tested manually (`imgfind gui --help` forwards to the GUI).
- **Regression:** existing tests stay green.

## Out of scope

- A `-g`/`--gui` short alias or flag form (subcommand only).
- Detached/background launch (block model chosen).
- Any GUI behavior change beyond the `-d` alias.

## Documentation

- `README.md` / `USAGE.md`: document `imgfind gui [ARGS]` and the `imgfind-gui -d` alias.
- `CLAUDE.md`: add `gui` to the CLI commands list; note it forwards args to `imgfind-gui`.
