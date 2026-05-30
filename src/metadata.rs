use crate::database::{Database, extract_image_metadata};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use tracing::{debug, info, warn};

pub fn extract_missing_metadata(db: &mut Database, quiet: bool, count: usize) -> Result<()> {
    let images_without_metadata = db.get_images_without_metadata(count)?;

    if !images_without_metadata.is_empty() {
        if !quiet {
            println!(
                "Found {} images missing metadata, extracting...",
                images_without_metadata.len()
            );
        }
        info!(
            "Found {} images missing metadata",
            images_without_metadata.len()
        );

        let metadata_progress = if quiet {
            ProgressBar::hidden()
        } else {
            let pb = ProgressBar::new(images_without_metadata.len() as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) Extracting metadata: {msg}")
                    .unwrap()
                    .progress_chars("#>-"),
            );
            pb
        };

        let mut metadata_extracted = 0;
        let mut failed = 0;
        for (image_id, image_path, _hash) in images_without_metadata {
            if !quiet {
                metadata_progress.set_message(format!(
                    "{}",
                    std::path::Path::new(&image_path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }

            match extract_image_metadata(&image_path) {
                Ok(metadata) => {
                    if let Err(e) = db.insert_or_update_metadata(image_id, &metadata) {
                        warn!(
                            "Failed to store backfilled metadata for {}: {}",
                            image_path, e
                        );
                        failed += 1;
                    } else {
                        metadata_extracted += 1;
                        debug!("Backfilled metadata for: {}", image_path);
                    }
                }
                Err(e) => {
                    debug!(
                        "Failed to extract backfill metadata for {}: {}",
                        image_path, e
                    );
                    failed += 1;
                }
            }
            metadata_progress.inc(1);
        }

        metadata_progress.finish_with_message("Metadata extraction complete!");

        if !quiet {
            println!("  📊 Metadata extracted: {}", metadata_extracted);
            if failed > 0 {
                println!("  ⚠️  Failed: {}", failed);
            }
        }
        if failed > 0 {
            warn!("{} images failed metadata extraction or storage", failed);
        }
        info!(
            "Metadata backfill complete: {} extracted, {} failed",
            metadata_extracted, failed
        );
    }

    Ok(())
}
