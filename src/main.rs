#![allow(clippy::collapsible_if)]
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clipper::ClipEmbedder;
use imgfind::logging::init_tracing;
use imgfind::metadata::extract_missing_metadata;
use imgfind::{config, get_db_path, get_local_db_path};
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, info, warn};
use oshash::oshash;
use std::path::PathBuf;
use walkdir::WalkDir;

use imgfind::AbsolutePath;
use imgfind::DistanceThreshold;
use imgfind::MaxK;
use imgfind::ThumbnailSize;
use imgfind::abs_to_relative_path;
use imgfind::database::Database;
use imgfind::indexing::chunk_pending;
use imgfind::search::SearchEngine;
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
        /// Deprecated: thumbnails are now deferred to `process`; this flag is a no-op.
        #[arg(long)]
        no_thumbnails: bool,
        /// Embedding model to use for this run (sets it active first).
        #[arg(long)]
        model: Option<String>,
        /// Re-embed every image even if it already has an embedding for the
        /// active model (otherwise already-indexed images are skipped).
        #[arg(long)]
        reindex: bool,
    },
    Metadata {
        #[arg(short, long)]
        dir: Option<String>,
        #[arg(short, long)]
        quiet: bool,
        #[arg(short, long, default_value = "100")]
        count: usize,
    },
    #[cfg(feature = "tui")]
    Tui {
        #[arg(short, long)]
        dir: Option<String>,
    },
    /// Launch the native desktop GUI (forwards remaining args to imgfind-gui)
    Gui {
        /// Arguments passed through to imgfind-gui (e.g. -d / --dir DIR)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
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
        /// Deprecated: recursive search is now the default; this flag is a no-op.
        #[arg(short, long, hide = true)]
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
    /// Generate thumbnails in batches.
    ///
    /// The GUI uses 300px (grid), 512px (detail panel), and 2048px (lightbox/preview).
    /// Pass --gui-sizes to pre-generate exactly those three sizes.
    Thumbnails {
        /// Thumbnail size(s) to generate. Repeat for multiple, e.g. -s 300 -s 512.
        /// The GUI uses 300 (grid), 512 (detail panel), and 2048 (lightbox/preview);
        /// pass --gui-sizes to pre-generate exactly those.
        #[arg(short, long)]
        size: Vec<u32>,
        /// Generate the full set of sizes the GUI uses (300, 512, 2048).
        #[arg(long)]
        gui_sizes: bool,
        /// Number of thumbnails to generate per size in this batch.
        #[arg(short, long, default_value_t = 50)]
        count: usize,
        /// Generate ALL missing thumbnails for each requested size (loops in
        /// batches of --count until none remain), instead of a single batch.
        #[arg(long)]
        all: bool,
    },
    /// Generate embeddings and thumbnails for indexed images (resumable).
    ///
    /// Runs the three processing phases in order: 300 px thumbnails →
    /// CLIP embeddings → 512/2048 px thumbnails. Each phase is skippable
    /// if already complete, so the command is safe to re-run.
    Process {
        /// Per-batch size (images processed per iteration).
        #[arg(long, default_value_t = 64)]
        count: usize,
        /// Skip embedding generation (thumbnail-only pass; avoids loading the
        /// ~1.7 GB CLIP model).
        #[arg(long)]
        no_embeddings: bool,
        /// Directory whose DB to use (walk-up/global logic otherwise).
        #[arg(short, long)]
        dir: Option<String>,
    },
    /// Manage embedding models
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// Generate a shell completion script
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum ModelsAction {
    /// List registered models (active marked with *)
    List,
    /// Set the active model
    Use {
        name: String,
        /// Also save this model as the global default for new databases.
        #[arg(long)]
        default: bool,
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
    /// Get, set, or clear the default embedding model for new databases
    Model {
        /// Model name to set as the default. Omit to show the current default.
        name: Option<String>,
        /// Clear the configured default (revert to the built-in baseline).
        #[arg(long)]
        clear: bool,
    },
}

fn main() -> Result<()> {
    init_tracing().context("Failed to initialize logging")?;

    let cli = Cli::parse();

    match cli.command {
        #[cfg(feature = "tui")]
        Commands::Tui { dir } => {
            let db_path = get_db_path(dir.as_deref())?;
            let db = imgfind::block_on(Database::new(&db_path))?;
            imgfind::block_on(imgfind::tui::tui(db))?;
        }
        Commands::Gui { args } => launch_gui(&args)?,
        Commands::Metadata { dir, quiet, count } => {
            let db_path = get_db_path(dir.as_deref())?;
            let mut db = imgfind::block_on(Database::new(&db_path))?;
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
            reindex,
        } => {
            let db_path = if root {
                get_local_db_path()?
            } else {
                get_db_path(None)?
            };
            // A brand-new database is seeded with the configured default model;
            // an explicit --model (applied below) still wins over it.
            let default_model = config::Config::load()?.default_model;
            let mut db = imgfind::block_on(imgfind::models::open_db_seeding_default(
                &db_path,
                default_model.as_deref(),
            ))?;
            if let Some(m) = model {
                imgfind::block_on(imgfind::models::ensure_and_activate_model(&db, &m))?;
            }
            index_directory(
                &mut db,
                &dir,
                recursive,
                quiet,
                batch_size,
                no_thumbnails,
                reindex,
            )?;
        }
        Commands::Search {
            prompt,
            limit,
            threshold,
            short,
            recursive: _,
            all,
            display,
            model,
        } => {
            let db_path = get_db_path(None)?;
            let db = imgfind::block_on(Database::new(&db_path))?;
            if let Some(m) = model {
                imgfind::block_on(imgfind::models::ensure_and_activate_model(&db, &m))?;
            }
            let config = config::Config::load()?;
            let distance_threshold = threshold
                .map(DistanceThreshold)
                .unwrap_or(config.search.distance_threshold);
            let max_k = config.search.max_k;
            search_images(
                &db,
                &prompt,
                limit,
                distance_threshold,
                max_k,
                short,
                all,
                display,
            )?;
        }
        Commands::Clean => {
            let db_path = get_db_path(None)?;
            let mut db = imgfind::block_on(Database::new(&db_path))?;
            clean_database(&mut db)?;
        }
        Commands::Status => {
            let db_path = get_db_path(None)?;
            let db = imgfind::block_on(Database::new(&db_path))?;
            show_status(&db, &db_path)?;
        }
        Commands::Config { config_command } => {
            handle_config_command(config_command)?;
        }
        Commands::Thumbnails {
            size,
            count,
            gui_sizes,
            all,
        } => {
            let db_path = get_db_path(None)?;
            let mut db = imgfind::block_on(Database::new(&db_path))?;
            for s in resolve_thumbnail_sizes(&size, gui_sizes)? {
                if all {
                    let n = imgfind::thumbnail::generate_all_missing_thumbnails(&mut db, s, count)?;
                    println!("Generated {} thumbnails of size {}px (all)", n, s.get());
                } else {
                    generate_thumbnails_batch(&mut db, s, count)?;
                }
            }
        }
        Commands::Process {
            count,
            no_embeddings,
            dir,
        } => {
            let db_path = get_db_path(dir.as_deref())?;
            let mut db = imgfind::block_on(Database::new(&db_path))?;
            let embedder: Option<ClipEmbedder> = if no_embeddings {
                None
            } else {
                info!("Loading CLIP model...");
                let spinner = ProgressBar::new_spinner();
                spinner
                    .set_message("Loading CLIP model… (this may take a minute on first use)");
                spinner.enable_steady_tick(std::time::Duration::from_millis(120));
                let model_name = imgfind::block_on(db.active_model())?.name;
                let model = ClipEmbedder::from_model(&model_name, false)
                    .context("Failed to create ClipEmbedder")?;
                spinner.finish_and_clear();
                Some(model)
            };
            // The `sizes` arg controls the FullThumbnails phase only; 300 px is
            // always handled by Phase 1 internally. Passing 512 and 2048 matches
            // the GUI_THUMBNAIL_SIZES without the 300 px redundancy.
            imgfind::processing::run_to_completion(
                &mut db,
                embedder.as_ref(),
                count,
                &[ThumbnailSize(512), ThumbnailSize(2048)],
                !no_embeddings,
                |phase, n| println!("{phase:?}: processed {n}"),
            )?;
        }
        Commands::Models { action } => {
            let db_path = get_db_path(None)?;
            let db = imgfind::block_on(Database::new(&db_path))?;
            match action {
                ModelsAction::List => {
                    for row in imgfind::block_on(imgfind::models::list_rows(&db))? {
                        let mark = if row.active { "*" } else { " " };
                        let tag = if row.indexed {
                            ""
                        } else {
                            " [available, not indexed]"
                        };
                        println!("{} {} (dim {}){}", mark, row.name, row.dim, tag);
                    }
                }
                ModelsAction::Use { name, default } => {
                    imgfind::block_on(imgfind::models::ensure_and_activate_model(&db, &name))?;
                    println!("Active model: {name}");
                    if default {
                        let mut cfg = config::Config::load()?;
                        cfg.default_model = Some(name.clone());
                        cfg.save()?;
                        println!("Saved {name} as the global default model.");
                    }
                }
            }
        }
        Commands::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "imgfind",
                &mut std::io::stdout(),
            );
        }
    }

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

/// Launch the imgfind-gui binary with `args`, blocking until it exits and
/// propagating its exit code, so `imgfind gui ARGS` behaves like `imgfind-gui ARGS`.
///
/// On the happy path this never returns `Ok`: it diverges via `process::exit` with
/// the child's status. The `Result<()>` return type exists only to propagate a spawn
/// failure (`?` at the call site) — that is the function's sole `Err` path.
fn launch_gui(args: &[std::ffi::OsString]) -> anyhow::Result<()> {
    let program = imgfind::resolve_sibling_binary("imgfind-gui");

    let status = std::process::Command::new(&program)
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "failed to launch imgfind-gui ({}); is it installed / on PATH?",
                program.to_string_lossy()
            )
        })?;
    std::process::exit(status.code().unwrap_or(1));
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
    _no_thumbnails: bool,
    reindex: bool,
) -> Result<()> {
    if !quiet {
        println!("Indexing directory: {}", dir);
    }
    info!("Indexing directory: {}", dir);

    let config = config::Config::load().context("Failed to load configuration")?;
    if !quiet {
        println!(
            "Loaded configuration with {} ignore patterns",
            config.ignore_patterns.len()
        );
    }

    let dir_path = std::path::Path::new(dir);
    if !dir_path.exists() {
        return Err(anyhow::anyhow!("Directory does not exist: {}", dir));
    }
    if !dir_path.is_dir() {
        return Err(anyhow::anyhow!("Path is not a directory: {}", dir));
    }

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
            Err(e) => {
                debug!("skipped path on walk error: {e}");
                continue;
            }
        };

        let path = entry.path();

        if config.should_ignore_path(path) {
            debug!("Ignoring path due to config pattern: {}", path.display());
            if path.is_dir() {
                walker_iter.skip_current_dir();
            }
            continue;
        }

        if !path.is_file() {
            continue;
        }

        if let Some(ext_str) = path.extension().and_then(|e| e.to_str())
            && imgfind::decode::is_supported_extension(ext_str)
        {
            image_files.push(path.to_path_buf());
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
    }
    info!("Found {} image files to process", image_files.len());

    let progress_bar = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(image_files.len() as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({eta}) {msg}")
                .expect("progress bar template is a valid constant")
                .progress_chars("#>-"),
        );
        pb
    };

    let batch_size = batch_size_override.unwrap_or(config.index.batch_size);

    let mut indexed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut error_count = 0usize;

    // Scan phase: hash each file and collect (rel_path, hash) pairs for new images.
    // The dedup check is row-only (no embedding check) so images queued here will
    // still need `process` to generate embeddings and thumbnails.
    let mut pending: Vec<(String, String)> = Vec::new();

    for path in &image_files {
        let abs_path = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };
        let path_str = abs_path.to_string_lossy();

        let hash = match oshash(&path_str) {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to calculate hash for {}: {}", path_str, e);
                error_count += 1;
                progress_bar.inc(1);
                continue;
            }
        };

        // Skip if a row for this path+hash already exists (unless --reindex).
        if !reindex
            && imgfind::block_on(db.image_row_exists(&AbsolutePath(abs_path.clone()), &hash))?
        {
            debug!("Skipping already indexed: {}", path_str);
            skipped_count += 1;
            progress_bar.inc(1);
            continue;
        }

        let rel_path = match abs_to_relative_path(&abs_path, &db.parent_dir) {
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

        pending.push((rel_path, hash));
        progress_bar.inc(1);
    }

    progress_bar.finish_with_message("Inserting rows...");

    // Insert phase: write row-only records in batches (no CLIP, no thumbnails).
    for chunk in chunk_pending(&pending, batch_size) {
        if let Err(e) = imgfind::block_on(db.insert_image_rows_batch(chunk)) {
            warn!("Failed to insert image batch into database: {}", e);
            error_count += chunk.len();
            continue;
        }
        indexed_count += chunk.len();
    }

    if !quiet {
        println!("\nIndexing Summary:");
        println!("  Total files found: {}", image_files.len());
        println!("  Newly indexed: {}", indexed_count);
        println!("  Already indexed (skipped): {}", skipped_count);
        if error_count > 0 {
            println!("  Failed: {}", error_count);
        }
        println!();
    }

    info!(
        "Indexed {} files. Run `imgfind process` (or open the GUI) to generate embeddings + thumbnails.",
        indexed_count
    );

    if let Err(e) = imgfind::block_on(db.checkpoint_wal()) {
        warn!("WAL checkpoint failed (non-fatal): {e:#}");
    }

    Ok(())
}

