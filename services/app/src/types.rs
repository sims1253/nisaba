//! Data transfer objects and error types for the application service.
//!
//! These are the stable wire contract; adapters translate them to domain crates.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A path-addressed document within a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: Uuid,
    pub project_id: Uuid,
    /// Logical path within the project (e.g. `"main.typ"`, `"chapters/intro.typ"`).
    pub path: String,
    /// Human-readable title.
    pub title: String,
    pub body: String,
    pub data: BTreeMap<String, String>,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceEntry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub metadata: ReferenceMetadata,
    pub provenance: Option<Provenance>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferenceMetadata {
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub year: Option<u16>,
    pub doi: Option<String>,
    pub pmid: Option<String>,
    pub journal: Option<String>,
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub search: Option<String>,
    pub database: Option<String>,
    pub searched_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FulltextMetadata {
    pub reference_id: Uuid,
    pub blob_ref: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub checksum_sha256: Option<String>,
    pub uploaded_at: DateTime<Utc>,
}

/// Maximum size of a decoded fulltext payload (64 MiB).
pub const MAX_FULLTEXT_BYTES: usize = 64 * 1024 * 1024;

/// A historical snapshot of a document body, saved on every patch so the editor
/// can show a revision timeline and diff between any two versions (Overleaf-style).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentRevision {
    pub id: Uuid,
    pub document_id: Uuid,
    pub project_id: Uuid,
    pub body: String,
    pub revision: u64,
    pub author: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A shareable link that grants project-scoped access via an opaque token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareLink {
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub token: String,
    pub token_hash: String,
    pub project_id: Uuid,
    pub role: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareLinkCreate {
    pub role: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Uuid,
    pub at: DateTime<Utc>,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCreate {
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectPatch {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentCreate {
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub data: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentPatch {
    pub path: Option<String>,
    pub body: Option<String>,
    pub title: Option<String>,
    pub data: Option<BTreeMap<String, String>>,
    /// Optimistic concurrency guard; a stale value returns 409. Accepts both
    /// the canonical `expected_revision` key (used by the web client and
    /// `OpenAPI`) and the commonly-guessed `revision` alias, so API clients that
    /// send `revision` (as the public API reference documents) are not silently
    /// routed past the guard into a blind overwrite.
    #[serde(alias = "revision")]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceCreate {
    pub metadata: ReferenceMetadata,
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencePatch {
    pub metadata: Option<ReferenceMetadataPatch>,
    pub provenance: Option<Provenance>,
}

/// Partial reference metadata for `PATCH /references/{id}`: every field is
/// optional and only the fields present are merged into the stored metadata.
/// `year`/`doi`/`pmid`/`journal` use `Option<Option<T>>` so `null` clears the
/// value while absence leaves it untouched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReferenceMetadataPatch {
    pub title: Option<String>,
    pub authors: Option<Vec<String>>,
    pub year: Option<Option<u16>>,
    pub doi: Option<Option<String>>,
    pub pmid: Option<Option<String>>,
    pub journal: Option<Option<String>>,
    pub extra: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FulltextInput {
    /// Retained for wire compatibility; the server derives the object key from the reference ID.
    #[serde(default)]
    pub blob_ref: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub checksum_sha256: Option<String>,
    #[serde(default)]
    pub contents_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileRequest {
    pub project_id: Uuid,
    pub entry: String,
    pub sources: BTreeMap<String, String>,
    #[serde(default)]
    pub marks: BTreeMap<String, Vec<MarkInput>>,
    pub mode: CompileMode,
    pub view: CompileView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkInput {
    #[serde(default)]
    pub id: Option<u64>,
    pub start: u32,
    pub end: u32,
    pub kind: String,
    pub author: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompileMode {
    Document,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompileView {
    Baseline,
    Proposed,
    Redline,
    Public,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileResponse {
    pub pdf_base64: Option<String>,
    pub frames: Vec<Value>,
    pub span_map: Vec<Value>,
    pub diagnostics: Vec<Value>,
    pub outline: Vec<Value>,
    pub build_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceExport {
    pub files: Vec<ExportFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFile {
    pub path: String,
    pub content_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub entry: String,
    pub mode: CompileMode,
    pub view: CompileView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResponse {
    pub compile: CompileResponse,
    pub references: ReferenceExport,
    /// Deterministic export bundle (document PDF + per-document RIS + full-text tree),
    /// base64 so the existing JSON contract carries it.
    #[serde(default)]
    pub zip_base64: Option<String>,
    /// Export PDF filename; present when `zip_base64` is.
    #[serde(default)]
    pub zip_filename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MembershipRole {
    Owner,
    Author,
    Reviewer,
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMembership {
    pub project_id: Uuid,
    pub subject: String,
    pub role: MembershipRole,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMemberCreate {
    pub subject: String,
    pub role: MembershipRole,
}

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("repository failure: {0}")]
    Failure(String),
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("You don't have permission to do that")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("dependency unavailable: {0}")]
    Dependency(String),
    #[error("internal error")]
    Internal,
}

impl From<RepoError> for AppError {
    fn from(error: RepoError) -> Self {
        match error {
            RepoError::NotFound => Self::NotFound,
            RepoError::Conflict(message) => Self::Conflict(message),
            RepoError::Failure(_) => Self::Internal,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            Self::Unauthorized(message) => {
                (StatusCode::UNAUTHORIZED, "unauthorized", message.clone())
            }
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", self.to_string()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "bad_request", message.clone()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", self.to_string()),
            Self::Conflict(message) => (StatusCode::CONFLICT, "conflict", message.clone()),
            Self::Dependency(message) => (
                StatusCode::BAD_GATEWAY,
                "dependency_unavailable",
                message.clone(),
            ),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                self.to_string(),
            ),
        };
        (
            status,
            Json(json!({"error": {"code": code, "message": message}})),
        )
            .into_response()
    }
}
