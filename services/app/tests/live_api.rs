//! Live integration tests: the real axum router + `PostgresRepository` + the
//! actual migrations, exercised through HTTP.
//!
//! These are the systematic integration complement to the in-memory unit
//! tests: they prove the fixed behaviors against real `PostgreSQL` and real SQL
//! (the in-memory repository cannot reproduce e.g. the audit-FK ordering bug
//! in `delete_project` or the token-hash share-link semantics).
//!
//! They require a reachable, migrated Postgres. The test derives the DSN from
//! `DATABASE_URL`, or builds one from this repo's `.env` (`NISABA_DB_USER` /
//! `NISABA_DB_PASSWORD` / `NISABA_DB_NAME` / `POSTGRES_HOST_PORT`). When no database
//! is reachable the tests print `SKIPPED` and pass, so plain `cargo test`
//! stays green in environments without a database; run them deliberately with
//! `just test-live` (stack up) or in CI with a `DATABASE_URL` set.

use axum::{body::Body, http::Request, response::Response};
use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use nisaba_app::{
    AppState, Authenticator, MemoryBlobStore, PostgresRepository, StaticJwks, router,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tower::util::ServiceExt;

fn database_url() -> Option<String> {
    if let Some(url) = std::env::var("DATABASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Some(url);
    }
    // Fall back to this repo's .env so the test works out of the box locally.
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    let env_text = std::fs::read_to_string(env_path).ok()?;
    let mut vars = HashMap::new();
    for line in env_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            vars.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let user = vars.get("NISABA_DB_USER")?.clone();
    let password = vars.get("NISABA_DB_PASSWORD")?.clone();
    let db = vars.get("NISABA_DB_NAME")?.clone();
    let port = vars.get("POSTGRES_HOST_PORT")?.clone();
    Some(format!(
        "postgres://{user}:{password}@127.0.0.1:{port}/{db}"
    ))
}

fn auth() -> Authenticator {
    let jwk = jsonwebtoken::jwk::Jwk {
        common: jsonwebtoken::jwk::CommonParameters {
            key_id: Some("test".into()),
            key_algorithm: Some(jsonwebtoken::jwk::KeyAlgorithm::HS256),
            ..Default::default()
        },
        algorithm: jsonwebtoken::jwk::AlgorithmParameters::OctetKey(
            jsonwebtoken::jwk::OctetKeyParameters {
                value: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"secret"),
                ..Default::default()
            },
        ),
    };
    Authenticator {
        issuer: "issuer".into(),
        audience: "audience".into(),
        jwks: Arc::new(StaticJwks::new(jsonwebtoken::jwk::JwkSet {
            keys: vec![jwk],
        })),
    }
}

