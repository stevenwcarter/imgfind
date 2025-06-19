use anyhow::{Context, Result};
use dirs::home_dir;
use std::{fs, path::PathBuf};

pub mod api;
pub mod config;
pub mod context;
pub mod database;
pub mod graphql;
pub mod routes;
pub mod search;
pub mod thumbnail;

pub fn get_db_path() -> Result<PathBuf> {
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

pub fn get_local_db_path() -> Result<PathBuf> {
    // Create .imgfind directory in current directory
    let current_dir = std::env::current_dir()?;
    let imgfind_dir = current_dir.join(".imgfind");
    fs::create_dir_all(&imgfind_dir)?;
    Ok(imgfind_dir.join("imgfind.db"))
}
