use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use clipper::ClipEmbedder;
use dirs::home_dir;
use imgfind::context::GraphQLContext;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, info, warn};
use oshash::oshash;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

use imgfind::database::Database;
use imgfind::routes::app;
use imgfind::search::SearchEngine;

#[derive(Parser)]
#[command(name = "imgfind")]
#[command(about = "CLIP-based image search CLI")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Index images in a directory
    Index {
        /// Directory to index (default: current directory)
        #[arg(short, long, default_value = ".")]
        dir: String,
        /// Recursive indexing
        #[arg(short, long, default_value_t = true)]
        recursive: bool,
        /// Quiet mode: suppress progress output
        #[arg(short, long)]
        quiet: bool,
    },
    /// Search for images using natural language
    Search {
        /// Search query
        prompt: String,
        /// Number of results to return
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
        /// Output only image paths, one per line (useful for piping to other tools)
        #[arg(short, long)]
        short: bool,
        /// Search recursively in subdirectories
        #[arg(short, long)]
        recursive: bool,
    },
    /// Clean up missing files from database
    Clean,
    /// Show database status and statistics
    Status,
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let db_path = get_db_path()?;
    let mut db = Database::new(&db_path)?;

    match cli.command {
        Commands::Index {
            dir,
            recursive,
            quiet,
        } => {
            index_directory(&mut db, &dir, recursive, quiet)?;
        }
        Commands::Search {
            prompt,
            limit,
            short,
            recursive,
        } => {
            search_images(&db, &prompt, limit, short, recursive)?;
        }
        Commands::Clean => {
            clean_database(&mut db)?;
        }
        Commands::Status => {
            show_status(&db, &db_path)?;
        }
        Commands::Serve => {
            serve(db).await?;
        }
    }

    Ok(())
}

async fn serve(db: Database) -> Result<()> {
    // Placeholder for future server implementation
    println!("Server functionality is not yet implemented.");
    let port = 6060;
    let context = GraphQLContext::new(db);
    let app = app(context.clone());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap();
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    });

    server.await.expect("Server failed to start");

    Ok(())
}

fn get_db_path() -> Result<PathBuf> {
    // First, try to find existing database by walking up directory tree
    let mut current_dir = std::env::current_dir()?;

    loop {
        let potential_db = current_dir.join(".imgfind").join("imgfind.db");
        if potential_db.exists() {
            return Ok(potential_db);
        }

        if let Some(parent) = current_dir.parent() {
            current_dir = parent.to_path_buf();
        } else {
            break;
        }
    }

    // Default to ~/.imgfind/imgfind.db
    let home = home_dir().context("Could not find home directory")?;
    let imgfind_dir = home.join(".imgfind");
    fs::create_dir_all(&imgfind_dir)?;
    Ok(imgfind_dir.join("imgfind.db"))
}

