# imgfind - Usage Guide

## Installation

Build both binaries from source:

```bash
cargo build --release --workspace
```

The CLI binary is at `target/release/imgfind`; the native GUI binary is at `target/release/imgfind-gui`.

## Usage

### 1. Index Images

`imgfind index` is **fast**: it records which files exist (walks the directory, hashes each file, inserts rows) without loading CLIP or generating thumbnails. Run `imgfind process` — or just open the GUI — to generate embeddings and thumbnails in the background.

Index all images in the current directory:
```bash
imgfind index
```

Index images in a specific directory recursively:
```bash
imgfind index --dir /path/to/images --recursive
```

Index options:
- `--root` — create the database in the current directory instead of using an existing or global one
- `--reindex` — re-insert rows even for already-indexed paths (force rescan)
- `--batch-size <N>` — row-insert chunk size (default: `[index].batch_size` config, `32`)
- `--model <name>` — set the active embedding model for the subsequent `process` pass
- `--no-thumbnails` — deprecated, accepted but ignored (thumbnails are always deferred to `process`)
- `--quiet` — suppress progress output

```bash
imgfind index --dir ~/Pictures --recursive
imgfind index --dir ~/Pictures --root         # create a new library in ~/Pictures
```

### 2. Process (Generate Embeddings + Thumbnails)

After indexing, run `imgfind process` to complete the deferred work (or just open the GUI and let it run in the background):

```bash
imgfind process                              # process all unfinished work from the cwd's DB
imgfind process --dir ~/Pictures             # target a specific directory's DB
imgfind process --no-embeddings              # thumbnails-only pass (skips loading the CLIP model)
imgfind process --count 32                   # smaller per-batch size (default: 64)
```

`process` runs in three phases in order — 300px thumbnails → CLIP embeddings → 512/2048px thumbnails — followed by an EXIF backfill. Each phase is idempotent: already-complete work is skipped, so the command is safe to re-run and will pick up where it left off. With `--no-embeddings` the CLIP model (~1.7 GB on first download) is never loaded. If an image's thumbnail fails to generate (e.g. a corrupted JPEG), a permanent failure marker is recorded; the image is excluded from future passes. Use `--retry-failed` to clear all markers and re-attempt.

Process options:
- `--count <N>` — per-batch size (default: `64`)
- `--no-embeddings` — thumbnail-only pass; avoids loading the CLIP model
- `--retry-failed` — clear all permanent thumbnail failure markers before the pass, re-attempting previously-failed images
- `-d/--dir <DIR>` — target a specific directory's DB (walk-up/global logic otherwise)

### 3. Search for Images

Search for images matching a natural language query:
```bash
imgfind search "a cat sitting on a chair"
```

Limit the number of results:
```bash
imgfind search "sunset over mountains" --limit 5
```

Search options:
- `--threshold <f>` — max cosine distance to include (lower = stricter); overrides `[search].distance_threshold`
- `--short` — print bare image paths, one per line (useful for piping)
- (search is recursive from the current directory by default; `--recursive`/`-r` is accepted but a no-op)
- `--display` — render result images inline in supporting terminals
- `--all` — search the entire indexed database (including directories above the cwd)
- `--model <name>` — embedding model to use for this run

```bash
imgfind search "beach" --threshold 1.0
imgfind search "beach" --short | xargs -I{} cp {} ~/selected/
```

### 4. Browse in the TUI

Open an interactive terminal UI with inline previews:
```bash
imgfind tui
```

Keybindings:
- `e` — edit search
- `h` / `j` / `k` / `l` — move focus
- `H` / `L` — previous / next page (9 images per page)
- `1`–`9` — zoom that image
- `Enter` — zoom focused image
- `Esc` — close zoom / help
- scroll — zoom in/out (in zoom view); right-click — reset zoom
- `?` — toggle the help overlay (lists all keybindings)
- `q` / `Ctrl-C` — quit

Each thumbnail shows a similarity score label, and the zoom view shows a control hint along the bottom.

### 5. Open the Native GUI

Launch the Slint desktop GUI:
```bash
imgfind gui                   # launcher subcommand: forwards args to imgfind-gui, blocks until it exits
imgfind gui -d ~/Pictures     # -d / --dir targets a specific directory's database
imgfind-gui                   # or run the GUI binary directly (uses the DB found by walking up from cwd)
imgfind-gui --dir ~/Pictures  # --dir / -d target a specific directory's database
```

The GUI launches directly into a browse-all view (or restores the previous session). Results appear as a **virtualized scrollable grid** — the full result set is held in memory and only a bounded window of tiles is decoded at a time, so scrolling stays smooth over arbitrarily large libraries. Thumbnails are generated and persisted on first view; subsequent views load instantly.

