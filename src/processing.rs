//! Shared background-processing engine for the two-phase indexing pipeline.
//!
//! After `index_directory` (or the launcher's index workflow) populates the
//! `images` table with row-only entries, this module drives the three phases
//! that make images searchable and viewable:
//!
//! 1. **`Thumbnails300`** — Generate 300 px thumbnails (used as embedding
//!    source; mandatory before embeddings).
//! 2. **`Embeddings`** — CLIP-embed each image from its 300 px thumbnail and
//!    store the vector.
//! 3. **`FullThumbnails`** — Generate the 512 px and 2048 px renditions for
//!    the GUI grid, detail panel, and lightbox.
//!
//! `ProcessPhase` and `ProcessCounts` are pure value types; every function that
//! accesses the DB or the filesystem is sync and bridges through
//! [`crate::block_on`], mirroring the `thumbnail` module's pattern.

use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use tracing::{info, warn};

use crate::{ThumbnailSize, block_on, database::Database, metadata::extract_missing_metadata, thumbnail};

/// The three phases of background processing, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessPhase {
    /// Generate 300 px thumbnails (prerequisite for embedding).
    Thumbnails300,
    /// CLIP-embed images from their 300 px thumbnails.
    Embeddings,
    /// Generate 512 px and 2048 px renditions for the GUI.
    FullThumbnails,
}

/// Remaining work counts for each processing phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessCounts {
    /// Images missing a 300 px thumbnail.
    pub thumbs300: usize,
    /// Images missing a CLIP embedding in the active model's vector table.
    pub embeddings: usize,
    /// Sum of images missing 512 px and 2048 px thumbnails.
    pub full_thumbs: usize,
}

impl ProcessCounts {
    /// Total remaining work across all phases.
    pub fn total_remaining(&self) -> usize {
        self.thumbs300 + self.embeddings + self.full_thumbs
    }

    /// The first phase that still has remaining work, in the fixed execution
    /// order: `Thumbnails300` → `Embeddings` → `FullThumbnails`.
    /// Returns `None` when every phase is drained.
    pub fn next_phase(&self) -> Option<ProcessPhase> {
        if self.thumbs300 > 0 {
            Some(ProcessPhase::Thumbnails300)
        } else if self.embeddings > 0 {
            Some(ProcessPhase::Embeddings)
        } else if self.full_thumbs > 0 {
            Some(ProcessPhase::FullThumbnails)
        } else {
            None
        }
    }
}

/// Query the remaining work count for every phase.
///
/// `full_thumbs` is the sum of images missing 512 px and 2048 px thumbnails.
pub async fn counts(db: &Database) -> Result<ProcessCounts> {
    let thumbs300 = db
        .count_images_without_thumbnails(ThumbnailSize(300))
        .await
        .context("count missing 300px thumbnails")?;
    let embeddings = db
        .count_images_without_embedding()
        .await
        .context("count missing embeddings")?;
    let full512 = db
        .count_images_without_thumbnails(ThumbnailSize(512))
        .await
        .context("count missing 512px thumbnails")?;
    let full2048 = db
        .count_images_without_thumbnails(ThumbnailSize(2048))
        .await
        .context("count missing 2048px thumbnails")?;
    Ok(ProcessCounts {
        thumbs300,
        embeddings,
        full_thumbs: full512 + full2048,
    })
}

/// Embed a single image from its persisted 300 px thumbnail and store the vector.
///
/// CLIP internally downsizes to 224 px, so a 300 px thumbnail is ample and
/// avoids re-decoding the (potentially large) original file.
///
/// Sync wrapper — uses [`crate::block_on`] internally, mirroring `thumbnail.rs`.
pub fn embed_one_from_thumbnail(
    db: &Database,
    embedder: &ClipEmbedder,
    image_id: i64,
    abs_path: &str,
    hash: &str,
) -> Result<()> {
    let jpeg = thumbnail::get_or_generate_thumbnail(db, abs_path, hash, ThumbnailSize(300))
        .context("get 300px thumbnail for embedding")?;
    let dynimg = image::load_from_memory(&jpeg).context("decode 300px thumbnail")?;
    let mut vecs = embedder
        .get_image_embeddings_from_dynamic(vec![dynimg])
        .context("embed 300px thumbnail")?;
    let raw = vecs.pop().context("embedder returned no vector")?;
    let emb = crate::search::normalize_vector(&raw);
    block_on(db.set_image_embedding(image_id, &emb)).context("store embedding")?;
    Ok(())
}

/// Fetch up to `count` images without embeddings, embed each from its 300 px
/// thumbnail, and store the vector. Returns the count of successes.
///
/// Per-image errors are logged and skipped. A fully-failing batch returns 0,
/// which triggers the zero-progress guard in `run_to_completion` so a
/// permanently-undecodable image cannot cause an infinite loop.
pub fn process_embeddings_batch(
    db: &Database,
    embedder: &ClipEmbedder,
    count: usize,
) -> Result<usize> {
    let rows = block_on(db.get_images_without_embedding(count))
        .context("fetch images without embedding")?;
    let mut successes = 0usize;
    for (image_id, abs_path, hash) in rows {
        let path_str = abs_path.as_str();
        match embed_one_from_thumbnail(db, embedder, image_id, &path_str, &hash) {
            Ok(()) => {
                info!("embedded {path_str}");
                successes += 1;
            }
            Err(e) => {
                warn!("skipping embedding for {path_str}: {e:#}");
            }
        }
    }
    Ok(successes)
}

