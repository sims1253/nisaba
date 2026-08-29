//! HTTP application service for projects, documents, references, and exports.
//!
//! The DTOs in this crate are deliberately owned by the HTTP service.  They are the stable
//! wire contract; adapters can translate them to shared/domain crates when those crates settle.
#![allow(clippy::many_single_char_names)]

mod auth;
mod compile_client;
mod persistence;
mod review_state;
mod types;

pub use auth::*;
pub use compile_client::*;
pub use persistence::{BlobStore, MemoryBlobStore, PostgresRepository, S3BlobStore};
pub use review_state::{
    DecodedReviewState, HttpSyncStateClient, SyncStateClient, UnconfiguredSyncState,
    review_marks_from_snapshot,
};
pub use types::*;

use auth::{Auth, Permission, permitted, project_access, project_acl};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode, header},
    middleware,
    response::IntoResponse,
    routing::{delete, get, post},
};
use base64::Engine as _;
use chrono::Utc;
use nisaba_core::prelude::RedlineStyle;
use nisaba_core::{Document as CoreDocument, View as CoreView};
use nisaba_export::{PdfCompliance, ProjectArchiveInput, build_project_archive, write_zip};
use nisaba_references::{
    Bibliography, Citation, FullText as CoreFullText, IssuedDate, Metadata as CoreMetadata, Person,
    ReferenceEntry as CoreReferenceEntry, ReferenceId, bibliography_yaml, extract_citations,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Reserved bibliography basename.
const REFS_SOURCE_PATH: &str = "refs.yml";

/// Reserved review support-file path.
const REVIEW_SUPPORT_PATH: &str = "review.typ";

/// Minimum seconds between revision snapshots. Set low enough that normal
/// interactive editing cadence (one save every few seconds) produces a
/// recoverable history entry, while still coalescing rapid-fire saves
/// (e.g. autosave every 1 s) into a single snapshot.
const MIN_SNAPSHOT_INTERVAL_SECS: i64 = 10;

/// The review support-file body.
const REVIEW_SUPPORT_SOURCE: &str = "\
#let add = it => text(fill: green)[+#it]
#let del = it => text(fill: red)[#strike[#it]]
#let rep-open = it => []
#let rep-close = it => []
";

fn fulltext_blob_ref(reference_id: Uuid) -> String {
    format!("fulltext/{reference_id}")
}

/// Hashes a share-link token with SHA-256 (hex).
fn hash_token(token: &str) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// Resolve either the one-time plaintext secret or its list-safe revocation ID
/// to the hash stored by repositories. Share redemption still calls
/// `hash_token` directly, so publishing this ID never grants project access.
fn share_link_deletion_hash(value: &str) -> String {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        value.to_ascii_lowercase()
    } else {
        hash_token(value)
    }
}

/// Repository boundary. A durable Postgres adapter can implement this trait without changing
/// HTTP DTOs. `MemoryRepository` is only the explicitly limited local/test adapter.
#[async_trait]
pub trait Repository: Send + Sync {
    async fn create_project(
        &self,
        value: Project,
        audit: Option<AuditEvent>,
    ) -> Result<Project, RepoError>;
    async fn get_project(&self, id: Uuid) -> Result<Project, RepoError>;
    async fn list_projects(&self) -> Result<Vec<Project>, RepoError>;
    async fn create_membership(
        &self,
        value: ProjectMembership,
    ) -> Result<ProjectMembership, RepoError>;
    /// Insert the membership or update its role when it already exists
    /// (share-link redeem uses this to grant the link's role on upgrade).
    async fn upsert_membership(
        &self,
        value: ProjectMembership,
    ) -> Result<ProjectMembership, RepoError>;
    async fn delete_membership(&self, project_id: Uuid, subject: &str) -> Result<(), RepoError>;
    async fn get_membership(
        &self,
        project_id: Uuid,
        subject: &str,
    ) -> Result<ProjectMembership, RepoError>;
    async fn list_memberships(&self, project_id: Uuid)
    -> Result<Vec<ProjectMembership>, RepoError>;
    /// Every membership held by any of `subjects` (an OIDC sub and/or its
    /// `preferred_username` alias), across all projects. Lets the project list
    /// resolve membership in one query instead of two per project.
    async fn list_memberships_for_subjects(
        &self,
        subjects: &[&str],
    ) -> Result<Vec<ProjectMembership>, RepoError>;
    async fn update_project(
        &self,
        value: Project,
        audit: Option<AuditEvent>,
    ) -> Result<Project, RepoError>;
    async fn delete_project(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError>;
    async fn create_document(
        &self,
        value: Document,
        audit: Option<AuditEvent>,
    ) -> Result<Document, RepoError>;
    async fn get_document_by_id(&self, document_id: Uuid) -> Result<Document, RepoError>;
    async fn list_documents(&self, project_id: Uuid) -> Result<Vec<Document>, RepoError>;
    async fn update_document(
        &self,
        value: Document,
        expected_revision: u64,
        audit: Option<AuditEvent>,
    ) -> Result<Document, RepoError>;
    async fn delete_document(
        &self,
        document_id: Uuid,
        audit: Option<AuditEvent>,
    ) -> Result<(), RepoError>;
    async fn create_reference(
        &self,
        value: ReferenceEntry,
        audit: Option<AuditEvent>,
    ) -> Result<ReferenceEntry, RepoError>;
    async fn get_reference(&self, id: Uuid) -> Result<ReferenceEntry, RepoError>;
    async fn list_references(&self, project_id: Uuid) -> Result<Vec<ReferenceEntry>, RepoError>;
    async fn update_reference(
        &self,
        value: ReferenceEntry,
        audit: Option<AuditEvent>,
    ) -> Result<ReferenceEntry, RepoError>;
    async fn delete_reference(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError>;
    async fn get_fulltext(&self, reference_id: Uuid) -> Result<FulltextMetadata, RepoError>;
    async fn list_fulltexts(&self, project_id: Uuid) -> Result<Vec<FulltextMetadata>, RepoError>;
    async fn put_fulltext(
        &self,
        value: FulltextMetadata,
        audit: Option<AuditEvent>,
    ) -> Result<FulltextMetadata, RepoError>;
    async fn delete_fulltext(
        &self,
        reference_id: Uuid,
        audit: Option<AuditEvent>,
    ) -> Result<(), RepoError>;
    async fn append_audit(&self, value: AuditEvent) -> Result<AuditEvent, RepoError>;
    async fn list_audit(&self, project_id: Uuid) -> Result<Vec<AuditEvent>, RepoError>;
    async fn save_document_revision(
        &self,
        document_id: Uuid,
        project_id: Uuid,
        body: String,
        revision: u64,
        author: Option<String>,
    ) -> Result<DocumentRevision, RepoError>;
    async fn list_document_revisions(
        &self,
        document_id: Uuid,
    ) -> Result<Vec<DocumentRevision>, RepoError>;
    async fn get_document_revision(&self, id: Uuid) -> Result<DocumentRevision, RepoError>;
    async fn create_share_link(
        &self,
        project_id: Uuid,
        role: &str,
        created_by: &str,
        label: Option<String>,
    ) -> Result<ShareLink, RepoError>;
    async fn list_share_links(&self, project_id: Uuid) -> Result<Vec<ShareLink>, RepoError>;
    async fn delete_share_link(&self, token: &str) -> Result<(), RepoError>;
    async fn resolve_share_link(&self, token: &str) -> Result<ShareLink, RepoError>;
}

#[derive(Clone)]
pub struct AppState {
    pub repo: Arc<dyn Repository>,
    pub auth: Authenticator,
    pub compile: Arc<dyn CompileClient>,
    pub references: Arc<dyn ReferenceExporter>,
    pub blobs: Arc<dyn BlobStore>,
    /// Reads each document's whole CRDT state from the sync service (the
    /// review marks for exports live there, not in the document row).
    pub sync_state: Arc<dyn SyncStateClient>,
    sync_authz_token: Option<[u8; 32]>,
}
impl AppState {
    pub fn new(repo: Arc<dyn Repository>, auth: Authenticator) -> Self {
        Self {
            repo,
            auth,
            compile: Arc::new(UnconfiguredCompile),
            references: Arc::new(UnconfiguredReferences),
            blobs: Arc::new(MemoryBlobStore::default()),
            sync_state: Arc::new(UnconfiguredSyncState),
            sync_authz_token: None,
        }
    }
    #[must_use]
    pub fn with_sync_authz_token(mut self, token: impl Into<String>) -> Self {
        let token = token.into();
        self.sync_authz_token =
            (!token.trim().is_empty()).then(|| Sha256::digest(token.as_bytes()).into());
        self
    }
    /// Wire the sync state client used by the export path to recover review
    /// marks from each document's CRDT.
    #[must_use]
    pub fn with_sync_state_client(mut self, client: Arc<dyn SyncStateClient>) -> Self {
        self.sync_state = client;
        self
    }
    #[must_use]
    pub fn with_exporters(
        mut self,
        compile: Arc<dyn CompileClient>,
        references: Arc<dyn ReferenceExporter>,
    ) -> Self {
        self.compile = compile;
        self.references = references;
        self
    }
    #[must_use]
    pub fn with_blob_store(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = blobs;
        self
    }
}

fn id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw).map_err(|_| AppError::BadRequest("invalid UUID".into()))
}
fn actor(p: &Principal) -> String {
    p.subject.clone()
}
fn build_audit(
    p: &Principal,
    project_id: Uuid,
    action: &str,
    resource_type: &str,
    resource_id: Uuid,
    details: Value,
) -> AuditEvent {
    AuditEvent {
        id: Uuid::new_v4(),
        project_id,
        actor: actor(p),
        action: action.into(),
        resource_type: resource_type.into(),
        resource_id,
        at: Utc::now(),
        details,
    }
}
async fn audit(
    state: &AppState,
    p: &Principal,
    project_id: Uuid,
    action: &str,
    resource_type: &str,
    resource_id: Uuid,
    details: Value,
) -> Result<(), AppError> {
    state
        .repo
        .append_audit(AuditEvent {
            id: Uuid::new_v4(),
            project_id,
            actor: actor(p),
            action: action.into(),
            resource_type: resource_type.into(),
            resource_id,
            at: Utc::now(),
            details,
        })
        .await?;
    Ok(())
}
async fn project_for(state: &AppState, raw: &str) -> Result<Project, AppError> {
    state.repo.get_project(id(raw)?).await.map_err(Into::into)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn health_ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if state.repo.list_projects().await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "database unavailable");
    }
    (StatusCode::OK, "ready")
}

