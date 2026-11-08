use crate::errors::AppError;
use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub server_host: String,
    pub server_port: u16,
    pub database_url: String,
    pub app_secret_key: [u8; 32],
    pub telegram_bot_token: Option<String>,
}

impl Config {
    /// Loads configuration from environment variables (and optional `.env` file).
    pub fn from_env() -> Result<Self, AppError> {
        let _ = dotenvy::dotenv();

        let server_host = env::var("SERVER_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let server_port = env::var("SERVER_PORT")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u16>()
            .map_err(|e| AppError::Config(format!("Invalid SERVER_PORT: {}", e)))?;

        let database_url = env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite:miko_scraper.db".to_string());

        let raw_secret = env::var("APP_SECRET_KEY")
            .map_err(|_| AppError::Config("APP_SECRET_KEY environment variable is required".to_string()))?;

        let secret_bytes = hex::decode(raw_secret.trim())
            .map_err(|e| AppError::Config(format!("APP_SECRET_KEY must be valid hex: {}", e)))?;

        if secret_bytes.len() != 32 {
            return Err(AppError::Config(format!(
                "APP_SECRET_KEY must be 32 bytes (64 hex characters), got {} bytes",
                secret_bytes.len()
            )));
        }

        let mut app_secret_key = [0u8; 32];
        app_secret_key.copy_from_slice(&secret_bytes);

        let telegram_bot_token = env::var("TELOXIDE_TOKEN").ok();

        Ok(Self {
            server_host,
            server_port,
            database_url,
            app_secret_key,
            telegram_bot_token,
        })
    }
}
