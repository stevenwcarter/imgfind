use crate::{abs_to_relative_path, get_db_parent_dir, relative_to_abs_path};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use hashbrown::HashMap;
use image::GenericImageView;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{ffi::sqlite3_auto_extension, params};
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
        let mut db = Database { pool, parent_dir };
        db.initialize_schema()?;
        Ok(db)
    }

    /// Truncate the WAL back into the main DB file. Call after a large write batch (e.g. indexing).
    pub fn checkpoint_wal(&self) -> Result<()> {
        let conn = self.pool.get().context("get connection for WAL checkpoint")?;
        conn.pragma_update(None, "wal_checkpoint", "RESTART")
            .context("wal_checkpoint(RESTART)")?;
        Ok(())
    }

    fn initialize_schema(&mut self) -> Result<()> {
        let conn = self.pool.get().context("Failed to get DB connection")?;

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

        Ok(())
    }

    pub fn insert_image(&mut self, path: &str, hash: &str, embedding: &[f32]) -> Result<()> {
        // Convert absolute path to relative path for storage
        let abs_path = Path::new(path);
        let rel_path = abs_to_relative_path(abs_path, &self.parent_dir)
            .with_context(|| format!("Failed to convert path {} to relative path", path))?;
        let rel_path_str = rel_path.to_string_lossy();

        // Start a transaction for consistency
        {
            let mut conn = self
                .pool
                .get()
                .context("Failed to get DB connection to insert image")?;
            let tx = conn.transaction()?;

            // Insert into images table with relative path
            tx.execute(
                "INSERT OR REPLACE INTO images (path, hash) VALUES (?1, ?2)",
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
                "DELETE FROM image_vectors WHERE rowid = ?1",
                params![image_id],
            )?;

            // Insert the new vector using zerocopy for efficiency
            tx.execute(
                "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![image_id, embedding.as_bytes()],
            )?;

            tx.commit()?;
        }
        Ok(())
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
        let mut conn = self
            .pool
            .get()
            .context("get connection for batch insert")?;
        let tx = conn.transaction()?;
        for (rel_path_str, hash, embedding) in rows {
            // Insert into images table with relative path
            tx.execute(
                "INSERT OR REPLACE INTO images (path, hash) VALUES (?1, ?2)",
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
                "DELETE FROM image_vectors WHERE rowid = ?1",
                params![image_id],
            )?;

            // Insert the new vector using zerocopy for efficiency.
            tx.execute(
                "INSERT INTO image_vectors (rowid, embedding) VALUES (?1, ?2)",
                params![image_id, embedding.as_slice().as_bytes()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn is_image_indexed(&self, path: &str, hash: &str) -> Result<bool> {
        // Convert absolute path to relative path for database lookup
        let abs_path = Path::new(path);
        let rel_path = abs_to_relative_path(abs_path, &self.parent_dir)
            .with_context(|| format!("Failed to convert path {} to relative path", path))?;
        let rel_path_str = rel_path.to_string_lossy();

        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to check if image is indexed")?;
        let mut stmt = conn.prepare("SELECT COUNT(*) FROM images WHERE path = ?1 AND hash = ?2")?;

        let count: i64 = stmt.query_row(params![rel_path_str.as_ref(), hash], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Search for similar images using sqlite-vec
    pub fn search_similar_images(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        // Use a reasonable k value that's at least the limit but not too large
        let k = limit.clamp(1, 100);

        let query = format!(
            "SELECT i.path, distance 
             FROM image_vectors v
             JOIN images i ON i.id = v.rowid
             WHERE v.embedding MATCH ? AND k={k} 
            AND distance <= 1.3
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
    ) -> ImageSearchResult {
        // Use a reasonable k value that's at least the limit but not too large
        let k = limit.clamp(1, 100);

        let query = format!(
            "SELECT i.path, distance, t.thumbnail_data
              FROM image_vectors v
              JOIN images i ON i.id = v.rowid
              LEFT OUTER JOIN thumbnails t ON i.hash = t.image_hash AND t.size = 300
              WHERE v.embedding MATCH ? AND k={k} 
            AND distance <= 1.3
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
    pub fn search_similar_images_with_blob(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<(String, f32, Option<String>)>> {
        let search_results = self.search_similar_images_with_raw_blob(query_embedding, limit, 0)?;
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
            let abs_path = relative_to_abs_path(Path::new(&rel_path), &self.parent_dir);

            if !abs_path.exists() {
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
            tx.execute("DELETE FROM image_vectors WHERE rowid = ?1", params![id])?;
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

    pub fn get_sample_images(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to get sample images")?;
        let mut stmt = conn.prepare("SELECT path FROM images ORDER BY created_at DESC LIMIT ?1")?;

        let image_iter = stmt.query_map([limit], |row| {
            let rel_path: String = row.get(0)?;
            // Convert relative path back to absolute path
            let abs_path = relative_to_abs_path(Path::new(&rel_path), &self.parent_dir);
            Ok(abs_path.to_string_lossy().to_string())
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
    pub fn get_image_hash(&self, path: &str) -> Result<String> {
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to get image hash")?;
        let mut stmt = conn.prepare("SELECT hash FROM images WHERE path = ?1")?;
        let hash: String = stmt.query_row(params![&path], |row| row.get(0))?;
        Ok(hash)
    }

    /// Get images that don't have thumbnails of a specific size
    /// Returns a list of (path, hash) tuples for images missing thumbnails
    pub fn get_images_without_thumbnails(
        &self,
        size: u32,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
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
                let abs_path = relative_to_abs_path(Path::new(&rel_path), &self.parent_dir);
                let abs_path_str = abs_path.to_string_lossy().to_string();

                Ok((abs_path_str, hash))
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
    pub fn get_images_without_metadata(&self, limit: usize) -> Result<Vec<(i64, String, String)>> {
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
            let abs_path = relative_to_abs_path(Path::new(&rel_path), &self.parent_dir);
            let abs_path_str = abs_path.to_string_lossy().to_string();

            Ok((id, abs_path_str, hash))
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
            let abs_path = relative_to_abs_path(Path::new(&rel_path), &self.parent_dir);
            let abs_path_str = abs_path.to_string_lossy().to_string();

            Ok(ImageWithMetadata {
                path: rel_path, // Use relative path for frontend
                absolute_path: abs_path_str,
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
    pub fn get_image_id(&self, path: &str) -> Result<i64> {
        // Convert absolute path to relative path for database lookup
        let abs_path = Path::new(path);
        let rel_path = abs_to_relative_path(abs_path, &self.parent_dir)
            .with_context(|| format!("Failed to convert path {} to relative path", path))?;
        let rel_path_str = rel_path.to_string_lossy();

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

/// Image with metadata for GraphQL responses
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
        let dir = std::env::temp_dir().join(format!(
            "imgfind_test_{}_{n}",
            std::process::id()
        ));
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
        assert_eq!(meta_count, 0, "metadata should cascade-delete with its image");

        // Remove the unique temp dir (grandparent of the .imgfind/imgfind.db path).
        let _ = std::fs::remove_dir_all(db_path.parent().unwrap().parent().unwrap());
    }
}