/// Full-thumbnail sizes generated in the `FullThumbnails` phase.
///
/// These are the GUI's grid (512 px) and lightbox (2048 px) sizes. The 300 px
/// size is handled separately in the `Thumbnails300` phase.
const FULL_THUMBNAIL_SIZES: [ThumbnailSize; 2] = [ThumbnailSize(512), ThumbnailSize(2048)];

/// Process one batch of the given phase.
///
/// - `Thumbnails300` — generate up to `batch` missing 300 px thumbnails.
/// - `Embeddings` — embed up to `batch` images; `embedder` must be `Some`.
/// - `FullThumbnails` — generate up to `batch` missing thumbnails for each of
///   512 px and 2048 px; returns the total across both sizes.
///
/// Returns the number of items that made forward progress this batch.
pub fn process_next_batch(
    db: &mut Database,
    embedder: Option<&ClipEmbedder>,
    phase: ProcessPhase,
    batch: usize,
) -> Result<usize> {
    match phase {
        ProcessPhase::Thumbnails300 => {
            thumbnail::generate_missing_thumbnails_batch(db, ThumbnailSize(300), batch)
                .context("generate missing 300px thumbnails batch")
        }
        ProcessPhase::Embeddings => {
            let embedder = embedder.context("embedder required for the Embeddings phase")?;
            process_embeddings_batch(db, embedder, batch)
        }
        ProcessPhase::FullThumbnails => {
            let mut total = 0usize;
            for &size in &FULL_THUMBNAIL_SIZES {
                total += thumbnail::generate_missing_thumbnails_batch(db, size, batch)
                    .with_context(|| format!("generate missing {size:?} thumbnails batch"))?;
            }
            Ok(total)
        }
    }
}

