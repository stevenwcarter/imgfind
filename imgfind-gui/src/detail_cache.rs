//! UI-thread-only LRU of decoded 512px detail-panel images.
//!
//! `slint::Image` is `!Send`, and the detail-load path marshals work back via
//! `slint::invoke_from_event_loop` (whose closures must be `Send`). Capturing an
//! `Rc`/non-`Send` cache into those closures is therefore impossible. Instead we
//! keep the cache in a `thread_local!` that lives on the event-loop thread — all
//! detail-image reads/writes happen on that thread, so no `Send` capture is ever
//! required and the `!Send` `Image` never crosses a thread boundary.
//!
//! Background threads only ever carry `Vec<u8>` JPEG bytes; decoding to an
//! `Image` and inserting into this cache happens inside the UI-thread closure.

use std::cell::RefCell;
use std::num::NonZeroUsize;

use lru::LruCache;
use slint::Image;

/// Capacity (number of decoded detail images held). Generous enough to cover a
/// run of forward/back navigation plus preloaded neighbors.
const DETAIL_CACHE_CAPACITY: usize = 64;

thread_local! {
    static DETAIL_IMAGE_CACHE: RefCell<LruCache<String, Image>> =
        RefCell::new(LruCache::new(
            NonZeroUsize::new(DETAIL_CACHE_CAPACITY).expect("DETAIL_CACHE_CAPACITY is non-zero"),
        ));
}

/// Return the cached decoded image for `key` (the relative path), if present,
/// promoting it to most-recently-used. UI thread only.
pub fn get(key: &str) -> Option<Image> {
    DETAIL_IMAGE_CACHE.with_borrow_mut(|c| c.get(key).cloned())
}

/// Insert (or refresh) the decoded image for `key`. UI thread only.
pub fn insert(key: String, image: Image) {
    DETAIL_IMAGE_CACHE.with_borrow_mut(|c| {
        c.put(key, image);
    });
}

/// Drop the cached image for `key` so the next detail-panel view re-decodes it.
/// Used after an edit is accepted and the rebaked thumbnail must replace the
/// stale decoded copy. UI thread only.
pub fn remove(key: &str) {
    DETAIL_IMAGE_CACHE.with_borrow_mut(|c| {
        c.pop(key);
    });
}
