//! S3-backed op-log and snapshot stores (feature `s3`).
//!
//! The durable authority for collaborative documents moves onto the
//! S3-compatible object store (`SeaweedFS` in the self-hosted stack) so the sync
//! service shares one storage story with the app service's blob store and is no
//! longer pinned to a local disk. The filesystem stores remain available
//! (`NISABA_SYNC_STORE_BACKEND=fs`) for bare-metal/dev runs.
//!
//! # Key layout
//!
//! One bucket (`NISABA_S3_BUCKET_OPLOG`) holds both stores, separated by a
//! top-level prefix. [`DocId`] is restricted to `[A-Za-z0-9._-]` (no `/`), so a
//! document id is always exactly one path segment and prefixes cannot collide
//! between documents:
//!
//! ```text
//! oplog/{doc_id}/{part}.part      one immutable object per appended update
//! snapshot/{doc_id}/{seq}.snap    one immutable object per persisted snapshot
//! ```
//!
//! `{part}`/`{seq}` are zero-padded to 12 decimal digits, so S3's lexicographic
//! listing order equals numeric order and a reader can replay parts by listing
//! alone, without any client-side numbering scheme beyond the key.
//!
//! # Why appends cannot read-modify-write
//!
//! S3 objects are immutable once written, and the store never mutates or
//! rewrites an existing part. Every append allocates the **next** part number
//! and `PutObject`s a fresh key exactly once:
//!
//! * the part counter is seeded from a listing of the document's prefix
//!   (`max(existing) + 1`, `0` for an unseen document), so it survives restarts;
//! * allocation, the PUT, and the counter increment happen while holding a
//!   per-document async mutex, so two appends for one document can never be
//!   handed the same part number;
//! * the counter is incremented **only after** the PUT succeeds — a failed or
//!   crashed PUT never created its object, so the next append reuses the same
//!   number and the object set remains the contiguous prefix `0..=n`.
//!
//! This is the single-writer protocol the sync service already runs under (one
//! sync process is the authority for a bucket; a room serialises updates
//! through its authority). Two sync processes pointed at one bucket would
//! race for the same part keys — that topology is not supported, exactly as
//! the filesystem store is not multi-writer-safe across hosts.
//!
//! # Durable ordering: replay never observes a gap
//!
//! Because PUTs are atomic (an S3 object exists in full or not at all) and part
//! numbers are only consumed on success, the parts present for a document are
//! always a contiguous prefix of the naturals. Readers list the prefix, verify
//! contiguity from 0, and replay **only the contiguous prefix**; if a gap is
//! ever observed (bucket tampering, or the unsupported split-brain case) the
//! reader logs a warning and truncates at the gap rather than silently
//! replaying across it — a gap would mean a lost update and a diverged
//! authority.
//!
//! # Latest snapshot without in-place mutation
//!
//! Snapshots are immutable, monotonically numbered objects; there is no index
//! object to update and no "latest" pointer to rewrite. "Latest" is resolved
//! by listing: the highest sequence number is the newest snapshot (sequence
//! order equals version-vector order under the single-writer protocol), and if
//! its body fails to decode the reader walks down the sequence until one does
//! — mirroring the filesystem store's corrupt-file tolerance.
//!
//! Snapshot bodies reuse the filesystem encoding
//! (`[u32 be vv_len][vv bytes][snapshot bytes]`), so the two stores read each
//! other's data.
//!
//! # Cost model (deferred engineering, like the FS store)
//!
//! Replay is one `GetObject` per part (the FS store reads one file); the log
//! is not truncated after snapshots, so parts accumulate exactly as the FS
//! log's records do. Compaction — truncating parts covered by a snapshot —
//! and parallel part fetches are deferred until growth is instrumented
//! (§8.1 "instrument, do not engineer"); the correctness contract above holds
//! for either.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

use crate::config::{DocId, MAX_UPDATE_BYTES};
use crate::error::{SyncError, SyncResult};
use crate::op_log::OpLogStore;
use crate::snapshot::{Snapshot, SnapshotStore, decode_snapshot_file, encode_snapshot_file};

