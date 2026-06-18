# RAW format support (NEF, DNG, ORF, and the rest)

**Date:** 2026-06-17
**Status:** Approved (via `/ship-it --ask`)
**Branch:** `raw-format-support`

## Goal

Make the scan/index flow discover and process camera RAW files — starting with the
user's must-have **NEF**, plus **DNG**, **ORF**, and every other format the chosen
decoder supports (CR2, CR3, ARW, RAF, RW2, PEF, SRW, NRW, …). RAW files must flow
through the *entire* existing pipeline — embedding, thumbnails, EXIF metadata, TUI
display, and inline terminal display — producing the same DB rows and UX as JPEG/PNG
inputs.

## Background — why this is non-trivial

Every pixel-decode in imgfind goes through the `image` crate (v0.25), which does **not**
decode proprietary RAW sensor data and only unreliably opens TIFF-based DNG. Today a
RAW file isn't even *discovered*: the scan allowlist (`main.rs:371`) is a hardcoded set
of `jpg/jpeg/png/gif/bmp/tiff/webp`. So two things are missing — discovery (extension
allowlist) and decode (a RAW-aware decoder).

EXIF metadata (camera make/model, GPS, datetime) is **already** handled for NEF/DNG/ORF,
because `kamadak-exif`'s `read_from_container` (`database.rs:1380`) detects their TIFF
container. The only metadata sub-step that fails on RAW is the **pixel-dimensions** read,
which currently decodes via the `image` crate (`database.rs:1369`).

## Decode strategy (decided)

A RAW file embeds two usable image sources: (1) a camera-rendered **JPEG preview**
(often full-resolution) and (2) the **raw sensor data** (CFA mosaic). Our targets are a
~224px CLIP tensor and a ~300px thumbnail — both far below preview resolution.

**Strategy: embedded preview first, full demosaic as fallback.** Try to extract the
largest embedded preview; if none exists, demosaic the sensor data to sRGB. This gives
the widest coverage (every RAW yields *something*) while keeping the common path fast
(no demosaic when a preview is present).

## Decoder engine (decided): `rawler`

`rawler` (0.7.2, pure Rust, from the dnglab project) is the single dependency that
satisfies the whole strategy and has the broadest actively-maintained Rust RAW format
set. Both halves of the strategy use one crate:

- **Preview path** — the `Decoder` trait exposes
  `full_image(&RawDecodeParams) -> Result<Option<image::DynamicImage>>`,
  `preview_image(...)`, and `thumbnail_image(...)`. These return `image::DynamicImage`
  directly — the exact type the embedding/thumbnail/TUI paths already consume.
- **Demosaic fallback** — `Decoder::raw_image(...) -> Result<RawImage>` plus
  `rawler::imgop::develop::RawDevelop` develop the sensor data to an RGB/sRGB buffer we
  wrap into a `DynamicImage`.

Rejected alternatives: rawler-for-preview + imagepipe/rawloader-for-develop (two RAW
parsers, more deps, no fidelity win); libraw-rs (C FFI — build complexity, cuts against
the pure-Rust/musl-friendly default).

> **Note for implementation:** exact rawler entry-point names/signatures
> (`get_decoder` vs `decode_file`, `RawDecodeParams::default`, the precise `RawDevelop`
> builder call and how to turn its output into a `DynamicImage`) must be pinned against
> docs.rs/source for 0.7.2 at implementation time, not guessed. The architecture below
> does not depend on which exact method names are used, only that rawler yields a
> `DynamicImage` for both the preview and the developed-fallback path.

## Architecture — one decode seam

Introduce a new module **`src/decode.rs`** exposing a single function that every
pixel-decode in the codebase routes through:

```rust
/// Decode any supported still or RAW image to a DynamicImage.
/// RAW files (by extension) are decoded via rawler: largest embedded preview first,
/// full demosaic as fallback. All other extensions use the `image` crate as today.
pub fn decode_image(path: &Path) -> anyhow::Result<image::DynamicImage>
```

Supporting items in the same module:

```rust
/// Lowercased extensions the `image` crate handles (today's set).
pub const STILL_EXTENSIONS: &[&str] = &["jpg","jpeg","png","gif","bmp","tiff","webp"];

/// Lowercased RAW extensions rawler can decode. "All formats the decoder supports."
pub const RAW_EXTENSIONS: &[&str] = &[
    "nef","nrw",         // Nikon
    "dng",               // Adobe / generic
    "orf",               // Olympus
    "cr2","cr3","crw",   // Canon
    "arw","sr2","srf",   // Sony
    "raf",               // Fujifilm
    "rw2",               // Panasonic
    "pef",               // Pentax
    "srw",               // Samsung
    "erf",               // Epson
    "mrw",               // Minolta
    "raw","rwl",         // Leica / misc
    "iiq","3fr","fff",   // Phase One / Hasselblad
    "mef","mos","kdc","dcr", // Mamiya / Leaf / Kodak
];

/// Union used by the scanner. Membership test is case-insensitive.
pub fn is_supported_extension(ext: &str) -> bool { … }
pub fn is_raw_extension(ext: &str) -> bool { … }
```

`decode_image` dispatch:
1. Lowercase the path's extension.
2. If it's in `RAW_EXTENSIONS` → `decode_raw(path)` (rawler: preview → develop fallback;
   error if both fail).
