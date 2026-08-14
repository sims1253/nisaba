#![cfg(test)]
use crate::*;
use axum::{body::Body, http::Request, response::Response};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use tower::util::ServiceExt;

fn auth() -> Authenticator {
    let jwk = jsonwebtoken::jwk::Jwk {
        common: jsonwebtoken::jwk::CommonParameters {
            key_id: Some("test".into()),
            key_algorithm: Some(jsonwebtoken::jwk::KeyAlgorithm::HS256),
            ..Default::default()
        },
        algorithm: jsonwebtoken::jwk::AlgorithmParameters::OctetKey(
            jsonwebtoken::jwk::OctetKeyParameters {
                value: base64::Engine::encode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    b"secret",
                ),
                ..Default::default()
            },
        ),
    };
    Authenticator {
        issuer: "issuer".into(),
        audience: "audience".into(),
        jwks: Arc::new(StaticJwks {
            keys: Arc::new(HashMap::from([("test".into(), jwk)])),
        }),
    }
}

fn token(subject: &str, role: &str) -> String {
    let expires_at = u64::try_from(Utc::now().timestamp() + 3600).unwrap();
    let claims = json!({
        "sub": subject,
        "roles": [role],
        "exp": expires_at,
        "iss": "issuer",
        "aud": "audience"
    });
    encode(
        &Header {
            alg: Algorithm::HS256,
            kid: Some("test".into()),
            ..Default::default()
        },
        &claims,
        &EncodingKey::from_secret(b"secret"),
    )
    .unwrap()
}

fn state() -> AppState {
    AppState::new(Arc::new(MemoryRepository::new()), auth())
}

