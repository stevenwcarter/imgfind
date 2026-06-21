//! One-time migrator: legacy `rusqlite` + `sqlite-vec` database → Turso.
//!
//! This is the **only** module in the crate that may use `rusqlite`,
//! `sqlite-vec`, or `zerocopy`. It opens an existing legacy database
//! (`vec0` virtual-table embeddings), copies every row verbatim into a fresh
//! Turso database (`F32_BLOB` embedding columns), and atomically swaps the
//! Turso DB into the canonical location, keeping the legacy file as a backup.
//!
//! No CLIP re-embedding happens: embeddings are copied as raw little-endian f32
//! blobs, byte for byte, so the migration is fast and lossless. Image ids are
//! preserved with explicit `INSERT … (id, …)` writes so every foreign key and
//! per-model embedding link survives.
//!
//! ## Detection probe
//!
//! Turso happily *opens* a legacy `vec0` file (the engine only parses the DDL
//! lazily), so an open-failure probe is unreliable. Instead the canonical file
//! is classed as **already migrated** when Turso can open it *and* a
//! `schema_meta` table is present — that table only exists in the Turso schema
//! runner ([`crate::schema`]). Absence of `schema_meta` (with `images` present)
//! means a legacy DB that still needs migrating.

use crate::db_pool::TursoPool;
use crate::schema;
use anyhow::{Context, Result};
use rusqlite::ffi::sqlite3_auto_extension;
use sqlite_vec::sqlite3_vec_init;
use std::path::{Path, PathBuf};
use tracing::info;

/// Outcome of a [`migrate_db`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrateOutcome {
    /// The canonical file was already a Turso database; nothing was touched.
    AlreadyMigrated,
    /// A legacy database was migrated. Counts are copied-row totals.
    Migrated {
        images: usize,
        embeddings: usize,
        thumbnails: usize,
    },
}

/// One legacy `images` row: `(id, path, hash, created_at)`.
type ImageRow = (i64, String, String, Option<String>);

/// Embeddings for one vector table: `(table_name, [(image_id, raw_le_f32_blob)])`.
type EmbeddingTable = (String, Vec<(i64, Vec<u8>)>);

/// A registered embedding model, as read from the legacy `models` table.
struct LegacyModel {
    name: String,
    dim: usize,
    table_name: String,
    is_active: bool,
}

/// Initialise the `sqlite-vec` auto-extension so every subsequent rusqlite
/// connection can read `vec0` virtual tables. Idempotent across calls; the
/// legacy `Database::new` did exactly this before the Turso migration.
fn init_sqlite_vec() {
    // SAFETY: matches the legacy initialisation. `sqlite3_vec_init` has the
    // signature SQLite expects for an auto-extension entry point; the transmute
    // only adjusts the calling-convention wrapper, as in the original code.
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
}

/// Does the Turso database at `path` already carry the migrated schema?
///
/// Returns `Ok(true)` when Turso opens the file and a `schema_meta` table
/// exists (the marker of a Turso-managed schema). Any open/query error is
/// treated as "not a Turso DB" → `Ok(false)`, so a legacy file reads as
/// needing migration rather than erroring out.
async fn is_already_turso(path: &Path) -> bool {
    table_exists(path, "schema_meta").await
}

/// Open `path` with Turso and check whether `table` exists. Any open/query
/// error reads as "not present" (`false`), so a legacy file never errors here.
async fn table_exists(path: &Path, table: &str) -> bool {
    let Some(path_str) = path.to_str() else {
        return false;
    };
    let Ok(db) = turso::Builder::new_local(path_str).build().await else {
        return false;
    };
    let Ok(conn) = db.connect() else {
        return false;
    };
    let Ok(mut rows) = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [table],
        )
        .await
    else {
        return false;
    };
    matches!(rows.next().await, Ok(Some(_)))
}

/// Is the file at `path` a legacy (pre-Turso) database that needs migrating?
///
/// True when the file exists, lacks the Turso `schema_meta` marker, yet has the
/// `images` table — i.e. an old rusqlite/sqlite-vec database. A missing file
/// (a brand-new DB about to be created) or an already-migrated Turso DB is not
/// legacy. Used by the CLI to print a migration hint instead of crashing.
pub async fn is_legacy_db(path: &Path) -> bool {
    path.exists() && !is_already_turso(path).await && table_exists(path, "images").await
}

