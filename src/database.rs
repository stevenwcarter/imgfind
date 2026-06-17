use crate::{AbsolutePath, RelativePath, get_db_parent_dir};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use hashbrown::HashMap;
use image::GenericImageView;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, ffi::sqlite3_auto_extension, params};
use sqlite_vec::sqlite3_vec_init;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tracing::info;
use zerocopy::IntoBytes;

#[derive(Debug, Clone)]
pub struct Database {
    pub pool: Pool<SqliteConnectionManager>,
    pub parent_dir: PathBuf,
}

const MAX_JITTER: f64 = 0.000001;

pub type ImageSearchResult = Result<Vec<(String, f32, Option<Vec<u8>>)>>;

impl Database {
    pub fn new(db_path: &Path) -> Result<Self> {
        // Initialize sqlite-vec extension
        unsafe {
            sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(sqlite3_vec_init as *const ())));
        }

        let parent_path = db_path.parent().context("DB path has no parent")?;
        std::fs::create_dir_all(parent_path).context("Failed to create DB parent directory")?;

        // Enable foreign keys on every connection the pool hands out. PRAGMAs are
        // per-connection, so setting this once on a single connection would leave
        // FK enforcement (and the ON DELETE CASCADE on image_metadata) off for the
        // rest of the pool.
        let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
            conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
        });
        let max_size = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .min(32) as u32;
        let pool = r2d2::Pool::builder()
            .max_size(max_size)
            .build(manager)
            .with_context(|| format!("Failed to open database at {:?}", db_path))?;

        let parent_dir = get_db_parent_dir(db_path)?;
        let db = Database { pool, parent_dir };
        let conn = db.pool.get().context("Failed to get DB connection")?;
        run_migrations(&conn)?;
        Ok(db)
    }

    /// Truncate the WAL back into the main DB file. Call after a large write batch (e.g. indexing).
    pub fn checkpoint_wal(&self) -> Result<()> {
        let conn = self
            .pool
            .get()
            .context("get connection for WAL checkpoint")?;
        conn.pragma_update(None, "wal_checkpoint", "RESTART")
            .context("wal_checkpoint(RESTART)")?;
        Ok(())
    }
}

const LATEST_MIGRATION_VERSION: i32 = 2;

/// Apply any pending schema migrations, gated by SQLite's `PRAGMA user_version`.
///
/// Each migration bumps `user_version` after it succeeds, so re-running is a no-op
/// once a DB is at `LATEST_MIGRATION_VERSION`. Existing DBs created before this
/// runner existed start at version 0; migration 1 is the full baseline schema with
/// `IF NOT EXISTS` everywhere, so they adopt it as a no-op and get stamped to 1.
fn run_migrations(conn: &Connection) -> Result<()> {
    let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if current < 1 {
        migration_001_baseline(conn).context("migration 1 (baseline schema)")?;
    }
    if current < 2 {
        migration_002_models_and_userdata(conn).context("migration 2")?;
    }
    if current < LATEST_MIGRATION_VERSION {
        conn.execute_batch(&format!(
            "PRAGMA user_version = {LATEST_MIGRATION_VERSION};"
        ))?;
    }
    Ok(())
}

/// Migration 2: a `models` registry (which vec0 table is active for embeddings)
/// plus user-data tables for tags and collections.
fn migration_002_models_and_userdata(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS models (name TEXT PRIMARY KEY, dim INTEGER NOT NULL, table_name TEXT NOT NULL, is_active INTEGER NOT NULL DEFAULT 0, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
         INSERT OR IGNORE INTO models (name, dim, table_name, is_active) VALUES ('openai/clip-vit-base-patch32', 512, 'image_vectors', 1);
         CREATE TABLE IF NOT EXISTS tags (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
         CREATE TABLE IF NOT EXISTS image_tags (image_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, PRIMARY KEY(image_id, tag_id), FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE);
         CREATE TABLE IF NOT EXISTS collections (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
         CREATE TABLE IF NOT EXISTS collection_images (collection_id INTEGER NOT NULL, image_id INTEGER NOT NULL, PRIMARY KEY(collection_id, image_id), FOREIGN KEY(collection_id) REFERENCES collections(id) ON DELETE CASCADE, FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE);",
    )?;
    Ok(())
}

/// Migration 1: the full baseline schema (verbatim from the original `initialize_schema`).
fn migration_001_baseline(conn: &Connection) -> Result<()> {
    // Create images table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS images (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT UNIQUE NOT NULL,
                hash TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
        [],
    )?;

    // Create vector table for embeddings using sqlite-vec
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS image_vectors USING vec0(
                embedding float[512]
            )",
        [],
    )?;

    // Create index on path for faster lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_images_path ON images(path)",
        [],
    )?;

    // Create index on hash for faster duplicate detection
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_images_hash ON images(hash)",
        [],
    )?;

    // Create thumbnails table for caching resized images
    conn.execute(
        "CREATE TABLE IF NOT EXISTS thumbnails (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                image_hash TEXT NOT NULL,
                size INTEGER NOT NULL,
                thumbnail_data BLOB NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(image_hash, size)
            )",
        [],
    )?;

    // Create index on hash and size for faster thumbnail lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_thumbnails_hash_size ON thumbnails(image_hash, size)",
        [],
    )?;

    // Create image_metadata table for storing EXIF data
    conn.execute(
        "CREATE TABLE IF NOT EXISTS image_metadata (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                image_id INTEGER NOT NULL,
                file_size INTEGER,
                width INTEGER,
                height INTEGER,
                latitude REAL,
                longitude REAL,
                camera_make TEXT,
                camera_model TEXT,
                datetime_taken DATETIME,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE,
                UNIQUE(image_id)
            )",
        [],
    )?;

    // Create index on image_id for faster metadata lookups
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_image_id ON image_metadata(image_id)",
        [],
    )?;

    // Create index on GPS coordinates for location-based queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_gps ON image_metadata(latitude, longitude)",
        [],
    )?;

    // Composite index covering geo + time for map queries ordered by capture time
    conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_metadata_geo_time ON image_metadata(latitude, longitude, datetime_taken)",
            [],
        )?;

    // Composite index for camera-model filters ordered by capture time
    conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_metadata_camera_time ON image_metadata(camera_model, datetime_taken)",
            [],
        )?;

    // Partial index over capture time, skipping rows without a datetime
    conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_metadata_datetime ON image_metadata(datetime_taken) WHERE datetime_taken IS NOT NULL",
            [],
        )?;

    // Create favorites table for marking favorite images
    conn.execute(
        "CREATE TABLE IF NOT EXISTS favorites (
                image_id INTEGER PRIMARY KEY,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE
            )",
        [],
    )?;

    Ok(())
}

/// Metadata for an embedding model registered in the `models` table: its name,
/// embedding dimensionality, and the vec0 virtual table holding its vectors.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub dim: usize,
    pub table: String,
}

/// Derive a safe vec0 table name from a model name, lowercasing ASCII
/// alphanumerics and replacing everything else with `_`.
fn sanitize_model_table(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("image_vectors_{s}")
}