/// Width of the zero-padded decimal part/sequence number in object keys. Wide
/// enough (999,999,999,999) that lexicographic key order is stable for any
/// realistic document, and fixed so that order is *guaranteed* to equal numeric
/// order in S3 listings.
const NUMBER_WIDTH: usize = 12;
/// S3 listings return at most 1000 keys per page; the same batch size is the
/// `DeleteObjects` limit, so one constant serves both.
const LIST_PAGE: i32 = 1000;

// ---- environment configuration (mirrors the app service's S3BlobStore) ----

/// Connection + bucket configuration resolved from the environment.
///
/// Variable names deliberately mirror the app service's `NISABA_S3_*` set so
/// one `SeaweedFS` endpoint/identity configuration serves both services; the
/// reserved `NISABA_S3_BUCKET_OPLOG` selects the bucket sync persists to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3EnvConfig {
    /// S3 endpoint as seen from the sync process (e.g. `http://seaweedfs:8333`).
    pub endpoint: String,
    /// Access key of the S3 identity (the shared `nisaba-app` identity).
    pub access_key: String,
    /// Its secret key.
    pub secret_key: String,
    /// Region label; `SeaweedFS` accepts any (`us-east-1` code default,
    /// `local` in compose).
    pub region: String,
    /// Bucket holding the `oplog/` and `snapshot/` prefixes.
    pub bucket: String,
}

impl S3EnvConfig {
    /// Resolve the configuration from the process environment.
    ///
    /// # Errors
    /// A [`SyncError::Storage`] naming the first missing variable — a sync
    /// configured for S3 must fail loudly at startup, not fall back to a
    /// different durability plane.
    pub fn from_env() -> SyncResult<Self> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Pure, injectable core of [`Self::from_env`] (deterministic tests never
    /// mutate the process-wide environment).
    pub fn from_vars<V>(var: V) -> SyncResult<Self>
    where
        V: Fn(&str) -> Option<String>,
    {
        let required = |name: &str| {
            var(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    SyncError::Storage(format!(
                        "{name} is required for the S3 sync stores (NISABA_SYNC_STORE_BACKEND=s3)"
                    ))
                })
        };
        Ok(Self {
            endpoint: required("NISABA_S3_ENDPOINT")?,
            access_key: required("NISABA_S3_ACCESS_KEY")?,
            secret_key: required("NISABA_S3_SECRET_KEY")?,
            region: var("NISABA_S3_REGION")
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "us-east-1".to_string()),
            bucket: required("NISABA_S3_BUCKET_OPLOG")?,
        })
    }
}

/// Build the S3 client exactly the way the app service's `S3BlobStore` does:
/// explicit credentials (no ambient chain), an explicit endpoint for the S3
/// gateway, and path-style addressing, which self-hosted gateways such as
/// `SeaweedFS` require.
async fn build_client(config: &S3EnvConfig) -> S3Client {
    let credentials = aws_credential_types::Credentials::new(
        config.access_key.clone(),
        config.secret_key.clone(),
        None,
        None,
        "nisaba-sync",
    );
    let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(config.region.clone()))
        .endpoint_url(config.endpoint.clone())
        .credentials_provider(credentials)
        .load()
        .await;
    let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
        .force_path_style(true)
        .build();
    S3Client::from_conf(s3_config)
}

/// The connected pair of S3 stores plus a handle for the readiness probe.
#[derive(Clone)]
pub struct S3Stores {
    // Read only by the (server-gated) `HeadBucket` readiness probe; without
    // the server feature no probe exists and the fields go unread.
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    client: S3Client,
    #[cfg_attr(not(feature = "server"), allow(dead_code))]
    bucket: String,
    /// The op-log store (also usable via [`OpLogStore`]).
    pub op_log: Arc<S3OpLogStore>,
    /// The snapshot store (also usable via [`SnapshotStore`]).
    pub snapshots: Arc<S3SnapshotStore>,
}

