//! Process-wide bounded LRU of detail-panel image metadata, keyed by relative
//! path.
//!
//! Unlike `detail_cache` (UI-thread-only, holds `!Send` `slint::Image`), the
//! metadata read in `spawn_detail_meta` runs on a background thread, so this
//! cache must be `Send`: a `Mutex<LruCache<String, ImageMetadata>>` behind a
//! `OnceLock`. An indexed image's metadata is stable for the lifetime of the
//! GUI (re-indexing is a separate process), so no generation-bump invalidation
//! is needed.

use std::num::NonZeroUsize;
use std::sync::{Mutex, OnceLock};

use imgfind::database::ImageMetadata;
use lru::LruCache;

/// Number of metadata records held. Metadata structs are tiny, so this can be
/// generous relative to a browsing run.
const META_CACHE_CAPACITY: usize = 128;

fn cache() -> &'static Mutex<LruCache<String, ImageMetadata>> {
    static CACHE: OnceLock<Mutex<LruCache<String, ImageMetadata>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(LruCache::new(
            NonZeroUsize::new(META_CACHE_CAPACITY).expect("META_CACHE_CAPACITY is non-zero"),
        ))
    })
}

/// Cached metadata for `key` (relative path), if present; promotes it to MRU.
pub fn get(key: &str) -> Option<ImageMetadata> {
    cache().lock().unwrap().get(key).cloned()
}

/// Insert (or refresh) metadata for `key`.
pub fn insert(key: String, meta: ImageMetadata) {
    cache().lock().unwrap().put(key, meta);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_miss() {
        let meta = ImageMetadata {
            file_size: Some(123),
            width: Some(4),
            height: Some(2),
            coords: None,
            camera_make: Some("Sony".into()),
            camera_model: None,
            datetime_taken: None,
        };
        insert("meta_cache_test_key.jpg".into(), meta.clone());
        let got = get("meta_cache_test_key.jpg").expect("present after insert");
        assert_eq!(got.file_size, meta.file_size);
        assert_eq!(got.camera_make, meta.camera_make);
        assert!(get("meta_cache_absent_key.jpg").is_none());
    }
}
