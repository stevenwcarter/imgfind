# `imgfind` – CLIP-Based Image Search

## Overview

`imgfind` is a Rust-based tool for finding images by natural-language query. It uses CLIP embeddings to compute semantic similarity between a text prompt and indexed image content.

The project ships as a **3-crate workspace**: a core `imgfind` binary (CLI + ratatui TUI), a separate `imgfind-gui` binary (native Slint desktop GUI), and `imgfind-launcher` (desktop front-door launcher).

## Features

- **Natural Language Search**: Find images using descriptive text like "sunset over mountains" or "a cat sitting on a chair"
- **Fast Indexing**: `imgfind index` instantly records which files exist (walk + hash + row insert); embeddings and thumbnails are generated in the background by `imgfind process` or the GUI's auto-started worker
- **Smart Caching**: Avoids re-processing unchanged images using content hashing
- **SQLite Storage**: Reliable database storage via the async `turso` engine (pure-Rust, no C extension) with native vector similarity search
- **CLI Interface**: Simple command-line interface with helpful status information
- **Interactive TUI**: Browse results in a ratatui-based terminal UI with inline image previews
- **Native GUI**: Slint desktop app — virtualized thumbnail grid, detail panel, lightbox, filter bar (size/type/GPS/tags), keyboard navigation, and a keyboard-driven tagging system with color brushes; map view not yet ported
- **Shell Completions**: Generate completion scripts for bash, zsh, and fish

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

### Cutting a release (maintainers)

`just release X.Y.Z` bumps the version, regenerates `CHANGELOG.md` (git-cliff),
creates a GPG-signed `vX.Y.Z` tag, and pushes — triggering the installer build
and a GitHub Release. Requires `git-cliff` (`cargo install git-cliff`).

## Quick Start

### Basic Usage

1. **Index your images** (fast — records files, no CLIP load):

   ```bash
   imgfind index --dir ~/Pictures --recursive
   ```

2. **Generate embeddings + thumbnails** — run `imgfind process`, or just open the GUI and it auto-starts in the background:

   ```bash
   imgfind process --dir ~/Pictures
   ```

3. **Search for images (CLI):**

   ```bash
   imgfind search "beach vacation"
   imgfind search "family dinner" --limit 5
   ```

4. **Open the native GUI:**

   ```bash
   imgfind gui                  # launcher subcommand (forwards args to imgfind-gui)
   imgfind gui -d ~/Pictures    # -d / --dir targets a directory's database
   imgfind-gui                  # or run the GUI binary directly
   ```

5. **Check status** (now also shows unprocessed counts):

   ```bash
   imgfind status
   ```

## Commands

| Command | Description | Options |
|---------|-------------|---------|
| `index` | Fast row-only scan: walks dir, hashes files, inserts rows — no CLIP, no thumbnails | `--dir <path>`, `--recursive`, `--root`, `--quiet`, `--reindex`, `--batch-size <N>`, `--model <name>` |
| `process` | Complete deferred work: 300px thumbs → embeddings → 512/2048px thumbs (resumable) | `--count <N>`, `--no-embeddings`, `-d/--dir <path>` |
| `search` | Search using natural language (recursive from the cwd by default) | `--limit <N>`, `--threshold <f>`, `--short`, `--display`, `--all`, `--model <name>` |
| `tui` | Browse results in an interactive terminal UI | `--dir <path>` |
| `gui` | Launch the native desktop GUI (forwards remaining args to `imgfind-gui`) | passthrough, e.g. `-d`/`--dir <path>` |
| `thumbnails` | Generate thumbnails in batches | `--size <px>`, `--gui-sizes`, `--count <N>`, `--all` |
| `metadata` | Backfill EXIF metadata for indexed images | `--dir <path>`, `--quiet`, `--count <N>` |
| `clean` | Remove entries for missing files | - |
| `status` | Show database statistics and unprocessed counts | - |
| `config` | Manage configuration | `show`, `add-ignore <pat>`, `remove-ignore <pat>`, `reset` |
| `models` | Manage embedding models | `list`, `use <name>` |
| `completions` | Generate a shell completion script | `<bash\|zsh\|fish>` |

