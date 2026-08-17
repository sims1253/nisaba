//! Append-only op-log persistence.
//!
//! The op log feeds object-store snapshots. We model it as a trait with a
//! filesystem implementation standing in for the durable blob boundary. The key
//! invariant — **append-only** — is enforced structurally: the trait exposes no
//! mutation other than [`OpLogStore::append`], and the filesystem impl opens the
//! file with `O_APPEND` so even a buggy caller cannot rewrite history in place.
//!
//! The op log records every CRDT update the authority receives. Combined with the
//! snapshot store it lets a room rebuild its authoritative document on restart:
//! load the latest snapshot, then replay the log (re-importing already-applied
//! updates is a no-op in Loro, so correctness does not depend on truncation).
//!
//! Compaction (truncating the log after a snapshot) is intentionally deferred —
//! Memory and disk growth are instrumented before more elaborate compaction is added.

use std::fs::{File, OpenOptions};
use std::io::{BufReader, IoSlice, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::{DocId, MAX_UPDATE_BYTES};
use crate::error::{SyncError, SyncResult};

/// An append-only log of CRDT updates for a set of documents.
///
/// Entries are opaque byte blobs; the store treats them as `(doc_id, update)`.
#[async_trait::async_trait]
pub trait OpLogStore: Send + Sync {
    /// Append `update` to the end of `doc`'s log. Never overwrites.
    async fn append(&self, doc: &DocId, update: &[u8]) -> SyncResult<()>;

    /// Read every recorded update for `doc`, in insertion order.
    async fn read_all(&self, doc: &DocId) -> SyncResult<Vec<Vec<u8>>>;

    /// Number of recorded updates for `doc`.
    async fn len(&self, doc: &DocId) -> SyncResult<u64>;

    /// Whether `doc` has any recorded updates.
    async fn is_empty(&self, doc: &DocId) -> SyncResult<bool> {
        Ok(self.len(doc).await? == 0)
    }

    /// Close and release any resources held for `doc` (e.g. an open file
    /// handle). A no-op by default; the filesystem store flushes + syncs and
    /// drops its handle so idle rooms do not pin file descriptors forever.
    ///
    /// Kept synchronous: it is simple cleanup (flush + sync + drop a handle) with
    /// no blocking I/O worth yielding on, and callers do not need to await it.
    fn close(&self, doc: &DocId) -> SyncResult<()> {
        let _ = doc;
        Ok(())
    }
}

/// On-disk record framing: `[u32 be len][bytes]`. Reading stops cleanly at EOF.
const LEN_HEADER: usize = 4;

/// Filesystem op-log store. One append-only file per document under `root`.
///
/// The directory layout is `root/<doc_id>.oplog`. Document ids are validated by
/// [`DocId`] (no path separators, no `..`), so this is path-traversal-safe.
pub struct FsOpLogStore {
    root: PathBuf,
    // Per-file append handle, lazily created. Writes are serialised by the mutex.
    handles: Mutex<std::collections::HashMap<String, File>>,
}

impl FsOpLogStore {
    /// Create a store rooted at `root`, creating the directory if needed.
    pub fn new(root: impl Into<PathBuf>) -> SyncResult<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(SyncError::from)?;
        Ok(Self {
            root,
            handles: Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn path_for(&self, doc: &DocId) -> PathBuf {
        self.root.join(format!("{}.oplog", doc.as_str()))
    }

    fn handle(
        &self,
        doc: &DocId,
    ) -> SyncResult<std::sync::MutexGuard<'_, std::collections::HashMap<String, File>>> {
        // Poisoning maps to the store's error type; the map (possibly
        // mid-update when the panic hit) is never recovered via `into_inner`.
        let mut guards = self
            .handles
            .lock()
            .map_err(|_| SyncError::Internal("op-log handle map lock poisoned".into()))?;
        if !guards.contains_key(doc.as_str()) {
            let path = self.path_for(doc);
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&path)
                .map_err(SyncError::from)?;
            guards.insert(doc.as_str().to_string(), file);
        }
        Ok(guards)
    }
}

#[async_trait::async_trait]
impl OpLogStore for FsOpLogStore {
    async fn append(&self, doc: &DocId, update: &[u8]) -> SyncResult<()> {
        let mut guards = self.handle(doc)?;
        let file = guards.get_mut(doc.as_str()).expect("just inserted");
        let len = u32::try_from(update.len())
            .map_err(|_| SyncError::Limit("update too large for op log frame".into()))?;
        let header = len.to_be_bytes();
        let bufs = [IoSlice::new(&header), IoSlice::new(update)];
        let written = file.write_vectored(&bufs).map_err(SyncError::from)?;
        let expected = header.len() + update.len();
        if written != expected {
            return Err(SyncError::Storage(format!(
                "short op-log write: {written} of {expected} bytes"
            )));
        }
        file.flush().map_err(SyncError::from)?;
        // Fsync before returning so the ACK sent to the
        // client after this call is a durable acknowledgement. Without fsync,
        // a crash after ACK but before the OS flushes the page cache loses the
        // update — a violation of the durable-ACK contract. The cost is one
        // syscall per accepted update; the recovery model (snapshot + oplog
        // replay) still works, but now the oplog records up to the last ACK
        // are guaranteed to survive a crash.
        file.sync_all().map_err(SyncError::from)?;
        Ok(())
    }

    async fn read_all(&self, doc: &DocId) -> SyncResult<Vec<Vec<u8>>> {
        let path = self.path_for(doc);
        read_records(&path)
    }

    async fn len(&self, doc: &DocId) -> SyncResult<u64> {
        Ok(read_records(&self.path_for(doc))?.len() as u64)
    }

    fn close(&self, doc: &DocId) -> SyncResult<()> {
        let mut guards = self
            .handles
            .lock()
            .map_err(|_| SyncError::Internal("op-log handle map lock poisoned".into()))?;
        if let Some(mut file) = guards.remove(doc.as_str()) {
            file.flush().map_err(SyncError::from)?;
            file.sync_all().map_err(SyncError::from)?;
        }
        Ok(())
    }
}

/// Bound each record's length header so a corrupt/torn header (e.g. `0xFFFFFFFF`)
/// cannot trigger a multi-GB allocation while replaying an op log. Lengths beyond
/// [`MAX_UPDATE_BYTES`] are treated as torn writes: the read stops cleanly with a
/// warning and the records read so far are returned.
fn read_records(path: &Path) -> SyncResult<Vec<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(SyncError::from(e)),
    };
    let mut reader = BufReader::new(file);
    let mut out = Vec::new();
    loop {
        let mut header = [0u8; LEN_HEADER];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            // Clean EOF between records is the normal end of the log.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            // Any other failure (I/O error, not a truncated tail) is genuine.
            Err(e) => return Err(SyncError::from(e)),
        }
        let len = u32::from_be_bytes(header) as usize;
        if len > MAX_UPDATE_BYTES {
            tracing::warn!(
                path = %path.display(),
                len,
                max = MAX_UPDATE_BYTES,
                "torn or corrupt op-log length header; stopping replay at clean records"
            );
            break;
        }
        let mut buf = vec![0u8; len];
        match reader.read_exact(&mut buf) {
            Ok(()) => out.push(buf),
            // A record body cut short by EOF is a torn trailing write: keep the
            // records read so far and stop cleanly rather than failing the room.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                tracing::warn!(
                    path = %path.display(),
                    len,
                    "torn trailing op-log record truncated at EOF; dropping it"
                );
                break;
            }
            Err(e) => return Err(SyncError::from(e)),
        }
    }
    Ok(out)
}