#[derive(Debug, Deserialize)]
struct SyncAuthorizeRequest {
    subject: String,
    document: String,
}

#[derive(Debug, Serialize)]
struct SyncAuthorizeResponse {
    role: &'static str,
}

fn constant_time_token_matches(expected: Option<&[u8; 32]>, presented: &str) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let presented = Sha256::digest(presented.as_bytes());
    bool::from(expected.as_slice().ct_eq(presented.as_slice()))
}

/// Extract the internal service bearer token from request headers. Shared by
/// the machine-token-only `/internal/*` endpoints.
fn service_token(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::Unauthorized("bearer token required".into()))
}

async fn authorize_sync_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SyncAuthorizeRequest>,
) -> Result<Json<SyncAuthorizeResponse>, AppError> {
    let token = service_token(&headers)?;
    if !constant_time_token_matches(state.sync_authz_token.as_ref(), token) {
        return Err(AppError::Forbidden);
    }
    if request.subject.is_empty() {
        return Err(AppError::Forbidden);
    }
    let document_id = Uuid::parse_str(&request.document)
        .map_err(|_| AppError::BadRequest("invalid document UUID".into()))?;
    let document = state
        .repo
        .get_document_by_id(document_id)
        .await
        .map_err(|_| AppError::Forbidden)?;
    let membership = state
        .repo
        .get_membership(document.project_id, &request.subject)
        .await
        .map_err(|_| AppError::Forbidden)?;
    let role = match membership.role {
        MembershipRole::Owner | MembershipRole::Author => "author",
        MembershipRole::Reviewer => "reviewer",
        MembershipRole::ReadOnly => "read-only",
    };
    Ok(Json(SyncAuthorizeResponse { role }))
}

/// Internal (service-token) endpoint: the authoritative body of a document.
/// Used by the sync service's seed verifier to check that a reviewer's seed of
/// an empty room matches what the app stores — otherwise a reviewer could plant
/// arbitrary text as the room state before any author connects.
async fn document_body(
    State(s): State<AppState>,
    Path(document_id): Path<String>,
    h: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let token = service_token(&h)?;
    if !constant_time_token_matches(s.sync_authz_token.as_ref(), token) {
        return Err(AppError::Forbidden);
    }
    let doc = s.repo.get_document_by_id(id(&document_id)?).await?;
    Ok(Json(json!({ "body": doc.body })))
}

pub fn router(state: AppState) -> Router {
    let acl_state = state.clone();
    Router::new()
        .route("/openapi.json", get(openapi))
        .route("/projects", post(create_project).get(list_projects))
        .route(
            "/projects/{project_id}",
            get(get_project).patch(patch_project).delete(delete_project),
        )
        .route(
            "/projects/{project_id}/members",
            get(list_members).post(add_member),
        )
        .route(
            "/projects/{project_id}/members/{subject}",
            delete(remove_member),
        )
        .route("/projects/{project_id}/membership", get(get_my_membership))
        .route("/healthz", get(healthz))
        .route("/health/ready", get(health_ready))
        .route("/internal/sync/authorize", post(authorize_sync_document))
        .route("/internal/document/{document_id}/body", get(document_body))
        .route("/api/compile", post(api_compile))
        .route(
            "/projects/{project_id}/documents",
            post(create_document).get(list_documents),
        )
        .route(
            "/projects/{project_id}/documents/{document_id}",
            get(get_document)
                .patch(patch_document)
                .delete(delete_document),
        )
        .route(
            "/projects/{project_id}/documents/{document_id}/history",
            get(list_document_history),
        )
        .route(
            "/projects/{project_id}/documents/{document_id}/history/{revision_id}",
            get(get_document_revision),
        )
        .route(
            "/projects/{project_id}/references",
            post(create_reference).get(list_references),
        )
        .route(
            "/projects/{project_id}/references/{reference_id}",
            get(get_reference)
                .patch(patch_reference)
                .delete(delete_reference),
        )
        .route(
            "/projects/{project_id}/references/{reference_id}/fulltext",
            get(get_fulltext).put(put_fulltext).delete(delete_fulltext),
        )
        .route_layer(DefaultBodyLimit::max(MAX_FULLTEXT_BYTES.saturating_mul(2)))
        .route("/projects/{project_id}/audit", get(list_audit))
        .route("/projects/{project_id}/fulltexts", get(list_fulltexts))
        .route("/projects/{project_id}/exports", post(export_project))
        .route(
            "/projects/{project_id}/share-links",
            post(create_share_link).get(list_share_links),
        )
        .route(
            "/projects/{project_id}/share-links/{token}",
            delete(delete_share_link),
        )
        .route("/share/{token}/redeem", post(redeem_share_link))
        .with_state(state)
        .layer(middleware::from_fn_with_state(acl_state, project_acl))
        .layer(middleware::from_fn(security_headers))
}

/// Injects common security response headers on every response. These are
/// defense-in-depth measures; they do not replace input validation or
/// output encoding but make the API surface harder to abuse (e.g. MIME
/// sniffing attacks, clickjacking, legacy XSS filters).
async fn security_headers(
    request: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    // Normalize 422 (axum/serde deserialization) responses to structured JSON
    // errors so Rust/serde internals are not leaked to API consumers.
    if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "bad_request", "message": "invalid request body"}})),
        )
            .into_response();
    }
    response
}

// --- Project handlers ---

