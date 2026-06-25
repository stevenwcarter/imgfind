# Cross-Platform Installers + Release CI

**Date:** 2026-06-24
**Status:** Approved (brainstorm)
**Topic:** Ship installable artifacts for macOS, Windows, and Linux, built by CI.

## Goal

Let an end user install imgfind on macOS, Windows, or Linux **without compiling
from source**. The launcher GUI (`imgfind-launcher`) is the user-facing front
door; the CLI (`imgfind`) is exposed on the command line for power users.

- **macOS / Windows** — a true installer (`.dmg` containing an `.app`; `.msi`).
- **Linux** — a release tarball of prebuilt binaries + the existing `install.sh`
  (no compile step for the user). No `.deb`/AppImage/Flatpak.
- **CI** — a GitHub Actions workflow that builds all three artifacts and attaches
  them to a GitHub Release.

## Decisions (locked in brainstorm)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Code signing | **Unsigned** | No Apple Developer ID / Authenticode cert. Installers work; users click through Gatekeeper/SmartScreen once. Signing can be added later without redesign. |
| `clipper-rs` access in CI | **Public checkout** | `clipper-rs` is public (`github.com/stevenwcarter/clipper-rs`); CI clones it as a sibling, no secrets. |
| Install scope | **GUI app + CLI on PATH** | Install the clickable launcher app (bundling all three binaries as siblings) AND expose `imgfind` on PATH. |
| Tooling | **`cargo-packager`** | One declarative config produces `.dmg` (mac) and `.msi` (Windows); GUI-app-native; bundles sibling binaries; handles Info.plist/icons/PATH. |
| macOS arch | **universal2** | Single `.dmg` runs on Intel + Apple Silicon (`lipo` of x86_64 + aarch64). |
| Release trigger | **`v*` tag + manual dispatch** | Build artifacts and publish a GitHub Release on version tags. |

## The sibling-binary constraint (load-bearing)

`imgfind-launcher` spawns `imgfind-gui` and `imgfind` via
`imgfind::resolve_sibling_binary`, which resolves a binary name relative to the
**directory of the launcher's own `current_exe`**, falling back to `PATH`. Every
installer MUST therefore place all three binaries in the **same directory**:

- **macOS** — `imgfind.app/Contents/MacOS/{imgfind-launcher, imgfind-gui, imgfind}`
- **Windows** — `<InstallDir>\{imgfind-launcher.exe, imgfind-gui.exe, imgfind.exe}`
- **Linux** — `install.sh` already copies all three into `~/.local/bin` (siblings).

**Invariants this feature depends on:** `resolve_sibling_binary` resolves
siblings of `current_exe` before falling back to PATH. If that resolution order
ever changes, the bundled-but-not-on-PATH `imgfind-gui` would stop launching. A
test pins this (see Testing).

## Architecture / components

### 1. Packaging config (`cargo-packager`)

A packager config (in each GUI crate's `Cargo.toml` under
`[package.metadata.packager]`, or a top-level `packager.toml` if cleaner) for the
**launcher** as the primary bundle:

- `product-name = "imgfind"`, identifier `plus.javapl.imgfind` (reverse-DNS).
- `binaries` — the launcher (`main: true`) plus `imgfind-gui` and `imgfind` as
  additional bundled binaries so all three land in `Contents/MacOS` / the MSI
  install dir.
