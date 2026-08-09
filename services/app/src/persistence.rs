//! Durable application adapters: `PostgreSQL` metadata and S3-compatible fulltext blobs.
#![allow(
    clippy::wildcard_imports,
    clippy::needless_pass_by_value,
    clippy::redundant_closure
)]

use super::*;
use aws_sdk_s3::{Client as S3Client, primitives::ByteStream};
use sqlx::{
    PgPool, Row,
    postgres::{PgPoolOptions, PgRow},
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

fn db_error(error: sqlx::Error) -> RepoError {
    if let sqlx::Error::Database(e) = &error {
        match e.code().as_deref() {
            Some("23505") => return RepoError::Conflict("database uniqueness constraint".into()),
            Some("23503") => return RepoError::NotFound,
            _ => {}
        }
    }
    RepoError::Failure(error.to_string())
}
fn json_error(error: serde_json::Error) -> RepoError {
    RepoError::Failure(error.to_string())
}
fn required<T>(value: Option<T>) -> Result<T, RepoError> {
    value.ok_or(RepoError::NotFound)
}
fn role(value: &str) -> Result<MembershipRole, RepoError> {
    match value {
        "owner" => Ok(MembershipRole::Owner),
        "author" => Ok(MembershipRole::Author),
        "reviewer" => Ok(MembershipRole::Reviewer),
        "read-only" => Ok(MembershipRole::ReadOnly),
        _ => Err(RepoError::Failure(format!("invalid role {value}"))),
    }
}
fn role_name(value: &MembershipRole) -> &'static str {
    match value {
        MembershipRole::Owner => "owner",
        MembershipRole::Author => "author",
        MembershipRole::Reviewer => "reviewer",
        MembershipRole::ReadOnly => "read-only",
    }
}
fn row_project(row: &PgRow) -> Result<Project, RepoError> {
    Ok(Project {
        id: row.try_get("id").map_err(db_error)?,
        name: row.try_get("name").map_err(db_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
        updated_at: row.try_get("updated_at").map_err(db_error)?,
    })
}
fn row_document(row: &PgRow) -> Result<Document, RepoError> {
    Ok(Document {
        id: row.try_get("id").map_err(db_error)?,
        project_id: row.try_get("project_id").map_err(db_error)?,
        path: row.try_get("path").map_err(db_error)?,
        title: row.try_get("title").map_err(db_error)?,
        body: row.try_get("body").map_err(db_error)?,
        data: serde_json::from_value(row.try_get("data").map_err(db_error)?).map_err(json_error)?,
        revision: row
            .try_get::<i64, _>("revision")
            .map_err(db_error)?
            .try_into()
            .map_err(|_| RepoError::Failure("invalid document revision".into()))?,
        updated_at: row.try_get("updated_at").map_err(db_error)?,
    })
}
fn row_reference(row: &PgRow) -> Result<ReferenceEntry, RepoError> {
    Ok(ReferenceEntry {
        id: row.try_get("id").map_err(db_error)?,
        project_id: row.try_get("project_id").map_err(db_error)?,
        metadata: serde_json::from_value(row.try_get("metadata").map_err(db_error)?)
            .map_err(json_error)?,
        provenance: row
            .try_get::<Option<Value>, _>("provenance")
            .map_err(db_error)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(json_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
        updated_at: row.try_get("updated_at").map_err(db_error)?,
    })
}
fn row_fulltext(row: &PgRow) -> Result<FulltextMetadata, RepoError> {
    Ok(FulltextMetadata {
        reference_id: row.try_get("reference_id").map_err(db_error)?,
        blob_ref: row.try_get("blob_ref").map_err(db_error)?,
        filename: row.try_get("filename").map_err(db_error)?,
        content_type: row.try_get("content_type").map_err(db_error)?,
        size_bytes: row
            .try_get::<i64, _>("size_bytes")
            .map_err(db_error)?
            .try_into()
            .map_err(|_| RepoError::Failure("invalid blob size".into()))?,
        checksum_sha256: row.try_get("checksum_sha256").map_err(db_error)?,
        uploaded_at: row.try_get("uploaded_at").map_err(db_error)?,
    })
}
fn row_audit(row: &PgRow) -> Result<AuditEvent, RepoError> {
    Ok(AuditEvent {
        id: row.try_get("id").map_err(db_error)?,
        project_id: row.try_get("project_id").map_err(db_error)?,
        actor: row.try_get("actor").map_err(db_error)?,
        action: row.try_get("action").map_err(db_error)?,
        resource_type: row.try_get("resource_type").map_err(db_error)?,
        resource_id: row.try_get("resource_id").map_err(db_error)?,
        at: row.try_get("at").map_err(db_error)?,
        details: row.try_get("details").map_err(db_error)?,
    })
}
fn row_doc_revision(row: &PgRow) -> Result<DocumentRevision, RepoError> {
    Ok(DocumentRevision {
        id: row.try_get("id").map_err(db_error)?,
        document_id: row.try_get("document_id").map_err(db_error)?,
        project_id: row.try_get("project_id").map_err(db_error)?,
        body: row.try_get("body").map_err(db_error)?,
        revision: row
            .try_get::<i64, _>("revision")
            .map_err(db_error)?
            .try_into()
            .map_err(|_| RepoError::Failure("invalid document revision".into()))?,
        author: row.try_get("author").map_err(db_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
    })
}
fn row_share_link(row: &PgRow) -> Result<ShareLink, RepoError> {
    Ok(ShareLink {
        token: String::new(),
        token_hash: row.try_get("token_hash").map_err(db_error)?,
        project_id: row.try_get("project_id").map_err(db_error)?,
        role: row.try_get("role").map_err(db_error)?,
        created_by: row.try_get("created_by").map_err(db_error)?,
        created_at: row.try_get("created_at").map_err(db_error)?,
        expires_at: row.try_get("expires_at").map_err(db_error)?,
        label: row.try_get("label").map_err(db_error)?,
    })
}

#[derive(Clone)]
pub struct PostgresRepository {
    pub pool: PgPool,
}
impl PostgresRepository {
    pub async fn connect(database_url: &str) -> Result<Self, RepoError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(db_error)?;
        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .map_err(|error| RepoError::Failure(error.to_string()))?;
        Ok(Self { pool })
    }
    pub async fn from_env() -> Result<Self, RepoError> {
        Self::connect(
            &std::env::var("DATABASE_URL")
                .map_err(|_| RepoError::Failure("DATABASE_URL is required".into()))?,
        )
        .await
    }
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn create_project(
        &self,
        v: Project,
        audit: Option<AuditEvent>,
    ) -> Result<Project, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO projects (id,name,created_at,updated_at) VALUES ($1,$2,$3,$4)")
            .bind(v.id)
            .bind(&v.name)
            .bind(v.created_at)
            .bind(v.updated_at)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn get_project(&self, id: Uuid) -> Result<Project, RepoError> {
        required(
            sqlx::query("SELECT id, name, created_at, updated_at FROM projects WHERE id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?
                .as_ref()
                .map(row_project)
                .transpose()?,
        )
    }
    async fn list_projects(&self) -> Result<Vec<Project>, RepoError> {
        sqlx::query("SELECT id, name, created_at, updated_at FROM projects ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?
            .iter()
            .map(row_project)
            .collect()
    }
    async fn create_membership(
        &self,
        v: ProjectMembership,
    ) -> Result<ProjectMembership, RepoError> {
        sqlx::query("INSERT INTO project_memberships (project_id,subject,role,created_at) VALUES ($1,$2,$3,$4)").bind(v.project_id).bind(&v.subject).bind(role_name(&v.role)).bind(v.created_at).execute(&self.pool).await.map_err(db_error)?;
        Ok(v)
    }
    async fn get_membership(
        &self,
        project_id: Uuid,
        subject: &str,
    ) -> Result<ProjectMembership, RepoError> {
        let r = sqlx::query("SELECT project_id, subject, role, created_at FROM project_memberships WHERE project_id=$1 AND subject=$2")
            .bind(project_id)
            .bind(subject)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?;
        let r = required(r)?;
        Ok(ProjectMembership {
            project_id: r.try_get("project_id").map_err(db_error)?,
            subject: r.try_get("subject").map_err(db_error)?,
            role: role(r.try_get::<String, _>("role").map_err(db_error)?.as_str())?,
            created_at: r.try_get("created_at").map_err(db_error)?,
        })
    }
    async fn list_memberships(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, RepoError> {
        let rows =
            sqlx::query("SELECT project_id, subject, role, created_at FROM project_memberships WHERE project_id=$1 ORDER BY subject")
                .bind(project_id)
                .fetch_all(&self.pool)
                .await
                .map_err(db_error)?;
        rows.iter()
            .map(|r| {
                Ok(ProjectMembership {
                    project_id: r.try_get("project_id").map_err(db_error)?,
                    subject: r.try_get("subject").map_err(db_error)?,
                    role: role(r.try_get::<String, _>("role").map_err(db_error)?.as_str())?,
                    created_at: r.try_get("created_at").map_err(db_error)?,
                })
            })
            .collect()
    }
    async fn update_project(
        &self,
        v: Project,
        audit: Option<AuditEvent>,
    ) -> Result<Project, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("UPDATE projects SET name=$2,updated_at=$3 WHERE id=$1")
            .bind(v.id)
            .bind(&v.name)
            .bind(v.updated_at)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn delete_project(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("DELETE FROM projects WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn create_document(
        &self,
        v: Document,
        audit: Option<AuditEvent>,
    ) -> Result<Document, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO documents (id,project_id,path,title,body,data,revision,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(v.id).bind(v.project_id).bind(&v.path).bind(&v.title).bind(&v.body).bind(serde_json::to_value(&v.data).map_err(json_error)?).bind(i64::try_from(v.revision).map_err(|_|RepoError::Failure("revision overflow".into()))?).bind(v.updated_at).execute(&mut *tx).await.map_err(db_error)?;
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn get_document_by_id(&self, document_id: Uuid) -> Result<Document, RepoError> {
        required(
            sqlx::query("SELECT id, project_id, path, title, body, data, revision, updated_at FROM documents WHERE id=$1")
                .bind(document_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?
                .as_ref()
                .map(row_document)
                .transpose()?,
        )
    }
    async fn list_documents(&self, project_id: Uuid) -> Result<Vec<Document>, RepoError> {
        sqlx::query("SELECT id, project_id, path, title, body, data, revision, updated_at FROM documents WHERE project_id=$1 ORDER BY path")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?
            .iter()
            .map(row_document)
            .collect()
    }
    async fn update_document(
        &self,
        v: Document,
        expected_revision: u64,
        audit: Option<AuditEvent>,
    ) -> Result<Document, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("UPDATE documents SET project_id=$2,path=$3,title=$4,body=$5,data=$6,revision=$7,updated_at=$8 WHERE id=$1 AND revision=$9").bind(v.id).bind(v.project_id).bind(&v.path).bind(&v.title).bind(&v.body).bind(serde_json::to_value(&v.data).map_err(json_error)?).bind(i64::try_from(v.revision).map_err(|_|RepoError::Failure("revision overflow".into()))?).bind(v.updated_at).bind(i64::try_from(expected_revision).map_err(|_|RepoError::Failure("revision overflow".into()))?).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if n == 0 {
            let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM documents WHERE id=$1")
                .bind(v.id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
            return Err(match exists {
                Some(_) => RepoError::Conflict(format!(
                    "document revision conflict: expected {expected_revision}"
                )),
                None => RepoError::NotFound,
            });
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn delete_document(
        &self,
        document_id: Uuid,
        audit: Option<AuditEvent>,
    ) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("DELETE FROM documents WHERE id=$1")
            .bind(document_id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn create_reference(
        &self,
        v: ReferenceEntry,
        audit: Option<AuditEvent>,
    ) -> Result<ReferenceEntry, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO reference_entries (id,project_id,metadata,provenance,created_at,updated_at) VALUES ($1,$2,$3,$4,$5,$6)").bind(v.id).bind(v.project_id).bind(serde_json::to_value(&v.metadata).map_err(json_error)?).bind(v.provenance.as_ref().map(|x|serde_json::to_value(x)).transpose().map_err(json_error)?).bind(v.created_at).bind(v.updated_at).execute(&mut *tx).await.map_err(db_error)?;
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn get_reference(&self, id: Uuid) -> Result<ReferenceEntry, RepoError> {
        required(
            sqlx::query("SELECT id, project_id, metadata, provenance, created_at, updated_at FROM reference_entries WHERE id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?
                .as_ref()
                .map(row_reference)
                .transpose()?,
        )
    }
    async fn list_references(&self, p: Uuid) -> Result<Vec<ReferenceEntry>, RepoError> {
        sqlx::query("SELECT id, project_id, metadata, provenance, created_at, updated_at FROM reference_entries WHERE project_id=$1 ORDER BY id")
            .bind(p)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?
            .iter()
            .map(row_reference)
            .collect()
    }
    async fn update_reference(
        &self,
        v: ReferenceEntry,
        audit: Option<AuditEvent>,
    ) -> Result<ReferenceEntry, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("UPDATE reference_entries SET project_id=$2,metadata=$3,provenance=$4,updated_at=$5 WHERE id=$1").bind(v.id).bind(v.project_id).bind(serde_json::to_value(&v.metadata).map_err(json_error)?).bind(v.provenance.as_ref().map(|x|serde_json::to_value(x)).transpose().map_err(json_error)?).bind(v.updated_at).execute(&mut *tx).await.map_err(db_error)?.rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn delete_reference(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("DELETE FROM reference_entries WHERE id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn get_fulltext(&self, id: Uuid) -> Result<FulltextMetadata, RepoError> {
        required(
            sqlx::query("SELECT reference_id, blob_ref, filename, content_type, size_bytes, checksum_sha256, uploaded_at FROM fulltexts WHERE reference_id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?
                .as_ref()
                .map(row_fulltext)
                .transpose()?,
        )
    }
    async fn list_fulltexts(&self, p: Uuid) -> Result<Vec<FulltextMetadata>, RepoError> {
        sqlx::query(
            "SELECT f.reference_id, f.blob_ref, f.filename, f.content_type, f.size_bytes, f.checksum_sha256, f.uploaded_at FROM fulltexts f \
             JOIN reference_entries r ON r.id = f.reference_id \
             WHERE r.project_id = $1 ORDER BY f.reference_id",
        )
        .bind(p)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?
        .iter()
        .map(row_fulltext)
        .collect()
    }
    async fn put_fulltext(
        &self,
        v: FulltextMetadata,
        audit: Option<AuditEvent>,
    ) -> Result<FulltextMetadata, RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("INSERT INTO fulltexts (reference_id,blob_ref,filename,content_type,size_bytes,checksum_sha256,uploaded_at) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (reference_id) DO UPDATE SET blob_ref=EXCLUDED.blob_ref,filename=EXCLUDED.filename,content_type=EXCLUDED.content_type,size_bytes=EXCLUDED.size_bytes,checksum_sha256=EXCLUDED.checksum_sha256,uploaded_at=EXCLUDED.uploaded_at").bind(v.reference_id).bind(&v.blob_ref).bind(&v.filename).bind(&v.content_type).bind(i64::try_from(v.size_bytes).map_err(|_|RepoError::Failure("blob size overflow".into()))?).bind(&v.checksum_sha256).bind(v.uploaded_at).execute(&mut *tx).await.map_err(db_error)?;
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(v)
    }
    async fn delete_fulltext(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let n = sqlx::query("DELETE FROM fulltexts WHERE reference_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = &audit {
            sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(event.id).bind(event.project_id).bind(&event.actor).bind(&event.action).bind(&event.resource_type).bind(event.resource_id).bind(event.at).bind(&event.details).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(())
    }
    async fn append_audit(&self, v: AuditEvent) -> Result<AuditEvent, RepoError> {
        sqlx::query("INSERT INTO audit_events (id,project_id,actor,action,resource_type,resource_id,at,details) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(v.id).bind(v.project_id).bind(&v.actor).bind(&v.action).bind(&v.resource_type).bind(v.resource_id).bind(v.at).bind(&v.details).execute(&self.pool).await.map_err(db_error)?;
        Ok(v)
    }
    async fn list_audit(&self, p: Uuid) -> Result<Vec<AuditEvent>, RepoError> {
        sqlx::query("SELECT id, project_id, actor, action, resource_type, resource_id, at, details FROM audit_events WHERE project_id=$1 ORDER BY at,id")
            .bind(p)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?
            .iter()
            .map(row_audit)
            .collect()
    }
    async fn save_document_revision(
        &self,
        document_id: Uuid,
        project_id: Uuid,
        body: String,
        revision: u64,
        author: Option<String>,
    ) -> Result<DocumentRevision, RepoError> {
        let id = Uuid::new_v4();
        let created_at = Utc::now();
        sqlx::query(
            "INSERT INTO document_revisions (id,document_id,project_id,body,revision,author,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(id)
        .bind(document_id)
        .bind(project_id)
        .bind(&body)
        .bind(i64::try_from(revision).map_err(|_| RepoError::Failure("revision overflow".into()))?)
        .bind(&author)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(DocumentRevision {
            id,
            document_id,
            project_id,
            body,
            revision,
            author,
            created_at,
        })
    }
    async fn list_document_revisions(
        &self,
        document_id: Uuid,
    ) -> Result<Vec<DocumentRevision>, RepoError> {
        sqlx::query(
            "SELECT id, document_id, project_id, body, revision, author, created_at FROM document_revisions WHERE document_id=$1 ORDER BY created_at DESC,id",
        )
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?
        .iter()
        .map(row_doc_revision)
        .collect()
    }
    async fn get_document_revision(&self, id: Uuid) -> Result<DocumentRevision, RepoError> {
        sqlx::query("SELECT id, document_id, project_id, body, revision, author, created_at FROM document_revisions WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(RepoError::NotFound)
            .and_then(|row| row_doc_revision(&row))
    }
    async fn create_share_link(
        &self,
        project_id: Uuid,
        role: &str,
        created_by: &str,
        label: Option<String>,
    ) -> Result<ShareLink, RepoError> {
        let token = format!("nsl_{}", Uuid::new_v4().simple());
        let token_hash = crate::hash_token(&token);
        let created_at = Utc::now();
        sqlx::query(
            "INSERT INTO share_links (token_hash,project_id,role,created_by,created_at,expires_at,label) VALUES ($1,$2,$3,$4,$5,NULL,$6)",
        )
        .bind(&token_hash)
        .bind(project_id)
        .bind(role)
        .bind(created_by)
        .bind(created_at)
        .bind(&label)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(ShareLink {
            token,
            token_hash,
            project_id,
            role: role.to_string(),
            created_by: created_by.to_string(),
            created_at,
            expires_at: None,
            label,
        })
    }
    async fn list_share_links(&self, project_id: Uuid) -> Result<Vec<ShareLink>, RepoError> {
        sqlx::query("SELECT token_hash, project_id, role, created_by, created_at, expires_at, label FROM share_links WHERE project_id=$1 ORDER BY created_at")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?
            .iter()
            .map(row_share_link)
            .collect()
    }
    async fn delete_share_link(&self, token: &str) -> Result<(), RepoError> {
        let h = crate::hash_token(token);
        let n = sqlx::query("DELETE FROM share_links WHERE token_hash=$1")
            .bind(&h)
            .execute(&self.pool)
            .await
            .map_err(db_error)?
            .rows_affected();
        if n == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
    async fn resolve_share_link(&self, token: &str) -> Result<ShareLink, RepoError> {
        let h = crate::hash_token(token);
        sqlx::query("SELECT token_hash, project_id, role, created_by, created_at, expires_at, label FROM share_links WHERE token_hash=$1")
            .bind(&h)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?
            .ok_or(RepoError::NotFound)
            .and_then(|row| row_share_link(&row))
    }
}

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn get(&self, reference_id: Uuid) -> Result<Vec<u8>, RepoError>;
    async fn put(&self, reference_id: Uuid, bytes: Vec<u8>) -> Result<(), RepoError>;
    async fn delete(&self, reference_id: Uuid) -> Result<(), RepoError>;
}

#[derive(Clone)]
pub struct S3BlobStore {
    client: S3Client,
    bucket: String,
}
impl S3BlobStore {
    pub async fn from_env() -> Result<Self, RepoError> {
        let endpoint = std::env::var("NISABA_S3_ENDPOINT")
            .map_err(|_| RepoError::Failure("NISABA_S3_ENDPOINT is required".into()))?;
        let access = std::env::var("NISABA_S3_ACCESS_KEY")
            .map_err(|_| RepoError::Failure("NISABA_S3_ACCESS_KEY is required".into()))?;
        let secret = std::env::var("NISABA_S3_SECRET_KEY")
            .map_err(|_| RepoError::Failure("NISABA_S3_SECRET_KEY is required".into()))?;
        let region = std::env::var("NISABA_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
        let creds =
            aws_credential_types::Credentials::new(access, secret, None, None, "nisaba-app");
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region))
            .endpoint_url(endpoint)
            .credentials_provider(creds)
            .load()
            .await;
        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(true)
            .build();
        let bucket = std::env::var("NISABA_S3_BUCKET_BLOBS")
            .map_err(|_| RepoError::Failure("NISABA_S3_BUCKET_BLOBS is required".into()))?;
        if bucket.trim().is_empty() {
            return Err(RepoError::Failure(
                "NISABA_S3_BUCKET_BLOBS must not be empty".into(),
            ));
        }
        Ok(Self {
            client: S3Client::from_conf(s3_config),
            bucket,
        })
    }
    fn key(id: Uuid) -> String {
        fulltext_blob_ref(id)
    }
}
#[async_trait]
impl BlobStore for S3BlobStore {
    async fn get(&self, id: Uuid) -> Result<Vec<u8>, RepoError> {
        self.client
            .get_object()
            .bucket(&self.bucket)
            .key(Self::key(id))
            .send()
            .await
            .map_err(|e| RepoError::Failure(e.to_string()))?
            .body
            .collect()
            .await
            .map(|x| x.into_bytes().to_vec())
            .map_err(|e| RepoError::Failure(e.to_string()))
    }
    async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<(), RepoError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(Self::key(id))
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map_err(|e| RepoError::Failure(e.to_string()))
            .map(|_| ())
    }
    async fn delete(&self, id: Uuid) -> Result<(), RepoError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(Self::key(id))
            .send()
            .await
            .map_err(|e| RepoError::Failure(e.to_string()))
            .map(|_| ())
    }
}

#[derive(Default, Clone)]
pub struct MemoryBlobStore {
    bytes: Arc<RwLock<HashMap<Uuid, Vec<u8>>>>,
}
#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn get(&self, id: Uuid) -> Result<Vec<u8>, RepoError> {
        self.bytes
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn put(&self, id: Uuid, bytes: Vec<u8>) -> Result<(), RepoError> {
        self.bytes.write().await.insert(id, bytes);
        Ok(())
    }
    async fn delete(&self, id: Uuid) -> Result<(), RepoError> {
        self.bytes.write().await.remove(&id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_contains_project_document_and_reference_tables() {
        let sql = include_str!("../../../migrations/0001_initial.sql");
        for table in [
            "projects",
            "project_memberships",
            "documents",
            "reference_entries",
            "fulltexts",
            "audit_events",
        ] {
            assert!(
                sql.contains(&format!("CREATE TABLE {table}")),
                "missing {table}"
            );
        }
        assert!(sql.contains("PRIMARY KEY (project_id, subject)"));
        assert!(sql.contains("audit_events_project_at_idx"));
    }

    #[tokio::test]
    async fn postgres_round_trip_when_database_is_configured() {
        let Some(url) = std::env::var_os("TEST_DATABASE_URL") else {
            return;
        };
        let repo = PostgresRepository::connect(&url.to_string_lossy())
            .await
            .expect("TEST_DATABASE_URL must point at a migrated PostgreSQL database");
        let now = Utc::now();
        let project = repo
            .create_project(
                Project {
                    id: Uuid::new_v4(),
                    name: format!("integration-{}", Uuid::new_v4()),
                    created_at: now,
                    updated_at: now,
                },
                None,
            )
            .await
            .expect("project insert");
        repo.create_membership(ProjectMembership {
            project_id: project.id,
            subject: "integration-subject".into(),
            role: MembershipRole::Owner,
            created_at: now,
        })
        .await
        .expect("membership insert");
        let doc = repo
            .create_document(
                Document {
                    id: Uuid::new_v4(),
                    project_id: project.id,
                    path: "main.typ".into(),
                    title: "Main".into(),
                    body: "Hello".into(),
                    data: BTreeMap::new(),
                    revision: 0,
                    updated_at: now,
                },
                None,
            )
            .await
            .expect("document insert");
        assert_eq!(
            repo.get_document_by_id(doc.id)
                .await
                .expect("document read")
                .id,
            doc.id
        );
        // Duplicate path should conflict.
        assert!(matches!(
            repo.create_document(
                Document {
                    id: Uuid::new_v4(),
                    path: "main.typ".into(),
                    ..doc
                },
                None,
            )
            .await,
            Err(RepoError::Conflict(_))
        ));
    }
}