async fn create_project(
    State(s): State<AppState>,
    p: Auth,
    Json(r): Json<ProjectCreate>,
) -> Result<(StatusCode, Json<Project>), AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    if r.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    let name = r.name.trim().to_string();
    validate_text(&name, "name", 1024)?;
    let now = Utc::now();
    let id = Uuid::new_v4();
    let value = Project {
        id,
        name,
        created_at: now,
        updated_at: now,
    };
    let event = build_audit(&p, id, "created", "project", id, json!({}));
    let out = s.repo.create_project(value, Some(event)).await?;
    s.repo
        .create_membership(ProjectMembership {
            project_id: out.id,
            subject: p.subject.clone(),
            role: MembershipRole::Owner,
            created_at: Utc::now(),
        })
        .await?;
    Ok((StatusCode::CREATED, Json(out)))
}
async fn list_projects(State(s): State<AppState>, p: Auth) -> Result<Json<Vec<Project>>, AppError> {
    let principal = p.0;
    permitted(&principal, Permission::Read)?;
    // Match the same sub-then-preferred_username resolution used by
    // project_access so project lists agree with per-project access — resolved
    // in ONE membership query for both identifiers instead of two queries per
    // project (the previous N+1).
    let mut subjects = vec![principal.subject.as_str()];
    if let Some(ref username) = principal.preferred_username {
        subjects.push(username.as_str());
    }
    let member_of: HashSet<Uuid> = s
        .repo
        .list_memberships_for_subjects(&subjects)
        .await?
        .into_iter()
        .map(|membership| membership.project_id)
        .collect();
    let projects = s
        .repo
        .list_projects()
        .await?
        .into_iter()
        .filter(|project| member_of.contains(&project.id))
        .collect();
    Ok(Json(projects))
}
async fn get_project(
    State(state): State<AppState>,
    p: Auth,
    Path(project_raw): Path<String>,
) -> Result<Json<Project>, AppError> {
    let p = p.0;
    let project = project_for(&state, &project_raw).await?;
    project_access(&state, &p, project.id, Permission::Read).await?;
    Ok(Json(project))
}
async fn patch_project(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
    Json(r): Json<ProjectPatch>,
) -> Result<Json<Project>, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let mut v = project_for(&s, &pid).await?;
    project_access(&s, &p, v.id, Permission::Manage).await?;
    if let Some(name) = r.name {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::BadRequest("name is required".into()));
        }
        validate_text(&name, "name", 1024)?;
        v.name = name;
    }
    v.updated_at = Utc::now();
    let event = build_audit(&p, v.id, "updated", "project", v.id, json!({}));
    let out = s.repo.update_project(v, Some(event)).await?;
    Ok(Json(out))
}
async fn delete_project(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
) -> Result<StatusCode, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let project = project_for(&s, &pid).await?;
    project_access(&s, &p, project.id, Permission::Manage).await?;
    // Collect the reference ids before the DB rows cascade away, delete the
    // rows first, then remove the blobs from object storage best-effort. If a
    // blob delete fails afterwards only an orphaned blob remains (recoverable
    // by a sweeper); deleting blobs first left rows pointing at deleted blobs,
    // making every later export of the project fail with 409.
    let references = s.repo.list_references(project.id).await?;
    let event = build_audit(&p, project.id, "deleted", "project", project.id, json!({}));
    s.repo.delete_project(project.id, Some(event)).await?;
    for reference in &references {
        if let Err(error) = s.blobs.delete(reference.id).await {
            tracing::warn!(project_id = %project.id, reference_id = %reference.id, error = %error, "failed to delete fulltext blob during project deletion");
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

fn valid_document_path(path: &str) -> bool {
    // Cap the length like the other user-facing text fields (project names,
    // titles): paths feed compile/export include statements and URLs, so an
    // unbounded path would bloat exports. 1024 matches the project-name cap.
    //
    // Deliberately stricter than the compile core's validate_virtual_path
    // (crates/nisaba-compile-core/src/lib.rs): stored paths are user-facing identifiers
    // rendered in listings and URLs, so `.`/`..` segments and control
    // characters are rejected outright. The compile validator only guards its
    // per-request virtual filesystem and therefore tolerates `.` and
    // depth-tracked `..`; the divergence is intentional.
    !path.is_empty()
        && path == path.trim()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.chars().count() <= 1024
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

/// Rejects strings Postgres cannot store (NUL) or that are not reasonable
/// user input (other control characters), plus runaway length. Used for names,
/// paths, titles, and metadata fields.
fn validate_text(value: &str, field: &str, max_len: usize) -> Result<(), AppError> {
    if value.chars().any(char::is_control) {
        return Err(AppError::BadRequest(format!(
            "{field} contains control characters"
        )));
    }
    if value.chars().count() > max_len {
        return Err(AppError::BadRequest(format!(
            "{field} exceeds the maximum length of {max_len} characters"
        )));
    }
    Ok(())
}

/// Document bodies are free-form text: tabs/newlines are legitimate, only NUL
/// is unstorable by Postgres. A generous cap prevents unbounded storage abuse.
const MAX_DOCUMENT_BODY_BYTES: usize = 2 * 1024 * 1024; // 2 MiB
fn validate_body(value: &str) -> Result<(), AppError> {
    if value.contains('\0') {
        return Err(AppError::BadRequest(
            "document body contains NUL bytes".into(),
        ));
    }
    if value.len() > MAX_DOCUMENT_BODY_BYTES {
        return Err(AppError::BadRequest(format!(
            "document body exceeds {MAX_DOCUMENT_BODY_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Document metadata (`data`) is stored as jsonb: a control character in a key
/// or value makes Postgres reject the whole write (HTTP 500). Validate keys and
/// values like every other user string, with the same caps as reference
/// metadata extras.
fn validate_document_data(data: &BTreeMap<String, String>) -> Result<(), AppError> {
    for (key, value) in data {
        validate_text(key, "data key", 256)?;
        validate_text(value, "data value", 4096)?;
    }
    Ok(())
}

// --- Document handlers ---

async fn document_for_project(
    state: &AppState,
    project_raw: &str,
    document_raw: &str,
) -> Result<Document, AppError> {
    let project = project_for(state, project_raw).await?;
    let document = state.repo.get_document_by_id(id(document_raw)?).await?;
    if document.project_id != project.id {
        return Err(AppError::NotFound);
    }
    Ok(document)
}
async fn create_document(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
    Json(r): Json<DocumentCreate>,
) -> Result<(StatusCode, Json<Document>), AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let project = project_for(&s, &pid).await?;
    if !valid_document_path(&r.path) {
        return Err(AppError::BadRequest(
            "path must be a safe project-relative path".into(),
        ));
    }
    validate_text(&r.title, "title", 2048)?;
    validate_body(&r.body)?;
    validate_document_data(&r.data)?;
    let id = Uuid::new_v4();
    let event = build_audit(
        &p,
        project.id,
        "created",
        "document",
        id,
        json!({"path": r.path}),
    );
    let out = s
        .repo
        .create_document(
            Document {
                id,
                project_id: project.id,
                path: r.path,
                title: r.title,
                body: r.body,
                data: r.data,
                revision: 0,
                updated_at: Utc::now(),
            },
            Some(event),
        )
        .await
        .map_err(|error| match error {
            RepoError::Conflict(_) => {
                AppError::Conflict("a document with that path already exists".into())
            }
            other => AppError::from(other),
        })?;
    // Save the initial revision (0) so the original content is preserved in
    // the document history timeline.
    if let Err(e) = s
        .repo
        .save_document_revision(
            out.id,
            out.project_id,
            out.body.clone(),
            0,
            Some(p.subject.clone()),
        )
        .await
    {
        tracing::warn!("failed to save initial document revision snapshot: {e}");
    }
    Ok((StatusCode::CREATED, Json(out)))
}
async fn list_documents(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
) -> Result<Json<Vec<Document>>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let p = project_for(&s, &pid).await?;
    Ok(Json(s.repo.list_documents(p.id).await?))
}
async fn get_document(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, did)): Path<(String, String)>,
) -> Result<Json<Document>, AppError> {
    permitted(&p.0, Permission::Read)?;
    Ok(Json(document_for_project(&s, &pid, &did).await?))
}
async fn patch_document(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, did)): Path<(String, String)>,
    Json(r): Json<DocumentPatch>,
) -> Result<Json<Document>, AppError> {
    // Baseline writes are author/owner only; reviewers propose through the
    // review layer instead (see auth.rs project_acl).
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let mut v = document_for_project(&s, &pid, &did).await?;
    if let Some(expected) = r.expected_revision
        && expected != v.revision
    {
        return Err(AppError::Conflict(format!(
            "document revision is {}, expected {expected}",
            v.revision
        )));
    }
    if let Some(x) = r.path {
        if !valid_document_path(&x) {
            return Err(AppError::BadRequest(
                "path must be a safe project-relative path".into(),
            ));
        }
        v.path = x;
    }
    if let Some(x) = r.body {
        validate_body(&x)?;
        v.body = x;
    }
    if let Some(x) = r.title {
        validate_text(&x, "title", 2048)?;
        v.title = x;
    }
    if let Some(x) = r.data {
        validate_document_data(&x)?;
        v.data = x;
    }
    let old_revision = v.revision;
    v.revision += 1;
    v.updated_at = Utc::now();
    let event = build_audit(
        &p,
        id(&pid)?,
        "updated",
        "document",
        v.id,
        json!({"revision": v.revision}),
    );
    let out = s
        .repo
        .update_document(v.clone(), old_revision, Some(event))
        .await?;
    // Save a revision snapshot for the history timeline, throttled.
    let mut should_snapshot = true;
    if let Ok(existing) = s.repo.list_document_revisions(out.id).await
        && let Some(latest) = existing.first()
    {
        let elapsed = Utc::now().timestamp() - latest.created_at.timestamp();
        if elapsed < MIN_SNAPSHOT_INTERVAL_SECS {
            should_snapshot = false;
        }
    }
    if should_snapshot
        && let Err(e) = s
            .repo
            .save_document_revision(
                out.id,
                out.project_id,
                out.body.clone(),
                out.revision,
                Some(p.subject.clone()),
            )
            .await
    {
        tracing::warn!("failed to save document revision snapshot: {e}");
    }
    Ok(Json(out))
}
async fn delete_document(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, did)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    // Deleting a document destroys author work: owner/author only (a reviewer
    // must never be able to delete what they were invited to review).
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let v = document_for_project(&s, &pid, &did).await?;
    let event = build_audit(&p, id(&pid)?, "deleted", "document", v.id, json!({}));
    s.repo.delete_document(v.id, Some(event)).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Membership handlers ---

async fn list_members(
    State(state): State<AppState>,
    p: Auth,
    Path(project_raw): Path<String>,
) -> Result<Json<Vec<ProjectMembership>>, AppError> {
    let project = project_for(&state, &project_raw).await?;
    // Any project member may see who else is collaborating (the share panel
    // renders this list); only owner/author may modify it.
    project_access(&state, &p.0, project.id, Permission::Read).await?;
    Ok(Json(state.repo.list_memberships(project.id).await?))
}
async fn get_my_membership(
    State(state): State<AppState>,
    p: Auth,
    Path(project_raw): Path<String>,
) -> Result<Json<ProjectMembership>, AppError> {
    let principal = p.0;
    let project = project_for(&state, &project_raw).await?;
    project_access(&state, &principal, project.id, Permission::Read).await?;
    let membership = if let Ok(m) = state
        .repo
        .get_membership(project.id, &principal.subject)
        .await
    {
        m
    } else {
        // Fall back to preferred_username so that memberships created
        // through the UI sharing flow (which sends the human-typed
        // username) also resolve.
        let username = principal
            .preferred_username
            .as_ref()
            .ok_or(AppError::Forbidden)?;
        state
            .repo
            .get_membership(project.id, username)
            .await
            .map_err(|_| AppError::Forbidden)?
    };
    Ok(Json(membership))
}
async fn add_member(
    State(state): State<AppState>,
    p: Auth,
    Path(project_raw): Path<String>,
    Json(request): Json<ProjectMemberCreate>,
) -> Result<(StatusCode, Json<ProjectMembership>), AppError> {
    let project = project_for(&state, &project_raw).await?;
    project_access(&state, &p.0, project.id, Permission::Manage).await?;
    let principal = p.0;
    if request.subject.trim().is_empty() {
        return Err(AppError::BadRequest("subject is required".into()));
    }
    validate_text(&request.subject, "subject", 256)?;
    if matches!(request.role, MembershipRole::Owner) {
        return Err(AppError::BadRequest(
            "owner access is assigned to the creator".into(),
        ));
    }
    let role_value = serde_json::to_value(&request.role).unwrap_or_default();
    let membership = state
        .repo
        .upsert_membership(ProjectMembership {
            project_id: project.id,
            subject: request.subject.clone(),
            role: request.role,
            created_at: Utc::now(),
        })
        .await?;
    // Granting or changing project access is security-sensitive: record who
    // assigned whom which role (remove_member already audits removals).
    audit(
        &state,
        &principal,
        project.id,
        "added",
        "membership",
        project.id,
        json!({"subject": request.subject, "role": role_value}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(membership)))
}
async fn remove_member(
    State(state): State<AppState>,
    p: Auth,
    Path((project_raw, subject)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let project = project_for(&state, &project_raw).await?;
    // Any member may remove their own membership (self-service leave); only
    // managers may remove others.
    let permission = if subject == p.0.subject {
        Permission::Read
    } else {
        Permission::Manage
    };
    project_access(&state, &p.0, project.id, permission).await?;
    let principal = p.0;
    let membership = state
        .repo
        .get_membership(project.id, &subject)
        .await
        .map_err(|_| AppError::NotFound)?;
    if membership.role == MembershipRole::Owner {
        return Err(AppError::BadRequest(
            "owner access is assigned to the creator and cannot be removed".into(),
        ));
    }
    state.repo.delete_membership(project.id, &subject).await?;
    audit(
        &state,
        &principal,
        project.id,
        "removed",
        "membership",
        project.id,
        json!({"subject": subject}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Reference handlers ---

async fn reference_for_project(
    state: &AppState,
    project_raw: &str,
    reference_raw: &str,
) -> Result<ReferenceEntry, AppError> {
    let project = project_for(state, project_raw).await?;
    let reference = state.repo.get_reference(id(reference_raw)?).await?;
    if reference.project_id != project.id {
        return Err(AppError::NotFound);
    }
    Ok(reference)
}
/// Sanity limits for reference metadata. Keeps one bad record from bloating
/// the auto-generated refs.yml that is injected into every compile of the
/// project (a 200 KB title previously propagated into every build).
fn validate_reference_metadata(m: &ReferenceMetadata) -> Result<(), AppError> {
    validate_text(&m.title, "title", 2048)?;
    if m.authors.len() > 64 {
        return Err(AppError::BadRequest("too many authors (max 64)".into()));
    }
    for author in &m.authors {
        validate_text(author, "author", 512)?;
    }
    for (key, value) in &m.extra {
        validate_text(key, "extra key", 256)?;
        validate_text(value, "extra value", 4096)?;
    }
    if let Some(doi) = &m.doi {
        validate_text(doi, "doi", 512)?;
    }
    if let Some(pmid) = &m.pmid {
        validate_text(pmid, "pmid", 128)?;
    }
    if let Some(journal) = &m.journal {
        validate_text(journal, "journal", 1024)?;
    }
    Ok(())
}

async fn create_reference(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
    Json(r): Json<ReferenceCreate>,
) -> Result<(StatusCode, Json<ReferenceEntry>), AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let project = project_for(&s, &pid).await?;
    // Normalize DOI: trim whitespace so leading/trailing spaces don't bypass
    // the case-insensitive uniqueness check.
    let mut metadata = r.metadata;
    if let Some(ref mut doi) = metadata.doi {
        *doi = doi.trim().to_string();
    }
    validate_reference_metadata(&metadata)?;
    let now = Utc::now();
    let id = Uuid::new_v4();
    let event = build_audit(&p, project.id, "created", "reference", id, json!({}));
    let out = s
        .repo
        .create_reference(
            ReferenceEntry {
                id,
                project_id: project.id,
                metadata,
                provenance: r.provenance,
                created_at: now,
                updated_at: now,
            },
            Some(event),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(out)))
}
async fn list_references(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
) -> Result<Json<Vec<ReferenceEntry>>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let project = project_for(&s, &pid).await?;
    Ok(Json(s.repo.list_references(project.id).await?))
}
async fn get_reference(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, rid)): Path<(String, String)>,
) -> Result<Json<ReferenceEntry>, AppError> {
    permitted(&p.0, Permission::Read)?;
    Ok(Json(reference_for_project(&s, &pid, &rid).await?))
}
async fn patch_reference(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, rid)): Path<(String, String)>,
    Json(r): Json<ReferencePatch>,
) -> Result<Json<ReferenceEntry>, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let mut v = reference_for_project(&s, &pid, &rid).await?;
    if let Some(patch) = r.metadata {
        // PATCH semantics: merge only the supplied fields into the stored
        // metadata (a partial body previously failed with a 422 because
        // ReferenceMetadata required every field).
        let mut merged = v.metadata.clone();
        if let Some(title) = patch.title {
            merged.title = title;
        }
        if let Some(authors) = patch.authors {
            merged.authors = authors;
        }
        if let Some(year) = patch.year {
            merged.year = year;
        }
        if let Some(doi) = patch.doi {
            merged.doi = doi.map(|d| d.trim().to_string());
        }
        if let Some(pmid) = patch.pmid {
            merged.pmid = pmid;
        }
        if let Some(journal) = patch.journal {
            merged.journal = journal;
        }
        if let Some(extra) = patch.extra {
            merged.extra = extra;
        }
        validate_reference_metadata(&merged)?;
        v.metadata = merged;
    }
    if r.provenance.is_some() {
        v.provenance = r.provenance;
    }
    v.updated_at = Utc::now();
    let event = build_audit(&p, v.project_id, "updated", "reference", v.id, json!({}));
    let out = s.repo.update_reference(v, Some(event)).await?;
    Ok(Json(out))
}
async fn delete_reference(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, rid)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let v = reference_for_project(&s, &pid, &rid).await?;
    // DB row first, then the blob best-effort: an orphaned blob is recoverable,
    // a row pointing at a deleted blob made every later export fail with 409.
    let event = build_audit(&p, v.project_id, "deleted", "reference", v.id, json!({}));
    s.repo.delete_reference(v.id, Some(event)).await?;
    if let Err(error) = s.blobs.delete(v.id).await {
        tracing::warn!(reference_id = %v.id, error = %error, "failed to delete fulltext blob during reference deletion");
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- Fulltext handlers ---

async fn get_fulltext(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, rid)): Path<(String, String)>,
) -> Result<Json<FulltextMetadata>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let reference = reference_for_project(&s, &pid, &rid).await?;
    Ok(Json(s.repo.get_fulltext(reference.id).await?))
}
async fn put_fulltext(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, rid)): Path<(String, String)>,
    Json(r): Json<FulltextInput>,
) -> Result<Json<FulltextMetadata>, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let reference = reference_for_project(&s, &pid, &rid).await?;
    if r.filename.trim().is_empty() {
        return Err(AppError::BadRequest("filename is required".into()));
    }
    let contents = r
        .contents_base64
        .ok_or_else(|| AppError::BadRequest("contents_base64 is required".into()))
        .and_then(|contents| {
            base64::engine::general_purpose::STANDARD
                .decode(contents)
                .map_err(|_| AppError::BadRequest("contents_base64 is not valid base64".into()))
        })?;
    let expected_size = usize::try_from(r.size_bytes)
        .map_err(|_| AppError::BadRequest("size_bytes does not fit on this platform".into()))?;
    if contents.len() != expected_size {
        return Err(AppError::BadRequest(
            "size_bytes does not match contents_base64".into(),
        ));
    }
    if contents.len() > MAX_FULLTEXT_BYTES {
        return Err(AppError::BadRequest(
            "fulltext exceeds the 64 MiB byte limit".into(),
        ));
    }
    if r.content_type != "application/pdf" || contents.is_empty() {
        return Err(AppError::BadRequest(
            "fulltext contents must be a non-empty PDF".into(),
        ));
    }
    // Validate the payload actually looks like a PDF (magic header + EOF
    // trailer) instead of trusting the declared content type: a 1-byte file
    // declared as application/pdf previously sailed through and later produced
    // a corrupt PDF inside the export archive.
    let looks_like_pdf = contents.len() > 4
        && &contents[..5] == b"%PDF-"
        && contents[contents.len().saturating_sub(1024)..]
            .windows(5)
            .any(|window| window == b"%%EOF");
    if !looks_like_pdf {
        return Err(AppError::BadRequest(
            "fulltext contents are not a valid PDF (missing %PDF- header or %%EOF trailer)".into(),
        ));
    }
    s.blobs
        .put(reference.id, contents)
        .await
        .map_err(|_| AppError::Dependency("blob store unavailable".into()))?;
    let filename = r.filename.clone();
    let event = build_audit(
        &p,
        reference.project_id,
        "attached",
        "fulltext",
        reference.id,
        json!({"filename":filename}),
    );
    let out = s
        .repo
        .put_fulltext(
            FulltextMetadata {
                reference_id: reference.id,
                blob_ref: fulltext_blob_ref(reference.id),
                filename: r.filename,
                content_type: r.content_type,
                size_bytes: r.size_bytes,
                checksum_sha256: r.checksum_sha256,
                uploaded_at: Utc::now(),
            },
            Some(event),
        )
        .await?;
    Ok(Json(out))
}
async fn delete_fulltext(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, rid)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let reference = reference_for_project(&s, &pid, &rid).await?;
    // DB row first, then the blob best-effort (same rationale as
    // delete_reference: orphaned blobs are recoverable, dangling rows are not).
    let event = build_audit(
        &p,
        reference.project_id,
        "detached",
        "fulltext",
        reference.id,
        json!({}),
    );
    s.repo.delete_fulltext(reference.id, Some(event)).await?;
    if let Err(error) = s.blobs.delete(reference.id).await {
        tracing::warn!(reference_id = %reference.id, error = %error, "failed to delete fulltext blob during fulltext detach");
    }
    Ok(StatusCode::NO_CONTENT)
}
async fn list_fulltexts(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
) -> Result<Json<Vec<FulltextMetadata>>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let project = project_for(&s, &pid).await?;
    Ok(Json(s.repo.list_fulltexts(project.id).await?))
}

