//! Brute-force KNN vector-search SQL builder for the Turso F32_BLOB embedding tables.
//!
//! Turso's local driver exposes `vector_distance_cos` and `vector32()`; this module
//! constructs the query string for a brute-force scan of an `F32_BLOB` embedding
//! table, leaving parameter binding to the caller.
//!
//! # Parameter order
//!
//! The first `?` in the returned SQL is the query embedding (a blob or
//! `vector32('...')` expression, caller decides the binding form).  Any filter
//! params produced by [`crate::filters::build_filter_clause_turso`] follow in
//! the same order they appear in `filter_clause`.

/// Build a brute-force cosine-distance KNN query over an F32_BLOB embedding table.
///
/// # Parameters
///
/// - `vt`            — vector table name (e.g. `"image_vectors"`).
/// - `select`        — columns to surface in the outer `SELECT` (e.g. `"id, path, distance"`).
/// - `extra_joins`   — extra `JOIN` clauses inserted after the `LEFT JOIN image_metadata`;
///   pass `""` for none.
/// - `filter_clause` — SQL predicate fragment from [`crate::filters::build_filter_clause_turso`];
///   either `""` or starts with `" AND "`.
/// - `limit`         — max rows to return.
/// - `offset`        — result page offset.
/// - `threshold`     — maximum cosine distance (e.g. `0.5`); rows with
///   `distance > threshold` are excluded.
///
/// # Binding order
///
/// Caller must bind: `[embedding_blob, ...filter_params]`.
pub fn knn_query(
    vt: &str,
    select: &str,
    extra_joins: &str,
    filter_clause: &str,
    limit: usize,
    offset: usize,
    threshold: f32,
) -> String {
    let joins = if extra_joins.is_empty() {
        String::new()
    } else {
        format!("\n    {extra_joins}")
    };
    format!(
        "SELECT {select} FROM (\n  \
           SELECT i.id AS id, i.path AS path, m.file_size AS file_size,\n         \
                  vector_distance_cos(v.embedding, ?) AS distance\n    \
             FROM {vt} v JOIN images i ON i.id = v.image_id\n    \
             LEFT JOIN image_metadata m ON m.image_id = i.id{joins}\n\
         ) WHERE distance <= {threshold}{filter_clause}\n  \
           ORDER BY distance LIMIT {limit} OFFSET {offset}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::run_migrations;

    /// Helper: open a fresh in-memory Turso connection and run migrations.
    async fn mem_db() -> turso::Connection {
        let db = turso::Builder::new_local(":memory:")
            .build()
            .await
            .expect("build in-memory turso db");
        let conn = db.connect().expect("connect to in-memory turso db");
        run_migrations(&conn).await.expect("run migrations");
        conn
    }

    /// Build a 512-dim L2-normalised f32 vector seeded with a constant.
    fn unit_vec(seed: f32) -> Vec<f32> {
        let mut v: Vec<f32> = (0..512).map(|i| (i as f32 * seed).sin()).collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }

    /// Serialize a 512-dim f32 slice to little-endian bytes (the F32_BLOB wire format).
    fn to_le_bytes(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    #[tokio::test]
    async fn blob_round_trip_and_knn_query_shape() {
        let conn = mem_db().await;

        // Insert two images so the FK constraint is satisfied.
        conn.execute(
            "INSERT INTO images (id, path, hash) VALUES (1,'a','h1'),(2,'b','h2')",
            (),
        )
        .await
        .expect("insert test images");

        let v1 = unit_vec(1.0);
        let v2 = unit_vec(2.0);
        let b1 = to_le_bytes(&v1);
        let b2 = to_le_bytes(&v2);

        // Plan A: bind embeddings as raw Blob values.
        //
        // Turso's F32_BLOB column type accepts raw little-endian float bytes when
        // bound as a BLOB.  Confirmed working with turso 0.7.0-pre.10 local driver.
        // Plan B (vector32('[f1,f2,...]') text form) exists as a fallback for
        // driver/backend combinations that reject raw blob binding, but is not used here.
        conn.execute(
            "INSERT INTO image_vectors (image_id, embedding) VALUES (?1, ?2)",
            (turso::Value::Integer(1), turso::Value::Blob(b1.clone())),
        )
        .await
        .expect("Plan A blob binding must succeed: F32_BLOB insert via turso::Value::Blob");

        conn.execute(
            "INSERT INTO image_vectors (image_id, embedding) VALUES (?1, ?2)",
            (turso::Value::Integer(2), turso::Value::Blob(b2.clone())),
        )
        .await
        .expect("insert second embedding (Plan A)");

        // Read back row 1 and assert byte-exact equality.
        let mut rows = conn
            .query("SELECT embedding FROM image_vectors WHERE image_id = 1", ())
            .await
            .expect("query embedding row 1");
        let row = rows
            .next()
            .await
            .expect("advance row")
            .expect("embedding row 1 present");
        let got = row
            .get_value(0)
            .expect("get embedding value")
            .as_blob()
            .expect("embedding is a blob")
            .clone();
        assert_eq!(got, b1, "round-tripped embedding bytes must be identical");

        // knn_query string shape checks (no actual KNN search — vector_distance_cos
        // requires Turso's vector extension which may not be available in the local
        // in-memory driver under test).
        let q1 = knn_query("image_vectors", "id, path, distance", "", "", 10, 0, 0.5);
        assert!(
            q1.contains("vector_distance_cos"),
            "KNN query must call vector_distance_cos"
        );
        assert!(
            q1.contains("image_vectors"),
            "KNN query must reference the vector table"
        );
        assert!(
            q1.contains("distance <="),
            "KNN query must contain threshold predicate 'distance <='"
        );
        assert!(
            q1.contains("WHERE distance"),
            "KNN query must filter with 'WHERE distance'"
        );
        assert!(
            q1.contains("ORDER BY distance"),
            "KNN query must order by distance"
        );

        let q2 = knn_query(
            "image_vectors",
            "id, distance",
            "JOIN favorites f ON f.image_id = i.id",
            " AND m.file_size >= ?",
            5,
            10,
            0.3,
        );
        assert!(q2.contains("JOIN favorites"));
        assert!(q2.contains("file_size"));
        assert!(q2.contains("0.3"), "threshold must appear as float literal");
        assert!(
            q2.contains("distance <="),
            "KNN query must contain threshold predicate 'distance <='"
        );
        assert!(
            q2.contains("ORDER BY distance"),
            "KNN query must order by distance"
        );
        // LIMIT must appear before OFFSET in the query string.
        let limit_pos = q2.find("LIMIT 5").expect("LIMIT 5 must appear in query");
        let offset_pos = q2
            .find("OFFSET 10")
            .expect("OFFSET 10 must appear in query");
        assert!(
            limit_pos < offset_pos,
            "LIMIT must precede OFFSET in KNN query"
        );
    }
}
