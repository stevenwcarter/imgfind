use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::Path;

pub struct Database {
    pub conn: Connection,
}

impl Database {
    pub fn new(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database at {:?}", db_path))?;

        let mut db = Database { conn };
        db.initialize_schema()?;
        Ok(db)
    }

    fn initialize_schema(&mut self) -> Result<()> {
        // Enable sqlite-vec extension
        self.conn.execute("PRAGMA foreign_keys = ON;", [])?;

        // Create images table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS images (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT UNIQUE NOT NULL,
                hash TEXT NOT NULL,
                embedding BLOB NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;

        // Create index on path for faster lookups
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_images_path ON images(path)",
            [],
        )?;

        // Create index on hash for faster duplicate detection
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_images_hash ON images(hash)",
            [],
        )?;

        Ok(())
    }

    pub fn insert_image(&mut self, path: &str, hash: &str, embedding: &[f32]) -> Result<()> {
        // Convert f32 vector to bytes
        let embedding_bytes: Vec<u8> = embedding
            .iter()
            .flat_map(|&f| f.to_le_bytes().to_vec())
            .collect();

        // Insert or replace existing entry
        self.conn.execute(
            "INSERT OR REPLACE INTO images (path, hash, embedding) VALUES (?1, ?2, ?3)",
            params![path, hash, embedding_bytes],
        )?;

        Ok(())
    }

    pub fn is_image_indexed(&self, path: &str, hash: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT COUNT(*) FROM images WHERE path = ?1 AND hash = ?2")?;

        let count: i64 = stmt.query_row(params![path, hash], |row| row.get(0))?;
        Ok(count > 0)
    }

    pub fn get_all_images(&self) -> Result<Vec<(String, Vec<f32>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, embedding FROM images ORDER BY id")?;

        let image_iter = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let embedding_bytes: Vec<u8> = row.get(1)?;
            Ok((path, embedding_bytes))
        })?;

        let mut results = Vec::new();
        for image in image_iter {
            let (path, embedding_bytes) = image?;
            let embedding = bytes_to_f32_vec(&embedding_bytes);
            results.push((path, embedding));
        }

        Ok(results)
    }

    pub fn clean_missing_files(&mut self) -> Result<usize> {
        // Get all paths from database
        let mut stmt = self.conn.prepare("SELECT id, path FROM images")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut to_delete = Vec::new();
        for row in rows {
            let (id, path) = row?;

            // Handle both absolute and relative paths
            let path_buf = Path::new(&path);
            let file_exists = if path_buf.is_absolute() {
                // For absolute paths, check directly
                path_buf.exists()
            } else {
                // For relative paths, try to resolve them
                // This handles legacy entries that might have been stored as relative paths
                let current_dir =
                    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
                let resolved_path = current_dir.join(path_buf);
                resolved_path.exists()
            };

            if !file_exists {
                to_delete.push(id);
            }
        }

        // Delete missing files
        let mut removed_count = 0;
        for id in to_delete {
            self.conn
                .execute("DELETE FROM images WHERE id = ?1", params![id])?;
            removed_count += 1;
        }

        Ok(removed_count)
    }

    pub fn get_image_count(&self) -> Result<i64> {
        let mut stmt = self.conn.prepare("SELECT COUNT(*) FROM images")?;
        let count: i64 = stmt.query_row([], |row| row.get(0))?;
        Ok(count)
    }

    pub fn get_sample_images(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM images ORDER BY created_at DESC LIMIT ?1")?;

        let image_iter = stmt.query_map([limit], |row| {
            let path: String = row.get(0)?;
            Ok(path)
        })?;

        let mut results = Vec::new();
        for image in image_iter {
            results.push(image?);
        }

        Ok(results)
    }

    #[allow(dead_code)]
    pub fn get_connection(&self) -> &Connection {
        &self.conn
    }
}

fn bytes_to_f32_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let array: [u8; 4] = chunk.try_into().unwrap();
            f32::from_le_bytes(array)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytes_conversion() {
        let original = [1.0, -2.5, 3.15, 0.0];
        let bytes: Vec<u8> = original
            .iter()
            .flat_map(|&f| (f as f32).to_le_bytes().to_vec())
            .collect();
        let converted = bytes_to_f32_vec(&bytes);

        assert_eq!(original.len(), converted.len());
        for (orig, conv) in original.iter().zip(converted.iter()) {
            assert!((orig - conv).abs() < 1e-6);
        }
    }
}