// --- Audit handler ---

async fn list_audit(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
) -> Result<Json<Vec<AuditEvent>>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let p = project_for(&s, &pid).await?;
    Ok(Json(s.repo.list_audit(p.id).await?))
}

// --- Document history handlers ---

async fn list_document_history(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, did)): Path<(String, String)>,
) -> Result<Json<Vec<DocumentRevision>>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let doc = document_for_project(&s, &pid, &did).await?;
    Ok(Json(s.repo.list_document_revisions(doc.id).await?))
}

async fn get_document_revision(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, did, rid)): Path<(String, String, String)>,
) -> Result<Json<DocumentRevision>, AppError> {
    permitted(&p.0, Permission::Read)?;
    let project = project_for(&s, &pid).await?;
    // Scope the revision to the document named in the path. Previously the
    // document id was discarded (`_did`), so a caller could fetch document B's
    // revision body by placing B's revision UUID under document A's path as
    // long as both lived in the same project. (Fetch the document directly —
    // document_for_project would re-fetch the project we already hold.)
    let document = s.repo.get_document_by_id(id(&did)?).await?;
    if document.project_id != project.id {
        return Err(AppError::NotFound);
    }
    let rev = s.repo.get_document_revision(id(&rid)?).await?;
    if rev.project_id != project.id || rev.document_id != document.id {
        return Err(AppError::NotFound);
    }
    Ok(Json(rev))
}

