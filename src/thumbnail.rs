use crate::{ThumbnailSize, ThumbnailSpec, database::Database, get_db_path};
use anyhow::{Context, Result};

/// Long-edge target for the GUI lightbox/preview cached render.
pub const LIGHTBOX_SIZE: ThumbnailSize = ThumbnailSize(2048);

/// Thumbnail sizes the GUI requests: grid (300 px), detail panel (512 px),
/// and lightbox/preview (2048 px). Pre-generating all three avoids decode
/// latency on first view at each size.
pub const GUI_THUMBNAIL_SIZES: [ThumbnailSize; 3] =
    [ThumbnailSize(300), ThumbnailSize(512), LIGHTBOX_SIZE];
use crate::block_on;
use rayon::prelude::*;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc::Sender};
use std::thread;

/// Generate thumbnails in batches for images that don't have cached thumbnails
///
/// This function finds images that don't have a thumbnail entry of the given size
/// and generates thumbnails for the first `count` number of them.
///
/// # Arguments
/// * `db` - Database connection
/// * `size` - Desired thumbnail size
/// * `count` - Maximum number of thumbnails to generate in this batch
///
/// # Returns
/// * `Result<usize>` - Number of thumbnails actually generated
///
/// # Example
/// ```no_run
/// // Generate thumbnails for up to 10 images that don't have 300px thumbnails
/// use imgfind::ThumbnailSize;
/// use imgfind::block_on;
/// use imgfind::database::Database;
/// use imgfind::thumbnail::generate_missing_thumbnails_batch;
/// use std::path::Path;
/// let mut db = block_on(Database::new(Path::new("/tmp/.imgfind/imgfind.db"))).unwrap();
/// let generated = generate_missing_thumbnails_batch(&mut db, ThumbnailSize(300), 10).unwrap();
/// println!("Generated {} thumbnails", generated);
/// ```
pub fn generate_missing_thumbnails_batch(
    db: &mut Database,
    size: ThumbnailSize,
    count: usize,
) -> Result<usize> {
    // Fetch image paths/hashes lacking thumbnails of this size.
    let images_without_thumbnails = block_on(db.get_images_without_thumbnails(size, count))?;

    // Channel used by producer tasks (thumbnail generators) to send bytes to a single DB writer.
    let (tx, rx) = std::sync::mpsc::channel::<(String, u32, Vec<u8>)>();

    // Atomic counter shared with writer thread to record successful inserts.
    let generated_count = Arc::new(AtomicUsize::new(0));
    let writer_count = Arc::clone(&generated_count);

    // Open a single database in the writer thread to avoid write deadlocks.
    let db_path = get_db_path(None).context("Failed to resolve database path")?;
    let writer_db = match block_on(Database::new(&db_path)) {
        Ok(db) => db,
        Err(e) => {
            log::error!("Writer thread failed to open database: {:?}", e);
            panic!("Cannot proceed without DB access");
        }
    };
    let writer_handle = thread::spawn(move || {
        let mut buffer: Vec<(String, u32, Vec<u8>)> = Vec::with_capacity(10);

        // Flush the current buffer in one batched transaction (via the async
        // `insert_thumbnails_batch`, bridged through `block_on`).
        let flush = |buf: &mut Vec<(String, u32, Vec<u8>)>| {
            if buf.is_empty() {
                return;
            }
            log::info!("Flushing {} thumbnails to database", buf.len());
            let batch = std::mem::take(buf);
            let n = batch.len();
            match block_on(writer_db.insert_thumbnails_batch(&batch)) {
                Ok(()) => {
                    writer_count.fetch_add(n, Ordering::SeqCst);
                    log::debug!("Inserted {n} thumbnails");
                }
                Err(e) => {
                    log::error!("Failed to commit thumbnail batch: {:?}", e);
                }
            }
        };

        for item in rx {
            log::debug!("Writer received hash {}", item.0);
            buffer.push(item);
            if buffer.len() >= 40 {
                flush(&mut buffer);
            }
        }
        // Final flush after channel close.
        flush(&mut buffer);
    });

    // Parallel generation of thumbnails (CPU-bound); each task sends bytes to single writer.
    images_without_thumbnails
        .par_iter()
        .for_each(|(path, hash)| {
            let path = path.as_str();
            if let Err(e) = generate_and_store_thumbnail(path.as_ref(), hash, size, &tx) {
                log::warn!("Failed to generate thumbnail for {}: {:?}", path, e);
            } else {
                log::info!("Generated thumbnail for: {}", path);
            }
        });

    log::info!("All thumbnail generation tasks completed.");
    // Close the sending side so the writer thread can finish once queue is drained.
    drop(tx);
    log::info!("Waiting for writer thread to finish...");
    if let Err(e) = writer_handle.join() {
        log::error!("Writer thread panicked: {:?}", e);
    }

    Ok(generated_count.load(Ordering::SeqCst))
}

