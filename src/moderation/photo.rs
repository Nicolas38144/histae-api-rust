use std::future::Future;
use std::pin::Pin;

use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use serde::Deserialize;

use super::domain::AutomatedPhotoModeration;
use crate::config::{PhotoModerationConfig, PhotoModerationProvider};
use crate::profiles::domain::{ModerationReason, ModerationStatus};

pub const PHOTO_MODERATION_POLICY_VERSION: &str = "local_vision_v1";

pub type PhotoModerationFuture<'a> =
    Pin<Box<dyn Future<Output = AutomatedPhotoModeration> + Send + 'a>>;

pub trait PhotoModerator: Send + Sync {
    fn analyze<'a>(&'a self, webp: &'a [u8]) -> PhotoModerationFuture<'a>;
}

#[derive(Clone)]
pub struct HttpPhotoModerator {
    provider: PhotoModerationProvider,
    endpoint: url::Url,
    token: String,
    min_sharpness_score: f64,
    nsfw_review_threshold: f64,
    client: reqwest::Client,
}

impl HttpPhotoModerator {
    pub fn new(config: &PhotoModerationConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(config.timeout)
            .build()?;
        Ok(Self {
            provider: config.provider,
            endpoint: config.endpoint.clone(),
            token: config.token.expose_secret().to_owned(),
            min_sharpness_score: config.min_sharpness_score,
            nsfw_review_threshold: config.nsfw_review_threshold,
            client,
        })
    }

    async fn analyze_http(&self, webp: &[u8]) -> AutomatedPhotoModeration {
        let endpoint = match self.endpoint.join("v1/analyze") {
            Ok(endpoint) => endpoint,
            Err(_) => return unavailable_decision(),
        };
        let response = self
            .client
            .post(endpoint)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
            .header(CONTENT_TYPE, "image/webp")
            .header(CONTENT_LENGTH, webp.len())
            .body(webp.to_vec())
            .send()
            .await;
        let result = async {
            let response = response.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let analysis = response.json::<AnalysisResponse>().await.ok()?;
            analysis.valid().then_some(analysis)
        }
        .await;
        let Some(analysis) = result else {
            tracing::warn!(event_code = "photo_moderation_analysis_failed");
            return unavailable_decision();
        };
        decision_from_analysis(
            analysis,
            self.min_sharpness_score,
            self.nsfw_review_threshold,
        )
    }
}

fn decision_from_analysis(
    analysis: AnalysisResponse,
    min_sharpness_score: f64,
    nsfw_review_threshold: f64,
) -> AutomatedPhotoModeration {
    let mut reasons = Vec::new();
    if analysis.face_count == 0 {
        reasons.push(ModerationReason::FaceNotDetected);
    }
    if analysis.face_count > 1 {
        reasons.push(ModerationReason::MultipleFaces);
    }
    if analysis.sharpness_score < min_sharpness_score {
        reasons.push(ModerationReason::Blurry);
    }
    if analysis.nsfw_score >= nsfw_review_threshold {
        reasons.push(ModerationReason::ExplicitImage);
    }
    AutomatedPhotoModeration {
        status: if reasons.is_empty() {
            ModerationStatus::Approved
        } else {
            ModerationStatus::Pending
        },
        reasons,
        policy_version: PHOTO_MODERATION_POLICY_VERSION,
        face_count: Some(analysis.face_count),
        sharpness_score: Some(analysis.sharpness_score),
        nsfw_score: Some(analysis.nsfw_score),
    }
}

impl PhotoModerator for HttpPhotoModerator {
    fn analyze<'a>(&'a self, webp: &'a [u8]) -> PhotoModerationFuture<'a> {
        Box::pin(async move {
            if self.provider == PhotoModerationProvider::Disabled {
                unavailable_decision()
            } else {
                self.analyze_http(webp).await
            }
        })
    }
}

#[derive(Deserialize)]
struct AnalysisResponse {
    face_count: i32,
    sharpness_score: f64,
    nsfw_score: f64,
}

