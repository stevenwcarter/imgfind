# Cross-Platform Installers + Release CI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship installable artifacts for macOS (`.dmg`), Windows (`.msi`), and Linux (tarball + `install.sh`), built and published by a GitHub Actions release workflow.

**Architecture:** `cargo-packager` builds the macOS `.app`/`.dmg` and Windows WiX `.msi` from one declarative config in `imgfind-launcher/Cargo.toml`, bundling all three binaries as siblings. Linux ships a tarball of prebuilt binaries + the existing `install.sh`. A new `.github/workflows/release.yml` runs three native-runner jobs (each checking out the public `clipper-rs` as a `../clipper` sibling), then attaches the three artifacts to a GitHub Release on `v*` tags.

**Tech Stack:** Rust (edition 2024) workspace, `cargo-packager`, GitHub Actions, WiX (Windows), `lipo` (macOS universal2), `actionlint`, bash.

**Spec:** `docs/superpowers/specs/2026-06-24-cross-platform-installers-design.md`

## Global Constraints

- **Unsigned** installers — no code signing / notarization. Document Gatekeeper/SmartScreen click-through.
- **`clipper-rs` is public** (`github.com/stevenwcarter/clipper-rs`) — CI checks it out into `../clipper` (the path `Cargo.toml`'s `clipper = { path = "../clipper" }` expects), no secrets.
- **Sibling-binary invariant** — every installer MUST place `imgfind-launcher`, `imgfind-gui`, and `imgfind` in the **same directory** (`resolve_sibling_binary` resolves siblings of `current_exe` before PATH). This invariant is **already covered** by `sibling_binary_prefers_existing_sibling` / `_falls_back_to_bare_name_when_missing` / `_bare_name_when_no_current_exe` in `src/lib.rs` — do **not** add a duplicate test; just preserve the layout in each installer.
- **Install scope** — GUI app + CLI on PATH: Windows MSI adds install dir to PATH; macOS via a documented `/usr/local/bin/imgfind` symlink.
- **macOS arch** — universal2 (x86_64 + aarch64).
- **Release trigger** — `push` of `v*` tags + `workflow_dispatch` (dry-run).
- **App identifier** — `plus.javapl.imgfind`. **Product name** — `imgfind`.
- **Edition 2024**; clippy/fmt clean.
- Out of scope (do not build): signing, auto-update, Homebrew/winget/Flatpak/AppImage/.deb, model-weight bundling, ARM Windows/Linux.

> **Verification reality:** A `.dmg` only builds on macOS and a `.msi` only on Windows; the full release only runs in Actions. So local/CI verification per task is: `cargo test` (Rust), a sandboxed bash run (`install.sh`), `cargo packager` config parse (`--help`/dry validation on Linux can't emit dmg/wix but validates the manifest), and `actionlint` (workflow). **End-to-end acceptance is a `workflow_dispatch` dry-run after merge** (noted in the final task) — it cannot be done from this dev box.

---

### Task 1: `install.sh` installs prebuilt binaries shipped beside it (tarball mode)

The Linux release tarball ships the three prebuilt binaries next to `install.sh`. Today `install.sh` hard-requires `target/release/imgfind` and aborts otherwise. Make it prefer `target/release/` but fall back to the script's own directory, so the tarball installs without a build step.

**Files:**
- Modify: `install.sh` (binary-source resolution near the top; the three `cp` blocks)
- Create: `tests/install_sh_test.sh` (sandboxed bash test)

**Interfaces:**
- Produces: an `install.sh` that defines `SRC_DIR` = first of `$PROJECT_DIR/target/release` or the script's own dir that contains an `imgfind` binary, and copies `imgfind`, `imgfind-gui`, `imgfind-launcher` from `$SRC_DIR`. No change to install targets (`~/.local/bin`, `~/.local/share/applications`).

- [ ] **Step 1: Write the failing test**

Create `tests/install_sh_test.sh`:

```bash
#!/usr/bin/env bash
# Sandboxed test: install.sh must install prebuilt binaries that sit beside it
# (tarball layout), with no target/release present.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Simulate an extracted tarball: install.sh + three fake binaries side by side.
PKG="$WORK/pkg"
mkdir -p "$PKG/packaging"
cp "$REPO_ROOT/install.sh" "$PKG/install.sh"
cp "$REPO_ROOT/packaging/imgfind-launcher.desktop" "$PKG/packaging/imgfind-launcher.desktop"
for b in imgfind imgfind-gui imgfind-launcher; do
  printf '#!/bin/sh\necho %s\n' "$b" > "$PKG/$b"
  chmod +x "$PKG/$b"
done

# Run install.sh from the package dir with a sandboxed HOME and no target/release.
FAKE_HOME="$WORK/home"
mkdir -p "$FAKE_HOME"
( cd "$PKG" && HOME="$FAKE_HOME" bash ./install.sh >/dev/null )

fail=0
for b in imgfind imgfind-gui imgfind-launcher; do
  if [ ! -x "$FAKE_HOME/.local/bin/$b" ]; then
    echo "MISSING: $b not installed to ~/.local/bin"; fail=1
  fi
done
[ "$fail" -eq 0 ] && echo "PASS: all three binaries installed from tarball layout"
exit "$fail"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `bash tests/install_sh_test.sh`
Expected: FAIL — current `install.sh` aborts with "Release binary not found at …/target/release/imgfind" (exit 1) because there is no `target/release`.

- [ ] **Step 3: Implement the minimal change in `install.sh`**

Replace the current `PROJECT_DIR` / `BINARY_PATH` / not-found block:

```bash
PROJECT_DIR=$(pwd)
BINARY_PATH="$PROJECT_DIR/target/release/imgfind"

# Check if release binary exists
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ Release binary not found at $BINARY_PATH"
    echo "Please run 'cargo build --release --workspace' first."
    exit 1
fi
```

with source-dir resolution that also accepts binaries beside the script (tarball):

```bash
PROJECT_DIR=$(pwd)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Resolve where the prebuilt binaries live: a local release build, or — for the
# downloaded release tarball — right next to this script.
if [ -f "$PROJECT_DIR/target/release/imgfind" ]; then
    SRC_DIR="$PROJECT_DIR/target/release"
elif [ -f "$SCRIPT_DIR/imgfind" ]; then
    SRC_DIR="$SCRIPT_DIR"
else
    echo "❌ imgfind binary not found in $PROJECT_DIR/target/release or $SCRIPT_DIR"
    echo "Build first ('cargo build --release --workspace') or run this from an extracted release tarball."
    exit 1
fi
```

Then update the three copy blocks to use `$SRC_DIR` instead of `$PROJECT_DIR/target/release` / `$BINARY_PATH`. The CLI block:

```bash
# Copy CLI binary to local bin
echo "📦 Installing imgfind to $LOCAL_BIN..."
cp "$SRC_DIR/imgfind" "$LOCAL_BIN/imgfind"
chmod +x "$LOCAL_BIN/imgfind"
```

and likewise `GUI_BINARY="$SRC_DIR/imgfind-gui"` and `LAUNCHER_BINARY="$SRC_DIR/imgfind-launcher"`. Also change the desktop-entry source so it works from a tarball: `DESKTOP_SRC="$SCRIPT_DIR/packaging/imgfind-launcher.desktop"` (the tarball ships `packaging/` beside the script; for a repo checkout `SCRIPT_DIR` == repo root so this still resolves).

- [ ] **Step 4: Run test to verify it passes**

Run: `bash tests/install_sh_test.sh`
Expected: `PASS: all three binaries installed from tarball layout`

- [ ] **Step 5: Regression-check the repo-build path and lint**

Run: `bash -n install.sh && command -v shellcheck >/dev/null && shellcheck install.sh || echo "shellcheck not installed (skip)"`
Expected: no syntax errors; shellcheck clean if present.

- [ ] **Step 6: Commit**

```bash
git add install.sh tests/install_sh_test.sh
git commit -m "build: install.sh installs prebuilt binaries from a release tarball layout"
```

---

### Task 2: App icon assets

`cargo-packager` needs an icon source to produce the macOS `.icns` and Windows `.ico`. Commit a real 1024×1024 PNG (a simple placeholder is fine — swappable later with no code change).

**Files:**
- Create: `packaging/icon.png` (1024×1024)
- Create: `packaging/README.md` (one paragraph: what the icon is, how to replace it)

**Interfaces:**
- Produces: `packaging/icon.png` — the path the packager config (Task 3) references for icon generation.

- [ ] **Step 1: Generate the placeholder icon**

Prefer ImageMagick if available; otherwise use the Rust `image` crate via a throwaway script. ImageMagick path:

```bash
command -v magick >/dev/null && CONVERT=magick || CONVERT=convert
"$CONVERT" -size 1024x1024 xc:'#1e6fd9' \
  -gravity center -fill white -font DejaVu-Sans-Bold -pointsize 520 -annotate +0+0 'if' \
  packaging/icon.png
file packaging/icon.png
```

If neither `magick`/`convert` nor a suitable font is available, generate a solid-color 1024×1024 PNG with a tiny Rust snippet using the workspace's `image` dep (run via `cargo run` in a scratch bin or `python3 -c` with Pillow) — any real 1024×1024 RGBA PNG is acceptable. The deliverable is a valid 1024×1024 PNG at `packaging/icon.png`.

- [ ] **Step 2: Verify it is a valid 1024×1024 PNG**

Run: `file packaging/icon.png` (expect "PNG image data, 1024 x 1024"). If `identify` is available: `identify packaging/icon.png`.
Expected: dimensions 1024×1024, PNG.

- [ ] **Step 3: Write `packaging/README.md`**

```markdown
# Packaging assets

- `icon.png` — 1024×1024 app icon source. `cargo-packager` derives the macOS
  `.icns` and Windows `.ico` from it (see the `[package.metadata.packager]`
  config in `imgfind-launcher/Cargo.toml`). Replace this file with a branded
  icon (same dimensions) to rebrand; no code change needed.
- `imgfind-launcher.desktop` — Linux XDG desktop entry installed by `install.sh`.
```

- [ ] **Step 4: Commit**

```bash
git add packaging/icon.png packaging/README.md
git commit -m "build: add app icon source for installers"
```

---

### Task 3: `cargo-packager` config for the launcher (multi-binary bundle)

Add a `cargo-packager` manifest so `imgfind-launcher` is the bundled app, with `imgfind-gui` and `imgfind` packaged alongside it as siblings, producing `dmg` on macOS and `wix` (MSI) on Windows.

**Files:**
- Modify: `imgfind-launcher/Cargo.toml` (add `[package.metadata.packager]` + sub-tables)

**Interfaces:**
- Produces: a packager config that, when `cargo packager` runs on the matching OS, emits an installer whose payload contains all three binaries in one directory, references `packaging/icon.png`, sets identifier `plus.javapl.imgfind`, and (Windows) adds the install dir to PATH.

> **IMPORTANT — schema is version-specific.** Before writing the config, fetch
> the current `cargo-packager` config reference (Context7: resolve
> `cargo-packager` / `crabnebula-dev/cargo-packager`, then query "Cargo.toml
> metadata packager config binaries icons formats wix nsis macos"; or the docs
> at https://docs.crabnebula.dev/packager/). Field names below are the intended
> shape — **reconcile them with the fetched docs and adjust** (e.g. exact keys
> for multi-binary `binaries`, `formats`, per-OS tables, WiX PATH). Do not invent
> fields the tool rejects.

- [ ] **Step 1: Install the tool and read its config schema**

Run: `cargo install cargo-packager --locked` (and fetch the config docs per the note above).
Expected: `cargo packager --version` prints a version.

- [ ] **Step 2: Add the config to `imgfind-launcher/Cargo.toml`**

Append (reconciled with current docs):

```toml
[package.metadata.packager]
product-name = "imgfind"
identifier = "plus.javapl.imgfind"
description = "CLIP-based semantic image search"
authors = ["Steven Carter"]
icons = ["../packaging/icon.png"]
# The launcher is the main app; ship the GUI and CLI beside it so the launcher's
# resolve_sibling_binary finds them, and so `imgfind` is available on PATH.
binaries = [
    { path = "imgfind-launcher", main = true },
    { path = "imgfind-gui" },
    { path = "imgfind" },
]

[package.metadata.packager.macos]
minimum-system-version = "10.15"

[package.metadata.packager.windows]
# Add the install directory to PATH so imgfind / imgfind-gui are callable.
# (Exact key per current WiX/NSIS docs — e.g. an `append-to-path` / wix PATH
# component; reconcile with fetched docs.)
```

Restrict formats per-OS via CLI flags in the workflow (`--formats dmg` / `--formats wix`) rather than hard-coding, so each runner emits only its native installer.

- [ ] **Step 3: Validate the manifest parses**

Run (on this Linux box, which can't emit dmg/wix but will parse the manifest and report config errors):
`cd imgfind-launcher && cargo packager --help >/dev/null && cargo build --release -p imgfind-launcher -p imgfind-gui -p imgfind 2>&1 | tail -3`
Then a config-only check: `cargo packager --formats deb --release -p imgfind-launcher 2>&1 | tail -20` — a `deb` build exercises the config on Linux; a config/schema error fails loudly, a successful or "unsupported on this target" message past config parsing means the manifest is accepted. (If `deb` pulls unwanted deps, instead rely on the tool reporting manifest parse errors and defer real dmg/wix emission to CI.)
Expected: no "unknown field"/"invalid config" errors; manifest accepted.

- [ ] **Step 4: clippy/fmt + workspace build still clean**

Run: `cargo fmt --all -- --check && cargo build --release --workspace 2>&1 | tail -3`
Expected: fmt clean; workspace builds.

- [ ] **Step 5: Commit**

```bash
git add imgfind-launcher/Cargo.toml
git commit -m "build: cargo-packager config bundling launcher + gui + cli"
```

---

### Task 4: Release CI workflow

Add `.github/workflows/release.yml`: three native-runner jobs build + package each OS artifact (checking out `clipper-rs` as a sibling), and a publish step attaches them to a GitHub Release on `v*` tags. `workflow_dispatch` enables a dry-run that builds artifacts without publishing.

**Files:**
- Create: `.github/workflows/release.yml`
- Create: `.github/workflows/actionlint.yml` (lints workflows on PR — cheap guard)

**Interfaces:**
- Consumes: `install.sh` (tarball mode, Task 1), `packaging/` (icon + desktop entry, Tasks 1–2), the packager config (Task 3).
- Produces: artifacts `imgfind-<ref>-macos-universal.dmg`, `imgfind-<ref>-windows-x86_64.msi`, `imgfind-<ref>-linux-x86_64.tar.gz` attached to the tag's Release.

- [ ] **Step 1: Write `actionlint.yml`**

```yaml
name: actionlint
on:
  pull_request:
    paths: [".github/workflows/**"]
  push:
    branches: [main]
    paths: [".github/workflows/**"]
jobs:
  actionlint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Run actionlint
        uses: docker://rhysd/actionlint:latest
        with:
          args: -color
```

- [ ] **Step 2: Write `release.yml`**

Key requirements (write the full file; shape below):

```yaml
name: release
on:
  push:
    tags: ["v*"]
  workflow_dispatch:

permissions:
  contents: write   # needed to create the Release

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: macos-14
            artifact: macos
          - os: windows-latest
            artifact: windows
          - os: ubuntu-latest
            artifact: linux
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
        with: { path: imgfind }
      - name: Checkout clipper (public sibling)
        uses: actions/checkout@v4
        with:
          repository: stevenwcarter/clipper-rs
          path: clipper
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with: { workspaces: imgfind }

      # Linux GUI build deps (Slint).
      - name: Install Linux deps
        if: matrix.artifact == 'linux'
        run: sudo apt-get update && sudo apt-get install -y libxkbcommon-dev libfontconfig1-dev libxcb1-dev

      - name: Install cargo-packager
        if: matrix.artifact != 'linux'
        run: cargo install cargo-packager --locked

      # macOS: universal2 via two targets + lipo, then package.
      - name: Build + package (macOS)
        if: matrix.artifact == 'macos'
        working-directory: imgfind
        run: |
          rustup target add x86_64-apple-darwin aarch64-apple-darwin
          cargo build --release --target x86_64-apple-darwin -p imgfind-launcher -p imgfind-gui -p imgfind
          cargo build --release --target aarch64-apple-darwin -p imgfind-launcher -p imgfind-gui -p imgfind
          mkdir -p target/release
          for b in imgfind-launcher imgfind-gui imgfind; do
            lipo -create -output target/release/$b \
              target/x86_64-apple-darwin/release/$b \
              target/aarch64-apple-darwin/release/$b
          done
          cargo packager --release --formats dmg -p imgfind-launcher

      - name: Build + package (Windows)
        if: matrix.artifact == 'windows'
        working-directory: imgfind
        shell: bash
        run: |
          cargo build --release -p imgfind-launcher -p imgfind-gui -p imgfind
          cargo packager --release --formats wix -p imgfind-launcher

      - name: Build + tarball (Linux)
        if: matrix.artifact == 'linux'
        working-directory: imgfind
        run: |
          cargo build --release -p imgfind-launcher -p imgfind-gui -p imgfind
          STAGE=imgfind-${GITHUB_REF_NAME}-linux-x86_64
          mkdir -p "$STAGE/packaging"
          cp target/release/imgfind target/release/imgfind-gui target/release/imgfind-launcher "$STAGE/"
          cp install.sh "$STAGE/"
          cp packaging/imgfind-launcher.desktop "$STAGE/packaging/"
          cp README.md USAGE.md "$STAGE/" 2>/dev/null || true
          tar czf "$STAGE.tar.gz" "$STAGE"

      - name: Collect artifacts
        working-directory: imgfind
        shell: bash
        run: |
          mkdir -p ../dist
          # dmg/msi land under target/release/<packager-output-dir>; find and rename.
          find target -maxdepth 3 -name '*.dmg' -exec cp {} "../dist/imgfind-${GITHUB_REF_NAME}-macos-universal.dmg" \; 2>/dev/null || true
          find target -maxdepth 3 -name '*.msi' -exec cp {} "../dist/imgfind-${GITHUB_REF_NAME}-windows-x86_64.msi" \; 2>/dev/null || true
          cp imgfind-*-linux-x86_64.tar.gz ../dist/ 2>/dev/null || true

      - uses: actions/upload-artifact@v4
        with:
          name: imgfind-${{ matrix.artifact }}
          path: dist/*

  release:
    needs: build
    if: startsWith(github.ref, 'refs/tags/v')
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with: { path: dist, merge-multiple: true }
      - uses: softprops/action-gh-release@v2
        with:
          files: dist/*
          generate_release_notes: true
```

> The implementer MUST reconcile the packager **output path** (`find target … -name '*.dmg'`) and the `--formats`/`-p` flags with what `cargo packager` actually emits per the Task 3 docs; adjust the `find`/glob accordingly. The exact `GITHUB_REF_NAME` for `workflow_dispatch` is the branch name — that is fine for dry-run artifact names.

- [ ] **Step 3: Lint the workflows**

Run: `command -v actionlint >/dev/null && actionlint .github/workflows/release.yml .github/workflows/actionlint.yml || docker run --rm -v "$PWD":/repo -w /repo rhysd/actionlint:latest -color`
Expected: no errors. (If neither `actionlint` nor Docker is available locally, rely on the `actionlint.yml` workflow to catch issues on PR — note this in the commit.)

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/release.yml .github/workflows/actionlint.yml
git commit -m "ci: release workflow building mac/windows/linux installers + actionlint guard"
```

---

### Task 5: Documentation

Document per-OS install (incl. the unsigned-app click-through) and point CLAUDE.md at the spec + packaging config.

**Files:**
- Modify: `README.md` (add/replace an "## Installation" section)
- Modify: `CLAUDE.md` (add a short "Packaging / Release" note)

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the README install section**

Add an "## Installation" section near the top covering:

```markdown
## Installation

Prebuilt installers are attached to each [GitHub Release](https://github.com/stevenwcarter/imgfind/releases).

### macOS (`.dmg`)
1. Download `imgfind-<version>-macos-universal.dmg`, open it, drag **imgfind** to Applications.
2. The app is **unsigned**, so the first launch is blocked by Gatekeeper. Right-click the app → **Open** → **Open** (only needed once), or run `xattr -dr com.apple.quarantine /Applications/imgfind.app`.
3. (Optional) put the CLI on your PATH:
   `sudo ln -sf /Applications/imgfind.app/Contents/MacOS/imgfind /usr/local/bin/imgfind`

### Windows (`.msi`)
1. Download `imgfind-<version>-windows-x86_64.msi` and run it.
2. SmartScreen may warn (unsigned): **More info → Run anyway**.
3. The installer adds imgfind to your PATH, so `imgfind`, `imgfind-gui`, and `imgfind-launcher` work from a terminal; the launcher also appears in the Start menu.

### Linux (tarball)
1. Download `imgfind-<version>-linux-x86_64.tar.gz`, extract it, and run `./install.sh`.
2. It installs the three binaries to `~/.local/bin` and a desktop entry to `~/.local/share/applications`. Ensure `~/.local/bin` is on your PATH.

### Build from source
See [CLAUDE.md](CLAUDE.md). Requires the sibling `clipper` repo checked out at `../clipper`.
```

(Adjust an existing install section if one already exists rather than duplicating — read `README.md` first.)

- [ ] **Step 2: Add the CLAUDE.md packaging note**

Under a suitable heading (e.g. after Build & run), add:

```markdown
## Packaging / Release

Installers are built by `.github/workflows/release.yml` on `v*` tags (or a
`workflow_dispatch` dry-run): macOS `.dmg` and Windows `.msi` via `cargo-packager`
(config in `imgfind-launcher/Cargo.toml` `[package.metadata.packager]`, unsigned),
and a Linux tarball of prebuilt binaries + `install.sh`. CI checks out the public
`clipper-rs` repo as the `../clipper` sibling. All three binaries are bundled in
one directory so the launcher's `resolve_sibling_binary` finds them. See
`docs/superpowers/specs/2026-06-24-cross-platform-installers-design.md`.
```

- [ ] **Step 3: Verify the docs render and links resolve**

Run: `grep -n "## Installation" README.md && grep -n "Packaging / Release" CLAUDE.md`
Expected: both present.

- [ ] **Step 4: Commit**

```bash
git add README.md CLAUDE.md
git commit -m "docs: per-OS install instructions + packaging/release note"
```

---

## Final: review + manual acceptance + finish branch

- [ ] Dispatch the final code-reviewer over the whole branch diff.
- [ ] **Manual acceptance (cannot run from the dev box):** push the branch and trigger the `release` workflow via `workflow_dispatch`; confirm all three artifacts build and upload. Then cut a real `v*` tag to verify the Release publish step. Flag this to the user as the real end-to-end check — CI is the only environment that can build the `.dmg`/`.msi`.
- [ ] Invoke `superpowers:finishing-a-development-branch`.

## Self-Review notes

- **Spec coverage:** packaging tooling (T3), icon (T2), CLI-on-PATH (T3 Windows / T5 macOS doc), CI 3-job + release (T4), Linux tarball + install.sh (T1), docs incl. unsigned click-through (T5), actionlint (T4). Sibling invariant — already tested, called out in Global Constraints (no task, by design). universal2 (T4 macOS job). All spec sections map to a task.
- **No duplicate test** for `sibling_binary_from` — already covered in `src/lib.rs`.
- **Type/name consistency:** `SRC_DIR`/`SCRIPT_DIR` (T1), identifier `plus.javapl.imgfind` and product `imgfind` (T3, CLAUDE note), artifact names identical across T4 and T5.
- **Known soft spots (flagged inline, not placeholders):** exact `cargo-packager` field names and output paths are version-specific — tasks explicitly fetch current docs and reconcile; dmg/msi emission is CI-only, so local verification is config-parse + lint, with a `workflow_dispatch` dry-run as the real acceptance gate.
