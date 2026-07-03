use crate::{ThumbnailSize, ThumbnailSpec, database::Database, edits::ImageEdits};
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

/// A message from a thumbnail-generation worker to the single DB writer thread.
enum ThumbMsg {
    /// Successfully generated JPEG bytes for `(hash, size)`.
    Ok { hash: String, size: u32, data: Vec<u8> },
    /// Generation failed for `(hash, size)`; record a permanent marker.
    Failed { hash: String, size: u32, error: String },
}

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
    let (tx, rx) = std::sync::mpsc::channel::<ThumbMsg>();

    // Atomic counter shared with writer thread to record successful inserts.
    let generated_count = Arc::new(AtomicUsize::new(0));
    let writer_count = Arc::clone(&generated_count);

    // Open a single database in the writer thread to avoid write deadlocks.
    // Derive the path from the PASSED db's parent_dir so the writer always
    // targets the same library as the reader, regardless of the process cwd.
    // Using get_db_path(None) here was a bug: it resolved via cwd walk-up and
    // could open a completely different database when the caller was launched
    // with --dir pointing elsewhere.
    let db_path = db.parent_dir.join(".imgfind").join("imgfind.db");
    let writer_db =
        block_on(Database::new(&db_path)).context("writer thread failed to open database")?;
    let writer_handle = thread::spawn(move || {
        let mut buffer: Vec<(String, u32, Vec<u8>)> = Vec::with_capacity(10);

        // Flush the current buffer in one batched transaction (via the async
        // `insert_thumbnails_batch`, bridged through `block_on`).
        let flush = |buf: &mut Vec<(String, u32, Vec<u8>)>| {
            if buf.is_empty() {
                return;
            }
            tracing::info!("Flushing {} thumbnails to database", buf.len());
            let batch = std::mem::take(buf);
            let n = batch.len();
            match block_on(writer_db.insert_thumbnails_batch(&batch)) {
                Ok(()) => {
                    writer_count.fetch_add(n, Ordering::SeqCst);
                    tracing::debug!("Inserted {n} thumbnails");
                }
                Err(e) => {
                    tracing::error!("Failed to commit thumbnail batch: {:?}", e);
                }
            }
        };

        for msg in rx {
            match msg {
                ThumbMsg::Ok { hash, size, data } => {
                    tracing::debug!("Writer received hash {}", hash);
                    buffer.push((hash, size, data));
                    if buffer.len() >= 40 {
                        flush(&mut buffer);
                    }
                }
                ThumbMsg::Failed { hash, size, error } => {
                    tracing::debug!("Writer received hash {} (failed)", hash);
                    if let Err(e) = block_on(writer_db.insert_thumbnail_failure(&hash, size, &error))
                    {
                        tracing::error!("Failed to record thumbnail failure for {hash}: {e:#}");
                    }
                }
            }
        }
        // Final flush after channel close.
        flush(&mut buffer);
    });

    // Fetch edits for each image on the coordinating thread before handing off to rayon
    // workers.  The async DB call must not run inside a rayon task.
    let images_with_edits: Vec<(crate::AbsolutePath, String, ImageEdits)> =
        images_without_thumbnails
            .into_iter()
            .map(|(abs_path, hash)| {
                let edits = match abs_path.to_relative(&db.parent_dir) {
                    Ok(rel) => block_on(db.get_image_edits(&rel)).unwrap_or_else(|e| {
                        tracing::warn!("failed to fetch edits for {}: {e:#}", abs_path.as_str());
                        ImageEdits::default()
                    }),
                    Err(_) => {
                        tracing::warn!(
                            "could not compute relative path for {}, using identity edits",
                            abs_path.as_str()
                        );
                        ImageEdits::default()
                    }
                };
                (abs_path, hash, edits)
            })
            .collect();

    // Parallel generation of thumbnails (CPU-bound); each task sends bytes to single writer.
    images_with_edits
        .par_iter()
        .for_each(|(path, hash, edits)| {
            let path_str = path.as_str();
            if let Err(e) = generate_and_store_thumbnail(path_str.as_ref(), hash, size, edits, &tx)
            {
                tracing::warn!("Failed to generate thumbnail for {}: {:?}", path_str, e);
                let _ = tx.send(ThumbMsg::Failed {
                    hash: hash.to_string(),
                    size: size.get(),
                    error: format!("{e:#}"),
                });
            } else {
                tracing::info!("Generated thumbnail for: {}", path_str);
            }
        });

    tracing::info!("All thumbnail generation tasks completed.");
    // Close the sending side so the writer thread can finish once queue is drained.
    drop(tx);
    tracing::info!("Waiting for writer thread to finish...");
    if let Err(e) = writer_join_result(writer_handle.join()) {
        tracing::error!("{e:#}");
        return Err(e);
    }

    Ok(generated_count.load(Ordering::SeqCst))
}