impl AnalysisResponse {
    fn valid(&self) -> bool {
        (0..=100).contains(&self.face_count)
            && self.sharpness_score.is_finite()
            && self.sharpness_score >= 0.0
            && self.nsfw_score.is_finite()
            && (0.0..=1.0).contains(&self.nsfw_score)
    }
}

pub fn unavailable_decision() -> AutomatedPhotoModeration {
    AutomatedPhotoModeration {
        status: ModerationStatus::Pending,
        reasons: vec![ModerationReason::AnalysisUnavailable],
        policy_version: PHOTO_MODERATION_POLICY_VERSION,
        face_count: None,
        sharpness_score: None,
        nsfw_score: None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::SecretString;

    fn config(provider: PhotoModerationProvider) -> PhotoModerationConfig {
        PhotoModerationConfig {
            provider,
            endpoint: url::Url::parse("http://127.0.0.1:9/")
                .unwrap_or_else(|error| panic!("valid test URL: {error}")),
            token: SecretString::new("t".repeat(32)),
            timeout: Duration::from_millis(25),
            min_sharpness_score: 80.0,
            nsfw_review_threshold: 0.7,
        }
    }

    #[test]
    fn approves_only_a_clearly_safe_analysis() {
        let safe = decision_from_analysis(
            AnalysisResponse {
                face_count: 1,
                sharpness_score: 100.0,
                nsfw_score: 0.01,
            },
            80.0,
            0.7,
        );
        assert_eq!(safe.status, ModerationStatus::Approved);
        assert!(safe.reasons.is_empty());

        let review = decision_from_analysis(
            AnalysisResponse {
                face_count: 0,
                sharpness_score: 79.0,
                nsfw_score: 0.7,
            },
            80.0,
            0.7,
        );
        assert_eq!(review.status, ModerationStatus::Pending);
        assert_eq!(
            review.reasons,
            vec![
                ModerationReason::FaceNotDetected,
                ModerationReason::Blurry,
                ModerationReason::ExplicitImage,
            ]
        );
    }

    #[tokio::test]
    async fn disabled_and_unavailable_providers_fail_closed_without_rejection() {
        let disabled = HttpPhotoModerator::new(&config(PhotoModerationProvider::Disabled))
            .unwrap_or_else(|error| panic!("client builds: {error}"));
        let disabled_result = disabled.analyze(b"webp").await;
        assert_eq!(disabled_result, unavailable_decision());

        let unavailable = HttpPhotoModerator::new(&config(PhotoModerationProvider::LocalHttp))
            .unwrap_or_else(|error| panic!("client builds: {error}"));
        let unavailable_result = unavailable.analyze(b"webp").await;
        assert_eq!(unavailable_result, unavailable_decision());
        assert_ne!(unavailable_result.status, ModerationStatus::Rejected);
    }

    #[tokio::test]
    async fn malformed_http_responses_become_analysis_unavailable() {
        use axum::Json;
        use axum::routing::post;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("test listener: {error}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|error| panic!("test address: {error}"));
        let app = axum::Router::new().route(
            "/v1/analyze",
            post(|| async {
                Json(serde_json::json!({
                    "face_count": "invalid",
                    "sharpness_score": 100,
                    "nsfw_score": 0.1
                }))
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let mut test_config = config(PhotoModerationProvider::LocalHttp);
        test_config.endpoint = url::Url::parse(&format!("http://{address}/"))
            .unwrap_or_else(|error| panic!("valid test endpoint: {error}"));
        let moderator = HttpPhotoModerator::new(&test_config)
            .unwrap_or_else(|error| panic!("client builds: {error}"));
        assert_eq!(moderator.analyze(b"webp").await, unavailable_decision());
        server.abort();
    }

    #[test]
    fn rejects_malformed_numeric_results() {
        assert!(
            !AnalysisResponse {
                face_count: 101,
                sharpness_score: 100.0,
                nsfw_score: 0.1,
            }
            .valid()
        );
        assert!(
            !AnalysisResponse {
                face_count: 1,
                sharpness_score: -1.0,
                nsfw_score: 0.1,
            }
            .valid()
        );
        assert!(
            !AnalysisResponse {
                face_count: 1,
                sharpness_score: 100.0,
                nsfw_score: 1.1,
            }
            .valid()
        );
    }
}
