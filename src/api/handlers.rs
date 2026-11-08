use crate::errors::AppError;
use crate::services::{AccountService, ScrapingService};
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub account_service: Arc<AccountService>,
    pub scraping_service: Arc<ScrapingService>,
}

#[derive(Debug, Deserialize)]
pub struct RequestOtpInput {
    pub phone_number: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyOtpInput {
    pub phone_number: String,
    pub otp_code: String,
    pub challenge_id: String,
}

/// Request OTP for a phone number
pub async fn request_otp_handler(
    State(state): State<AppState>,
    Json(payload): Json<RequestOtpInput>,
) -> Result<Json<Value>, AppError> {
    let challenge_id = state
        .account_service
        .initiate_registration(&payload.phone_number)
        .await?;

    Ok(Json(json!({
        "success": true,
        "message": "OTP request dispatched successfully to target authentication mechanism.",
        "challenge_id": challenge_id
    })))
}

/// Verify OTP and store encrypted session
pub async fn verify_otp_handler(
    State(state): State<AppState>,
    Json(payload): Json<VerifyOtpInput>,
) -> Result<Json<Value>, AppError> {
    let account = state
        .account_service
        .verify_and_save_session(
            &payload.phone_number,
            &payload.otp_code,
            &payload.challenge_id,
        )
        .await?;

    Ok(Json(json!({
        "success": true,
        "message": "Authentication successful. Session encrypted and persisted.",
        "account": account
    })))
}

/// List all registered phone accounts
pub async fn list_accounts_handler(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let accounts = state.account_service.list_accounts().await?;
    Ok(Json(json!({
        "success": true,
        "accounts": accounts
    })))
}

/// Get account by ID
pub async fn get_account_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let account = state.account_service.get_account_by_id(id).await?;
    Ok(Json(json!({
        "success": true,
        "account": account
    })))
}

/// Delete an account by ID
pub async fn delete_account_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    state.account_service.delete_account(id).await?;
    Ok(Json(json!({
        "success": true,
        "message": format!("Account #{} deleted successfully.", id)
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateLabelInput {
    pub label: String,
}

/// Update label for an account (e.g., Mom, Dad, Brother, Self)
pub async fn update_label_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<UpdateLabelInput>,
) -> Result<Json<Value>, AppError> {
    let account = state.account_service.update_account_label(id, &payload.label).await?;
    Ok(Json(json!({
        "success": true,
        "message": "Account label updated successfully.",
        "account": account
    })))
}

/// Get batch vote status and eligibility overview for all accounts
pub async fn get_all_vote_status_handler(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let accounts = state.account_service.list_accounts().await?;
    let statuses = state.scraping_service.get_all_accounts_vote_status(&accounts).await?;
    Ok(Json(json!({
        "success": true,
        "accounts_status": statuses
    })))
}

/// Get 12-hour cooldown status for a specific phone account
pub async fn get_cooldown_status_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let status = state.scraping_service.get_cooldown_status(id).await?;
    Ok(Json(json!({
        "success": true,
        "cooldown": status
    })))
}

/// Start an authenticated scraping job
pub async fn start_scraping_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let record = state.scraping_service.start_scraping(id).await?;
    Ok(Json(json!({
        "success": true,
        "message": "Free vote claim operation executed.",
        "record": record
    })))
}

/// Retrieve scraping history for a phone account
pub async fn get_history_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let history = state.scraping_service.get_history(id, 20).await?;
    Ok(Json(json!({
        "success": true,
        "history": history
    })))
}

/// Sync live status from target site for all accounts
pub async fn sync_all_accounts_handler(
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let accounts = state.account_service.list_accounts().await?;
    let mut synced = 0;
    for acc in accounts {
        if acc.registration_status == crate::models::RegistrationStatus::Verified {
            let _ = state.scraping_service.start_scraping(acc.id).await;
            synced += 1;
        }
    }
    Ok(Json(json!({
        "success": true,
        "message": format!("Synced {} accounts with target site.", synced)
    })))
}

/// Reset cooldown for development testing
pub async fn reset_cooldown_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    state.scraping_service.reset_cooldown_for_test(id).await?;
    Ok(Json(json!({
        "success": true,
        "message": "Cooldown reset successfully for testing."
    })))
}
