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
use std::sync::Arc;
use walkdir::WalkDir;

use imgfind::AbsolutePath;
use imgfind::abs_to_relative_path;
use imgfind::database::{Database, extract_image_metadata};
use imgfind::indexing::chunk_pending;
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
        /// Number of images to embed per CLIP batch (default: [index].batch_size config, 32)
        #[arg(long)]
        batch_size: Option<usize>,
        /// Skip generating thumbnails during indexing (generate later with `thumbnails`).
        #[arg(long)]
        no_thumbnails: bool,
        /// Embedding model to use for this run (sets it active first).
        #[arg(long)]
        model: Option<String>,
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
        /// Max cosine distance to include (lower = stricter). Overrides config [search].distance_threshold.
        #[arg(long, value_parser = parse_threshold)]
        threshold: Option<f32>,
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

        /// Embedding model to use for this run (sets it active first).
        #[arg(long)]
        model: Option<String>,
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
        /// Address to bind. Use 0.0.0.0 to expose on all interfaces.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(short, long, default_value_t = 6060)]
        port: usize,
    },
    /// Manage embedding models
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
}

#[derive(Subcommand)]
enum ModelsAction {
    /// List registered models (active marked with *)
    List,
    /// Set the active model
    Use { name: String },
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
            batch_size,
            no_thumbnails,
            model,
        } => {
            let db_path = if root {
                get_local_db_path()?
            } else {
                get_db_path(None)?
            };
            let mut db = Database::new(&db_path)?;
            if let Some(m) = model {
                db.set_active_model(&m)?;
            }
            index_directory(&mut db, &dir, recursive, quiet, batch_size, no_thumbnails)?;
        }
        Commands::Search {
            prompt,
            limit,
            threshold,
            short,
            recursive,
            all,
            display,
            model,
        } => {
            let db_path = get_db_path(None)?;
            let db = Database::new(&db_path)?;
            if let Some(m) = model {
                db.set_active_model(&m)?;
            }
            let config = config::Config::load()?;
            let distance_threshold = threshold.unwrap_or(config.search.distance_threshold);
            let max_k = config.search.max_k;
            search_images(
                &db,
                &prompt,
                limit,
                distance_threshold,
                max_k,
                short,
                recursive,
                all,
                display,
            )?;
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
        Commands::Serve { dir, host, port } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = Database::new(&db_path)?;
            serve(db, dir.unwrap_or(".".to_owned()), host, port).await?;
        }
        Commands::Models { action } => {
            let db_path = get_db_path(None)?;
            let db = Database::new(&db_path)?;
            match action {
                ModelsAction::List => {
                    for (name, dim, active) in db.list_models()? {
                        println!("{} {} (dim {})", if active { "*" } else { " " }, name, dim);
                    }
                }
                ModelsAction::Use { name } => {
                    db.set_active_model(&name)?;
                    println!("Active model: {name}");
                }
            }
        }
    }

    Ok(())
}

async fn serve(db: Database, directory: String, host: String, port: usize) -> Result<()> {
    info!("Loading CLIP model...");
    let embedder =
        Arc::new(ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?);
    let context = GraphQLContext::new(db, directory, embedder);
    let app = app(context.clone());
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;
    info!("Listening on http://{addr}");
    let server = axum::serve(listener, app).with_graceful_shutdown(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    });

    server.await.context("Server error")?;

    Ok(())
}

