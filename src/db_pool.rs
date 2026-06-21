//! Async Turso connection pool.
//!
//! `TursoPool` wraps a [`deadpool`] managed pool around a [`turso::Database`],
//! giving callers a `get()` that checks out a ready-to-use connection with
//! per-connection PRAGMAs already applied.
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
}
