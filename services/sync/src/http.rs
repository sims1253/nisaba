//! Injectable HTTP transport used by the OIDC resolver.
//!
//! Both the **JWKS URL refresher** and the **document-authorization verifier**
//! need to make outbound HTTP calls, but the *sync transport core* must stay
//! testable without a network. This component defines the narrow [`HttpFetch`]
//! trait they depend on:
//!
//! * production wires [`ReqwestHttpFetch`] (rustls; gated behind the `server`
//!   feature so the headless library builds without it),
//! * tests inject [`MockHttpFetch`] to return canned responses.
//!
//! The trait is deliberately tiny — one owned request → one response — so a mock
//! is trivial and there is no ambient client state to reason about. It is the
//! single place outbound HTTP leaves the process, which makes the network
//! surface auditable alongside the limits in [`crate::config`].

use std::time::Duration;

use async_trait::async_trait;

/// An outbound HTTP request. Owned (no borrowed lifetimes) so it composes cleanly
/// with `#[async_trait]` and a boxed future behind `dyn HttpFetch`.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// HTTP verb. Only `GET` (JWKS) and `POST` (authz verifier) are used today.
    pub method: HttpMethod,
    /// Absolute URL (`https://`/`http://`).
    pub url: String,
    /// Headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Request body (present for `POST`). Must already be serialised by the
    /// caller — the transport treats it as opaque bytes.
    pub body: Option<Vec<u8>>,
}

/// The two verbs the sync service emits outbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// An inbound HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// Status code (e.g. `200`, `403`).
    pub status: u16,
    /// Response body bytes.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Whether the status is a 2xx success.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Errors from the HTTP transport. These are **fail-closed**: the OIDC resolver
/// turns any transport error into an authorisation denial (never an allow).
#[derive(Debug, thiserror::Error)]
pub enum HttpFetchError {
    /// The call exceeded its configured timeout.
    #[error("http timeout after {0:?}")]
    Timeout(Duration),
    /// A network / DNS / TLS / connection failure.
    #[error("http transport error: {0}")]
    Transport(String),
}

/// The narrow outbound-HTTP seam.
///
/// Implementations must be cheap to clone (they are held inside `Arc`) and must
/// honour the request's intent verbatim — no retries, no ambient auth headers.
/// Retries/timeouts are owned by the caller ([`HttpDocumentAuthorizer`] wraps the
/// call in `tokio::time::timeout`; the JWKS refresher retries on its schedule).
#[async_trait]
pub trait HttpFetch: Send + Sync {
    /// Execute `req`. Returning `Err` denies the dependent authorisation
    /// decision — never allow on a transport failure.
    async fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, HttpFetchError>;
}

#[cfg(feature = "server")]
mod reqwest_impl {
    use async_trait::async_trait;

    use super::{HttpFetch, HttpFetchError, HttpRequest, HttpResponse};

    /// Production HTTP transport backed by `reqwest` with **rustls** (never
    /// openssl — see `deny.toml`). Built once and shared by the JWKS refresher
    /// and the document-authorization verifier.
    #[derive(Clone)]
    pub struct ReqwestHttpFetch {
        client: reqwest::Client,
    }

    impl ReqwestHttpFetch {
        /// Build a client. `connect_timeout` bounds the TCP+TLS handshake;
        /// `request_timeout` bounds the whole call.
        ///
        /// # Errors
        /// Returns the underlying `reqwest` builder error if the client cannot
        /// be constructed (e.g. an invalid TLS configuration).
        pub fn new(
            connect_timeout: std::time::Duration,
            request_timeout: std::time::Duration,
        ) -> Result<Self, reqwest::Error> {
            let client = reqwest::Client::builder()
                .connect_timeout(connect_timeout)
                .timeout(request_timeout)
                .https_only(true)
                .build()?;
            Ok(Self { client })
        }

        /// Build a client that permits plain `http://` URLs in addition to
        /// `https://` (for local dev against an in-cluster `http://app:8080`
        /// authz endpoint or a local Keycloak).
        ///
        /// This only clears reqwest's `https_only` scheme restriction; it does
        /// **not** disable TLS certificate verification. HTTPS connections are
        /// still verified against the bundled webpki (Mozilla) root set, and
        /// plain-`http://` URLs carry no TLS to verify in the first place.
        #[must_use]
        pub fn new_allow_http(
            connect_timeout: std::time::Duration,
            request_timeout: std::time::Duration,
        ) -> Self {
            Self {
                client: reqwest::Client::builder()
                    .connect_timeout(connect_timeout)
                    .timeout(request_timeout)
                    .build()
                    .expect("reqwest builder with no https_only never fails"),
            }
        }
    }

    #[async_trait]
    impl HttpFetch for ReqwestHttpFetch {
        async fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, HttpFetchError> {
            use super::HttpMethod;
            let mut builder = match req.method {
                HttpMethod::Get => self.client.get(&req.url),
                HttpMethod::Post => self.client.post(&req.url),
            };
            for (name, value) in &req.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            if let Some(body) = req.body {
                builder = builder.body(body);
            }
            let response = builder
                .send()
                .await
                .map_err(|e| HttpFetchError::Transport(e.to_string()))?;
            let status = response.status().as_u16();
            let body = response
                .bytes()
                .await
                .map_err(|e| HttpFetchError::Transport(e.to_string()))?
                .to_vec();
            Ok(HttpResponse { status, body })
        }
    }
}

#[cfg(feature = "server")]
pub use reqwest_impl::ReqwestHttpFetch;
