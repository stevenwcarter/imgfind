//! Domain value newtypes (non-id). `FileSize` is bytes; `#[serde(transparent)]`
//! keeps the persisted `Filters.size_min/size_max` JSON bare integers.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileSize(pub i64);

impl FileSize {
    pub const fn bytes(self) -> i64 {
        self.0
    }
}

/// Upper bound on the brute-force KNN `k` value (result-set ceiling). A distinct
/// newtype so the search cap can't be transposed with the per-page `limit`.
/// `#[serde(transparent)]` keeps the persisted `SearchConfig.max_k` (read from
/// `config.toml`) a bare integer on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaxK(pub usize);

impl MaxK {
    pub const fn get(self) -> usize {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_size_is_transparent_bytes() {
        assert_eq!(serde_json::to_string(&FileSize(1024)).unwrap(), "1024");
        assert!(FileSize(10) < FileSize(20));
    }

    #[test]
    fn max_k_is_transparent_usize() {
        assert_eq!(serde_json::to_string(&MaxK(100)).unwrap(), "100");
        assert_eq!(
            serde_json::from_str::<MaxK>("250").unwrap(),
            MaxK(250),
            "config.toml stores a bare integer"
        );
        assert_eq!(MaxK(100).get(), 100);
    }
}