impl S3Stores {
    /// Resolve configuration from the environment and connect.
    ///
    /// # Errors
    /// Propagates [`S3EnvConfig::from_env`] errors (missing variables); the
    /// client construction itself is lazy, so no network round-trip happens
    /// here — reachability is the readiness probe's job.
    pub async fn from_env() -> SyncResult<Self> {
        Self::connect(S3EnvConfig::from_env()?).await
    }

    /// Connect using an explicit configuration.
    ///
    /// # Errors
    /// See [`Self::from_env`].
    pub async fn connect(config: S3EnvConfig) -> SyncResult<Self> {
        let client = build_client(&config).await;
        let bucket = config.bucket.clone();
        Ok(Self {
            op_log: Arc::new(S3OpLogStore::new(client.clone(), bucket.clone())),
            snapshots: Arc::new(S3SnapshotStore::new(client.clone(), bucket.clone())),
            client,
            bucket,
        })
    }
}

/// S3-backed [`OpLogStore`]: one immutable object per appended update.
///
/// See the [module documentation](self) for the key layout, the single-writer
/// append protocol, and the gap-free replay guarantee.
pub struct S3OpLogStore {
    client: S3Client,
    bucket: String,
    /// Serialises (allocate → PUT → increment) per document; see module docs.
    doc_locks: DocLocks,
    /// Next part number per document. Seeded from a listing on first use,
    /// bumped only after a successful PUT.
    next_part: Mutex<HashMap<String, u64>>,
}

impl S3OpLogStore {
    /// Build a store over an existing client + bucket.
    #[must_use]
    pub fn new(client: S3Client, bucket: String) -> Self {
        Self {
            client,
            bucket,
            doc_locks: DocLocks::default(),
            next_part: Mutex::new(HashMap::new()),
        }
    }

    /// The part numbers present for `doc`, ascending. Unknown keys under the
    /// prefix (foreign objects) are ignored.
    async fn part_numbers(&self, doc: &DocId) -> SyncResult<Vec<u64>> {
        let prefix = oplog_prefix(doc.as_str());
        let mut numbers: Vec<u64> = list_keys(&self.client, &self.bucket, &prefix)
            .await?
            .iter()
            .filter_map(|key| parse_key_number(&prefix, key, OPLOG_PART_EXT))
            .collect();
        numbers.sort_unstable();
        Ok(numbers)
    }

    /// The next part number to write, seeding the cache from a listing on
    /// first use. The caller must hold the document's lock.
    async fn next_part_for(&self, doc: &DocId) -> SyncResult<u64> {
        {
            let cache = self.lock_next_part()?;
            if let Some(next) = cache.get(doc.as_str()) {
                return Ok(*next);
            }
        }
        let next = next_number(&self.part_numbers(doc).await?);
        self.lock_next_part()?
            .insert(doc.as_str().to_string(), next);
        Ok(next)
    }

    fn lock_next_part(&self) -> SyncResult<std::sync::MutexGuard<'_, HashMap<String, u64>>> {
        self.next_part
            .lock()
            .map_err(|_| SyncError::Internal("op-log part cache lock poisoned".into()))
    }
}

