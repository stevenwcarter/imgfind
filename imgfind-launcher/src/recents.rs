use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum number of remembered libraries.
pub(crate) const MAX_RECENTS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub root: PathBuf,
    /// Unix epoch seconds of the last open/index.
    pub last_opened: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Recents {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<RecentEntry>,
}

fn one() -> u32 {
    1
}

impl Recents {
    /// Load from `path`; a missing or corrupt file yields an empty store
    /// (logged) — the launcher must always start.
    pub fn load_from(path: &Path) -> Recents {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("ignoring corrupt {}: {e}", path.display());
                Recents::default()
            }),
            Err(_) => Recents::default(),
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serializing recents")?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    /// Drop entries whose library DB no longer exists.
    pub fn prune_missing(&mut self) {
        self.entries
            .retain(|e| e.root.join(".imgfind").join("imgfind.db").exists());
    }

    /// Record `root` as most-recently-used: canonicalize, dedup, move to front,
    /// cap at `MAX_RECENTS`.
    pub fn record(&mut self, root: &Path, now: u64) {
        let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        self.entries.retain(|e| e.root != key);
        self.entries.insert(
            0,
            RecentEntry {
                root: key,
                last_opened: now,
            },
        );
        self.entries.truncate(MAX_RECENTS);
    }
}

/// Returns `~/.imgfind/recent.json`.
pub fn default_recents_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".imgfind").join("recent.json"))
}

/// Unix epoch seconds now (saturating to 0 before the epoch).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_lib(parent: &std::path::Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        fs::create_dir_all(root.join(".imgfind")).unwrap();
        fs::write(root.join(".imgfind").join("imgfind.db"), b"x").unwrap();
        root
    }

    #[test]
    fn record_moves_to_front_and_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_lib(tmp.path(), "a");
        let b = make_lib(tmp.path(), "b");
        let mut r = Recents::default();
        r.record(&a, 1);
        r.record(&b, 2);
        r.record(&a, 3); // a back to front, no dup
        assert_eq!(r.entries.len(), 2);
        assert_eq!(r.entries[0].root, a.canonicalize().unwrap());
        assert_eq!(r.entries[1].root, b.canonicalize().unwrap());
    }

    #[test]
    fn record_caps_at_max() {
        let tmp = tempfile::tempdir().unwrap();
        let mut r = Recents::default();
        for i in 0..(MAX_RECENTS + 5) {
            let root = make_lib(tmp.path(), &format!("lib{i}"));
            r.record(&root, i as u64);
        }
        assert_eq!(r.entries.len(), MAX_RECENTS);
    }

    #[test]
    fn prune_drops_missing_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_lib(tmp.path(), "a");
        let mut r = Recents::default();
        r.record(&a, 1);
        // Push a bogus entry that does not exist.
        r.entries.push(RecentEntry {
            root: tmp.path().join("gone"),
            last_opened: 2,
        });
        r.prune_missing();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].root, a.canonicalize().unwrap());
    }

    #[test]
    fn load_missing_or_corrupt_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        assert!(Recents::load_from(&missing).entries.is_empty());
        let corrupt = tmp.path().join("bad.json");
        fs::write(&corrupt, b"{ not json").unwrap();
        assert!(Recents::load_from(&corrupt).entries.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_lib(tmp.path(), "a");
        let mut r = Recents::default();
        r.record(&a, 7);
        let path = tmp.path().join("recent.json");
        r.save_to(&path).unwrap();
        let loaded = Recents::load_from(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].last_opened, 7);
    }
}
