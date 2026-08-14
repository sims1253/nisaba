//! Reviewer seed verification.
//!
//! When a reviewer connects to a brand-new document room (no CRDT state yet),
//! their client seeds the room with the document body it loaded from the app
//! (the web client's "origin" flow). An attacker with a custom client could
//! abuse that same path to plant arbitrary text as the room state before any
//! author connects — the QA-reported "reviewer silently overwrites the
//! baseline via WebSocket" (the relay previously imported any decodable update
//! from any role with `PUSH_UPDATES`).
//!
//! [`SeedVerifier`] lets the room check a reviewer's seed text against the
//! authoritative body the app stores for the document. Verification runs once
//! per empty-room reviewer seed; every later reviewer update that touches the
//! text container is handled by the room's review-policy gate instead.

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::DocId;
use crate::http::{HttpFetch, HttpMethod, HttpRequest};

/// Verifies that text a reviewer wants to seed into an empty room matches the
/// authoritative document body. `Err` is a hard denial (fail-closed).
#[async_trait]
pub trait SeedVerifier: Send + Sync {
    async fn verify(&self, doc: &DocId, text: &str) -> Result<bool, String>;
}

/// Denies every seed — the safe default when no app body endpoint is wired.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAllSeedVerifier;

#[async_trait]
impl SeedVerifier for DenyAllSeedVerifier {
    async fn verify(&self, _doc: &DocId, _text: &str) -> Result<bool, String> {
        Err("no seed verifier configured; denying reviewer seed".into())
    }
}

/// Fetches the authoritative body from the app service and compares it.
///
/// Wire contract:
///
/// ```text
/// GET <NISABA_SYNC_SEED_VERIFY_URL>/{document_id}/body
/// Authorization: Bearer <NISABA_SYNC_AUTHZ_TOKEN>
///
/// → 200 { "body": "<document body>" }
/// → 4xx | 5xx                    // deny
/// ```
pub struct HttpSeedVerifier {
    http: Arc<dyn HttpFetch>,
    url: String,
    service_token: String,
    timeout: std::time::Duration,
}

impl HttpSeedVerifier {
    #[must_use]
    pub fn new(
        http: Arc<dyn HttpFetch>,
        url: impl Into<String>,
        service_token: impl Into<String>,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            http,
            url: url.into(),
            service_token: service_token.into(),
            timeout,
        }
    }
}

#[async_trait]
impl SeedVerifier for HttpSeedVerifier {
    async fn verify(&self, doc: &DocId, text: &str) -> Result<bool, String> {
        let req = HttpRequest {
            method: HttpMethod::Get,
            url: format!("{}/{}/body", self.url, doc.as_str()),
            headers: vec![(
                "authorization".into(),
                format!("Bearer {}", self.service_token),
            )],
            body: None,
        };
        let resp = tokio::time::timeout(self.timeout, self.http.fetch(req))
            .await
            .map_err(|_| "seed verification timed out".to_string())?
            .map_err(|e| format!("seed verification transport error: {e}"))?;
        if !resp.is_success() {
            return Err(format!("seed verification denied (HTTP {})", resp.status));
        }
        let parsed: serde_json::Value = serde_json::from_slice(&resp.body)
            .map_err(|e| format!("seed verification returned malformed body: {e}"))?;
        let body = parsed
            .get("body")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "seed verification response missing body".to_string())?;
        Ok(body == text)
    }
}
