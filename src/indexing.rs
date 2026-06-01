//! Helpers for the indexing pipeline.

/// Yield slices of `items` of at most `batch_size` (clamped to >= 1).
pub fn chunk_pending<T>(items: &[T], batch_size: usize) -> impl Iterator<Item = &[T]> {
    items.chunks(batch_size.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_pending_respects_batch_size() {
        let items: Vec<u32> = (0..10).collect();
        let chunks: Vec<Vec<u32>> = chunk_pending(&items, 4).map(|c| c.to_vec()).collect();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], vec![0, 1, 2, 3]);
        assert_eq!(chunks[2], vec![8, 9]);
    }

    #[test]
    fn chunks_pending_empty_input_yields_no_chunks() {
        let items: Vec<u32> = vec![];
        assert_eq!(chunk_pending(&items, 4).count(), 0);
    }

    #[test]
    fn chunks_pending_clamps_zero_batch_size_to_one() {
        let items: Vec<u32> = (0..3).collect();
        assert_eq!(chunk_pending(&items, 0).count(), 3);
    }
}