// --- Compile proxy ---

fn projected_source(
    source: &str,
    marks: &[MarkInput],
    view: &CompileView,
) -> Result<String, AppError> {
    let mut document = CoreDocument::from_text(source);
    for mark in marks {
        let kind = match mark.kind.as_str() {
            "insert" => nisaba_core::MarkKind::Insert,
            "delete" => nisaba_core::MarkKind::Delete,
            "comment" => nisaba_core::MarkKind::Comment,
            "secret" => nisaba_core::MarkKind::Secret,
            _ => {
                return Err(AppError::BadRequest(format!(
                    "unknown mark kind: {}",
                    mark.kind
                )));
            }
        };
        document.add_mark(nisaba_core::Mark::new(
            nisaba_core::MarkId::new(mark.id.unwrap_or(mark.timestamp)),
            nisaba_core::TextRange::new(
                nisaba_core::Position::from_char_idx(mark.start),
                nisaba_core::Position::from_char_idx(mark.end),
            ),
            kind,
            nisaba_core::AuthorId::new(mark.author.clone()),
            nisaba_core::Timestamp::new(mark.timestamp),
            None,
        ));
    }
    let core_view = match view {
        CompileView::Baseline => CoreView::Baseline,
        CompileView::Proposed => CoreView::Proposed,
        CompileView::Redline => CoreView::Redline,
        CompileView::Public => CoreView::Public,
    };
    Ok(document.project(core_view))
}

fn core_reference_entry(reference: &ReferenceEntry) -> Option<CoreReferenceEntry> {
    let id = ReferenceId::new(reference.id.to_string()).ok()?;
    Some(CoreReferenceEntry {
        id,
        metadata: core_metadata(&reference.metadata),
        unknown_ris: Vec::new(),
        fulltext: None,
        provenance: Vec::new(),
    })
}