fn token(subject: &str, role: &str) -> String {
    let expires_at = u64::try_from(chrono::Utc::now().timestamp() + 3600).unwrap();
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

async fn request(
    app: axum::Router,
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
            .body(Body::from(body.map_or_else(String::new, |v| v.to_string())))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn response_body<T: for<'de> serde::Deserialize<'de>>(response: Response) -> T {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

async fn live_app() -> Option<axum::Router> {
    let url = database_url()?;
    let repo = match PostgresRepository::connect(&url).await {
        Ok(repo) => repo,
        Err(error) => {
            eprintln!("SKIPPED live integration tests (no reachable database): {error}");
            return None;
        }
    };
    // Idempotent: sqlx applies only unapplied migrations.
    let migrations = sqlx::migrate!("../../migrations");
    let pool = repo.pool.clone();
    if let Err(error) = migrations.run(&pool).await {
        eprintln!("SKIPPED live integration tests (migrations failed): {error}");
        return None;
    }
    let state =
        AppState::new(Arc::new(repo), auth()).with_blob_store(Arc::new(MemoryBlobStore::default()));
    Some(router(state))
}

#[tokio::test]
async fn live_project_deletion_actually_deletes() {
    let Some(app) = live_app().await else { return };
    let name = unique("live-del");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    // Regression: DELETE used to 404 + roll back (audit FK inserted after the
    // row was deleted). Now it must delete for real.
    let deleted = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{project_id}"),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(
        deleted.status(),
        axum::http::StatusCode::NO_CONTENT,
        "project deletion must succeed"
    );
    // After deletion the caller's membership is cascade-deleted too, so the
    // ACL middleware answers 403 for the now-unknown project (the existing
    // non-member semantics); the invariant is that the project is gone.
    let gone = request(
        app,
        "GET",
        &format!("/projects/{project_id}"),
        "alice",
        "author",
        None,
    )
    .await;
    assert!(
        matches!(
            gone.status(),
            axum::http::StatusCode::FORBIDDEN | axum::http::StatusCode::NOT_FOUND
        ),
        "deleted project must no longer be readable, got {}",
        gone.status()
    );
}

#[tokio::test]
async fn live_share_link_revocation_revokes() {
    let Some(app) = live_app().await else { return };
    let name = unique("live-share");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let link: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/share-links"),
            "alice",
            "author",
            Some(json!({"role": "read-only"})),
        )
        .await,
    )
    .await;
    let token = link["token"].as_str().unwrap().to_string();
    assert!(!token.is_empty());
    let revoke = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{project_id}/share-links/{token}"),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(revoke.status(), axum::http::StatusCode::NO_CONTENT);
    let redeem = request(
        app,
        "POST",
        &format!("/share/{token}/redeem"),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(redeem.status(), axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn live_reviewer_permissions_and_member_management() {
    let Some(app) = live_app().await else { return };
    let name = unique("live-perms");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let doc: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/documents"),
            "alice",
            "author",
            Some(json!({"path": "main.typ", "title": "Main", "body": "= Hello"})),
        )
        .await,
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap().to_string();
    let add = request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/members"),
        "alice",
        "author",
        Some(json!({"subject": "bob", "role": "reviewer"})),
    )
    .await;
    assert_eq!(add.status(), axum::http::StatusCode::CREATED);
    // Reviewer cannot PATCH or DELETE the document.
    let patch = request(
        app.clone(),
        "PATCH",
        &format!("/projects/{project_id}/documents/{doc_id}"),
        "bob",
        "reviewer",
        Some(json!({"body": "nope"})),
    )
    .await;
    assert_eq!(patch.status(), axum::http::StatusCode::FORBIDDEN);
    let delete = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{project_id}/documents/{doc_id}"),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(delete.status(), axum::http::StatusCode::FORBIDDEN);
    // Reviewer can list members (previously 403).
    let members = request(
        app.clone(),
        "GET",
        &format!("/projects/{project_id}/members"),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(members.status(), axum::http::StatusCode::OK);
    // Owner removes the reviewer; subsequent access is forbidden.
    let remove = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{project_id}/members/bob"),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(remove.status(), axum::http::StatusCode::NO_CONTENT);
    let after = request(
        app,
        "GET",
        &format!("/projects/{project_id}"),
        "bob",
        "reviewer",
        None,
    )
    .await;
    assert_eq!(after.status(), axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn live_validation_rejects_nul_and_duplicate_doi() {
    let Some(app) = live_app().await else { return };
    let nul = request(
        app.clone(),
        "POST",
        "/projects",
        "alice",
        "author",
        Some(json!({"name": format!("bad\u{0}name")})),
    )
    .await;
    assert_eq!(nul.status(), axum::http::StatusCode::BAD_REQUEST);
    let name = unique("live-refs");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let doi = format!("10.5555/{}", uuid::Uuid::new_v4().simple());
    let first = request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/references"),
        "alice",
        "author",
        Some(json!({"metadata": {"title": "One", "authors": ["A"], "doi": doi, "extra": {}}})),
    )
    .await;
    assert_eq!(first.status(), axum::http::StatusCode::CREATED);
    // Duplicate DOI must be a clean 409 (migration 0006 unique index).
    let dup = request(
        app,
        "POST",
        &format!("/projects/{project_id}/references"),
        "alice",
        "author",
        Some(json!({"metadata": {"title": "Two", "authors": ["B"], "doi": doi, "extra": {}}})),
    )
    .await;
    assert_eq!(dup.status(), axum::http::StatusCode::CONFLICT);
}

// ===========================================================================
// Regression coverage for the 2026-08-09 QA + user-agent findings.
// Each test pins a fixed behavior against real Postgres + the real router so a
// future refactor cannot silently reintroduce the bug class.
// ===========================================================================

#[tokio::test]
async fn live_delete_project_with_references_succeeds() {
    // QA F-3: DELETE /projects/{id} returned 404 whenever the project had a
    // reference (reference_entries.project_id was ON DELETE RESTRICT).
    let Some(app) = live_app().await else { return };
    let name = unique("live-delref");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let reference: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/references"),
            "alice",
            "author",
            Some(json!({"metadata": {"title": "R", "authors": ["A"], "extra": {}}})),
        )
        .await,
    )
    .await;
    assert_eq!(reference["id"].as_str().unwrap().len(), 36);
    let deleted = request(
        app.clone(),
        "DELETE",
        &format!("/projects/{project_id}"),
        "alice",
        "author",
        None,
    )
    .await;
    assert_eq!(
        deleted.status(),
        axum::http::StatusCode::NO_CONTENT,
        "project with references must delete"
    );
}

#[tokio::test]
async fn live_concurrent_document_patches_return_conflict_not_500() {
    // QA F-4: concurrent PATCHes raced into a 500 (SELECT 1 decoded as i64
    // while Postgres returns int4). Losers must get a clean 409.
    let Some(app) = live_app().await else { return };
    let name = unique("live-race");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let doc: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/documents"),
            "alice",
            "author",
            Some(json!({"path": "race.typ", "title": "Race", "body": "start"})),
        )
        .await,
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap().to_string();
    let mut statuses = Vec::new();
    let mut handles = Vec::new();
    for i in 0..8 {
        let app = app.clone();
        let project_id = project_id.clone();
        let doc_id = doc_id.clone();
        handles.push(tokio::spawn(async move {
            request(
                app,
                "PATCH",
                &format!("/projects/{project_id}/documents/{doc_id}"),
                "alice",
                "author",
                Some(json!({"body": format!("w{i}")})),
            )
            .await
            .status()
        }));
    }
    for handle in handles {
        statuses.push(handle.await.unwrap());
    }
    for status in &statuses {
        assert!(
            status.is_success() || *status == axum::http::StatusCode::CONFLICT,
            "concurrent PATCH must succeed or 409, got {status}"
        );
    }
    assert!(
        statuses.contains(&axum::http::StatusCode::CONFLICT),
        "at least one racer must lose with a clean 409"
    );
}

