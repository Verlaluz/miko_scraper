use crate::errors::AppError;
use sqlx::SqlitePool;

/// Applies all pending database migrations located in the `migrations/` directory.
pub async fn run_migrations(pool: &SqlitePool) -> Result<(), AppError> {
    tracing::info!("Running database migrations...");
    sqlx::migrate!("./migrations")
        .run(pool)
        .await
        .map_err(|e| AppError::Database(sqlx::Error::Migrate(Box::new(e))))?;
    tracing::info!("Database migrations applied successfully.");
    Ok(())
}
