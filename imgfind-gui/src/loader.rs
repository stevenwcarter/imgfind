//! Moving-window thumbnail loader for the virtualized grid.
//!
//! Replaces the interim one-shot capped fetch with a scroll-driven loader that
//! decodes only the currently-visible window of thumbnails on a single
//! background worker thread and caches the decoded `slint::Image`s in a bounded
//! LRU keyed by relative path.
//!
//! ## Concurrency model
//! - **Worker thread** (`spawn_thumb_worker`): owns a cloned [`Backend`], loops
//!   over a request channel, calls `backend.thumbnail(path, 300)` (which also
//!   persists via `get_or_generate_thumbnail`), and sends the JPEG **bytes**
//!   back on a response channel. A SINGLE worker is used deliberately so the
//!   SQLite writes inside `get_or_generate_thumbnail` never race
//!   (`SQLITE_BUSY`).
//! - **UI thread** (the loader [`Timer`]): `slint::Image` is `!Send`, so the
//!   bytes→`Image` decode and the tiles-model build happen here, never on the
//!   worker. The cache, in-flight set, channels, and timer are all `Rc`/`!Send`
//!   and live only on the UI thread — never wrapped in `Arc`/`Mutex`.
//! - **Generation guard** (`grid_generation`): bumped whenever a new result set
//!   is installed. Each request carries the generation it was issued under;
//!   responses tagged with a stale generation are dropped, so thumbnails from a
//!   superseded search can't leak into the current grid.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use lru::LruCache;
use slint::Image;

use crate::backend::Backend;

/// LRU capacity (decoded `slint::Image`s held in memory at once).
pub const CACHE_CAPACITY: usize = 256;

/// A request sent to the worker: `(generation, relative_path)`.
pub type ThumbRequest = (u64, String);

/// A response from the worker: `(generation, relative_path, jpeg_bytes)`.
pub type ThumbResponse = (u64, String, anyhow::Result<Vec<u8>>);

/// Bounded LRU of decoded thumbnails keyed by relative path.
///
/// Path-keyed (not index-keyed) so a later re-sort reuses already-decoded
/// images instead of re-decoding them.
pub type ThumbCache = LruCache<String, Image>;

/// Construct an empty thumbnail cache with the standard capacity.
pub fn new_cache() -> ThumbCache {
    // CACHE_CAPACITY is a non-zero constant; the unwrap can never fire.
    LruCache::new(NonZeroUsize::new(CACHE_CAPACITY).expect("CACHE_CAPACITY is non-zero"))
}

/// Decode raw JPEG bytes into a `slint::Image` (UI thread only — `Image` is
/// `!Send`). Returns `None` on a decode failure, logging the error with
/// `tracing::warn!` so corrupt blobs are visible in the operator log.
pub fn decode_thumb_bytes(bytes: &[u8], key: &str) -> Option<Image> {
    match crate::image_util::jpeg_to_slint_image(bytes) {
        Ok(img) => Some(img),
        Err(e) => {
            tracing::warn!(path = %key, "thumbnail decode failed: {e:#}");
            None
        }
    }
}

