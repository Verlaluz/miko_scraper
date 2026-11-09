use crate::errors::AppError;
use chrono::Utc;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};

const BACKUP_DIR: &str = "/app/data/backups";
const RETAINED_BACKUPS: usize = 7;

pub async fn create_daily_backup(pool: &SqlitePool) -> Result<PathBuf, AppError> {
    let backup_dir = Path::new(BACKUP_DIR);
    tokio::fs::create_dir_all(backup_dir)
        .await
        .map_err(|error| AppError::Internal(format!("Failed to create backup directory: {}", error)))?;

    let backup_path = backup_dir.join(format!(
        "miko_scraper-{}.db",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    ));
    let backup_path_string = backup_path.to_string_lossy().to_string();

    sqlx::query("VACUUM INTO ?")
        .bind(&backup_path_string)
        .execute(pool)
        .await
        .map_err(|error| AppError::Database(error))?;

    prune_old_backups(backup_dir).await?;
    Ok(backup_path)
}

async fn prune_old_backups(backup_dir: &Path) -> Result<(), AppError> {
    let mut entries = tokio::fs::read_dir(backup_dir)
        .await
        .map_err(|error| AppError::Internal(format!("Failed to read backup directory: {}", error)))?;
    let mut backups = Vec::new();

    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| AppError::Internal(format!("Failed to inspect backup directory: {}", error)))?
    {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("db") {
            continue;
        }
        let modified = entry
            .metadata()
            .await
            .map_err(|error| AppError::Internal(format!("Failed to inspect backup metadata: {}", error)))?
            .modified()
            .map_err(|error| AppError::Internal(format!("Failed to read backup timestamp: {}", error)))?;
        backups.push((modified, path));
    }

    backups.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, path) in backups.into_iter().skip(RETAINED_BACKUPS) {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|error| AppError::Internal(format!("Failed to remove old backup {:?}: {}", path, error)))?;
    }

    Ok(())
}
