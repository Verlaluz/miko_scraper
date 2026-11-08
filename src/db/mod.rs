pub mod migrations;

use crate::errors::AppError;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};
use std::str::FromStr;
use std::time::Duration;

/// Initializes the SQLite connection pool with production-grade settings:
/// - Write-Ahead Logging (WAL) for concurrency
/// - Normal synchronous mode for fast disk operations
/// - Busy timeout so concurrent writers wait instead of returning immediately with `SQLITE_BUSY`
/// - Foreign key enforcement turned ON
pub async fn init_db_pool(database_url: &str) -> Result<SqlitePool, AppError> {
    let connect_opts = SqliteConnectOptions::from_str(database_url)
        .map_err(|e| AppError::Config(format!("Invalid database connection string: {}", e)))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(connect_opts)
        .await
        .map_err(AppError::Database)?;

    // Run database migrations automatically on startup
    migrations::run_migrations(&pool).await?;

    Ok(pool)
}
