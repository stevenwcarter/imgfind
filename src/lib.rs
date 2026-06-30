use anyhow::{Context, Result};
use dirs::home_dir;
use std::{
    ffi::OsString,
    fs,
    future::Future,
    path::{Path, PathBuf},
    sync::LazyLock,
};

pub mod colors;
pub mod db_pool;

static DB_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build shared DB tokio runtime")
});

/// Run `fut` to completion on the shared multi-thread runtime.
///
/// For **sync callers only** (CLI, GUI worker threads, the thumbnail writer).
/// Panics if called from inside an existing tokio runtime — async callers must
/// `.await` directly instead of going through this bridge.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    DB_RUNTIME.block_on(fut)
}
pub mod config;
pub mod database;
pub mod decode;
pub mod edits;
pub mod filters;
pub mod hashing;
pub mod ids;
pub mod indexing;
pub mod logging;
pub mod metadata;
pub mod models;
pub mod processing;
pub mod schema;
pub mod search;
pub mod sort;
pub mod thumbnail;
#[cfg(feature = "tui")]
pub mod tui;
pub mod ui_state;
pub mod units;
pub mod vector_sql;

pub use ids::{CollectionId, ImageId, TagId};
pub use units::{
    DistanceThreshold, EmbeddingDim, FileSize, GeoRect, MaxK, ThumbnailSize, ThumbnailSpec,
};

/// Walk up from `start` (inclusive) and return the first directory that
/// contains a `.imgfind/imgfind.db`, or `None` if no ancestor does. Pure
/// lookup: never creates anything and never falls back to `~/.imgfind`.
pub fn find_db_root_upward(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        if dir.join(".imgfind").join("imgfind.db").exists() {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => return None,
        }
    }
}

/// Resolve a sibling executable: prefer one next to the current executable
/// (the `install.sh` / cargo target layout), else fall back to the bare name
/// for a `PATH` lookup. Used to spawn `imgfind` / `imgfind-gui` from sibling
/// binaries (e.g. the launcher, `imgfind gui`).
pub fn resolve_sibling_binary(name: &str) -> OsString {
    sibling_binary_from(std::env::current_exe().ok().as_deref(), name, |p| {
        p.exists()
    })
}

fn sibling_binary_from(
    current_exe: Option<&Path>,
    name: &str,
    exists: impl Fn(&Path) -> bool,
) -> OsString {
    if let Some(exe) = current_exe {
        let cand = exe.with_file_name(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if exists(&cand) {
            return cand.into_os_string();
        }
    }
    OsString::from(name)
}

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

    let current_dir = std::env::current_dir().context("Failed to get current directory")?;
    if let Some(root) = find_db_root_upward(&current_dir) {
        return Ok(root.join(".imgfind").join("imgfind.db"));
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

    /// Like [`to_absolute`](Self::to_absolute), but rejects a stored path that
    /// would escape `base` before resolving it. The write side already keeps
    /// stored paths within the library root; this is defense-in-depth for a
    /// corrupted or hand-edited DB whose `images.path` contains `..` or an
    /// absolute component, which `Path::join` would otherwise honour and let
    /// escape the library root (e.g. when opened in the OS viewer).
    pub fn try_to_absolute(&self, base: &Path) -> Result<AbsolutePath> {
        use std::path::Component;
        if self
            .0
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
        {
            return Err(anyhow::anyhow!(
                "Stored relative path escapes the library root: {:?}",
                self.0
            ));
        }
        Ok(self.to_absolute(base))
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
    use std::fs;

    #[test]
    fn find_db_root_upward_finds_at_start() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        fs::create_dir_all(root.join(".imgfind")).unwrap();
        fs::write(root.join(".imgfind").join("imgfind.db"), b"x").unwrap();
        assert_eq!(find_db_root_upward(&root), Some(root.clone()));
    }

    #[test]
    fn find_db_root_upward_finds_at_ancestor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib");
        let deep = root.join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir_all(root.join(".imgfind")).unwrap();
        fs::write(root.join(".imgfind").join("imgfind.db"), b"x").unwrap();
        assert_eq!(find_db_root_upward(&deep), Some(root.clone()));
    }

    #[test]
    fn find_db_root_upward_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_db_root_upward(&deep), None);
    }

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

    #[test]
    fn try_to_absolute_rejects_parent_escape() {
        let base = Path::new("/data");
        let rel = RelativePath(PathBuf::from("../../etc/passwd"));
        assert!(rel.try_to_absolute(base).is_err());
    }

    #[test]
    fn try_to_absolute_ok_for_normal_relative() {
        let base = Path::new("/data");
        let rel = RelativePath(PathBuf::from("sub/img.jpg"));
        let abs = rel.try_to_absolute(base).unwrap();
        assert_eq!(abs.0, PathBuf::from("/data/sub/img.jpg"));
    }

    #[test]
    fn sibling_binary_prefers_existing_sibling() {
        let exe = PathBuf::from("/opt/app/imgfind");
        let got = sibling_binary_from(Some(&exe), "imgfind-gui", |_p| true);
        let want = PathBuf::from(format!(
            "/opt/app/imgfind-gui{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert_eq!(got, want.into_os_string());
    }

    #[test]
    fn sibling_binary_falls_back_to_bare_name_when_missing() {
        let exe = PathBuf::from("/opt/app/imgfind");
        let got = sibling_binary_from(Some(&exe), "imgfind-gui", |_p| false);
        assert_eq!(got, std::ffi::OsString::from("imgfind-gui"));
    }

    #[test]
    fn sibling_binary_bare_name_when_no_current_exe() {
        let got = sibling_binary_from(None, "imgfind-gui", |_p| true);
        assert_eq!(got, std::ffi::OsString::from("imgfind-gui"));
    }
}
