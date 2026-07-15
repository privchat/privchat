//! Process-wide single-instance gate for the CODEX-9 local delivery worker.
//!
//! The advisory lock deliberately uses a dedicated PostgreSQL connection. A
//! pooled connection can be returned to the pool and release the lock while
//! the server is still alive, which would invalidate the single-instance
//! guarantee.

use sqlx::{Connection, PgConnection};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{error, info};

/// Stable application-wide advisory-lock key. It must not be reused by an
/// unrelated service in the same PostgreSQL database.
pub const SINGLE_INSTANCE_ADVISORY_LOCK_KEY: i64 = 0x5052_4956_4348_4154;

#[derive(Debug, Error)]
pub enum SingleInstanceGuardError {
    #[error("another PrivChat server instance already owns the PostgreSQL advisory lock")]
    AlreadyHeld,
    #[error("single-instance advisory lock database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

pub struct SingleInstanceGuard {
    connection: Arc<Mutex<Option<PgConnection>>>,
    monitor: JoinHandle<()>,
}

impl SingleInstanceGuard {
    pub async fn acquire(
        database_url: &str,
    ) -> std::result::Result<Self, SingleInstanceGuardError> {
        Self::acquire_with_key(database_url, SINGLE_INSTANCE_ADVISORY_LOCK_KEY).await
    }

    async fn acquire_with_key(
        database_url: &str,
        lock_key: i64,
    ) -> std::result::Result<Self, SingleInstanceGuardError> {
        let mut connection = PgConnection::connect(database_url).await?;
        let acquired = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(lock_key)
            .fetch_one(&mut connection)
            .await?;
        if !acquired {
            return Err(SingleInstanceGuardError::AlreadyHeld);
        }

        let connection = Arc::new(Mutex::new(Some(connection)));
        let monitor_connection = Arc::clone(&connection);
        let monitor = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let ping_result = {
                    let mut connection = monitor_connection.lock().await;
                    match connection.as_mut() {
                        Some(connection) => connection.ping().await,
                        None => return,
                    }
                };
                if let Err(error) = ping_result {
                    error!(%error, "single-instance advisory lock connection lost; terminating");
                    // Continuing after losing the session-level lock would
                    // permit a second instance to run the dispatch worker.
                    std::process::exit(1);
                }
            }
        });

        info!("single-instance PostgreSQL advisory lock acquired");
        Ok(Self {
            connection,
            monitor,
        })
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        self.monitor.abort();
        if let Ok(mut connection) = self.connection.try_lock() {
            connection.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advisory_lock_is_exclusive_for_a_database_session() {
        let Ok(database_url) =
            std::env::var("PRIVCHAT_TEST_DATABASE_URL").or_else(|_| std::env::var("DATABASE_URL"))
        else {
            eprintln!("skip single-instance advisory lock test: DATABASE_URL not configured");
            return;
        };
        let key = SINGLE_INSTANCE_ADVISORY_LOCK_KEY ^ std::process::id() as i64;
        let first = SingleInstanceGuard::acquire_with_key(&database_url, key)
            .await
            .expect("first session acquires advisory lock");
        let second = SingleInstanceGuard::acquire_with_key(&database_url, key).await;
        assert!(matches!(second, Err(SingleInstanceGuardError::AlreadyHeld)));
        drop(first);

        let replacement = tokio::time::timeout(
            Duration::from_secs(2),
            SingleInstanceGuard::acquire_with_key(&database_url, key),
        )
        .await
        .expect("replacement acquisition must not hang")
        .expect("replacement acquires the lock after the first process exits");
        drop(replacement);
    }
}