/// Migrate the legacy database at `canonical_path` into a fresh Turso database.
///
/// Idempotent: if the file is already a Turso DB this is a no-op returning
/// [`MigrateOutcome::AlreadyMigrated`]. Otherwise the legacy file is renamed to
/// `*.db.rusqlite.bak` (refused if that backup already exists unless `force`)
/// and the migrated Turso DB takes its place.
pub async fn migrate_db(canonical_path: &Path, force: bool) -> Result<MigrateOutcome> {
    if is_already_turso(canonical_path).await {
        return Ok(MigrateOutcome::AlreadyMigrated);
    }
    anyhow::ensure!(
        canonical_path.exists(),
        "no database found at {canonical_path:?}"
    );

    let tmp = canonical_path.with_extension("db.turso.tmp");
    let bak = canonical_path.with_extension("db.rusqlite.bak");
    anyhow::ensure!(
        force || !bak.exists(),
        "backup {bak:?} already exists; pass --force to overwrite"
    );
    // A leftover temp from a previous aborted run must not pollute the new DB.
    if tmp.exists() {
        std::fs::remove_file(&tmp).with_context(|| format!("remove stale temp {tmp:?}"))?;
    }

    let legacy = LegacyData::read(canonical_path).context("read legacy database")?;
    let counts = legacy.counts();
    write_turso(&tmp, &legacy)
        .await
        .context("write Turso database")?;
    // `write_turso` checkpoints and drops its pool before returning, but the
    // temp turso may still leave empty `-wal`/`-shm` sidecars; clear them so
    // they cannot survive the rename next to the canonical DB.
    remove_sqlite_sidecars(&tmp).context("clean temp turso sidecars")?;

    // Back up the (now self-contained) legacy DB first, then move its redundant
    // legacy sidecars aside too so the canonical name keeps no stale rusqlite
    // WAL/SHM. The backup must land before the new DB is installed so a failure
    // never leaves the canonical name empty.
    std::fs::rename(canonical_path, &bak)
        .with_context(|| format!("back up legacy DB to {bak:?}"))?;
    // The legacy WAL was folded into the `.db` on read, so these are redundant;
    // drop them so only `imgfind.db` + `imgfind.db.rusqlite.bak` remain.
    remove_sqlite_sidecars(canonical_path).context("clean legacy sidecars")?;
    if let Err(e) = std::fs::rename(&tmp, canonical_path) {
        // Roll the backup back into place so the canonical file is never lost.
        let _ = std::fs::rename(&bak, canonical_path);
        return Err(e).with_context(|| format!("install migrated DB at {canonical_path:?}"));
    }

    info!(
        images = counts.0,
        embeddings = counts.1,
        thumbnails = counts.2,
        "migrated legacy database to Turso"
    );
    Ok(MigrateOutcome::Migrated {
        images: counts.0,
        embeddings: counts.1,
        thumbnails: counts.2,
    })
}

/// Everything read out of the legacy database, held in memory for the writer.
struct LegacyData {
    models: Vec<LegacyModel>,
    images: Vec<ImageRow>,
    embeddings: Vec<EmbeddingTable>,
    thumbnails: Vec<(String, i64, Vec<u8>)>,
    metadata: Vec<MetadataRow>,
    favorites: Vec<(i64, Option<String>)>,
    tags: Vec<(i64, String)>,
    image_tags: Vec<(i64, i64)>,
    collections: Vec<(i64, String)>,
    collection_images: Vec<(i64, i64)>,
    ui_state: Option<String>,
}

/// A row of the legacy `image_metadata` table.
struct MetadataRow {
    image_id: i64,
    file_size: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    camera_make: Option<String>,
    camera_model: Option<String>,
    datetime_taken: Option<String>,
}

impl LegacyData {
    /// `(images, embeddings, thumbnails)` row counts.
    fn counts(&self) -> (usize, usize, usize) {
        let embeddings = self.embeddings.iter().map(|(_, v)| v.len()).sum();
        (self.images.len(), embeddings, self.thumbnails.len())
    }

    /// Read every table from the legacy database into memory.
    ///
    /// Before reading, the legacy DB is opened writable and its WAL is folded
    /// into the main `.db` file via `wal_checkpoint(TRUNCATE)`, so the backup we
    /// later make by renaming the `.db` is self-contained (the legacy `-wal`/
    /// `-shm` sidecars become empty/redundant and are cleaned up post-rename).
    fn read(path: &Path) -> Result<Self> {
        init_sqlite_vec();
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("open legacy database at {path:?}"))?;
        checkpoint_truncate(&conn).context("checkpoint legacy WAL into main DB")?;

        let models = read_models(&conn)?;

        let images = read_images(&conn)?;