#[async_trait::async_trait]
impl OpLogStore for S3OpLogStore {
    async fn append(&self, doc: &DocId, update: &[u8]) -> SyncResult<()> {
        // Hold the per-document lock across allocate → PUT → increment so the
        // set of existing parts is always a contiguous prefix (module docs).
        let doc_lock = self.doc_locks.lock_for(doc.as_str())?;
        let _guard = doc_lock.lock().await;
        let part = self.next_part_for(doc).await?;
        let key = oplog_part_key(doc.as_str(), part);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(update.to_vec()))
            .send()
            .await
            .map_err(|e| s3_error("put", &key, e))?;
        // Only now is the part number consumed; a failed PUT above leaves the
        // object absent and the counter untouched, so the next append reuses
        // this part number instead of punching a hole.
        self.lock_next_part()?
            .insert(doc.as_str().to_string(), part + 1);
        Ok(())
    }

    async fn read_all(&self, doc: &DocId) -> SyncResult<Vec<Vec<u8>>> {
        let prefix = oplog_prefix(doc.as_str());
        let mut entries: Vec<(u64, String)> = list_keys(&self.client, &self.bucket, &prefix)
            .await?
            .into_iter()
            .filter_map(|key| parse_key_number(&prefix, &key, OPLOG_PART_EXT).map(|n| (n, key)))
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        let contiguous =
            contiguous_prefix_len(&entries.iter().map(|(n, _)| *n).collect::<Vec<_>>());
        if contiguous < entries.len() {
            // Impossible under the write protocol; a gap means bucket
            // tampering or an unsupported second writer. Replay only the
            // contiguous prefix — never across a missing update.
            tracing::warn!(
                doc = doc.as_str(),
                present = entries.len(),
                contiguous,
                first_missing = contiguous,
                "op-log part gap detected; replaying the contiguous prefix only"
            );
        }
        let mut out = Vec::with_capacity(contiguous);
        for (_, key) in entries.iter().take(contiguous) {
            let response = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map_err(|e| s3_error("get", key, e))?;
            // Defensive bound: this store never writes a part larger than
            // MAX_UPDATE_BYTES, so an oversized object is foreign/corrupt —
            // stop replay at it rather than allocating gigabytes.
            if let Some(len) = response.content_length()
                && len > i64::try_from(MAX_UPDATE_BYTES).expect("fits i64")
            {
                tracing::warn!(
                    key,
                    len,
                    max = MAX_UPDATE_BYTES,
                    "oversized op-log part; stopping replay at it"
                );
                break;
            }
            let bytes = response
                .body
                .collect()
                .await
                .map_err(|e| s3_error("read", key, e))?
                .into_bytes()
                .to_vec();
            out.push(bytes);
        }
        Ok(out)
    }

    async fn len(&self, doc: &DocId) -> SyncResult<u64> {
        Ok(contiguous_prefix_len(&self.part_numbers(doc).await?) as u64)
    }

    fn close(&self, doc: &DocId) -> SyncResult<()> {
        // Release the cached counter + lock entry; the listing is the
        // authoritative part count, so the next append re-seeds correctly.
        self.next_part
            .lock()
            .map_err(|_| SyncError::Internal("op-log part cache lock poisoned".into()))?
            .remove(doc.as_str());
        self.doc_locks.remove(doc.as_str())
    }
}

/// S3-backed [`SnapshotStore`]: immutable, monotonically numbered snapshot
/// objects; "latest" resolved by listing (see module docs).
pub struct S3SnapshotStore {
    client: S3Client,
    bucket: String,
    /// Serialises (allocate → PUT → increment) per document.
    doc_locks: DocLocks,
    /// Next sequence number per document, seeded from a listing on first use
    /// and bumped only after a successful PUT.
    next_seq: Mutex<HashMap<String, u64>>,
}

impl S3SnapshotStore {
    /// Build a store over an existing client + bucket.
    #[must_use]
    pub fn new(client: S3Client, bucket: String) -> Self {
        Self {
            client,
            bucket,
            doc_locks: DocLocks::default(),
            next_seq: Mutex::new(HashMap::new()),
        }
    }

    /// The `(seq, key)` pairs present for `doc`, ascending by seq. Unknown
    /// keys under the prefix are ignored.
    async fn snapshot_entries(&self, doc: &DocId) -> SyncResult<Vec<(u64, String)>> {
        let prefix = snapshot_prefix(doc.as_str());
        let mut entries: Vec<(u64, String)> = list_keys(&self.client, &self.bucket, &prefix)
            .await?
            .into_iter()
            .filter_map(|key| parse_key_number(&prefix, &key, SNAPSHOT_EXT).map(|n| (n, key)))
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        Ok(entries)
    }

    async fn next_seq_for(&self, doc: &DocId) -> SyncResult<u64> {
        {
            let cache = self.lock_next_seq()?;
            if let Some(next) = cache.get(doc.as_str()) {
                return Ok(*next);
            }
        }
        let entries = self.snapshot_entries(doc).await?;
        let next = next_number(&entries.iter().map(|(n, _)| *n).collect::<Vec<_>>());
        self.lock_next_seq()?.insert(doc.as_str().to_string(), next);
        Ok(next)
    }

