mod api;
mod auth;
mod config;
mod db;
mod errors;
mod models;
mod scheduler;
mod scraper;
mod services;
mod telegram;

use api::{create_router, AppState};
use auth::{CreativeAwardsOtpProvider, OtpProvider};
use config::Config;
use errors::AppError;
use scraper::{CreativeAwardsScraperEngine, ScraperEngine};
use services::{AccountService, ScrapingService};
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), AppError> {
    // 1. Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,miko_scraper=debug,tower_http=info".into()),
        )
        .init();

    tracing::info!("Starting Rust Authenticated Web Scraper for CreativeAwards.et...");

    // 2. Load configuration from environment / .env
    let config = Config::from_env()?;
    tracing::info!(
        "Configuration loaded: listening on {}:{}, DB: {}",
        config.server_host,
        config.server_port,
        config.database_url
    );

    // 3. Initialize SQLite Connection Pool with PRAGMAs and run Migrations
    let pool = db::init_db_pool(&config.database_url).await?;

    // 4. Instantiate isolated pluggable components for creativeawards.et
    let otp_provider: Arc<dyn OtpProvider> = Arc::new(CreativeAwardsOtpProvider::new());
    let scraper_engine: Arc<dyn ScraperEngine> = Arc::new(CreativeAwardsScraperEngine::new(None, None));

    // 5. Instantiate Service Layer
    let account_service = Arc::new(AccountService::new(
        pool.clone(),
        config.app_secret_key,
        otp_provider,
    ));

    let scraping_service = Arc::new(ScrapingService::new(
        pool.clone(),
        config.app_secret_key,
        scraper_engine,
    ));

    // 6. Build App State
    let app_state = AppState {
        account_service: Arc::clone(&account_service),
        scraping_service: Arc::clone(&scraping_service),
    };

    // 7. Start Autonomous 24/7 Background Scheduler (checks every 30 seconds)
    scheduler::start_autonomous_scheduler(
        Arc::clone(&account_service),
        Arc::clone(&scraping_service),
        std::time::Duration::from_secs(30),
    );
    scheduler::start_daily_database_backup(pool.clone());

    // 8. Start Telegram Bot if token is configured
    if let Some(token) = config.telegram_bot_token {
        tracing::info!("Telegram Bot Token found. Starting Telegram Bot listener...");
        telegram::start_telegram_bot(
            token,
            Arc::clone(&account_service),
            Arc::clone(&scraping_service),
        );
    } else {
        tracing::warn!("TELOXIDE_TOKEN not set. Running Web Dashboard only.");
    }

    // 9. Build Axum Router
    let app = create_router(app_state);

    // 8. Bind TCP Socket and Serve HTTP Requests
    let bind_addr = format!("{}:{}", config.server_host, config.server_port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| AppError::Config(format!("Failed to bind to {}: {}", bind_addr, e)))?;

    tracing::info!("Server listening on http://{}", bind_addr);
    tracing::info!("Web Dashboard accessible at http://{}:{}/", config.server_host, config.server_port);

    axum::serve(listener, app)
        .await
        .map_err(|e| AppError::Internal(format!("Server error: {}", e)))?;

    Ok(())
}
