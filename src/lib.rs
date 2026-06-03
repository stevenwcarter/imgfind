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
pub mod indexing;
pub mod logging;
pub mod metadata;
pub mod models;
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
            return Err(anyhow::anyhow!("No database found in directory: {dir}"));
        }
    }

    let mut current_dir = std::env::current_dir().context("Failed to get current directory")?;

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

/// A path relative to the database parent directory — i.e. the form stored in
/// the `images.path` column. Wrapping it in a newtype makes "is this relative or
/// absolute?" a compile-time distinction at DB boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativePath(pub PathBuf);

/// An absolute filesystem path — the form callers use to touch the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbsolutePath(pub PathBuf);

impl RelativePath {
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.0.to_string_lossy()
    }

    /// Resolve this relative path against `base` (the database parent directory).
    pub fn to_absolute(&self, base: &Path) -> AbsolutePath {
        AbsolutePath(base.join(&self.0))
    }
}

impl AbsolutePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.0.to_string_lossy()
    }

    /// Strip `base` (the database parent directory) to produce the stored form.
    /// Errors if this path is not within `base`.
    pub fn to_relative(&self, base: &Path) -> Result<RelativePath> {
        self.0
            .strip_prefix(base)
            .map(|p| RelativePath(p.to_path_buf()))
            .context("Path is not within database parent directory")
    }
}

/// Convert an absolute path to a path relative to the database parent directory
pub fn abs_to_relative_path(abs_path: &Path, db_parent: &Path) -> Result<PathBuf> {
    AbsolutePath(abs_path.to_path_buf())
        .to_relative(db_parent)
        .map(|r| r.0)
}

/// Convert a relative path (stored in database) to an absolute path
pub fn relative_to_abs_path(rel_path: &Path, db_parent: &Path) -> PathBuf {
    RelativePath(rel_path.to_path_buf())
        .to_absolute(db_parent)
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_abs_roundtrip_within_base() {
        let base = Path::new("/data");
        let abs = AbsolutePath(PathBuf::from("/data/sub/a.jpg"));
        let rel = abs.to_relative(base).unwrap();
        assert_eq!(rel.0, PathBuf::from("sub/a.jpg"));
        assert_eq!(rel.to_absolute(base).0, abs.0);
    }

    #[test]
    fn to_relative_errors_outside_base() {
        let base = Path::new("/data");
        let abs = AbsolutePath(PathBuf::from("/other/a.jpg"));
        assert!(abs.to_relative(base).is_err());
    }
}
