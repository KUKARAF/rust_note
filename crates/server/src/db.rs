//! SQLite connection pool setup and migrations.
//!
//! Exposes [`init_pool`], which opens (creating if necessary) the sqlite
//! database file at the given path and runs the embedded migrations
//! (`crates/server/migrations/`) against it.
//!
//! Note: we deliberately use `sqlx`'s runtime-checked query API
//! (`sqlx::query`/`sqlx::query_as`) rather than the compile-time-checked
//! macros (`sqlx::query!`/`sqlx::query_as!`). The macros require a live
//! `DATABASE_URL` (or a checked-in `.sqlx` query cache produced by
//! `cargo sqlx prepare`) available at *compile* time, which is extra
//! machinery we don't want to force on this fresh scaffold yet. This can be
//! revisited once the schema stabilizes.

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

/// How long a single connection waits for a competing writer to release the
/// database lock before returning `SQLITE_BUSY`. With WAL enabled, writers no
/// longer block readers, but two concurrent writers still serialize; a busy
/// timeout lets the second one wait briefly instead of erroring out.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on pooled connections. SQLite allows only one writer at a time
/// regardless, so a small pool is plenty and avoids piling up connections all
/// contending for the write lock.
const MAX_CONNECTIONS: u32 = 8;

/// How long `pool.acquire()` waits for a free connection before erroring,
/// rather than hanging a request indefinitely under load.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// Open (creating if necessary) the sqlite database at `sqlite_path` and run
/// migrations against it, returning a ready-to-use connection pool.
pub async fn init_pool(sqlite_path: &str) -> anyhow::Result<SqlitePool> {
    if let Some(parent) = Path::new(sqlite_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating sqlite parent dir {parent:?}"))?;
        }
    }

    let options = SqliteConnectOptions::from_str(&format!("sqlite://{sqlite_path}"))
        .with_context(|| format!("parsing sqlite path {sqlite_path}"))?
        .create_if_missing(true)
        // WAL lets readers proceed concurrently with a writer (the default
        // rollback journal makes a writer block all readers), which is what
        // caused writer-blocks-reader stalls under load (dos-06). NORMAL
        // synchronous is the standard, safe pairing with WAL.
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT);

    let pool = SqlitePoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await
        .with_context(|| format!("connecting to sqlite database at {sqlite_path}"))?;

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("running database migrations")?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn init_pool_creates_db_and_runs_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = init_pool(db_path.to_str().unwrap()).await.unwrap();

        // Sanity check that the migration actually ran.
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, 0);

        pool.close().await;
    }

    #[tokio::test]
    async fn init_pool_enables_wal_journal_mode() {
        // dos-06 regression: the pool must open the database in WAL mode.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("wal.db");
        let pool = init_pool(db_path.to_str().unwrap()).await.unwrap();

        let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal", "journal_mode should be WAL");

        pool.close().await;
    }
}