fn index_directory(db: &mut Database, dir: &str, recursive: bool, quiet: bool) -> Result<()> {
    if !quiet {
        println!("Indexing directory: {}", dir);
    }
    info!("Indexing directory: {}", dir);

    // Check if directory exists
    let dir_path = std::path::Path::new(dir);
    if !dir_path.exists() {
        return Err(anyhow::anyhow!("Directory does not exist: {}", dir));
    }
    if !dir_path.is_dir() {
        return Err(anyhow::anyhow!("Path is not a directory: {}", dir));
    }

    if !quiet {
        println!("Loading CLIP model...");
    }
    info!("Loading CLIP model...");
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;
    info!("CLIP model loaded successfully");

    let image_extensions: HashSet<&str> = ["jpg", "jpeg", "png", "gif", "bmp", "tiff", "webp"]
        .iter()
        .cloned()
        .collect();

    // First pass: collect all image files
    if !quiet {
        println!("Scanning for image files...");
    }
    info!("Scanning for image files...");

    let walker = if recursive {
        WalkDir::new(dir).into_iter()
    } else {
        WalkDir::new(dir).max_depth(1).into_iter()
    };

    let mut image_files = Vec::new();
    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        // Check if it's an image file
        if let Some(extension) = path.extension() {
            if let Some(ext_str) = extension.to_str() {
                if image_extensions.contains(ext_str.to_lowercase().as_str()) {
                    image_files.push(path.to_path_buf());
                }
            }
        }
    }

    if image_files.is_empty() {
        if !quiet {
            println!("No image files found in the specified directory.");
        }
        return Ok(());
    }

    if !quiet {
        println!("Found {} image files", image_files.len());
        println!("Processing images...");
    }
    info!("Found {} image files to process", image_files.len());

    // Create progress bar
    let progress_bar = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(image_files.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) {msg}")
                .unwrap()
                .progress_chars("#>-"),
        );
        pb
    };

    let mut indexed_count = 0;
    let mut skipped_count = 0;
    let mut error_count = 0;

    info!("Starting to process images...");

    for path in image_files.iter() {
        // Convert to absolute path for storage consistency
        let abs_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(), // fallback to original path if canonicalize fails
        };
        let path_str = abs_path.to_string_lossy();

        if !quiet {
            progress_bar.set_message(format!(
                "Processing: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }

        // Calculate hash
        let hash = match oshash(&path_str) {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to calculate hash for {}: {}", path_str, e);
                error_count += 1;
                progress_bar.inc(1);
                continue;
            }
        };

        // Check if already indexed with same hash
        if db.is_image_indexed(&path_str, &hash)? {
            debug!("Skipping already indexed: {}", path_str);
            skipped_count += 1;
            progress_bar.inc(1);
            continue;
        }

        // Generate embedding
        debug!("Processing: {}", path_str);
        let embedding = match model.get_image_embedding(&path_str) {
            Ok(emb) => emb,
            Err(e) => {
                warn!("Failed to generate embedding for {}: {}", path_str, e);
                error_count += 1;
                progress_bar.inc(1);
                continue;
            }
        };

        // Normalize embedding
        let normalized_embedding = normalize_vector(&embedding);

        // Store in database
        if let Err(e) = db.insert_image(&path_str, &hash, &normalized_embedding) {
            warn!("Failed to insert image into database {}: {}", path_str, e);
            error_count += 1;
            progress_bar.inc(1);
            continue;
        }

        indexed_count += 1;
        progress_bar.inc(1);
    }

    progress_bar.finish_with_message("Indexing complete!");

    if !quiet {
        println!("\nIndexing Summary:");
        println!("  📁 Total files found: {}", image_files.len());
        println!("  ✅ Newly indexed: {}", indexed_count);
        println!("  ⏭️  Already indexed (skipped): {}", skipped_count);
        if error_count > 0 {
            println!("  ❌ Errors: {}", error_count);
        }
        println!();
    }

    info!("Indexing complete!");
    info!("  Total files: {}", image_files.len());
    info!("  Indexed: {}", indexed_count);
    info!("  Skipped: {}", skipped_count);
    info!("  Errors: {}", error_count);

    Ok(())
}