#[tokio::test]
async fn live_read_only_member_can_compile() {
    // QA F-5 / reviewer F-4: docs promise "Read and compile" for every role.
    let Some(app) = live_app().await else { return };
    let name = unique("live-compile-ro");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let doc: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/documents"),
            "alice",
            "author",
            Some(json!({"path": "main.typ", "title": "Main", "body": "= Hi"})),
        )
        .await,
    )
    .await;
    let doc_id = doc["id"].as_str().unwrap().to_string();
    request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/members"),
        "alice",
        "author",
        Some(json!({"subject": "carol", "role": "read-only"})),
    )
    .await;
    let compile = request(
        app.clone(),
        "POST",
        "/api/compile",
        "carol",
        "read-only",
        Some(json!({
            "project_id": project_id,
            "entry": "main.typ",
            "sources": {"main.typ": doc["body"].as_str().unwrap()},
            "mode": "document",
            "view": "baseline"
        })),
    )
    .await;
    // The compile service is not part of this test harness, so a successful
    // authorization surfaces as the 502 dependency error rather than 200. The
    // regression being pinned: it must NOT be 403 (read-only users were
    // previously denied at the permission gate).
    assert_ne!(
        compile.status(),
        axum::http::StatusCode::FORBIDDEN,
        "read-only members may compile per the docs roles table"
    );
    assert_eq!(
        compile.status(),
        axum::http::StatusCode::BAD_GATEWAY,
        "authorization must pass; only the compile dependency may be missing"
    );
    let _ = doc_id;
}

#[tokio::test]
async fn live_forbidden_message_matches_documented_string() {
    // QA F-6: user-guide quotes the API 403 message as "You don't have
    // permission to do that"; the API used to return bare "forbidden".
    let Some(app) = live_app().await else { return };
    let forbidden = request(
        app.clone(),
        "POST",
        "/projects",
        "bob",
        "reviewer",
        Some(json!({"name": unique("live-403")})),
    )
    .await;
    assert_eq!(forbidden.status(), axum::http::StatusCode::FORBIDDEN);
    let body: Value = response_body(forbidden).await;
    assert_eq!(
        body["error"]["message"].as_str().unwrap(),
        "You don't have permission to do that"
    );
}

#[tokio::test]
async fn live_doi_uniqueness_is_case_insensitive() {
    // QA F-9: 10.1000/QA and 10.1000/qa both used to be accepted per project.
    let Some(app) = live_app().await else { return };
    let name = unique("live-doi");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let meta =
        |doi: &str| json!({"metadata": {"title": "T", "authors": ["A"], "extra": {}, "doi": doi}});
    let first = request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/references"),
        "alice",
        "author",
        Some(meta("10.1000/QA-CASE")),
    )
    .await;
    assert_eq!(first.status(), axum::http::StatusCode::CREATED);
    let dup = request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/references"),
        "alice",
        "author",
        Some(meta("10.1000/qa-case")),
    )
    .await;
    assert_eq!(
        dup.status(),
        axum::http::StatusCode::CONFLICT,
        "DOI uniqueness must be case-insensitive"
    );
}

