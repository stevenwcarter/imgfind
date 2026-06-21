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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_size_is_transparent_bytes() {
        assert_eq!(serde_json::to_string(&FileSize(1024)).unwrap(), "1024");
        assert!(FileSize(10) < FileSize(20));
    }
}