/// Filter DB-relative image paths to those under `current_dir` (absolute).
///
/// `parent_dir` is `Database::parent_dir`; paths in `results` are relative to it.
/// Returns items passing the location filter, truncated to `limit`.
fn filter_results(
    results: Vec<(String, f32)>,
    parent_dir: &std::path::Path,
    current_dir: &std::path::Path,
    all: bool,
    limit: usize,
) -> Vec<(String, f32)> {
    let current_dir_canonical = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());

    results
        .into_iter()
        .filter(|(path, _score)| {
            // Paths from the DB are relative to parent_dir — make them absolute
            // before comparing against current_dir. Using canonicalize() on the
            // raw relative path would resolve against cwd, not parent_dir.
            let abs_path = imgfind::relative_to_abs_path(std::path::Path::new(path), parent_dir);
            let abs_path = abs_path.canonicalize().unwrap_or(abs_path);

            if all {
                true
            } else {
                abs_path.starts_with(&current_dir_canonical)
            }
        })
        .take(limit)
        .collect()
}

// CLI dispatch fn; arg list mirrors the clap subcommand flags.
#[allow(clippy::too_many_arguments)]
fn search_images(
    db: &Database,
    prompt: &str,
    limit: usize,
    distance_threshold: DistanceThreshold,
    max_k: MaxK,
    short: bool,
    all: bool,
    display: bool,
) -> Result<()> {
    info!("Searching for: \"{}\"", prompt);

    // Get current directory for filtering results
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;

    // Check if database has any images
    let total_images = imgfind::block_on(db.get_image_count())?;
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
    let model_name = imgfind::block_on(db.active_model())?.name;
    let model =
        ClipEmbedder::from_model(&model_name, false).context("Failed to create ClipEmbedder")?;
    spinner.finish_and_clear();

    // Generate embedding for query
    info!("Generating embedding for query...");
    let query_embedding = model
        .get_text_embedding(prompt)
        .context("Failed to generate text embedding")?;

    // Search database (SearchEngine normalizes the query internally)
    info!("Searching database...");
    let search_engine = SearchEngine::new(db);
    let all_results = imgfind::block_on(search_engine.search(
        &query_embedding,
        usize::MAX,
        distance_threshold,
        max_k,
    ))?; // Get all results first

    // Filter results to those under current_dir, resolving DB-relative paths
    // against the DB parent directory (not cwd).
    let filtered_results = filter_results(all_results, &db.parent_dir, &current_dir, all, limit);

    if filtered_results.is_empty() {
        if !short {
            if all {
                println!("No images found matching the query \"{}\".", prompt);
            } else {
                println!(
                    "No images found matching the query \"{}\" in the current directory or subdirectories.",
                    prompt
                );
            }
            println!("Try running 'imgfind index' to index the current directory.");
        }
        return Ok(());
    }

    if short {
        // Short format: just output paths, one per line
        for (path, _score) in filtered_results.iter() {
            println!("{}", path);
            if display {
                let abs = imgfind::relative_to_abs_path(std::path::Path::new(path), &db.parent_dir);
                print_image(&abs.to_string_lossy()).context("Failed to display image")?;
            }
        }
    } else {
        // Standard format: detailed output with scores
        let search_scope = if all {
            "the database"
        } else {
            "the current directory and subdirectories"
        };
        println!(
            "\nFound {} result{} for \"{}\" in {}:\n",
            filtered_results.len(),
            if filtered_results.len() == 1 { "" } else { "s" },
            prompt,
            search_scope
        );

        for (i, (path, score)) in filtered_results.iter().enumerate() {
            // `score` is cosine *distance* (lower = more similar). Results are
            // already ordered best-first (distance ascending); convert to the
            // familiar cosine similarity (1 - distance) for display.
            println!("{:3}. {:<60} (similarity: {:.4})", i + 1, path, 1.0 - score);
            if display {
                let abs = imgfind::relative_to_abs_path(std::path::Path::new(path), &db.parent_dir);
                print_image(&abs.to_string_lossy()).context("Failed to display image")?;
            }
        }

        println!();
    }

    Ok(())
}