impl Database {
    /// The currently active embedding model (the single `models` row with
    /// `is_active = 1`).
    pub fn active_model(&self) -> Result<ModelInfo> {
        let conn = self.pool.get().context("get connection")?;
        let (name, dim, table): (String, i64, String) = conn
            .query_row(
                "SELECT name, dim, table_name FROM models WHERE is_active = 1 LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .context("no active model")?;
        Ok(ModelInfo {
            name,
            dim: dim as usize,
            table,
        })
    }

    /// The active model's vec0 table name, validated as a safe SQL identifier
    /// before it is interpolated into queries.
    fn vectors_table(&self) -> Result<String> {
        let t = self.active_model()?.table;
        anyhow::ensure!(
            t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid table name"
        );
        Ok(t)
    }

    /// Register a new embedding model and create its (inactive) vec0 table.
    pub fn register_model(&self, name: &str, dim: usize) -> Result<()> {
        let table = sanitize_model_table(name);
        let conn = self.pool.get().context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO models (name, dim, table_name, is_active) VALUES (?1, ?2, ?3, 0)",
            params![name, dim as i64, table],
        )?;
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS {table} USING vec0(embedding float[{dim}]);"
        ))?;
        Ok(())
    }

    /// Flip the active model to `name`, deactivating all others.
    pub fn set_active_model(&self, name: &str) -> Result<()> {
        let conn = self.pool.get().context("get connection")?;
        let exists: bool = conn
            .query_row("SELECT 1 FROM models WHERE name=?1", [name], |_| Ok(()))
            .optional()?
            .is_some();
        anyhow::ensure!(exists, "unknown model: {name}");
        conn.execute("UPDATE models SET is_active = (name = ?1)", [name])?;
        Ok(())
    }

    /// List all registered models as `(name, dim, is_active)`, ordered by name.
    pub fn list_models(&self) -> Result<Vec<(String, usize, bool)>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare("SELECT name, dim, is_active FROM models ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)? as usize,
                r.get::<_, i64>(2)? != 0,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn insert_image(
        &mut self,
        path: &AbsolutePath,
        hash: &str,
        embedding: &[f32],
    ) -> Result<()> {
        // Convert absolute path to relative path for storage
        let rel_path = path.to_relative(&self.parent_dir).with_context(|| {
            format!("Failed to convert path {} to relative path", path.as_str())
        })?;
        let rel_path_str = rel_path.as_str();
        let vt = self.vectors_table()?;

        // Start a transaction for consistency
        {
            let mut conn = self
                .pool
                .get()
                .context("Failed to get DB connection to insert image")?;
            let tx = conn.transaction()?;

            // Insert into images table with relative path
            tx.execute(
                // Upsert that PRESERVES the row id on a path conflict. `INSERT OR
                // REPLACE` would delete+reinsert, allocating a new id and thereby
                // orphaning this image's embeddings in *other* models' vector
                // tables (which key on the id). `ON CONFLICT … DO UPDATE` keeps the
                // id stable so per-model embeddings stay linked across re-indexing.
                "INSERT INTO images (path, hash) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET hash = excluded.hash",
                params![rel_path_str.as_ref(), hash],
            )?;

            // Get the image ID
            let image_id: i64 = tx.query_row(
                "SELECT id FROM images WHERE path = ?1",
                params![rel_path_str.as_ref()],
                |row| row.get(0),
            )?;

            // Insert into vector table using sqlite-vec
            // First delete any existing vector for this image
            tx.execute(
                &format!("DELETE FROM {vt} WHERE rowid = ?1"),
                params![image_id],
            )?;

            // Insert the new vector using zerocopy for efficiency
            tx.execute(
                &format!("INSERT INTO {vt} (rowid, embedding) VALUES (?1, ?2)"),
                params![image_id, embedding.as_bytes()],
            )?;

            tx.commit()?;
        }
        Ok(())
    }

    /// Toggle the favorite flag for the image at `relative_path`, returning the
    /// new state (`true` = now favorited, `false` = now un-favorited).
    pub fn toggle_favorite(&self, relative_path: &RelativePath) -> Result<bool> {
        let relative_path = relative_path.as_str();
        let conn = self.pool.get().context("get connection")?;
        let image_id: i64 = conn
            .query_row(
                "SELECT id FROM images WHERE path = ?1",
                [relative_path.as_ref()],
                |r| r.get(0),
            )
            .with_context(|| format!("no indexed image at {relative_path}"))?;
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM favorites WHERE image_id = ?1",
                [image_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            conn.execute("DELETE FROM favorites WHERE image_id = ?1", [image_id])?;
            Ok(false)
        } else {
            conn.execute("INSERT INTO favorites (image_id) VALUES (?1)", [image_id])?;
            Ok(true)
        }
    }

    /// Return whether the image at `relative_path` is favorited. Unknown paths
    /// are not favorites.
    pub fn is_favorite(&self, relative_path: &RelativePath) -> Result<bool> {
        let relative_path = relative_path.as_str();
        let conn = self.pool.get().context("get connection")?;
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM images WHERE path = ?1",
                [relative_path.as_ref()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(image_id) = id else {
            return Ok(false);
        };
        Ok(conn
            .query_row(
                "SELECT 1 FROM favorites WHERE image_id = ?1",
                [image_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// List the relative paths of all favorited images, most recently favorited
    /// first.
    pub fn list_favorites(&self) -> Result<Vec<RelativePath>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare(
            "SELECT i.path FROM favorites f JOIN images i ON i.id = f.image_id ORDER BY f.created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(RelativePath(PathBuf::from(r?))))
            .collect::<Result<Vec<_>>>()
    }

    /// Resolve the `images.id` for a stored relative path, erroring if the image
    /// is not indexed. Used by the tag/collection write methods.
    fn image_id_for(&self, relative_path: &RelativePath) -> Result<i64> {
        let relative_path = relative_path.as_str();
        let conn = self.pool.get().context("get connection")?;
        conn.query_row(
            "SELECT id FROM images WHERE path = ?1",
            [relative_path.as_ref()],
            |r| r.get(0),
        )
        .with_context(|| format!("no indexed image at {relative_path}"))
    }

    /// Ensure a tag exists, returning its id.
    pub fn create_tag(&self, name: &str) -> Result<i64> {
        let conn = self.pool.get().context("get connection")?;
        conn.execute("INSERT OR IGNORE INTO tags (name) VALUES (?1)", [name])?;
        Ok(conn.query_row("SELECT id FROM tags WHERE name = ?1", [name], |r| r.get(0))?)
    }

    /// Attach `tag` to the image at `relative_path` (creating the tag if needed).
    pub fn tag_image(&self, relative_path: &RelativePath, tag: &str) -> Result<()> {
        let image_id = self.image_id_for(relative_path)?;
        let tag_id = self.create_tag(tag)?;
        let conn = self.pool.get().context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
            params![image_id, tag_id],
        )?;
        Ok(())
    }

    /// Remove `tag` from the image at `relative_path`.
    pub fn untag_image(&self, relative_path: &RelativePath, tag: &str) -> Result<()> {
        let image_id = self.image_id_for(relative_path)?;
        let conn = self.pool.get().context("get connection")?;
        conn.execute(
            "DELETE FROM image_tags WHERE image_id = ?1 AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
            params![image_id, tag],
        )?;
        Ok(())
    }

    /// List the tags attached to the image at `relative_path`, alphabetically.
    /// Unknown paths simply have no tags.
    pub fn tags_for_image(&self, relative_path: &RelativePath) -> Result<Vec<String>> {
        let relative_path = relative_path.as_str();
        let conn = self.pool.get().context("get connection")?;
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM images WHERE path = ?1",
                [relative_path.as_ref()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(image_id) = id else {
            return Ok(Vec::new());
        };
        let mut stmt = conn.prepare(
            "SELECT t.name FROM image_tags it JOIN tags t ON t.id = it.tag_id WHERE it.image_id = ?1 ORDER BY t.name",
        )?;
        let rows = stmt.query_map([image_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// List all tag names, alphabetically.
    pub fn list_tags(&self) -> Result<Vec<String>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare("SELECT name FROM tags ORDER BY name")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// List the relative paths of images carrying `name`, ordered by path.
    pub fn images_by_tag(&self, name: &str) -> Result<Vec<RelativePath>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare(
            "SELECT i.path FROM image_tags it JOIN tags t ON t.id = it.tag_id JOIN images i ON i.id = it.image_id WHERE t.name = ?1 ORDER BY i.path",
        )?;
        let rows = stmt.query_map([name], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(RelativePath(PathBuf::from(r?))))
            .collect::<Result<Vec<_>>>()
    }

    /// Ensure a collection exists, returning its id.
    pub fn create_collection(&self, name: &str) -> Result<i64> {
        let conn = self.pool.get().context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO collections (name) VALUES (?1)",
            [name],
        )?;
        Ok(
            conn.query_row("SELECT id FROM collections WHERE name = ?1", [name], |r| {
                r.get(0)
            })?,
        )
    }

    /// Add the image at `relative_path` to `name` (creating the collection if needed).
    pub fn add_to_collection(&self, name: &str, relative_path: &RelativePath) -> Result<()> {
        let collection_id = self.create_collection(name)?;
        let image_id = self.image_id_for(relative_path)?;
        let conn = self.pool.get().context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO collection_images (collection_id, image_id) VALUES (?1, ?2)",
            params![collection_id, image_id],
        )?;
        Ok(())
    }

    /// Remove the image at `relative_path` from `name`.
    pub fn remove_from_collection(&self, name: &str, relative_path: &RelativePath) -> Result<()> {
        let image_id = self.image_id_for(relative_path)?;
        let conn = self.pool.get().context("get connection")?;
        conn.execute(
            "DELETE FROM collection_images WHERE image_id = ?1 AND collection_id = (SELECT id FROM collections WHERE name = ?2)",
            params![image_id, name],
        )?;
        Ok(())
    }

    /// List the relative paths of images in `name`, ordered by path.
    pub fn collection_images(&self, name: &str) -> Result<Vec<RelativePath>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare(
            "SELECT i.path FROM collection_images ci JOIN collections c ON c.id = ci.collection_id JOIN images i ON i.id = ci.image_id WHERE c.name = ?1 ORDER BY i.path",
        )?;
        let rows = stmt.query_map([name], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(RelativePath(PathBuf::from(r?))))
            .collect::<Result<Vec<_>>>()
    }

    /// List all collection names, alphabetically.
    pub fn list_collections(&self) -> Result<Vec<String>> {
        let conn = self.pool.get().context("get connection")?;
        let mut stmt = conn.prepare("SELECT name FROM collections ORDER BY name")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Insert many (relative_path, hash, normalized_embedding) rows in one transaction.
    ///
    /// Paths are expected to already be relative to `parent_dir` (matching the
    /// storage invariant). Replicates `insert_image`'s per-row writes (an
    /// `images` row plus the corresponding `image_vectors` vec0 row) against a
    /// single transaction.
    pub fn insert_images_batch(&mut self, rows: &[(String, String, Vec<f32>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let vt = self.vectors_table()?;
        let mut conn = self.pool.get().context("get connection for batch insert")?;
        let tx = conn.transaction()?;
        for (rel_path_str, hash, embedding) in rows {
            // Insert into images table with relative path
            tx.execute(
                // Upsert that PRESERVES the row id on a path conflict. `INSERT OR
                // REPLACE` would delete+reinsert, allocating a new id and thereby
                // orphaning this image's embeddings in *other* models' vector
                // tables (which key on the id). `ON CONFLICT … DO UPDATE` keeps the
                // id stable so per-model embeddings stay linked across re-indexing.
                "INSERT INTO images (path, hash) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET hash = excluded.hash",
                params![rel_path_str.as_str(), hash],
            )?;

            // Get the image ID
            let image_id: i64 = tx.query_row(
                "SELECT id FROM images WHERE path = ?1",
                params![rel_path_str.as_str()],
                |row| row.get(0),
            )?;

            // Insert into vector table using sqlite-vec.
            // First delete any existing vector for this image.
            tx.execute(
                &format!("DELETE FROM {vt} WHERE rowid = ?1"),
                params![image_id],
            )?;

            // Insert the new vector using zerocopy for efficiency.
            tx.execute(
                &format!("INSERT INTO {vt} (rowid, embedding) VALUES (?1, ?2)"),
                params![image_id, embedding.as_slice().as_bytes()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether `path`/`hash` is indexed **for the active model**.
    ///
    /// Model-aware: an image counts as indexed only if it is present in the
    /// shared `images` table *and* has an embedding row in the active model's
    /// vector table. Because each model stores embeddings in its own `vec0`
    /// table, switching the active model leaves existing `images` rows without a
    /// vector in the new table — so they are reported as not-indexed and get
    /// re-embedded on the next `index` run (backfilling the new model).
    pub fn is_image_indexed(&self, path: &AbsolutePath, hash: &str) -> Result<bool> {
        // Convert absolute path to relative path for database lookup
        let rel_path = path.to_relative(&self.parent_dir).with_context(|| {
            format!("Failed to convert path {} to relative path", path.as_str())
        })?;
        let rel_path_str = rel_path.as_str();
        let vt = self.vectors_table()?;

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to check if image is indexed")?;
        let sql = format!(
            "SELECT COUNT(*) FROM images i \
             WHERE i.path = ?1 AND i.hash = ?2 \
               AND EXISTS (SELECT 1 FROM {vt} WHERE rowid = i.id)"
        );

        let count: i64 =
            conn.query_row(&sql, params![rel_path_str.as_ref(), hash], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Search for similar images using sqlite-vec
    pub fn search_similar_images(
        &self,
        query_embedding: &[f32],
        limit: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> Result<Vec<(String, f32)>> {
        // Use a reasonable k value that's at least the limit but not too large
        let k = limit.clamp(1, max_k);
        let vt = self.vectors_table()?;

        // `k` and the distance threshold are interpolated as the vec0 MATCH/k
        // syntax requires literal values (not bound params). Both are trusted
        // numeric config values, never user free-text.
        let query = format!(
            "SELECT i.path, distance
             FROM {vt} v
             JOIN images i ON i.id = v.rowid
             WHERE v.embedding MATCH ? AND k={k}
            AND distance <= {distance_threshold:.6}
             ORDER BY distance LIMIT {k}"
        );

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for searching similar images")?;
        let mut stmt = conn.prepare(&query)?;

        let results = stmt.query_map(params![query_embedding.as_bytes()], |row| {
            let rel_path: String = row.get(0)?;
            let distance: f32 = row.get(1)?;

            Ok((rel_path, distance))
        })?;

        let mut search_results = Vec::new();
        for result in results {
            search_results.push(result?);
        }

        Ok(search_results)
    }

    pub fn search_similar_images_with_raw_blob(
        &self,
        query_embedding: &[f32],
        limit: usize,
        offset: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> ImageSearchResult {
        // Use a reasonable k value that's at least the limit but not too large
        let k = limit.clamp(1, max_k);
        let vt = self.vectors_table()?;

        // `k` and the distance threshold are interpolated as the vec0 MATCH/k
        // syntax requires literal values (not bound params). Both are trusted
        // numeric config values, never user free-text.
        let query = format!(
            "SELECT i.path, distance, t.thumbnail_data
              FROM {vt} v
              JOIN images i ON i.id = v.rowid
              LEFT OUTER JOIN thumbnails t ON i.hash = t.image_hash AND t.size = 300
              WHERE v.embedding MATCH ? AND k={k}
            AND distance <= {distance_threshold:.6}
              ORDER BY distance LIMIT {k} OFFSET {offset}"
        );

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for searching similar images")?;
        let mut stmt = conn.prepare(&query)?;

        let results = stmt.query_map(params![query_embedding.as_bytes()], |row| {
            let rel_path: String = row.get(0)?;
            let distance: f32 = row.get(1)?;
            let thumbnail_data: Option<Vec<u8>> = row.get(2)?;

            Ok((rel_path, distance, thumbnail_data))
        })?;

        let mut search_results = Vec::new();
        for result in results {
            search_results.push(result?);
        }

        Ok(search_results)
    }

    /// Metadata-first search: returns (relative path, distance, file_size) with
    /// no thumbnail join. Joins `image_metadata` for `file_size`. Mirrors the
    /// embedding-binding + execution idiom of `search_similar_images_with_raw_blob`.
    pub fn search_similar_images_meta(
        &self,
        query_embedding: &[f32],
        limit: usize,
        offset: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> Result<Vec<(String, f32, Option<i64>)>> {
        // vec0's `k` bounds the TOTAL neighbors the virtual table yields, and
        // OFFSET is applied on top of that set. So `k` must cover the entire
        // requested window (offset + limit), otherwise pages past the first
        // get nothing (OFFSET skips all `k` rows). Capped at max_k, which
        // bounds total reachable results across all pages.
        let k = (offset + limit).clamp(1, max_k);
        let vt = self.vectors_table()?;

        // `k` and the distance threshold are interpolated as the vec0 MATCH/k
        // syntax requires literal values (not bound params). Both are trusted
        // numeric config values, never user free-text.
        let query = format!(
            "SELECT i.path, v.distance, m.file_size
              FROM {vt} v
              JOIN images i ON i.id = v.rowid
              LEFT JOIN image_metadata m ON m.image_id = i.id
              WHERE v.embedding MATCH ?1 AND k = {k}
            AND v.distance <= {distance_threshold:.6}
              ORDER BY v.distance LIMIT {limit} OFFSET {offset}"
        );

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for searching similar images")?;
        let mut stmt = conn.prepare(&query)?;

        let results = stmt.query_map(params![query_embedding.as_bytes()], |row| {
            let rel_path: String = row.get(0)?;
            let distance: f32 = row.get(1)?;
            let file_size: Option<i64> = row.get(2)?;

            Ok((rel_path, distance, file_size))
        })?;

        let mut search_results = Vec::new();
        for result in results {
            search_results.push(result?);
        }

        Ok(search_results)
    }

    /// Find images similar to an already-indexed image, using its STORED
    /// embedding from the active model's vec0 table (no re-embedding). The seed
    /// itself is typically the nearest neighbour (distance ~0); callers may filter
    /// it out. Returns `(relative_path, distance, file_size)` rows.
    pub fn find_similar_to_path(
        &self,
        path: &RelativePath,
        limit: usize,
        offset: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> Result<Vec<(String, f32, Option<i64>)>> {
        let vt = self.vectors_table()?;
        let rel = path.as_str();
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for find_similar_to_path")?;

        let id: i64 = conn
            .query_row(
                "SELECT id FROM images WHERE path = ?1",
                params![rel.as_ref()],
                |row| row.get(0),
            )
            .with_context(|| format!("No indexed image at path {rel}"))?;

        let blob: Vec<u8> = conn
            .query_row(
                &format!("SELECT embedding FROM {vt} WHERE rowid = ?1"),
                params![id],
                |row| row.get(0),
            )
            .with_context(|| format!("No stored embedding for image id {id}"))?;

        // Stored vectors are LE-f32 and already L2-normalized; decode as-is.
        let embedding: Vec<f32> = blob
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        // Reuse the existing vec0 search path with the stored vector.
        self.search_similar_images_meta(&embedding, limit, offset, distance_threshold, max_k)
    }

    pub fn search_similar_images_with_blob(
        &self,
        query_embedding: &[f32],
        limit: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> Result<Vec<(String, f32, Option<String>)>> {
        let search_results = self.search_similar_images_with_raw_blob(
            query_embedding,
            limit,
            0,
            distance_threshold,
            max_k,
        )?;
        let search_results: Vec<(String, f32, Option<String>)> = search_results
            .into_iter()
            .map(|(path, distance, thumbnail_data)| {
                let thumbnail_base64 =
                    thumbnail_data.map(|data| general_purpose::STANDARD.encode(&data));
                (path, distance, thumbnail_base64)
            })
            .collect();
        Ok(search_results)
    }

    pub fn clean_missing_files(&mut self) -> Result<usize> {
        let vt = self.vectors_table()?;
        // Get all paths from database
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to clean missing files")?;
        let mut stmt = conn.prepare("SELECT id, path FROM images")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut to_delete = Vec::new();
        for row in rows {
            let (id, rel_path) = row?;

            // Convert relative path to absolute path for file existence check
            let abs_path = RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir);

            if !abs_path.as_path().exists() {
                to_delete.push(id);
            }
        }

        // Delete missing files from both tables in a single transaction
        let mut conn = self
            .pool
            .get()
            .context("Failed to get DB connection to delete missing files")?;
        let tx = conn.transaction()?;
        let removed_count = to_delete.len();
        for id in to_delete {
            // Delete from vector table first
            tx.execute(&format!("DELETE FROM {vt} WHERE rowid = ?1"), params![id])?;
            // Then delete from images table
            tx.execute("DELETE FROM images WHERE id = ?1", params![id])?;
        }
        tx.commit()?;

        Ok(removed_count)
    }

    pub fn get_image_count(&self) -> Result<i64> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to count images")?;
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM images")?;
        let count: i64 = stmt.query_row([], |row| row.get(0))?;
        Ok(count)
    }

    pub fn get_sample_images(&self, limit: usize) -> Result<Vec<AbsolutePath>> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to get sample images")?;
        let mut stmt = conn.prepare("SELECT path FROM images ORDER BY created_at DESC LIMIT ?1")?;

        let image_iter = stmt.query_map([limit], |row| {
            let rel_path: String = row.get(0)?;
            // Convert relative path back to absolute path
            Ok(RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir))
        })?;

        let mut results = Vec::new();
        for image in image_iter {
            results.push(image?);
        }

        Ok(results)
    }

    /// Insert a thumbnail into the database cache
    pub fn insert_thumbnail(
        &self,
        image_hash: &str,
        size: u32,
        thumbnail_data: &[u8],
    ) -> Result<()> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to insert thumbnail")?;
        conn.execute(
            "INSERT OR REPLACE INTO thumbnails (image_hash, size, thumbnail_data) VALUES (?1, ?2, ?3)",
            params![image_hash, size as i64, thumbnail_data],
        ).context("failed to insert or replace")?;
        Ok(())
    }

    /// Get a thumbnail from the database cache
    pub fn get_thumbnail(&self, image_hash: &str, size: u32) -> Result<Vec<u8>> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to get thumbnail")?;
        let mut stmt = conn
            .prepare("SELECT thumbnail_data FROM thumbnails WHERE image_hash = ?1 AND size = ?2")?;

        let thumbnail_data: Vec<u8> =
            stmt.query_row(params![image_hash, size as i64], |row| row.get(0))?;

        Ok(thumbnail_data)
    }

    /// Get the hash for an image by its path
    pub fn get_image_hash(&self, path: &RelativePath) -> Result<String> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to get image hash")?;
        let mut stmt = conn.prepare("SELECT hash FROM images WHERE path = ?1")?;
        let hash: String = stmt.query_row(params![path.as_str().as_ref()], |row| row.get(0))?;
        Ok(hash)
    }

    /// Get images that don't have thumbnails of a specific size
    /// Returns a list of (path, hash) tuples for images missing thumbnails
    pub fn get_images_without_thumbnails(
        &self,
        size: u32,
        limit: usize,
    ) -> Result<Vec<(AbsolutePath, String)>> {
        let query = "
            SELECT i.path, i.hash 
            FROM images i 
            LEFT JOIN thumbnails t ON i.hash = t.image_hash AND t.size = ?1
            WHERE t.id IS NULL
            LIMIT ?2
        ";

        let images = {
            let conn = self
                .pool
                .get()
                .context("Failed to get DB connection for getting images without thumbnails")?;
            let mut stmt = conn.prepare(query)?;
            let results = stmt.query_map(params![size as i64, limit], |row| {
                let rel_path: String = row.get(0)?;
                let hash: String = row.get(1)?;

                // Convert relative path back to absolute path
                let abs_path = RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir);

                Ok((abs_path, hash))
            })?;

            let mut images = Vec::new();
            for result in results {
                images.push(result?);
            }

            images
        };

        Ok(images)
    }
    /// Count images that don't have thumbnails of a specific size
    /// Returns the count of images missing thumbnails
    pub fn count_images_without_thumbnails(&self, size: u32) -> Result<usize> {
        let query = "
            SELECT COUNT(*)
            FROM images i 
            LEFT JOIN thumbnails t ON i.hash = t.image_hash AND t.size = ?1
            WHERE t.id IS NULL
        ";

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for counting images without thumbnails")?;
        let mut stmt = conn.prepare(query)?;
        stmt.query_row(params![size as i64], |row| row.get(0))
            .context("Failed to count images without thumbnails")
            .map(|count: i64| count as usize)
    }

    /// Insert or update metadata for an image
    pub fn insert_or_update_metadata(
        &mut self,
        image_id: i64,
        metadata: &ImageMetadata,
    ) -> Result<()> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to insert metadata")?;

        conn.execute(
            "INSERT OR REPLACE INTO image_metadata 
             (image_id, file_size, width, height, latitude, longitude, 
              camera_make, camera_model, datetime_taken) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                image_id,
                metadata.file_size.map(|s| s as i64),
                metadata.width.map(|w| w as i64),
                metadata.height.map(|h| h as i64),
                metadata.latitude,
                metadata.longitude,
                metadata.camera_make,
                metadata.camera_model,
                metadata.datetime_taken
            ],
        )?;

        Ok(())
    }

    /// Get images without metadata
    pub fn get_images_without_metadata(
        &self,
        limit: usize,
    ) -> Result<Vec<(i64, AbsolutePath, String)>> {
        let query = "
            SELECT i.id, i.path, i.hash 
            FROM images i 
            LEFT JOIN image_metadata m ON i.id = m.image_id
            WHERE m.id IS NULL
            LIMIT ?1
        ";

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for images without metadata")?;
        let mut stmt = conn.prepare(query)?;
        let results = stmt.query_map(params![limit], |row| {
            let id: i64 = row.get(0)?;
            let rel_path: String = row.get(1)?;
            let hash: String = row.get(2)?;

            // Convert relative path back to absolute path
            let abs_path = RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir);

            Ok((id, abs_path, hash))
        })?;

        let mut images = Vec::new();
        for result in results {
            images.push(result?);
        }

        Ok(images)
    }

    /// Get images within geographic bounds
    pub fn get_images_by_bounds(
        &self,
        north: f64,
        south: f64,
        east: f64,
        west: f64,
    ) -> Result<(Vec<ImageWithMetadata>, usize)> {
        let query = "
            SELECT i.path, i.hash, m.latitude, m.longitude, m.width, m.height, m.datetime_taken
            FROM images i
            JOIN image_metadata m ON i.id = m.image_id
            WHERE m.latitude IS NOT NULL 
              AND m.longitude IS NOT NULL
              AND m.latitude BETWEEN ?1 AND ?2
              AND m.longitude BETWEEN ?3 AND ?4
            ORDER BY m.datetime_taken DESC
        ";

        let lat_low = north.min(south);
        let lat_high = north.max(south);
        let long_low = east.min(west);
        let long_high = east.max(west);

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection for images by bounds")?;
        let mut stmt = conn.prepare(query)?;
        let results = stmt.query_map(params![lat_low, lat_high, long_low, long_high], |row| {
            let rel_path: String = row.get(0)?;
            let hash: String = row.get(1)?;
            let latitude: Option<f64> = row.get(2)?;
            let longitude: Option<f64> = row.get(3)?;
            let width: Option<i64> = row.get(4)?;
            let height: Option<i64> = row.get(5)?;
            let datetime_taken: Option<String> = row.get(6)?;

            // Convert relative path back to absolute path for thumbnail generation
            let abs_path = RelativePath(PathBuf::from(&rel_path)).to_absolute(&self.parent_dir);

            Ok(ImageWithMetadata {
                path: rel_path, // Use relative path for frontend
                absolute_path: abs_path.as_str().into_owned(),
                hash,
                latitude,
                longitude,
                width: width.map(|w| w as u32),
                height: height.map(|h| h as u32),
                datetime_taken,
            })
        })?;

        let images: Vec<ImageWithMetadata> = results
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("Failed to read image rows for bounds query")?;

        let biggest_difference = (lat_high - lat_low).max(long_high - long_low);

        info!("Biggest difference was: {biggest_difference}");

        let grid_size = biggest_difference / 200.;

        let original_count = images.len();

        let mut clustered = if original_count < 100 || biggest_difference < 0.01 {
            images
        } else {
            downsample_by_grid(images, grid_size, 10, 2)
        };

        if clustered.len() < 100 {
            apply_stable_jitter(&mut clustered);
        }

        Ok((clustered, original_count))
    }

    /// Get the image ID by path
    pub fn get_image_id(&self, path: &AbsolutePath) -> Result<i64> {
        // Convert absolute path to relative path for database lookup
        let rel_path = path.to_relative(&self.parent_dir).with_context(|| {
            format!("Failed to convert path {} to relative path", path.as_str())
        })?;
        let rel_path_str = rel_path.as_str();

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to get image ID")?;
        let mut stmt = conn.prepare("SELECT id FROM images WHERE path = ?1")?;
        let id: i64 = stmt.query_row(params![rel_path_str.as_ref()], |row| row.get(0))?;
        Ok(id)
    }
}

/// Metadata extracted from image EXIF data
#[derive(Debug, Clone)]
pub struct ImageMetadata {
    pub file_size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub datetime_taken: Option<String>,
}

/// Image with associated metadata (path, hash, EXIF fields) returned from a joined query.
#[derive(Debug, Clone)]
pub struct ImageWithMetadata {
    pub path: String,
    pub absolute_path: String,
    pub hash: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub datetime_taken: Option<String>,
}

/// Extract metadata from image file
pub fn extract_image_metadata(file_path: &str) -> Result<ImageMetadata> {
    use exif::{In, Reader, Tag};
    use image::ImageReader as ImgReader;
    use std::fs;
    use std::io::BufReader;

    let mut metadata = ImageMetadata {
        file_size: None,
        width: None,
        height: None,
        latitude: None,
        longitude: None,
        camera_make: None,
        camera_model: None,
        datetime_taken: None,
    };

    // Get file size
    if let Ok(file_metadata) = fs::metadata(file_path) {
        metadata.file_size = Some(file_metadata.len());
    }

    // Get image dimensions
    if let Ok(img_reader) = ImgReader::open(file_path)
        && let Ok(img) = img_reader.decode()
    {
        let (width, height) = img.dimensions();
        metadata.width = Some(width);
        metadata.height = Some(height);
    }

    // Extract EXIF data
    if let Ok(file) = std::fs::File::open(file_path) {
        let mut bufreader = BufReader::new(&file);
        if let Ok(exifreader) = Reader::new().read_from_container(&mut bufreader) {
            // Camera make
            if let Some(make_field) = exifreader.get_field(Tag::Make, In::PRIMARY) {
                metadata.camera_make = Some(make_field.display_value().to_string());
            }

            // Camera model
            if let Some(model_field) = exifreader.get_field(Tag::Model, In::PRIMARY) {
                metadata.camera_model = Some(model_field.display_value().to_string());
            }

            // DateTime taken
            if let Some(datetime_field) = exifreader.get_field(Tag::DateTime, In::PRIMARY) {
                metadata.datetime_taken = Some(datetime_field.display_value().to_string());
            }

            // GPS coordinates
            let lat_ref = exifreader.get_field(Tag::GPSLatitudeRef, In::PRIMARY);
            let lat = exifreader.get_field(Tag::GPSLatitude, In::PRIMARY);
            let lon_ref = exifreader.get_field(Tag::GPSLongitudeRef, In::PRIMARY);
            let lon = exifreader.get_field(Tag::GPSLongitude, In::PRIMARY);

            if let (Some(lat_ref), Some(lat), Some(lon_ref), Some(lon)) =
                (lat_ref, lat, lon_ref, lon)
                && let (Ok(latitude), Ok(longitude)) = (
                    parse_gps_coordinate(
                        &lat.display_value().to_string(),
                        &lat_ref.display_value().to_string(),
                    ),
                    parse_gps_coordinate(
                        &lon.display_value().to_string(),
                        &lon_ref.display_value().to_string(),
                    ),
                )
            {
                metadata.latitude = Some(latitude);
                metadata.longitude = Some(longitude);
            }
        }
    }

    Ok(metadata)
}

/// Parse GPS coordinate from EXIF format
fn parse_gps_coordinate(coordinate_str: &str, reference: &str) -> Result<f64> {
    // EXIF GPS format is typically "deg min sec" like "40 deg 42 min 51.45 sec"
    let parts: Vec<&str> = coordinate_str.split_whitespace().collect();

    if parts.len() >= 6 {
        let degrees: f64 = parts[0].parse().context("Failed to parse degrees")?;
        let minutes: f64 = parts[2].parse().context("Failed to parse minutes")?;
        let seconds: f64 = parts[4].parse().context("Failed to parse seconds")?;

        let mut decimal = degrees + (minutes / 60.0) + (seconds / 3600.0);

        // Apply reference direction
        if reference == "S" || reference == "W" {
            decimal = -decimal;
        }

        Ok(decimal)
    } else {
        Err(anyhow::anyhow!(
            "Invalid GPS coordinate format: {}",
            coordinate_str
        ))
    }
}

fn downsample_by_grid(
    images: Vec<ImageWithMetadata>,
    grid_size: f64,
    max_per_cluster: usize,
    sample_per_cluster: usize,
) -> Vec<ImageWithMetadata> {
    let mut buckets: HashMap<(i64, i64), Vec<ImageWithMetadata>> = HashMap::new();

    for img in images {
        if let (Some(lat), Some(lon)) = (img.latitude, img.longitude) {
            let key = (
                (lat / grid_size).floor() as i64,
                (lon / grid_size).floor() as i64,
            );
            buckets.entry(key).or_default().push(img);
        }
    }

    let mut result = vec![];

    for mut bucket in buckets.into_values() {
        bucket.sort_by(|a, b| b.datetime_taken.cmp(&a.datetime_taken)); // newest first

        if bucket.len() > max_per_cluster {
            result.extend(bucket.into_iter().take(sample_per_cluster));
        } else {
            result.extend(bucket);
        }
    }

    result
}

pub fn apply_stable_jitter(images: &mut [ImageWithMetadata]) {
    for img in images.iter_mut() {
        if let (Some(lat), Some(lon)) = (img.latitude, img.longitude) {
            let (jitter_lat, jitter_lon) = generate_jitter(img);

            img.latitude = Some(lat + jitter_lat);
            img.longitude = Some(lon + jitter_lon);
        }
    }
}

/// Generate stable jitter based on the content of the struct
fn generate_jitter(img: &ImageWithMetadata) -> (f64, f64) {
    // Combine identifying fields
    let mut s = DefaultHasher::new();
    img.absolute_path.hash(&mut s);
    img.hash.hash(&mut s);

    let hash_val = s.finish();

    // Split the 64-bit hash into two 32-bit halves for separate jitters
    let lat_bits = (hash_val & 0xFFFF_FFFF) as u32;
    let lon_bits = ((hash_val >> 32) & 0xFFFF_FFFF) as u32;

    // Convert to deterministic floats between -1.0 and +1.0
    let lat_unit = (lat_bits as f64 / u32::MAX as f64) * 2.0 - 1.0;
    let lon_unit = (lon_bits as f64 / u32::MAX as f64) * 2.0 - 1.0;

    // Scale to jitter range
    let jitter_lat = lat_unit * MAX_JITTER;
    let jitter_lon = lon_unit * (MAX_JITTER * 2.5);

    (jitter_lat, jitter_lon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique temp directory for an isolated test database.
    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("imgfind_test_{}_{n}", std::process::id()));
        // Database::new -> get_db_parent_dir requires a `.imgfind/imgfind.db` layout.
        dir.join(".imgfind").join("imgfind.db")
    }

    /// Foreign keys must be enabled on *every* connection the pool hands out,
    /// not just the one used during schema init. PRAGMAs are per-connection.
    #[test]
    fn foreign_keys_enabled_on_fresh_pool_connection() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");

        // Hold one connection, then force the pool to build a *second*, fresh one.
        // The old one-off PRAGMA only set foreign_keys on the connection used during
        // schema init; any later connection would have FK enforcement off.
        let _held = db.pool.get().expect("get first conn");
        let conn = db.pool.get().expect("get second conn");
        let fk_on: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("read pragma");
        assert_eq!(fk_on, 1, "foreign_keys should be ON for pooled connections");

        // Remove the unique temp dir (grandparent of the .imgfind/imgfind.db path).
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    /// Deleting an image must cascade to its metadata row (ON DELETE CASCADE),
    /// which only fires when foreign key enforcement is actually active.
    #[test]
    fn delete_image_cascades_to_metadata() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");

        // Force a fresh second connection so the cascade is exercised on a
        // connection other than the one used during schema init.
        let _held = db.pool.get().expect("get first conn");
        let conn = db.pool.get().expect("get second conn");
        conn.execute(
            "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
            [],
        )
        .expect("insert image");
        conn.execute(
            "INSERT INTO image_metadata (image_id, width) VALUES (1, 100)",
            [],
        )
        .expect("insert metadata");

        conn.execute("DELETE FROM images WHERE id = 1", [])
            .expect("delete image");

        let meta_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM image_metadata WHERE image_id = 1",
                [],
                |row| row.get(0),
            )
            .expect("count metadata");
        assert_eq!(
            meta_count, 0,
            "metadata should cascade-delete with its image"
        );

        // Remove the unique temp dir (grandparent of the .imgfind/imgfind.db path).
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn toggle_favorite_flips_state() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");

        // Insert an image directly so the stored path is a known relative value.
        let conn = db.pool.get().expect("get conn");
        conn.execute(
            "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
            [],
        )
        .expect("insert image");
        drop(conn);

        let p = RelativePath(PathBuf::from("a.jpg"));
        assert!(!db.is_favorite(&p).unwrap());
        assert!(db.toggle_favorite(&p).unwrap());
        assert!(db.is_favorite(&p).unwrap());
        assert_eq!(db.list_favorites().unwrap(), vec![p.clone()]);
        assert!(!db.toggle_favorite(&p).unwrap());
        assert!(!db.is_favorite(&p).unwrap());

        // Remove the unique temp dir (grandparent of the .imgfind/imgfind.db path).
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn tag_and_collection_roundtrip() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");

        // Insert an image directly so the stored path is a known relative value.
        let conn = db.pool.get().expect("get conn");
        conn.execute(
            "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
            [],
        )
        .expect("insert image");
        drop(conn);

        let p = RelativePath(PathBuf::from("a.jpg"));
        db.tag_image(&p, "cats").unwrap();
        assert_eq!(db.tags_for_image(&p).unwrap(), vec!["cats".to_string()]);
        assert_eq!(db.images_by_tag("cats").unwrap(), vec![p.clone()]);
        db.untag_image(&p, "cats").unwrap();
        assert!(db.tags_for_image(&p).unwrap().is_empty());
        db.create_collection("trip").unwrap();
        db.add_to_collection("trip", &p).unwrap();
        assert_eq!(db.collection_images("trip").unwrap(), vec![p.clone()]);
        assert_eq!(db.list_collections().unwrap(), vec!["trip".to_string()]);

        // Remove the unique temp dir (grandparent of the .imgfind/imgfind.db path).
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn migrations_set_user_version_and_create_tables() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        let conn = db.pool.get().unwrap();
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, LATEST_MIGRATION_VERSION);
        for t in [
            "images",
            "image_vectors",
            "thumbnails",
            "image_metadata",
            "favorites",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} should exist");
        }

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    /// Regression test for the pagination bug: vec0's `k` bounds the TOTAL
    /// neighbors returned, and OFFSET applies on top. If `k` only covers
    /// `limit` (not `offset + limit`), page 2 gets zero rows. With the fix,
    /// page 2 must return the next slice in distance order.
    #[test]
    fn search_meta_paginates_past_first_page() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        let conn = db.pool.get().expect("get conn");

        // Insert 10 images with embeddings whose first component decreases as
        // the id grows. The query embedding is the unit vector along axis 0, so
        // distance increases monotonically with id => a deterministic ordering.
        for id in 1..=10i64 {
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                params![id, format!("img{id}.jpg"), format!("h{id}")],
            )
            .expect("insert image");

            let mut emb = vec![0.0f32; 512];
            // Larger id -> smaller axis-0 component -> larger cosine distance.
            emb[0] = 1.0 - (id as f32) * 0.01;
            emb[1] = (id as f32) * 0.01;
            conn.execute(
                "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![id, emb.as_slice().as_bytes()],
            )
            .expect("insert vector");
        }
        drop(conn);

        let mut query = vec![0.0f32; 512];
        query[0] = 1.0;

        // Page 1: first 4 (nearest) results.
        let page1 = db
            .search_similar_images_meta(&query, 4, 0, 2.0, 100)
            .expect("page 1");
        // Page 2: next 4 results. Pre-fix this returned ZERO rows.
        let page2 = db
            .search_similar_images_meta(&query, 4, 4, 2.0, 100)
            .expect("page 2");

        assert_eq!(page1.len(), 4, "page 1 should be full");
        assert_eq!(page2.len(), 4, "page 2 should return the next slice");

        let p1_paths: Vec<&str> = page1.iter().map(|(p, ..)| p.as_str()).collect();
        let p2_paths: Vec<&str> = page2.iter().map(|(p, ..)| p.as_str()).collect();
        assert_eq!(p1_paths, ["img1.jpg", "img2.jpg", "img3.jpg", "img4.jpg"]);
        assert_eq!(p2_paths, ["img5.jpg", "img6.jpg", "img7.jpg", "img8.jpg"]);

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    /// Regression test for the silent "Generated 0 thumbnails" bug.
    ///
    /// `index_directory` used to pass `usize::MAX` as the thumbnail batch `count`,
    /// which flows into `get_images_without_thumbnails` as a SQL `LIMIT ?` bound
    /// parameter. rusqlite binds usize via `i64::try_from`, so `usize::MAX` (2^64-1)
    /// overflows i64 and the bind FAILS — the error was swallowed and zero thumbnails
    /// were ever generated. The fix counts the missing images and passes that exact
    /// number. This test pins both halves:
    ///   1. `usize::MAX` as the LIMIT still errors (the trap is real).
    ///   2. The count from `count_images_without_thumbnails` binds cleanly and the
    ///      paired `get_images_without_thumbnails(size, count)` returns every missing
    ///      image — i.e. the value `index_directory` now passes works end to end.
    #[test]
    fn missing_thumbnail_limit_bind_uses_real_count_not_usize_max() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        let conn = db.pool.get().expect("get conn");

        // Three images, none with a thumbnail of size 300.
        for id in 1..=3i64 {
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                params![id, format!("img{id}.jpg"), format!("h{id}")],
            )
            .expect("insert image");
        }
        drop(conn);

        // The bug: usize::MAX overflows the i64 LIMIT bind and errors out.
        assert!(
            db.get_images_without_thumbnails(300, usize::MAX).is_err(),
            "usize::MAX must overflow the i64 LIMIT bind (this is the bug the fix avoids)"
        );

        // The fix: count the missing images, then pass that count.
        let missing = db
            .count_images_without_thumbnails(300)
            .expect("count missing thumbnails must succeed");
        assert_eq!(missing, 3, "all three images are missing a 300px thumbnail");

        // That count binds cleanly as LIMIT and returns every missing image.
        let rows = db
            .get_images_without_thumbnails(300, missing)
            .expect("real-count LIMIT bind must succeed");
        assert_eq!(
            rows.len(),
            3,
            "the count value index_directory now passes covers all missing images"
        );

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn migration_2_seeds_baseline_model_and_user_tables() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        let conn = db.pool.get().unwrap();
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, LATEST_MIGRATION_VERSION);
        let (name, dim, table, active): (String, i64, String, i64) = conn
            .query_row(
                "SELECT name, dim, table_name, is_active FROM models WHERE is_active = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((dim, table.as_str(), active), (512, "image_vectors", 1));
        assert!(name.contains("clip"));
        for t in [
            "tags",
            "image_tags",
            "collections",
            "collection_images",
            "models",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} exists");
        }

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn active_model_defaults_to_baseline_table() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        let m = db.active_model().unwrap();
        assert_eq!(m.dim, 512);
        assert_eq!(m.table, "image_vectors");

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn register_and_switch_model_creates_table_and_flips_active() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        db.register_model("test-model", 256).unwrap();
        db.set_active_model("test-model").unwrap();
        let m = db.active_model().unwrap();
        assert_eq!((m.dim, m.table.as_str()), (256, "image_vectors_test_model"));
        let n: i64 = db
            .pool
            .get()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='image_vectors_test_model'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    /// `is_image_indexed` is model-aware: an image indexed under one model is
    /// reported as NOT indexed after switching to a different model (whose
    /// vector table is empty), so a subsequent `index` run backfills the new
    /// model's embeddings instead of skipping the file.
    #[test]
    fn is_image_indexed_is_model_aware() {
        let db_path = temp_db_path();
        let mut db = Database::new(&db_path).expect("create db");

        let abs = RelativePath(PathBuf::from("photo.jpg")).to_absolute(&db.parent_dir);
        let hash = "deadbeef";

        // Nothing indexed yet.
        assert!(!db.is_image_indexed(&abs, hash).unwrap());

        // Index under the default model (dim 512, table image_vectors).
        db.insert_images_batch(&[("photo.jpg".to_string(), hash.to_string(), vec![0.1f32; 512])])
            .unwrap();
        assert!(
            db.is_image_indexed(&abs, hash).unwrap(),
            "indexed under default model"
        );

        // Switch to a new model: its vector table is empty, so the same image
        // is reported as NOT indexed and would be re-embedded on the next index.
        db.register_model("other-model", 8).unwrap();
        db.set_active_model("other-model").unwrap();
        assert!(
            !db.is_image_indexed(&abs, hash).unwrap(),
            "not indexed under the freshly-switched model"
        );

        // Backfill under the new model -> now indexed for it too.
        db.insert_images_batch(&[("photo.jpg".to_string(), hash.to_string(), vec![0.2f32; 8])])
            .unwrap();
        assert!(
            db.is_image_indexed(&abs, hash).unwrap(),
            "indexed under new model after backfill"
        );

        // The original model's embedding is still present after switching back.
        db.set_active_model("openai/clip-vit-base-patch32").unwrap();
        assert!(
            db.is_image_indexed(&abs, hash).unwrap(),
            "default model embedding still present"
        );

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn migrations_are_idempotent() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");
        let conn = db.pool.get().unwrap();
        run_migrations(&conn).unwrap(); // re-run: no-op
        let v: i32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, LATEST_MIGRATION_VERSION);

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn find_similar_to_path_returns_neighbors_from_stored_embedding() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).expect("create db");

        // Two images with distinct 512-dim embeddings (default model dim).
        let mut a = vec![0.0f32; 512];
        a[0] = 1.0;
        let mut b = vec![0.0f32; 512];
        b[1] = 1.0;

        {
            let conn = db.pool.get().expect("conn");
            conn.execute("INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'ha')", [])
                .expect("img a");
            conn.execute("INSERT INTO images (id, path, hash) VALUES (2, 'b.jpg', 'hb')", [])
                .expect("img b");
            conn.execute(
                "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![1i64, a.as_bytes()],
            )
            .expect("vec a");
            conn.execute(
                "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![2i64, b.as_bytes()],
            )
            .expect("vec b");
        }

        // Seed = a.jpg. Its nearest neighbour is itself (distance ~0); b.jpg is
        // orthogonal (L2 distance = sqrt(2) ≈ 1.414), so we use threshold 2.0.
        let rows = db
            .find_similar_to_path(&RelativePath(PathBuf::from("a.jpg")), 10, 0, 2.0, 100)
            .expect("similar");
        let paths: Vec<&str> = rows.iter().map(|(p, _, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.jpg"), "seed should appear among neighbours");
        assert!(paths.contains(&"b.jpg"), "other image should appear among neighbours");
        // a.jpg (the seed) is closest to itself.
        assert_eq!(rows[0].0, "a.jpg");

        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }
}