/// In-memory [`OpLogStore`] for tests and dev. Not durable across restarts.
#[derive(Default)]
pub struct MemoryOpLogStore {
    logs: Mutex<std::collections::HashMap<String, Vec<Vec<u8>>>>,
}

#[async_trait::async_trait]
impl OpLogStore for MemoryOpLogStore {
    async fn append(&self, doc: &DocId, update: &[u8]) -> SyncResult<()> {
        let mut logs = self.logs.lock().expect("mem oplog poisoned");
        logs.entry(doc.as_str().to_string())
            .or_default()
            .push(update.to_vec());
        Ok(())
    }

    async fn read_all(&self, doc: &DocId) -> SyncResult<Vec<Vec<u8>>> {
        Ok(self
            .logs
            .lock()
            .expect("mem oplog poisoned")
            .get(doc.as_str())
            .cloned()
            .unwrap_or_default())
    }

    async fn len(&self, doc: &DocId) -> SyncResult<u64> {
        Ok(self
            .logs
            .lock()
            .expect("mem oplog poisoned")
            .get(doc.as_str())
            .map_or(0, Vec::len) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn appends_and_reads_back_in_order() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("d1").unwrap();
        store.append(&doc, b"one").await.unwrap();
        store.append(&doc, b"two").await.unwrap();
        store.append(&doc, b"three").await.unwrap();
        assert_eq!(
            store.read_all(&doc).await.unwrap(),
            vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]
        );
        assert_eq!(store.len(&doc).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn separate_documents_get_separate_logs() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let a = DocId::new("a").unwrap();
        let b = DocId::new("b").unwrap();
        store.append(&a, b"ax").await.unwrap();
        store.append(&b, b"bx").await.unwrap();
        assert_eq!(store.read_all(&a).await.unwrap(), vec![b"ax".to_vec()]);
        assert_eq!(store.read_all(&b).await.unwrap(), vec![b"bx".to_vec()]);
    }

    #[tokio::test]
    async fn missing_doc_reads_empty() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("ghost").unwrap();
        assert!(store.is_empty(&doc).await.unwrap());
        assert!(store.read_all(&doc).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn record_with_zero_bytes_is_stored_and_read() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("z").unwrap();
        store.append(&doc, &[]).await.unwrap();
        assert_eq!(store.read_all(&doc).await.unwrap(), vec![Vec::<u8>::new()]);
    }

    #[tokio::test]
    async fn torn_trailing_length_header_is_skipped() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("torn1").unwrap();
        store.append(&doc, b"good").await.unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.path_for(&doc))
            .unwrap()
            .write_all(&0xFFFF_FFFFu32.to_be_bytes())
            .unwrap();
        assert_eq!(store.read_all(&doc).await.unwrap(), vec![b"good".to_vec()]);
        assert_eq!(store.len(&doc).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn torn_body_record_is_dropped_cleanly() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("torn2").unwrap();
        store.append(&doc, b"first").await.unwrap();
        store.append(&doc, b"second").await.unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.path_for(&doc))
            .unwrap()
            .write_all(&(100u32).to_be_bytes())
            .unwrap();
        assert_eq!(
            store.read_all(&doc).await.unwrap(),
            vec![b"first".to_vec(), b"second".to_vec()]
        );
    }

    #[tokio::test]
    async fn close_releases_handle_and_reappends() {
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("c1").unwrap();
        store.append(&doc, b"a").await.unwrap();
        store.close(&doc).unwrap();
        store.append(&doc, b"b").await.unwrap();
        assert_eq!(
            store.read_all(&doc).await.unwrap(),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
    }

    #[tokio::test]
    async fn poisoned_handle_map_fails_append_gracefully() {
        // A panic while the handle map is held poisons it: appends must return
        // the store's error (denying the update) instead of panicking every
        // later request through the store.
        let dir = tempdir().unwrap();
        let store = FsOpLogStore::new(dir.path()).unwrap();
        let doc = DocId::new("p1").unwrap();
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.handles.lock().unwrap();
            panic!("poison the op-log handle map");
        }));
        assert!(poisoned.is_err());
        let error = store.append(&doc, b"x").await.unwrap_err();
        assert!(matches!(error, SyncError::Internal(_)));
    }
}
