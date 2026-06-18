use crate::database::{Database, ImageSearchResult};
use crate::filters::Filters;
use anyhow::Result;

pub struct SearchEngine<'a> {
    db: &'a Database,
}

impl<'a> SearchEngine<'a> {
    pub fn new(db: &'a Database) -> Self {
        SearchEngine { db }
    }

    pub fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> Result<Vec<(String, f32)>> {
        // Use sqlite-vec for efficient similarity search
        let query_embedding = normalize_vector(query_embedding);
        self.db
            .search_similar_images(&query_embedding, limit, distance_threshold, max_k)
    }
    pub fn search_with_thumbnails(
        &self,
        query_embedding: &[f32],
        limit: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> Result<Vec<(String, f32, Option<String>)>> {
        // Use sqlite-vec for efficient similarity search
        let query_embedding = normalize_vector(query_embedding);
        self.db
            .search_similar_images_with_blob(&query_embedding, limit, distance_threshold, max_k)
    }
    pub fn search_meta(
        &self,
        query_embedding: Vec<f32>,
        limit: usize,
        offset: usize,
        distance_threshold: f32,
        max_k: usize,
        filters: &Filters,
    ) -> Result<Vec<(String, f32, Option<i64>)>> {
        let query_embedding = normalize_vector(&query_embedding);
        self.db.search_similar_images_meta(
            &query_embedding,
            limit,
            offset,
            distance_threshold,
            max_k,
            filters,
        )
    }
    pub fn search_with_thumbnails_raw(
        &self,
        query_embedding: &[f32],
        limit: usize,
        offset: usize,
        distance_threshold: f32,
        max_k: usize,
    ) -> ImageSearchResult {
        // Use sqlite-vec for efficient similarity search
        let query_embedding = normalize_vector(query_embedding);
        self.db.search_similar_images_with_raw_blob(
            &query_embedding,
            limit,
            offset,
            distance_threshold,
            max_k,
        )
    }
}

pub fn normalize_vector(vector: &[f32]) -> Vec<f32> {
    let magnitude: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if magnitude == 0.0 {
        return vector.to_vec();
    }
    vector.iter().map(|x| x / magnitude).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_vector() {
        let vector = vec![3.0, 4.0];
        let normalized = normalize_vector(&vector);
        let magnitude: f32 = normalized.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-6);
    }
    #[test]
    fn test_double_normalize_vector() {
        let vector = vec![3.0, 4.0];
        let normalized = normalize_vector(&vector);
        let normalized2 = normalize_vector(&normalized);
        let magnitude: f32 = normalized2.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-6);
    }
}