        let mut embeddings = Vec::with_capacity(models.len());
        for m in &models {
            let rows = read_embeddings(&conn, &m.table_name)
                .with_context(|| format!("read embeddings from {}", m.table_name))?;
            embeddings.push((m.table_name.clone(), rows));
        }

        let thumbnails = read_thumbnails(&conn)?;
        let metadata = read_metadata(&conn)?;
        let favorites = read_favorites(&conn)?;
        let tags = read_id_name(&conn, "SELECT id, name FROM tags")?;
        let image_tags = read_id_pairs(&conn, "SELECT image_id, tag_id FROM image_tags")?;
        let collections = read_id_name(&conn, "SELECT id, name FROM collections")?;
        let collection_images = read_id_pairs(
            &conn,
            "SELECT collection_id, image_id FROM collection_images",
        )?;
        let ui_state = read_ui_state(&conn)?;

        Ok(Self {
            models,
            images,
            embeddings,
            thumbnails,
            metadata,
            favorites,
            tags,
            image_tags,
            collections,
            collection_images,
            ui_state,
        })
    }
}

/// Fold a rusqlite connection's WAL fully into its main `.db` file. After this
/// the `.db` is self-contained; the `-wal`/`-shm` sidecars are empty/redundant.
fn checkpoint_truncate(conn: &rusqlite::Connection) -> Result<()> {
    // `wal_checkpoint(TRUNCATE)` returns a `(busy, log, checkpointed)` row;
    // `query_row` consumes it so rusqlite does not complain about the result.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .context("PRAGMA wal_checkpoint(TRUNCATE)")
}

/// Remove the `-wal` and `-shm` sidecars beside `db_path`, if present. SQLite
/// sidecars are the main path with `-wal`/`-shm` appended to the file name.
fn remove_sqlite_sidecars(db_path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(suffix);
        let sidecar = PathBuf::from(name);
        if sidecar.exists() {
            std::fs::remove_file(&sidecar)
                .with_context(|| format!("remove sidecar {sidecar:?}"))?;
        }
    }
    Ok(())
}

fn read_models(conn: &rusqlite::Connection) -> Result<Vec<LegacyModel>> {
    let mut stmt = conn
        .prepare("SELECT name, dim, table_name, is_active FROM models ORDER BY name")
        .context("prepare models query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LegacyModel {
                name: r.get(0)?,
                dim: r.get::<_, i64>(1)? as usize,
                table_name: r.get(2)?,
                is_active: r.get::<_, i64>(3)? != 0,
            })
        })
        .context("query models")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect models")
}

fn read_images(conn: &rusqlite::Connection) -> Result<Vec<ImageRow>> {
    let mut stmt = conn
        .prepare("SELECT id, path, hash, created_at FROM images ORDER BY id")
        .context("prepare images query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .context("query images")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect images")
}

/// Read `(rowid, embedding)` pairs from a `vec0` virtual table. The embedding
/// blob (raw little-endian f32) is copied verbatim; no f32 round-trip occurs.
fn read_embeddings(conn: &rusqlite::Connection, table: &str) -> Result<Vec<(i64, Vec<u8>)>> {
    anyhow::ensure!(
        table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "invalid vector table name: {table}"
    );
    let mut stmt = conn
        .prepare(&format!("SELECT rowid, embedding FROM {table}"))
        .with_context(|| format!("prepare embeddings query for {table}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .with_context(|| format!("query embeddings for {table}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("collect embeddings for {table}"))
}

fn read_thumbnails(conn: &rusqlite::Connection) -> Result<Vec<(String, i64, Vec<u8>)>> {
    let mut stmt = conn
        .prepare("SELECT image_hash, size, thumbnail_data FROM thumbnails")
        .context("prepare thumbnails query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .context("query thumbnails")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect thumbnails")
}

fn read_metadata(conn: &rusqlite::Connection) -> Result<Vec<MetadataRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT image_id, file_size, width, height, latitude, longitude, \
                    camera_make, camera_model, datetime_taken \
             FROM image_metadata",
        )
        .context("prepare metadata query")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(MetadataRow {
                image_id: r.get(0)?,
                file_size: r.get(1)?,
                width: r.get(2)?,
                height: r.get(3)?,
                latitude: r.get(4)?,
                longitude: r.get(5)?,
                camera_make: r.get(6)?,
                camera_model: r.get(7)?,
                datetime_taken: r.get(8)?,
            })
        })
        .context("query metadata")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect metadata")
}

