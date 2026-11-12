use std::future::Future;

use sqlx::SqlitePool;
use tokio::sync::Mutex;

use crate::core::Result;

/// Holds the current SQLite pool and serializes hot replacement during backup overwrite import.
pub struct DatabaseState {
    pool: Mutex<SqlitePool>,
}

impl DatabaseState {
    /// Creates database state from the startup pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool: Mutex::new(pool),
        }
    }

    /// Returns a clone of the currently active pool.
    pub async fn pool(&self) -> SqlitePool {
        self.pool.lock().await.clone()
    }

    /// Closes the active pool, runs replacement work, and installs the new pool atomically.
    ///
    /// Contract for `replace`: it must always hand back a usable pool. When the replacement
    /// work fails, the closure is responsible for restoring the previous data and reopening it,
    /// returning `Ok(old_pool)` and reporting the import/switch error through its own side
    /// channel. Returning `Err` means not even the previous database could be reopened; the
    /// closed pool then stays installed and every later query fails until restart.
    pub async fn close_and_replace<F, Fut>(&self, replace: F) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<SqlitePool>>,
    {
        let mut current = self.pool.lock().await;
        current.close().await;
        let next = replace().await?;
        *current = next;
        Ok(())
    }
}
