use crate::{abs_to_relative_path, get_db_parent_dir, relative_to_abs_path};
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{ffi::sqlite3_auto_extension, params};
use sqlite_vec::sqlite3_vec_init;
use std::path::{Path, PathBuf};
use zerocopy::IntoBytes;

#[derive(Debug, Clone)]
pub struct Database {
    pub pool: Pool<SqliteConnectionManager>,
    pub parent_dir: PathBuf,
}

pub type ImageSearchResult = Result<Vec<(String, f32, Option<Vec<u8>>)>>;

// https://chatgpt.com/c/6854c675-e160-800d-a033-fe236a615de4
// TODO: Add extension load for diesel

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

        let manager = SqliteConnectionManager::file(db_path);
        let pool = r2d2::Pool::new(manager)
            .with_context(|| format!("Failed to open database at {:?}", db_path))?;

        let parent_dir = get_db_parent_dir(db_path)?;
        let mut db = Database { pool, parent_dir };
        db.initialize_schema()?;
        Ok(db)
    }

    fn initialize_schema(&mut self) -> Result<()> {
        // Enable foreign keys

        let conn = self.pool.get().context("Failed to get DB connection")?;
        conn.execute("PRAGMA foreign_keys = ON;", [])?;

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

            // Convert relative path back to absolute path
            // let abs_path = relative_to_abs_path(Path::new(&rel_path), &self.parent_dir);
            // let abs_path_str = abs_path.to_string_lossy().to_string();

            Ok((rel_path, distance))
        })?;

        let mut search_results = Vec::new();
        for result in results {
            search_results.push(result?);
        }

        // Limit results to the requested amount
        search_results.truncate(limit);
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

        // Limit results to the requested amount
        search_results.truncate(limit);
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

        // Delete missing files from both tables
        let mut removed_count = 0;
        let conn = self
            .pool
            .get()
            .context("Failed to get DB connection to delete missing files")?;
        for id in to_delete {
            // Delete from vector table first
            conn.execute("DELETE FROM image_vectors WHERE rowid = ?1", params![id])?;
            // Then delete from images table
            conn.execute("DELETE FROM images WHERE id = ?1", params![id])?;
            removed_count += 1;
        }

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

    // #[allow(dead_code)]
    // pub fn get_connection(&self) -> &Connection {
    //     &self.conn.
    // }

    /// Insert a thumbnail into the database cache
    pub fn insert_thumbnail(
        &mut self,
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
        // Convert absolute path to relative path for database lookup
        let path = if path.starts_with("/") {
            let abs_path = Path::new(path);
            let rel_path = abs_to_relative_path(abs_path, &self.parent_dir)
                .with_context(|| format!("Failed to convert path {} to relative path", path))?;
            let rel_path_str = rel_path.to_string_lossy();
            rel_path_str.to_string()
        } else {
            path.to_string()
        };

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
}