3. Else → `image::open(path)` (today's behavior, byte-for-byte unchanged for non-RAW).

> The `RAW_EXTENSIONS` list is the single, explicit, reviewable place where format
> coverage lives. Trimming or extending coverage later is a one-line edit here.

### Call sites routed through the seam

| # | Site | Today | After |
|---|------|-------|-------|
| 1 | `main.rs:515` (index embedding) | `image::open(abs_path)` | `decode::decode_image(abs_path)` |
| 2 | `thumbnail.rs:166` (`generate_thumbnail_bytes`) | `ImageReader::open().decode()` | `decode::decode_image(path)` then resize |
| 3 | `tui/app/zoom.rs:135` (TUI full view) | `ImageReader::open().decode()` | `decode::decode_image(&image_path)` |
| 4 | `database.rs:1369` (`extract_image_metadata` dimensions) | `ImgReader::open().decode()` | `decode::decode_image(Path::new(file_path))` (best-effort; failure leaves width/height `None`, as today) |
| 5 | `main.rs:806` (`print_image`, `search --display`) | `iterm2img::from_bytes(raw file bytes)` | for RAW extensions: `decode_image` → re-encode to JPEG bytes → `iterm2img::from_bytes`; non-RAW unchanged (original bytes streamed as today) |

The scanner allowlist at `main.rs:371` is replaced by `decode::is_supported_extension`,
so RAW files are discovered. EXIF extraction (`database.rs:1378`+) is untouched — it
already reads RAW containers.

The GUI needs **no** direct change: it renders from DB-cached thumbnails, which become
RAW-capable once site #2 uses the seam.

## Data flow (RAW index, end to end)

```
walk dir → ext in is_supported_extension? → oshash (content hash) → already indexed? skip
   ↓ (new)
decode_image(nef)  ── rawler ──►  embedded preview?  ── yes ─►  DynamicImage
                                        └─ no ─► raw_image → RawDevelop → DynamicImage
   ↓
clipper get_image_embeddings_from_dynamic (resizes to model image_size)  → normalize → image_vectors
   ↓
extract_image_metadata(nef): EXIF (make/model/GPS/datetime) via kamadak-exif  +  dimensions via decode_image
   ↓
later: thumbnails (decode_image → resize → JPEG blob), TUI/inline display via seam
```

`oshash` is a content hash over file bytes — format-agnostic, so change-detection and the
relative-path storage invariant are unaffected by RAW inputs.

## Error handling

A RAW file where **both** preview extraction and demosaic fail is treated exactly like a
corrupt JPEG today: `decode_image` returns `Err`, the indexing loop logs a warning,
increments `error_count`, advances the progress bar, and continues. No batch is aborted
by one bad file (decode is already per-file in the batch loop). For `extract_image_metadata`,
a decode failure leaves `width`/`height` as `None` (unchanged best-effort semantics).

## Invariants this feature depends on

Per project spec-discipline, the seam is a shared funnel; record what relies on it so a
later change touching `decode.rs` can grep for dependents:

1. **`decode_image` returns the same `image::DynamicImage` type the pipeline already
   consumes** — clipper preprocessing, `resize`, `zoom_center`, and `.dimensions()` all
   accept any `DynamicImage` regardless of source (preview vs developed vs `image`-crate).
   *Test:* round-trip a known still image through `decode_image` and assert it matches
   `image::open` (non-RAW path is behavior-preserving).
2. **Non-RAW decode is byte-for-byte unchanged.** The seam must not alter results for
   jpg/png/etc. *Test:* dispatch + equivalence test above.
3. **A RAW extension routes to the RAW decoder, a still extension to `image::open`.**
   *Test:* `is_raw_extension` / dispatch unit tests over the extension tables
   (case-insensitivity included).
4. **A real RAW file decodes to a non-empty image.** *Test:* integration test over a
   committed sample RAW fixture (see Testing).
5. **EXIF still reads RAW containers** (kamadak-exif). *Test:* metadata extraction over
   the sample RAW fixture asserts at least one EXIF field (e.g. camera make) is populated
   and dimensions are `Some`.

## Testing

- **Unit (pure, no I/O):** extension classification — `is_supported_extension`,
  `is_raw_extension`, case-insensitivity, and that the dispatch picks the RAW branch only
  for RAW extensions. These guard invariants 2 and 3 without needing a RAW file.
- **Integration (real decode):** commit one small **RAW fixture** under `tests/fixtures/`
  and assert `decode_image` yields a non-empty image and that `extract_image_metadata`
  populates dimensions + at least one EXIF field (invariants 1, 4, 5).
  - Fixture sourcing (resolve in plan): prefer a **small DNG** (open spec, broadly
    decodable, often the smallest real-world RAW). If no suitably small/licensable real
    fixture is available, the integration test is gated `#[ignore]` with a comment naming
    the fixture path it expects, and the unit tests remain the always-on guard. Do **not**
    fabricate a fake "RAW" that only the test understands — that would test nothing real.
- **Regression:** existing decode/thumbnail/metadata tests must stay green (non-RAW path
  unchanged).

## Out of scope

- Demosaic *quality* tuning (white balance, color matrices) — `RawDevelop` defaults are
  sufficient for 224px CLIP and thumbnails.
- RAW+JPEG sidecar pairing / dedup.
- A GUI map view or any new UI — unchanged.
- Writing/exporting RAW or DNG conversion.

## Documentation to update on completion

- `README.md` and `USAGE.md` supported-formats lists → add RAW formats.
- `CLAUDE.md` architecture notes → mention `src/decode.rs` as the single decode seam and
  that RAW is decoded via rawler (preview → develop fallback).
