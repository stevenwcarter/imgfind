//! In-process data backend for the GUI. Wraps the imgfind library: opens the
//! SQLite DB, loads the CLIP embedder in the background, and runs searches /
//! thumbnail loads. No HTTP.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use imgfind::config::SearchConfig;
use imgfind::database::Database;
use imgfind::search::SearchEngine;
use imgfind::thumbnail::get_or_generate_thumbnail;
use imgfind::{RelativePath, get_db_path, relative_to_abs_path};

use crate::state::{PAGE_SIZE, SearchResult};

#[derive(Clone)]
pub struct Backend {
    db: Database,
    embedder: Arc<OnceLock<ClipEmbedder>>,
    parent_dir: PathBuf,
}

impl Backend {
    pub fn open(dir: Option<&str>) -> Result<Backend> {
        let db_path = get_db_path(dir).context("Failed to resolve image database")?;
        let db = Database::new(&db_path).context("Failed to open database")?;
        let parent_dir = db.parent_dir.clone();
        Ok(Backend { db, embedder: Arc::new(OnceLock::new()), parent_dir })
    }

    /// Build the CLIP embedder on a background thread (it can take seconds and
    /// must not block the UI). Mirrors `serve`'s lazy init pattern.
    pub fn start_loading_model(&self) {
        let embedder = Arc::clone(&self.embedder);
        let db = self.db.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<ClipEmbedder> {
                let model_name = db.active_model().context("Failed to resolve active model")?.name;
                ClipEmbedder::from_model(&model_name, false)
                    .context("Failed to load CLIP model")
            })();
            match result {
                Ok(e) => {
                    let _ = embedder.set(e);
                }
                Err(err) => tracing::error!("model load failed: {err:#}"),
            }
        });
    }

    pub fn model_ready(&self) -> bool {
        self.embedder.get().is_some()
    }

    pub fn search(&self, query: &str, offset: usize) -> Result<Vec<SearchResult>> {
        let embedder = self
            .embedder
            .get()
            .context("Embedding model is still loading")?;
        let embedding = embedder
            .get_text_embedding(query)
            .context("Failed to embed query")?;
        let sc = SearchConfig::default();
        let engine = SearchEngine::new(&self.db);
        let rows = engine
            .search_meta(embedding, PAGE_SIZE, offset, sc.distance_threshold, sc.max_k)
            .context("Search failed")?;
        Ok(rows
            .into_iter()
            .map(|(path, distance, file_size)| SearchResult { path, distance, file_size })
            .collect())
    }

    pub fn thumbnail(&self, rel_path: &str, size: u32) -> Result<Vec<u8>> {
        let hash = self
            .db
            .get_image_hash(&RelativePath(PathBuf::from(rel_path)))
            .with_context(|| format!("No hash for {rel_path}"))?;
        get_or_generate_thumbnail(&self.db, rel_path, &hash, size)
            .with_context(|| format!("Failed to load thumbnail for {rel_path}"))
    }

    pub fn abs_path(&self, rel_path: &str) -> PathBuf {
        relative_to_abs_path(std::path::Path::new(rel_path), &self.parent_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db() -> (Database, PathBuf) {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("imgfind_gui_test_{}_{n}", std::process::id()));
        let db_path = root.join(".imgfind").join("imgfind.db");
        let db = Database::new(&db_path).expect("create db");
        (db, root)
    }

    fn backend_with(db: Database) -> Backend {
        let parent_dir = db.parent_dir.clone();
        Backend { db, embedder: Arc::new(OnceLock::new()), parent_dir }
    }

    #[test]
    fn abs_path_joins_relative_onto_parent_dir() {
        let (db, root) = temp_db();
        let parent = db.parent_dir.clone();
        let backend = backend_with(db);
        assert_eq!(backend.abs_path("a/b.jpg"), parent.join("a/b.jpg"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn thumbnail_round_trips_through_relative_path() {
        let (db, root) = temp_db();
        // Insert an image row + a cached 300px thumbnail blob (cache hit, no file I/O).
        {
            let conn = db.pool.get().expect("conn");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
                [],
            )
            .expect("insert image");
        }
        db.insert_thumbnail("h", 300, &[1, 2, 3, 4]).expect("insert thumb");

        let backend = backend_with(db);
        let bytes = backend.thumbnail("a.jpg", 300).expect("thumb");
        assert_eq!(bytes, vec![1, 2, 3, 4]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn model_not_ready_before_load() {
        let (db, root) = temp_db();
        let backend = backend_with(db);
        assert!(!backend.model_ready());
        let _ = std::fs::remove_dir_all(root);
    }
}
