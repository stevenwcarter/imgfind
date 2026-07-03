//! Async Turso schema runner.
//!
//! Manages the imgfind schema via a `schema_meta(version)` table instead of
//! `PRAGMA user_version`, which Turso does not reliably support. Each migration
//! is idempotent (`CREATE TABLE IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`),
//! and the version stamp is written once after all three succeed.
//!
//! # Produced public surface
//!
//! - [`LATEST_MIGRATION_VERSION`] — the highest migration version this module knows.
//! - [`run_migrations`] — apply all pending migrations to a Turso connection.
//! - [`sanitize_model_table`] — derive a safe SQL identifier from a model name.
//! - [`create_vector_table`] — create an `F32_BLOB` embedding table.

use anyhow::{Context, Result};

use crate::EmbeddingDim;

/// The highest migration version applied by this module.
pub const LATEST_MIGRATION_VERSION: i32 = 6;

/// Derive a safe SQL identifier from a model name.
///
/// ASCII alphanumeric characters are lowercased; everything else becomes `_`.
/// The result is prefixed with `image_vectors_` so it is clearly namespaced.
pub fn sanitize_model_table(name: &str) -> String {
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

/// Create a named `F32_BLOB` vector table keyed on `images.id`.
///
/// The table name is validated as a safe SQL identifier (only ASCII
/// alphanumerics and underscores are allowed) before interpolation to
/// prevent SQL injection from caller-supplied names.
pub async fn create_vector_table(
    conn: &turso::Connection,
    table: &str,
    dim: EmbeddingDim,
) -> Result<()> {
    anyhow::ensure!(
        !table.is_empty() && table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "invalid table name: {table}"
    );
    let dim = dim.get();
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
                image_id INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE, \
                embedding F32_BLOB({dim}) NOT NULL\
            )"
        ),
        (),
    )
    .await
    .with_context(|| format!("create vector table {table}"))?;
    Ok(())
}

/// Read (and initialise if absent) the current schema version from
/// `schema_meta`. Returns 0 when the table is freshly created.
async fn current_version(conn: &turso::Connection) -> Result<i32> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_meta (version INTEGER NOT NULL)",
        (),
    )
    .await
    .context("create schema_meta table")?;

    let mut rows = conn
        .query("SELECT version FROM schema_meta LIMIT 1", ())
        .await
        .context("query schema_meta version")?;

    if let Some(row) = rows.next().await.context("read schema_meta row")? {
        let v = row
            .get_value(0)
            .context("get version value")?
            .as_integer()
            .copied()
            .unwrap_or(0);
        Ok(v as i32)
    } else {
        conn.execute("INSERT INTO schema_meta (version) VALUES (0)", ())
            .await
            .context("seed schema_meta version=0")?;
        Ok(0)
    }
}

/// Apply all pending schema migrations to `conn`.
///
/// Gated on `schema_meta.version` — already-applied migrations are skipped.
/// The version is stamped after all migrations succeed, so a mid-run failure
/// leaves the version at its prior value and the next run retries cleanly.
pub async fn run_migrations(conn: &turso::Connection) -> Result<()> {
    let current = current_version(conn).await?;
    if current < 1 {
        migration_001_baseline(conn)
            .await
            .context("migration 1 (baseline schema)")?;
    }
    if current < 2 {
        migration_002_models_and_userdata(conn)
            .await
            .context("migration 2 (models + user data)")?;
    }
    if current < 3 {
        migration_003_ui_state(conn)
            .await
            .context("migration 3 (ui_state)")?;
    }
    if current < 4 {
        migration_004_image_edits(conn)
            .await
            .context("migration 4 (image_edits)")?;
    }
    if current < 5 {
        migration_005_edit_controls(conn)
            .await
            .context("migration 5 (edit control columns)")?;
    }
    if current < 6 {
        migration_006_thumbnail_failures(conn)
            .await
            .context("migration 6 (thumbnail failures)")?;
    }
    if current < LATEST_MIGRATION_VERSION {
        conn.execute(
            "UPDATE schema_meta SET version = ?1",
            [LATEST_MIGRATION_VERSION as i64],
        )
        .await
        .context("stamp schema_meta version")?;
    }
    Ok(())
}