If the library has unprocessed images (e.g. after a fresh `imgfind index`), the GUI **auto-starts a background process-worker** that runs the same engine while you browse — 300px thumbnails first, then embeddings, then full thumbnails. It is **pausable**: press `\` (backslash) or click the **progress pill** in the statusline to open the **status panel**, which shows three progress rows (Thumbnails 300px / Embeddings / Full thumbnails), the current state (Running / Paused / Waiting for model... / Idle), and a Pause/Resume button. Closing the panel keeps the worker running. A subtle "N still indexing" hint appears near the search box while embeddings are incomplete; semantic search returns the already-embedded subset and browse/grid work immediately over the full set.

A **sort selector** beside the filter bar lets you order by Name, Size, or Type (with an asc/desc toggle); while a semantic query is active, Relevance is also offered. Browse re-queries the DB; search re-sorts in memory.

Click a thumbnail to open a **detail panel** on the right (512px thumbnail + metadata + "Search similar"). Double-click (or "View full") opens the full-screen **lightbox** (prev/next navigation, Esc to close); the lightbox uses a persisted 2048px cached thumbnail, upgrading to full native resolution on first zoom. In the lightbox, scroll to zoom and drag to pan; the bottom bar has **Fit** and **1:1** (100% / native) buttons, and the keys `+`/`-` step zoom, `0` fits, `1` jumps to 100%. Right-click a thumbnail to open the original file in the OS viewer.

The **filter bar** beneath the search bar includes a "Hide failed" toggle that excludes images with a permanent thumbnail failure marker (images whose decode failed irreparably). Use this to hide broken files from the grid; use `imgfind process --retry-failed` on the command line to clear the markers and re-attempt.

Keyboard navigation: `h/j/k/l` or arrow keys move the selection; `Enter` opens the detail panel; `Space` opens the lightbox; `Esc` closes the panel.

#### Tagging

Assign free-text tags to images from the keyboard (all chord keys work in the grid, detail panel, and lightbox, and are suppressed while typing in a text field or modal):

| Key(s) | Action |
|--------|--------|
| `` ` `` | Toggle the **left rail** (five color brushes + Most Recent staging area) |
| `t` | Open the **add-tags modal** — type space-separated words and press Enter |
| `m` then `r`/`g`/`y`/`p`/`b` | **Paint** the red/green/yellow/purple/blue brush's tags onto the focused image (also copies them into the Most Recent buffer) |
| `mm` | **Re-apply** the current Most Recent buffer to the focused image |
| `f` then `r`/`g`/`y`/`p`/`b` | **Load** that brush's tags into the tag filter (keeps the current AND/OR mode) |
| `ft` | **Toggle** the tag filter on/off without losing the chosen tags |

Color brushes are pure input shortcuts — a brush is a named set of tags. Applying it assigns those tags as ordinary (colorless) tags; no color is ever stored on the image. Edit a brush's tag set in the left rail; click a tag in the Most Recent area to remove it before re-applying with `mm`. Per-image tags can also be edited directly in the right-side detail panel.

Tag filtering appears as a row in the filter pane below the search bar: a tag editor, an AND/OR toggle (match-all vs. match-any), and a slide Toggle to enable/disable without losing the chosen tags. The tag filter combines with the existing size, type, and GPS filters.

Note: the map view is not yet ported to the GUI.

### 6. Generate Thumbnails

Generate thumbnails in batches (outside the GUI, or to pre-generate sizes before first launch — note `imgfind index` no longer generates thumbnails; use `imgfind process` or the GUI background job for that):
```bash
imgfind thumbnails                           # generate missing 300px thumbnails (default, batch of 50)
imgfind thumbnails --size 300 --size 512 --size 2048  # one or more explicit sizes
imgfind thumbnails --gui-sizes               # shorthand for all three GUI sizes (300, 512, 2048)
imgfind thumbnails --gui-sizes --count 200   # larger batch per size
imgfind thumbnails --gui-sizes --all         # loop batches until ALL missing thumbnails are generated
```

The GUI uses three thumbnail sizes: **300px** (grid tiles), **512px** (detail panel), and **2048px** (lightbox/preview, long-edge). All three are persisted in the DB so repeat views load instantly. Pass `--gui-sizes` to pre-generate all three before the first launch to avoid per-image decode on first view.

`--all` loops batches of `--count` (default 50) until no thumbnails remain missing for the requested sizes. Useful for a one-shot "fill in everything" run outside the GUI; note that `imgfind-launcher` no longer runs `thumbnails` after indexing — the GUI's background process-worker handles that when the library opens.

### 7. Manage Embedding Models

Vectors are stored in per-model tables, so multiple models can coexist. Two models are available:

| Model | Dim | Notes |
| --- | --- | --- |
| `openai/clip-vit-base-patch32` | 512 | Default. Fast indexing and search. |
| `laion/CLIP-ViT-L-14-laion2B-s32B-b82K` | 768 | ~1.7 GB download on first use, slower indexing, higher search quality (LAION-2B trained). |

```bash
imgfind models list                 # active marked *; unindexed-but-supported models show [available, not indexed]
imgfind models use <name>           # auto-registers a supported model (creates its vector table) and makes it active
imgfind models use <name> --default # ...and also save it as the global default for new databases
imgfind index  --model <name> ~/Pictures   # select the active model for this run
imgfind search --model <name> "a cat"
```

