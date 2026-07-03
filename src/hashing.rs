//! File hashing for indexing/dedup/thumbnail-cache keying.
//!
//! `oshash` is fast (it reads only the file size plus the first and last
//! 64 KB) but rejects files smaller than 128 KB. Those files fall back to a
//! full-content md5 — cheap precisely because the file is small.

use oshash::{HashError, oshash};

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
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn small_file_uses_md5_32_hex_stable_and_content_sensitive() {
        // 1 KB (< 128 KB) -> md5 fallback, 32-char lowercase hex.
        let f = write_temp(b"small file contents that are well under 128 KB");
        let path = f.path().to_str().unwrap();
        let h1 = hash_file(path).expect("hash small file");
        let h2 = hash_file(path).expect("hash small file again");
        assert_eq!(h1.len(), 32, "md5 hex is 32 chars, got {h1:?}");
        assert!(
            h1.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
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