    fn lock_next_seq(&self) -> SyncResult<std::sync::MutexGuard<'_, HashMap<String, u64>>> {
        self.next_seq
            .lock()
            .map_err(|_| SyncError::Internal("snapshot seq cache lock poisoned".into()))
    }

    /// Fetch + decode one snapshot object. `Err` (not `None`) signals a
    /// transport failure; an undecodable body is surfaced by the callers as a
    /// skip-with-warning.
    async fn fetch(&self, key: &str) -> SyncResult<Snapshot> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| s3_error("get", key, e))?;
        let bytes = response
            .body
            .collect()
            .await
            .map_err(|e| s3_error("read", key, e))?
            .into_bytes()
            .to_vec();
        decode_snapshot_file(&bytes)
            .map_err(|e| SyncError::Storage(format!("s3 snapshot {key} is corrupt: {e}")))
    }
}

#[async_trait::async_trait]
impl SnapshotStore for S3SnapshotStore {
    async fn put(&self, doc: &DocId, snapshot: Snapshot) -> SyncResult<()> {
        // Hold the per-document lock across allocate → PUT → increment so two
        // puts can never land the same sequence number (module docs).
        let doc_lock = self.doc_locks.lock_for(doc.as_str())?;
        let _guard = doc_lock.lock().await;
        let seq = self.next_seq_for(doc).await?;
        let key = snapshot_key(doc.as_str(), seq);
        let payload = encode_snapshot_file(&snapshot);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(payload))
            .send()
            .await
            .map_err(|e| s3_error("put", &key, e))?;
        // Consumed only after a successful PUT; the immutable key is never
        // rewritten (a retry re-lists and takes the next free sequence).
        self.lock_next_seq()?
            .insert(doc.as_str().to_string(), seq + 1);
        Ok(())
    }

    async fn latest(&self, doc: &DocId) -> SyncResult<Option<Snapshot>> {
        // Highest sequence first; on a corrupt body walk down to the newest
        // snapshot that decodes — one bad object must not break recovery.
        let entries = self.snapshot_entries(doc).await?;
        for (seq, key) in entries.iter().rev() {
            match self.fetch(key).await {
                Ok(snapshot) => return Ok(Some(snapshot)),
                Err(error) => {
                    tracing::warn!(
                        doc = doc.as_str(),
                        seq,
                        key,
                        error = %error,
                        "skipping unreadable snapshot while resolving latest"
                    );
                }
            }
        }
        Ok(None)
    }

    async fn list(&self, doc: &DocId) -> SyncResult<Vec<Snapshot>> {
        let mut out = Vec::new();
        for (seq, key) in self.snapshot_entries(doc).await? {
            match self.fetch(&key).await {
                Ok(snapshot) => out.push(snapshot),
                Err(error) => {
                    tracing::warn!(
                        doc = doc.as_str(),
                        seq,
                        key,
                        error = %error,
                        "skipping unreadable snapshot in listing"
                    );
                }
            }
        }
        Ok(out)
    }

    async fn drop_all(&self, doc: &DocId) -> SyncResult<()> {
        // Delete every key under the document's prefix (tests / maintenance).
        // Batched DeleteObjects, LIST_PAGE keys per request (the S3 maximum).
        let prefix = snapshot_prefix(doc.as_str());
        let keys = list_keys(&self.client, &self.bucket, &prefix).await?;
        for chunk in keys.chunks(usize::try_from(LIST_PAGE).expect("fits usize")) {
            let objects: Vec<ObjectIdentifier> = chunk
                .iter()
                .map(|key| {
                    ObjectIdentifier::builder()
                        .key(key)
                        .build()
                        .map_err(|e| SyncError::Storage(format!("s3 delete build: {e}")))
                })
                .collect::<Result<_, _>>()?;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .build()
                .map_err(|e| SyncError::Storage(format!("s3 delete build: {e}")))?;
            self.client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
                .map_err(|e| s3_error("delete-objects", &prefix, e))?;
        }
        // The cached sequence is stale once the objects are gone; drop it so
        // the next put re-seeds from the (now empty) listing.
        self.next_seq
            .lock()
            .map_err(|_| SyncError::Internal("snapshot seq cache lock poisoned".into()))?
            .remove(doc.as_str());
        self.doc_locks.remove(doc.as_str())
    }
}