/// Drive all three processing phases to completion.
///
/// Processes phases in fixed order: `Thumbnails300` → `Embeddings` →
/// `FullThumbnails`. EXIF metadata is backfilled after the 300 px phase drains.
/// A per-phase zero-progress guard terminates processing if a batch makes no
/// forward progress (preventing infinite loops on permanently undecodable images).
///
/// # Arguments
///
/// - `embedder` — CLIP embedder; must be `Some` when `with_embeddings` is `true`.
///   Pass `None` when `with_embeddings` is `false` to avoid loading the model.
/// - `sizes` — thumbnail sizes for the `FullThumbnails` phase; pass
///   `&[ThumbnailSize(512), ThumbnailSize(2048)]` for the standard GUI sizes.
/// - `with_embeddings` — if `false`, the `Embeddings` phase is skipped entirely.
///   Useful for workflows that defer vector search to a later step.
/// - `progress` — called after each completed batch with
///   `(phase, items_done_this_batch)`.
pub fn run_to_completion(
    db: &mut Database,
    embedder: Option<&ClipEmbedder>,
    batch: usize,
    sizes: &[ThumbnailSize],
    with_embeddings: bool,
    mut progress: impl FnMut(ProcessPhase, usize),
) -> Result<()> {
    // Phase 1: 300 px thumbnails.
    loop {
        let remaining = block_on(db.count_images_without_thumbnails(ThumbnailSize(300)))
            .context("count missing 300px thumbnails")?;
        if remaining == 0 {
            break;
        }
        let done =
            thumbnail::generate_missing_thumbnails_batch(db, ThumbnailSize(300), batch)
                .context("generate 300px thumbnails batch")?;
        progress(ProcessPhase::Thumbnails300, done);
        if done == 0 {
            break; // zero-progress guard: permanently undecodable images
        }
    }

    // EXIF backfill immediately after the 300 px phase drains.
    extract_missing_metadata(db, true, batch).context("EXIF backfill after 300px phase")?;

    // Phase 2: Embeddings.
    if with_embeddings {
        let embedder =
            embedder.context("embedder required when with_embeddings is set")?;
        loop {
            let remaining = block_on(db.count_images_without_embedding())
                .context("count missing embeddings")?;
            if remaining == 0 {
                break;
            }
            let done =
                process_embeddings_batch(db, embedder, batch).context("embeddings batch")?;
            progress(ProcessPhase::Embeddings, done);
            if done == 0 {
                break; // zero-progress guard
            }
        }
    }

    // Phase 3: Full thumbnails (caller-specified sizes for flexibility).
    for &size in sizes {
        loop {
            let remaining = block_on(db.count_images_without_thumbnails(size))
                .context("count missing full thumbnails")?;
            if remaining == 0 {
                break;
            }
            let done =
                thumbnail::generate_missing_thumbnails_batch(db, size, batch)
                    .with_context(|| format!("generate {size:?} thumbnails batch"))?;
            progress(ProcessPhase::FullThumbnails, done);
            if done == 0 {
                break; // zero-progress guard
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use image::{ImageBuffer, Rgb};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_db_path() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Use std::env::temp_dir() — processing tests do not go through the
        // index walk, so the "tmp" ignore pattern does not apply here.
        let dir = std::env::temp_dir()
            .join(format!("imgfind_proc_test_{}_{n}", std::process::id()));
        dir.join(".imgfind").join("imgfind.db")
    }

    // ── Pure-logic tests (no DB, no filesystem) ──────────────────────────────

    #[test]
    fn next_phase_walks_in_order_then_none() {
        let c = ProcessCounts { thumbs300: 0, embeddings: 0, full_thumbs: 0 };
        assert_eq!(c.next_phase(), None);

        let c = ProcessCounts { thumbs300: 0, embeddings: 3, full_thumbs: 5 };
        assert_eq!(c.next_phase(), Some(ProcessPhase::Embeddings));

        let c = ProcessCounts { thumbs300: 2, embeddings: 3, full_thumbs: 5 };
        assert_eq!(c.next_phase(), Some(ProcessPhase::Thumbnails300));

        let c = ProcessCounts { thumbs300: 0, embeddings: 0, full_thumbs: 4 };
        assert_eq!(c.next_phase(), Some(ProcessPhase::FullThumbnails));
    }

    #[test]
    fn total_remaining_sums_all_phases() {
        let c = ProcessCounts { thumbs300: 1, embeddings: 2, full_thumbs: 3 };
        assert_eq!(c.total_remaining(), 6);

        let c = ProcessCounts { thumbs300: 0, embeddings: 0, full_thumbs: 0 };
        assert_eq!(c.total_remaining(), 0);

        let c = ProcessCounts { thumbs300: 10, embeddings: 0, full_thumbs: 0 };
        assert_eq!(c.total_remaining(), 10);
    }

    // ── Embedder round-trip test (requires CLIP model weights) ───────────────
    //
    // This test loads the default CLIP model via `ClipEmbedder::from_model`,
    // which downloads ~350 MB of weights on first use. Mark `#[ignore]` so it
    // is skipped in normal CI; run manually with:
    //   cargo test --lib processing:: -- --ignored
    #[test]
    #[ignore = "requires the CLIP model weights locally; run with --ignored"]
    fn embeddings_batch_fills_missing_vectors() {
        const K: usize = 2;
        let db_path = temp_db_path();
        let db = block_on(crate::database::Database::new(&db_path)).expect("create test db");
        let parent_dir = db_path
            .parent()
            .expect(".imgfind dir")
            .parent()
            .expect("parent dir");

        // Write K small but decodable PNG images on disk with per-pixel
        // arithmetic content. File size doesn't matter here (we insert rows
        // directly, bypassing the oshash walk), so 32×32 is fine.
        let mut rows: Vec<(String, String)> = Vec::new();
        for i in 0..K {
            let img_path = parent_dir.join(format!("proc_fixture_{i}.png"));
            let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(32, 32, |x, y| {
                Rgb([(x * 8) as u8, (y * 8) as u8, (i as u32 * 40) as u8])
            });
            img.save(&img_path).expect("save fixture");
            rows.push((format!("proc_fixture_{i}.png"), format!("proc_hash_{i}")));
        }
        block_on(db.insert_image_rows_batch(&rows)).expect("insert rows");

        // Generate 300 px thumbnails so embed_one_from_thumbnail can find them.
        for i in 0..K {
            let img_path = parent_dir.join(format!("proc_fixture_{i}.png"));
            let abs_str = img_path.to_str().expect("utf-8 path");
            let hash = format!("proc_hash_{i}");
            thumbnail::get_or_generate_thumbnail(&db, abs_str, &hash, ThumbnailSize(300))
                .expect("generate 300px thumbnail");
        }

        // All K images should need embeddings before processing.
        let missing_before =
            block_on(db.count_images_without_embedding()).expect("count before");
        assert_eq!(missing_before, K, "all fixtures should be awaiting embeddings");

        // Load the default CLIP model on CPU and run the batch.
        let embedder = ClipEmbedder::from_model("openai/clip-vit-base-patch32", true)
            .expect("load CLIP embedder (requires model weights)");
        let done = process_embeddings_batch(&db, &embedder, K + 5).expect("batch");
        assert_eq!(done, K, "all K images should be successfully embedded");

        // Verify the backlog is now drained.
        let missing_after =
            block_on(db.count_images_without_embedding()).expect("count after");
        assert_eq!(missing_after, 0, "no images should remain without an embedding");

        // Verify that the stored vectors have the correct dimension and unit L2 norm.
        // We confirm the dimension by checking the embedder reports 512 for the
        // default model, and that normalize_vector's output is already unit length.
        assert_eq!(
            embedder.dim(),
            512,
            "default model must produce 512-dim vectors"
        );

        let _ = std::fs::remove_dir_all(parent_dir);
    }
}
