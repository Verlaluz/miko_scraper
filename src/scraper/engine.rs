use crate::errors::AppError;
use crate::models::SessionPayload;
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, ORIGIN, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeResult {
    pub records_count: i64,
    pub sample_data: Vec<String>,
    pub response_status: u16,
    pub response_body: String,
    pub already_voted: bool,
    pub public_vote_count_before: Option<i64>,
    pub public_vote_count_after: Option<i64>,
}

#[async_trait]
pub trait ScraperEngine: Send + Sync {
    /// Executes an authenticated vote claiming operation using the provided decrypted session.
    async fn scrape(&self, session: &SessionPayload) -> Result<ScrapeResult, AppError>;
}

/// Automated Vote Claiming & Scraping Engine for https://creativeawards.et
pub struct CreativeAwardsScraperEngine {
    pub base_url: String,
    pub default_nominee_id: String,
    pub default_category_id: String,
    pub client: reqwest::Client,
}

impl CreativeAwardsScraperEngine {
    pub fn new(nominee_id: Option<String>, category_id: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        Self {
            base_url: "https://api.creativeawards.et".to_string(),
            // #Alpha mobile market ID
            default_nominee_id: nominee_id
                .unwrap_or_else(|| "01a15f21-5408-704c-b358-acdbde802506".to_string()),
            // Yezemene Award category ID
            default_category_id: category_id
                .unwrap_or_else(|| "01a0ecb7-741d-71b5-80cb-d80816db991f".to_string()),
            client,
        }
    }
}

#[derive(Serialize)]
struct ClaimVotePayload {
    nominee_id: String,
    category_id: String,
}

#[derive(Deserialize, Debug)]
struct ClaimVoteResponse {
    data: Option<serde_json::Value>,
    message: Option<String>,
    points: Option<i64>,
    funding_source: Option<String>,
}

impl CreativeAwardsScraperEngine {
    async fn fetch_public_vote_count(
        &self,
        category_id: &str,
        nominee_id: &str,
    ) -> Result<i64, AppError> {
        let url = format!("{}/api/engage/results", self.base_url);
        let response = self
            .client
            .get(url)
            .query(&[("category_id", category_id)])
            .send()
            .await
            .map_err(|e| AppError::Scraper(format!("Network error while reading public vote count: {}", e)))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| AppError::Scraper(format!("Failed to read public vote count response: {}", e)))?;
        if !status.is_success() {
            return Err(AppError::Scraper(format!(
                "Public vote count endpoint returned HTTP {}: {}",
                status, body
            )));
        }

        let payload: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| AppError::Scraper(format!("Invalid public vote count response: {}", e)))?;
        payload["data"]
            .as_array()
            .and_then(|categories| categories.first())
            .and_then(|category| category["nominees"].as_array())
            .and_then(|nominees| {
                nominees
                    .iter()
                    .find(|nominee| nominee["id"].as_str() == Some(nominee_id))
            })
            .and_then(|nominee| nominee["votes"].as_i64())
            .ok_or_else(|| {
                AppError::Scraper(format!(
                    "Public vote count response did not contain nominee {}",
                    nominee_id
                ))
            })
    }
}

#[async_trait]
impl ScraperEngine for CreativeAwardsScraperEngine {
    async fn scrape(&self, session: &SessionPayload) -> Result<ScrapeResult, AppError> {
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

        // Inject the decrypted Bearer token
        if let Some(token) = &session.token {
            let auth_val = format!("Bearer {}", token);
            if let Ok(val) = HeaderValue::from_str(&auth_val) {
                headers.insert(AUTHORIZATION, val);
            }
        } else {
            return Err(AppError::Crypto("No auth token available in session".to_string()));
        }

        tracing::info!(
            "Automated Bot: Claiming +5 free votes for nominee {} in category {}...",
            self.default_nominee_id,
            self.default_category_id
        );

        let mut sample_data = Vec::new();
        let public_vote_count_before = match self
            .fetch_public_vote_count(&self.default_category_id, &self.default_nominee_id)
            .await
        {
            Ok(count) => {
                sample_data.push(format!("Public vote count before claim: {}", count));
                Some(count)
            }
            Err(error) => {
                sample_data.push(format!("Public vote count before claim unavailable: {}", error));
                tracing::warn!("{}", error);
                None
            }
        };

        // 1. Send the automated Claim Free Vote POST request
        let vote_url = format!("{}/api/engage/votes", self.base_url);
        let vote_res = self
            .client
            .post(&vote_url)
            .headers(headers.clone())
            .json(&ClaimVotePayload {
                nominee_id: self.default_nominee_id.clone(),
                category_id: self.default_category_id.clone(),
            })
            .send()
            .await
            .map_err(|e| AppError::Scraper(format!("Network error while claiming vote: {}", e)))?;

        let status = vote_res.status();
        let body_text = vote_res.text().await.unwrap_or_default();

        let mut records_count = 0;
        let mut already_voted = false;

        if status.is_success() {
            let parsed: Option<ClaimVoteResponse> = serde_json::from_str(&body_text).ok();
            let points = parsed.as_ref().and_then(|p| p.points).unwrap_or(5);
            let funding = parsed
                .as_ref()
                .and_then(|p| p.funding_source.clone())
                .unwrap_or_else(|| "free_vote".to_string());

            records_count = points;
            sample_data.push(format!(
                "SUCCESS: Claimed +{} free votes via {} for #Alpha Mobile Market!",
                points, funding
            ));
            tracing::info!("Free vote claimed successfully: +{} points", points);
        } else {
            // Check if already claimed today or if an informative message was returned
            sample_data.push(format!(
                "API Response (HTTP {}): {}",
                status, body_text
            ));
            tracing::warn!("Target vote claim response: {} - {}", status, body_text);

            let lower = body_text.to_lowercase();
            // CreativeAwards sends 409 Conflict when already voted today!
            if lower.contains("already cast")
                || lower.contains("already claimed") 
                || lower.contains("already voted") 
            {
                already_voted = true;
                sample_data.push("Free vote was already cast for this nominee today. No new votes added; 12-hour cooldown retained.".to_string());
                tracing::info!("Account already cast its vote today. No new votes added; retaining the 12-hour cooldown.");
            } else {
                return Err(AppError::ScraperResponse {
                    status: status.as_u16(),
                    body: body_text,
                });
            }
        }

        // 2. Fetch updated balance to verify
        let balance_url = format!("{}/api/balance", self.base_url);
        if let Ok(bal_res) = self.client.get(&balance_url).headers(headers).send().await {
            if let Ok(bal_json) = bal_res.json::<serde_json::Value>().await {
                sample_data.push(format!("Current Balance: {}", bal_json));
            }
        }

        let public_vote_count_after = match self
            .fetch_public_vote_count(&self.default_category_id, &self.default_nominee_id)
            .await
        {
            Ok(count) => {
                sample_data.push(format!("Public vote count after claim: {}", count));
                Some(count)
            }
            Err(error) => {
                sample_data.push(format!("Public vote count after claim unavailable: {}", error));
                tracing::warn!("{}", error);
                None
            }
        };

        Ok(ScrapeResult {
            records_count,
            sample_data,
            response_status: status.as_u16(),
            response_body: body_text,
            already_voted,
            public_vote_count_before,
            public_vote_count_after,
        })
    }
}