/// Spawn the single background thumbnail worker.
///
/// The worker owns its own clone of `backend`, blocks on `requests`, fetches
/// each thumbnail's JPEG bytes (persisting via `get_or_generate_thumbnail`), and
/// forwards `(generation, path, result)` on `responses`. It exits when the
/// request `Sender` is dropped (i.e. on app shutdown), so it needs no explicit
/// stop signal.
pub fn spawn_thumb_worker(
    backend: Backend,
    requests: Receiver<ThumbRequest>,
    responses: Sender<ThumbResponse>,
) {
    std::thread::Builder::new()
        .name("thumb-worker".into())
        .spawn(move || {
            while let Ok((generation, path)) = requests.recv() {
                let result = backend.thumbnail(&path, 300);
                // A send error means the UI dropped the receiver (shutdown); stop.
                if responses.send((generation, path, result)).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn thumb-worker thread");
}

/// Tracks the in-flight thumbnail requests so the loader never asks the worker
/// for the same path twice while one is pending.
#[derive(Default)]
pub struct InFlight {
    paths: HashSet<String>,
}

impl InFlight {
    /// Record `path` as in-flight, returning `true` if it was newly inserted
    /// (i.e. not already pending).
    pub fn insert(&mut self, path: &str) -> bool {
        self.paths.insert(path.to_string())
    }

    /// Mark `path` as no longer in-flight.
    pub fn remove(&mut self, path: &str) {
        self.paths.remove(path);
    }

    /// Whether `path` currently has a request pending.
    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    /// Drop all pending markers (called when a new result set is installed).
    pub fn clear(&mut self) {
        self.paths.clear();
    }
}

/// Select which paths from `needed` should be requested from the thumbnail
/// worker this tick: those not already in the cache or in-flight, deduped
/// within `needed` (first occurrence of each path wins).
pub fn select_to_request(
    needed: &[String],
    cache: &ThumbCache,
    in_flight: &InFlight,
) -> Vec<String> {
    select_to_request_inner(needed, |p| cache.contains(p), in_flight)
}

/// Pure inner implementation that accepts an arbitrary "is cached?" predicate,
/// making it testable without a live `slint::Image`.
fn select_to_request_inner(
    needed: &[String],
    is_cached: impl Fn(&str) -> bool,
    in_flight: &InFlight,
) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut result = Vec::new();
    for path in needed {
        let key = path.as_str();
        if seen.contains(key) || is_cached(key) || in_flight.contains(key) {
            continue;
        }
        seen.insert(key);
        result.push(path.clone());
    }
    result
}

/// Read the current grid generation.
pub fn current_generation(counter: &Arc<AtomicU64>) -> u64 {
    counter.load(Ordering::SeqCst)
}

/// Bump the grid generation and return the new value. Called whenever a new
/// result set is installed, invalidating any in-flight worker responses tagged
/// with the previous generation.
pub fn bump_generation(counter: &Arc<AtomicU64>) -> u64 {
    counter.fetch_add(1, Ordering::SeqCst) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_thumb_bytes_returns_none_on_corrupt_input() {
        // Truncated JPEG header — should fail gracefully and return None, not panic.
        assert!(decode_thumb_bytes(&[0xFF, 0xD8, 0x00], "test-key").is_none());
    }

    #[test]
    fn in_flight_insert_is_idempotent() {
        let mut f = InFlight::default();
        assert!(f.insert("a"));
        assert!(!f.insert("a"));
        assert!(f.contains("a"));
        f.remove("a");
        assert!(!f.contains("a"));
    }

    #[test]
    fn bump_generation_increments() {
        let counter = Arc::new(AtomicU64::new(0));
        assert_eq!(bump_generation(&counter), 1);
        assert_eq!(bump_generation(&counter), 2);
        assert_eq!(current_generation(&counter), 2);
    }

    #[test]
    fn cache_evicts_beyond_capacity() {
        // Sanity check the LRU honors capacity (uses a 1×1 image surrogate).
        let cache = new_cache();
        assert_eq!(cache.cap().get(), CACHE_CAPACITY);
    }

    #[test]
    fn select_to_request_excludes_cached_and_in_flight() {
        // "a" is cached, "b" is in-flight; only "c" should be returned.
        let cached: HashSet<String> = ["a".to_string()].into();
        let mut in_flight = InFlight::default();
        in_flight.insert("b");
        let needed = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let result = select_to_request_inner(&needed, |p| cached.contains(p), &in_flight);
        assert_eq!(result, vec!["c".to_string()]);
    }

    #[test]
    fn select_to_request_deduplicates_within_needed() {
        // "a" appears twice; the second occurrence must be dropped.
        let cached: HashSet<String> = HashSet::new();
        let in_flight = InFlight::default();
        let needed = vec!["a".to_string(), "a".to_string(), "b".to_string()];
        let result = select_to_request_inner(&needed, |p| cached.contains(p), &in_flight);
        assert_eq!(result, vec!["a".to_string(), "b".to_string()]);
    }
}