/// Parse a `--threshold` value, rejecting non-finite or negative numbers.
///
/// The value is interpolated into SQL as `distance <= {:.6}`, so `nan`/`inf`/
/// negative inputs would render invalid or nonsensical SQL. Reject them up front
/// with a clear message instead of failing opaquely at query time.
fn parse_threshold(s: &str) -> Result<f32, String> {
    let v: f32 = s.parse().map_err(|_| format!("invalid number: {s}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err("threshold must be a finite, non-negative number".to_string());
    }
    Ok(v)
}

fn metadata(db: &mut Database, quiet: bool, count: usize) -> Result<()> {
    extract_missing_metadata(db, quiet, count)
}

fn index_directory(
    db: &mut Database,
    dir: &str,
    recursive: bool,
    quiet: bool,
    batch_size_override: Option<usize>,
    no_thumbnails: bool,
) -> Result<()> {
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

    info!("Loading CLIP model...");
    let spinner = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_message("Loading CLIP model… (this may take a minute on first use)");
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        pb
    };
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;
    spinner.finish_and_clear();
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

    let batch_size = batch_size_override.unwrap_or(config.index.batch_size);

    let mut indexed_count = 0;
    let mut skipped_count = 0;
    let mut error_count = 0;

    info!("Starting to process images...");

    // Phase 1: collect images that are not yet indexed. We compute the content
    // hash and run the existing already-indexed check here; only genuinely-new
    // images make it into `pending`. Each entry carries (abs_path, hash) — the
    // relative path is derived just before insertion.
    let mut pending: Vec<(PathBuf, String)> = Vec::new();

    for path in image_files.iter() {
        // Convert to absolute path for storage consistency
        let abs_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(), // fallback to original path if canonicalize fails
        };
        let path_str = abs_path.to_string_lossy();

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
        if db.is_image_indexed(&AbsolutePath(abs_path.clone()), &hash)? {
            debug!("Skipping already indexed: {}", path_str);
            skipped_count += 1;
            progress_bar.inc(1);
            continue;
        }

        pending.push((abs_path, hash));
    }

    // Phase 2: batch-embed and insert the pending images. Images are decoded
    // individually so a single corrupt file can be skipped and counted rather
    // than failing an entire embedding batch.
    for chunk in chunk_pending(&pending, batch_size) {
        // Decode each image individually; survivors stay index-aligned with the
        // DynamicImage vec passed to the embedder.
        let mut survivors: Vec<(String, String, String)> = Vec::new(); // (abs_path_str, rel_path_str, hash)
        let mut images: Vec<image::DynamicImage> = Vec::new();

        for (abs_path, hash) in chunk {
            let path_str = abs_path.to_string_lossy().to_string();

            if !quiet {
                progress_bar.set_message(format!(
                    "Processing: {}",
                    abs_path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }

            let img = match image::open(abs_path) {
                Ok(img) => img,
                Err(e) => {
                    warn!("Failed to decode image {}: {}", path_str, e);
                    error_count += 1;
                    progress_bar.inc(1);
                    continue;
                }
            };

            let rel_path = match abs_to_relative_path(abs_path, &db.parent_dir) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(e) => {
                    warn!(
                        "Failed to convert path {} to relative path: {}",
                        path_str, e
                    );
                    error_count += 1;
                    progress_bar.inc(1);
                    continue;
                }
            };

            survivors.push((path_str, rel_path, hash.clone()));
            images.push(img);
        }

        if survivors.is_empty() {
            continue;
        }

        // Whole-batch embedding: a failure here drops the entire surviving chunk.
        let embeddings = match model.get_image_embeddings_from_dynamic(images) {
            Ok(embs) => embs,
            Err(e) => {
                warn!("Failed to generate embeddings for batch: {}", e);
                error_count += survivors.len();
                progress_bar.inc(survivors.len() as u64);
                continue;
            }
        };

        // Build rows (relative_path, hash, normalized_embedding) for batch insert.
        let rows: Vec<(String, String, Vec<f32>)> = survivors
            .iter()
            .zip(embeddings.iter())
            .map(|((_, rel_path, hash), embedding)| {
                (rel_path.clone(), hash.clone(), normalize_vector(embedding))
            })
            .collect();

        if let Err(e) = db.insert_images_batch(&rows) {
            warn!("Failed to insert image batch into database: {}", e);
            error_count += rows.len();
            progress_bar.inc(survivors.len() as u64);
            continue;
        }

        indexed_count += rows.len();

        // Extract and store metadata for the newly-indexed images. Metadata
        // extraction stays per-image and is non-critical.
        for (abs_path_str, _, _) in survivors.iter() {
            match extract_image_metadata(abs_path_str) {
                Ok(metadata) => match db.get_image_id(&AbsolutePath(PathBuf::from(abs_path_str))) {
                    Ok(image_id) => {
                        if let Err(e) = db.insert_or_update_metadata(image_id, &metadata) {
                            warn!("Failed to store metadata for {}: {}", abs_path_str, e);
                        } else {
                            debug!("Stored metadata for: {}", abs_path_str);
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get image ID for metadata storage {}: {}",
                            abs_path_str, e
                        );
                    }
                },
                Err(e) => {
                    debug!("Failed to extract metadata for {}: {}", abs_path_str, e);
                    // This is not critical, so we don't increment error_count
                }
            }
        }

        progress_bar.inc(survivors.len() as u64);
    }

    progress_bar.finish_with_message("Indexing complete!");

    if !quiet {
        println!("\nIndexing Summary:");
        println!("  📁 Total files found: {}", image_files.len());
        println!("  ✅ Newly indexed: {}", indexed_count);
        println!("  ⏭️  Already indexed (skipped): {}", skipped_count);
        if error_count > 0 {
            println!("  ❌ Failed: {}", error_count);
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
    info!("  Failed: {}", error_count);

    // Generate thumbnails for any images still missing a 300px thumbnail. `count`
    // is bound as a SQL `LIMIT ?` parameter (see get_images_without_thumbnails),
    // and rusqlite binds usize via i64::try_from — so usize::MAX overflows i64 and
    // the bind fails. Instead, count the images actually missing a thumbnail and
    // pass that exact number, which covers every one of them.
    if !no_thumbnails {
        let missing = db.count_images_without_thumbnails(300).unwrap_or_else(|e| {
            warn!("counting images without thumbnails failed (non-fatal): {e:#}");
            0
        });
        if missing > 0 {
            let made = generate_missing_thumbnails_batch(db, 300, missing).unwrap_or_else(|e| {
                warn!("thumbnail generation failed (non-fatal): {e:#}");
                0
            });
            if !quiet {
                info!("Generated {made} thumbnails");
            }
        }
    }

    if let Err(e) = db.checkpoint_wal() {
        warn!("WAL checkpoint failed (non-fatal): {e:#}");
    }

    Ok(())
}

fn search_images(
    db: &Database,
    prompt: &str,
    limit: usize,
    distance_threshold: f32,
    max_k: usize,
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
    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Loading CLIP model… (this may take a minute on first use)");
    spinner.enable_steady_tick(std::time::Duration::from_millis(120));
    let model = ClipEmbedder::new(None, None, false).context("Failed to create ClipEmbedder")?;
    spinner.finish_and_clear();

    // Generate embedding for query
    info!("Generating embedding for query...");
    let query_embedding = model
        .get_text_embedding(prompt)
        .context("Failed to generate text embedding")?;

    // Search database (SearchEngine normalizes the query internally)
    info!("Searching database...");
    let search_engine = SearchEngine::new(db);
    let all_results =
        search_engine.search(&query_embedding, usize::MAX, distance_threshold, max_k)?; // Get all results first

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
    println!("Database: {}", db_path.display());
    println!("imgfind Database Status");
    println!("======================");

    let total_images = db.get_image_count()?;
    println!("Total indexed images: {}", total_images);

    if total_images > 0 {
        let sample_images = db.get_sample_images(5)?;
        println!("\nSample images:");
        for (i, path) in sample_images.iter().enumerate() {
            println!("  {}. {}", i + 1, path.as_str());
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

#[cfg(test)]
mod tests {
    use super::parse_threshold;

    #[test]
    fn parse_threshold_accepts_valid_values() {
        assert_eq!(parse_threshold("1.3").unwrap(), 1.3);
        assert_eq!(parse_threshold("0.0").unwrap(), 0.0);
    }

    #[test]
    fn parse_threshold_rejects_invalid_values() {
        assert!(parse_threshold("-1.0").is_err());
        assert!(parse_threshold("inf").is_err());
        assert!(parse_threshold("-inf").is_err());
        assert!(parse_threshold("nan").is_err());
        assert!(parse_threshold("abc").is_err());
    }
}