fn core_metadata(metadata: &ReferenceMetadata) -> CoreMetadata {
    CoreMetadata {
        title: metadata.title.clone(),
        authors: metadata
            .authors
            .iter()
            .map(|author| Person::family(author.clone()))
            .collect(),
        issued: metadata.year.map(|year| IssuedDate {
            year: i32::from(year),
            ..IssuedDate::default()
        }),
        container_title: metadata.journal.clone(),
        doi: metadata.doi.clone(),
        pmid: metadata.pmid.clone(),
        ..CoreMetadata::default()
    }
}

fn references_bibliography_yaml(references: &[ReferenceEntry]) -> String {
    let core: Vec<CoreReferenceEntry> =
        references.iter().filter_map(core_reference_entry).collect();
    bibliography_yaml(&core)
}

fn inject_bibliography(request: &mut CompileRequest, yaml: String) {
    if yaml.trim().is_empty() {
        return;
    }
    let entry_dir = request.entry.rsplit_once('/').map_or("", |(dir, _)| dir);
    let bib_path = if entry_dir.is_empty() {
        REFS_SOURCE_PATH.to_owned()
    } else {
        format!("{entry_dir}/{REFS_SOURCE_PATH}")
    };
    if request.sources.contains_key(&bib_path) {
        return;
    }
    request.sources.insert(bib_path, yaml);
    if let Some(entry_source) = request.sources.get_mut(&request.entry)
        && !entry_source.contains("#bibliography(")
    {
        entry_source.push_str("\n#bibliography(\"refs.yml\")\n");
    }
}

fn inject_per_document_bibliography(
    request: &mut CompileRequest,
    doc_yaml: &BTreeMap<String, String>,
) {
    for (path, yaml) in doc_yaml {
        if yaml.trim().is_empty() {
            continue;
        }
        let (dir, file_name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        let dir = dir.trim_start_matches('/');
        let stem = file_name.trim_end_matches(".typ");
        let bib_path = if dir.is_empty() {
            format!("refs-{stem}.yml")
        } else {
            format!("{dir}/refs-{stem}.yml")
        };
        if request.sources.contains_key(&bib_path) {
            continue;
        }
        request.sources.insert(bib_path, yaml.clone());
        let call = format!("\n#bibliography(\"refs-{stem}.yml\", group: none)\n");
        if let Some(source) = request.sources.get_mut(path)
            && !source.contains("#bibliography(")
        {
            source.push_str(&call);
        }
    }
}

fn inject_redline_review(request: &mut CompileRequest) {
    if !matches!(request.view, CompileView::Redline) {
        return;
    }
    // Check ALL sources for review markers, not just the entry. In a multi-file
    // project the marks may live on an included file (e.g. chapters/intro.typ)
    // while the entry only #includes it. The marker strings come from the
    // redline style's defaults so they cannot drift from what projection emits.
    let has_markers = request.sources.values().any(|src| {
        [
            RedlineStyle::DEFAULT_INSERT_OPEN,
            RedlineStyle::DEFAULT_DELETE_OPEN,
            RedlineStyle::DEFAULT_REPLACED_OPEN,
            RedlineStyle::DEFAULT_REPLACED_CLOSE,
        ]
        .iter()
        .any(|marker| src.contains(marker))
    });
    if !has_markers {
        return;
    }
    let entry_dir = request.entry.rsplit_once('/').map_or("", |(dir, _)| dir);
    let module_path = if entry_dir.is_empty() {
        REVIEW_SUPPORT_PATH.to_owned()
    } else {
        format!("{entry_dir}/{REVIEW_SUPPORT_PATH}")
    };
    if request.sources.contains_key(&module_path) {
        return;
    }
    request
        .sources
        .insert(module_path, REVIEW_SUPPORT_SOURCE.to_owned());
    if let Some(entry_source) = request.sources.get_mut(&request.entry)
        && !entry_source.contains("#import \"review.typ\"")
        && !entry_source.contains("#import 'review.typ'")
    {
        entry_source.insert_str(0, "#import \"review.typ\" as review\n\n");
    }
}

async fn api_compile(
    State(state): State<AppState>,
    p: Auth,
    Json(mut request): Json<CompileRequest>,
) -> Result<Json<CompileResponse>, AppError> {
    // Compiling is a read-only operation (the compile service never mutates the
    // project). The docs' roles table promises "Read and compile" for every
    // role, so any authenticated project member may compile — including
    // read-only users, who were previously denied here.
    let principal = p.0;
    project_access(&state, &principal, request.project_id, Permission::Read).await?;
    if request.sources.is_empty() || !request.sources.contains_key(&request.entry) {
        return Err(AppError::BadRequest("sources must contain entry".into()));
    }
    let sources = std::mem::take(&mut request.sources);
    request.sources = sources
        .into_iter()
        .map(|(path, source)| {
            let projected = projected_source(
                &source,
                request.marks.get(&path).map_or(&[], Vec::as_slice),
                &request.view,
            )?;
            // Convert markdown-style headings to Typst syntax at compile time
            // (never mutating the stored body), matching the export path so a
            // document that exports successfully also previews successfully.
            Ok((path, markdown_headings_to_typst(&projected)))
        })
        .collect::<Result<_, AppError>>()?;
    request.marks.clear();
    let yaml = references_bibliography_yaml(&state.repo.list_references(request.project_id).await?);
    inject_bibliography(&mut request, yaml);
    inject_redline_review(&mut request);
    let project_id = request.project_id;
    let response = state.compile.compile(request).await?;
    audit(
        &state,
        &principal,
        project_id,
        "compiled",
        "compile",
        project_id,
        json!({"build_id": response.build_id}),
    )
    .await?;
    Ok(Json(response))
}

// --- Export ---

/// One document's gathered export data.
struct DocumentExport {
    /// The document's id — its CRDT lives in the sync service keyed by this.
    id: Uuid,
    path: String,
    body: String,
    citations: Vec<Citation>,
    yaml: String,
}

async fn gather_documents(
    repo: &dyn Repository,
    project: &Project,
    references: &[ReferenceEntry],
) -> Result<Vec<DocumentExport>, AppError> {
    let by_id: HashMap<String, CoreReferenceEntry> = references
        .iter()
        .filter_map(|r| core_reference_entry(r).map(|e| (e.id.to_string(), e)))
        .collect();
    let mut docs = Vec::new();
    for document in repo.list_documents(project.id).await? {
        let citations = extract_citations(&document.body).map_err(|error| {
            AppError::BadRequest(format!(
                "citation extraction failed for {}: {error}",
                document.path
            ))
        })?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut cited: Vec<CoreReferenceEntry> = Vec::new();
        let mut known_citations: Vec<nisaba_references::Citation> = Vec::new();
        for citation in &citations {
            let key = citation.reference_id.as_str().to_owned();
            if by_id.contains_key(&key) {
                known_citations.push(citation.clone());
                if seen.insert(key.clone())
                    && let Some(entry) = by_id.get(&key)
                {
                    cited.push(entry.clone());
                }
            }
        }
        let yaml = bibliography_yaml(&cited);
        docs.push(DocumentExport {
            id: document.id,
            path: document.path,
            body: document.body,
            citations: known_citations,
            yaml,
        });
    }
    Ok(docs)
}

/// Gather each document's review marks from its synced CRDT state, keyed by
/// document path for the compile request.
///
/// Review marks are NOT stored in the document row the app owns — they live in
/// the document's Loro CRDT, replicated by the sync service — so the export
/// path asks sync for each document's whole state and decodes the review
/// container with exactly the web compile path's semantics (see
/// [`review_marks_from_snapshot`]). A document with no review items (or no CRDT
/// state at all — nobody ever collaborated on it) is normal and yields an
/// empty mark list.
///
/// Failure policy — correctness over availability: if sync is unreachable, or
/// answers unexpectedly, or a snapshot cannot be decoded, the export FAILS with
/// a dependency error (502) instead of silently exporting a marks-less
/// archive. A redline export that quietly dropped every pending suggestion
/// would misrepresent the project's review state, which is precisely what the
/// export is meant to capture.
///
/// The same policy governs the **at-rest check**: mark offsets are resolved
/// against the snapshot's CRDT text, so they may only be projected over a body
/// identical to it. The web client persists the body with a debounced PATCH,
/// so while a document is being edited (or a save has failed) the stored body
/// lags the CRDT — at that moment the export refuses (502, "retry once
/// editing has settled") rather than project the marks over text they were
/// never resolved against. Exports are taken at rest, and enforced to be.
async fn project_review_marks(
    state: &AppState,
    docs: &[DocumentExport],
) -> Result<BTreeMap<String, Vec<MarkInput>>, AppError> {
    let mut marks = BTreeMap::new();
    for doc in docs {
        let decoded = match state.sync_state.document_state(doc.id).await? {
            // No synced state at all: an empty list, not an error.
            None => None,
            Some(snapshot) => {
                let decoded = review_marks_from_snapshot(&snapshot).map_err(|error| {
                    AppError::Dependency(format!(
                        "review state for {} could not be decoded: {error}",
                        doc.path
                    ))
                })?;
                if decoded.text != doc.body {
                    return Err(AppError::Dependency(format!(
                        "document {} has collaborative changes that are not saved yet; retry the export once editing has settled",
                        doc.path
                    )));
                }
                Some(decoded)
            }
        };
        marks.insert(doc.path.clone(), decoded.map_or_else(Vec::new, |d| d.marks));
    }
    Ok(marks)
}

async fn document_bibliographies(
    repo: &dyn Repository,
    blobs: &dyn BlobStore,
    references: &[ReferenceEntry],
    docs: &[DocumentExport],
) -> Result<(Vec<Bibliography>, Vec<String>), AppError> {
    let by_uuid: HashMap<String, &ReferenceEntry> =
        references.iter().map(|r| (r.id.to_string(), r)).collect();
    let mut bytes_cache: HashMap<String, CoreFullText> = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut bibliographies = Vec::new();
    for doc in docs {
        if doc.citations.is_empty() {
            continue;
        }
        let mut entries: Vec<CoreReferenceEntry> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for citation in &doc.citations {
            let key = citation.reference_id.as_str();
            if !seen.insert(key.to_owned()) {
                continue;
            }
            let Some(reference) = by_uuid.get(key) else {
                continue;
            };
            let Some(mut entry) = core_reference_entry(reference) else {
                continue;
            };
            match bytes_cache.get(key) {
                Some(fulltext) => entry.fulltext = Some(fulltext.clone()),
                None => match fetch_fulltext(repo, blobs, reference.id).await {
                    Ok(fulltext) => {
                        entry.fulltext = Some(fulltext.clone());
                        bytes_cache.insert(key.to_owned(), fulltext);
                    }
                    Err(FulltextError::Missing) => missing.push(key.to_owned()),
                    Err(FulltextError::Unavailable(error)) => {
                        return Err(AppError::Dependency(format!(
                            "failed to fetch fulltext for reference {key}: {error}"
                        )));
                    }
                },
            }
            entries.push(entry);
        }
        let directory = format!("references-{}", bibliographies.len() + 1);
        bibliographies.push(Bibliography {
            directory,
            entries,
            citations: doc.citations.clone(),
        });
    }
    Ok((bibliographies, missing))
}

/// Why a cited reference's fulltext could not be attached to an export.
enum FulltextError {
    /// The reference genuinely has no usable fulltext (none recorded, blob
    /// absent, empty bytes, or a non-PDF content type) — a client-actionable
    /// 409, same as before.
    Missing,
    /// The metadata store or blob store failed — an infrastructure fault that
    /// must surface as a 502, not as "cited references are missing".
    Unavailable(String),
}

async fn fetch_fulltext(
    repo: &dyn Repository,
    blobs: &dyn BlobStore,
    reference_id: Uuid,
) -> Result<CoreFullText, FulltextError> {
    let fulltext = repo.get_fulltext(reference_id).await.map_err(|e| match e {
        RepoError::NotFound => FulltextError::Missing,
        other => FulltextError::Unavailable(other.to_string()),
    })?;
    let bytes = blobs.get(reference_id).await.map_err(|e| match e {
        RepoError::NotFound => FulltextError::Missing,
        other => FulltextError::Unavailable(other.to_string()),
    })?;
    if bytes.is_empty() || fulltext.content_type != "application/pdf" {
        return Err(FulltextError::Missing);
    }
    Ok(CoreFullText {
        blob_ref: fulltext.blob_ref,
        media_type: fulltext.content_type,
        contents: bytes,
    })
}

fn decode_compile_pdf(compile: &CompileResponse) -> Result<Vec<u8>, AppError> {
    let b64 = compile
        .pdf_base64
        .as_ref()
        .ok_or_else(|| AppError::Conflict("compile produced no PDF".into()))?;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|error| AppError::Conflict(format!("compile PDF was not base64: {error}")))
}

