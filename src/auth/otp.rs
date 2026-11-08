use crate::errors::AppError;
use crate::models::SessionPayload;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, ORIGIN, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pluggable trait for OTP request and verification.
#[async_trait]
pub trait OtpProvider: Send + Sync {
    /// Dispatches an OTP to the given phone number.
    async fn request_otp(&self, phone_number: &str) -> Result<String, AppError>;

    /// Submits the user-entered OTP and returns the authenticated session.
    async fn verify_otp(
        &self,
        phone_number: &str,
        otp_code: &str,
        challenge_id: &str,
    ) -> Result<SessionPayload, AppError>;
}

/// Official OTP Provider implementation for https://creativeawards.et
pub struct CreativeAwardsOtpProvider {
    pub client: reqwest::Client,
    pub base_url: String,
}

impl CreativeAwardsOtpProvider {
    pub fn new() -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ORIGIN, HeaderValue::from_static("https://creativeawards.et"));
        headers.insert(REFERER, HeaderValue::from_static("https://creativeawards.et/"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
            ),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        Self {
            client,
            base_url: "https://api.creativeawards.et".to_string(),
        }
    }

    /// Normalizes international "+2519..." to local Ethiopian format "09..." expected by the API.
    fn to_local_format(&self, phone: &str) -> String {
        let trimmed = phone.trim();
        if trimmed.starts_with("+251") {
            format!("0{}", &trimmed[4..])
        } else if trimmed.starts_with("251") {
            format!("0{}", &trimmed[3..])
        } else {
            trimmed.to_string()
        }
    }
}

#[derive(Serialize)]
struct SendOtpPayload {
    phone: String,
}

#[derive(Deserialize)]
struct SendOtpResponse {
    message: Option<String>,
}

#[derive(Serialize)]
struct VerifyOtpPayload {
    phone: String,
    otp: String,
}

#[derive(Deserialize)]
struct VerifyOtpResponse {
    message: Option<String>,
    token: Option<String>,
}

#[async_trait]
impl OtpProvider for CreativeAwardsOtpProvider {
    async fn request_otp(&self, phone_number: &str) -> Result<String, AppError> {
        let local_phone = self.to_local_format(phone_number);
        let url = format!("{}/api/send-otp", self.base_url);

        tracing::info!("Sending OTP request to {} for phone {}", url, local_phone);

        let response = self
            .client
            .post(&url)
            .json(&SendOtpPayload {
                phone: local_phone.clone(),
            })
            .send()
            .await
            .map_err(|e| AppError::Scraper(format!("Network error sending OTP: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            tracing::error!("Target site rejected OTP request: {} - {}", status, error_text);
            return Err(AppError::Scraper(format!(
                "Failed to send OTP (Status {}): {}",
                status, error_text
            )));
        }

        let body: SendOtpResponse = response
            .json()
            .await
            .map_err(|e| AppError::Scraper(format!("Invalid response JSON from send-otp: {}", e)))?;

        tracing::info!("Target responded: {:?}", body.message);
        Ok(local_phone)
    }

    async fn verify_otp(
        &self,
        phone_number: &str,
        otp_code: &str,
        _challenge_id: &str,
    ) -> Result<SessionPayload, AppError> {
        let local_phone = self.to_local_format(phone_number);
        let url = format!("{}/api/verify-otp", self.base_url);

        tracing::info!("Submitting OTP verification to {} for phone {}", url, local_phone);

        let response = self
            .client
            .post(&url)
            .json(&VerifyOtpPayload {
                phone: local_phone,
                otp: otp_code.trim().to_string(),
            })
            .send()
            .await
            .map_err(|e| AppError::Scraper(format!("Network error verifying OTP: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            tracing::warn!("Target rejected OTP verification: {} - {}", status, error_text);
            return Err(AppError::InvalidOtp);
        }

        let body: VerifyOtpResponse = response
            .json()
            .await
            .map_err(|e| AppError::Scraper(format!("Invalid verification JSON response: {}", e)))?;

        let token = body.token.ok_or_else(|| {
            AppError::Scraper("Verification succeeded but no auth token was returned".to_string())
        })?;

        tracing::info!("Successfully received auth token for {}", phone_number);

        let mut custom_headers = HashMap::new();
        custom_headers.insert("Authorization".to_string(), format!("Bearer {}", token));

        Ok(SessionPayload {
            token: Some(token),
            cookies: HashMap::new(),
            custom_headers,
        })
    }
}