fn read_favorites(conn: &rusqlite::Connection) -> Result<Vec<(i64, Option<String>)>> {
    let mut stmt = conn
        .prepare("SELECT image_id, created_at FROM favorites")
        .context("prepare favorites query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("query favorites")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect favorites")
}

fn read_id_name(conn: &rusqlite::Connection, sql: &str) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(sql).context("prepare id/name query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("query id/name rows")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect id/name rows")
}

fn read_id_pairs(conn: &rusqlite::Connection, sql: &str) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare(sql).context("prepare id-pair query")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .context("query id-pair rows")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("collect id-pair rows")
}

fn read_ui_state(conn: &rusqlite::Connection) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT state_json FROM ui_state WHERE id = 1", [], |r| {
        r.get(0)
    })
    .optional()
    .context("read ui_state")
}

/// Build the Turso database at `tmp` from the in-memory legacy data.
async fn write_turso(tmp: &Path, data: &LegacyData) -> Result<()> {
    let pool = TursoPool::open(tmp, 1)
        .await
        .with_context(|| format!("open temp Turso DB at {tmp:?}"))?;
    let conn = pool.get().await.context("checkout Turso connection")?;
    schema::run_migrations(&conn)
        .await
        .context("run Turso schema migrations")?;

    write_models(&conn, &data.models).await?;
    write_images(&conn, &data.images).await?;
    write_embeddings(&conn, &data.embeddings).await?;
    write_thumbnails(&conn, &data.thumbnails).await?;
    write_metadata(&conn, &data.metadata).await?;
    write_favorites(&conn, &data.favorites).await?;
    write_id_name(
        &conn,
        "INSERT INTO tags (id, name) VALUES (?1, ?2)",
        &data.tags,
    )
    .await?;
    write_id_pairs(
        &conn,
        "INSERT INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
        &data.image_tags,
    )
    .await?;
    write_id_name(
        &conn,
        "INSERT INTO collections (id, name) VALUES (?1, ?2)",
        &data.collections,
    )
    .await?;
    write_id_pairs(
        &conn,
        "INSERT INTO collection_images (collection_id, image_id) VALUES (?1, ?2)",
        &data.collection_images,
    )
    .await?;
    if let Some(json) = &data.ui_state {
        conn.execute(
            "INSERT INTO ui_state (id, state_json) VALUES (1, ?1)",
            [turso::Value::Text(json.clone())],
        )
        .await
        .context("write ui_state")?;
    }

    // Fold the WAL back into the main DB file so the temp file is fully
    // self-contained: the atomic swap only renames the main file, so any rows
    // still living in the side WAL would otherwise be lost on rename.
    let mut wal = conn
        .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
        .await
        .context("checkpoint WAL into temp DB")?;
    while wal.next().await?.is_some() {}

    // Drop the connection and the pool so the temp DB's file handles are
    // released and any remaining WAL/SHM is flushed/closed before the caller
    // renames the temp file into the canonical location.
    drop(wal);
    drop(conn);
    drop(pool);
    Ok(())
}

/// Replace the migration-seeded `models` registry with the legacy one and
/// create each non-default model's `F32_BLOB` vector table. The baseline
/// `image_vectors` table already exists from the schema runner.
async fn write_models(conn: &turso::Connection, models: &[LegacyModel]) -> Result<()> {
    // The schema runner seeded the default model row already; clear it so the
    // legacy registry (including the legacy `is_active` flag) is authoritative.
    conn.execute("DELETE FROM models", ())
        .await
        .context("clear seeded models")?;
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin models tx")?;
    for m in models {
        if m.table_name != "image_vectors" {
            schema::create_vector_table(conn, &m.table_name, m.dim).await?;
        }
        tx.execute(
            "INSERT INTO models (name, dim, table_name, is_active) VALUES (?1, ?2, ?3, ?4)",
            (
                m.name.clone(),
                m.dim as i64,
                m.table_name.clone(),
                i64::from(m.is_active),
            ),
        )
        .await
        .with_context(|| format!("insert model {}", m.name))?;
    }
    tx.commit().await.context("commit models tx")?;
    Ok(())
}

async fn write_images(conn: &turso::Connection, images: &[ImageRow]) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin images tx")?;
    for (id, path, hash, created_at) in images {
        tx.execute(
            "INSERT INTO images (id, path, hash, created_at) VALUES (?1, ?2, ?3, ?4)",
            (
                *id,
                path.clone(),
                hash.clone(),
                opt_text(created_at.clone()),
            ),
        )
        .await
        .with_context(|| format!("insert image id {id}"))?;
    }
    tx.commit().await.context("commit images tx")?;
    Ok(())
}

