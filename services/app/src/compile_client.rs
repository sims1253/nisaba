//! Compile service and reference export adapters.

use async_trait::async_trait;
use base64::Engine as _;
use nisaba_references::{Bibliography, ExportManifest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

use crate::types::{
    AppError, CompileMode, CompileRequest, CompileResponse, CompileView, ExportFile, Project,
    ReferenceExport,
};

#[async_trait]
pub trait CompileClient: Send + Sync {
    async fn compile(&self, request: CompileRequest) -> Result<CompileResponse, AppError>;
}

/// HTTP adapter for the compile service. It keeps the app's public contract stable while
/// speaking the compile service's internal `pdf`/base64 contract and authenticating the hop.
pub struct HttpCompileClient {
    client: reqwest::Client,
    endpoint: String,
    internal_token: String,
}
impl HttpCompileClient {
    pub fn new(endpoint: impl Into<String>, internal_token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(150))
                .build()
                .expect("reqwest client builder is infallible"),
            endpoint: endpoint.into().trim_end_matches('/').to_owned(),
            internal_token: internal_token.into(),
        }
    }
}
#[async_trait]
impl CompileClient for HttpCompileClient {
    async fn compile(&self, request: CompileRequest) -> Result<CompileResponse, AppError> {
        let view = match request.view {
            CompileView::Baseline => "baseline",
            CompileView::Proposed => "proposed",
            CompileView::Public => "public",
            CompileView::Redline => "redline",
        };
        let payload = json!({
            "project_id": request.project_id.to_string(),
            "entry": request.entry,
            "sources": request.sources,
            "mode": match request.mode { CompileMode::Document => "document", CompileMode::Full => "full" },
            "view": view,
        });
        let response = self
            .client
            .post(format!("{}/compile", self.endpoint))
            // The compile service authenticates the hop with the standard
            // Authorization: Bearer header (see its `authorized()`).
            .bearer_auth(&self.internal_token)
            .json(&payload)
            .send()
            .await
            .map_err(|error| AppError::Dependency(format!("compile request failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 => AppError::Unauthorized(format!("compile service: {detail}")),
                403 => AppError::Forbidden,
                429 | 413 => {
                    AppError::BadRequest(format!("compile service rejected request: {detail}"))
                }
                s if (400..500).contains(&s) => {
                    AppError::BadRequest(format!("compile service returned {status}: {detail}"))
                }
                _ => AppError::Dependency(format!("compile service returned {status}: {detail}")),
            });
        }
        let body: CompileWireResponse = response
            .json()
            .await
            .map_err(|error| AppError::Dependency(format!("invalid compile response: {error}")))?;
        Ok(CompileResponse {
            pdf_base64: body.pdf,
            frames: body.frames,
            span_map: body.span_map,
            diagnostics: body.diagnostics,
            outline: body.outline,
            build_id: body.build_id,
        })
    }
}
#[derive(Debug, Deserialize)]
struct CompileWireResponse {
    pdf: Option<String>,
    frames: Vec<Value>,
    span_map: Vec<Value>,
    diagnostics: Vec<Value>,
    outline: Vec<Value>,
    build_id: String,
}
#[async_trait]
pub trait ReferenceExporter: Send + Sync {
    /// Packages per-document bibliographies (RIS + full-text PDFs) into export files.
    /// The caller attaches fulltext bytes to cited entries and builds each document's
    /// `Bibliography`; packaging only numbers, names, and lays out the tree.
    async fn export(
        &self,
        project: Project,
        bibliographies: Vec<Bibliography>,
    ) -> Result<ReferenceExport, AppError>;
}
pub struct UnconfiguredCompile;
#[async_trait]
impl CompileClient for UnconfiguredCompile {
    async fn compile(&self, _: CompileRequest) -> Result<CompileResponse, AppError> {
        Err(AppError::Dependency(
            "compile service is not configured".into(),
        ))
    }
}
pub struct UnconfiguredReferences;
#[async_trait]
impl ReferenceExporter for UnconfiguredReferences {
    async fn export(&self, _: Project, _: Vec<Bibliography>) -> Result<ReferenceExport, AppError> {
        Err(AppError::Dependency(
            "reference exporter is not configured".into(),
        ))
    }
}

/// Adapter for nisaba-references. It numbers each document bibliography, writes the RIS
/// and full-text files, and validates the tree. PDF bytes come in already attached to
/// cited entries (the caller fetched them from the blob store), so uncited references
/// without attachments never block packaging — the inverted gate lived in the old caller.
/// The per-document bibliographies arrive with fulltext already attached, so this
/// adapter performs no I/O of its own and holds no handles.
pub struct NisabaReferencesExporter;
#[async_trait]
impl ReferenceExporter for NisabaReferencesExporter {
    async fn export(
        &self,
        project: Project,
        bibliographies: Vec<Bibliography>,
    ) -> Result<ReferenceExport, AppError> {
        // Each bibliography carries its own output directory; the project
        // name is accepted only to satisfy the trait and is not used for paths.
        let _ = project;
        let mut files = Vec::new();
        for bibliography in bibliographies {
            // ExportManifest::build numbers by first-citation order, rejects cited
            // entries missing fulltext (the correct, non-inverted gate), and emits
            // RIS + `<n>_<author>_<year>.pdf` files under that directory.
            let manifest = ExportManifest::build(&bibliography).map_err(|error| {
                AppError::Conflict(format!("reference export blocked: {error}"))
            })?;
            files.extend(manifest.files);
        }
        let files = files
            .into_iter()
            .map(|file| ExportFile {
                path: file.path,
                content_base64: base64::engine::general_purpose::STANDARD.encode(file.contents),
            })
            .collect();
        Ok(ReferenceExport { files })
    }
}