async fn request(
    app: Router,
    method: &str,
    path: &str,
    subject: &str,
    role: &str,
    body: Option<Value>,
) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {}", token(subject, role)));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    app.oneshot(
        builder
            .body(Body::from(
                body.map_or_else(String::new, |value| value.to_string()),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn response_body<T: for<'de> Deserialize<'de>>(response: Response) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_project_and_document(app: Router) -> (Project, Document) {
    let response = request(
        app.clone(),
        "POST",
        "/projects",
        "alice",
        "author",
        Some(json!({"name": "Book"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let project: Project = response_body(response).await;
    let response = request(
        app,
        "POST",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        Some(json!({
            "path": "chapters/introduction.typ",
            "title": "Introduction",
            "body": "= Hello",
            "data": {}
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let document = response_body(response).await;
    (project, document)
}

#[tokio::test]
async fn health_is_public() {
    let response = router(state())
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn document_crud_is_flat_path_addressed_and_revision_checked() {
    let app = router(state());
    let (project, document) = create_project_and_document(app.clone()).await;

    let response = request(
        app.clone(),
        "GET",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        None,
    )
    .await;
    let documents: Vec<Document> = response_body(response).await;
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].path, "chapters/introduction.typ");

    let response = request(
        app.clone(),
        "PATCH",
        &format!("/projects/{}/documents/{}", project.id, document.id),
        "alice",
        "author",
        Some(json!({
            "path": "chapters/start.typ",
            "title": "Start",
            "body": "= Updated",
            "data": {"audience": "general"},
            "expected_revision": 0
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated: Document = response_body(response).await;
    assert_eq!(updated.revision, 1);
    assert_eq!(updated.body, "= Updated");

    let stale = request(
        app.clone(),
        "PATCH",
        &format!("/projects/{}/documents/{}", project.id, document.id),
        "alice",
        "author",
        Some(json!({"expected_revision": 0})),
    )
    .await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let deleted = request(
        app,
        "DELETE",
        &format!("/projects/{}/documents/{}", project.id, document.id),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unsafe_and_duplicate_paths_are_rejected() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    for path in ["../secret.typ", "/absolute.typ", "a//b.typ", r"a\b.typ"] {
        let response = request(
            app.clone(),
            "POST",
            &format!("/projects/{}/documents", project.id),
            "alice",
            "author",
            Some(json!({"path": path, "title": "bad"})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
    let duplicate = request(
        app,
        "POST",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        Some(json!({"path": "chapters/introduction.typ", "title": "duplicate"})),
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_document_cannot_be_addressed_through_another_project() {
    let app = router(state());
    let (_, document) = create_project_and_document(app.clone()).await;
    let response = request(
        app.clone(),
        "POST",
        "/projects",
        "alice",
        "author",
        Some(json!({"name": "Other"})),
    )
    .await;
    let other: Project = response_body(response).await;
    let response = request(
        app,
        "GET",
        &format!("/projects/{}/documents/{}", other.id, document.id),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn sync_fixture(role: MembershipRole) -> (AppState, Document) {
    let repository = Arc::new(MemoryRepository::new());
    let now = Utc::now();
    let project = repository
        .create_project(
            Project {
                id: Uuid::new_v4(),
                name: "Shared notes".into(),
                created_at: now,
                updated_at: now,
            },
            None,
        )
        .await
        .unwrap();
    repository
        .create_membership(ProjectMembership {
            project_id: project.id,
            subject: "alice".into(),
            role,
            created_at: now,
        })
        .await
        .unwrap();
    let document = repository
        .create_document(
            Document {
                id: Uuid::new_v4(),
                project_id: project.id,
                path: "main.typ".into(),
                title: "Main".into(),
                body: String::new(),
                data: BTreeMap::new(),
                revision: 0,
                updated_at: now,
            },
            None,
        )
        .await
        .unwrap();
    (
        AppState::new(repository, auth()).with_sync_authz_token("machine-secret"),
        document,
    )
}

#[tokio::test]
async fn sync_authorization_maps_project_roles() {
    for (membership_role, expected_role) in [
        (MembershipRole::Owner, "author"),
        (MembershipRole::Author, "author"),
        (MembershipRole::Reviewer, "reviewer"),
        (MembershipRole::ReadOnly, "read-only"),
    ] {
        let (state, document) = sync_fixture(membership_role).await;
        let response = router(state)
            .oneshot(
                Request::post("/internal/sync/authorize")
                    .header("authorization", "Bearer machine-secret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"subject": "alice", "document": document.id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value = response_body(response).await;
        assert_eq!(value["role"], expected_role);
    }
}

struct RecordingCompile(std::sync::Mutex<Option<CompileRequest>>);

#[async_trait]
impl CompileClient for RecordingCompile {
    async fn compile(&self, request: CompileRequest) -> Result<CompileResponse, AppError> {
        *self.0.lock().unwrap() = Some(request);
        Ok(CompileResponse {
            pdf_base64: Some("JVBERi0xLjQ=".into()),
            frames: vec![],
            span_map: vec![],
            diagnostics: vec![],
            outline: vec![],
            build_id: "test".into(),
        })
    }
}

#[tokio::test]
async fn compile_proxy_converts_markdown_headings_like_export() {
    // A document written with markdown `#` headings must compile the same way
    // in the editor preview as it does in the export path (which already
    // converts them); previously the same body failed only in the preview.
    let repository = Arc::new(MemoryRepository::new());
    let now = Utc::now();
    let project = repository
        .create_project(
            Project {
                id: Uuid::new_v4(),
                name: "Headings".into(),
                created_at: now,
                updated_at: now,
            },
            None,
        )
        .await
        .unwrap();
    repository
        .create_membership(ProjectMembership {
            project_id: project.id,
            subject: "alice".into(),
            role: MembershipRole::Owner,
            created_at: now,
        })
        .await
        .unwrap();
    let recorder = Arc::new(RecordingCompile(std::sync::Mutex::new(None)));
    let state = AppState::new(repository, auth())
        .with_exporters(recorder.clone(), Arc::new(UnconfiguredReferences));
    let response = request(
        router(state),
        "POST",
        "/api/compile",
        "alice",
        "author",
        Some(json!({
            "project_id": project.id,
            "entry": "main.typ",
            "sources": {"main.typ": "# Hello\n### Sub"},
            "mode": "document",
            "view": "baseline"
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = recorder.0.lock().unwrap().clone().unwrap();
    assert_eq!(forwarded.sources["main.typ"], "= Hello\n=== Sub");
}

#[tokio::test]
async fn compile_proxy_projects_review_marks_before_forwarding() {
    let repository = Arc::new(MemoryRepository::new());
    let now = Utc::now();
    let project = repository
        .create_project(
            Project {
                id: Uuid::new_v4(),
                name: "Compile".into(),
                created_at: now,
                updated_at: now,
            },
            None,
        )
        .await
        .unwrap();
    repository
        .create_membership(ProjectMembership {
            project_id: project.id,
            subject: "alice".into(),
            role: MembershipRole::Owner,
            created_at: now,
        })
        .await
        .unwrap();
    let recorder = Arc::new(RecordingCompile(std::sync::Mutex::new(None)));
    let state = AppState::new(repository, auth())
        .with_exporters(recorder.clone(), Arc::new(UnconfiguredReferences));
    let response = request(
        router(state),
        "POST",
        "/api/compile",
        "alice",
        "author",
        Some(json!({
            "project_id": project.id,
            "entry": "main.typ",
            "sources": {"main.typ": "ABC"},
            "marks": {"main.typ": [{
                "id": 1, "start": 1, "end": 2, "kind": "insert",
                "author": "alice", "timestamp": 1
            }]},
            "mode": "document",
            "view": "baseline"
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let forwarded = recorder.0.lock().unwrap().clone().unwrap();
    assert_eq!(forwarded.sources["main.typ"], "AC");
    assert!(forwarded.marks.is_empty());
}

#[test]
fn openapi_describes_flat_document_routes_only() {
    let text = openapi_document().to_string().to_lowercase();
    assert!(text.contains("/projects/{project_id}/documents"));
    assert!(text.contains("/projects/{project_id}/documents/{document_id}"));
}

#[tokio::test]
async fn reviewer_cannot_write_or_delete_documents() {
    let app = router(state());
    let (project, document) = create_project_and_document(app.clone()).await;
    let add = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/members", project.id),
        "alice",
        "author",
        Some(json!({"subject": "bob", "role": "reviewer"})),
    )
    .await;
    assert_eq!(add.status(), StatusCode::CREATED);
    // Reviewer (membership + IdP role) must be blocked from baseline writes:
    // PATCH/DELETE document, and create document.
    let cases: Vec<(String, String, Option<Value>)> = vec![
        (
            "PATCH".into(),
            format!("/projects/{}/documents/{}", project.id, document.id),
            Some(json!({"body": "sneaky overwrite"})),
        ),
        (
            "DELETE".into(),
            format!("/projects/{}/documents/{}", project.id, document.id),
            None,
        ),
        (
            "POST".into(),
            format!("/projects/{}/documents", project.id),
            Some(json!({"path": "new.typ", "title": "New"})),
        ),
    ];
    for (method, path, body) in cases {
        let response = request(app.clone(), &method, &path, "bob", "reviewer", body).await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{method} {path} should be forbidden for a reviewer"
        );
    }
    // The author can still edit.
    let edit = request(
        app.clone(),
        "PATCH",
        &format!("/projects/{}/documents/{}", project.id, document.id),
        "alice",
        "author",
        Some(json!({"body": "= Author edit"})),
    )
    .await;
    assert_eq!(edit.status(), StatusCode::OK);
    // And the reviewer can still READ the document (review workflow needs it).
    let read = request(
        app,
        "GET",
        &format!("/projects/{}/documents/{}", project.id, document.id),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(read.status(), StatusCode::OK);
}

#[tokio::test]
async fn nul_and_control_characters_are_rejected() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    // NUL byte in project name
    let nul_name = request(
        app.clone(),
        "POST",
        "/projects",
        "alice",
        "author",
        Some(json!({"name": "bad\u{0}name"})),
    )
    .await;
    assert_eq!(nul_name.status(), StatusCode::BAD_REQUEST);
    // Control character (tab) in document path
    let tab_path = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        Some(json!({"path": "a\tb.typ", "title": "bad"})),
    )
    .await;
    assert_eq!(tab_path.status(), StatusCode::BAD_REQUEST);
    // Trailing whitespace in path
    let trail = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        Some(json!({"path": "trail.typ ", "title": "bad"})),
    )
    .await;
    assert_eq!(trail.status(), StatusCode::BAD_REQUEST);
    // NUL in document body
    let nul_body = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        Some(json!({"path": "ok.typ", "title": "ok", "body": "a\u{0}b"})),
    )
    .await;
    assert_eq!(nul_body.status(), StatusCode::BAD_REQUEST);
    // NUL in a document metadata (data) map value — stored as jsonb, so an
    // unvalidated value would make Postgres reject the write with a 500.
    let nul_data = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/documents", project.id),
        "alice",
        "author",
        Some(json!({"path": "meta.typ", "title": "ok", "data": {"k": "v\u{0}"}})),
    )
    .await;
    assert_eq!(nul_data.status(), StatusCode::BAD_REQUEST);
    // ... and on PATCH, where the map previously replaced the stored one
    // verbatim.
    let created = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{}/documents", project.id),
            "alice",
            "author",
            Some(json!({"path": "meta.typ", "title": "ok"})),
        )
        .await,
    )
    .await;
    let document: Document = created;
    let nul_patch = request(
        app.clone(),
        "PATCH",
        &format!("/projects/{}/documents/{}", project.id, document.id),
        "alice",
        "author",
        Some(json!({"data": {"k": "v\u{0}"}})),
    )
    .await;
    assert_eq!(nul_patch.status(), StatusCode::BAD_REQUEST);
    // Oversized project name
    let huge = request(
        app,
        "POST",
        "/projects",
        "alice",
        "author",
        Some(json!({"name": "x".repeat(2000)})),
    )
    .await;
    assert_eq!(huge.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn owner_can_remove_members_but_not_the_owner_row() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    let add = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/members", project.id),
        "alice",
        "author",
        Some(json!({"subject": "reader", "role": "read-only"})),
    )
    .await;
    assert_eq!(add.status(), StatusCode::CREATED);
    let remove = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{}/members/reader", project.id),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(remove.status(), StatusCode::NO_CONTENT);
    let members: Vec<ProjectMembership> = response_body(
        request(
            app.clone(),
            "GET",
            &format!("/projects/{}/members", project.id),
            "alice",
            "author",
            None,
        )
        .await,
    )
    .await;
    assert!(!members.iter().any(|m| m.subject == "reader"));
    // Owner row cannot be removed.
    let owner_remove = request(
        app,
        "DELETE",
        &format!("/projects/{}/members/alice", project.id),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(owner_remove.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn inviting_an_existing_member_updates_their_role() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    for role in ["read-only", "reviewer", "author"] {
        let response = request(
            app.clone(),
            "POST",
            &format!("/projects/{}/members", project.id),
            "alice",
            "author",
            Some(json!({"subject": "reader", "role": role})),
        )
        .await;
        assert!(
            response.status().is_success(),
            "role {role}: {}",
            response.status()
        );
    }
    let members: Vec<ProjectMembership> = response_body(
        request(
            app,
            "GET",
            &format!("/projects/{}/members", project.id),
            "alice",
            "author",
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        members
            .iter()
            .find(|member| member.subject == "reader")
            .map(|member| &member.role),
        Some(&MembershipRole::Author)
    );
}

#[tokio::test]
async fn any_member_can_list_members() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    let add = request(
        app.clone(),
        "POST",
        &format!("/projects/{}/members", project.id),
        "alice",
        "author",
        Some(json!({"subject": "bob", "role": "reviewer"})),
    )
    .await;
    assert_eq!(add.status(), StatusCode::CREATED);
    let response = request(
        app,
        "GET",
        &format!("/projects/{}/members", project.id),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn share_link_revocation_actually_revokes() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    let created: ShareLink = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{}/share-links", project.id),
            "alice",
            "author",
            Some(json!({"role": "read-only"})),
        )
        .await,
    )
    .await;
    assert!(!created.token.is_empty());
    let revoke = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{}/share-links/{}", project.id, created.token),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
    // Redeeming the revoked token must now fail.
    let redeem = request(
        app.clone(),
        "POST",
        &format!("/share/{}/redeem", created.token),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(redeem.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn redacted_share_link_can_be_revoked_by_its_list_identifier() {
    let app = router(state());
    let (project, _) = create_project_and_document(app.clone()).await;
    let created: ShareLink = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{}/share-links", project.id),
            "alice",
            "author",
            Some(json!({"role": "reviewer"})),
        )
        .await,
    )
    .await;
    let revoke = request(
        app.clone(),
        "DELETE",
        &format!(
            "/projects/{}/share-links/{}",
            project.id, created.token_hash
        ),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(revoke.status(), StatusCode::NO_CONTENT);
    let redeem = request(
        app,
        "POST",
        &format!("/share/{}/redeem", created.token),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(redeem.status(), StatusCode::NOT_FOUND);
}

#[test]
fn openapi_describes_all_public_routes() {
    let text = openapi_document().to_string().to_lowercase();
    for path in [
        "/projects/{project_id}/documents",
        "/projects/{project_id}/documents/{document_id}",
        "/projects/{project_id}/members/{subject}",
        "/projects/{project_id}/membership",
        "/projects/{project_id}/share-links",
        "/share/{token}/redeem",
        "/projects/{project_id}/audit",
        "/projects/{project_id}/documents/{document_id}/history",
        "/api/compile",
        "/healthz",
        "/health/ready",
    ] {
        assert!(text.contains(path), "openapi must document {path}");
    }
}