/// Write embeddings as raw `F32_BLOB` bytes, byte-for-byte from the legacy
/// `vec0` blobs. Keyed by `image_id` so per-model links stay intact.
async fn write_embeddings(conn: &turso::Connection, embeddings: &[EmbeddingTable]) -> Result<()> {
    for (table, rows) in embeddings {
        anyhow::ensure!(
            table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid vector table name: {table}"
        );
        let sql = format!("INSERT INTO {table} (image_id, embedding) VALUES (?1, ?2)");
        let tx = conn
            .unchecked_transaction()
            .await
            .with_context(|| format!("begin embeddings tx for {table}"))?;
        for (image_id, blob) in rows {
            tx.execute(
                &sql,
                (
                    turso::Value::Integer(*image_id),
                    turso::Value::Blob(blob.clone()),
                ),
            )
            .await
            .with_context(|| format!("insert embedding image_id {image_id} into {table}"))?;
        }
        tx.commit()
            .await
            .with_context(|| format!("commit embeddings tx for {table}"))?;
    }
    Ok(())
}

async fn write_thumbnails(
    conn: &turso::Connection,
    thumbnails: &[(String, i64, Vec<u8>)],
) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin thumbnails tx")?;
    for (hash, size, blob) in thumbnails {
        tx.execute(
            "INSERT INTO thumbnails (image_hash, size, thumbnail_data) VALUES (?1, ?2, ?3)",
            (hash.clone(), *size, turso::Value::Blob(blob.clone())),
        )
        .await
        .with_context(|| format!("insert thumbnail {hash}/{size}"))?;
    }
    tx.commit().await.context("commit thumbnails tx")?;
    Ok(())
}

async fn write_metadata(conn: &turso::Connection, metadata: &[MetadataRow]) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin metadata tx")?;
    for m in metadata {
        tx.execute(
            "INSERT INTO image_metadata \
                (image_id, file_size, width, height, latitude, longitude, \
                 camera_make, camera_model, datetime_taken) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                m.image_id,
                opt_int(m.file_size),
                opt_int(m.width),
                opt_int(m.height),
                opt_real(m.latitude),
                opt_real(m.longitude),
                opt_text(m.camera_make.clone()),
                opt_text(m.camera_model.clone()),
                opt_text(m.datetime_taken.clone()),
            ),
        )
        .await
        .with_context(|| format!("insert metadata image_id {}", m.image_id))?;
    }
    tx.commit().await.context("commit metadata tx")?;
    Ok(())
}

/// Write favorites preserving each row's original `created_at`, so the
/// post-migration ordering (`list_favorites` orders by `created_at DESC`)
/// matches the source. Mirrors the explicit-id/timestamp `images` writer.
async fn write_favorites(
    conn: &turso::Connection,
    favorites: &[(i64, Option<String>)],
) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin favorites tx")?;
    for (id, created_at) in favorites {
        tx.execute(
            "INSERT INTO favorites (image_id, created_at) VALUES (?1, ?2)",
            (*id, opt_text(created_at.clone())),
        )
        .await
        .with_context(|| format!("insert favorite image_id {id}"))?;
    }
    tx.commit().await.context("commit favorites tx")?;
    Ok(())
}

async fn write_id_name(conn: &turso::Connection, sql: &str, rows: &[(i64, String)]) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin id/name tx")?;
    for (id, name) in rows {
        tx.execute(sql, (*id, name.clone()))
            .await
            .with_context(|| format!("insert id/name row {id}"))?;
    }
    tx.commit().await.context("commit id/name tx")?;
    Ok(())
}

async fn write_id_pairs(conn: &turso::Connection, sql: &str, rows: &[(i64, i64)]) -> Result<()> {
    let tx = conn
        .unchecked_transaction()
        .await
        .context("begin id-pair tx")?;
    for (a, b) in rows {
        tx.execute(sql, (*a, *b))
            .await
            .with_context(|| format!("insert id-pair row ({a}, {b})"))?;
    }
    tx.commit().await.context("commit id-pair tx")?;
    Ok(())
}

/// Map an `Option<String>` to a Turso value (`NULL` when `None`).
fn opt_text(v: Option<String>) -> turso::Value {
    v.map_or(turso::Value::Null, turso::Value::Text)
}

/// Map an `Option<i64>` to a Turso value (`NULL` when `None`).
fn opt_int(v: Option<i64>) -> turso::Value {
    v.map_or(turso::Value::Null, turso::Value::Integer)
}

