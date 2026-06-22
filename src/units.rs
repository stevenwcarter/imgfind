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

/// The dimensionality of a CLIP model's embedding vectors (512 for the default
/// `openai/clip-vit-base-patch32`, 768 for the LAION ViT-L/14). A newtype so the
/// per-model dimension can't be confused with other bare `usize` counts (limits,
/// indices) when threaded into the `F32_BLOB({dim})` schema SQL. Plain (no serde):
/// the dimension lives in the `models` DB table, not in any serialized struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingDim(pub usize);

impl EmbeddingDim {
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A thumbnail's long-edge target in pixels (e.g. the GUI grid/detail/lightbox
/// sizes 300/512/2048). A newtype so a pixel size can't be transposed with the
/// batch `count`/`limit` or any other bare `u32` threaded through the thumbnail
/// cache and the `thumbnails (image_hash, size)` table. Plain (no serde): sizes
/// are compile-time constants and bound `i64` columns, never serialized structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThumbnailSize(pub u32);

impl ThumbnailSize {
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Which rendition of an image to fetch / generate / store. The DB
/// `thumbnails.size` column encodes a `ScaleSize` as its pixel value and
/// `FullSize` as the sentinel `0`. That encoding lives only in
/// `to_db_size`/`from_db_size`, so the `0` can never leak into application
/// logic — callers always pass and match on the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbnailSpec {
    /// A scaled thumbnail with the given long-edge target (e.g. 300/512/2048).
    ScaleSize(ThumbnailSize),
    /// The original, full-resolution rendition.
    FullSize,
}

impl ThumbnailSpec {
    /// On-disk `thumbnails.size` value. `FullSize` → 0; `ScaleSize(px)` → px.
    pub const fn to_db_size(self) -> u32 {
        match self {
            ThumbnailSpec::ScaleSize(px) => px.get(),
            ThumbnailSpec::FullSize => 0,
        }
    }

    /// Inverse of [`to_db_size`](Self::to_db_size). `0` → `FullSize`, else
    /// `ScaleSize`.
    pub const fn from_db_size(n: u32) -> Self {
        match n {
            0 => ThumbnailSpec::FullSize,
            n => ThumbnailSpec::ScaleSize(ThumbnailSize(n)),
        }
    }
}

impl From<ThumbnailSize> for ThumbnailSpec {
    fn from(size: ThumbnailSize) -> Self {
        ThumbnailSpec::ScaleSize(size)
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

    #[test]
    fn thumbnail_size_exposes_pixel_value() {
        assert_eq!(ThumbnailSize(512).get(), 512);
        assert_eq!(ThumbnailSize(300), ThumbnailSize(300));
    }

    #[test]
    fn thumbnail_spec_db_size_round_trips() {
        use ThumbnailSpec::*;
        assert_eq!(FullSize.to_db_size(), 0);
        assert_eq!(ScaleSize(ThumbnailSize(2048)).to_db_size(), 2048);
        // round-trip both ways
        assert_eq!(ThumbnailSpec::from_db_size(0), FullSize);
        assert_eq!(
            ThumbnailSpec::from_db_size(300),
            ScaleSize(ThumbnailSize(300))
        );
        for spec in [FullSize, ScaleSize(ThumbnailSize(512))] {
            assert_eq!(ThumbnailSpec::from_db_size(spec.to_db_size()), spec);
        }
    }

    #[test]
    fn thumbnail_size_converts_to_scale_spec() {
        let spec: ThumbnailSpec = ThumbnailSize(300).into();
        assert_eq!(spec, ThumbnailSpec::ScaleSize(ThumbnailSize(300)));
    }
}