fn search_images(
    db: &Database,
    prompt: &str,
    limit: usize,
    short: bool,
    recursive: bool,
) -> Result<()> {
    info!("Searching for: \"{}\"", prompt);

    // Get current directory for filtering results
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;

    // Check if database has any images
    let total_images = db.get_image_count()?;
    if total_images == 0 {
        if !short {
            println!("No images found in database. Please index some images first using:");
            println!("  imgfind index --dir /path/to/images");
        }
        return Ok(());
    }

    info!("Loading CLIP model...");
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;

    // Generate embedding for query
    info!("Generating embedding for query...");
    let query_embedding = model
        .get_text_embedding(prompt)
        .context("Failed to generate text embedding")?;

    let normalized_query = normalize_vector(&query_embedding);

    // Search database
    info!("Searching database...");
    let search_engine = SearchEngine::new(db);
    let all_results = search_engine.search(&normalized_query, usize::MAX)?; // Get all results first

    // Filter results based on current directory and recursive flag
    let filtered_results: Vec<_> = all_results
        .into_iter()
        .filter(|(path, _score)| {
            let path_buf = std::path::Path::new(path);

            // Convert stored path to absolute path for comparison
            let abs_path = if path_buf.is_absolute() {
                path_buf.to_path_buf()
            } else {
                // For relative paths, they were stored relative to some working directory
                // Try to resolve against current directory first, then against likely project root
                let current_resolved = current_dir.join(path_buf);
                if current_resolved.exists() {
                    current_resolved
                } else {
                    // Try resolving from potential project root locations
                    let mut search_dir = current_dir.clone();
                    loop {
                        let candidate = search_dir.join(path_buf);
                        if candidate.exists() {
                            break candidate;
                        }
                        if let Some(parent) = search_dir.parent() {
                            search_dir = parent.to_path_buf();
                        } else {
                            // Fallback to current directory resolution
                            break current_resolved;
                        }
                    }
                }
            };

            // Canonicalize paths to handle . and .. components and get absolute paths
            let abs_path = abs_path.canonicalize().unwrap_or(abs_path);
            let current_dir_canonical = current_dir
                .canonicalize()
                .unwrap_or_else(|_| current_dir.clone());

            if recursive {
                // For recursive search, check if the image is in current directory or any subdirectory
                abs_path.starts_with(&current_dir_canonical)
            } else {
                // For non-recursive search, check if the image is directly in current directory
                if let Some(parent) = abs_path.parent() {
                    parent == current_dir_canonical
                } else {
                    false
                }
            }
        })
        .take(limit)
        .collect();

    if filtered_results.is_empty() {
        if !short {
            if recursive {
                println!(
                    "No images found matching the query \"{}\" in current directory or subdirectories.",
                    prompt
                );
            } else {
                println!(
                    "No images found matching the query \"{}\" in current directory.",
                    prompt
                );
            }
            println!(
                "Try using --recursive to search subdirectories, or run 'imgfind index' to index current directory."
            );
        }
        return Ok(());
    }

    if short {
        // Short format: just output paths, one per line
        for (path, _score) in filtered_results.iter() {
            println!("{}", path);
        }
    } else {
        // Standard format: detailed output with scores
        let search_scope = if recursive {
            "current directory and subdirectories"
        } else {
            "current directory"
        };
        println!(
            "\nFound {} result{} for \"{}\" in {}:\n",
            filtered_results.len(),
            if filtered_results.len() == 1 { "" } else { "s" },
            prompt,
            search_scope
        );

        for (i, (path, score)) in filtered_results.iter().enumerate() {
            println!("{:3}. {:<60} (similarity: {:.4})", i + 1, path, score);
        }

        println!();
    }

    Ok(())
}

fn clean_database(db: &mut Database) -> Result<()> {
    info!("Cleaning database of missing files...");

    let removed_count = db.clean_missing_files()?;

    if removed_count == 0 {
        println!("Database is clean - no missing files found.");
    } else {
        println!("Removed {} entries for missing files.", removed_count);
    }

    info!("Database cleanup complete");
    Ok(())
}

fn show_status(db: &Database, db_path: &PathBuf) -> Result<()> {
    println!("imgfind Database Status");
    println!("======================");
    println!("Database location: {}", db_path.display());

    let total_images = db.get_image_count()?;
    println!("Total indexed images: {}", total_images);

    if total_images > 0 {
        let sample_images = db.get_sample_images(5)?;
        println!("\nSample images:");
        for (i, path) in sample_images.iter().enumerate() {
            println!("  {}. {}", i + 1, path);
        }
        if total_images > 5 {
            println!("  ... and {} more", total_images - 5);
        }
    }

    // Check database file size
    if let Ok(metadata) = std::fs::metadata(db_path) {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        println!("\nDatabase size: {:.2} MB", size_mb);
    }

    println!();
    Ok(())
}

fn normalize_vector(vector: &[f32]) -> Vec<f32> {
    let magnitude: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if magnitude == 0.0 {
        return vector.to_vec();
    }
    vector.iter().map(|x| x / magnitude).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_vector() {
        let vector = vec![3.0, 4.0];
        let normalized = normalize_vector(&vector);
        let magnitude: f32 = normalized.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-6);
    }
}