The active-model choice is stored **per database** (in the DB's `models` table), so it persists across runs and each index can use a different model.

**Global default model.** A new database is seeded with the model named in the global config (`~/.imgfind/config.toml`); if none is set, it falls back to the built-in baseline `openai/clip-vit-base-patch32`. The default only applies when a database is *created* — it never overrides an existing DB's active model or a later `models use`. Manage it with:

```bash
imgfind config model                  # show the current default
imgfind config model <name>           # set the default (validated against supported models)
imgfind config model --clear          # revert to the built-in baseline
imgfind models use <name> --default   # set the active model AND save it as the default in one step
```

So to make a higher-quality model your default everywhere, set it once — every new index you create afterward starts on it:
```bash
imgfind config model laion/CLIP-ViT-L-14-laion2B-s32B-b82K
cd ~/new-folder && imgfind index --root .   # new DB is seeded with the LAION model automatically
```

**Each model has its own vector table.** Switching the active model does not migrate existing embeddings — images must be indexed under a model before they are searchable under it.

To populate the embeddings for a model you just switched to, run `imgfind process` (or open the GUI and let the background worker do it). The embedding phase is per-model: images that lack an embedding for the active model are embedded; images that already have one are skipped. So processing after a switch only does the work needed to backfill the new model; embeddings for other models are left intact. Pass `--reindex` to `index` to force a fresh row-pass (e.g. after files changed), then re-run `process` to re-embed.

A typical workflow for trying the higher-quality model:
```bash
imgfind models use laion/CLIP-ViT-L-14-laion2B-s32B-b82K   # auto-registers (dim 768), sets active
imgfind process                                            # backfills L/14 embeddings (downloads ~1.7GB once)
imgfind search "a dog on a beach"                          # searches the active model's table
imgfind models use openai/clip-vit-base-patch32            # switch back; its embeddings are still there
imgfind process                                            # backfill openai embeddings if any are missing
```

### 8. Shell Completions

Generate a completion script for your shell:
```bash
imgfind completions bash > /etc/bash_completion.d/imgfind
imgfind completions zsh  > ~/.zfunc/_imgfind
imgfind completions fish > ~/.config/fish/completions/imgfind.fish
# or: eval "$(imgfind completions bash)"
```

### 9. Clean Database

Remove entries for images that no longer exist:
```bash
imgfind clean
```

## Database Location

The database is stored at `~/.imgfind/imgfind.db` by default. The tool will also search up the directory tree for an existing database.

## Configuration

Configuration is stored at `~/.imgfind/config.toml` (created on first run). View it with `imgfind config show`.

```toml
# Path patterns to skip during indexing (regex, with substring fallback)
ignore_patterns = ["node_modules", ".git", "target", "build", "dist"]

[index]
batch_size = 32          # images embedded per CLIP batch

[search]
distance_threshold = 1.3 # max cosine distance to include (lower = stricter)
max_k = 100              # upper bound on the KNN result-set ceiling

[gui]
preload_neighbors = 2        # neighbors to preload when lightbox/detail panel opens (default 2)
default_sort = "name"        # initial sort for browse-all: "name" | "size" | "type"
default_sort_direction = "asc"  # "asc" | "desc"
```

Manage ignore patterns from the CLI:
```bash
imgfind config add-ignore "screenshots"
imgfind config remove-ignore "screenshots"
imgfind config reset
```

## Supported Image Formats

- JPEG (.jpg, .jpeg)
- PNG (.png)
- GIF (.gif)
- BMP (.bmp)
- TIFF (.tiff)
- WebP (.webp)
- **RAW (camera):** Nikon (.nef, .nrw), Adobe/generic (.dng), Olympus (.orf), Canon (.cr2, .cr3, .crw), Sony (.arw, .sr2, .srf), Fujifilm (.raf), Panasonic (.rw2), Pentax (.pef), Samsung (.srw), Epson (.erf), Minolta (.mrw), Leica/misc (.raw, .rwl), Phase One/Hasselblad (.iiq, .3fr, .fff), Mamiya/Leaf/Kodak (.mef, .mos, .kdc, .dcr) — decoded via embedded preview with full-demosaic fallback.

## Example Workflow

1. Record which images exist (fast — no CLIP, no thumbnails):
   ```bash
   imgfind index --dir ~/Pictures --recursive
   ```

2. Generate embeddings + thumbnails (or just open the GUI and let it handle this):
   ```bash
   imgfind process --dir ~/Pictures
   ```

3. Search for specific images:
   ```bash
   imgfind search "beach vacation"
   imgfind search "family dinner"
   imgfind search "landscape photography"
   ```

4. Periodically clean up the database:
   ```bash
   imgfind clean
   ```

## Performance Notes

- `imgfind index` is fast — it only walks the directory and records which files exist
- The CLIP model (~1.7 GB for the LAION model, smaller for the default) is downloaded automatically on first use and loaded by `imgfind process` (or the GUI background worker), not by `imgfind index`
- Embeddings are normalized and stored for efficient similarity search
- The database uses the `turso` SQLite engine for reliable storage and native vector search
