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
