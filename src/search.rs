use anyhow::Result;
use crate::database::Database;

pub struct SearchEngine<'a> {
    db: &'a Database,
}

impl<'a> SearchEngine<'a> {
    pub fn new(db: &'a Database) -> Self {
        SearchEngine { db }
    }
    
    pub fn search(&self, query_embedding: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        // Use sqlite-vec for efficient similarity search
        self.db.search_similar_images(query_embedding, limit)
    }
}