The `imgfind-gui` binary takes an optional `-d`/`--dir DIR` flag to target a specific directory's database. You can also launch it via `imgfind gui [ARGS]`, which forwards `ARGS` (e.g. `-d ~/Pictures`) to `imgfind-gui` and blocks until it exits.

### Index options

`imgfind index` is fast — it only walks the directory and inserts `(path, hash)` rows; no CLIP model is loaded and no thumbnails are generated. Run `imgfind process` (or open the GUI) afterward to generate embeddings and thumbnails.

- `--root` — create the database in the current directory instead of using an existing or global one.
- `--reindex` — re-insert rows for already-indexed paths (force a fresh pass).
- `--batch-size <N>` — row-insert chunk size (default: `[index].batch_size` config, `32`).
- `--no-thumbnails` — deprecated, accepted but ignored (thumbnails are always deferred to `process`).
- `--model <name>` — set the active embedding model for the subsequent `process` pass.

### Process options

- `--count <N>` — per-batch size (default: `64`).
- `--no-embeddings` — thumbnail-only pass; avoids loading the CLIP model entirely.
- `-d/--dir <DIR>` — target a specific directory's DB (walk-up/global logic otherwise).

### Search options

- `--threshold <f>` — maximum cosine distance to include (lower = stricter). Overrides `[search].distance_threshold`.
- `--short` — print bare image paths, one per line (useful for piping).
- `--recursive` — include images in subdirectories of the current directory.
- `--display` — render result images inline in supporting terminals.
- `--all` — return matches from anywhere in the database, not just the current directory.
- `--model <name>` — embedding model to use for this run.

### Shell completions

```bash
imgfind completions bash > /etc/bash_completion.d/imgfind
imgfind completions zsh  > ~/.zfunc/_imgfind
imgfind completions fish > ~/.config/fish/completions/imgfind.fish
eval "$(imgfind completions bash)"   # or eval directly into current shell
```

### Embedding models

`imgfind` stores vectors in per-model tables, so multiple embedding models can coexist in one database.

```bash
imgfind models list        # list registered models (active marked with *)
imgfind models use <name>  # set the active model
```

## Project Structure

### Technology Stack

- **Language**: Rust
- **Image/Text Embeddings**: Custom `clipper` crate (CLIP-based)
- **Hashing**: `oshash-rs` (media-optimized hashing)
- **Database**: SQLite via `turso` (pure-Rust async engine)
- **Vector Search**: `vector_distance_cos` on `F32_BLOB` columns (exact KNN, no C extension)
- **CLI**: `clap` for argument parsing
- **TUI**: `ratatui` + `ratatui-image`
- **Native GUI**: `slint`

### Supported Formats

- JPEG (.jpg, .jpeg)
- PNG (.png)
- GIF (.gif)
- BMP (.bmp)
- TIFF (.tiff)
- WebP (.webp)
- **RAW (camera):** Nikon (.nef, .nrw), Adobe/generic (.dng), Olympus (.orf), Canon (.cr2, .cr3, .crw), Sony (.arw, .sr2, .srf), Fujifilm (.raf), Panasonic (.rw2), Pentax (.pef), Samsung (.srw), Epson (.erf), Minolta (.mrw), Leica/misc (.raw, .rwl), Phase One/Hasselblad (.iiq, .3fr, .fff), Mamiya/Leaf/Kodak (.mef, .mos, .kdc, .dcr) — decoded via embedded preview with full-demosaic fallback.

## Interactive TUI

Launch the terminal UI with `imgfind tui`. It shows a 3x3 grid (9 images per page) with inline previews and a per-thumbnail similarity score label, plus a `?` help overlay listing every keybinding.

| Key | Action |
|-----|--------|
| `e` | edit search |
| `h` / `j` / `k` / `l` | move focus |
| `H` / `L` | previous / next page |
| `1`-`9` | zoom that image |
| `Enter` | zoom focused image |
| `Esc` | close zoom / help |
| scroll | zoom in/out (in zoom view) |
| right-click | reset zoom |
| `?` | toggle help overlay |
| `q` / `Ctrl-C` | quit |