// Generate JPEG bytes for a thumbnail rendition (pure aside from file IO).
// `ScaleSize` downscales via the fast decode path; `FullSize` uses the
// RAW-aware full-resolution decode and encodes at native dimensions.
fn generate_thumbnail_bytes(filepath: &str, spec: ThumbnailSpec) -> Result<Vec<u8>> {
    let path = std::path::Path::new(filepath);
    let out_image = match spec {
        ThumbnailSpec::ScaleSize(size) => {
            let image = crate::decode::decode_image(path)
                .with_context(|| format!("Failed to decode image: {}", filepath))?;
            let px = size.get();
            image.resize(px, px, image::imageops::FilterType::Lanczos3)
        }
        ThumbnailSpec::FullSize => crate::decode::decode_full_image(path)
            .with_context(|| format!("Failed to decode full image: {}", filepath))?,
    };

    let mut bytes: Vec<u8> = Vec::new();
    out_image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("Failed to encode thumbnail as JPEG")?;
    Ok(bytes)
}

/// Generate thumbnail bytes and send them for insertion via channel.
fn generate_and_store_thumbnail(
    filepath: &str,
    hash: &str,
    size: ThumbnailSize,
    tx: &Sender<(String, u32, Vec<u8>)>,
) -> Result<()> {
    let bytes = generate_thumbnail_bytes(filepath, ThumbnailSpec::ScaleSize(size))?;
    tx.send((hash.to_string(), size.get(), bytes))
        .context("Failed to send thumbnail bytes over channel")?;
    Ok(())
}

/// Generate or retrieve a thumbnail for an image.
///
/// Checks the database cache first. On a miss, generates the rendition
/// (scaled or full-resolution depending on `spec`), persists it, and returns it.
///
/// # Arguments
/// * `db` - Database connection
/// * `filepath` - Path to the image file
/// * `hash` - Hash of the image (used as cache key)
/// * `spec` - `ThumbnailSize` for a scaled rendition, or `ThumbnailSpec::FullSize`
///
/// # Returns
/// * `Result<Vec<u8>>` - JPEG encoded thumbnail bytes
pub fn get_or_generate_thumbnail(
    db: &Database,
    filepath: &str,
    hash: &str,
    spec: impl Into<ThumbnailSpec>,
) -> Result<Vec<u8>> {
    let spec = spec.into();
    // First, try the database cache.
    if let Ok(thumbnail_data) = block_on(db.get_thumbnail(hash, spec)) {
        return Ok(thumbnail_data);
    }

    // Miss: generate, persist, return.
    let bytes = generate_thumbnail_bytes(filepath, spec)?;
    block_on(db.insert_thumbnail(hash, spec, &bytes))
        .context("Failed to store thumbnail in database")?;
    block_on(db.get_thumbnail(hash, spec)).context("Failed to retrieve newly generated thumbnail")
}

/// Abstraction over "how many thumbnails are missing" and "generate one batch",
/// so the loop control in `run_until_complete` can be tested with a fake.
pub trait ThumbnailBatcher {
    fn remaining(&mut self) -> Result<usize>;
    fn generate_batch(&mut self) -> Result<usize>;
}

/// Drive batched generation to completion. Stops when nothing remains, OR when a
/// batch makes zero forward progress (guards against permanently-undecodable
/// images so the loop can never run forever). Returns the total generated.
pub fn run_until_complete(b: &mut impl ThumbnailBatcher) -> Result<usize> {
    let mut total = 0usize;
    loop {
        if b.remaining()? == 0 {
            break;
        }
        let generated = b.generate_batch()?;
        total += generated;
        if generated == 0 {
            break;
        }
    }
    Ok(total)
}

/// Real `ThumbnailBatcher` over a database + target size + per-batch count.
struct DbThumbnailBatcher<'a> {
    db: &'a mut Database,
    size: ThumbnailSize,
    batch: usize,
}
impl ThumbnailBatcher for DbThumbnailBatcher<'_> {
    fn remaining(&mut self) -> Result<usize> {
        block_on(self.db.count_images_without_thumbnails(self.size))
    }
    fn generate_batch(&mut self) -> Result<usize> {
        generate_missing_thumbnails_batch(self.db, self.size, self.batch)
    }
}

