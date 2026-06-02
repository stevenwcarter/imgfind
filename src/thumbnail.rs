use crate::{database::Database, get_db_path};
use anyhow::{Context, Result};
use image::ImageReader;
use rayon::prelude::*;
use rusqlite::params;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc::Sender};
use std::thread;
use std::time::Duration;

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
/// ```rust
/// // Generate thumbnails for up to 10 images that don't have 300px thumbnails
/// use imgfind::database::Database;
/// use imgfind::thumbnail::generate_missing_thumbnails_batch;
/// use std::path::Path;
/// let mut db = Database::new(Path::new("/tmp/.imgfind/imgfind.db")).unwrap();
/// let generated = generate_missing_thumbnails_batch(&mut db, 300, 10).unwrap();
/// println!("Generated {} thumbnails", generated);
/// ```
pub fn generate_missing_thumbnails_batch(
    db: &mut Database,
    size: u32,
    count: usize,
) -> Result<usize> {
    // Enable WAL mode for better concurrent performance
    {
        let conn = db
            .pool
            .get()
            .context("Failed to get connection for WAL setup")?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .context("Failed to enable WAL mode")?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .context("Failed to set synchronous mode")?;
        log::info!("Database configured with WAL mode");
    }

    // Fetch image paths/hashes lacking thumbnails of this size.
    let images_without_thumbnails = db.get_images_without_thumbnails(size, count)?;

    // Channel used by producer tasks (thumbnail generators) to send bytes to a single DB writer.
    let (tx, rx) = std::sync::mpsc::channel::<(String, u32, Vec<u8>)>();

    // Atomic counter shared with writer thread to record successful inserts.
    let generated_count = Arc::new(AtomicUsize::new(0));
    let writer_count = Arc::clone(&generated_count);

    // Open a single database connection in the writer thread to avoid write deadlocks.
    let db_path = get_db_path(None).context("Failed to resolve database path")?;
    let writer_db = match Database::new(&db_path) {
        Ok(db) => db,
        Err(e) => {
            log::error!("Writer thread failed to open database: {:?}", e);
            panic!("Cannot proceed without DB access");
        }
    };
    let writer_handle = thread::spawn(move || {
        // Single pooled connection reused across flushes.
        let mut conn = match writer_db.pool.get() {
            Ok(c) => c,
            Err(e) => {
                log::error!("Writer thread failed to get pooled connection: {:?}", e);
                return;
            }
        };
        // Improve concurrency resilience.
        if let Err(e) = conn.busy_timeout(Duration::from_secs(5)) {
            log::warn!("Failed setting busy timeout: {:?}", e);
        }

        let mut buffer: Vec<(String, u32, Vec<u8>)> = Vec::with_capacity(10);

        // Helper to flush current buffer using a transaction.
        let mut flush = |buf: &mut Vec<(String, u32, Vec<u8>)>| {
            if buf.is_empty() {
                return;
            }
            log::info!("Flushing {} thumbnails to database", buf.len());
            let txn = match conn.transaction() {
                Ok(t) => t,
                Err(e) => {
                    log::error!("Failed to start transaction: {:?}", e);
                    return;
                }
            };
            let mut stmt = match txn.prepare(
                "INSERT OR REPLACE INTO thumbnails (image_hash, size, thumbnail_data) VALUES (?1, ?2, ?3)",
            ) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Failed to prepare insert statement: {:?}", e);
                    return;
                }
            };
            log::info!("Inserting {} thumbnails", buf.len());
            for (hash, size, bytes) in buf.drain(..) {
                log::info!("Inserting thumbnail hash={} size={}", hash, size);
                match stmt.execute(params![&hash, size as i64, &bytes]) {
                    Ok(_) => {
                        writer_count.fetch_add(1, Ordering::SeqCst);
                        log::debug!("Inserted thumbnail hash={} size={}", hash, size);
                    }
                    Err(e) => {
                        log::warn!("Insert failed for hash {} size {}: {:?}", hash, size, e);
                    }
                }
            }
            // Drop statement before commit.
            drop(stmt);
            if let Err(e) = txn.commit() {
                log::error!("Failed to commit thumbnail batch: {:?}", e);
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

// Generate resized JPEG bytes for a thumbnail (pure function aside from file IO)
fn generate_thumbnail_bytes(filepath: &str, size: u32) -> Result<Vec<u8>> {
    let image = ImageReader::open(filepath)
        .with_context(|| format!("Failed to open image: {}", filepath))?
        .decode()
        .with_context(|| format!("Failed to decode image: {}", filepath))?;

    let resized_image = image.resize(size, size, image::imageops::FilterType::Lanczos3);

    let mut bytes: Vec<u8> = Vec::new();
    resized_image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("Failed to encode thumbnail as JPEG")?;
    Ok(bytes)
}

/// Generate thumbnail bytes and send them for insertion via channel.
fn generate_and_store_thumbnail(
    filepath: &str,
    hash: &str,
    size: u32,
    tx: &Sender<(String, u32, Vec<u8>)>,
) -> Result<()> {
    let bytes = generate_thumbnail_bytes(filepath, size)?;
    tx.send((hash.to_string(), size, bytes))
        .context("Failed to send thumbnail bytes over channel")?;
    Ok(())
}

/// Generate or retrieve a thumbnail for an image
///
/// This function first checks if a thumbnail exists in the database cache.
/// If not, it generates a new thumbnail, stores it in the database, and returns it.
///
/// # Arguments
/// * `db` - Database connection
/// * `filepath` - Path to the image file
/// * `hash` - Hash of the image (used as cache key)
/// * `size` - Desired thumbnail size
///
/// # Returns
/// * `Result<Vec<u8>>` - JPEG encoded thumbnail bytes
pub fn get_or_generate_thumbnail(
    db: &Database,
    filepath: &str,
    hash: &str,
    size: u32,
) -> Result<Vec<u8>> {
    // First, try to get the thumbnail from the database
    if let Ok(thumbnail_data) = db.get_thumbnail(hash, size) {
        return Ok(thumbnail_data);
    }

    // If not found, generate bytes and insert directly (synchronous path)
    let bytes = generate_thumbnail_bytes(filepath, size)?;
    db.insert_thumbnail(hash, size, &bytes)
        .context("Failed to store thumbnail in database")?;

    // Return the newly generated thumbnail
    db.get_thumbnail(hash, size)
        .context("Failed to retrieve newly generated thumbnail")
}