fn print_image(path: &str) -> Result<()> {
    let is_raw = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(imgfind::decode::is_raw_extension)
        .unwrap_or(false);

    let bytes = if is_raw {
        // Terminals can't render RAW; decode via the seam and re-encode to JPEG.
        let img = imgfind::decode::decode_image(std::path::Path::new(path))
            .context("Failed to decode RAW image for display")?;
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .context("Failed to encode RAW preview as JPEG for display")?;
        buf
    } else {
        std::fs::read(path).context("Failed to read image file for display")?
    };

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

    let removed_count = imgfind::block_on(db.clean_missing_files())?;

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

    let total_images = imgfind::block_on(db.get_image_count())?;
    println!("Total indexed images: {}", total_images);

    if total_images > 0 {
        let sample_images = imgfind::block_on(db.get_sample_images(5))?;
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

    // Unprocessed work remaining
    let c = imgfind::block_on(imgfind::processing::counts(db))?;
    println!("\nUnprocessed work:");
    println!("  missing 300px thumbnails: {}", c.thumbs300);
    println!("  missing embeddings:       {}", c.embeddings);
    println!("  missing 512/2048 thumbs:  {}", c.full_thumbs);

    println!();
    Ok(())
}

fn handle_config_command(config_command: ConfigCommands) -> Result<()> {
    match config_command {
        ConfigCommands::Show => {
            let config = config::Config::load()?;
            let config_path = config::Config::get_config_path()?;

            println!("Configuration file: {}", config_path.display());
            match &config.default_model {
                Some(m) => println!("Default model: {m}"),
                None => println!("Default model: (built-in baseline) openai/clip-vit-base-patch32"),
            }
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
        ConfigCommands::Model { name, clear } => {
            let mut config = config::Config::load()?;
            if clear {
                config.default_model = None;
                config.save()?;
                println!("Cleared default model (reverting to the built-in baseline).");
            } else if let Some(name) = name {
                // Only allow models clipper actually knows how to load.
                let supported: Vec<String> = clipper::supported_models()
                    .into_iter()
                    .map(|m| m.name)
                    .collect();
                anyhow::ensure!(
                    supported.iter().any(|n| n == &name),
                    "unknown model '{name}'; supported models: {supported:?}"
                );
                config.default_model = Some(name.clone());
                config.save()?;
                println!("Default model set to {name}");
            } else {
                match config.default_model {
                    Some(m) => println!("Default model: {m}"),
                    None => println!(
                        "No default model set (using built-in baseline openai/clip-vit-base-patch32)"
                    ),
                }
            }
        }
    }
    Ok(())
}

/// Upper bound on a requested thumbnail dimension. `--size` is a bounding-box
/// long edge in px; 12000 yields ~100 MP at a typical 3:2 photo aspect
/// (12000×8000 = 96 MP). Guards against `--size 4294967295` OOM-ing image::resize.
const MAX_THUMBNAIL_SIZE: u32 = 12_000;

/// Resolve the set of thumbnail sizes to generate.
///
/// - `gui_sizes` → returns `GUI_THUMBNAIL_SIZES` (300, 512, 2048).
/// - `sizes` non-empty → uses the provided list.
/// - Both empty/false → defaults to `[300]`.
///
/// Duplicates are removed while preserving the first-occurrence order.
///
/// Returns `Err` if any size is 0 (since `image::resize(0, 0, …)` panics) or
/// exceeds `MAX_THUMBNAIL_SIZE` (which would OOM/hang `image::resize`).
fn resolve_thumbnail_sizes(sizes: &[u32], gui_sizes: bool) -> anyhow::Result<Vec<ThumbnailSize>> {
    if let Some(&bad) = sizes.iter().find(|&&s| s == 0) {
        anyhow::bail!("thumbnail size must be ≥ 1, got {bad}");
    }
    if let Some(&bad) = sizes.iter().find(|&&s| s > MAX_THUMBNAIL_SIZE) {
        anyhow::bail!("thumbnail size must be ≤ {MAX_THUMBNAIL_SIZE}, got {bad}");
    }
    let raw: Vec<ThumbnailSize> = if gui_sizes {
        imgfind::thumbnail::GUI_THUMBNAIL_SIZES.to_vec()
    } else if sizes.is_empty() {
        vec![ThumbnailSize(300)]
    } else {
        sizes.iter().copied().map(ThumbnailSize).collect()
    };
    let mut seen = std::collections::HashSet::new();
    Ok(raw.into_iter().filter(|s| seen.insert(*s)).collect())
}

/// Generate thumbnails in batches for images that don't have cached thumbnails
fn generate_thumbnails_batch(db: &mut Database, size: ThumbnailSize, count: usize) -> Result<()> {
    let px = size.get();
    info!(
        "Starting thumbnail generation: size={}px, batch_count={}",
        px, count
    );

    let generated = generate_missing_thumbnails_batch(db, size, count)
        .context("Failed to generate thumbnails")?;

    if generated == 0 {
        println!("No images found that need thumbnails of size {}px", px);
    } else {
        println!("Generated {} thumbnails of size {}px", generated, px);

        // Check if there are more images that need thumbnails
        let remaining = imgfind::block_on(db.count_images_without_thumbnails(size))?;
        if remaining != 0 {
            println!(
                "There are more images without thumbnails ({remaining}). Run the command again to generate more.",
            );
        } else {
            println!("All images now have thumbnails of size {}px", px);
        }
    }

    Ok(())
}

#[cfg(test)]
mod gui_cli_tests {
    use super::*;
    use clap::Parser;
    use std::ffi::OsString;

    #[test]
    fn gui_captures_trailing_args_including_hyphens() {
        let cli = Cli::try_parse_from(["imgfind", "gui", "--dir", "x", "-d", "y", "--whatever"])
            .expect("parse");
        match cli.command {
            Commands::Gui { args } => {
                let want: Vec<OsString> = ["--dir", "x", "-d", "y", "--whatever"]
                    .iter()
                    .map(OsString::from)
                    .collect();
                assert_eq!(args, want);
            }
            _ => panic!("expected Gui subcommand"),
        }
    }

    #[test]
    fn gui_with_no_args_is_empty() {
        let cli = Cli::try_parse_from(["imgfind", "gui"]).expect("parse");
        assert!(matches!(cli.command, Commands::Gui { args } if args.is_empty()));
    }

    #[cfg(feature = "tui")]
    #[test]
    fn existing_subcommand_still_parses() {
        let cli = Cli::try_parse_from(["imgfind", "tui"]).expect("parse");
        assert!(matches!(cli.command, Commands::Tui { .. }));
    }

    #[test]
    fn status_subcommand_still_parses() {
        let cli = Cli::try_parse_from(["imgfind", "status"]).expect("parse");
        assert!(matches!(cli.command, Commands::Status));
    }

    #[test]
    fn resolve_sizes_default_is_300() {
        assert_eq!(
            resolve_thumbnail_sizes(&[], false).unwrap(),
            vec![ThumbnailSize(300)]
        );
    }

    #[test]
    fn resolve_sizes_gui_flag_expands() {
        assert_eq!(
            resolve_thumbnail_sizes(&[], true).unwrap(),
            vec![ThumbnailSize(300), ThumbnailSize(512), ThumbnailSize(2048)]
        );
    }

    #[test]
    fn resolve_sizes_explicit_dedup_preserves_order() {
        assert_eq!(
            resolve_thumbnail_sizes(&[512, 300, 512], false).unwrap(),
            vec![ThumbnailSize(512), ThumbnailSize(300)]
        );
    }

    /// Regression: `--size 0` must be rejected before reaching `image::resize`,
    /// which panics when either dimension is 0.
    #[test]
    fn resolve_sizes_rejects_zero() {
        assert!(resolve_thumbnail_sizes(&[0], false).is_err());
    }

    #[test]
    fn resolve_sizes_rejects_zero_mixed_with_valid() {
        assert!(resolve_thumbnail_sizes(&[300, 0, 512], false).is_err());
    }

    #[test]
    fn resolve_sizes_accepts_positive() {
        assert!(resolve_thumbnail_sizes(&[300], false).is_ok());
    }

    /// Regression: an unbounded `--size` (e.g. `4294967295`) must be rejected
    /// before reaching `image::resize`, which would otherwise try to allocate
    /// an absurd rendition (OOM/hang). Sizes at/below the cap stay valid.
    #[test]
    fn resolve_sizes_rejects_above_cap() {
        assert!(resolve_thumbnail_sizes(&[MAX_THUMBNAIL_SIZE + 1], false).is_err());
        assert!(resolve_thumbnail_sizes(&[20_000], false).is_err());
        assert!(resolve_thumbnail_sizes(&[u32::MAX], false).is_err());
        assert!(resolve_thumbnail_sizes(&[300, u32::MAX, 512], false).is_err());
        assert!(resolve_thumbnail_sizes(&[MAX_THUMBNAIL_SIZE], false).is_ok());
        assert!(resolve_thumbnail_sizes(&[2048], false).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_results, parse_threshold};

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

    /// `filter_results` must resolve DB-relative paths against `parent_dir`, not cwd.
    ///
    /// The key scenario: `parent_dir` is a *subdirectory* of cwd (e.g. `src/`),
    /// so the relative path `"main.rs"` lives at `parent_dir/main.rs` but NOT
    /// at `cwd/main.rs`. With the old buggy code, `canonicalize("main.rs")`
    /// resolved against cwd (workspace root) and failed or found the wrong file,
    /// so the entry was excluded from results. After the fix, `relative_to_abs_path`
    /// joins against `parent_dir` and the entry passes the filter.
    ///
    /// This test is RED with the old inline code and GREEN after the fix.
    #[test]
    fn filter_results_uses_parent_dir_not_cwd() {
        // parent_dir = workspace-root/src (a real subdirectory, distinct from cwd
        // which is the workspace root when cargo test runs).
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let parent_dir = manifest.join("src");
        // "main.rs" exists under parent_dir/src. With recursive search (now the
        // default), it resolves to <manifest>/src/main.rs, which is under
        // current_dir (== parent_dir) and so passes the filter.
        let results = vec![("main.rs".to_string(), 0.9_f32)];
        let out = filter_results(
            results,
            &parent_dir,
            &parent_dir, // current_dir == parent_dir: recursive match expected
            false,       // not --all
            10,
        );
        assert_eq!(
            out.len(),
            1,
            "main.rs under src/ must pass the recursive filter when cwd=src/"
        );
    }

    /// Proves search descends into subdirectories (the recursive default).
    ///
    /// `parent_dir` and `current_dir` are both `CARGO_MANIFEST_DIR`. The relative
    /// path `"src/main.rs"` resolves to `<manifest>/src/main.rs`, a *nested*
    /// subdirectory of cwd, and must pass the filter.
    #[test]
    fn filter_results_includes_nested_subdirectory() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let parent_dir = manifest;
        let current_dir = manifest;
        // "src/main.rs" lives in a subdirectory of cwd.
        let results = vec![("src/main.rs".to_string(), 0.9_f32)];
        let out = filter_results(
            results,
            parent_dir,
            current_dir,
            false, // not --all
            10,
        );
        assert_eq!(
            out.len(),
            1,
            "src/main.rs must be INCLUDED: recursive search descends into subdirectories"
        );
    }

    /// With a different `current_dir`, a result under `parent_dir` must be
    /// excluded (boundary test).
    ///
    /// This also guards that paths resolve against `parent_dir`, not cwd: the
    /// relative `"Cargo.toml"` correctly resolves to `<manifest>/Cargo.toml`,
    /// which is NOT under `/tmp` → excluded. A wrong cwd-based join would land it
    /// under `/tmp` and wrongly include it.
    #[test]
    fn filter_results_excludes_when_current_dir_differs() {
        let parent_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // /tmp is distinct from parent_dir; the image should not pass the filter.
        let current_dir = std::path::Path::new("/tmp");
        let results = vec![("Cargo.toml".to_string(), 0.9_f32)];
        let out = filter_results(
            results,
            parent_dir,
            current_dir,
            false, // not --all
            10,
        );
        assert_eq!(
            out.len(),
            0,
            "image under parent_dir must not appear when cwd=/tmp"
        );
    }

    /// The `--all` flag ignores the directory filter entirely.
    #[test]
    fn filter_results_all_bypasses_directory_filter() {
        let parent_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // current_dir is /tmp (unrelated), but --all must include the row anyway.
        let current_dir = std::path::Path::new("/tmp");
        let results = vec![("Cargo.toml".to_string(), 0.9_f32)];
        let out = filter_results(
            results,
            parent_dir,
            current_dir,
            true, // --all
            10,
        );
        assert_eq!(
            out.len(),
            1,
            "--all must include results regardless of the current directory"
        );
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use image::{ImageBuffer, Rgb};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db_path() -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Avoid /tmp: the "tmp" default ignore pattern filters every path whose
        // components contain "tmp", so indexing fixtures placed there would yield
        // zero results.  Prefer XDG_RUNTIME_DIR (/run/user/<uid>), which is a
        // per-user tmpfs on Linux and contains no matching path components.
        let base = std::env::var("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/dev/shm"));
        let dir = base.join(format!("imgfind_fast_idx_{}_{n}", std::process::id()));
        dir.join(".imgfind").join("imgfind.db")
    }

    #[test]
    fn fast_index_writes_rows_without_vectors_or_thumbnails() {
        const N: usize = 3;
        let db_path = temp_db_path();
        let mut db = imgfind::block_on(Database::new(&db_path)).expect("create db");
        // parent_dir is the dir that contains .imgfind/
        let parent_dir = db_path.parent().unwrap().parent().unwrap().to_path_buf();

        // Write N decodable PNGs large enough for oshash (>= 128 KB).
        // Solid-colour images compress to near zero, so use per-pixel variation
        // to defeat PNG's DEFLATE compressor and guarantee file size > 128 KB.
        for i in 0..N {
            let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
                ImageBuffer::from_fn(512, 512, |x, y| {
                    Rgb([
                        ((x.wrapping_mul(y).wrapping_add(i as u32 * 17)) % 256) as u8,
                        ((x.wrapping_add(y).wrapping_add(i as u32 * 31)) % 256) as u8,
                        ((x.wrapping_mul(3).wrapping_add(y.wrapping_mul(7)).wrapping_add(i as u32 * 53)) % 256) as u8,
                    ])
                });
            img.save(parent_dir.join(format!("fixture_{i}.png")))
                .expect("save fixture");
        }

        let dir_str = parent_dir.to_string_lossy().into_owned();
        index_directory(&mut db, &dir_str, true, true, None, false, false)
            .expect("index_directory");

        let total = imgfind::block_on(db.get_image_count()).unwrap();
        assert_eq!(total as usize, N, "row count must equal fixture count");
        assert_eq!(
            imgfind::block_on(db.count_images_without_embedding()).unwrap(),
            N,
            "no embeddings should be generated by fast index"
        );
        assert_eq!(
            imgfind::block_on(db.count_images_without_thumbnails(ThumbnailSize(300))).unwrap(),
            N,
            "no thumbnails should be generated by fast index"
        );

        let _ = std::fs::remove_dir_all(&parent_dir);
    }
}