/// Generate *every* missing thumbnail of `size`, in batches of `batch`.
pub fn generate_all_missing_thumbnails(
    db: &mut Database,
    size: ThumbnailSize,
    batch: usize,
) -> Result<usize> {
    let mut b = DbThumbnailBatcher { db, size, batch };
    run_until_complete(&mut b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Scripted fake batcher: `remaining` and `generated` are popped front-to-back;
    /// when a sequence is exhausted its last value repeats.
    struct FakeBatcher {
        remaining: std::cell::RefCell<Vec<usize>>,
        generated: std::cell::RefCell<Vec<usize>>,
    }
    impl FakeBatcher {
        fn pop(seq: &std::cell::RefCell<Vec<usize>>) -> usize {
            let mut v = seq.borrow_mut();
            if v.len() > 1 { v.remove(0) } else { v[0] }
        }
    }
    impl ThumbnailBatcher for FakeBatcher {
        fn remaining(&mut self) -> Result<usize> {
            Ok(Self::pop(&self.remaining))
        }
        fn generate_batch(&mut self) -> Result<usize> {
            Ok(Self::pop(&self.generated))
        }
    }

    #[test]
    fn run_until_complete_stops_when_none_remain() {
        let mut b = FakeBatcher {
            remaining: std::cell::RefCell::new(vec![5, 0]),
            generated: std::cell::RefCell::new(vec![5]),
        };
        assert_eq!(run_until_complete(&mut b).unwrap(), 5);
    }

    #[test]
    fn run_until_complete_stops_on_zero_progress() {
        // 2 images remain forever (undecodable); each batch generates 0.
        let mut b = FakeBatcher {
            remaining: std::cell::RefCell::new(vec![2]),
            generated: std::cell::RefCell::new(vec![0]),
        };
        // Must terminate (not hang) and report zero generated.
        assert_eq!(run_until_complete(&mut b).unwrap(), 0);
    }

    #[test]
    fn run_until_complete_sums_then_stops_on_zero_progress() {
        let mut b = FakeBatcher {
            remaining: std::cell::RefCell::new(vec![4, 2, 2]),
            generated: std::cell::RefCell::new(vec![2, 0]),
        };
        assert_eq!(run_until_complete(&mut b).unwrap(), 2);
    }

    /// Returns a unique `<tmpdir>/.imgfind/imgfind.db` path per test invocation.
    fn temp_db_path() -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("imgfind_thumb_test_{}_{n}", std::process::id()));
        dir.join(".imgfind").join("imgfind.db")
    }

    #[test]
    fn gui_sizes_are_300_512_2048() {
        assert_eq!(
            GUI_THUMBNAIL_SIZES,
            [ThumbnailSize(300), ThumbnailSize(512), ThumbnailSize(2048)]
        );
        assert_eq!(LIGHTBOX_SIZE, ThumbnailSize(2048));
    }

    /// Asserts that `get_or_generate_thumbnail` persists a thumbnail row for
    /// each of the three GUI sizes: the row must be absent before the call and
    /// present (non-empty bytes) after it.
    #[test]
    fn get_or_generate_persists_each_gui_size() {
        let db_path = temp_db_path();
        let db = block_on(Database::new(&db_path)).expect("create test db");

        // Parent dir (the directory that contains `.imgfind/`) is the base for
        // relative image paths stored in the database.
        let parent_dir = db_path
            .parent()
            .expect(".imgfind dir")
            .parent()
            .expect("parent dir");

        // Write a real, decodable 8×8 RGB PNG.  RGB8 (not RGBA8) because
        // `generate_thumbnail_bytes` re-encodes as JPEG which has no alpha channel.
        let img_path = parent_dir.join("test_fixture.png");
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(8, 8, Rgb([255, 0, 128]));
        img.save(&img_path).expect("save test fixture image");

        let abs_path = img_path.to_str().expect("utf-8 path");
        let hash = "test_persistence_hash";

        for size in GUI_THUMBNAIL_SIZES {
            // Before: no thumbnail row exists for this (hash, size) pair.
            assert!(
                block_on(db.get_thumbnail(hash, size)).is_err(),
                "size {size:?} should be absent before get_or_generate_thumbnail"
            );

            // Generate (and persist) the thumbnail.
            let bytes =
                get_or_generate_thumbnail(&db, abs_path, hash, size).expect("generate thumbnail");
            assert!(
                !bytes.is_empty(),
                "returned bytes must be non-empty for size {size:?}"
            );

            // After: the row must be present and non-empty.
            let cached = block_on(db.get_thumbnail(hash, size))
                .expect("thumbnail must be present after get_or_generate");
            assert!(
                !cached.is_empty(),
                "persisted bytes must be non-empty for size {size:?}"
            );
        }

        let _ = std::fs::remove_dir_all(parent_dir);
    }

    /// `get_or_generate_thumbnail(FullSize)` persists a row under size=0 and the
    /// returned bytes decode to the original (un-downscaled) dimensions.
    #[test]
    fn get_or_generate_full_size_persists_size_zero() {
        use crate::ThumbnailSpec;
        let db_path = temp_db_path();
        let db = block_on(Database::new(&db_path)).expect("create test db");
        let parent_dir = db_path.parent().unwrap().parent().unwrap();

        // 64×40 so the original is larger than a tiny thumbnail and clearly "full".
        let img_path = parent_dir.join("full_fixture.png");
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(64, 40, Rgb([10, 20, 30]));
        img.save(&img_path).unwrap();
        let abs_path = img_path.to_str().unwrap();
        let hash = "full_size_hash";

        assert!(block_on(db.get_thumbnail(hash, ThumbnailSpec::FullSize)).is_err());

        let bytes =
            get_or_generate_thumbnail(&db, abs_path, hash, ThumbnailSpec::FullSize).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (64, 40),
            "FullSize must preserve original dimensions (no downscale)"
        );

        // Persisted under size=0 and retrievable as FullSize.
        assert!(block_on(db.get_thumbnail(hash, ThumbnailSpec::FullSize)).is_ok());
        let _ = std::fs::remove_dir_all(parent_dir);
    }
}