/// Per-document async mutexes so (allocate → PUT → increment) is atomic per
/// document without serialising unrelated documents.
#[derive(Default)]
struct DocLocks {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl DocLocks {
    fn lock_for(&self, doc: &str) -> SyncResult<Arc<tokio::sync::Mutex<()>>> {
        Ok(self.guard()?.entry(doc.to_string()).or_default().clone())
    }

    fn remove(&self, doc: &str) -> SyncResult<()> {
        self.guard()?.remove(doc);
        Ok(())
    }

    fn guard(
        &self,
    ) -> SyncResult<std::sync::MutexGuard<'_, HashMap<String, Arc<tokio::sync::Mutex<()>>>>> {
        self.locks
            .lock()
            .map_err(|_| SyncError::Internal("document lock map poisoned".into()))
    }
}

/// List every key under `prefix`, following continuation tokens. Keys come
/// back sorted + deduplicated: S3 lists lexicographically, but the explicit
/// sort makes the ordering assumption local and testable.
async fn list_keys(client: &S3Client, bucket: &str, prefix: &str) -> SyncResult<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(bucket)
            .prefix(prefix)
            .max_keys(LIST_PAGE);
        if let Some(token) = continuation.clone() {
            request = request.continuation_token(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| s3_error("list", prefix, e))?;
        for object in response.contents() {
            if let Some(key) = object.key() {
                keys.push(key.to_string());
            }
        }
        if !response.is_truncated().unwrap_or(false) {
            break;
        }
        // A truncated page without a token cannot be followed; the keys
        // gathered so far are still correct, just possibly short.
        if let Some(token) = response.next_continuation_token() {
            continuation = Some(token.to_string());
        } else {
            tracing::warn!(prefix, "truncated S3 listing without a continuation token");
            break;
        }
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

fn s3_error<E: std::fmt::Display>(operation: &str, target: &str, error: E) -> SyncError {
    SyncError::Storage(format!("s3 {operation} {target}: {error}"))
}

/// Readiness probe: `HeadBucket` against the configured bucket — the cheapest
/// round-trip that proves endpoint reachability, working credentials, and
/// bucket existence, i.e. everything sync needs before it can durably accept
/// an update.
#[cfg(feature = "server")]
#[async_trait::async_trait]
impl crate::server::StorageProbe for S3Stores {
    async fn probe(&self) -> Result<(), String> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| format!("HeadBucket {} failed: {error}", self.bucket))
    }
}

// ---- key layout (pure; unit-tested below) -----------------------------------

const OPLOG_PART_EXT: &str = "part";
const SNAPSHOT_EXT: &str = "snap";

/// Key prefix covering every op-log part of `doc`.
fn oplog_prefix(doc: &str) -> String {
    format!("oplog/{doc}/")
}

/// The immutable key of `doc`'s part number `part`.
fn oplog_part_key(doc: &str, part: u64) -> String {
    format!(
        "{}{part:0width$}.{OPLOG_PART_EXT}",
        oplog_prefix(doc),
        width = NUMBER_WIDTH
    )
}

/// Key prefix covering every snapshot of `doc`.
fn snapshot_prefix(doc: &str) -> String {
    format!("snapshot/{doc}/")
}

/// The immutable key of `doc`'s snapshot sequence `seq`.
fn snapshot_key(doc: &str, seq: u64) -> String {
    format!(
        "{}{seq:0width$}.{SNAPSHOT_EXT}",
        snapshot_prefix(doc),
        width = NUMBER_WIDTH
    )
}

