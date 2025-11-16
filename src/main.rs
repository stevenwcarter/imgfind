#![allow(clippy::collapsible_if)]
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use clipper::ClipEmbedder;
use imgfind::context::GraphQLContext;
use imgfind::logging::init_tracing;
use imgfind::metadata::extract_missing_metadata;
use imgfind::{config, get_db_path, get_local_db_path};
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, info, warn};
use oshash::oshash;
use std::collections::HashSet;
use std::path::PathBuf;
use walkdir::WalkDir;

use imgfind::database::{Database, extract_image_metadata};
use imgfind::routes::app;
use imgfind::search::{SearchEngine, normalize_vector};
use imgfind::thumbnail::generate_missing_thumbnails_batch;

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
        /// Create database in current directory instead of using existing or global database
        #[arg(long)]
        root: bool,
    },
    Metadata {
        #[arg(short, long)]
        dir: Option<String>,
        #[arg(short, long)]
        quiet: bool,
        #[arg(short, long, default_value = "100")]
        count: usize,
    },
    Tui {
        #[arg(short, long)]
        dir: Option<String>,
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

        /// Display image results
        #[arg(short, long)]
        display: bool,

        /// Search all images in the database
        #[arg(short, long)]
        all: bool,
    },
    /// Clean up missing files from database
    Clean,
    /// Show database status and statistics
    Status,
    /// Configuration management
    Config {
        #[command(subcommand)]
        config_command: ConfigCommands,
    },
    /// Generate thumbnails in batches
    Thumbnails {
        /// Thumbnail size (default: 300)
        #[arg(short, long, default_value_t = 300)]
        size: u32,
        /// Number of thumbnails to generate in this batch (default: 50)
        #[arg(short, long, default_value_t = 50)]
        count: usize,
    },
    Serve {
        #[arg(short, long)]
        dir: Option<String>,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Show current configuration
    Show,
    /// Add an ignore pattern
    AddIgnore {
        /// Pattern to add (regex supported)
        pattern: String,
    },
    /// Remove an ignore pattern
    RemoveIgnore {
        /// Pattern to remove
        pattern: String,
    },
    /// Reset configuration to defaults
    Reset,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing().context("Failed to initialize logging")?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Tui { dir } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = Database::new(&db_path)?;
            imgfind::tui::tui(db).await?;
        }
        Commands::Metadata { dir, quiet, count } => {
            let db_path = get_db_path(dir.as_deref())?;
            let mut db = Database::new(&db_path)?;
            metadata(&mut db, quiet, count)?;
        }
        Commands::Index {
            dir,
            recursive,
            quiet,
            root,
        } => {
            let db_path = if root {
                get_local_db_path()?
            } else {
                get_db_path(None)?
            };
            let mut db = Database::new(&db_path)?;
            index_directory(&mut db, &dir, recursive, quiet)?;
        }
        Commands::Search {
            prompt,
            limit,
            short,
            recursive,
            all,
            display,
        } => {
            let db_path = get_db_path(None)?;
            let db = Database::new(&db_path)?;
            search_images(&db, &prompt, limit, short, recursive, all, display)?;
        }
        Commands::Clean => {
            let db_path = get_db_path(None)?;
            let mut db = Database::new(&db_path)?;
            clean_database(&mut db)?;
        }
        Commands::Status => {
            let db_path = get_db_path(None)?;
            let db = Database::new(&db_path)?;
            show_status(&db, &db_path)?;
        }
        Commands::Config { config_command } => {
            handle_config_command(config_command)?;
        }
        Commands::Thumbnails { size, count } => {
            let db_path = get_db_path(None)?;
            let mut db = Database::new(&db_path)?;
            generate_thumbnails_batch(&mut db, size, count)?;
        }
        Commands::Serve { dir } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = Database::new(&db_path)?;
            serve(db, dir.unwrap_or(".".to_owned())).await?;
        }
    }

    Ok(())
}

