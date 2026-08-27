//! Dedicated read-connection pool.
//!
//! The backend's primary connection is a writer: it serialises every query
//! behind one mutex and carries a 2 MB page cache (the SQLite default), which
//! is far too small for multi-gigabyte databases. Read queries therefore run
//! on a small pool of dedicated connections, each with a large in-process
//! page cache and file mapping. Reads no longer queue behind the writer and
//! repeated queries (auto-refresh, dashboard widgets) hit a warm cache.
//!
//! A checkout *owns* the `Connection` (popped from the pool) and returns it
//! on drop, so callers get a plain `&Connection` without lifetime coupling
//! to the pool. Connections are opened lazily and kept warm for the life of
//! the backend so the page cache survives between requests.

use crate::error::{Result, StorageError};
use parking_lot::{Condvar, Mutex};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Arc;

/// Page-cache size for pooled read connections: 256 MB. Negative cache_size
/// values are bytes; this is KiB to match SQLite's byte-unit convention.
/// Chosen to comfortably cover the hot window of recent telemetry without
/// approaching host memory limits (pool of 4 → up to 1 GB, all lazily used).
const READ_CACHE_SIZE_KIB: i64 = 256 * 1024;

/// File-mapping size for pooled read connections: 2 GB. The OS pages this in
/// on demand; it is virtual address space, not resident memory.
const READ_MMAP_SIZE: i64 = 2 * 1024 * 1024 * 1024;

/// How long a pooled reader waits for the writer to release the database
/// before failing with a busy error.
const READ_BUSY_TIMEOUT_MS: i64 = 10_000;

/// Guard returned by [`ReadPool::checkout`]; owns the checked-out connection
/// and returns it to the pool on drop.
pub struct ReadGuard {
    conn: Option<Connection>,
    pool: Arc<ReadPool>,
}

impl std::ops::Deref for ReadGuard {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("connection present until the guard is dropped")
    }
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            ReadPool::checkin(&self.pool, conn);
        }
    }
}

/// Pool of dedicated read connections.
pub struct ReadPool {
    path: std::path::PathBuf,
    capacity: usize,
    inner: Mutex<Vec<Connection>>,
    cond: Condvar,
}

impl ReadPool {
    /// Create a pool with `capacity` slots for the database at `path`.
    /// No connections are opened until the first checkout.
    pub fn new(path: std::path::PathBuf, capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            path,
            capacity: capacity.max(1),
            inner: Mutex::new(Vec::new()),
            cond: Condvar::new(),
        })
    }

    /// Open a fresh connection with read-tuned pragmas.
    fn open(path: &Path) -> Result<Connection> {
        let conn = match Connection::open(path) {
            Ok(conn) => conn,
            Err(e) => {
                return Err(StorageError::QueryError(format!(
                    "Failed to open pooled read connection for {}: {}",
                    path.display(),
                    e
                )))
            },
        };
        let pragma_sql = format!(
            "PRAGMA cache_size=-{}; \
             PRAGMA mmap_size={}; \
             PRAGMA busy_timeout={};",
            READ_CACHE_SIZE_KIB, READ_MMAP_SIZE, READ_BUSY_TIMEOUT_MS
        );
        if let Err(e) = conn.execute_batch(&pragma_sql) {
            return Err(StorageError::QueryError(format!(
                "Failed to apply read pragmas for {}: {}",
                path.display(),
                e
            )));
        }
        Ok(conn)
    }

    /// Check out a connection, opening a new one if the pool is below
    /// capacity. Blocks (on the condvar) when the pool is exhausted.
    ///
    /// Callers must run on a blocking thread (via
    /// `tokio::task::spawn_blocking`) because this may block.
    pub fn checkout(this: &Arc<Self>) -> Result<ReadGuard> {
        let mut inner = this.inner.lock();
        loop {
            if let Some(conn) = inner.pop() {
                return Ok(ReadGuard {
                    conn: Some(conn),
                    pool: Arc::clone(this),
                });
            }
            if inner.len() < this.capacity {
                let conn = Self::open(&this.path)?;
                return Ok(ReadGuard {
                    conn: Some(conn),
                    pool: Arc::clone(this),
                });
            }
            // Pool exhausted: wait for a checkin.
            this.cond.wait(&mut inner);
        }
    }

    /// Return a connection to the pool and wake one waiting checker-out.
    fn checkin(this: &Arc<Self>, conn: Connection) {
        let mut inner = this.inner.lock();
        if inner.len() >= this.capacity {
            // Pool shrank or double-checkin: just close it.
            let _ = conn.close();
            return;
        }
        // A `close_all` during an in-flight query could hand us a closed
        // handle back; never push a broken connection into the pool.
        if conn.execute_batch("SELECT 1;").is_err() {
            let _ = conn.close();
            return;
        }
        inner.push(conn);
        this.cond.notify_one();
    }

    /// Close all pooled connections. Called from the backend's `close`.
    pub fn close_all(&self) {
        let mut inner = self.inner.lock();
        for conn in inner.drain(..) {
            if let Err((_, e)) = conn.close() {
                tracing::warn!("Failed to close pooled read connection: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_checkout_reuses_connection() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pool.db");
        Connection::open(&path).unwrap();

        let pool = ReadPool::new(path, 2);
        {
            let guard = ReadPool::checkout(&pool).unwrap();
            let count: i64 = guard.query_row("SELECT 42", [], |r| r.get(0)).unwrap();
            assert_eq!(count, 42);
        }

        // Pool must hold exactly one connection after the first checkout/return.
        assert_eq!(pool.inner.lock().len(), 1);

        // Second checkout reuses it (pool size stays at 1, no new connection).
        let guard = ReadPool::checkout(&pool).unwrap();
        assert!(guard.prepare("SELECT 1").is_ok());
        assert_eq!(pool.inner.lock().len(), 0);
    }

    #[test]
    fn test_cache_pragma_applied() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pool2.db");
        Connection::open(&path).unwrap();

        let pool = ReadPool::new(path, 1);
        let guard = ReadPool::checkout(&pool).unwrap();
        let cache_size: i64 = guard
            .query_row("PRAGMA cache_size", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cache_size, -READ_CACHE_SIZE_KIB);
    }

    #[test]
    fn test_close_all_drops_connections() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("pool3.db");
        Connection::open(&path).unwrap();

        let pool = ReadPool::new(path, 2);
        let _guard = ReadPool::checkout(&pool).unwrap();
        pool.close_all();
        assert!(pool.inner.lock().is_empty());
    }
}