/// Map an `Option<f64>` to a Turso value (`NULL` when `None`).
fn opt_real(v: Option<f64>) -> turso::Value {
    v.map_or(turso::Value::Null, turso::Value::Real)
}

/// The conventional backup path for a migrated canonical database.
pub fn backup_path(canonical_path: &Path) -> PathBuf {
    canonical_path.with_extension("db.rusqlite.bak")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("imgfind_migrate_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The SQLite sidecar path for `db_path` with `suffix` (`-wal` / `-shm`)
    /// appended to the full file name.
    fn sidecar(db_path: &Path, suffix: &str) -> PathBuf {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }

    /// 512-dim L2-normalised f32 vector seeded with a constant.
    fn unit_vec(seed: f32) -> Vec<f32> {
        let mut v: Vec<f32> = (0..512).map(|i| (i as f32 * seed + 0.1).sin()).collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    fn to_le_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    /// Build a legacy rusqlite + sqlite-vec database at `path` and return the
    /// three known embedding blobs (by image id) plus the still-open
    /// connection. The connection is returned (not dropped) so the data stays
    /// in the WAL: closing the last WAL connection auto-checkpoints, which
    /// would hide the very sidecars the migrator must handle. The caller drops
    /// it (after asserting the sidecars exist) before running `migrate_db`.
    fn build_legacy_db(path: &Path) -> (rusqlite::Connection, Vec<(i64, Vec<u8>)>) {
        init_sqlite_vec();
        let conn = rusqlite::Connection::open(path).unwrap();
        // Real legacy DBs ran in WAL mode, so they carry `-wal`/`-shm`
        // sidecars. Build the fixture the same way so the migrator's sidecar
        // handling is actually exercised. `journal_mode` returns a row.
        conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        // Never auto-fold the WAL, so the data really lives in the `-wal` file.
        conn.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
        conn.execute_batch(
            "CREATE TABLE images (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                path TEXT UNIQUE NOT NULL, hash TEXT NOT NULL, \
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
             CREATE VIRTUAL TABLE image_vectors USING vec0(embedding float[512]);
             CREATE TABLE thumbnails (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                image_hash TEXT NOT NULL, size INTEGER NOT NULL, \
                thumbnail_data BLOB NOT NULL, \
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP, UNIQUE(image_hash, size));
             CREATE TABLE image_metadata (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                image_id INTEGER NOT NULL, file_size INTEGER, width INTEGER, \
                height INTEGER, latitude REAL, longitude REAL, camera_make TEXT, \
                camera_model TEXT, datetime_taken DATETIME, \
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, UNIQUE(image_id));
             CREATE TABLE favorites (image_id INTEGER PRIMARY KEY, \
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE);
             CREATE TABLE models (name TEXT PRIMARY KEY, dim INTEGER NOT NULL, \
                table_name TEXT NOT NULL, is_active INTEGER NOT NULL DEFAULT 0, \
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE tags (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE image_tags (image_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, \
                PRIMARY KEY(image_id, tag_id), \
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, \
                FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE);
             CREATE TABLE collections (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT UNIQUE NOT NULL, created_at DATETIME DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE collection_images (collection_id INTEGER NOT NULL, \
                image_id INTEGER NOT NULL, PRIMARY KEY(collection_id, image_id), \
                FOREIGN KEY(collection_id) REFERENCES collections(id) ON DELETE CASCADE, \
                FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE);
             CREATE TABLE ui_state (id INTEGER PRIMARY KEY CHECK (id = 1), \
                state_json TEXT NOT NULL, updated_at DATETIME DEFAULT CURRENT_TIMESTAMP);",
        )
        .unwrap();

        // Default model + a non-default model with its own vec0 table.
        conn.execute(
            "INSERT INTO models (name, dim, table_name, is_active) \
                VALUES ('openai/clip-vit-base-patch32', 512, 'image_vectors', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO models (name, dim, table_name, is_active) \
                VALUES ('test/other-model', 512, 'image_vectors_test_other_model', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE VIRTUAL TABLE image_vectors_test_other_model USING vec0(embedding float[512])",
            [],
        )
        .unwrap();

        // Three images with known, distinct, normalized embeddings.
        let mut blobs = Vec::new();
        for id in 1..=3i64 {
            conn.execute(
                "INSERT INTO images (id, path, hash) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, format!("img{id}.jpg"), format!("hash{id}")],
            )
            .unwrap();
            let blob = to_le_bytes(&unit_vec(id as f32));
            conn.execute(
                "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                rusqlite::params![id, blob],
            )
            .unwrap();
            blobs.push((id, blob));
        }
        // Put one embedding in the non-default model's table too (image 1).
        conn.execute(
            "INSERT INTO image_vectors_test_other_model (rowid, embedding) VALUES (?1, ?2)",
            rusqlite::params![1i64, blobs[0].1.clone()],
        )
        .unwrap();

        // Two thumbnails.
        conn.execute(
            "INSERT INTO thumbnails (image_hash, size, thumbnail_data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["hash1", 300i64, vec![1u8, 2, 3, 4]],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO thumbnails (image_hash, size, thumbnail_data) VALUES (?1, ?2, ?3)",
            rusqlite::params!["hash2", 512i64, vec![5u8, 6, 7, 8, 9]],
        )
        .unwrap();

        // Metadata for image 1 (with GPS + camera), bare row for image 2.
        conn.execute(
            "INSERT INTO image_metadata \
                (image_id, file_size, width, height, latitude, longitude, \
                 camera_make, camera_model, datetime_taken) \
             VALUES (1, 1024, 800, 600, 37.5, -122.3, 'Canon', 'EOS', '2024-01-01 12:00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO image_metadata (image_id, file_size) VALUES (2, 2048)",
            [],
        )
        .unwrap();

        // Three favorites with DISTINCT created_at in a known order. By
        // `created_at DESC` (how `list_favorites` orders) this is [2, 3, 1]:
        // image 2 is newest, image 1 oldest.
        for (image_id, created_at) in [
            (1i64, "2024-01-01 00:00:00"),
            (3i64, "2024-02-01 00:00:00"),
            (2i64, "2024-03-01 00:00:00"),
        ] {
            conn.execute(
                "INSERT INTO favorites (image_id, created_at) VALUES (?1, ?2)",
                rusqlite::params![image_id, created_at],
            )
            .unwrap();
        }
        conn.execute("INSERT INTO tags (id, name) VALUES (1, 'sunset')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO image_tags (image_id, tag_id) VALUES (1, 1)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO collections (id, name) VALUES (1, 'trip')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO collection_images (collection_id, image_id) VALUES (1, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ui_state (id, state_json) VALUES (1, '{\"search\":\"x\"}')",
            [],
        )
        .unwrap();

        (conn, blobs)
    }

    async fn turso_blob(conn: &turso::Connection, sql: &str) -> Option<Vec<u8>> {
        let mut rows = conn.query(sql, ()).await.unwrap();
        let row = rows.next().await.unwrap()?;
        Some(row.get_value(0).unwrap().as_blob().unwrap().clone())
    }

    #[tokio::test]
    async fn migrates_images_embeddings_thumbnails_with_stable_ids() {
        let dir = temp_dir();
        let canonical = dir.join("imgfind.db");
        let (legacy_conn, source_blobs) = build_legacy_db(&canonical);

        // The fixture is genuinely WAL-mode with un-checkpointed data: the
        // `-wal` sidecar must exist before migration (otherwise this test
        // would not exercise the sidecar handling at all). Then close the
        // connection so the migrator can open the DB writable.
        assert!(
            sidecar(&canonical, "-wal").exists(),
            "fixture must be WAL-mode with a live -wal sidecar"
        );
        drop(legacy_conn);

        let outcome = migrate_db(&canonical, false).await.unwrap();
        assert_eq!(
            outcome,
            MigrateOutcome::Migrated {
                images: 3,
                embeddings: 4, // 3 in image_vectors + 1 in the non-default table
                thumbnails: 2,
            }
        );

        // Legacy file was moved aside as a backup.
        assert!(backup_path(&canonical).exists(), "backup must exist");
        let tmp = canonical.with_extension("db.turso.tmp");
        assert!(!tmp.exists(), "temp turso DB must be gone");

        // The canonical directory must contain ONLY the new turso `imgfind.db`
        // and its `.rusqlite.bak` backup — no orphaned rusqlite sidecars under
        // the canonical name, and no leaked temp turso DB/sidecars.
        assert!(
            !sidecar(&canonical, "-wal").exists(),
            "no orphaned imgfind.db-wal beside the canonical DB"
        );
        assert!(
            !sidecar(&canonical, "-shm").exists(),
            "no orphaned imgfind.db-shm beside the canonical DB"
        );
        assert!(!sidecar(&tmp, "-wal").exists(), "no leaked tmp turso -wal");
        assert!(!sidecar(&tmp, "-shm").exists(), "no leaked tmp turso -shm");

        let pool = TursoPool::open(&canonical, 1).await.unwrap();
        let conn = pool.get().await.unwrap();

        // Image ids / paths / hashes preserved exactly.
        let mut rows = conn
            .query("SELECT id, path, hash FROM images ORDER BY id", ())
            .await
            .unwrap();
        for id in 1..=3i64 {
            let row = rows.next().await.unwrap().unwrap();
            assert_eq!(row.get_value(0).unwrap().as_integer().copied(), Some(id));
            assert_eq!(
                row.get_value(1).unwrap().as_text().unwrap(),
                &format!("img{id}.jpg")
            );
            assert_eq!(
                row.get_value(2).unwrap().as_text().unwrap(),
                &format!("hash{id}")
            );
        }
        assert!(rows.next().await.unwrap().is_none());

        // Byte-exact embeddings in the default table.
        for (id, blob) in &source_blobs {
            let got = turso_blob(
                &conn,
                &format!("SELECT embedding FROM image_vectors WHERE image_id = {id}"),
            )
            .await
            .unwrap();
            assert_eq!(&got, blob, "embedding {id} must be byte-identical");
        }

        // Non-default model's table was created and carries image 1's embedding.
        let got = turso_blob(
            &conn,
            "SELECT embedding FROM image_vectors_test_other_model WHERE image_id = 1",
        )
        .await
        .unwrap();
        assert_eq!(&got, &source_blobs[0].1);

        // Byte-exact thumbnails.
        let t1 = turso_blob(
            &conn,
            "SELECT thumbnail_data FROM thumbnails WHERE image_hash='hash1' AND size=300",
        )
        .await
        .unwrap();
        assert_eq!(t1, vec![1u8, 2, 3, 4]);
        let t2 = turso_blob(
            &conn,
            "SELECT thumbnail_data FROM thumbnails WHERE image_hash='hash2' AND size=512",
        )
        .await
        .unwrap();
        assert_eq!(t2, vec![5u8, 6, 7, 8, 9]);

        // Metadata preserved (image 1 GPS + camera).
        let mut rows = conn
            .query(
                "SELECT file_size, latitude, camera_model FROM image_metadata WHERE image_id=1",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get_value(0).unwrap().as_integer().copied(), Some(1024));
        assert!((row.get_value(1).unwrap().as_real().copied().unwrap() - 37.5).abs() < 1e-9);
        assert_eq!(row.get_value(2).unwrap().as_text().unwrap(), "EOS");

        // Tag + link present.
        let mut rows = conn
            .query(
                "SELECT t.name FROM tags t JOIN image_tags it ON it.tag_id=t.id \
                 WHERE it.image_id=1",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get_value(0).unwrap().as_text().unwrap(), "sunset");

        // Favorites order preserved: `created_at DESC` (how `list_favorites`
        // orders) must yield [2, 3, 1] from the source timestamps, not the
        // insert order. A migration that drops `created_at` would default all
        // rows to migration time and lose this ordering.
        let mut rows = conn
            .query(
                "SELECT image_id FROM favorites ORDER BY created_at DESC",
                (),
            )
            .await
            .unwrap();
        let mut fav_order = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            fav_order.push(row.get_value(0).unwrap().as_integer().copied().unwrap());
        }
        assert_eq!(
            fav_order,
            vec![2, 3, 1],
            "favorites must keep their source created_at ordering"
        );
        let mut rows = conn
            .query("SELECT state_json FROM ui_state WHERE id=1", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get_value(0)
                .unwrap()
                .as_text()
                .unwrap(),
            "{\"search\":\"x\"}"
        );

        // KNN parity: querying with image 1's own embedding ranks image 1 first.
        let q = crate::vector_sql::knn_query("image_vectors", "id, distance", "", "", 3, 0, 2.0);
        let mut rows = conn
            .query(&q, [turso::Value::Blob(source_blobs[0].1.clone())])
            .await
            .unwrap();
        let first = rows.next().await.unwrap().unwrap();
        assert_eq!(
            first.get_value(0).unwrap().as_integer().copied(),
            Some(1),
            "nearest neighbor to image 1's embedding must be image 1"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn already_migrated_is_noop() {
        let dir = temp_dir();
        let canonical = dir.join("imgfind.db");

        // Build a fresh Turso DB at the path via the schema runner.
        {
            let pool = TursoPool::open(&canonical, 1).await.unwrap();
            let conn = pool.get().await.unwrap();
            schema::run_migrations(&conn).await.unwrap();
        }

        let outcome = migrate_db(&canonical, false).await.unwrap();
        assert_eq!(outcome, MigrateOutcome::AlreadyMigrated);
        // No backup created for an already-migrated DB.
        assert!(!backup_path(&canonical).exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
