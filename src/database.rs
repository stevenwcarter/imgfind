use crate::filters::{Filters, build_filter_clause_turso};
use crate::ids::{CollectionId, ImageId, TagId};
use crate::ui_state::UiState;
use crate::{
    AbsolutePath, DistanceThreshold, EmbeddingDim, GeoRect, MaxK, RelativePath, ThumbnailSize,
    ThumbnailSpec, db_pool, get_db_parent_dir,
};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use hashbrown::HashMap;
use image::GenericImageView;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tracing::info;
use turso::Value;

#[derive(Clone)]
pub struct Database {
    pool: db_pool::TursoPool,
    pub parent_dir: PathBuf,
}

const MAX_JITTER: f64 = 0.000001;

pub type ImageSearchResult = Result<Vec<(String, f32, Option<Vec<u8>>)>>;

/// One ranked metadata-search row: `(image id, relative path, distance,
/// file_size)`. The `id` lets callers build stable [`crate::sort::RowMeta`]
/// rows from ranked (relevance-ordered) results.
pub type RankedMetaRow = (ImageId, String, f32, Option<i64>);

/// Serialize an f32 slice to little-endian bytes (the `F32_BLOB` wire format).
fn to_le_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Read an `i64` from row column `idx`, with `ctx` as the error context.
fn col_i64(row: &turso::Row, idx: usize, ctx: &str) -> Result<i64> {
    row.get_value(idx)?
        .as_integer()
        .copied()
        .with_context(|| format!("expected integer for {ctx}"))
}

/// Read a `String` from row column `idx`, with `ctx` as the error context.
fn col_text(row: &turso::Row, idx: usize, ctx: &str) -> Result<String> {
    Ok(row
        .get_value(idx)?
        .as_text()
        .with_context(|| format!("expected text for {ctx}"))?
        .to_string())
}

/// Read a nullable `i64` from row column `idx` (`NULL` → `None`).
fn col_opt_i64(row: &turso::Row, idx: usize) -> Result<Option<i64>> {
    Ok(match row.get_value(idx)? {
        Value::Integer(i) => Some(i),
        _ => None,
    })
}

/// Read a nullable `f64` from row column `idx` (`NULL` → `None`).
fn col_opt_f64(row: &turso::Row, idx: usize) -> Result<Option<f64>> {
    Ok(match row.get_value(idx)? {
        Value::Real(r) => Some(r),
        Value::Integer(i) => Some(i as f64),
        _ => None,
    })
}

/// Read a nullable `String` from row column `idx` (`NULL` → `None`).
fn col_opt_text(row: &turso::Row, idx: usize) -> Result<Option<String>> {
    Ok(match row.get_value(idx)? {
        Value::Text(s) => Some(s),
        _ => None,
    })
}

/// Read an `f64` from row column `idx`, with `ctx` as the error context.
fn col_f64(row: &turso::Row, idx: usize, ctx: &str) -> Result<f64> {
    col_opt_f64(row, idx)?.with_context(|| format!("expected real for {ctx}"))
}

impl Database {
    pub async fn new(db_path: &Path) -> Result<Self> {
        let parent_path = db_path.parent().context("DB path has no parent")?;
        std::fs::create_dir_all(parent_path).context("Failed to create DB parent directory")?;

        let max_size = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            .min(32);
        let pool = db_pool::TursoPool::open(db_path, max_size)
            .await
            .with_context(|| format!("Failed to open database at {db_path:?}"))?;

        let parent_dir = get_db_parent_dir(db_path)?;
        let conn = pool.get().await?;
        crate::schema::run_migrations(&conn).await?;
        drop(conn);
        Ok(Self { pool, parent_dir })
    }

    /// Truncate the WAL back into the main DB file. Call after a large write batch (e.g. indexing).
    pub async fn checkpoint_wal(&self) -> Result<()> {
        let conn = self.pool.get().await?;
        // `wal_checkpoint` returns a result row; drain via `query` so turso does
        // not treat the row as an unexpected result.
        let mut rows = conn
            .query("PRAGMA wal_checkpoint(RESTART)", ())
            .await
            .context("wal_checkpoint(RESTART)")?;
        while rows.next().await?.is_some() {}
        Ok(())
    }

    /// Retrieve the persisted GUI session state from the `ui_state` table.
    ///
    /// Returns `Ok(None)` when no row exists yet, and also when the stored JSON
    /// cannot be deserialised (e.g. after a schema evolution that changed field
    /// types). In the latter case a `tracing::warn!` is emitted so the problem
    /// is visible in logs without crashing the GUI.
    pub async fn get_ui_state(&self) -> Result<Option<UiState>> {
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for get_ui_state")?;
        let mut rows = conn
            .query("SELECT state_json FROM ui_state WHERE id = 1", ())
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let json = col_text(&row, 0, "state_json")?;
        match serde_json::from_str::<UiState>(&json) {
            Ok(st) => Ok(Some(st)),
            Err(e) => {
                tracing::warn!("discarding unreadable ui_state: {e}");
                Ok(None)
            }
        }
    }

    /// Persist the GUI session state as a single JSON blob (UPSERT on `id = 1`).
    pub async fn set_ui_state(&self, state: &UiState) -> Result<()> {
        let json = serde_json::to_string(state).context("serialize ui_state")?;
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for set_ui_state")?;
        conn.execute(
            "INSERT INTO ui_state (id, state_json, updated_at)
             VALUES (1, ?1, CURRENT_TIMESTAMP)
             ON CONFLICT(id) DO UPDATE SET state_json = ?1, updated_at = CURRENT_TIMESTAMP",
            (json,),
        )
        .await?;
        Ok(())
    }
}

/// Metadata for an embedding model registered in the `models` table: its name,
/// embedding dimensionality, and the vector table holding its vectors.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub dim: EmbeddingDim,
    pub table: String,
}