/// Convert a writer-thread join result into a flat error. A panic payload is a
/// `Box<dyn Any + Send>`; extract a `&str`/`String` message when present.
fn writer_join_result(joined: thread::Result<()>) -> Result<()> {
    joined.map_err(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "writer thread panicked".to_string());
        anyhow::anyhow!("thumbnail writer thread panicked: {msg}")
    })
}

// Generate JPEG bytes for a thumbnail rendition (pure aside from file IO).
// Identity edits take the fast path (decode_image/decode_full_image → resize);
// non-identity edits go through the linear pipeline for highlight-preserving
// tonemapping (decode_linear → downscale → render).
fn generate_thumbnail_bytes(
    filepath: &str,
    spec: ThumbnailSpec,
    edits: &ImageEdits,
) -> Result<Vec<u8>> {
    let path = std::path::Path::new(filepath);
    let out_image: image::DynamicImage = if edits.is_identity() {
        // Fast path (unchanged): no demosaic, no tonemap.
        match spec {
            ThumbnailSpec::ScaleSize(size) => {
                let img = crate::decode::decode_image(path)
                    .with_context(|| format!("Failed to decode image: {filepath}"))?;
                let px = size.get();
                img.resize(px, px, image::imageops::FilterType::Lanczos3)
            }
            ThumbnailSpec::FullSize => crate::decode::decode_full_image(path)
                .with_context(|| format!("Failed to decode full image: {filepath}"))?,
        }
    } else {
        // High-fidelity path: linear decode -> downscale in linear -> tonemap.
        let linear = crate::decode::decode_linear(path)
            .with_context(|| format!("Failed to decode (linear) image: {filepath}"))?;
        let sized = match spec {
            ThumbnailSpec::ScaleSize(size) => linear.downscale(size.get()),
            ThumbnailSpec::FullSize => linear,
        };
        image::DynamicImage::ImageRgb8(sized.render(edits))
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
    edits: &ImageEdits,
    tx: &Sender<ThumbMsg>,
) -> Result<()> {
    let bytes = generate_thumbnail_bytes(filepath, ThumbnailSpec::ScaleSize(size), edits)?;
    tx.send(ThumbMsg::Ok {
        hash: hash.to_string(),
        size: size.get(),
        data: bytes,
    })
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

    // Miss: fetch edits (identity is the fast common case), generate, persist, return.
    let abs = crate::AbsolutePath(std::path::PathBuf::from(filepath));
    let edits = match abs.to_relative(&db.parent_dir) {
        Ok(rel) => block_on(db.get_image_edits(&rel)).context("fetch image edits for thumbnail")?,
        Err(_) => {
            tracing::warn!("could not compute relative path for {filepath}, using identity edits");
            ImageEdits::default()
        }
    };
    let bytes = match generate_thumbnail_bytes(filepath, spec, &edits) {
        Ok(b) => b,
        Err(e) => {
            if let ThumbnailSpec::ScaleSize(size) = spec {
                if let Err(rec) =
                    block_on(db.insert_thumbnail_failure(hash, size.get(), &format!("{e:#}")))
                {
                    tracing::error!("Failed to record thumbnail failure for {hash}: {rec:#}");
                }
            }
            return Err(e);
        }
    };
    block_on(db.insert_thumbnail(hash, spec, &bytes))
        .context("Failed to store thumbnail in database")?;
    block_on(db.get_thumbnail(hash, spec)).context("Failed to retrieve newly generated thumbnail")
}

/// Regenerate all cached thumbnail sizes for a single image with new edits applied.
///
/// For each size currently stored under `hash`, decodes the original file at
/// `abs_path`, applies `edits`, and overwrites the blob via `insert_thumbnail`.
/// Returns the count of sizes regenerated.
///
/// The caller is responsible for ensuring `abs_path` still points to the
/// original file (edits are non-destructive; originals are never modified).
pub fn regenerate_thumbnails_for_image(
    db: &Database,
    abs_path: &str,
    hash: &str,
    edits: &ImageEdits,
) -> anyhow::Result<usize> {
    let sizes = block_on(db.get_thumbnail_sizes(hash))?;
    let mut count = 0;
    for size in sizes {
        let spec = if size == 0 {
            ThumbnailSpec::FullSize
        } else {
            ThumbnailSpec::ScaleSize(ThumbnailSize(size))
        };
        let bytes = generate_thumbnail_bytes(abs_path, spec, edits).with_context(|| {
            format!("failed to regenerate thumbnail (spec={spec:?}) for {abs_path}")
        })?;
        block_on(db.insert_thumbnail(hash, spec, &bytes))
            .with_context(|| format!("failed to store regenerated thumbnail (spec={spec:?})"))?;
        count += 1;
    }
    Ok(count)
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
    fn writer_join_result_propagates_panic_message() {
        let handle = thread::spawn(|| panic!("boom"));
        let err = writer_join_result(handle.join()).expect_err("panic must surface as Err");
        assert!(format!("{err:#}").contains("boom"));
    }

    #[test]
    fn writer_join_result_ok_on_normal_return() {
        let handle = thread::spawn(|| {});
        assert!(writer_join_result(handle.join()).is_ok());
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

    #[test]
    fn identity_thumbnail_matches_plain_decode() {
        // Identity edits must take the fast path: bytes equal a direct decode+resize+encode.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.png");
        image::RgbImage::from_pixel(80, 80, image::Rgb([90, 90, 90]))
            .save(&path)
            .unwrap();
        let p = path.to_str().unwrap();

        let via_seam = generate_thumbnail_bytes(
            p,
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &ImageEdits::identity(),
        )
        .unwrap();

        // Reference: the exact fast-path operations.
        let img = crate::decode::decode_image(std::path::Path::new(p)).unwrap();
        let resized = img.resize(64, 64, image::imageops::FilterType::Lanczos3);
        let mut want = Vec::new();
        resized
            .write_to(
                &mut std::io::Cursor::new(&mut want),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        assert_eq!(
            via_seam, want,
            "identity must be byte-identical to the fast path"
        );
    }

    #[test]
    fn highlight_edit_preserves_more_than_hard_clamp() {
        // A bright image pushed +2 EV through the linear path must NOT be a single
        // flat 255 block — decode the regenerated JPEG and assert it isn't all 255.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bright.png");
        // A gradient near white so a hard clamp would flatten it.
        let mut img = image::RgbImage::new(64, 1);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            let v = 200 + (x * 55 / 63) as u8; // 200..=255
            *px = image::Rgb([v, v, v]);
        }
        img.save(&path).unwrap();
        let bytes = generate_thumbnail_bytes(
            path.to_str().unwrap(),
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &ImageEdits {
                exposure: 2.0,
                ..ImageEdits::identity()
            },
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        let all_white = decoded.pixels().all(|p| p[0] == 255);
        assert!(!all_white, "highlights flattened to pure white (blowout)");
    }

    #[test]
    fn edited_thumbnail_differs_from_unedited() {
        // Build a small temp image file, generate at 64px with identity vs +2 EV.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.png");
        image::RgbImage::from_pixel(80, 80, image::Rgb([100, 100, 100]))
            .save(&path)
            .unwrap();
        let p = path.to_str().unwrap();
        let plain = generate_thumbnail_bytes(
            p,
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &ImageEdits::identity(),
        )
        .unwrap();
        let bright = generate_thumbnail_bytes(
            p,
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &ImageEdits {
                exposure: 2.0,
                ..ImageEdits::identity()
            },
        )
        .unwrap();
        assert_ne!(
            plain, bright,
            "exposure edit must change generated thumbnail bytes"
        );

        // And identity equals a second identity render (determinism + true no-op).
        let plain2 = generate_thumbnail_bytes(
            p,
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &ImageEdits::identity(),
        )
        .unwrap();
        assert_eq!(plain, plain2);
    }

    /// `regenerate_thumbnails_for_image` overwrites every cached size with the
    /// new edits baked in — the stored bytes change for a non-identity edit.
    #[test]
    fn regenerate_overwrites_existing_sizes_with_edits() {
        use crate::{ThumbnailSpec, edits::ImageEdits};

        let db_path = temp_db_path();
        let db = block_on(Database::new(&db_path)).expect("create test db");
        let parent_dir = db_path.parent().unwrap().parent().unwrap();

        // A small but real decodable image (80×80 mid-grey).
        let img_path = parent_dir.join("regen_fixture.png");
        image::RgbImage::from_pixel(80, 80, image::Rgb([120, 120, 120]))
            .save(&img_path)
            .expect("save fixture");
        let abs_path = img_path.to_str().unwrap();
        let hash = "regen_test_hash";

        // Seed identity thumbnails at 64px and FullSize (size=0) so
        // get_thumbnail_sizes returns both.
        let identity_bytes_64 = generate_thumbnail_bytes(
            abs_path,
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &ImageEdits::identity(),
        )
        .expect("identity 64");
        let identity_bytes_full =
            generate_thumbnail_bytes(abs_path, ThumbnailSpec::FullSize, &ImageEdits::identity())
                .expect("identity full");

        block_on(db.insert_thumbnail(
            hash,
            ThumbnailSpec::ScaleSize(ThumbnailSize(64)),
            &identity_bytes_64,
        ))
        .expect("seed 64");
        block_on(db.insert_thumbnail(hash, ThumbnailSpec::FullSize, &identity_bytes_full))
            .expect("seed full");

        // Capture what is currently stored for both seeded sizes.
        let before_64 =
            block_on(db.get_thumbnail(hash, ThumbnailSpec::ScaleSize(ThumbnailSize(64))))
                .expect("before 64");
        let before_full =
            block_on(db.get_thumbnail(hash, ThumbnailSpec::FullSize)).expect("before full");

        // Regenerate with +2 EV — both sizes must change.
        let n = super::regenerate_thumbnails_for_image(
            &db,
            abs_path,
            hash,
            &ImageEdits {
                exposure: 2.0,
                ..ImageEdits::identity()
            },
        )
        .expect("regenerate");

        assert_eq!(n, 2, "both seeded sizes should be regenerated");

        let after_64 =
            block_on(db.get_thumbnail(hash, ThumbnailSpec::ScaleSize(ThumbnailSize(64))))
                .expect("after 64");
        assert_ne!(
            before_64, after_64,
            "64px thumbnail bytes must change after +2 EV edit"
        );

        let after_full =
            block_on(db.get_thumbnail(hash, ThumbnailSpec::FullSize)).expect("after full");
        assert_ne!(
            before_full, after_full,
            "FullSize thumbnail bytes must change after +2 EV edit"
        );

        let _ = std::fs::remove_dir_all(parent_dir);
    }

    /// `get_or_generate_thumbnail` records a failure marker for a `ScaleSize`
    /// request on an undecodable file, but does NOT record one for `FullSize`
    /// (only `ScaleSize` failures are recorded — see `generate_thumbnail_bytes`
    /// call site in `get_or_generate_thumbnail`).
    #[test]
    fn get_or_generate_records_failure_only_for_scale_size() {
        let db_path = temp_db_path();
        let db = block_on(Database::new(&db_path)).expect("create test db");
        let parent_dir = db_path.parent().unwrap().parent().unwrap();

        // A garbage, undecodable "image" file.
        let bad_path = parent_dir.join("garbage.png");
        std::fs::write(&bad_path, b"not an image").expect("write garbage file");
        let bad_path_str = bad_path.to_str().unwrap();

        // ScaleSize failure IS recorded.
        let err = get_or_generate_thumbnail(&db, bad_path_str, "H", ThumbnailSize(300));
        assert!(err.is_err(), "undecodable file must fail to thumbnail");
        assert_eq!(
            block_on(db.thumbnail_failure_count("H")).unwrap(),
            1,
            "ScaleSize failure must be recorded"
        );

        // FullSize failure is NOT recorded.
        let err = get_or_generate_thumbnail(&db, bad_path_str, "H2", ThumbnailSpec::FullSize);
        assert!(err.is_err(), "undecodable file must fail to thumbnail");
        assert_eq!(
            block_on(db.thumbnail_failure_count("H2")).unwrap(),
            0,
            "FullSize failure must NOT be recorded"
        );

        let _ = std::fs::remove_dir_all(parent_dir);
    }
}