/// Migration 1: baseline schema — images, vector table, thumbnails, metadata, favorites.
async fn migration_001_baseline(conn: &turso::Connection) -> Result<()> {
    // Core images table.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS images (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            path TEXT UNIQUE NOT NULL, \
            hash TEXT NOT NULL, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create images")?;

    // Vector embedding table (F32_BLOB instead of vec0 virtual table).
    create_vector_table(conn, "image_vectors", EmbeddingDim(512)).await?;

    // Path index for fast lookup.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_images_path ON images(path)",
        (),
    )
    .await
    .context("create idx_images_path")?;

    // Hash index for duplicate detection.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_images_hash ON images(hash)",
        (),
    )
    .await
    .context("create idx_images_hash")?;

    // Thumbnail cache.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS thumbnails (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            image_hash TEXT NOT NULL, \
            size INTEGER NOT NULL, \
            thumbnail_data BLOB NOT NULL, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
            UNIQUE(image_hash, size)\
        )",
        (),
    )
    .await
    .context("create thumbnails")?;

    // Thumbnail lookup index.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_thumbnails_hash_size ON thumbnails(image_hash, size)",
        (),
    )
    .await
    .context("create idx_thumbnails_hash_size")?;

    // EXIF / metadata table.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS image_metadata (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            image_id INTEGER NOT NULL, \
            file_size INTEGER, \
            width INTEGER, \
            height INTEGER, \
            latitude REAL, \
            longitude REAL, \
            camera_make TEXT, \
            camera_model TEXT, \
            datetime_taken DATETIME, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
            FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, \
            UNIQUE(image_id)\
        )",
        (),
    )
    .await
    .context("create image_metadata")?;

    // Metadata lookup index.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_image_id ON image_metadata(image_id)",
        (),
    )
    .await
    .context("create idx_metadata_image_id")?;

    // GPS index for location queries.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_gps ON image_metadata(latitude, longitude)",
        (),
    )
    .await
    .context("create idx_metadata_gps")?;

    // Composite geo+time index for map queries ordered by capture time.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_geo_time \
            ON image_metadata(latitude, longitude, datetime_taken)",
        (),
    )
    .await
    .context("create idx_metadata_geo_time")?;

    // Composite camera+time index for camera-model filters.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_camera_time \
            ON image_metadata(camera_model, datetime_taken)",
        (),
    )
    .await
    .context("create idx_metadata_camera_time")?;

    // Partial index over capture time (only rows with a datetime).
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_metadata_datetime \
            ON image_metadata(datetime_taken) WHERE datetime_taken IS NOT NULL",
        (),
    )
    .await
    .context("create idx_metadata_datetime")?;

    // Favorites table.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS favorites (\
            image_id INTEGER PRIMARY KEY, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
            FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE\
        )",
        (),
    )
    .await
    .context("create favorites")?;

    Ok(())
}

/// Migration 2: model registry and user-data tables (tags, collections).
async fn migration_002_models_and_userdata(conn: &turso::Connection) -> Result<()> {
    // Model registry.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS models (\
            name TEXT PRIMARY KEY, \
            dim INTEGER NOT NULL, \
            table_name TEXT NOT NULL, \
            is_active INTEGER NOT NULL DEFAULT 0, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create models")?;

    // Seed the default model (openai/clip-vit-base-patch32, 512-dim, active).
    conn.execute(
        "INSERT OR IGNORE INTO models (name, dim, table_name, is_active) \
            VALUES ('openai/clip-vit-base-patch32', 512, 'image_vectors', 1)",
        (),
    )
    .await
    .context("seed default model")?;

    // Free-text tag catalogue.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tags (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT UNIQUE NOT NULL, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create tags")?;

    // Many-to-many image ↔ tag associations.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS image_tags (\
            image_id INTEGER NOT NULL, \
            tag_id INTEGER NOT NULL, \
            PRIMARY KEY(image_id, tag_id), \
            FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE, \
            FOREIGN KEY(tag_id) REFERENCES tags(id) ON DELETE CASCADE\
        )",
        (),
    )
    .await
    .context("create image_tags")?;

    // Named collections of images.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS collections (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT UNIQUE NOT NULL, \
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create collections")?;

    // Many-to-many collection ↔ image associations.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS collection_images (\
            collection_id INTEGER NOT NULL, \
            image_id INTEGER NOT NULL, \
            PRIMARY KEY(collection_id, image_id), \
            FOREIGN KEY(collection_id) REFERENCES collections(id) ON DELETE CASCADE, \
            FOREIGN KEY(image_id) REFERENCES images(id) ON DELETE CASCADE\
        )",
        (),
    )
    .await
    .context("create collection_images")?;

    Ok(())
}

