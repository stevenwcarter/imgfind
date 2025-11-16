use anyhow::{Context, Result};
use dirs::home_dir;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub mod api;
pub mod config;
pub mod context;
pub mod database;
pub mod graphql;
pub mod logging;
pub mod metadata;
pub mod routes;
pub mod search;
pub mod thumbnail;
pub mod tui;

pub fn get_db_path(dir: Option<&str>) -> Result<PathBuf> {
    // First, try to find existing database by walking up directory tree
    if let Some(dir) = dir {
        let potential_db = Path::new(&dir).join(".imgfind").join("imgfind.db");
        if potential_db.exists() {
            return Ok(potential_db);
        } else {
            panic!("No database found in this directory")
        }
    }

    let mut current_dir = std::env::current_dir().unwrap();

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

pub fn get_local_db_path() -> Result<PathBuf> {
    // Create .imgfind directory in current directory
    let current_dir = std::env::current_dir()?;
    let imgfind_dir = current_dir.join(".imgfind");
    fs::create_dir_all(&imgfind_dir)?;
    Ok(imgfind_dir.join("imgfind.db"))
}

/// Get the parent directory where the database is located
/// This is the directory that contains the .imgfind folder
pub fn get_db_parent_dir(db_path: &Path) -> Result<PathBuf> {
    // The database path is something like /path/to/parent/.imgfind/imgfind.db
    // We want to return /path/to/parent

    // Handle the case where the database file doesn't exist yet
    let imgfind_dir = db_path
        .parent()
        .context("Database path has no parent directory")?;

    // Verify that this is indeed an .imgfind directory
    if imgfind_dir.file_name().and_then(|name| name.to_str()) != Some(".imgfind") {
        return Err(anyhow::anyhow!(
            "Database path is not in expected .imgfind directory structure: {:?}",
            db_path
        ));
    }

    let parent_dir = imgfind_dir
        .parent()
        .context(".imgfind directory has no parent")?;

    Ok(parent_dir.to_path_buf())
}

/// Convert an absolute path to a path relative to the database parent directory
pub fn abs_to_relative_path(abs_path: &Path, db_parent: &Path) -> Result<PathBuf> {
    abs_path
        .strip_prefix(db_parent)
        .map(|p| p.to_path_buf())
        .context("Path is not within database parent directory")
}

/// Convert a relative path (stored in database) to an absolute path
pub fn relative_to_abs_path(rel_path: &Path, db_parent: &Path) -> PathBuf {
    db_parent.join(rel_path)
}
