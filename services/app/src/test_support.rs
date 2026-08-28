#![cfg(test)]

use crate::Repository;
use crate::types::{
    AuditEvent, Document, DocumentRevision, FulltextMetadata, Project, ProjectMembership,
    ReferenceEntry, RepoError, ShareLink,
};
use crate::{hash_token, share_link_deletion_hash};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Default)]
struct MemoryData {
    projects: HashMap<Uuid, Project>,
    documents: HashMap<Uuid, Document>,
    references: HashMap<Uuid, ReferenceEntry>,
    fulltexts: HashMap<Uuid, FulltextMetadata>,
    audit: Vec<AuditEvent>,
    memberships: HashMap<(Uuid, String), ProjectMembership>,
    doc_revisions: Vec<DocumentRevision>,
    share_links: Vec<ShareLink>,
}

/// Deterministic in-memory repository used by tests.
#[derive(Default)]
pub(crate) struct MemoryRepository {
    data: tokio::sync::RwLock<MemoryData>,
}

impl MemoryRepository {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Repository for MemoryRepository {
    async fn create_project(
        &self,
        value: Project,
        audit: Option<AuditEvent>,
    ) -> Result<Project, RepoError> {
        let mut d = self.data.write().await;
        if d.projects.values().any(|p| p.name == value.name) {
            return Err(RepoError::Conflict("project name already exists".into()));
        }
        d.projects.insert(value.id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn get_project(&self, id: Uuid) -> Result<Project, RepoError> {
        self.data
            .read()
            .await
            .projects
            .get(&id)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn list_projects(&self) -> Result<Vec<Project>, RepoError> {
        Ok(self.data.read().await.projects.values().cloned().collect())
    }
    async fn create_membership(
        &self,
        value: ProjectMembership,
    ) -> Result<ProjectMembership, RepoError> {
        let mut data = self.data.write().await;
        if !data.projects.contains_key(&value.project_id) {
            return Err(RepoError::NotFound);
        }
        let key = (value.project_id, value.subject.clone());
        if data.memberships.contains_key(&key) {
            return Err(RepoError::Conflict(
                "project membership already exists".into(),
            ));
        }
        data.memberships.insert(key, value.clone());
        Ok(value)
    }
    async fn upsert_membership(
        &self,
        value: ProjectMembership,
    ) -> Result<ProjectMembership, RepoError> {
        let mut data = self.data.write().await;
        if !data.projects.contains_key(&value.project_id) {
            return Err(RepoError::NotFound);
        }
        data.memberships
            .insert((value.project_id, value.subject.clone()), value.clone());
        Ok(value)
    }
    async fn delete_membership(&self, project_id: Uuid, subject: &str) -> Result<(), RepoError> {
        let mut data = self.data.write().await;
        if data
            .memberships
            .remove(&(project_id, subject.to_owned()))
            .is_none()
        {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
    async fn get_membership(
        &self,
        project_id: Uuid,
        subject: &str,
    ) -> Result<ProjectMembership, RepoError> {
        self.data
            .read()
            .await
            .memberships
            .get(&(project_id, subject.to_owned()))
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn list_memberships(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, RepoError> {
        Ok(self
            .data
            .read()
            .await
            .memberships
            .values()
            .filter(|membership| membership.project_id == project_id)
            .cloned()
            .collect())
    }
    async fn list_memberships_for_subjects(
        &self,
        subjects: &[&str],
    ) -> Result<Vec<ProjectMembership>, RepoError> {
        Ok(self
            .data
            .read()
            .await
            .memberships
            .values()
            .filter(|membership| subjects.contains(&membership.subject.as_str()))
            .cloned()
            .collect())
    }
    async fn update_project(
        &self,
        value: Project,
        audit: Option<AuditEvent>,
    ) -> Result<Project, RepoError> {
        let mut d = self.data.write().await;
        if !d.projects.contains_key(&value.id) {
            return Err(RepoError::NotFound);
        }
        d.projects.insert(value.id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn delete_project(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError> {
        let mut d = self.data.write().await;
        if !d.projects.contains_key(&id) {
            return Err(RepoError::NotFound);
        }
        // Cascade-delete children.
        let doc_ids: HashSet<Uuid> = d
            .documents
            .values()
            .filter(|doc| doc.project_id == id)
            .map(|doc| doc.id)
            .collect();
        let ref_ids: HashSet<Uuid> = d
            .references
            .values()
            .filter(|r| r.project_id == id)
            .map(|r| r.id)
            .collect();
        d.documents.retain(|_, doc| doc.project_id != id);
        d.fulltexts.retain(|rid, _| !ref_ids.contains(rid));
        d.references.retain(|_, r| r.project_id != id);
        d.memberships.retain(|(project_id, _), _| *project_id != id);
        d.audit.retain(|e| e.project_id != id);
        d.doc_revisions
            .retain(|r| !doc_ids.contains(&r.document_id));
        d.projects.remove(&id);
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(())
    }
    async fn create_document(
        &self,
        value: Document,
        audit: Option<AuditEvent>,
    ) -> Result<Document, RepoError> {
        let mut d = self.data.write().await;
        if !d.projects.contains_key(&value.project_id) {
            return Err(RepoError::NotFound);
        }
        if d.documents
            .values()
            .any(|x| x.project_id == value.project_id && x.path == value.path)
        {
            return Err(RepoError::Conflict("document path already exists".into()));
        }
        d.documents.insert(value.id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn get_document_by_id(&self, document_id: Uuid) -> Result<Document, RepoError> {
        self.data
            .read()
            .await
            .documents
            .get(&document_id)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn list_documents(&self, project_id: Uuid) -> Result<Vec<Document>, RepoError> {
        let mut docs: Vec<Document> = self
            .data
            .read()
            .await
            .documents
            .values()
            .filter(|x| x.project_id == project_id)
            .cloned()
            .collect();
        docs.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(docs)
    }
    async fn update_document(
        &self,
        value: Document,
        expected_revision: u64,
        audit: Option<AuditEvent>,
    ) -> Result<Document, RepoError> {
        let mut d = self.data.write().await;
        let stored = d
            .documents
            .get(&value.id)
            .ok_or(RepoError::NotFound)?
            .clone();
        if stored.revision != expected_revision {
            return Err(RepoError::Conflict(format!(
                "document revision is {}, expected {expected_revision}",
                stored.revision
            )));
        }
        d.documents.insert(value.id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn delete_document(
        &self,
        document_id: Uuid,
        audit: Option<AuditEvent>,
    ) -> Result<(), RepoError> {
        let mut d = self.data.write().await;
        if d.documents.remove(&document_id).is_none() {
            return Err(RepoError::NotFound);
        }
        d.doc_revisions.retain(|r| r.document_id != document_id);
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(())
    }
    async fn create_reference(
        &self,
        value: ReferenceEntry,
        audit: Option<AuditEvent>,
    ) -> Result<ReferenceEntry, RepoError> {
        let mut d = self.data.write().await;
        if !d.projects.contains_key(&value.project_id) {
            return Err(RepoError::NotFound);
        }
        d.references.insert(value.id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn get_reference(&self, id: Uuid) -> Result<ReferenceEntry, RepoError> {
        self.data
            .read()
            .await
            .references
            .get(&id)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn list_references(&self, p: Uuid) -> Result<Vec<ReferenceEntry>, RepoError> {
        Ok(self
            .data
            .read()
            .await
            .references
            .values()
            .filter(|x| x.project_id == p)
            .cloned()
            .collect())
    }
    async fn update_reference(
        &self,
        value: ReferenceEntry,
        audit: Option<AuditEvent>,
    ) -> Result<ReferenceEntry, RepoError> {
        let mut d = self.data.write().await;
        if !d.references.contains_key(&value.id) {
            return Err(RepoError::NotFound);
        }
        d.references.insert(value.id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn delete_reference(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError> {
        let mut d = self.data.write().await;
        if !d.references.contains_key(&id) {
            return Err(RepoError::NotFound);
        }
        d.references.remove(&id);
        d.fulltexts.remove(&id);
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(())
    }
    async fn get_fulltext(&self, id: Uuid) -> Result<FulltextMetadata, RepoError> {
        self.data
            .read()
            .await
            .fulltexts
            .get(&id)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn list_fulltexts(&self, p: Uuid) -> Result<Vec<FulltextMetadata>, RepoError> {
        let d = self.data.read().await;
        let reference_ids: HashSet<Uuid> = d
            .references
            .values()
            .filter(|x| x.project_id == p)
            .map(|x| x.id)
            .collect();
        Ok(d.fulltexts
            .values()
            .filter(|x| reference_ids.contains(&x.reference_id))
            .cloned()
            .collect())
    }
    async fn put_fulltext(
        &self,
        value: FulltextMetadata,
        audit: Option<AuditEvent>,
    ) -> Result<FulltextMetadata, RepoError> {
        let mut d = self.data.write().await;
        if !d.references.contains_key(&value.reference_id) {
            return Err(RepoError::NotFound);
        }
        d.fulltexts.insert(value.reference_id, value.clone());
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(value)
    }
    async fn delete_fulltext(&self, id: Uuid, audit: Option<AuditEvent>) -> Result<(), RepoError> {
        let mut d = self.data.write().await;
        if d.fulltexts.remove(&id).is_none() {
            return Err(RepoError::NotFound);
        }
        if let Some(event) = audit {
            d.audit.push(event);
        }
        Ok(())
    }
    async fn append_audit(&self, value: AuditEvent) -> Result<AuditEvent, RepoError> {
        self.data.write().await.audit.push(value.clone());
        Ok(value)
    }
    async fn list_audit(&self, p: Uuid) -> Result<Vec<AuditEvent>, RepoError> {
        Ok(self
            .data
            .read()
            .await
            .audit
            .iter()
            .filter(|x| x.project_id == p)
            .cloned()
            .collect())
    }
    async fn save_document_revision(
        &self,
        document_id: Uuid,
        project_id: Uuid,
        body: String,
        revision: u64,
        author: Option<String>,
    ) -> Result<DocumentRevision, RepoError> {
        let entry = DocumentRevision {
            id: Uuid::new_v4(),
            document_id,
            project_id,
            body,
            revision,
            author,
            created_at: Utc::now(),
        };
        self.data.write().await.doc_revisions.push(entry.clone());
        Ok(entry)
    }
    async fn list_document_revisions(
        &self,
        document_id: Uuid,
    ) -> Result<Vec<DocumentRevision>, RepoError> {
        Ok(self
            .data
            .read()
            .await
            .doc_revisions
            .iter()
            .filter(|r| r.document_id == document_id)
            .rev()
            .cloned()
            .collect())
    }
    async fn get_document_revision(&self, id: Uuid) -> Result<DocumentRevision, RepoError> {
        self.data
            .read()
            .await
            .doc_revisions
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
    async fn create_share_link(
        &self,
        project_id: Uuid,
        role: &str,
        created_by: &str,
        label: Option<String>,
    ) -> Result<ShareLink, RepoError> {
        let token = format!("nsl_{}", Uuid::new_v4().simple());
        let link = ShareLink {
            token_hash: hash_token(&token),
            token,
            project_id,
            role: role.to_string(),
            created_by: created_by.to_string(),
            created_at: Utc::now(),
            expires_at: None,
            label,
        };
        self.data.write().await.share_links.push(link.clone());
        Ok(link)
    }
    async fn list_share_links(&self, project_id: Uuid) -> Result<Vec<ShareLink>, RepoError> {
        Ok(self
            .data
            .read()
            .await
            .share_links
            .iter()
            .filter(|l| l.project_id == project_id)
            .cloned()
            .map(|mut link| {
                link.token.clear();
                link
            })
            .collect())
    }
    async fn delete_share_link(&self, token: &str) -> Result<(), RepoError> {
        let mut d = self.data.write().await;
        let h = share_link_deletion_hash(token);
        let before = d.share_links.len();
        d.share_links.retain(|l| l.token_hash != h);
        if d.share_links.len() == before {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
    async fn resolve_share_link(&self, token: &str) -> Result<ShareLink, RepoError> {
        let h = hash_token(token);
        self.data
            .read()
            .await
            .share_links
            .iter()
            .find(|l| l.token_hash == h)
            .cloned()
            .ok_or(RepoError::NotFound)
    }
}
