//! In-process data backend for the GUI. Wraps the imgfind library: opens the
//! SQLite DB, loads the CLIP embedder in the background, and runs searches /
//! thumbnail loads. No HTTP.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use clipper::ClipEmbedder;
use imgfind::config::SearchConfig;
use imgfind::database::{Database, ImageMetadata, extract_image_metadata};
use imgfind::filters::Filters;
use imgfind::search::SearchEngine;
use imgfind::sort::{RowMeta, Sort};
use imgfind::thumbnail::get_or_generate_thumbnail;
use imgfind::ui_state::UiState;
use imgfind::{AbsolutePath, RelativePath, get_db_path, relative_to_abs_path};

/// Maximum number of ranked results fetched per search/similar query. The
/// full ordered set lives in `SearchState`, but ranked vector search is
/// inherently bounded — the most relevant `SEARCH_LIMIT` rows.
const SEARCH_LIMIT: usize = 80;

/// Derive the lowercased file extension from a path (empty when no dot),
/// matching `Database::browse_all`'s Rust-side derivation.
fn ext_from(path: &str) -> String {
    path.rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

#[derive(Clone)]
pub struct Backend {
    db: Database,
    embedder: Arc<OnceLock<ClipEmbedder>>,
    parent_dir: PathBuf,
}

impl Backend {
    pub fn open(dir: Option<&str>) -> Result<Backend> {
        let db_path = get_db_path(dir).context("Failed to resolve image database")?;
        let db = imgfind::block_on(Database::new(&db_path)).context("Failed to open database")?;
        let parent_dir = db.parent_dir.clone();
        Ok(Backend {
            db,
            embedder: Arc::new(OnceLock::new()),
            parent_dir,
        })
    }

    /// Loads the CLIP model lazily on a background thread so the UI stays
    /// responsive; the embedder is stored in a `OnceLock` once ready.
    pub fn start_loading_model(&self) {
        let embedder = Arc::clone(&self.embedder);
        let db = self.db.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<ClipEmbedder> {
                let model_name = imgfind::block_on(db.active_model())
                    .context("Failed to resolve active model")?
                    .name;
                ClipEmbedder::from_model(&model_name, false).context("Failed to load CLIP model")
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

    /// Text query → ranked [`RowMeta`] in relevance order (distance ascending).
    /// Relevance lives in the Vec ordering; `distance` is not stored.
    pub fn search(&self, query: &str, filters: &Filters) -> Result<Vec<RowMeta>> {
        let embedder = self
            .embedder
            .get()
            .context("Embedding model is still loading")?;
        let embedding = embedder
            .get_text_embedding(query)
            .context("Failed to embed query")?;
        let sc = SearchConfig::default();
        let engine = SearchEngine::new(&self.db);
        let rows = imgfind::block_on(engine.search_meta(
            embedding,
            SEARCH_LIMIT,
            0,
            sc.distance_threshold,
            sc.max_k,
            filters,
        ))
        .context("Search failed")?;
        Ok(rows
            .into_iter()
            .map(|(id, path, _distance, size)| RowMeta {
                ext: ext_from(&path),
                id,
                path,
                size,
            })
            .collect())
    }

    /// Browse the full filtered set in `sort` order (no paging).
    pub fn browse(&self, filters: &Filters, sort: &Sort) -> Result<Vec<RowMeta>> {
        imgfind::block_on(self.db.browse_all(filters, sort)).context("Browse failed")
    }

    /// Fetch [`RowMeta`] for an explicit ordered id list (session restore).
    pub fn rehydrate(&self, ids: &[i64]) -> Result<Vec<RowMeta>> {
        imgfind::block_on(self.db.rehydrate_rows(ids)).context("Rehydrate failed")
    }

    /// Load the persisted GUI session, if any. Returns `Ok(None)` when no row
    /// exists or the stored blob is malformed (callers fall back to a default
    /// browse rather than failing startup).
    pub fn get_ui_state(&self) -> Result<Option<UiState>> {
        imgfind::block_on(self.db.get_ui_state()).context("Failed to read UI state")
    }

    /// Persist the GUI session state (single-row upsert).
    pub fn set_ui_state(&self, st: &UiState) -> Result<()> {
        imgfind::block_on(self.db.set_ui_state(st)).context("Failed to write UI state")
    }

    /// Resolve a stored relative image path back to its DB row id (used to
    /// persist/restore a similarity-search seed by id rather than by path).
    pub fn id_for_rel_path(&self, rel_path: &str) -> Result<i64> {
        let abs = AbsolutePath(self.abs_path(rel_path));
        imgfind::block_on(self.db.get_image_id(&abs))
            .with_context(|| format!("No image id for {rel_path}"))
    }

    pub fn extensions(&self) -> Result<Vec<String>> {
        imgfind::block_on(self.db.distinct_extensions()).context("Failed to list extensions")
    }

    pub fn size_bounds(&self) -> Result<(i64, i64)> {
        imgfind::block_on(self.db.file_size_bounds()).context("Failed to read size bounds")
    }

    pub fn thumbnail(&self, rel_path: &str, size: u32) -> Result<Vec<u8>> {
        let hash = imgfind::block_on(self.db.get_image_hash(&Self::rel(rel_path)))
            .with_context(|| format!("No hash for {rel_path}"))?;
        let abs = self.abs_path(rel_path);
        let abs_str = abs.to_string_lossy();
        get_or_generate_thumbnail(&self.db, &abs_str, &hash, size)
            .with_context(|| format!("Failed to load thumbnail for {rel_path}"))
    }

    pub fn abs_path(&self, rel_path: &str) -> PathBuf {
        relative_to_abs_path(std::path::Path::new(rel_path), &self.parent_dir)
    }

    /// EXIF/metadata for an indexed image.
    ///
    /// Reads the stored `image_metadata` row first (the same fields are persisted
    /// at index time), avoiding a full file decode on the detail-panel critical
    /// path. Falls back to decoding the file only when no stored row exists
    /// (e.g. metadata not yet backfilled).
    pub fn metadata(&self, rel_path: &str) -> Result<ImageMetadata> {
        if let Some(meta) = imgfind::block_on(self.db.get_image_metadata(&Self::rel(rel_path)))
            .with_context(|| format!("Failed to read stored metadata for {rel_path}"))?
        {
            return Ok(meta);
        }
        let abs = self.abs_path(rel_path);
        extract_image_metadata(&abs.to_string_lossy())
            .with_context(|| format!("Failed to read metadata for {rel_path}"))
    }

    /// Assign `tag` to the image at `rel_path` (creates the tag if new).
    pub fn add_tag(&self, rel_path: &str, tag: &str) -> Result<()> {
        imgfind::block_on(self.db.tag_image(&Self::rel(rel_path), tag))
            .with_context(|| format!("add tag {tag} to {rel_path}"))
    }

    /// Remove `tag` from the image at `rel_path`.
    pub fn remove_tag(&self, rel_path: &str, tag: &str) -> Result<()> {
        imgfind::block_on(self.db.untag_image(&Self::rel(rel_path), tag))
            .with_context(|| format!("remove tag {tag} from {rel_path}"))
    }

    /// Attach `tag` to every image in `rel_paths` (batch; single round-trip).
    pub fn batch_add_tags(&self, rel_paths: &[&str], tag: &str) -> Result<()> {
        imgfind::block_on(self.db.batch_tag_images(rel_paths, tag))
            .with_context(|| format!("batch add tag {tag}"))
    }

    /// Remove `tag` from every image in `rel_paths` (batch; single round-trip).
    /// Sibling to `batch_add_tags`; wired into the untag chord path when added.
    #[allow(dead_code)]
    pub fn batch_remove_tags(&self, rel_paths: &[&str], tag: &str) -> Result<()> {
        imgfind::block_on(self.db.batch_untag_images(rel_paths, tag))
            .with_context(|| format!("batch remove tag {tag}"))
    }

    /// Tags currently assigned to the image at `rel_path`.
    pub fn tags_for(&self, rel_path: &str) -> Result<Vec<String>> {
        imgfind::block_on(self.db.tags_for_image(&Self::rel(rel_path)))
            .with_context(|| format!("tags for {rel_path}"))
    }

    /// All tag names in the database (alphabetical).
    #[allow(dead_code)] // consumed by upcoming tag-panel task; not yet wired into main.rs
    pub fn all_tags(&self) -> Result<Vec<String>> {
        imgfind::block_on(self.db.list_tags()).context("list all tags")
    }

    fn rel(p: &str) -> RelativePath {
        RelativePath(PathBuf::from(p))
    }

    /// Images similar to `rel_path`, using its stored embedding. The seed itself
    /// is filtered out of the results.
    pub fn search_similar(&self, rel_path: &str, filters: &Filters) -> Result<Vec<RowMeta>> {
        let sc = SearchConfig::default();
        let rows = imgfind::block_on(self.db.find_similar_to_path(
            &Self::rel(rel_path),
            SEARCH_LIMIT,
            0,
            sc.distance_threshold,
            sc.max_k,
            filters,
        ))
        .with_context(|| format!("Similar search failed for {rel_path}"))?;
        Ok(rows
            .into_iter()
            .filter(|(_, path, _, _)| path != rel_path)
            .map(|(id, path, _distance, size)| RowMeta {
                ext: ext_from(&path),
                id,
                path,
                size,
            })
            .collect())
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
        let db = imgfind::block_on(Database::new(&db_path)).expect("create db");
        (db, root)
    }

    fn backend_with(db: Database) -> Backend {
        let parent_dir = db.parent_dir.clone();
        Backend {
            db,
            embedder: Arc::new(OnceLock::new()),
            parent_dir,
        }
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
        imgfind::block_on(db.insert_images_batch(&[(
            "a.jpg".to_string(),
            "h".to_string(),
            vec![0.0f32; 512],
        )]))
        .expect("insert image");
        imgfind::block_on(db.insert_thumbnail("h", 300, &[1, 2, 3, 4])).expect("insert thumb");

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

    #[test]
    fn search_similar_filters_out_the_seed() {
        let (db, root) = temp_db();

        // Use close vectors (not orthogonal) so both fall within the default
        // distance_threshold of 1.3. a is unit vector along dim 0; b is
        // normalized [1.0, 0.1, 0, ...] giving L2(a,b) ≈ 0.1 << 1.3.
        let mut a = vec![0.0f32; 512];
        a[0] = 1.0;
        let mut b = vec![0.0f32; 512];
        b[0] = 1.0;
        b[1] = 0.1;
        let norm: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        b.iter_mut().for_each(|x| *x /= norm);

        imgfind::block_on(db.insert_images_batch(&[
            ("a.jpg".to_string(), "ha".to_string(), a),
            ("b.jpg".to_string(), "hb".to_string(), b),
        ]))
        .expect("insert images");

        let backend = backend_with(db);

        let results = backend
            .search_similar("a.jpg", &Filters::default())
            .expect("similar");
        let paths: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
        assert!(
            !paths.contains(&"a.jpg"),
            "seed must be filtered out of similar results"
        );
        assert!(paths.contains(&"b.jpg"), "other images should remain");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn backend_browse_applies_filters() {
        use imgfind::filters::{Filters, GpsFilter};

        let (db, root) = temp_db();

        imgfind::block_on(db.insert_images_batch(&[
            ("a.jpg".to_string(), "h1".to_string(), vec![0.0f32; 512]),
            ("b.png".to_string(), "h2".to_string(), vec![0.0f32; 512]),
        ]))
        .expect("insert images");

        // Retrieve auto-assigned IDs so we can attach metadata.
        let id_a = imgfind::block_on(db.get_image_id(&AbsolutePath(db.parent_dir.join("a.jpg"))))
            .expect("id for a.jpg");
        let id_b = imgfind::block_on(db.get_image_id(&AbsolutePath(db.parent_dir.join("b.png"))))
            .expect("id for b.png");

        imgfind::block_on(db.insert_or_update_metadata(
            id_a,
            &ImageMetadata {
                file_size: Some(1000),
                width: None,
                height: None,
                latitude: Some(1.0),
                longitude: Some(1.0),
                camera_make: None,
                camera_model: None,
                datetime_taken: None,
            },
        ))
        .expect("insert metadata a");
        imgfind::block_on(db.insert_or_update_metadata(
            id_b,
            &ImageMetadata {
                file_size: Some(50),
                width: None,
                height: None,
                latitude: None,
                longitude: None,
                camera_make: None,
                camera_model: None,
                datetime_taken: None,
            },
        ))
        .expect("insert metadata b");

        let backend = backend_with(db);

        let jpg = backend
            .browse(
                &Filters {
                    extensions: vec!["jpg".into()],
                    ..Default::default()
                },
                &Sort::default(),
            )
            .unwrap();
        assert_eq!(jpg.len(), 1);
        assert_eq!(jpg[0].path, "a.jpg");

        let gps = backend
            .browse(
                &Filters {
                    gps: GpsFilter::HasGps,
                    ..Default::default()
                },
                &Sort::default(),
            )
            .unwrap();
        assert_eq!(gps.len(), 1);
        assert_eq!(gps[0].path, "a.jpg");

        let _ = std::fs::remove_dir_all(root);
    }

    /// `metadata` must read the stored `image_metadata` row, not decode the
    /// file. We insert image + metadata rows but never create a file on disk;
    /// the old decode-based path would fail (no file) — DB-first must succeed.
    #[test]
    fn metadata_reads_stored_row_without_touching_disk() {
        let (db, root) = temp_db();

        imgfind::block_on(db.insert_images_batch(&[(
            "ghost.jpg".to_string(),
            "h".to_string(),
            vec![0.0f32; 512],
        )]))
        .expect("insert image");

        let id = imgfind::block_on(db.get_image_id(&AbsolutePath(db.parent_dir.join("ghost.jpg"))))
            .expect("id for ghost.jpg");

        imgfind::block_on(db.insert_or_update_metadata(
            id,
            &ImageMetadata {
                file_size: Some(2048),
                width: Some(1024),
                height: Some(768),
                latitude: Some(5.5),
                longitude: Some(6.5),
                camera_make: None,
                camera_model: None,
                datetime_taken: None,
            },
        ))
        .expect("insert metadata");

        let backend = backend_with(db);
        // No file at <parent_dir>/ghost.jpg exists; the decode fallback would fail.
        let meta = backend.metadata("ghost.jpg").expect("stored metadata");
        assert_eq!(meta.file_size, Some(2048));
        assert_eq!(meta.width, Some(1024));
        assert_eq!(meta.height, Some(768));
        assert_eq!(meta.latitude, Some(5.5));
        assert_eq!(meta.longitude, Some(6.5));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn add_list_remove_tag_roundtrip() {
        let (db, root) = temp_db();
        imgfind::block_on(db.insert_images_batch(&[(
            "a.jpg".to_string(),
            "h".to_string(),
            vec![0.0f32; 512],
        )]))
        .expect("insert image");

        let backend = backend_with(db);
        backend.add_tag("a.jpg", "beach").unwrap();
        backend.add_tag("a.jpg", "sunset").unwrap();
        let mut tags = backend.tags_for("a.jpg").unwrap();
        tags.sort();
        assert_eq!(tags, vec!["beach".to_string(), "sunset".to_string()]);
        backend.remove_tag("a.jpg", "beach").unwrap();
        assert_eq!(
            backend.tags_for("a.jpg").unwrap(),
            vec!["sunset".to_string()]
        );
        assert!(backend.all_tags().unwrap().contains(&"sunset".to_string()));
        let _ = std::fs::remove_dir_all(root);
    }

    /// Confirms that a cache-miss falls through to `generate_thumbnail_bytes`,
    /// which must receive the *absolute* path — not `rel_path` relative to the
    /// process cwd.  If `thumbnail` passed the relative path this test would
    /// fail to open the image because the test cwd differs from `parent_dir`.
    #[test]
    fn thumbnail_cache_miss_uses_absolute_path() {
        use image::{ImageBuffer, Rgb};

        let (db, root) = temp_db();

        // Write a real, decodable 8×8 RGB PNG at <parent_dir>/pic.png.
        // RGB8 (not RGBA8) because generate_thumbnail_bytes re-encodes as JPEG,
        // and the JPEG encoder does not support an alpha channel.
        let img_path = db.parent_dir.join("pic.png");
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(8, 8, Rgb([255, 0, 128]));
        img.save(&img_path).expect("save test image");

        // Insert the image row (no cached thumbnail → cache miss guaranteed).
        imgfind::block_on(db.insert_images_batch(&[(
            "pic.png".to_string(),
            "h2".to_string(),
            vec![0.0f32; 512],
        )]))
        .expect("insert image");

        let backend = backend_with(db);
        let bytes = backend
            .thumbnail("pic.png", 64)
            .expect("thumbnail from abs path");
        assert!(
            !bytes.is_empty(),
            "thumbnail bytes must be non-empty on cache miss"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