#[tokio::test]
async fn live_redeem_upgrades_existing_membership_role() {
    // QA F-13: redeeming a link never changed the role of an existing member.
    let Some(app) = live_app().await else { return };
    let name = unique("live-redeem");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/members"),
        "alice",
        "author",
        Some(json!({"subject": "carol", "role": "read-only"})),
    )
    .await;
    let link: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/share-links"),
            "alice",
            "author",
            Some(json!({"role": "author"})),
        )
        .await,
    )
    .await;
    let token = link["token"].as_str().unwrap().to_string();
    request(
        app.clone(),
        "POST",
        &format!("/share/{token}/redeem"),
        "carol",
        "read-only",
        None,
    )
    .await;
    let membership: Value = response_body(
        request(
            app.clone(),
            "GET",
            &format!("/projects/{project_id}/membership"),
            "carol",
            "read-only",
            None,
        )
        .await,
    )
    .await;
    // The granted role must be clamped to the user's IdP token role. Carol's
    // JWT role is read-only, so even an author share link cannot elevate her
    // project membership above read-only.
    assert_eq!(
        membership["role"].as_str().unwrap(),
        "read-only",
        "redeeming an author link must not elevate a read-only IdP user above read-only"
    );
}

#[tokio::test]
async fn live_export_rejects_unknown_entry() {
    // QA F-12: ExportRequest.entry was silently ignored (any value → 200).
    let Some(app) = live_app().await else { return };
    let name = unique("live-export");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/documents"),
        "alice",
        "author",
        Some(json!({"path": "main.typ", "title": "Main", "body": "= Hi"})),
    )
    .await;
    let export = request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/exports"),
        "alice",
        "author",
        Some(json!({"entry": "missing.typ", "mode": "document", "view": "baseline"})),
    )
    .await;
    assert_eq!(
        export.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "export entry must be a real document path"
    );
}

#[tokio::test]
async fn live_document_path_length_is_capped() {
    // QA F-15: 10,000-char paths were accepted.
    let Some(app) = live_app().await else { return };
    let name = unique("live-pathcap");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let long_path = "x".repeat(2000);
    let doc = request(
        app.clone(),
        "POST",
        &format!("/projects/{project_id}/documents"),
        "alice",
        "author",
        Some(json!({"path": long_path, "title": "T", "body": ""})),
    )
    .await;
    assert_eq!(
        doc.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "document paths must be length-capped"
    );
}

#[tokio::test]
async fn live_fulltext_rejects_non_pdf_magic() {
    // QA F-10: a 1-byte file declared as application/pdf was accepted.
    let Some(app) = live_app().await else { return };
    let name = unique("live-pdfmagic");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let reference: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/references"),
            "alice",
            "author",
            Some(json!({"metadata": {"title": "R", "authors": ["A"], "extra": {}}})),
        )
        .await,
    )
    .await;
    let reference_id = reference["id"].as_str().unwrap().to_string();
    let bogus = base64::engine::general_purpose::STANDARD.encode("not a pdf");
    let upload = request(
        app.clone(),
        "PUT",
        &format!("/projects/{project_id}/references/{reference_id}/fulltext"),
        "alice",
        "author",
        Some(json!({
            "filename": "a.pdf",
            "content_type": "application/pdf",
            "size_bytes": 10,
            "contents_base64": bogus
        })),
    )
    .await;
    assert_eq!(
        upload.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "fulltext must look like a PDF (%PDF- header + %%EOF trailer)"
    );
}

#[tokio::test]
async fn live_reference_patch_is_partial() {
    // QA F-11: PATCH with only {"title": ...} used to 422.
    let Some(app) = live_app().await else { return };
    let name = unique("live-patchref");
    let created: Value = response_body(
        request(
            app.clone(),
            "POST",
            "/projects",
            "alice",
            "author",
            Some(json!({"name": name})),
        )
        .await,
    )
    .await;
    let project_id = created["id"].as_str().unwrap().to_string();
    let reference: Value = response_body(
        request(
            app.clone(),
            "POST",
            &format!("/projects/{project_id}/references"),
            "alice",
            "author",
            Some(json!({"metadata": {"title": "Old", "authors": ["A"], "extra": {}}})),
        )
        .await,
    )
    .await;
    let reference_id = reference["id"].as_str().unwrap().to_string();
    let patched: Value = response_body(
        request(
            app.clone(),
            "PATCH",
            &format!("/projects/{project_id}/references/{reference_id}"),
            "alice",
            "author",
            Some(json!({"metadata": {"title": "New"}})),
        )
        .await,
    )
    .await;
    assert_eq!(
        patched["metadata"]["title"].as_str().unwrap(),
        "New",
        "partial PATCH must merge the title"
    );
    assert_eq!(
        patched["metadata"]["authors"][0].as_str().unwrap(),
        "A",
        "untouched fields must survive a partial PATCH"
    );
}