/// Convert markdown-style ATX headings (`#`, `##`, `###`...) to Typst heading
/// syntax (`=`, `==`, `===`...) so that document bodies authored as markdown
/// compile correctly under Typst. Only leading `#` sequences that look like
/// headings (optional spaces + text) are converted; `#` used as Typst code
/// syntax (followed by a letter/function name with no space) is left alone.
fn markdown_headings_to_typst(source: &str) -> String {
    source
        .lines()
        .map(|line| {
            let trimmed_start = line.trim_start_matches(' ');
            let hashes = trimmed_start.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&hashes)
                && trimmed_start.chars().nth(hashes).is_some_and(|c| c == ' ')
            {
                // Markdown heading: convert `### Title` → `=== Title`
                let leading_spaces = line.len() - trimmed_start.len();
                let rest = &trimmed_start[hashes + 1..];
                format!(
                    "{}{} {}",
                    " ".repeat(leading_spaces),
                    "=".repeat(hashes),
                    rest
                )
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn export_project(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
    Json(r): Json<ExportRequest>,
) -> Result<Json<ExportResponse>, AppError> {
    let principal = p.0;
    permitted(&principal, Permission::Document)?;
    let project = project_for(&s, &pid).await?;
    // The export archive is a full project snapshot: the generated master
    // includes every document, so `entry` only selects which document the
    // request was made from. Silently ignoring a bogus entry (previously any
    // value returned 200) made the field a lie — reject unknown entries so
    // clients get a 400 instead of a wrong-but-successful export.
    let document_paths: Vec<String> = s
        .repo
        .list_documents(project.id)
        .await?
        .into_iter()
        .map(|document| document.path)
        .collect();
    if !document_paths.contains(&r.entry) {
        return Err(AppError::BadRequest(format!(
            "entry must be a document path in this project: {}",
            r.entry
        )));
    }
    let references = s.repo.list_references(project.id).await?;
    let docs = gather_documents(s.repo.as_ref(), &project, &references).await?;
    let (bibliographies, missing) =
        document_bibliographies(s.repo.as_ref(), s.blobs.as_ref(), &references, &docs).await?;
    if !missing.is_empty() {
        return Err(AppError::Conflict(format!(
            "cited references are missing fulltext PDFs: {}",
            missing.join(", ")
        )));
    }
    // Review marks travel with the CRDT, not the document row: fetch each
    // document's synced review state and project it below (see
    // `project_review_marks` for the failure policy — sync being down fails
    // the export rather than dropping marks).
    let marks = project_review_marks(&s, &docs).await?;
    let mut sources = BTreeMap::new();
    let mut doc_yaml = BTreeMap::new();
    for doc in &docs {
        // Apply view-based projection so that exported content respects the
        // requested view (baseline/proposed/redline/public), matching the
        // behaviour of the interactive compile endpoint. The marks come from
        // each document's synced CRDT state; a document with none projects
        // its body unchanged.
        let projected = projected_source(
            &doc.body,
            marks.get(&doc.path).map_or(&[], Vec::as_slice),
            &r.view,
        )?;
        // Convert markdown-style headings to Typst syntax so document bodies
        // compile correctly. Markdown `#`/`##`/`###` headings are misread by
        // Typst as code-mode expressions, causing compile errors.
        let projected = markdown_headings_to_typst(&projected);
        sources.insert(doc.path.clone(), projected);
        doc_yaml.insert(doc.path.clone(), doc.yaml.clone());
    }
    // The generated master lives at the project root and includes each
    // document by its full project-relative path, so documents in nested
    // directories resolve correctly (a bare basename like "ch2.typ" would only
    // work for docs at the root). The user's own main.typ, if any, is skipped
    // from the includes to avoid self-inclusion (its body is replaced by the
    // generated master, which is the existing export contract).
    let master_path = "main.typ".to_owned();
    let master_source = docs
        .iter()
        .filter(|doc| doc.path != master_path)
        .map(|doc| format!("#include \"{}\"", doc.path))
        .collect::<Vec<_>>()
        .join("\n");
    sources.insert(master_path.clone(), master_source);
    let mut compile_request = CompileRequest {
        project_id: project.id,
        entry: master_path,
        sources,
        marks,
        view: r.view,
    };
    inject_per_document_bibliography(&mut compile_request, &doc_yaml);
    let compile = s.compile.compile(compile_request).await?;
    let pdf = decode_compile_pdf(&compile)?;
    let archive_documents = docs
        .iter()
        .map(|d| (d.path.clone(), d.body.clone()))
        .collect();
    let date = Utc::now().format("%Y-%m-%d").to_string();
    // The download is the portable ARCHIVE, not the compiled PDF; naming it
    // after the PDF filename misled every client (the archive contains all
    // documents + per-document RIS + the PDF). Use a stable, descriptive name.
    let zip_filename = format!("{}-export-{}.zip", project.name, date);
    let archive_input = ProjectArchiveInput {
        date,
        name: project.name.clone(),
        pdf,
        documents: archive_documents,
        bibliographies,
    };
    let export = build_project_archive(&archive_input, &pdf_compliance())
        .map_err(|error| AppError::Conflict(format!("export blocked: {error}")))?;
    let zip =
        write_zip(&export).map_err(|error| AppError::Conflict(format!("zip failed: {error}")))?;
    let references_export = s
        .references
        .export(project.clone(), archive_input.bibliographies.clone())
        .await?;
    audit(
        &s,
        &principal,
        project.id,
        "exported",
        "export",
        project.id,
        json!({"zip_filename": zip_filename}),
    )
    .await?;
    Ok(Json(ExportResponse {
        compile,
        references: references_export,
        zip_base64: Some(base64::engine::general_purpose::STANDARD.encode(zip)),
        zip_filename: Some(zip_filename),
    }))
}

fn pdf_compliance() -> PdfCompliance {
    PdfCompliance {
        no_watermark: true,
        not_protected: true,
        commentable: true,
        text_extractable: true,
        indexes_rendered: true,
        links_live: true,
    }
}

// --- Share link handlers ---

async fn create_share_link(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
    Json(r): Json<ShareLinkCreate>,
) -> Result<(StatusCode, Json<ShareLink>), AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let project = project_for(&s, &pid).await?;
    if !matches!(r.role.as_str(), "author" | "reviewer" | "read-only") {
        return Err(AppError::BadRequest("invalid share link role".into()));
    }
    // Validate the label like every other user string (it is echoed back in
    // listings; an unbounded/control-character label was the only unvalidated
    // one left).
    if let Some(label) = r.label.as_deref() {
        validate_text(label, "label", 256)?;
    }
    let link = s
        .repo
        .create_share_link(project.id, &r.role, &p.subject, r.label)
        .await?;
    // Creating a share link grants project access at the chosen role: record
    // who created it and with what role so the audit trail is complete.
    audit(
        &s,
        &p,
        project.id,
        "created",
        "share_link",
        project.id,
        json!({"role": r.role, "label": link.label, "token_hash": link.token_hash}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(link)))
}
async fn list_share_links(
    State(s): State<AppState>,
    p: Auth,
    Path(pid): Path<String>,
) -> Result<Json<Vec<ShareLink>>, AppError> {
    permitted(&p.0, Permission::Manage)?;
    let project = project_for(&s, &pid).await?;
    Ok(Json(s.repo.list_share_links(project.id).await?))
}
async fn delete_share_link(
    State(s): State<AppState>,
    p: Auth,
    Path((pid, token)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let p = p.0;
    permitted(&p, Permission::Manage)?;
    let project = project_for(&s, &pid).await?;
    // Created links can be revoked with their one-time token; listed/redacted
    // links use the non-secret hash returned as their revocation identifier.
    let link = if token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        s.repo
            .list_share_links(project.id)
            .await?
            .into_iter()
            .find(|link| link.token_hash.eq_ignore_ascii_case(&token))
            .ok_or(AppError::NotFound)?
    } else {
        s.repo
            .resolve_share_link(&token)
            .await
            .map_err(|_| AppError::NotFound)?
    };
    if link.project_id != project.id {
        return Err(AppError::NotFound);
    }
    s.repo.delete_share_link(&token).await?;
    // Revoking a share link removes a project access path: record who revoked it
    // and which link was removed so the audit trail is complete.
    audit(
        &s,
        &p,
        project.id,
        "deleted",
        "share_link",
        project.id,
        json!({"token_hash": link.token_hash, "role": link.role}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
/// Ordering used to decide whether a share link upgrades an existing
/// membership: owner > author > reviewer > read-only.
fn membership_role_rank(role: &MembershipRole) -> u8 {
    match role {
        MembershipRole::Owner => 3,
        MembershipRole::Author => 2,
        MembershipRole::Reviewer => 1,
        MembershipRole::ReadOnly => 0,
    }
}

async fn redeem_share_link(
    State(s): State<AppState>,
    p: Auth,
    Path(token): Path<String>,
) -> Result<Json<Value>, AppError> {
    let principal = p.0;
    let link = s.repo.resolve_share_link(&token).await?;
    if let Some(exp) = link.expires_at
        && exp < Utc::now()
    {
        return Err(AppError::BadRequest("share link has expired".into()));
    }
    let link_role = match link.role.as_str() {
        "author" => MembershipRole::Author,
        "reviewer" => MembershipRole::Reviewer,
        "read-only" => MembershipRole::ReadOnly,
        _ => return Err(AppError::BadRequest("invalid share link role".into())),
    };
    // Clamp the granted role to the user's IdP token role: a reviewer or
    // read-only user redeeming an author share link must not gain author
    // capabilities. The granted role is min(link_role, max_idp_role).
    let idp_rank = if principal.roles.contains(&Role::Author) {
        membership_role_rank(&MembershipRole::Author)
    } else if principal.roles.contains(&Role::Reviewer) {
        membership_role_rank(&MembershipRole::Reviewer)
    } else {
        membership_role_rank(&MembershipRole::ReadOnly)
    };
    let role = if membership_role_rank(&link_role) <= idp_rank {
        link_role
    } else {
        match idp_rank {
            r if r >= membership_role_rank(&MembershipRole::Author) => MembershipRole::Author,
            r if r >= membership_role_rank(&MembershipRole::Reviewer) => MembershipRole::Reviewer,
            _ => MembershipRole::ReadOnly,
        }
    };
    let existing = s
        .repo
        .get_membership(link.project_id, &principal.subject)
        .await;
    // Redeeming a share link grants (or upgrades) project access — record it so
    // the audit trail shows who gained access through which link. The role is
    // serialized before it is moved into the membership below.
    let granted_role = serde_json::to_value(&role).unwrap_or_default();
    match existing {
        Err(_) => {
            s.repo
                .create_membership(ProjectMembership {
                    project_id: link.project_id,
                    subject: principal.subject.clone(),
                    role,
                    created_at: Utc::now(),
                })
                .await?;
            audit(
                &s,
                &principal,
                link.project_id,
                "redeemed",
                "share_link",
                link.project_id,
                json!({"subject": principal.subject, "role": granted_role, "via": "share_link"}),
            )
            .await?;
        }
        Ok(existing) if membership_role_rank(&existing.role) < membership_role_rank(&role) => {
            // A link grants access at its chosen role: redeeming it upgrades an
            // existing lower membership. A higher existing role is never
            // downgraded (a link is a grant, not a restriction).
            s.repo
                .upsert_membership(ProjectMembership {
                    project_id: link.project_id,
                    subject: principal.subject.clone(),
                    role,
                    created_at: existing.created_at,
                })
                .await?;
            audit(
                &s,
                &principal,
                link.project_id,
                "redeemed",
                "share_link",
                link.project_id,
                json!({"subject": principal.subject, "role": granted_role, "via": "share_link", "upgraded_from": serde_json::to_value(&existing.role).unwrap_or_default()}),
            )
            .await?;
        }
        Ok(_) => {}
    }
    Ok(Json(json!({"project_id": link.project_id})))
}

#[must_use]
pub fn openapi_document() -> Value {
    // The OpenAPI document is data, not code: it lives in openapi.json
    // (embedded at compile time) so it stays machine-validatable and diffable.
    serde_json::from_str(include_str!("openapi.json")).expect("openapi.json must be valid JSON")
}
async fn openapi() -> Json<Value> {
    Json(openapi_document())
}

pub async fn serve(
    state: AppState,
    listener: tokio::net::TcpListener,
) -> Result<(), std::io::Error> {
    axum::serve(listener, router(state)).await
}
mod test_support;
mod tests;