## Native GUI

Launch with `imgfind gui` (or `imgfind-gui` directly). The GUI restores the previous session on startup: last search/filter, selection, scroll position, and result list — no query re-executed.

**Navigation & panels**: `h/j/k/l` or arrow keys move the selection in the virtualized grid; `Enter` opens a right-side detail panel (metadata + "Search similar"); `Space` opens the full-screen lightbox (prev/next, Esc closes); right-click opens the original in the OS viewer.

**Tagging**: assign free-text tags to images using keyboard chords (active in grid, detail panel, and lightbox):

| Key(s) | Action |
|--------|--------|
| `` ` `` | Toggle the left rail (brush editors + Most Recent staging area) |
| `t` | Open add-tags modal (space-separated words) |
| `m` + `r`/`g`/`y`/`p`/`b` | Paint the red/green/yellow/purple/blue brush's tags onto the focused image |
| `mm` | Re-apply the Most Recent tag buffer |
| `f` + `r`/`g`/`y`/`p`/`b` | Load a brush into the tag filter |
| `ft` | Toggle the tag filter on/off |

Color brushes are input shortcuts only — a brush is a set of tags; no color is stored on an image. Brushes and filter state persist across sessions. Tag filtering (AND/OR, enable toggle) appears in the filter pane and combines with the size/type/GPS filters.

## Launcher

`imgfind-launcher` is the desktop "front door" — launch it from the OS app menu or by running `imgfind-launcher`. It shows recently-opened libraries and lets you open one (or pick another folder) directly in the GUI; "Index a folder…" runs the fast `imgfind index` step and streams its output live, then opens the library in the GUI where the background process-worker handles embeddings and thumbnails. No CLIP model is ever loaded by the launcher itself.

## Configuration

Configuration lives at `~/.imgfind/config.toml` (created on first run). Inspect it with `imgfind config show`.

```toml
# Directory/path patterns to skip during indexing (regex, with substring fallback)
ignore_patterns = ["node_modules", ".git", "target", "build", "dist"]

[index]
batch_size = 32          # images embedded per CLIP batch

[search]
distance_threshold = 1.3 # max cosine distance to include (lower = stricter)
max_k = 100              # upper bound on the KNN result-set ceiling
```

## Advanced Usage

### Environment Variables

- `RUST_LOG=info`: Enable detailed logging
- `RUST_LOG=debug`: Enable debug logging for troubleshooting

### Search Tips

- Use descriptive, natural language queries
- Try different phrasings if initial results aren't optimal
- Use `--limit` to control number of results
- Tighten or loosen matching with `--threshold` (lower = stricter)

## Development

### Dependencies

- Rust 1.85+
- CLIP model (downloaded automatically on first use)
- `clipper` crate (local path dep at `../clipper`, must be present to build)
- No system SQLite required — `turso` is a pure-Rust engine

### Building

```bash
cargo build --release --workspace   # both binaries
cargo test --workspace              # all tests
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

### Third-party dependency licenses

imgfind statically links **[rawler](https://github.com/dnglab/dnglab)** (camera
RAW decoding), which is licensed under **LGPL-2.1**. This is fully compatible
with imgfind's MIT/Apache-2.0 license for normal use. Note for redistributors:
because rawler is statically linked, LGPL-2.1 §6 requires that recipients of a
**distributed binary** be able to relink it against a modified rawler. imgfind's
own source being openly available here satisfies that obligation; if you ship a
binary, retain this notice and point recipients to the rawler source. Other
dependencies are under permissive licenses (MIT/Apache-2.0/BSD).

## Acknowledgments

- [CLIP](https://openai.com/blog/clip/) by OpenAI for the underlying model
- [oshash-rs](https://github.com/stevenwcarter/oshash-rs) for efficient media hashing
- The Rust community for excellent ecosystem tools

---

(c) 2025 `imgfind` Contributors