impl Database {
    /// The currently active embedding model (the single `models` row with
    /// `is_active = 1`).
    pub async fn active_model(&self) -> Result<ModelInfo> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT name, dim, table_name FROM models WHERE is_active = 1 LIMIT 1",
                (),
            )
            .await?;
        let row = rows.next().await?.context("no active model")?;
        Ok(ModelInfo {
            name: col_text(&row, 0, "name")?,
            dim: EmbeddingDim(col_i64(&row, 1, "dim")? as usize),
            table: col_text(&row, 2, "table_name")?,
        })
    }

    /// The active model's vector table name, validated as a safe SQL identifier
    /// before it is interpolated into queries.
    async fn vectors_table(&self) -> Result<String> {
        let t = self.active_model().await?.table;
        anyhow::ensure!(
            t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid table name"
        );
        Ok(t)
    }

    /// Register a new embedding model and create its (inactive) vector table.
    pub async fn register_model(&self, name: &str, dim: EmbeddingDim) -> Result<()> {
        let table = crate::schema::sanitize_model_table(name);
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO models (name, dim, table_name, is_active) VALUES (?1, ?2, ?3, 0)",
            (name.to_string(), dim.get() as i64, table.clone()),
        )
        .await?;
        crate::schema::create_vector_table(&conn, &table, dim).await?;
        Ok(())
    }

    /// Flip the active model to `name`, deactivating all others.
    pub async fn set_active_model(&self, name: &str) -> Result<()> {
        let conn = self.pool.get().await.context("get connection")?;
        // Drain/drop the existence-check rows BEFORE the UPDATE: a live `Rows`
        // on the same connection leaves a statement in progress and the
        // following write silently does not persist.
        let exists = {
            let mut rows = conn
                .query("SELECT 1 FROM models WHERE name = ?1", (name.to_string(),))
                .await?;
            rows.next().await?.is_some()
        };
        anyhow::ensure!(exists, "unknown model: {name}");
        conn.execute(
            "UPDATE models SET is_active = (name = ?1)",
            (name.to_string(),),
        )
        .await?;
        Ok(())
    }

    /// List all registered models as `(name, dim, is_active)`, ordered by name.
    pub async fn list_models(&self) -> Result<Vec<(String, usize, bool)>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT name, dim, is_active FROM models ORDER BY name", ())
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push((
                col_text(&row, 0, "name")?,
                col_i64(&row, 1, "dim")? as usize,
                col_i64(&row, 2, "is_active")? != 0,
            ));
        }
        Ok(out)
    }

    pub async fn insert_image(
        &self,
        path: &AbsolutePath,
        hash: &str,
        embedding: &[f32],
    ) -> Result<()> {
        // Convert absolute path to relative path for storage
        let rel_path = path.to_relative(&self.parent_dir).with_context(|| {
            format!("Failed to convert path {} to relative path", path.as_str())
        })?;
        let rel_path_str = rel_path.as_str().into_owned();
        let vt = self.vectors_table().await?;

        let mut conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to insert image")?;
        let tx = conn.transaction().await?;

        // Upsert that PRESERVES the row id on a path conflict. `INSERT OR
        // REPLACE` would delete+reinsert, allocating a new id and thereby
        // orphaning this image's embeddings in *other* models' vector tables
        // (which key on the id). `ON CONFLICT … DO UPDATE` keeps the id stable so
        // per-model embeddings stay linked across re-indexing.
        tx.execute(
            "INSERT INTO images (path, hash) VALUES (?1, ?2)
             ON CONFLICT(path) DO UPDATE SET hash = excluded.hash",
            (rel_path_str.clone(), hash.to_string()),
        )
        .await?;

        let image_id = {
            let mut rows = tx
                .query(
                    "SELECT id FROM images WHERE path = ?1",
                    (rel_path_str.clone(),),
                )
                .await?;
            let row = rows.next().await?.context("inserted image row missing")?;
            col_i64(&row, 0, "id")?
        };

        // Replace any existing embedding for this image, then insert the new one.
        tx.execute(
            &format!("DELETE FROM {vt} WHERE image_id = ?1"),
            (image_id,),
        )
        .await?;
        tx.execute(
            &format!("INSERT INTO {vt} (image_id, embedding) VALUES (?1, ?2)"),
            (
                Value::Integer(image_id),
                Value::Blob(to_le_bytes(embedding)),
            ),
        )
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Toggle the favorite flag for the image at `relative_path`, returning the
    /// new state (`true` = now favorited, `false` = now un-favorited).
    pub async fn toggle_favorite(&self, relative_path: &RelativePath) -> Result<bool> {
        let rel = relative_path.as_str().into_owned();
        let conn = self.pool.get().await.context("get connection")?;
        let image_id = {
            let mut rows = conn
                .query("SELECT id FROM images WHERE path = ?1", (rel.clone(),))
                .await?;
            let row = rows
                .next()
                .await?
                .with_context(|| format!("no indexed image at {rel}"))?;
            col_i64(&row, 0, "id")?
        };
        let exists = {
            let mut rows = conn
                .query("SELECT 1 FROM favorites WHERE image_id = ?1", (image_id,))
                .await?;
            rows.next().await?.is_some()
        };
        if exists {
            conn.execute("DELETE FROM favorites WHERE image_id = ?1", (image_id,))
                .await?;
            Ok(false)
        } else {
            conn.execute("INSERT INTO favorites (image_id) VALUES (?1)", (image_id,))
                .await?;
            Ok(true)
        }
    }

    /// Return whether the image at `relative_path` is favorited. Unknown paths
    /// are not favorites.
    pub async fn is_favorite(&self, relative_path: &RelativePath) -> Result<bool> {
        let rel = relative_path.as_str().into_owned();
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT id FROM images WHERE path = ?1", (rel,))
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(false);
        };
        let image_id = col_i64(&row, 0, "id")?;
        let mut fav = conn
            .query("SELECT 1 FROM favorites WHERE image_id = ?1", (image_id,))
            .await?;
        Ok(fav.next().await?.is_some())
    }

    /// List the relative paths of all favorited images, most recently favorited
    /// first.
    pub async fn list_favorites(&self) -> Result<Vec<RelativePath>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT i.path FROM favorites f JOIN images i ON i.id = f.image_id \
                 ORDER BY f.created_at DESC",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(RelativePath(PathBuf::from(col_text(&row, 0, "path")?)));
        }
        Ok(out)
    }

    /// Resolve the `images.id` for a stored relative path, erroring if the image
    /// is not indexed. Used by the tag/collection write methods.
    async fn image_id_for(&self, relative_path: &RelativePath) -> Result<ImageId> {
        let rel = relative_path.as_str().into_owned();
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT id FROM images WHERE path = ?1", (rel.clone(),))
            .await?;
        let row = rows
            .next()
            .await?
            .with_context(|| format!("no indexed image at {rel}"))?;
        Ok(ImageId(col_i64(&row, 0, "id")?))
    }

    /// Ensure a tag exists, returning its id.
    pub async fn create_tag(&self, name: &str) -> Result<TagId> {
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO tags (name) VALUES (?1)",
            (name.to_string(),),
        )
        .await?;
        let mut rows = conn
            .query("SELECT id FROM tags WHERE name = ?1", (name.to_string(),))
            .await?;
        let row = rows.next().await?.context("tag row missing after insert")?;
        Ok(TagId(col_i64(&row, 0, "id")?))
    }

    /// Attach `tag` to the image at `relative_path` (creating the tag if needed).
    pub async fn tag_image(&self, relative_path: &RelativePath, tag: &str) -> Result<()> {
        let image_id = self.image_id_for(relative_path).await?;
        let tag_id = self.create_tag(tag).await?;
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
            (Value::Integer(image_id.get()), Value::Integer(tag_id.get())),
        )
        .await?;
        Ok(())
    }

    /// Remove `tag` from the image at `relative_path`.
    pub async fn untag_image(&self, relative_path: &RelativePath, tag: &str) -> Result<()> {
        let image_id = self.image_id_for(relative_path).await?;
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "DELETE FROM image_tags WHERE image_id = ?1 \
             AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
            (Value::Integer(image_id.get()), Value::Text(tag.to_string())),
        )
        .await?;
        Ok(())
    }

    /// Attach `tag` to every image whose relative path is in `rel_paths`,
    /// resolving all ids in a single query and writing in one transaction.
    /// Paths absent from the DB are silently skipped (not an error).
    pub async fn batch_tag_images(&self, rel_paths: &[&str], tag: &str) -> Result<()> {
        if rel_paths.is_empty() {
            return Ok(());
        }
        let tag_id = self.create_tag(tag).await?;
        let ids = self.resolve_image_ids(rel_paths).await?;
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.pool.get().await.context("get connection")?;
        let tx = conn.transaction().await?;
        for image_id in ids {
            tx.execute(
                "INSERT OR IGNORE INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
                (Value::Integer(image_id.get()), Value::Integer(tag_id.get())),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Remove `tag` from every image whose relative path is in `rel_paths`,
    /// resolving all ids in a single query and deleting in one transaction.
    /// Paths absent from the DB are silently skipped (not an error).
    pub async fn batch_untag_images(&self, rel_paths: &[&str], tag: &str) -> Result<()> {
        if rel_paths.is_empty() {
            return Ok(());
        }
        let ids = self.resolve_image_ids(rel_paths).await?;
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.pool.get().await.context("get connection")?;
        let tx = conn.transaction().await?;
        for image_id in ids {
            tx.execute(
                "DELETE FROM image_tags WHERE image_id = ?1 \
                 AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
                (Value::Integer(image_id.get()), Value::Text(tag.to_string())),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Resolve a slice of relative paths to their `images.id` values in one
    /// `IN (...)` query. Paths absent from the DB are simply omitted.
    async fn resolve_image_ids(&self, rel_paths: &[&str]) -> Result<Vec<ImageId>> {
        let placeholders = rel_paths.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id FROM images WHERE path IN ({placeholders})");
        let params: Vec<Value> = rel_paths
            .iter()
            .map(|p| Value::Text((*p).to_string()))
            .collect();
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn.query(&sql, turso::params_from_iter(params)).await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(ImageId(col_i64(&row, 0, "id")?));
        }
        Ok(ids)
    }

    /// List the tags attached to the image at `relative_path`, alphabetically.
    /// Unknown paths simply have no tags.
    pub async fn tags_for_image(&self, relative_path: &RelativePath) -> Result<Vec<String>> {
        let rel = relative_path.as_str().into_owned();
        let conn = self.pool.get().await.context("get connection")?;
        let image_id = {
            let mut rows = conn
                .query("SELECT id FROM images WHERE path = ?1", (rel,))
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(Vec::new());
            };
            ImageId(col_i64(&row, 0, "id")?)
        };
        let mut rows = conn
            .query(
                "SELECT t.name FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                 WHERE it.image_id = ?1 ORDER BY t.name",
                (Value::Integer(image_id.get()),),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(col_text(&row, 0, "name")?);
        }
        Ok(out)
    }

    /// List all tag names, alphabetically.
    pub async fn list_tags(&self) -> Result<Vec<String>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT name FROM tags ORDER BY name", ())
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(col_text(&row, 0, "name")?);
        }
        Ok(out)
    }

    /// List the relative paths of images carrying `name`, ordered by path.
    pub async fn images_by_tag(&self, name: &str) -> Result<Vec<RelativePath>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT i.path FROM image_tags it JOIN tags t ON t.id = it.tag_id \
                 JOIN images i ON i.id = it.image_id WHERE t.name = ?1 ORDER BY i.path",
                (name.to_string(),),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(RelativePath(PathBuf::from(col_text(&row, 0, "path")?)));
        }
        Ok(out)
    }

    /// Return the stored non-destructive edits for `path`.
    ///
    /// Returns [`crate::edits::ImageEdits::identity()`] when no row exists.
    /// The returned value is `.clamped()` so out-of-range DB values cannot
    /// reach the pixel pipeline.
    pub async fn get_image_edits(&self, path: &RelativePath) -> Result<crate::edits::ImageEdits> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT e.exposure, e.saturation, e.blacks, e.whites, e.brightness, e.contrast
                 FROM image_edits e
                 JOIN images i ON i.id = e.image_id
                 WHERE i.path = ?1",
                (path.as_str().into_owned(),),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(crate::edits::ImageEdits {
                exposure: col_f64(&row, 0, "exposure")? as f32,
                saturation: col_f64(&row, 1, "saturation")? as f32,
                blacks: col_f64(&row, 2, "blacks")? as f32,
                whites: col_f64(&row, 3, "whites")? as f32,
                brightness: col_f64(&row, 4, "brightness")? as f32,
                contrast: col_f64(&row, 5, "contrast")? as f32,
            }
            .clamped()),
            None => Ok(crate::edits::ImageEdits::identity()),
        }
    }

    /// Persist non-destructive edits for `path`, creating or replacing the row.
    ///
    /// All six adjustment fields are `.clamped()` before writing (exposure to
    /// `[-3.0, 3.0]`, the other five to `[-100.0, 100.0]`) so the DB cannot hold
    /// an out-of-range value even if the caller skips clamping.
    pub async fn set_image_edits(
        &self,
        path: &RelativePath,
        edits: &crate::edits::ImageEdits,
    ) -> Result<()> {
        let edits = edits.clamped();
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT INTO image_edits \
                (image_id, exposure, saturation, blacks, whites, brightness, contrast, updated_at)
             SELECT i.id, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP \
             FROM images i WHERE i.path = ?1
             ON CONFLICT(image_id) DO UPDATE SET
               exposure = excluded.exposure,
               saturation = excluded.saturation,
               blacks = excluded.blacks,
               whites = excluded.whites,
               brightness = excluded.brightness,
               contrast = excluded.contrast,
               updated_at = CURRENT_TIMESTAMP",
            (
                path.as_str().into_owned(),
                edits.exposure as f64,
                edits.saturation as f64,
                edits.blacks as f64,
                edits.whites as f64,
                edits.brightness as f64,
                edits.contrast as f64,
            ),
        )
        .await?;
        Ok(())
    }

    /// Return the distinct thumbnail `size` values stored for `image_hash`.
    ///
    /// Useful for checking which renditions are already cached before
    /// generating missing ones.
    pub async fn get_thumbnail_sizes(&self, image_hash: &str) -> Result<Vec<u32>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT DISTINCT size FROM thumbnails WHERE image_hash = ?1",
                (image_hash.to_string(),),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(col_i64(&row, 0, "size")? as u32);
        }
        Ok(out)
    }

    /// Ensure a collection exists, returning its id.
    pub async fn create_collection(&self, name: &str) -> Result<CollectionId> {
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO collections (name) VALUES (?1)",
            (name.to_string(),),
        )
        .await?;
        let mut rows = conn
            .query(
                "SELECT id FROM collections WHERE name = ?1",
                (name.to_string(),),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .context("collection row missing after insert")?;
        Ok(CollectionId(col_i64(&row, 0, "id")?))
    }

    /// Add the image at `relative_path` to `name` (creating the collection if needed).
    pub async fn add_to_collection(&self, name: &str, relative_path: &RelativePath) -> Result<()> {
        let collection_id = self.create_collection(name).await?;
        let image_id = self.image_id_for(relative_path).await?;
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "INSERT OR IGNORE INTO collection_images (collection_id, image_id) VALUES (?1, ?2)",
            (
                Value::Integer(collection_id.get()),
                Value::Integer(image_id.get()),
            ),
        )
        .await?;
        Ok(())
    }

    /// Remove the image at `relative_path` from `name`.
    pub async fn remove_from_collection(
        &self,
        name: &str,
        relative_path: &RelativePath,
    ) -> Result<()> {
        let image_id = self.image_id_for(relative_path).await?;
        let conn = self.pool.get().await.context("get connection")?;
        conn.execute(
            "DELETE FROM collection_images WHERE image_id = ?1 \
             AND collection_id = (SELECT id FROM collections WHERE name = ?2)",
            (
                Value::Integer(image_id.get()),
                Value::Text(name.to_string()),
            ),
        )
        .await?;
        Ok(())
    }

    /// List the relative paths of images in `name`, ordered by path.
    pub async fn collection_images(&self, name: &str) -> Result<Vec<RelativePath>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query(
                "SELECT i.path FROM collection_images ci \
                 JOIN collections c ON c.id = ci.collection_id \
                 JOIN images i ON i.id = ci.image_id WHERE c.name = ?1 ORDER BY i.path",
                (name.to_string(),),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(RelativePath(PathBuf::from(col_text(&row, 0, "path")?)));
        }
        Ok(out)
    }

    /// List all collection names, alphabetically.
    pub async fn list_collections(&self) -> Result<Vec<String>> {
        let conn = self.pool.get().await.context("get connection")?;
        let mut rows = conn
            .query("SELECT name FROM collections ORDER BY name", ())
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(col_text(&row, 0, "name")?);
        }
        Ok(out)
    }

    /// Insert many (relative_path, hash, normalized_embedding) rows in one transaction.
    ///
    /// Paths are expected to already be relative to `parent_dir` (matching the
    /// storage invariant). Replicates `insert_image`'s per-row writes (an
    /// `images` row plus the corresponding vector row) against a single
    /// transaction.
    pub async fn insert_images_batch(&self, rows: &[(String, String, Vec<f32>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let vt = self.vectors_table().await?;
        let mut conn = self
            .pool
            .get()
            .await
            .context("get connection for batch insert")?;
        let tx = conn.transaction().await?;
        for (rel_path_str, hash, embedding) in rows {
            tx.execute(
                "INSERT INTO images (path, hash) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET hash = excluded.hash",
                (rel_path_str.clone(), hash.clone()),
            )
            .await?;

            let image_id = {
                let mut id_rows = tx
                    .query(
                        "SELECT id FROM images WHERE path = ?1",
                        (rel_path_str.clone(),),
                    )
                    .await?;
                let row = id_rows
                    .next()
                    .await?
                    .context("inserted image row missing")?;
                col_i64(&row, 0, "id")?
            };

            tx.execute(
                &format!("DELETE FROM {vt} WHERE image_id = ?1"),
                (image_id,),
            )
            .await?;
            tx.execute(
                &format!("INSERT INTO {vt} (image_id, embedding) VALUES (?1, ?2)"),
                (
                    Value::Integer(image_id),
                    Value::Blob(to_le_bytes(embedding)),
                ),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Insert many `(image_hash, size, jpeg_bytes)` thumbnails in one transaction.
    ///
    /// Used by the thumbnail writer thread (which bridges through
    /// [`crate::block_on`]). Existing `(image_hash, size)` rows are replaced.
    pub async fn insert_thumbnails_batch(&self, items: &[(String, u32, Vec<u8>)]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let mut conn = self
            .pool
            .get()
            .await
            .context("get connection for thumbnail batch insert")?;
        let tx = conn.transaction().await?;
        for (hash, size, data) in items {
            tx.execute(
                "INSERT OR REPLACE INTO thumbnails (image_hash, size, thumbnail_data) \
                 VALUES (?1, ?2, ?3)",
                (hash.clone(), *size as i64, data.clone()),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether `path`/`hash` is indexed **for the active model**.
    ///
    /// Model-aware: an image counts as indexed only if it is present in the
    /// shared `images` table *and* has an embedding row in the active model's
    /// vector table. Because each model stores embeddings in its own vector
    /// table, switching the active model leaves existing `images` rows without a
    /// vector in the new table — so they are reported as not-indexed and get
    /// re-embedded on the next `index` run (backfilling the new model).
    pub async fn is_image_indexed(&self, path: &AbsolutePath, hash: &str) -> Result<bool> {
        let rel_path = path.to_relative(&self.parent_dir).with_context(|| {
            format!("Failed to convert path {} to relative path", path.as_str())
        })?;
        let rel_path_str = rel_path.as_str().into_owned();
        let vt = self.vectors_table().await?;

        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to check if image is indexed")?;
        let sql = format!(
            "SELECT COUNT(*) FROM images i \
             WHERE i.path = ?1 AND i.hash = ?2 \
               AND EXISTS (SELECT 1 FROM {vt} WHERE image_id = i.id)"
        );
        let mut rows = conn.query(&sql, (rel_path_str, hash.to_string())).await?;
        let row = rows.next().await?.context("COUNT returned no row")?;
        Ok(col_i64(&row, 0, "count")? > 0)
    }

    /// Search for similar images, returning `(relative_path, distance)`.
    pub async fn search_similar_images(
        &self,
        query_embedding: &[f32],
        limit: usize,
        distance_threshold: DistanceThreshold,
        max_k: MaxK,
    ) -> Result<Vec<(String, f32)>> {
        let k = limit.clamp(1, max_k.get());
        let vt = self.vectors_table().await?;
        let sql = crate::vector_sql::knn_query(
            &vt,
            "path, distance",
            "",
            "",
            k,
            0,
            distance_threshold.get(),
        );
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for searching similar images")?;
        let mut rows = conn
            .query(&sql, (Value::Blob(to_le_bytes(query_embedding)),))
            .await?;
        let mut out = Vec::with_capacity(k);
        while let Some(row) = rows.next().await? {
            out.push((
                col_text(&row, 0, "path")?,
                col_opt_f64(&row, 1)?.unwrap_or(0.0) as f32,
            ));
        }
        Ok(out)
    }

    pub async fn search_similar_images_with_raw_blob(
        &self,
        query_embedding: &[f32],
        limit: usize,
        offset: usize,
        distance_threshold: DistanceThreshold,
        max_k: MaxK,
    ) -> ImageSearchResult {
        let k = limit.clamp(1, max_k.get());
        let vt = self.vectors_table().await?;
        let sql = crate::vector_sql::knn_query(
            &vt,
            "path, distance, thumbnail_data",
            "LEFT OUTER JOIN thumbnails t ON i.hash = t.image_hash AND t.size = 300",
            "",
            k,
            offset,
            distance_threshold.get(),
        );
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for searching similar images")?;
        let mut rows = conn
            .query(&sql, (Value::Blob(to_le_bytes(query_embedding)),))
            .await?;
        let mut out = Vec::with_capacity(k);
        while let Some(row) = rows.next().await? {
            let thumb = match row.get_value(2)? {
                Value::Blob(b) => Some(b),
                _ => None,
            };
            out.push((
                col_text(&row, 0, "path")?,
                col_opt_f64(&row, 1)?.unwrap_or(0.0) as f32,
                thumb,
            ));
        }
        Ok(out)
    }

    /// Metadata-first search: returns `(image id, relative path, distance,
    /// file_size)` with no thumbnail join. The `id` lets callers build stable
    /// [`crate::sort::RowMeta`] rows from ranked results.
    pub async fn search_similar_images_meta(
        &self,
        query_embedding: &[f32],
        limit: usize,
        offset: usize,
        distance_threshold: DistanceThreshold,
        max_k: MaxK,
        filters: &Filters,
    ) -> Result<Vec<RankedMetaRow>> {
        // k must cover offset+limit AFTER filtering; max_k acts as a floor so a
        // full page survives post-scan filtering even on page 1.
        let k = (offset + limit).max(1).max(max_k.get());
        let vt = self.vectors_table().await?;
        let (clause, fparams) = build_filter_clause_turso(filters);
        let sql = crate::vector_sql::knn_query(
            &vt,
            "id, path, distance, file_size",
            "",
            &clause,
            limit.min(k),
            offset,
            distance_threshold.get(),
        );
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for filtered vector search")?;
        let mut params: Vec<Value> = vec![Value::Blob(to_le_bytes(query_embedding))];
        params.extend(fparams);
        let mut rows = conn.query(&sql, turso::params_from_iter(params)).await?;
        let mut out = Vec::with_capacity(limit.min(k));
        while let Some(row) = rows.next().await? {
            out.push((
                ImageId(col_i64(&row, 0, "id")?),
                col_text(&row, 1, "path")?,
                col_opt_f64(&row, 2)?.unwrap_or(0.0) as f32,
                col_opt_i64(&row, 3)?,
            ));
        }
        Ok(out)
    }

    /// Find images similar to an already-indexed image, using its STORED
    /// embedding from the active model's vector table (no re-embedding). The
    /// seed itself is typically the nearest neighbour (distance ~0); callers may
    /// filter it out. Returns `(image id, relative_path, distance, file_size)` rows.
    pub async fn find_similar_to_path(
        &self,
        path: &RelativePath,
        limit: usize,
        offset: usize,
        distance_threshold: DistanceThreshold,
        max_k: MaxK,
        filters: &crate::filters::Filters,
    ) -> Result<Vec<RankedMetaRow>> {
        let vt = self.vectors_table().await?;
        let rel = path.as_str().into_owned();
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for find_similar_to_path")?;

        let id = {
            let mut rows = conn
                .query("SELECT id FROM images WHERE path = ?1", (rel.clone(),))
                .await?;
            let row = rows
                .next()
                .await?
                .with_context(|| format!("No indexed image at path {rel}"))?;
            col_i64(&row, 0, "id")?
        };

        let blob = {
            let mut rows = conn
                .query(
                    &format!("SELECT embedding FROM {vt} WHERE image_id = ?1"),
                    (id,),
                )
                .await?;
            let row = rows
                .next()
                .await?
                .with_context(|| format!("No stored embedding for image id {id}"))?;
            match row.get_value(0)? {
                Value::Blob(b) => b,
                _ => anyhow::bail!("stored embedding is not a blob"),
            }
        };

        // Stored vectors are LE-f32 and already L2-normalized; decode as-is.
        let embedding: Vec<f32> = blob
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        drop(conn);

        self.search_similar_images_meta(
            &embedding,
            limit,
            offset,
            distance_threshold,
            max_k,
            filters,
        )
        .await
    }

    pub async fn search_similar_images_with_blob(
        &self,
        query_embedding: &[f32],
        limit: usize,
        distance_threshold: DistanceThreshold,
        max_k: MaxK,
    ) -> Result<Vec<(String, f32, Option<String>)>> {
        let search_results = self
            .search_similar_images_with_raw_blob(
                query_embedding,
                limit,
                0,
                distance_threshold,
                max_k,
            )
            .await?;
        Ok(search_results
            .into_iter()
            .map(|(path, distance, thumbnail_data)| {
                let thumbnail_base64 =
                    thumbnail_data.map(|data| general_purpose::STANDARD.encode(&data));
                (path, distance, thumbnail_base64)
            })
            .collect())
    }

    pub async fn clean_missing_files(&self) -> Result<usize> {
        let vt = self.vectors_table().await?;
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to clean missing files")?;

        let mut to_delete = Vec::new();
        {
            let mut rows = conn.query("SELECT id, path FROM images", ()).await?;
            while let Some(row) = rows.next().await? {
                let id = col_i64(&row, 0, "id")?;
                let rel_path = col_text(&row, 1, "path")?;
                let abs_path = RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir);
                if !abs_path.as_path().exists() {
                    to_delete.push(id);
                }
            }
        }
        drop(conn);

        let mut conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to delete missing files")?;
        let tx = conn.transaction().await?;
        let removed_count = to_delete.len();
        for id in to_delete {
            tx.execute(&format!("DELETE FROM {vt} WHERE image_id = ?1"), (id,))
                .await?;
            tx.execute("DELETE FROM images WHERE id = ?1", (id,))
                .await?;
        }
        tx.commit().await?;
        Ok(removed_count)
    }

    pub async fn get_image_count(&self) -> Result<i64> {
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to count images")?;
        let mut rows = conn.query("SELECT COUNT(*) FROM images", ()).await?;
        let row = rows.next().await?.context("COUNT returned no row")?;
        col_i64(&row, 0, "count")
    }

    pub async fn get_sample_images(&self, limit: usize) -> Result<Vec<AbsolutePath>> {
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to get sample images")?;
        let mut rows = conn
            .query(
                "SELECT path FROM images ORDER BY created_at DESC LIMIT ?1",
                (limit as i64,),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let rel_path = col_text(&row, 0, "path")?;
            out.push(RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir));
        }
        Ok(out)
    }

    /// Insert a thumbnail into the database cache.
    ///
    /// `spec` accepts either a [`ThumbnailSize`] (scaled thumbnail) or a
    /// [`ThumbnailSpec`] directly. `FullSize` is stored under `size = 0`.
    pub async fn insert_thumbnail(
        &self,
        image_hash: &str,
        spec: impl Into<ThumbnailSpec>,
        thumbnail_data: &[u8],
    ) -> Result<()> {
        let size_col = i64::from(spec.into().to_db_size());
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to insert thumbnail")?;
        conn.execute(
            "INSERT OR REPLACE INTO thumbnails (image_hash, size, thumbnail_data) \
             VALUES (?1, ?2, ?3)",
            (image_hash.to_string(), size_col, thumbnail_data.to_vec()),
        )
        .await
        .context("failed to insert or replace")?;
        Ok(())
    }

    /// Get a thumbnail from the database cache.
    ///
    /// `spec` accepts either a [`ThumbnailSize`] (scaled thumbnail) or a
    /// [`ThumbnailSpec`] directly. `FullSize` is looked up under `size = 0`.
    pub async fn get_thumbnail(
        &self,
        image_hash: &str,
        spec: impl Into<ThumbnailSpec>,
    ) -> Result<Vec<u8>> {
        let size_col = i64::from(spec.into().to_db_size());
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to get thumbnail")?;
        let mut rows = conn
            .query(
                "SELECT thumbnail_data FROM thumbnails WHERE image_hash = ?1 AND size = ?2",
                (image_hash.to_string(), size_col),
            )
            .await?;
        let row = rows.next().await?.context("no thumbnail row")?;
        match row.get_value(0)? {
            Value::Blob(b) => Ok(b),
            _ => anyhow::bail!("thumbnail_data is not a blob"),
        }
    }

    /// Get the hash for an image by its path.
    pub async fn get_image_hash(&self, path: &RelativePath) -> Result<String> {
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to get image hash")?;
        let mut rows = conn
            .query(
                "SELECT hash FROM images WHERE path = ?1",
                (path.as_str().into_owned(),),
            )
            .await?;
        let row = rows.next().await?.context("no image row for hash lookup")?;
        col_text(&row, 0, "hash")
    }

    /// Get images that don't have thumbnails of a specific size.
    /// Returns a list of `(path, hash)` tuples for images missing thumbnails.
    pub async fn get_images_without_thumbnails(
        &self,
        size: ThumbnailSize,
        limit: usize,
    ) -> Result<Vec<(AbsolutePath, String)>> {
        let limit = i64::try_from(limit).context("thumbnail LIMIT exceeds i64 range")?;
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for getting images without thumbnails")?;
        let mut rows = conn
            .query(
                "SELECT i.path, i.hash \
                 FROM images i \
                 LEFT JOIN thumbnails t ON i.hash = t.image_hash AND t.size = ?1 \
                 WHERE t.id IS NULL \
                 LIMIT ?2",
                (i64::from(size.get()), limit),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let rel_path = col_text(&row, 0, "path")?;
            let hash = col_text(&row, 1, "hash")?;
            let abs_path = RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir);
            out.push((abs_path, hash));
        }
        Ok(out)
    }

    /// Count images that don't have thumbnails of a specific size.
    pub async fn count_images_without_thumbnails(&self, size: ThumbnailSize) -> Result<usize> {
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for counting images without thumbnails")?;
        let mut rows = conn
            .query(
                "SELECT COUNT(*) \
                 FROM images i \
                 LEFT JOIN thumbnails t ON i.hash = t.image_hash AND t.size = ?1 \
                 WHERE t.id IS NULL",
                (i64::from(size.get()),),
            )
            .await?;
        let row = rows.next().await?.context("COUNT returned no row")?;
        Ok(col_i64(&row, 0, "count")? as usize)
    }

    /// Insert or update metadata for an image.
    pub async fn insert_or_update_metadata(
        &self,
        image_id: ImageId,
        metadata: &ImageMetadata,
    ) -> Result<()> {
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to insert metadata")?;
        conn.execute(
            "INSERT OR REPLACE INTO image_metadata \
             (image_id, file_size, width, height, latitude, longitude, \
              camera_make, camera_model, datetime_taken) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                Value::Integer(image_id.get()),
                opt_i64(metadata.file_size.map(|s| s as i64)),
                opt_i64(metadata.width.map(|w| w as i64)),
                opt_i64(metadata.height.map(|h| h as i64)),
                opt_f64(metadata.coords.map(|c| c.lat)),
                opt_f64(metadata.coords.map(|c| c.lon)),
                opt_text(metadata.camera_make.clone()),
                opt_text(metadata.camera_model.clone()),
                opt_text(metadata.datetime_taken.clone()),
            ),
        )
        .await?;
        Ok(())
    }

    /// Get images without metadata.
    pub async fn get_images_without_metadata(
        &self,
        limit: usize,
    ) -> Result<Vec<(ImageId, AbsolutePath, String)>> {
        let limit = i64::try_from(limit).context("metadata LIMIT exceeds i64 range")?;
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for images without metadata")?;
        let mut rows = conn
            .query(
                "SELECT i.id, i.path, i.hash \
                 FROM images i \
                 LEFT JOIN image_metadata m ON i.id = m.image_id \
                 WHERE m.id IS NULL \
                 LIMIT ?1",
                (limit,),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let id = ImageId(col_i64(&row, 0, "id")?);
            let rel_path = col_text(&row, 1, "path")?;
            let hash = col_text(&row, 2, "hash")?;
            let abs_path = RelativePath(PathBuf::from(rel_path)).to_absolute(&self.parent_dir);
            out.push((id, abs_path, hash));
        }
        Ok(out)
    }

    /// Get images within geographic bounds.
    pub async fn get_images_by_bounds(
        &self,
        rect: GeoRect,
    ) -> Result<(Vec<ImageWithMetadata>, usize)> {
        let lat_low = rect.lat_low();
        let lat_high = rect.lat_high();
        let long_low = rect.long_low();
        let long_high = rect.long_high();

        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection for images by bounds")?;
        let mut rows = conn
            .query(
                "SELECT i.path, i.hash, m.latitude, m.longitude, m.width, m.height, \
                        m.datetime_taken \
                 FROM images i \
                 JOIN image_metadata m ON i.id = m.image_id \
                 WHERE m.latitude IS NOT NULL AND m.longitude IS NOT NULL \
                   AND m.latitude BETWEEN ?1 AND ?2 \
                   AND m.longitude BETWEEN ?3 AND ?4 \
                 ORDER BY m.datetime_taken DESC",
                (lat_low, lat_high, long_low, long_high),
            )
            .await?;

        let mut images = Vec::new();
        while let Some(row) = rows.next().await? {
            let rel_path = col_text(&row, 0, "path")?;
            let abs_path = RelativePath(PathBuf::from(&rel_path)).to_absolute(&self.parent_dir);
            images.push(ImageWithMetadata {
                path: rel_path,
                absolute_path: abs_path.as_str().into_owned(),
                hash: col_text(&row, 1, "hash")?,
                latitude: col_opt_f64(&row, 2)?,
                longitude: col_opt_f64(&row, 3)?,
                width: col_opt_i64(&row, 4)?.map(|w| w as u32),
                height: col_opt_i64(&row, 5)?.map(|h| h as u32),
                datetime_taken: col_opt_text(&row, 6)?,
            });
        }

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

    /// Browse all indexed images matching `f` (no vector search), most-recent first.
    ///
    /// Images without a metadata row still appear when filters are permissive,
    /// because the join is a LEFT JOIN. Returns `(relative_path, file_size)` rows.
    pub async fn browse(
        &self,
        f: &Filters,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<(String, Option<i64>)>> {
        let (clause, mut values) = build_filter_clause_turso(f);
        let sql = format!(
            "SELECT i.path, m.file_size
               FROM images i
               LEFT JOIN image_metadata m ON m.image_id = i.id
              WHERE 1=1{clause}
              ORDER BY m.datetime_taken DESC, i.id DESC
              LIMIT ? OFFSET ?"
        );
        values.push(Value::Integer(limit as i64));
        values.push(Value::Integer(offset as i64));
        let conn = self.pool.get().await.context("DB connection for browse")?;
        let mut rows = conn.query(&sql, turso::params_from_iter(values)).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push((col_text(&row, 0, "path")?, col_opt_i64(&row, 1)?));
        }
        Ok(out)
    }

    /// Browse **all** matching images in the given sort order, with no LIMIT/OFFSET.
    ///
    /// Returns lightweight [`crate::sort::RowMeta`] rows (id, path, size, ext).
    /// `ext` is derived Rust-side as the lowercased substring after the last `.`
    /// (empty string when there is no dot), matching [`crate::sort::ext_sql_expr`]
    /// used for SQL-level ordering.
    pub async fn browse_all(
        &self,
        f: &Filters,
        sort: &crate::sort::Sort,
    ) -> Result<Vec<crate::sort::RowMeta>> {
        let (clause, values) = build_filter_clause_turso(f);
        let order = crate::sort::order_by_clause(sort);
        let sql = format!(
            "SELECT i.id, i.path, m.file_size
               FROM images i
               LEFT JOIN image_metadata m ON m.image_id = i.id
              WHERE 1=1{clause}
              ORDER BY {order}"
        );
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for browse_all")?;
        let mut rows = conn.query(&sql, turso::params_from_iter(values)).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(row_meta(&row)?);
        }
        Ok(out)
    }

    /// Fetch [`crate::sort::RowMeta`] for an explicit ordered id list.
    ///
    /// Returns one `RowMeta` per id that exists in `images`, in the **same order
    /// as `ids`**. Ids not found in the database are silently dropped. An empty
    /// `ids` slice returns an empty `Vec` without touching the database.
    pub async fn rehydrate_rows(&self, ids: &[ImageId]) -> Result<Vec<crate::sort::RowMeta>> {
        use crate::sort::RowMeta;

        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT i.id, i.path, m.file_size
               FROM images i
               LEFT JOIN image_metadata m ON m.image_id = i.id
              WHERE i.id IN ({placeholders})"
        );
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for rehydrate_rows")?;
        let params: Vec<Value> = ids.iter().map(|i| Value::Integer(i.get())).collect();
        let mut rows = conn.query(&sql, turso::params_from_iter(params)).await?;
        let mut found: HashMap<ImageId, RowMeta> = HashMap::new();
        while let Some(row) = rows.next().await? {
            let meta = row_meta(&row)?;
            found.insert(meta.id, meta);
        }
        Ok(ids.iter().filter_map(|id| found.get(id).cloned()).collect())
    }

    /// Distinct lowercased file extensions present across all indexed image paths.
    ///
    /// The extension is extracted Rust-side (via `rsplit_once('.')`) after
    /// `lower()` is applied in SQL, so both `a.JPG` and `b.jpg` yield `"jpg"`.
    /// Deduplication is handled by a `BTreeSet` (also giving alphabetical order).
    pub async fn distinct_extensions(&self) -> Result<Vec<String>> {
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for distinct_extensions")?;
        let mut rows = conn
            .query("SELECT DISTINCT lower(path) FROM images", ())
            .await?;
        let mut set = std::collections::BTreeSet::new();
        while let Some(row) = rows.next().await? {
            let p = col_text(&row, 0, "path")?;
            if let Some((_, ext)) = p.rsplit_once('.')
                && !ext.is_empty()
                && !ext.contains('/')
            {
                set.insert(ext.to_string());
            }
        }
        Ok(set.into_iter().collect())
    }

    /// `(min, max)` of non-null `file_size` values; `(0, 0)` when no rows have a size.
    pub async fn file_size_bounds(&self) -> Result<(i64, i64)> {
        let conn = self
            .pool
            .get()
            .await
            .context("DB connection for file_size_bounds")?;
        let mut rows = conn
            .query(
                "SELECT MIN(file_size), MAX(file_size) FROM image_metadata \
                 WHERE file_size IS NOT NULL",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok((0, 0));
        };
        Ok((
            col_opt_i64(&row, 0)?.unwrap_or(0),
            col_opt_i64(&row, 1)?.unwrap_or(0),
        ))
    }

    /// Get the image ID by path.
    pub async fn get_image_id(&self, path: &AbsolutePath) -> Result<ImageId> {
        let rel_path = path.to_relative(&self.parent_dir).with_context(|| {
            format!("Failed to convert path {} to relative path", path.as_str())
        })?;
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to get image ID")?;
        let mut rows = conn
            .query(
                "SELECT id FROM images WHERE path = ?1",
                (rel_path.as_str().into_owned(),),
            )
            .await?;
        let row = rows.next().await?.context("no image row for id lookup")?;
        Ok(ImageId(col_i64(&row, 0, "id")?))
    }

    /// Read an image's stored EXIF/metadata from the `image_metadata` table by
    /// its relative path, avoiding a fresh file decode.
    ///
    /// Returns `Ok(None)` when the path is unknown or has no metadata row.
    /// Dimensions/size are stored as `i64` (see [`Self::insert_or_update_metadata`])
    /// and cast back to `u64`/`u32` here.
    pub async fn get_image_metadata(&self, rel: &RelativePath) -> Result<Option<ImageMetadata>> {
        let conn = self
            .pool
            .get()
            .await
            .context("Failed to get DB connection to read stored metadata")?;
        let mut rows = conn
            .query(
                "SELECT m.file_size, m.width, m.height, m.latitude, m.longitude, \
                        m.camera_make, m.camera_model, m.datetime_taken \
                 FROM image_metadata m \
                 JOIN images i ON i.id = m.image_id \
                 WHERE i.path = ?1",
                (rel.as_str().into_owned(),),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(ImageMetadata {
            file_size: col_opt_i64(&row, 0)?.map(|s| s as u64),
            width: col_opt_i64(&row, 1)?.map(|w| w as u32),
            height: col_opt_i64(&row, 2)?.map(|h| h as u32),
            coords: col_opt_f64(&row, 3)?
                .zip(col_opt_f64(&row, 4)?)
                .map(|(lat, lon)| GpsCoords { lat, lon }),
            camera_make: col_opt_text(&row, 5)?,
            camera_model: col_opt_text(&row, 6)?,
            datetime_taken: col_opt_text(&row, 7)?,
        }))
    }
}

/// Build a [`crate::sort::RowMeta`] from a `(id, path, file_size)` row, deriving
/// the lowercased extension Rust-side.
fn row_meta(row: &turso::Row) -> Result<crate::sort::RowMeta> {
    let id = ImageId(col_i64(row, 0, "id")?);
    let path = col_text(row, 1, "path")?;
    let size = col_opt_i64(row, 2)?.map(crate::units::FileSize);
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default();
    Ok(crate::sort::RowMeta {
        id,
        path,
        size,
        ext,
    })
}

/// Wrap an optional `i64` as a turso [`Value`] (`None` → `Null`).
fn opt_i64(v: Option<i64>) -> Value {
    v.map_or(Value::Null, Value::Integer)
}

/// Wrap an optional `f64` as a turso [`Value`] (`None` → `Null`).
fn opt_f64(v: Option<f64>) -> Value {
    v.map_or(Value::Null, Value::Real)
}

/// Wrap an optional `String` as a turso [`Value`] (`None` → `Null`).
fn opt_text(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::Text)
}

/// A complete GPS fix: latitude and longitude are always present together, so a
/// half-present coordinate (one without the other) is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpsCoords {
    pub lat: f64,
    pub lon: f64,
}

