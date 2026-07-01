# Small-file md5 hash fallback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Index files smaller than 128 KB (currently silently dropped) by falling back to a full-content md5 hash when `oshash` rejects them as too small.

**Architecture:** Add a single `src/hashing.rs` module exposing `hash_file(path) -> Result<String, oshash::HashError>` that calls `oshash` and, only on `HashError::FileTooSmall`, reads the (small) file whole and returns its md5 hex. The lone indexing call site (`src/main.rs:548`) switches from `oshash(...)` to `imgfind::hashing::hash_file(...)`. No DB/schema change: the hash is opaque TEXT, and md5's 32-char hex never collides with oshash's 16-char hex.

**Tech Stack:** Rust (edition 2024), `oshash = "0.4"`, `md5 = "0.8"` (already in the dependency tree via `rawler`; promoted to a direct dependency), `tempfile` (dev) for tests.

## Global Constraints

- Rust edition 2024; errors elsewhere use `anyhow`, but this module's public fn returns `oshash::HashError` to keep the existing call-site `match` unchanged.
- `oshash` minimum file size: 128 KB (`MIN_FILE_SIZE = 2 * 65536`); below it returns `HashError::FileTooSmall`.
- `oshash` output: 16-char lowercase hex. md5 output via `format!("{:x}", md5::compute(bytes))`: 32-char lowercase hex.
- `HashError` implements `From<io::Error>`, so `?` on `std::fs::read` maps into the return type.
- Only `FileTooSmall` triggers the fallback; every other `Ok`/`Err` passes through unchanged.

---

### Task 1: md5 fallback hashing module + call-site switch

**Files:**
- Modify: `Cargo.toml` (add `md5 = "0.8"` under `[dependencies]`)
- Create: `src/hashing.rs`
- Modify: `src/lib.rs` (add `pub mod hashing;`)
- Modify: `src/main.rs:548` (call `imgfind::hashing::hash_file` instead of `oshash`)
- Test: inline `#[cfg(test)]` module in `src/hashing.rs`

**Interfaces:**
- Produces: `imgfind::hashing::hash_file(path: &str) -> Result<String, oshash::HashError>`
- Consumes: `oshash::{oshash, HashError}`, `md5::compute`

- [ ] **Step 1: Add the direct dependency**

In `Cargo.toml`, under `[dependencies]` (e.g. right after the `oshash = "0.4"` line at line 31), add:

```toml
md5 = "0.8"
```

- [ ] **Step 2: Write the failing tests**

Create `src/hashing.rs` with the test module first (the function body can be a stub that won't compile-fail the tests' intent — but to follow TDD, write the tests, confirm they fail, then implement). Full test module:

```rust
#[cfg(test)]
mod tests {
    use super::hash_file;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(bytes: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(bytes).expect("write temp file");
        f.flush().expect("flush temp file");
        f
    }

    #[test]
    fn large_file_uses_oshash_16_hex() {
        // 200 KB (> 128 KB) -> oshash path, 16-char lowercase hex.
        let f = write_temp(&vec![7u8; 200 * 1024]);
        let h = hash_file(f.path().to_str().unwrap()).expect("hash large file");
        assert_eq!(h.len(), 16, "oshash hex is 16 chars, got {h:?}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn small_file_uses_md5_32_hex_stable_and_content_sensitive() {
        // 1 KB (< 128 KB) -> md5 fallback, 32-char lowercase hex.
        let f = write_temp(b"small file contents that are well under 128 KB");
        let path = f.path().to_str().unwrap();
        let h1 = hash_file(path).expect("hash small file");
        let h2 = hash_file(path).expect("hash small file again");
        assert_eq!(h1.len(), 32, "md5 hex is 32 chars, got {h1:?}");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(h1, h2, "md5 must be stable for identical content");

        let g = write_temp(b"different small contents");
        let h3 = hash_file(g.path().to_str().unwrap()).expect("hash other small file");
        assert_ne!(h1, h3, "different content must hash differently");
    }

    #[test]
    fn empty_file_hashes_via_md5() {
        // 0 bytes is < 128 KB -> md5 of empty input, 32 chars (d41d8cd9...).
        let f = write_temp(b"");
        let h = hash_file(f.path().to_str().unwrap()).expect("hash empty file");
        assert_eq!(h.len(), 32);
        assert_eq!(h, "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn nonexistent_path_errors() {
        let err = hash_file("/no/such/file/at/all.bin");
        assert!(err.is_err(), "missing file should error, not panic");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p imgfind hashing::tests`
Expected: FAIL — `hash_file` not found (module has only the test submodule so far).

- [ ] **Step 4: Implement `hash_file`**

Prepend to `src/hashing.rs` (above the test module):

```rust
//! File hashing for indexing/dedup/thumbnail-cache keying.
//!
//! `oshash` is fast (it reads only the file size plus the first and last
//! 64 KB) but rejects files smaller than 128 KB. Those files fall back to a
//! full-content md5 — cheap precisely because the file is small.

use oshash::{oshash, HashError};

/// Hash a file for indexing.
///
/// Files ≥ 128 KB use `oshash` (16-char lowercase hex). Smaller files, which
/// `oshash` rejects with [`HashError::FileTooSmall`], fall back to a
/// full-content md5 (32-char lowercase hex). The two never collide because
/// their hex widths differ. Any other error (I/O, etc.) is propagated
/// unchanged so callers keep their existing handling.
pub fn hash_file(path: &str) -> Result<String, HashError> {
    match oshash(path) {
        Err(HashError::FileTooSmall) => {
            let bytes = std::fs::read(path)?; // io::Error -> HashError
            Ok(format!("{:x}", md5::compute(bytes)))
        }
        other => other,
    }
}
```

- [ ] **Step 5: Export the module**

In `src/lib.rs`, add alongside the other `pub mod` declarations:

```rust
pub mod hashing;
```

- [ ] **Step 6: Switch the call site**

In `src/main.rs` at line 548, change:

```rust
let hash = match oshash(&path_str) {
```

to:

```rust
let hash = match imgfind::hashing::hash_file(&path_str) {
```

Then remove the now-unused `use oshash::oshash;` import at `src/main.rs:10` (delete the line). Leave the rest of the `match` arms (Ok → use hash; Err → `warn!` + `error_count += 1` + `continue`) untouched.

- [ ] **Step 7: Run tests + build to verify they pass**

Run: `cargo test -p imgfind hashing::tests`
Expected: PASS (4 tests).

Run: `cargo build -p imgfind`
Expected: builds clean, no unused-import warning for `oshash`.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/hashing.rs src/lib.rs src/main.rs
git commit -m "feat(index): md5 fallback hash for sub-128KB files"
```

---

## Self-Review

- **Spec coverage:** hashing seam (Task 1, steps 4–5) ✓; call-site change + unused import removal (step 6) ✓; no DB/schema change (none in plan) ✓; dependency promotion (step 1) ✓; all four spec tests — large/small/empty/nonexistent (step 2) ✓.
- **Placeholder scan:** none — all code blocks complete.
- **Type consistency:** `hash_file(&str) -> Result<String, HashError>` consistent across module, export, and call site; `md5::compute(bytes)` + `{:x}` matches md5 0.8 `LowerHex` impl.
