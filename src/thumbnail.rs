use crate::{database::Database, get_db_path};
use anyhow::{Context, Result};
use image::ImageReader;
use rayon::prelude::*;
use std::{io::Cursor, sync::atomic::AtomicUsize};

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
/// let generated = generate_missing_thumbnails_batch(&mut db, 300, 10)?;
/// println!("Generated {} thumbnails", generated);
/// ```
pub fn generate_missing_thumbnails_batch(
    db: &mut Database,
    size: u32,
    count: usize,
) -> Result<usize> {
    // Get images that don't have thumbnails of the specified size
    let images_without_thumbnails = db.get_images_without_thumbnails(size, count)?;

    let generated_count: AtomicUsize = AtomicUsize::new(0);

    let db_path = get_db_path().unwrap();
    let db = Database::new(&db_path).unwrap();

    images_without_thumbnails
        .par_iter()
        .for_each(|(path, hash)| {
            match generate_and_store_thumbnail(&mut db.clone(), path, hash, size) {
                Ok(_) => {
                    generated_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    log::info!("Generated thumbnail for: {}", path);
                }
                Err(e) => {
                    log::warn!("Failed to generate thumbnail for {}: {:?}", path, e);
                    // Continue with the next image instead of failing the entire batch
                }
            }
        });

    Ok(generated_count.into_inner())
}

/// Generate and store a single thumbnail
fn generate_and_store_thumbnail(
    db: &mut Database,
    filepath: &str,
    hash: &str,
    size: u32,
) -> Result<()> {
    let image = ImageReader::open(filepath)
        .with_context(|| format!("Failed to open image: {}", filepath))?
        .decode()
        .with_context(|| format!("Failed to decode image: {}", filepath))?;

    let resized_image = image.resize(size, size, image::imageops::FilterType::Lanczos3);

    let mut bytes: Vec<u8> = Vec::new();
    resized_image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("Failed to encode thumbnail as JPEG")?;

    // Store the thumbnail in the database
    db.insert_thumbnail(hash, size, &bytes)
        .context("Failed to store thumbnail in database")?;

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
    db: &mut Database,
    filepath: &str,
    hash: &str,
    size: u32,
) -> Result<Vec<u8>> {
    // First, try to get the thumbnail from the database
    if let Ok(thumbnail_data) = db.get_thumbnail(hash, size) {
        return Ok(thumbnail_data);
    }

    // If not found, generate and store the thumbnail
    generate_and_store_thumbnail(db, filepath, hash, size)?;

    // Return the newly generated thumbnail
    db.get_thumbnail(hash, size)
        .context("Failed to retrieve newly generated thumbnail")
}
