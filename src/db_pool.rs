//! Async Turso connection pool.
//!
//! `TursoPool` wraps a [`deadpool`] managed pool around a [`turso::Database`],
//! giving callers a `get()` that checks out a ready-to-use connection with
//! per-connection settings already applied (a 5 s busy timeout, plus the
//! `foreign_keys = ON` and `journal_mode = WAL` PRAGMAs).
use anyhow::{Context, Result};
use deadpool::managed::{Manager, Metrics, Pool, RecycleResult};
use std::path::{Path, PathBuf};

/// [`deadpool`] manager that creates and recycles [`turso::Connection`]s.
pub struct TursoManager {
    db: turso::Database,
}

impl Manager for TursoManager {
    type Type = turso::Connection;
    type Error = turso::Error;

    async fn create(&self) -> std::result::Result<turso::Connection, turso::Error> {
        let conn = self.db.connect()?;
        // Without a busy timeout, turso defaults to `BusyHandler::None`, so a
        // second concurrent writer under WAL's single-writer lock fails
        // immediately with `SQLITE_BUSY` ("database is locked") instead of
        // waiting and retrying (e.g. the lightbox full-res store racing the
        // grid thumbnail worker). 5000 ms matches turso's own sync default.
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.execute("PRAGMA foreign_keys = ON", ()).await?;
        // `PRAGMA journal_mode = WAL` returns a result row ("wal"); use `query`
        // and drain it so turso does not treat the row as an unexpected result.
        let mut rows = conn.query("PRAGMA journal_mode = WAL", ()).await?;
        while rows.next().await?.is_some() {}
        Ok(conn)
    }

    async fn recycle(
        &self,
        _conn: &mut turso::Connection,
        _metrics: &Metrics,
    ) -> RecycleResult<turso::Error> {
        Ok(())
    }
}

/// A cloneable, async Turso connection pool.
///
/// Open with [`TursoPool::open`]; check out a connection with [`TursoPool::get`].
#[derive(Clone)]
pub struct TursoPool {
    inner: Pool<TursoManager>,
    /// The path the database was opened from.
    pub path: PathBuf,
}

impl TursoPool {
    /// Open (or create) the Turso database at `path` and build a pool with
    /// `max_size` maximum concurrent connections.
    pub async fn open(path: &Path, max_size: usize) -> Result<Self> {
        let path_str = path.to_str().context("non-UTF-8 database path")?;
        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .with_context(|| format!("open turso database at {path:?}"))?;
        let inner = Pool::builder(TursoManager { db })
            .max_size(max_size.max(1))
            .build()
            .context("build turso connection pool")?;
        Ok(Self {
            inner,
            path: path.to_path_buf(),
        })
    }

    /// Check out a connection from the pool.
    pub async fn get(&self) -> Result<deadpool::managed::Object<TursoManager>> {
        self.inner.get().await.context("checkout turso connection")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_opens_memory_db_and_runs_query() {
        let pool = TursoPool::open(std::path::Path::new(":memory:"), 4)
            .await
            .unwrap();
        let conn = pool.get().await.unwrap();
        let mut rows = conn.query("SELECT 1", ()).await.unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get_value(0).unwrap().as_integer().copied(), Some(1_i64));
    }

    /// Regression test: concurrent WAL writers must not fail immediately with
    /// `SQLITE_BUSY` ("database is locked"). Reproduces the lightbox full-res
    /// store racing the grid thumbnail worker. Requires a file-backed DB —
    /// `:memory:` does not exercise WAL's single-writer lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_do_not_hit_database_locked() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("contention.db");
        let pool = TursoPool::open(&db_path, 8).await.unwrap();

        {
            let conn = pool.get().await.unwrap();
            conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, blob BLOB)", ())
                .await
                .unwrap();
        }

        const TASKS: usize = 8;
        const WRITES_PER_TASK: usize = 40;
        let blob = vec![0xab_u8; 256 * 1024];

        let mut handles = Vec::with_capacity(TASKS);
        for task in 0..TASKS {
            let pool = pool.clone();
            let blob = blob.clone();
            handles.push(tokio::spawn(async move {
                for i in 0..WRITES_PER_TASK {
                    let conn = pool.get().await.map_err(|e| e.to_string())?;
                    let id = (task * WRITES_PER_TASK + i) as i64;
                    conn.execute(
                        "INSERT OR REPLACE INTO t (id, blob) VALUES (?, ?)",
                        (id, blob.clone()),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                }
                Ok::<(), String>(())
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap();
            assert!(
                result.is_ok(),
                "concurrent writer failed: {}",
                result.unwrap_err()
            );
        }
    }
}
