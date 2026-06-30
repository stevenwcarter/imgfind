# Small-file md5 hash fallback — design

**Date:** 2026-06-30
**Status:** Approved

## Problem

Indexing hashes every file with the `oshash` crate (`src/main.rs:548`, the only
call site). `oshash` requires files to be **≥ 128 KB** (it hashes the file size
plus the first and last 64 KB); for anything smaller it returns
`HashError::FileTooSmall`. The calling code treats *every* error the same way —
log a warning, bump `error_count`, `continue` — so **sub-128 KB images are
silently dropped from the index entirely**. Small thumbnails, icons, web-sized
JPEGs, etc. never get a row, never get embedded, never appear in search.

## Goal

Index small files too, by hashing them with a fast fallback (md5) when `oshash`
rejects them for being too small. Genuine I/O errors keep their current
skip-with-warning behavior.

## Design

### Hashing seam

Add a small, unit-testable module `src/hashing.rs` exposing one function that
becomes the single hashing entry point for indexing:

```rust
use oshash::{oshash, HashError};

/// Hash a file for indexing/dedup/thumbnail-cache keying.
///
/// Uses `oshash` (fast, partial-read) for files ≥ 128 KB. For smaller files,
/// which `oshash` rejects, falls back to a full-content md5 — cheap because the
/// file is small by definition. Any non-`FileTooSmall` error (I/O, etc.) is
/// propagated unchanged so callers keep their existing handling.
pub fn hash_file(path: &str) -> Result<String, HashError> {
    match oshash(path) {
        Err(HashError::FileTooSmall) => {
            let bytes = std::fs::read(path)?; // From<io::Error> for HashError
            Ok(format!("{:x}", md5::compute(bytes)))
        }
        other => other,
    }
}
```

`HashError` already implements `From<io::Error>`, so the `?` on `std::fs::read`
maps cleanly into the existing return type — no signature change rippling into
the caller.

### Call-site change

`src/main.rs:548` changes from `oshash(&path_str)` to
`imgfind::hashing::hash_file(&path_str)`. The surrounding `match` (Ok → use
hash, Err → warn + `error_count += 1` + `continue`) is unchanged. Export the
module from `src/lib.rs` (`pub mod hashing;`).

### Why this is safe with no DB/schema change

- The stored hash is an **opaque TEXT value** (`images.hash`,
  `schema.rs:150`), used only for exact-match dedup (`path = ? AND hash = ?`)
  and as the thumbnail-cache foreign key (`thumbnails.image_hash`). Nothing
  parses or validates its format.
- md5 output is **deterministic and content-derived**, so dedup and thumbnail
  caching behave identically to oshash.
- **No length collision:** oshash emits 16 hex chars (64-bit), md5 emits 32. A
  small file and a large file can never produce the same string, so the two
  algorithms share the column without ambiguity.

### Dependency

Add `md5 = "0.8"` to `[dependencies]` in `Cargo.toml`. The crate is already in
the lockfile/tree transitively (via `rawler`), so this only promotes it to a
direct dependency — no new download, no version bump.

## Testing

Unit tests in `src/hashing.rs` (using `tempfile`, already a dev-dependency):

1. **Large file (≥ 128 KB):** `hash_file` returns a 16-char lowercase-hex
   string (oshash path).
2. **Small file (< 128 KB):** returns a 32-char lowercase-hex string (md5
   path); the value is **stable** across two calls on the same content and
   **differs** for different content.
3. **Empty file:** still hashed via md5 (md5 of empty input), returns 32 chars —
   confirms 0-byte files index rather than error.
4. **Nonexistent path:** returns `Err` (propagated I/O error), not a panic.

## Out of scope

- Re-indexing already-indexed libraries to pick up previously-dropped small
  files happens naturally on the next `imgfind index` run (those paths have no
  row yet, so they're inserted) — no migration or backfill code needed.
- No change to `process`, thumbnails, or embeddings; they consume the stored
  hash unchanged.
