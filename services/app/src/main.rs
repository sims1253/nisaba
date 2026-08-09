use std::sync::Arc;

use jsonwebtoken::jwk::JwkSet;
use nisaba_app::{
    AppState, Authenticator, HttpCompileClient, NisabaReferencesExporter, PostgresRepository,
    S3BlobStore, StaticJwks, serve,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let issuer =
        std::env::var("NISABA_OIDC_ISSUER").unwrap_or_else(|_| "https://issuer.invalid".into());
    let audience = std::env::var("NISABA_OIDC_AUDIENCE").unwrap_or_else(|_| "nisaba".into());
    let jwks = std::env::var("NISABA_OIDC_JWKS_JSON")
        .ok()
        .map(|raw| serde_json::from_str::<JwkSet>(&raw))
        .transpose()?;
    let jwks = jwks.map_or_else(
        || StaticJwks::new(JwkSet { keys: Vec::new() }),
        StaticJwks::new,
    );
    let sync_authz_token = std::env::var("NISABA_SYNC_AUTHZ_TOKEN").unwrap_or_default();
    let compile_token = std::env::var("NISABA_COMPILE_TOKEN").unwrap_or_default();
    if sync_authz_token.trim().is_empty() {
        return Err("NISABA_SYNC_AUTHZ_TOKEN is required and must not be empty".into());
    }
    if compile_token.trim().is_empty() {
        return Err("NISABA_COMPILE_TOKEN is required and must not be empty".into());
    }
    // MemoryRepository is never available in the production binary.
    // The production binary always uses Postgres + S3. MemoryRepository exists
    // only in the library crate for tests.
    let (repository, blobs): (
        Arc<dyn nisaba_app::Repository>,
        Arc<dyn nisaba_app::BlobStore>,
    ) = (
        Arc::new(PostgresRepository::from_env().await?),
        Arc::new(S3BlobStore::from_env().await?),
    );
    let state = AppState::new(
        repository.clone(),
        Authenticator {
            issuer,
            audience,
            jwks: Arc::new(jwks),
        },
    )
    .with_sync_authz_token(sync_authz_token)
    .with_exporters(
        Arc::new(HttpCompileClient::new(
            std::env::var("NISABA_COMPILE_URL").unwrap_or_else(|_| "http://compile:8080".into()),
            compile_token,
        )),
        Arc::new(NisabaReferencesExporter {
            repo: repository,
            blobs: blobs.clone(),
        }),
    )
    .with_blob_store(blobs);
    let address = std::env::var("NISABA_APP_ADDR")
        .or_else(|_| std::env::var("PORT").map(|port| format!("0.0.0.0:{port}")))
        .unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    serve(state, listener).await?;
    Ok(())
}