/// Parse the zero-padded number out of a key of the form
/// `{prefix}{number:012}.{ext}`, rejecting anything that is not exactly that
/// shape (foreign objects, wrong width, non-digits).
fn parse_key_number(prefix: &str, key: &str, ext: &str) -> Option<u64> {
    let rest = key.strip_prefix(prefix)?;
    let digits = rest.strip_suffix(&format!(".{ext}"))?;
    if digits.len() != NUMBER_WIDTH || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `max + 1` over a sorted-ascending slice (`0` when empty): the next number
/// to allocate. Monotonic even across deletions/gaps, so numbers are never
/// reused and keys stay immutable.
fn next_number(sorted: &[u64]) -> u64 {
    sorted.last().map_or(0, |max| *max + 1)
}

/// Length of the leading run `0, 1, 2, …` of a sorted-ascending slice. Parts
/// beyond a gap are never replayed (module docs: durable ordering).
fn contiguous_prefix_len(sorted: &[u64]) -> usize {
    sorted
        .iter()
        .zip(0u64..)
        .take_while(|(n, expected)| **n == *expected)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- key layout -----------------------------------------------------------

    #[test]
    fn part_keys_are_zero_padded_and_parse_back() {
        assert_eq!(oplog_part_key("doc", 0), "oplog/doc/000000000000.part");
        assert_eq!(oplog_part_key("doc", 7), "oplog/doc/000000000007.part");
        assert_eq!(
            oplog_part_key("doc", 123_456_789_012),
            "oplog/doc/123456789012.part"
        );
        let prefix = oplog_prefix("doc");
        for n in [0u64, 1, 42, 999_999_999_999] {
            let key = oplog_part_key("doc", n);
            assert_eq!(parse_key_number(&prefix, &key, OPLOG_PART_EXT), Some(n));
        }
    }

    #[test]
    fn snapshot_keys_are_zero_padded_and_parse_back() {
        assert_eq!(snapshot_key("d", 0), "snapshot/d/000000000000.snap");
        assert_eq!(snapshot_key("d", 3), "snapshot/d/000000000003.snap");
        let prefix = snapshot_prefix("d");
        for n in [0u64, 1, 9_876_543_210] {
            let key = snapshot_key("d", n);
            assert_eq!(parse_key_number(&prefix, &key, SNAPSHOT_EXT), Some(n));
        }
    }

    #[test]
    fn lexicographic_key_order_equals_numeric_order() {
        // The property readers rely on (S3 lists lexicographically): across
        // digit-boundary values the sorted keys enumerate the numbers in order.
        let numbers: Vec<u64> = (0..1_100).map(|n| n * 97 % 1_100).collect();
        let mut keys: Vec<String> = numbers.iter().map(|n| oplog_part_key("doc", *n)).collect();
        keys.sort();
        let parsed: Vec<u64> = keys
            .iter()
            .map(|k| parse_key_number(&oplog_prefix("doc"), k, OPLOG_PART_EXT).unwrap())
            .collect();
        let mut expected = numbers.clone();
        expected.sort_unstable();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn document_prefixes_do_not_collide() {
        // doc ids cannot contain '/', so one document's prefix can never be a
        // prefix of another's key namespace.
        assert!(!oplog_part_key("doc-x", 0).starts_with(&oplog_prefix("doc")));
        assert!(!snapshot_key("doc", 0).starts_with(&oplog_prefix("doc")));
        assert_ne!(oplog_prefix("a"), oplog_prefix("a_b"));
    }

    #[test]
    fn parse_rejects_foreign_and_malformed_keys() {
        let prefix = oplog_prefix("doc");
        for bad in [
            "oplog/doc/7.part",               // not zero-padded to the width
            "oplog/doc/000000000007",         // wrong extension
            "oplog/doc/000000000007.bin",     // wrong extension
            "oplog/other/000000000007.part",  // different document
            "oplog/000000000007.part",        // no document segment
            "oplog/doc/x00000000007.part",    // non-digit
            "oplog/doc/-00000000007.part",    // sign, not a digit
            "snapshot/doc/000000000007.snap", // snapshot namespace
        ] {
            assert_eq!(
                parse_key_number(&prefix, bad, OPLOG_PART_EXT),
                None,
                "{bad:?}"
            );
        }
    }

    // ---- numbering invariants --------------------------------------------------

    #[test]
    fn next_number_is_max_plus_one_and_survives_gaps() {
        assert_eq!(next_number(&[]), 0);
        assert_eq!(next_number(&[0]), 1);
        assert_eq!(next_number(&[0, 1, 2]), 3);
        // Deleted/gapped history never reuses a number: keys stay immutable.
        assert_eq!(next_number(&[0, 1, 5]), 6);
    }

    #[test]
    fn contiguous_prefix_len_truncates_at_gaps() {
        assert_eq!(contiguous_prefix_len(&[]), 0);
        assert_eq!(contiguous_prefix_len(&[0, 1, 2]), 3);
        // A hole at 2 means only parts 0 and 1 are replayable.
        assert_eq!(contiguous_prefix_len(&[0, 1, 3, 4]), 2);
        assert_eq!(contiguous_prefix_len(&[1, 2]), 0);
        // A later run after a gap never re-extends the prefix.
        assert_eq!(contiguous_prefix_len(&[0, 2, 3, 4, 5]), 1);
    }

    #[test]
    fn replay_plan_replays_only_the_contiguous_prefix() {
        // Simulated listing: parts 0..=4 plus a foreign late part 9 (e.g. an
        // operator restored an old backup) — a reader must stop at 4.
        let numbers = [0u64, 1, 2, 3, 4, 9];
        let replayable = contiguous_prefix_len(&numbers);
        assert_eq!(replayable, 5);
        assert_eq!(numbers[..replayable], [0, 1, 2, 3, 4]);
    }

    // ---- environment configuration ---------------------------------------------

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: std::collections::HashMap<&str, &str> = pairs.iter().copied().collect();
        move |k| map.get(k).map(|v| (*v).to_string())
    }

    const FULL: &[(&str, &str)] = &[
        ("NISABA_S3_ENDPOINT", "http://seaweedfs:8333"),
        ("NISABA_S3_ACCESS_KEY", "nisaba-app"),
        ("NISABA_S3_SECRET_KEY", "secret"),
        ("NISABA_S3_REGION", "local"),
        ("NISABA_S3_BUCKET_OPLOG", "nisaba-oplog"),
    ];

    #[test]
    fn env_config_reads_all_variables() {
        let config = S3EnvConfig::from_vars(env_of(FULL)).unwrap();
        assert_eq!(config.endpoint, "http://seaweedfs:8333");
        assert_eq!(config.region, "local");
        assert_eq!(config.bucket, "nisaba-oplog");
    }

    #[test]
    fn env_config_requires_endpoint_access_secret_and_bucket() {
        for missing in [
            "NISABA_S3_ENDPOINT",
            "NISABA_S3_ACCESS_KEY",
            "NISABA_S3_SECRET_KEY",
            "NISABA_S3_BUCKET_OPLOG",
        ] {
            let vars: Vec<(&str, &str)> = FULL
                .iter()
                .copied()
                .filter(|(k, _)| *k != missing)
                .collect();
            let error = S3EnvConfig::from_vars(env_of(&vars)).unwrap_err();
            assert!(
                error.to_string().contains(missing),
                "error should name {missing}: {error}"
            );
        }
    }

    #[test]
    fn env_config_defaults_region_and_ignores_blank_values() {
        // Compose interpolates unset variables to "" — blank must behave like
        // unset (required) rather than satisfy the requirement with empties.
        let vars: Vec<(&str, &str)> = FULL
            .iter()
            .copied()
            .filter(|(k, _)| *k != "NISABA_S3_REGION")
            .chain([("NISABA_S3_ENDPOINT", "  ")])
            .collect();
        let error = S3EnvConfig::from_vars(env_of(&vars)).unwrap_err();
        assert!(error.to_string().contains("NISABA_S3_ENDPOINT"));

        let vars: Vec<(&str, &str)> = FULL
            .iter()
            .copied()
            .filter(|(k, _)| *k != "NISABA_S3_REGION")
            .collect();
        let config = S3EnvConfig::from_vars(env_of(&vars)).unwrap();
        assert_eq!(config.region, "us-east-1");
    }
}