async fn serve(db: Database, directory: String) -> Result<()> {
    // Placeholder for future server implementation
    println!("Server functionality is not yet implemented.");
    let port = 6060;
    let context = GraphQLContext::new(db, directory);
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

fn metadata(db: &mut Database, quiet: bool, count: usize) -> Result<()> {
    extract_missing_metadata(db, quiet, count)
}

fn index_directory(db: &mut Database, dir: &str, recursive: bool, quiet: bool) -> Result<()> {
    if !quiet {
        println!("Indexing directory: {}", dir);
    }
    info!("Indexing directory: {}", dir);

    // Load configuration
    let config = config::Config::load().context("Failed to load configuration")?;
    if !quiet {
        println!(
            "Loaded configuration with {} ignore patterns",
            config.ignore_patterns.len()
        );
    }

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
        WalkDir::new(dir)
    } else {
        WalkDir::new(dir).max_depth(1)
    };

    let mut image_files = Vec::new();
    let mut walker_iter = walker.into_iter();

    while let Some(entry_result) = walker_iter.next() {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        let path = entry.path();

        // Check if this path should be ignored based on configuration
        if config.should_ignore_path(path) {
            debug!("Ignoring path due to config pattern: {}", path.display());

            // If this is a directory, skip traversing into it entirely
            if path.is_dir() {
                walker_iter.skip_current_dir();
            }
            continue;
        }

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

        // Extract and store metadata
        match extract_image_metadata(&path_str) {
            Ok(metadata) => {
                // Get the image ID to store metadata
                match db.get_image_id(&path_str) {
                    Ok(image_id) => {
                        if let Err(e) = db.insert_or_update_metadata(image_id, &metadata) {
                            warn!("Failed to store metadata for {}: {}", path_str, e);
                        } else {
                            debug!("Stored metadata for: {}", path_str);
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get image ID for metadata storage {}: {}",
                            path_str, e
                        );
                    }
                }
            }
            Err(e) => {
                debug!("Failed to extract metadata for {}: {}", path_str, e);
                // This is not critical, so we don't increment error_count
            }
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

    // Backfill metadata for existing images that don't have it
    if !quiet {
        println!("Checking for images missing metadata...");
    }
    info!("Starting metadata backfill for existing images");

    extract_missing_metadata(db, quiet, 100).context("extracting missing metadata")?;

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
    all: bool,
    display: bool,
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

            // The paths returned from the database are already absolute paths
            let abs_path = path_buf.to_path_buf();

            // Canonicalize paths to handle . and .. components and get absolute paths
            let abs_path = abs_path.canonicalize().unwrap_or(abs_path);
            let current_dir_canonical = current_dir
                .canonicalize()
                .unwrap_or_else(|_| current_dir.clone());

            if all {
                // For --all flag, include all results regardless of location
                true
            } else if recursive {
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
            if display {
                print_image(path).context("Failed to display image")?;
            }
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
            if display {
                print_image(path).context("Failed to display image")?;
            }
        }

        println!();
    }

    Ok(())
}

fn print_image(path: &str) -> Result<()> {
    let bytes = std::fs::read(path).context("Failed to read image file for display")?;
    let image = iterm2img::from_bytes(bytes)
        .width_auto()
        .height_percent(33)
        .preserve_aspect_ratio(true)
        .inline(true)
        .build();
    println!("{}", image);

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

fn handle_config_command(config_command: ConfigCommands) -> Result<()> {
    match config_command {
        ConfigCommands::Show => {
            let config = config::Config::load()?;
            let config_path = config::Config::get_config_path()?;

            println!("Configuration file: {}", config_path.display());
            println!("Ignore patterns:");
            if config.ignore_patterns.is_empty() {
                println!("  (none)");
            } else {
                for (i, pattern) in config.ignore_patterns.iter().enumerate() {
                    println!("  {}. {}", i + 1, pattern);
                }
            }
        }
        ConfigCommands::AddIgnore { pattern } => {
            let mut config = config::Config::load()?;
            if !config.ignore_patterns.contains(&pattern) {
                config.ignore_patterns.push(pattern.clone());
                config.save()?;
                println!("Added ignore pattern: {}", pattern);
            } else {
                println!("Pattern already exists: {}", pattern);
            }
        }
        ConfigCommands::RemoveIgnore { pattern } => {
            let mut config = config::Config::load()?;
            if let Some(index) = config.ignore_patterns.iter().position(|x| x == &pattern) {
                config.ignore_patterns.remove(index);
                config.save()?;
                println!("Removed ignore pattern: {}", pattern);
            } else {
                println!("Pattern not found: {}", pattern);
            }
        }
        ConfigCommands::Reset => {
            let default_config = config::Config::default();
            default_config.save()?;
            println!("Configuration reset to defaults");
            println!("Default ignore patterns:");
            for (i, pattern) in default_config.ignore_patterns.iter().enumerate() {
                println!("  {}. {}", i + 1, pattern);
            }
        }
    }
    Ok(())
}

/// Generate thumbnails in batches for images that don't have cached thumbnails
fn generate_thumbnails_batch(db: &mut Database, size: u32, count: usize) -> Result<()> {
    info!(
        "Starting thumbnail generation: size={}px, batch_count={}",
        size, count
    );

    let generated = generate_missing_thumbnails_batch(db, size, count)
        .context("Failed to generate thumbnails")?;

    if generated == 0 {
        println!("No images found that need thumbnails of size {}px", size);
    } else {
        println!("Generated {} thumbnails of size {}px", generated, size);

        // Check if there are more images that need thumbnails
        let remaining = db.count_images_without_thumbnails(size)?;
        if remaining != 0 {
            println!(
                "There are more images without thumbnails ({remaining}). Run the command again to generate more.",
            );
        } else {
            println!("All images now have thumbnails of size {}px", size);
        }
    }

    Ok(())
}