- `icons` — point at generated `.icns` / `.ico` (see #2).
- macOS: `category = "public.app-category.photography"`, minimum-system-version
  default.
- Windows (WiX): enable adding the install dir to the user PATH and a Start-menu
  shortcut to the launcher.
- Output formats restricted per-OS: `dmg` on macOS, `wix` (MSI) on Windows. Linux
  packaging is handled by the tarball step, not cargo-packager.

### 2. App icon

- Source: a single high-res `packaging/icon.png` (1024×1024). If no brand icon
  exists yet, commit a simple generated placeholder (solid background + "if"
  glyph) so the pipeline is real; it can be swapped later with no code change.
- CI (or a committed step) derives `.icns` (macOS) and `.ico` (Windows) from the
  PNG. `cargo-packager` can consume multiple icon inputs; prefer letting it
  generate platform icons from the PNG where supported, else generate in CI with
  standard tools (`iconutil`/`png2icns` on mac, ImageMagick on Windows).

### 3. CLI-on-PATH

- **macOS** — a drag-install `.dmg` cannot modify PATH automatically (that would
  need a `.pkg` with a postinstall script). **Decision:** stay with the `.dmg`
  and make CLI-on-PATH a **documented one-line symlink** in the README:
  `sudo ln -sf /Applications/imgfind.app/Contents/MacOS/imgfind /usr/local/bin/imgfind`.
  The dmg window background text repeats this hint. Fully-automated mac PATH is a
  deferred option (switch to `.pkg`) — out of scope here. The GUI itself needs no
  PATH entry (double-click the app), so this only affects power-user CLI use.
- **Windows** — the WiX MSI adds `<InstallDir>` to the user PATH, so `imgfind`,
  `imgfind-gui`, and `imgfind-launcher` are all callable.
- **Linux** — `install.sh` already targets `~/.local/bin`.

### 4. Release CI (`.github/workflows/release.yml`)

New workflow, triggered on `push` of tags matching `v*` and `workflow_dispatch`.
Three jobs, each on its native runner:

**Common to every job:**
1. Checkout `imgfind`.
2. Checkout `stevenwcarter/clipper-rs` into `../clipper` (sibling path the
   `Cargo.toml` `path = "../clipper"` dep expects). Plain public checkout.
3. Install Rust stable (edition 2024) + needed targets.

**`macos` job** (runner `macos-14`, arm64):
- `rustup target add x86_64-apple-darwin aarch64-apple-darwin`.
- Build release for both targets; `lipo` the three binaries into universal2.
- `cargo packager --release` (or feed the universal binaries) producing the
  `.dmg`.
- Rename to `imgfind-<ver>-macos-universal.dmg`; upload.

**`windows` job** (runner `windows-latest`):
- Build release (`x86_64-pc-windows-msvc`).
- `cargo packager` producing the `.msi`.
- Rename to `imgfind-<ver>-windows-x86_64.msi`; upload.

**`linux` job** (runner `ubuntu-latest`):
- Build release (`x86_64-unknown-linux-gnu`) of all three binaries.
- Assemble `imgfind-<ver>-linux-x86_64.tar.gz` containing the three binaries,
  `install.sh`, `packaging/imgfind-launcher.desktop`, README/USAGE. The
  in-tarball `install.sh` copies prebuilt binaries (it already no-ops the build
  and only requires the binaries be present at `target/release` — the tarball
  layout will satisfy that, or `install.sh` gains a documented "use binaries in
  the current dir" path; see Open implementation detail).
- Upload.

**Release publish:** a final step (or `softprops/action-gh-release`) attaches the
three uploaded artifacts to the GitHub Release for the tag. Release notes can be
auto-generated.

> System deps: the Slint GUI crates may need Linux GUI dev libraries
> (`libxkbcommon`, `libfontconfig`, etc.) installed on the ubuntu runner to
> build; the job installs them via `apt`. macOS/Windows runners have the needed
> system frameworks.

### 5. Docs

- **README** — a per-OS "Install" section: download links pattern, the
  **unsigned-app click-through** steps (macOS: right-click → Open, or
  `xattr -dr com.apple.quarantine`; Windows: "More info → Run anyway"), and the
  CLI-on-PATH note per OS.
- **CLAUDE.md** — one-line pointer to this spec under a "Packaging / Release"
  note, plus mention of `cargo-packager` config location and the release
  workflow.

## Data flow

```
git tag v0.1.0  ──►  release.yml
   ├─ macos job  ─► build x86_64+aarch64 ─► lipo ─► cargo packager ─► .dmg ──┐
   ├─ windows job ─► build msvc          ─────────► cargo packager ─► .msi ──┤
   └─ linux job   ─► build gnu           ─► tar (bins+install.sh)  ─► .tar.gz┤
                                                                             ▼
                                                          GitHub Release (v0.1.0)
                                                          with 3 artifacts attached
```

## Error handling

- **CI build fails on one OS** — that job fails independently; the Release step
  runs only if all artifact jobs succeed (no partial release). Re-runnable.
- **Missing `clipper` sibling** — the clipper checkout step is a hard
  prerequisite; if it fails the build fails fast with a clear step name.
- **Unsigned-app friction** — not an error; documented click-through steps.
- **Icon generation failure** — fail the job (don't ship an icon-less bundle).

## Testing

This feature is mostly build/packaging config + CI, which unit tests can't fully
cover. The testable, load-bearing seams:

1. **Sibling-resolution invariant** — a Rust test (in the `imgfind` lib, where
   `resolve_sibling_binary` lives) asserting it resolves a binary that sits next
   to a given `current_exe`-style path **before** falling back to PATH. This pins
   the invariant every installer layout depends on. (Check whether such a test
   already exists; if so, extend rather than duplicate.)
2. **`install.sh` tarball path** — if `install.sh` gains a "install from the
   current directory's binaries" mode for the tarball, add a shell test
   (or a `bash -n` + a smoke run in CI) that it copies all three binaries.
3. **Workflow lint** — `release.yml` is validated by `actionlint` in CI (a cheap
   `lint` job or a pre-commit), catching YAML/expression errors before a tag.
4. **Manual release dry-run** — `workflow_dispatch` lets us run the full pipeline
   on a branch without cutting a real tag, verifying artifacts build end-to-end.

No attempt to programmatically "install and launch" the GUI in CI (out of scope);
the dispatch dry-run + downloading an artifact locally is the acceptance check.

## Acceptance criteria

- `cargo-packager` config builds a `.dmg` locally on macOS and an `.msi` on
  Windows containing all three sibling binaries; launching the app opens the
  launcher, and "Open"/"Index" spawn the bundled `imgfind-gui`/`imgfind`.
- `release.yml` on `workflow_dispatch` produces all three artifacts and (on a tag)
  attaches them to a GitHub Release.
- macOS dmg runs on both Intel and Apple Silicon (universal2).
- Windows MSI puts `imgfind` on PATH.
- Linux tarball extracts and `install.sh` installs all three binaries + desktop
  entry without a compile step.
- README documents per-OS install incl. unsigned click-through.

## Out of scope (YAGNI)

- Code signing / notarization (add later; no redesign needed).
- Auto-update.
- Homebrew tap, winget, Flatpak, Snap, AppImage, `.deb`.
- Bundling CLIP model weights (clipper downloads them on first run).
- ARM Windows / ARM Linux builds.

## Open implementation details (decide during planning)

- Whether `cargo-packager` config lives in `imgfind-launcher/Cargo.toml`
  `[package.metadata.packager]` vs a standalone `packager.toml` — pick whichever
  the tool documents as canonical for multi-binary bundles.
- Exact universal2 assembly: build-each-target-then-`lipo` vs cargo-packager's
  own universal support (use the tool's native support if it exists).
- `install.sh` change for the tarball: smallest tweak that lets it install
  prebuilt binaries shipped beside it (e.g. detect binaries in the script's own
  directory if `target/release` is absent).