/// Metadata extracted from image EXIF data
#[derive(Debug, Clone)]
pub struct ImageMetadata {
    pub file_size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub coords: Option<GpsCoords>,
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
    use std::fs;
    use std::io::BufReader;

    let mut metadata = ImageMetadata {
        file_size: None,
        width: None,
        height: None,
        coords: None,
        camera_make: None,
        camera_model: None,
        datetime_taken: None,
    };

    // Get file size
    if let Ok(file_metadata) = fs::metadata(file_path) {
        metadata.file_size = Some(file_metadata.len());
    }

    // Get image dimensions (RAW-aware via the decode seam; best-effort).
    if let Ok(img) = crate::decode::decode_image(std::path::Path::new(file_path)) {
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
                metadata.coords = Some(GpsCoords {
                    lat: latitude,
                    lon: longitude,
                });
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
    use crate::schema::LATEST_MIGRATION_VERSION;
    use crate::units::FileSize;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique temp directory for an isolated test database.
    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("imgfind_test_{}_{n}", std::process::id()));
        // Database::new -> get_db_parent_dir requires a `.imgfind/imgfind.db` layout.
        dir.join(".imgfind").join("imgfind.db")
    }

    /// Remove the unique temp dir (grandparent of the `.imgfind/imgfind.db` path).
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    /// Embed a raw f32 slice as the `F32_BLOB` wire format for direct inserts.
    fn emb_blob(v: &[f32]) -> Value {
        Value::Blob(to_le_bytes(v))
    }

    #[tokio::test]
    async fn get_image_metadata_reads_stored_row_without_decoding() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        {
            let conn = db.pool.get().await.expect("conn");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
                (),
            )
            .await
            .expect("insert image");
            conn.execute(
                "INSERT INTO image_metadata
                 (image_id, file_size, width, height, latitude, longitude,
                  camera_make, camera_model, datetime_taken)
                 VALUES (1, 4096, 800, 600, 12.5, -77.25, 'Canon', 'R5', '2026:06:20 12:00:00')",
                (),
            )
            .await
            .expect("insert metadata");
        }

        let meta = db
            .get_image_metadata(&RelativePath(PathBuf::from("a.jpg")))
            .await
            .expect("query")
            .expect("metadata row present");
        assert_eq!(meta.file_size, Some(4096));
        assert_eq!(meta.width, Some(800));
        assert_eq!(meta.height, Some(600));
        assert_eq!(
            meta.coords,
            Some(GpsCoords {
                lat: 12.5,
                lon: -77.25
            })
        );
        assert_eq!(meta.camera_make.as_deref(), Some("Canon"));
        assert_eq!(meta.camera_model.as_deref(), Some("R5"));
        assert_eq!(meta.datetime_taken.as_deref(), Some("2026:06:20 12:00:00"));

        // Image row exists but has no metadata row → Ok(None).
        {
            let conn = db.pool.get().await.expect("conn");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (2, 'b.jpg', 'h2')",
                (),
            )
            .await
            .expect("insert image");
        }
        assert!(
            db.get_image_metadata(&RelativePath(PathBuf::from("b.jpg")))
                .await
                .expect("query")
                .is_none(),
            "image without a metadata row should yield Ok(None)"
        );

        // Unknown path → Ok(None).
        assert!(
            db.get_image_metadata(&RelativePath(PathBuf::from("nope.jpg")))
                .await
                .expect("query")
                .is_none(),
            "unknown path should yield Ok(None)"
        );

        // Half-present coordinate: latitude stored, longitude NULL. The paired
        // `coords` field makes this unrepresentable, so it must read back `None`.
        {
            let conn = db.pool.get().await.expect("conn");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (3, 'c.jpg', 'h3')",
                (),
            )
            .await
            .expect("insert image");
            conn.execute(
                "INSERT INTO image_metadata (image_id, latitude) VALUES (3, 12.5)",
                (),
            )
            .await
            .expect("insert half coordinate");
        }
        let half = db
            .get_image_metadata(&RelativePath(PathBuf::from("c.jpg")))
            .await
            .expect("query")
            .expect("metadata row present");
        assert_eq!(
            half.coords, None,
            "latitude without longitude must yield coords: None"
        );

        cleanup(&db_path);
    }

    /// Deleting an image must cascade to its vector and metadata rows
    /// (ON DELETE CASCADE), which only fires with foreign-key enforcement active.
    #[tokio::test]
    async fn delete_image_cascades_to_vectors_and_metadata() {
        let (db, path) = test_db_with_rows(&[("a.jpg", Some(100))]).await;

        // Insert an embedding for image id 1.
        {
            let conn = db.pool.get().await.expect("conn");
            conn.execute(
                "INSERT INTO image_vectors (image_id, embedding) VALUES (1, ?1)",
                (emb_blob(&[0.5f32; 512]),),
            )
            .await
            .expect("insert vector");
        }

        // Delete the image row; cascade should remove the vector + metadata rows.
        {
            let conn = db.pool.get().await.expect("conn");
            conn.execute("DELETE FROM images WHERE id = 1", ())
                .await
                .expect("delete image");
        }

        let conn = db.pool.get().await.expect("conn");
        let mut vrows = conn
            .query("SELECT COUNT(*) FROM image_vectors WHERE image_id = 1", ())
            .await
            .unwrap();
        let vrow = vrows.next().await.unwrap().unwrap();
        assert_eq!(
            col_i64(&vrow, 0, "count").unwrap(),
            0,
            "vector row should cascade-delete with its image"
        );
        let mut mrows = conn
            .query("SELECT COUNT(*) FROM image_metadata WHERE image_id = 1", ())
            .await
            .unwrap();
        let mrow = mrows.next().await.unwrap().unwrap();
        assert_eq!(
            col_i64(&mrow, 0, "count").unwrap(),
            0,
            "metadata row should cascade-delete with its image"
        );
        drop(conn);

        cleanup(&path);
    }

    /// N concurrent tasks inserting thumbnails through the pool all succeed
    /// (the pool must not deadlock under concurrent write load).
    #[tokio::test]
    async fn concurrent_writes_do_not_deadlock() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");

        let mut handles = Vec::new();
        for i in 0..16u32 {
            let db = db.clone();
            handles.push(tokio::spawn(async move {
                db.insert_thumbnails_batch(&[(format!("h{i}"), 300, vec![i as u8; 8])])
                    .await
            }));
        }
        for h in handles {
            h.await.expect("task joined").expect("thumbnail insert");
        }

        let conn = db.pool.get().await.expect("conn");
        let mut rows = conn
            .query("SELECT COUNT(*) FROM thumbnails", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(col_i64(&row, 0, "count").unwrap(), 16);
        drop(conn);

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn toggle_favorite_flips_state() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");

        let conn = db.pool.get().await.expect("get conn");
        conn.execute(
            "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
            (),
        )
        .await
        .expect("insert image");
        drop(conn);

        let p = RelativePath(PathBuf::from("a.jpg"));
        assert!(!db.is_favorite(&p).await.unwrap());
        assert!(db.toggle_favorite(&p).await.unwrap());
        assert!(db.is_favorite(&p).await.unwrap());
        assert_eq!(db.list_favorites().await.unwrap(), vec![p.clone()]);
        assert!(!db.toggle_favorite(&p).await.unwrap());
        assert!(!db.is_favorite(&p).await.unwrap());

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn tag_and_collection_roundtrip() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");

        let conn = db.pool.get().await.expect("get conn");
        conn.execute(
            "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'h')",
            (),
        )
        .await
        .expect("insert image");
        drop(conn);

        let p = RelativePath(PathBuf::from("a.jpg"));
        db.tag_image(&p, "cats").await.unwrap();
        assert_eq!(
            db.tags_for_image(&p).await.unwrap(),
            vec!["cats".to_string()]
        );
        assert_eq!(db.images_by_tag("cats").await.unwrap(), vec![p.clone()]);
        db.untag_image(&p, "cats").await.unwrap();
        assert!(db.tags_for_image(&p).await.unwrap().is_empty());
        db.create_collection("trip").await.unwrap();
        db.add_to_collection("trip", &p).await.unwrap();
        assert_eq!(db.collection_images("trip").await.unwrap(), vec![p.clone()]);
        assert_eq!(
            db.list_collections().await.unwrap(),
            vec!["trip".to_string()]
        );

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn migrations_set_schema_meta_and_create_tables() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let conn = db.pool.get().await.unwrap();
        let mut rows = conn
            .query("SELECT version FROM schema_meta LIMIT 1", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(
            col_i64(&row, 0, "version").unwrap(),
            LATEST_MIGRATION_VERSION as i64
        );
        for t in [
            "images",
            "image_vectors",
            "thumbnails",
            "image_metadata",
            "favorites",
        ] {
            let mut trows = conn
                .query("SELECT count(*) FROM sqlite_master WHERE name = ?1", (t,))
                .await
                .unwrap();
            let trow = trows.next().await.unwrap().unwrap();
            assert_eq!(
                col_i64(&trow, 0, "count").unwrap(),
                1,
                "table {t} should exist"
            );
        }
        drop(conn);

        cleanup(&db_path);
    }

    /// Regression test for the pagination bug: the requested page must include
    /// rows past the first `limit` when `offset` is advanced.
    #[tokio::test]
    async fn search_meta_paginates_past_first_page() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let conn = db.pool.get().await.expect("get conn");

        // Insert 10 images with embeddings whose first component decreases as the
        // id grows. The query embedding is the unit vector along axis 0, so the
        // cosine distance increases monotonically with id.
        for id in 1..=10i64 {
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                (id, format!("img{id}.jpg"), format!("h{id}")),
            )
            .await
            .expect("insert image");

            let mut emb = vec![0.0f32; 512];
            emb[0] = 1.0 - (id as f32) * 0.01;
            emb[1] = (id as f32) * 0.01;
            conn.execute(
                "INSERT INTO image_vectors (image_id, embedding) VALUES (?1, ?2)",
                (Value::Integer(id), emb_blob(&emb)),
            )
            .await
            .expect("insert vector");
        }
        drop(conn);

        let mut query = vec![0.0f32; 512];
        query[0] = 1.0;

        let page1 = db
            .search_similar_images_meta(
                &query,
                4,
                0,
                DistanceThreshold(2.0),
                MaxK(100),
                &crate::filters::Filters::default(),
            )
            .await
            .expect("page 1");
        let page2 = db
            .search_similar_images_meta(
                &query,
                4,
                4,
                DistanceThreshold(2.0),
                MaxK(100),
                &crate::filters::Filters::default(),
            )
            .await
            .expect("page 2");

        assert_eq!(page1.len(), 4, "page 1 should be full");
        assert_eq!(page2.len(), 4, "page 2 should return the next slice");

        let p1_paths: Vec<&str> = page1.iter().map(|(_, p, ..)| p.as_str()).collect();
        let p2_paths: Vec<&str> = page2.iter().map(|(_, p, ..)| p.as_str()).collect();
        assert_eq!(p1_paths, ["img1.jpg", "img2.jpg", "img3.jpg", "img4.jpg"]);
        assert_eq!(p2_paths, ["img5.jpg", "img6.jpg", "img7.jpg", "img8.jpg"]);

        cleanup(&db_path);
    }

    /// Regression test: the requested page must be full even when
    /// `offset + limit > max_k`.
    #[tokio::test]
    async fn search_meta_paginates_past_max_k() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let conn = db.pool.get().await.expect("get conn");

        for id in 1..=60i64 {
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                (id, format!("img{id}.jpg"), format!("h{id}")),
            )
            .await
            .expect("insert image");

            let mut emb = vec![0.0f32; 512];
            emb[0] = 1.0 - (id as f32) * 0.01;
            emb[1] = (id as f32) * 0.01;
            conn.execute(
                "INSERT INTO image_vectors (image_id, embedding) VALUES (?1, ?2)",
                (Value::Integer(id), emb_blob(&emb)),
            )
            .await
            .expect("insert vector");
        }
        drop(conn);

        let mut query = vec![0.0f32; 512];
        query[0] = 1.0;

        let result = db
            .search_similar_images_meta(
                &query,
                20,
                35,
                DistanceThreshold(2.0),
                MaxK(40),
                &crate::filters::Filters::default(),
            )
            .await
            .expect("paginated search");

        assert_eq!(result.len(), 20, "page must be full (offset+limit > max_k)");

        cleanup(&db_path);
    }

    /// Regression test for the missing-thumbnail count path: counting the
    /// missing images and passing that count returns every missing image.
    #[tokio::test]
    async fn missing_thumbnail_limit_bind_uses_real_count() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let conn = db.pool.get().await.expect("get conn");

        for id in 1..=3i64 {
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                (id, format!("img{id}.jpg"), format!("h{id}")),
            )
            .await
            .expect("insert image");
        }
        drop(conn);

        // usize::MAX overflows the i64 LIMIT bind and is rejected.
        assert!(
            db.get_images_without_thumbnails(ThumbnailSize(300), usize::MAX)
                .await
                .is_err(),
            "usize::MAX must overflow the i64 LIMIT bind"
        );

        let missing = db
            .count_images_without_thumbnails(ThumbnailSize(300))
            .await
            .expect("count missing thumbnails must succeed");
        assert_eq!(missing, 3, "all three images are missing a 300px thumbnail");

        let rows = db
            .get_images_without_thumbnails(ThumbnailSize(300), missing)
            .await
            .expect("real-count LIMIT bind must succeed");
        assert_eq!(rows.len(), 3, "the count value covers all missing images");

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn migration_2_seeds_baseline_model_and_user_tables() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let conn = db.pool.get().await.unwrap();
        let mut rows = conn
            .query(
                "SELECT name, dim, table_name, is_active FROM models WHERE is_active = 1",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        let name = col_text(&row, 0, "name").unwrap();
        let dim = col_i64(&row, 1, "dim").unwrap();
        let table = col_text(&row, 2, "table_name").unwrap();
        let active = col_i64(&row, 3, "is_active").unwrap();
        assert_eq!((dim, table.as_str(), active), (512, "image_vectors", 1));
        assert!(name.contains("clip"));
        for t in [
            "tags",
            "image_tags",
            "collections",
            "collection_images",
            "models",
        ] {
            let mut trows = conn
                .query("SELECT count(*) FROM sqlite_master WHERE name=?1", (t,))
                .await
                .unwrap();
            let trow = trows.next().await.unwrap().unwrap();
            assert_eq!(col_i64(&trow, 0, "count").unwrap(), 1, "table {t} exists");
        }
        drop(conn);

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn active_model_defaults_to_baseline_table() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let m = db.active_model().await.unwrap();
        assert_eq!(m.dim, EmbeddingDim(512));
        assert_eq!(m.table, "image_vectors");

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn register_and_switch_model_creates_table_and_flips_active() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        db.register_model("test-model", EmbeddingDim(256))
            .await
            .unwrap();
        db.set_active_model("test-model").await.unwrap();
        let m = db.active_model().await.unwrap();
        assert_eq!(
            (m.dim, m.table.as_str()),
            (EmbeddingDim(256), "image_vectors_test_model")
        );
        let conn = db.pool.get().await.unwrap();
        let mut rows = conn
            .query(
                "SELECT count(*) FROM sqlite_master WHERE name='image_vectors_test_model'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(col_i64(&row, 0, "count").unwrap(), 1);
        drop(conn);

        cleanup(&db_path);
    }

    /// `is_image_indexed` is model-aware: an image indexed under one model is
    /// reported as NOT indexed after switching to a different (empty) model.
    #[tokio::test]
    async fn is_image_indexed_is_model_aware() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");

        let abs = RelativePath(PathBuf::from("photo.jpg")).to_absolute(&db.parent_dir);
        let hash = "deadbeef";

        assert!(!db.is_image_indexed(&abs, hash).await.unwrap());

        db.insert_images_batch(&[("photo.jpg".to_string(), hash.to_string(), vec![0.1f32; 512])])
            .await
            .unwrap();
        assert!(
            db.is_image_indexed(&abs, hash).await.unwrap(),
            "indexed under default model"
        );

        db.register_model("other-model", EmbeddingDim(8))
            .await
            .unwrap();
        db.set_active_model("other-model").await.unwrap();
        assert!(
            !db.is_image_indexed(&abs, hash).await.unwrap(),
            "not indexed under the freshly-switched model"
        );

        db.insert_images_batch(&[("photo.jpg".to_string(), hash.to_string(), vec![0.2f32; 8])])
            .await
            .unwrap();
        assert!(
            db.is_image_indexed(&abs, hash).await.unwrap(),
            "indexed under new model after backfill"
        );

        db.set_active_model("openai/clip-vit-base-patch32")
            .await
            .unwrap();
        assert!(
            db.is_image_indexed(&abs, hash).await.unwrap(),
            "default model embedding still present"
        );

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");
        let conn = db.pool.get().await.unwrap();
        crate::schema::run_migrations(&conn).await.unwrap(); // re-run: no-op
        let mut rows = conn
            .query("SELECT version FROM schema_meta LIMIT 1", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(
            col_i64(&row, 0, "version").unwrap(),
            LATEST_MIGRATION_VERSION as i64
        );
        drop(conn);

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn find_similar_to_path_returns_neighbors_from_stored_embedding() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");

        let mut a = vec![0.0f32; 512];
        a[0] = 1.0;
        let mut b = vec![0.0f32; 512];
        b[1] = 1.0;

        {
            let conn = db.pool.get().await.expect("conn");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (1, 'a.jpg', 'ha')",
                (),
            )
            .await
            .expect("img a");
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (2, 'b.jpg', 'hb')",
                (),
            )
            .await
            .expect("img b");
            conn.execute(
                "INSERT INTO image_vectors (image_id, embedding) VALUES (1, ?1)",
                (emb_blob(&a),),
            )
            .await
            .expect("vec a");
            conn.execute(
                "INSERT INTO image_vectors (image_id, embedding) VALUES (2, ?1)",
                (emb_blob(&b),),
            )
            .await
            .expect("vec b");
        }

        let rows = db
            .find_similar_to_path(
                &RelativePath(PathBuf::from("a.jpg")),
                10,
                0,
                DistanceThreshold(2.0),
                MaxK(100),
                &crate::filters::Filters::default(),
            )
            .await
            .expect("similar");
        let paths: Vec<&str> = rows.iter().map(|(_, p, _, _)| p.as_str()).collect();
        assert!(
            paths.contains(&"a.jpg"),
            "seed should appear among neighbours"
        );
        assert!(
            paths.contains(&"b.jpg"),
            "other image should appear among neighbours"
        );
        assert_eq!(rows[0].1, "a.jpg");

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn browse_filters_by_size_type_and_gps() {
        use crate::filters::{Filters, GpsFilter};
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("db");
        {
            let conn = db.pool.get().await.expect("conn");
            let rows = [
                (1, "a.jpg", 1000i64, Some(1.0f64), Some(2.0f64)),
                (2, "b.png", 5000, None, None),
                (3, "c.jpg", 9000, Some(3.0), Some(4.0)),
                (4, "d.nef", 200, None, None),
            ];
            for (id, path, size, lat, lon) in rows {
                conn.execute(
                    "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                    (id, path, format!("h{id}")),
                )
                .await
                .unwrap();
                conn.execute(
                    "INSERT INTO image_metadata (image_id, file_size, latitude, longitude) \
                     VALUES (?1, ?2, ?3, ?4)",
                    (
                        Value::Integer(id),
                        Value::Integer(size),
                        opt_f64(lat),
                        opt_f64(lon),
                    ),
                )
                .await
                .unwrap();
            }
        }

        let all = db.browse(&Filters::default(), 100, 0).await.unwrap();
        assert_eq!(all.len(), 4);

        let jpg = db
            .browse(
                &Filters {
                    extensions: vec!["jpg".into()],
                    ..Default::default()
                },
                100,
                0,
            )
            .await
            .unwrap();
        let p: Vec<&str> = jpg.iter().map(|(x, _)| x.as_str()).collect();
        assert_eq!(p.len(), 2);
        assert!(p.contains(&"a.jpg") && p.contains(&"c.jpg"));

        let sized = db
            .browse(
                &Filters {
                    size_min: Some(FileSize(500)),
                    size_max: Some(FileSize(6000)),
                    ..Default::default()
                },
                100,
                0,
            )
            .await
            .unwrap();
        let p: Vec<&str> = sized.iter().map(|(x, _)| x.as_str()).collect();
        assert!(
            p.contains(&"a.jpg")
                && p.contains(&"b.png")
                && !p.contains(&"c.jpg")
                && !p.contains(&"d.nef")
        );

        let gps = db
            .browse(
                &Filters {
                    gps: GpsFilter::HasGps,
                    ..Default::default()
                },
                100,
                0,
            )
            .await
            .unwrap();
        let p: Vec<&str> = gps.iter().map(|(x, _)| x.as_str()).collect();
        assert_eq!(p.len(), 2);
        assert!(p.contains(&"a.jpg") && p.contains(&"c.jpg"));

        cleanup(&db_path);
    }

    #[tokio::test]
    async fn distinct_extensions_and_size_bounds() {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("db");
        {
            let conn = db.pool.get().await.expect("conn");
            for (id, path, size) in [(1, "a.JPG", 10i64), (2, "b.png", 50), (3, "c.jpg", 30)] {
                conn.execute(
                    "INSERT INTO images (id, path, hash) VALUES (?1,?2,?3)",
                    (id, path, format!("h{id}")),
                )
                .await
                .unwrap();
                conn.execute(
                    "INSERT INTO image_metadata (image_id, file_size) VALUES (?1,?2)",
                    (id, size),
                )
                .await
                .unwrap();
            }
        }
        let mut exts = db.distinct_extensions().await.unwrap();
        exts.sort();
        assert_eq!(exts, vec!["jpg".to_string(), "png".to_string()]);
        assert_eq!(db.file_size_bounds().await.unwrap(), (10, 50));
        cleanup(&db_path);
    }

    #[tokio::test]
    async fn filtered_vector_search_excludes_nonmatching_types() {
        use crate::filters::Filters;
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("db");
        {
            let conn = db.pool.get().await.expect("conn");
            let mut a = vec![0.0f32; 512];
            a[0] = 1.0;
            let b = a.clone();
            for (id, path, emb) in [(1, "a.jpg", &a), (2, "b.png", &b)] {
                conn.execute(
                    "INSERT INTO images (id, path, hash) VALUES (?1,?2,?3)",
                    (id, path, format!("h{id}")),
                )
                .await
                .unwrap();
                conn.execute(
                    "INSERT INTO image_metadata (image_id, file_size) VALUES (?1, 1000)",
                    (id,),
                )
                .await
                .unwrap();
                conn.execute(
                    "INSERT INTO image_vectors (image_id, embedding) VALUES (?1, ?2)",
                    (Value::Integer(id), emb_blob(emb)),
                )
                .await
                .unwrap();
            }
        }
        let mut q = vec![0.0f32; 512];
        q[0] = 1.0;
        let jpg_only = Filters {
            extensions: vec!["jpg".into()],
            ..Default::default()
        };
        let rows = db
            .search_similar_images_meta(&q, 80, 0, DistanceThreshold(1.3), MaxK(100), &jpg_only)
            .await
            .unwrap();
        let paths: Vec<&str> = rows.iter().map(|(_, p, _, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.jpg"));
        assert!(
            !paths.contains(&"b.png"),
            "png filtered out of vector results"
        );
        let both = db
            .search_similar_images_meta(
                &q,
                80,
                0,
                DistanceThreshold(1.3),
                MaxK(100),
                &Filters::default(),
            )
            .await
            .unwrap();
        assert_eq!(both.len(), 2);
        cleanup(&db_path);
    }

    #[tokio::test]
    async fn extracts_metadata_from_raw_fixture() {
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.dng");
        let md = extract_image_metadata(fixture).expect("metadata extraction");
        assert!(
            md.width.is_some() && md.height.is_some(),
            "dimensions populated"
        );
        assert!(
            md.camera_make.is_some() || md.camera_model.is_some() || md.datetime_taken.is_some(),
            "some EXIF field populated"
        );
    }

    // ── browse_all helpers & tests ────────────────────────────────────────────

    /// Build an isolated test `Database` pre-populated with `(path, file_size)` rows.
    ///
    /// Rows are inserted with sequential ids (1, 2, 3 …). An `image_metadata` row is
    /// always written for each image so the LEFT JOIN sees a match; `file_size` is set
    /// to the given `Option<i64>` (NULL when `None`).
    async fn test_db_with_rows(rows: &[(&str, Option<i64>)]) -> (Database, PathBuf) {
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create test db");
        {
            let conn = db.pool.get().await.expect("get conn");
            for (id, (path, size)) in rows.iter().enumerate() {
                let id = (id + 1) as i64;
                conn.execute(
                    "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                    (id, *path, format!("h{id}")),
                )
                .await
                .expect("insert image");
                conn.execute(
                    "INSERT INTO image_metadata (image_id, file_size) VALUES (?1, ?2)",
                    (Value::Integer(id), opt_i64(*size)),
                )
                .await
                .expect("insert metadata");
            }
        }
        (db, db_path)
    }

    #[tokio::test]
    async fn browse_all_sorts_by_size_then_name_nulls_last() {
        use crate::filters::Filters;
        use crate::sort::{Sort, SortDir, SortKey};

        let (db, tmp) = test_db_with_rows(&[
            ("b.jpg", Some(10)),
            ("a.jpg", None),
            ("c.jpg", Some(10)),
            ("d.jpg", Some(5)),
        ])
        .await;
        let rows = db
            .browse_all(
                &Filters::default(),
                &Sort {
                    key: SortKey::Size,
                    dir: SortDir::Asc,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["d.jpg", "b.jpg", "c.jpg", "a.jpg"]
        );
        cleanup(&tmp);
    }

    #[tokio::test]
    async fn browse_all_sorts_by_type_then_name() {
        use crate::filters::Filters;
        use crate::sort::{Sort, SortDir, SortKey};

        let (db, tmp) =
            test_db_with_rows(&[("z.PNG", None), ("a.png", None), ("m.jpg", None)]).await;
        let rows = db
            .browse_all(
                &Filters::default(),
                &Sort {
                    key: SortKey::Type,
                    dir: SortDir::Asc,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["m.jpg", "a.png", "z.PNG"]
        );
        assert_eq!(rows[2].ext, "png");
        cleanup(&tmp);
    }

    #[tokio::test]
    async fn browse_all_name_desc() {
        use crate::filters::Filters;
        use crate::sort::{Sort, SortDir, SortKey};

        let (db, tmp) = test_db_with_rows(&[("a.jpg", None), ("b.jpg", None)]).await;
        let rows = db
            .browse_all(
                &Filters::default(),
                &Sort {
                    key: SortKey::Name,
                    dir: SortDir::Desc,
                },
            )
            .await
            .unwrap();
        assert_eq!(rows[0].path, "b.jpg");
        cleanup(&tmp);
    }

    /// Pin the invariant that `browse_all(Filters::default(), …)` returns **every**
    /// indexed image (the GUI's clear-search path relies on exactly this).
    #[tokio::test]
    async fn browse_all_default_filters_returns_all() {
        use crate::filters::Filters;
        use crate::sort::{Sort, SortDir, SortKey};

        let (db, tmp) =
            test_db_with_rows(&[("a.jpg", Some(1)), ("b.jpg", Some(2)), ("c.jpg", Some(3))]).await;
        let rows = db
            .browse_all(
                &Filters::default(),
                &Sort {
                    key: SortKey::Name,
                    dir: SortDir::Asc,
                },
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 3, "default filters must return every image");
        cleanup(&tmp);
    }

    // ── rehydrate_rows tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn rehydrate_preserves_order_and_drops_missing() {
        let (db, tmp) =
            test_db_with_rows(&[("a.jpg", Some(1)), ("b.jpg", Some(2)), ("c.jpg", Some(3))]).await;
        let want = vec![ImageId(3), ImageId(999), ImageId(1)];
        let rows = db.rehydrate_rows(&want).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec!["c.jpg", "a.jpg"]
        );
        cleanup(&tmp);
    }

    #[tokio::test]
    async fn rehydrate_empty_is_empty() {
        let (db, tmp) = test_db_with_rows(&[("a.jpg", Some(1))]).await;
        assert!(db.rehydrate_rows(&[]).await.unwrap().is_empty());
        cleanup(&tmp);
    }

    /// Characterization test for `rehydrate_rows`: input-id order preserved, missing
    /// ids dropped, and the metadata LEFT JOIN populates `size`.
    #[tokio::test]
    async fn rehydrate_rows_ordered_with_metadata_populated() {
        let (db, tmp) = test_db_with_rows(&[
            ("img1.jpg", Some(100)),
            ("img2.jpg", Some(200)),
            ("img3.jpg", Some(300)),
        ])
        .await;
        let ids = vec![ImageId(3), ImageId(999), ImageId(1), ImageId(2)];
        let rows = db.rehydrate_rows(&ids).await.unwrap();

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, ImageId(3));
        assert_eq!(rows[1].id, ImageId(1));
        assert_eq!(rows[2].id, ImageId(2));

        assert_eq!(rows[0].path, "img3.jpg");
        assert_eq!(rows[1].path, "img1.jpg");
        assert_eq!(rows[2].path, "img2.jpg");

        assert_eq!(rows[0].size, Some(FileSize(300)));
        assert_eq!(rows[1].size, Some(FileSize(100)));
        assert_eq!(rows[2].size, Some(FileSize(200)));

        cleanup(&tmp);
    }

    #[tokio::test]
    async fn ui_state_round_trips_through_db() {
        let (db, tmp) = test_db_with_rows(&[("a.jpg", Some(1))]).await;
        assert!(db.get_ui_state().await.unwrap().is_none());
        let st = UiState {
            search_text: "dog".into(),
            result_ids: vec![ImageId(1)],
            selected_index: Some(0),
            ..Default::default()
        };
        db.set_ui_state(&st).await.unwrap();
        assert_eq!(db.get_ui_state().await.unwrap().unwrap(), st);
        let st2 = UiState {
            search_text: "cat".into(),
            result_ids: vec![ImageId(1)],
            selected_index: Some(0),
            ..Default::default()
        };
        db.set_ui_state(&st2).await.unwrap();
        assert_eq!(db.get_ui_state().await.unwrap().unwrap().search_text, "cat");

        cleanup(&tmp);
    }

    #[tokio::test]
    async fn malformed_ui_state_is_none() {
        let (db, tmp) = test_db_with_rows(&[("a.jpg", Some(1))]).await;
        let conn = db.pool.get().await.unwrap();
        conn.execute(
            "INSERT INTO ui_state (id, state_json) VALUES (1, '{not json')",
            (),
        )
        .await
        .unwrap();
        drop(conn);
        assert!(db.get_ui_state().await.unwrap().is_none());

        cleanup(&tmp);
    }

    /// Convenience wrapper: build a temp DB with image rows (no file-size metadata).
    async fn test_db_with_images(paths: &[&str]) -> (Database, PathBuf) {
        let rows: Vec<(&str, Option<i64>)> = paths.iter().map(|p| (*p, None)).collect();
        test_db_with_rows(&rows).await
    }

    /// Convenience constructor for a `RelativePath` from a string literal.
    fn rel(s: &str) -> RelativePath {
        RelativePath(PathBuf::from(s))
    }

    #[tokio::test]
    async fn browse_all_filters_by_tags_all_and_any() {
        use crate::filters::{Filters, TagFilter, TagMatch};
        use crate::sort::Sort;

        let (db, tmp) = test_db_with_images(&["a.jpg", "b.jpg", "c.jpg"]).await;
        db.tag_image(&rel("a.jpg"), "beach").await.unwrap();
        db.tag_image(&rel("a.jpg"), "sunset").await.unwrap();
        db.tag_image(&rel("b.jpg"), "beach").await.unwrap();

        let all_beach_sunset = Filters {
            tag_filter: TagFilter::Active {
                tags: vec!["beach".into(), "sunset".into()],
                match_mode: TagMatch::AllOf,
            },
            ..Default::default()
        };
        let got = db
            .browse_all(&all_beach_sunset, &Sort::default())
            .await
            .unwrap();
        let paths: Vec<&str> = got.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a.jpg"]);

        let any_beach_sunset = Filters {
            tag_filter: TagFilter::Active {
                tags: vec!["beach".into(), "sunset".into()],
                match_mode: TagMatch::AnyOf,
            },
            ..Default::default()
        };
        let mut got = db
            .browse_all(&any_beach_sunset, &Sort::default())
            .await
            .unwrap();
        got.sort_by(|x, y| x.path.cmp(&y.path));
        let paths: Vec<&str> = got.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["a.jpg", "b.jpg"]);

        let disabled = Filters {
            tag_filter: TagFilter::Inactive {
                tags: vec!["beach".into(), "sunset".into()],
                match_mode: TagMatch::AllOf,
            },
            ..Default::default()
        };
        let got = db.browse_all(&disabled, &Sort::default()).await.unwrap();
        assert_eq!(got.len(), 3);

        cleanup(&tmp);
    }

    #[tokio::test]
    async fn batch_tag_and_untag_images() {
        let (db, tmp) = test_db_with_images(&["p1.jpg", "p2.jpg", "p3.jpg"]).await;

        db.batch_tag_images(&["p1.jpg", "p2.jpg", "p3.jpg"], "beach")
            .await
            .unwrap();
        assert_eq!(
            db.tags_for_image(&rel("p1.jpg")).await.unwrap(),
            vec!["beach"]
        );
        assert_eq!(
            db.tags_for_image(&rel("p2.jpg")).await.unwrap(),
            vec!["beach"]
        );
        assert_eq!(
            db.tags_for_image(&rel("p3.jpg")).await.unwrap(),
            vec!["beach"]
        );

        db.batch_untag_images(&["p1.jpg", "p3.jpg"], "beach")
            .await
            .unwrap();
        assert_eq!(
            db.tags_for_image(&rel("p1.jpg")).await.unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            db.tags_for_image(&rel("p2.jpg")).await.unwrap(),
            vec!["beach"]
        );
        assert_eq!(
            db.tags_for_image(&rel("p3.jpg")).await.unwrap(),
            Vec::<String>::new()
        );

        db.batch_tag_images(&["p2.jpg"], "beach").await.unwrap();
        assert_eq!(
            db.tags_for_image(&rel("p2.jpg")).await.unwrap(),
            vec!["beach"]
        );

        db.batch_tag_images(&["p1.jpg", "nonexistent.jpg"], "beach")
            .await
            .unwrap();
        assert_eq!(
            db.tags_for_image(&rel("p1.jpg")).await.unwrap(),
            vec!["beach"]
        );

        cleanup(&tmp);
    }

    #[tokio::test]
    async fn full_size_thumbnail_round_trips_and_is_distinct_from_scaled() {
        use crate::{ThumbnailSize, ThumbnailSpec};
        let db_path = temp_db_path();
        let db = Database::new(&db_path).await.expect("create db");

        db.insert_thumbnail("h", ThumbnailSpec::FullSize, &[1, 2, 3])
            .await
            .unwrap();
        db.insert_thumbnail("h", ThumbnailSize(2048), &[9, 9])
            .await
            .unwrap();

        // FullSize stored under size=0, retrievable by the enum (not the integer).
        assert_eq!(
            db.get_thumbnail("h", ThumbnailSpec::FullSize)
                .await
                .unwrap(),
            vec![1, 2, 3]
        );
        // Distinct from the scaled row for the same hash — no key collision.
        assert_eq!(
            db.get_thumbnail("h", ThumbnailSize(2048)).await.unwrap(),
            vec![9, 9]
        );

        cleanup(&db_path);
    }

    /// One-image fixture returning `(db, relative_path)`.
    async fn test_db_with_one_image() -> (Database, RelativePath) {
        let (db, _path) = test_db_with_images(&["a.jpg"]).await;
        (db, rel("a.jpg"))
    }

    /// One-image fixture returning `(db, relative_path, hash)`.
    async fn test_db_with_one_image_hash() -> (Database, RelativePath, String) {
        let (db, _path) = test_db_with_images(&["a.jpg"]).await;
        // test_db_with_rows inserts hash = format!("h{id}") with id=1 => "h1"
        (db, rel("a.jpg"), "h1".to_string())
    }

    #[tokio::test]
    async fn image_edits_upsert_and_read() {
        use crate::edits::ImageEdits;
        let (db, rel_path) = test_db_with_one_image().await;
        // Absent => identity
        assert!(db.get_image_edits(&rel_path).await.unwrap().is_identity());
        // Insert — all six fields round-trip
        db.set_image_edits(
            &rel_path,
            &ImageEdits {
                exposure: 1.5,
                saturation: 40.0,
                blacks: -20.0,
                whites: 15.0,
                brightness: 10.0,
                contrast: -5.0,
            },
        )
        .await
        .unwrap();
        let got = db.get_image_edits(&rel_path).await.unwrap();
        assert_eq!(got.exposure, 1.5);
        assert_eq!(got.saturation, 40.0);
        assert_eq!(got.blacks, -20.0);
        assert_eq!(got.whites, 15.0);
        assert_eq!(got.brightness, 10.0);
        assert_eq!(got.contrast, -5.0);
        // Update same row (no duplicate)
        db.set_image_edits(
            &rel_path,
            &ImageEdits {
                exposure: -0.75,
                ..ImageEdits::identity()
            },
        )
        .await
        .unwrap();
        assert_eq!(db.get_image_edits(&rel_path).await.unwrap().exposure, -0.75);
    }

    #[tokio::test]
    async fn thumbnail_sizes_lists_distinct() {
        let (db, _rel_path, hash) = test_db_with_one_image_hash().await;
        db.insert_thumbnail(&hash, ThumbnailSize(300), &[1, 2, 3])
            .await
            .unwrap();
        db.insert_thumbnail(&hash, ThumbnailSize(512), &[4, 5, 6])
            .await
            .unwrap();
        let mut sizes = db.get_thumbnail_sizes(&hash).await.unwrap();
        sizes.sort_unstable();
        assert_eq!(sizes, vec![300, 512]);
    }
}
