use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

/// Unified Application Error type.
///
/// Every error in the business logic maps into this enum.
/// Using `thiserror`, we automatically implement `std::error::Error`
/// and `std::fmt::Display` for each variant.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Cryptographic error: {0}")]
    Crypto(String),

    #[error("Account not found: {0}")]
    AccountNotFound(i64),

    #[error("Account with phone number '{0}' already exists")]
    AccountAlreadyExists(String),

    #[error("Invalid phone number format: {0}")]
    InvalidPhoneNumber(String),

    #[error("Invalid or expired OTP")]
    InvalidOtp,

    #[error("Authentication session missing or invalid for account {0}")]
    SessionInvalid(i64),

    #[error("Cooldown active: next scrape eligible at {next_eligible_at_utc} (in {remaining_seconds} seconds)")]
    CooldownActive {
        remaining_seconds: i64,
        next_eligible_at_utc: String,
    },

    #[error("A scraping task is already running for phone account {0}")]
    ScrapeAlreadyInProgress(i64),

    #[error("Scraper execution error: {0}")]
    Scraper(String),

    #[error("Scraper response (HTTP {status}): {body}")]
    ScraperResponse { status: u16, body: String },

    #[error("Internal server error: {0}")]
    Internal(String),
}

/// Implement `IntoResponse` for `AppError`.
///
/// In Axum, returning `Result<T, AppError>` from an HTTP handler requires that
/// `AppError` implements `IntoResponse`. This converts any error variant into
/// an HTTP Status Code and a structured JSON payload for the frontend.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_code, message) = match &self {
            AppError::AccountNotFound(_) => (StatusCode::NOT_FOUND, "ACCOUNT_NOT_FOUND", self.to_string()),
            AppError::AccountAlreadyExists(_) => (StatusCode::CONFLICT, "ACCOUNT_ALREADY_EXISTS", self.to_string()),
            AppError::InvalidPhoneNumber(_) => (StatusCode::BAD_REQUEST, "INVALID_PHONE_NUMBER", self.to_string()),
            AppError::InvalidOtp => (StatusCode::UNAUTHORIZED, "INVALID_OTP", self.to_string()),
            AppError::SessionInvalid(_) => (StatusCode::UNAUTHORIZED, "SESSION_INVALID", self.to_string()),
            AppError::CooldownActive { remaining_seconds, next_eligible_at_utc } => (
                StatusCode::TOO_MANY_REQUESTS,
                "COOLDOWN_ACTIVE",
                format!(
                    "12-hour cooldown is active. Wait {} seconds. Next eligible at {}.",
                    remaining_seconds, next_eligible_at_utc
                ),
            ),
            AppError::ScrapeAlreadyInProgress(_) => (StatusCode::CONFLICT, "SCRAPE_IN_PROGRESS", self.to_string()),
            AppError::Database(err) => {
                // Log detailed database error internally, avoid exposing sensitive schema details
                tracing::error!("Database error: {:?}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "DATABASE_ERROR", "A database error occurred.".to_string())
            }
            AppError::Crypto(err) => {
                tracing::error!("Crypto error: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "CRYPTO_ERROR", "A cryptographic failure occurred.".to_string())
            }
            AppError::Config(err) => (StatusCode::INTERNAL_SERVER_ERROR, "CONFIG_ERROR", err.clone()),
            AppError::Scraper(err) => (StatusCode::BAD_GATEWAY, "SCRAPER_ERROR", err.clone()),
            AppError::ScraperResponse { .. } => (
                StatusCode::BAD_GATEWAY,
                "SCRAPER_RESPONSE_ERROR",
                self.to_string(),
            ),
            AppError::Internal(err) => {
                tracing::error!("Internal error: {}", err);
                (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", "An internal error occurred.".to_string())
            }
        };

        let body = Json(json!({
            "success": false,
            "error": {
                "code": error_code,
                "message": message
            }
        }));

        (status, body).into_response()
    }
}
