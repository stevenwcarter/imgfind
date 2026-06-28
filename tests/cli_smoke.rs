/// Hermetic integration test: exercises the async `Database` API end-to-end
/// through the `block_on` sync bridge — no clipper model download, no network.
///
/// Verifies: `Database::new` (migrations + default model seed) →
/// `insert_images_batch` (rel-path strings + 512-dim embeddings) →
/// `search_similar_images` (KNN vector query) roundtrip.
use imgfind::block_on;
use imgfind::database::Database;

/// Build a unique temp dir for this test run so parallel test runs do not
/// collide. Returns both the root dir and the canonical DB path inside it.
fn temp_db() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("imgfind_smoke_{}", std::process::id()));
    let db_path = dir.join(".imgfind").join("imgfind.db");
    (dir, db_path)
}

/// Normalise `v` in-place to unit L2 length.
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

#[test]
fn index_and_search_roundtrip_through_block_on() {
    let (dir, db_path) = temp_db();

    // ── 1. Open DB (migrations run, default model seeded as active) ──────────
    let db = block_on(Database::new(&db_path)).expect("Database::new");

    // ── 2. Two 512-dim L2-normalised embeddings ───────────────────────────────
    // vec_a: unit vector on dim 0  → exact query match, distance ≈ 0
    // vec_b: unit vector on dim 1  → orthogonal, larger distance
    let mut vec_a = vec![0.0f32; 512];
    vec_a[0] = 1.0;
    // already unit length; normalise anyway for clarity
    l2_normalize(&mut vec_a);

    let mut vec_b = vec![0.0f32; 512];
    vec_b[1] = 1.0;
    l2_normalize(&mut vec_b);

    // ── 3. Insert via the public batch API (no real image file needed) ────────
    block_on(db.insert_images_batch(&[
        ("alpha.jpg".to_string(), "hash_a".to_string(), vec_a.clone()),
        ("beta.jpg".to_string(), "hash_b".to_string(), vec_b.clone()),
    ]))
    .expect("insert_images_batch");

    // ── 4. Search with query == vec_a ─────────────────────────────────────────
    // distance_threshold = 2.0 (generous upper-bound) so both rows are
    // candidates; max_k = 100 so no artificial cap truncates results.
    let results = block_on(db.search_similar_images(
        &vec_a,
        10,
        imgfind::DistanceThreshold(2.0),
        imgfind::MaxK(100),
    ))
    .expect("search_similar_images");

    assert!(
        !results.is_empty(),
        "search returned no results — check KNN threshold or vector insert"
    );
    assert_eq!(
        results[0].0,
        "alpha.jpg",
        "closest image should be alpha.jpg, got: {:?}",
        results.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>()
    );
    assert!(
        results[0].1 < 0.01,
        "distance to exact match should be near 0, got {}",
        results[0].1
    );

    // ── 5. Cleanup ────────────────────────────────────────────────────────────
    std::fs::remove_dir_all(&dir).ok();
}
