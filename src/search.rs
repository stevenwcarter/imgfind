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
        // Get all images and their embeddings
        let images = self.db.get_all_images()?;
        
        // Calculate cosine similarity for each image
        let mut scores: Vec<(String, f32)> = images
            .into_iter()
            .map(|(path, embedding)| {
                let similarity = cosine_similarity(query_embedding, &embedding);
                (path, similarity)
            })
            .collect();
        
        // Sort by similarity score (descending)
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        
        // Take top N results
        scores.truncate(limit);
        
        Ok(scores)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    // Since vectors are already normalized, cosine similarity is just the dot product
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| x * y)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_cosine_similarity() {
        // Test with normalized vectors
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
        
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-6);
        
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-6);
    }
}
