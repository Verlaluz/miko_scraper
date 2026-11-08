use crate::api::handlers::{
    delete_account_handler, get_account_handler, get_all_vote_status_handler, get_cooldown_status_handler,
    get_history_handler, list_accounts_handler, request_otp_handler,
    reset_cooldown_handler, start_scraping_handler, sync_all_accounts_handler,
    update_label_handler, verify_otp_handler, AppState,
};
use axum::{
    routing::{delete, get, post},
    Router,
};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

pub fn create_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let api_routes = Router::new()
        .route("/accounts", get(list_accounts_handler))
        .route("/accounts/vote-status", get(get_all_vote_status_handler))
        .route("/accounts/sync-all", post(sync_all_accounts_handler))
        .route("/accounts/:id", get(get_account_handler).delete(delete_account_handler))
        .route("/accounts/:id/label", post(update_label_handler))
        .route("/accounts/request-otp", post(request_otp_handler))
        .route("/accounts/verify-otp", post(verify_otp_handler))
        .route("/scrape/status/:id", get(get_cooldown_status_handler))
        .route("/scrape/start/:id", post(start_scraping_handler))
        .route("/scrape/history/:id", get(get_history_handler))
        .route("/scrape/reset/:id", post(reset_cooldown_handler));

    Router::new()
        .nest("/api", api_routes)
        // Serve static frontend files from the "frontend" directory
        .fallback_service(ServeDir::new("frontend"))
        .layer(cors)
        .with_state(state)
}