/// Migration 3: single-row persisted GUI session state.
async fn migration_003_ui_state(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS ui_state (\
            id INTEGER PRIMARY KEY CHECK (id = 1), \
            state_json TEXT NOT NULL, \
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create ui_state")?;
    Ok(())
}

/// Migration 5: add the five extra adjustment columns to image_edits.
async fn migration_005_edit_controls(conn: &turso::Connection) -> Result<()> {
    for col in ["contrast", "brightness", "blacks", "whites", "saturation"] {
        conn.execute(
            &format!("ALTER TABLE image_edits ADD COLUMN {col} REAL NOT NULL DEFAULT 0.0"),
            (),
        )
        .await
        .with_context(|| format!("add column {col} to image_edits"))?;
    }
    Ok(())
}

/// Migration 6: record thumbnails that permanently fail to generate so the
/// pipeline stops retrying undecodable images on every pass. Keyed by content
/// hash + size, mirroring the `thumbnails` table.
async fn migration_006_thumbnail_failures(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS thumbnail_failures (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            image_hash TEXT NOT NULL, \
            size INTEGER NOT NULL, \
            error TEXT, \
            failed_at DATETIME DEFAULT CURRENT_TIMESTAMP, \
            UNIQUE(image_hash, size)\
        )",
        (),
    )
    .await
    .context("create thumbnail_failures table")?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_thumbnail_failures_hash \
         ON thumbnail_failures(image_hash)",
        (),
    )
    .await
    .context("create idx_thumbnail_failures_hash")?;
    Ok(())
}

/// Migration 4: image edits table for brightness/exposure adjustments.
async fn migration_004_image_edits(conn: &turso::Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS image_edits (\
            image_id   INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE, \
            exposure   REAL NOT NULL DEFAULT 0.0, \
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP\
        )",
        (),
    )
    .await
    .context("create image_edits")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> turso::Connection {
        let db = turso::Builder::new_local(":memory:").build().await.unwrap();
        db.connect().unwrap()
    }

    async fn table_exists(conn: &turso::Connection, name: &str) -> bool {
        let mut rows = conn
            .query(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().is_some()
    }

    async fn column_exists(conn: &turso::Connection, table: &str, col: &str) -> bool {
        let mut rows = conn
            .query(&format!("PRAGMA table_info({table})"), ())
            .await
            .unwrap();
        while let Some(row) = rows.next().await.unwrap() {
            let name: String = row.get_value(1).unwrap().as_text().unwrap().clone();
            if name == col {
                return true;
            }
        }
        false
    }

    #[tokio::test]
    async fn migration_005_adds_edit_control_columns() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        for c in ["contrast", "brightness", "blacks", "whites", "saturation"] {
            assert!(
                column_exists(&conn, "image_edits", c).await,
                "missing column {c}"
            );
        }
    }

    #[tokio::test]
    async fn migration_006_creates_thumbnail_failures() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        assert!(table_exists(&conn, "thumbnail_failures").await);
    }

    #[tokio::test]
    async fn migrations_are_idempotent_and_create_tables() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        run_migrations(&conn).await.unwrap(); // second run is a no-op

        for t in [
            "images",
            "image_vectors",
            "thumbnails",
            "thumbnail_failures",
            "image_metadata",
            "favorites",
            "tags",
            "image_tags",
            "collections",
            "collection_images",
            "models",
            "ui_state",
            "image_edits",
            "schema_meta",
        ] {
            assert!(table_exists(&conn, t).await, "missing table {t}");
        }

        let mut rows = conn
            .query("SELECT version FROM schema_meta", ())
            .await
            .unwrap();
        let v = rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get_value(0)
            .unwrap()
            .as_integer()
            .copied();
        assert_eq!(v, Some(6i64));
    }

    #[tokio::test]
    async fn baseline_seeds_default_model() {
        let conn = mem().await;
        run_migrations(&conn).await.unwrap();
        let mut rows = conn
            .query(
                "SELECT dim, table_name, is_active FROM models \
                    WHERE name='openai/clip-vit-base-patch32'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get_value(0).unwrap().as_integer().copied(), Some(512));
        assert_eq!(row.get_value(2).unwrap().as_integer().copied(), Some(1));
    }

    #[tokio::test]
    async fn sanitize_model_table_replaces_non_alphanumeric() {
        assert_eq!(
            sanitize_model_table("openai/clip-vit-base-patch32"),
            "image_vectors_openai_clip_vit_base_patch32"
        );
        assert_eq!(
            sanitize_model_table("laion/CLIP-ViT-L-14"),
            "image_vectors_laion_clip_vit_l_14"
        );
    }

    #[tokio::test]
    async fn create_vector_table_rejects_invalid_names() {
        let conn = mem().await;
        // Need the images table first (FK dependency).
        conn.execute(
            "CREATE TABLE images (id INTEGER PRIMARY KEY AUTOINCREMENT, \
                path TEXT UNIQUE NOT NULL, hash TEXT NOT NULL)",
            (),
        )
        .await
        .unwrap();
        assert!(
            create_vector_table(&conn, "bad name!", EmbeddingDim(512))
                .await
                .is_err()
        );
        assert!(
            create_vector_table(&conn, "good_name", EmbeddingDim(128))
                .await
                .is_ok()
        );
    }
}
