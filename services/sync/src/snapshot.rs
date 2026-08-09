//! Pluggable snapshot store.
//!
//! Op-log snapshots live in an S3-compatible object store. This component
//! defines the trait at that boundary and ships a filesystem implementation that
//! stands in for the real blob store. Swapping in an S3 client later means
//! writing a second `impl SnapshotStore` — nothing in the rest of the service
//! changes.
//!
//! A snapshot is `(version_vector, bytes)`: the VV records what history the
//! snapshot covers, the bytes are a Loro `ExportMode::Snapshot`. The latest
//! snapshot (highest VV) is the recovery point used to hydrate a room.
//!
//! ## Filesystem robustness invariants
//!
//! The [`FsSnapshotStore`] is built to survive crashes and concurrent writers:
//!
//! * **One file per snapshot, written atomically.** Each snapshot lives in a
//!   single file `<seq>.snap` laid out as `[u32 be vv_len][vv bytes][snapshot
//!   bytes]`. It is written to a uniquely-named `.<unique>.tmp` sibling and
//!   `rename(2)`-ed into place, so a reader never observes a half-written pair
//!   (the old `<seq>.snapshot` + `<seq>.vv` layout was not atomic across the two
//!   writes).
//! * **Monotonic sequence from `max(existing)+1`**, never from the file *count*.
//!   A deleted or gapped file therefore cannot cause a sequence collision.
//! * **Latest is chosen by version-vector metadata**, not by directory order or
//!   filename. Files are parsed defensively; a corrupt or truncated file is
//!   logged and skipped rather than bricking recovery for the whole document.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use loro::VersionVector;

use crate::config::DocId;
use crate::error::{SyncError, SyncResult};

/// A persisted snapshot.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Version vector the snapshot covers.
    pub vv: VersionVector,
    /// Opaque Loro snapshot bytes.
    pub bytes: Vec<u8>,
}

/// A durable store of document snapshots.
///
/// Implementations are expected to be content-addressable-ish by version vector;
/// the trait keeps it simple: put, get-latest, list, drop-all (for tests).
#[async_trait::async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Persist `snapshot` for `doc`.
    async fn put(&self, doc: &DocId, snapshot: Snapshot) -> SyncResult<()>;

    /// The most recent snapshot for `doc`, by version-vector ordering, or `None`.
    async fn latest(&self, doc: &DocId) -> SyncResult<Option<Snapshot>>;

    /// All snapshots for `doc`, oldest first.
    async fn list(&self, doc: &DocId) -> SyncResult<Vec<Snapshot>>;

    /// Remove every snapshot for `doc` (tests / maintenance only).
    async fn drop_all(&self, doc: &DocId) -> SyncResult<()>;
}

/// Filesystem snapshot store standing in for an S3-compatible blob store.
///
/// Layout: `root/<doc_id>/<seq>.snap`. See the component documentation for the atomicity,
/// monotonic-sequence, and metadata-based "latest" invariants.
pub struct FsSnapshotStore {
    root: PathBuf,
    /// Per-process counter used only to make temp-file names unique; it is *not*
    /// the persisted sequence (that is `max(existing)+1`).
    tmp_nonce: AtomicU64,
}

impl FsSnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> SyncResult<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root).map_err(SyncError::from)?;
        Ok(Self {
            root,
            tmp_nonce: AtomicU64::new(0),
        })
    }

    fn doc_dir(&self, doc: &DocId) -> PathBuf {
        // DocId forbids path separators and `..`, so this is traversal-safe.
        self.root.join(doc.as_str())
    }

    /// Write `payload` atomically to `dir/<seq>.snap` via a temp file + rename.
    /// The sequence is bumped until the destination does not already exist, so
    /// concurrent writers cannot clobber one another's snapshot.
    fn put_atomic(&self, dir: &Path, payload: &[u8]) -> SyncResult<()> {
        loop {
            let seqs = existing_seqs(dir)?;
            let next_seq = seqs.iter().copied().max().map_or(0, |m| m + 1);
            let final_path = dir.join(format!("{next_seq}.snap"));
            if final_path.exists() {
                // A concurrent writer landed the same sequence first; recompute.
                continue;
            }
            let nonce = self.tmp_nonce.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let tmp_path = dir.join(format!("{next_seq}.snap.{pid}.{nonce}.tmp"));

            // Write + fsync the temp file before renaming, so the destination is
            // never observed with partially-flushed bytes.
            {
                let mut f = File::create(&tmp_path).map_err(SyncError::from)?;
                f.write_all(payload).map_err(SyncError::from)?;
                f.sync_all().map_err(SyncError::from)?;
            }
            match std::fs::rename(&tmp_path, &final_path) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // Best-effort cleanup of the temp file on failure.
                    let _ = std::fs::remove_file(&tmp_path);
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        // Lost a race to create `final_path`; retry with a fresh seq.
                        continue;
                    }
                    return Err(SyncError::from(e));
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl SnapshotStore for FsSnapshotStore {
    async fn put(&self, doc: &DocId, snapshot: Snapshot) -> SyncResult<()> {
        let dir = self.doc_dir(doc);
        std::fs::create_dir_all(&dir).map_err(SyncError::from)?;
        let payload = encode_snapshot_file(&snapshot);
        self.put_atomic(&dir, &payload)
    }

    async fn latest(&self, doc: &DocId) -> SyncResult<Option<Snapshot>> {
        // Parse defensively: a single corrupt/partial file must not prevent
        // recovery from the valid ones. Latest is chosen by VV, never by name.
        let valid = read_all_valid(&self.doc_dir(doc))?;
        Ok(pick_latest_by_vv(valid))
    }

    async fn list(&self, doc: &DocId) -> SyncResult<Vec<Snapshot>> {
        // Oldest first by sequence number; corrupt files are skipped (with a
        // warning) so one bad file does not break enumeration.
        let mut entries = read_all_valid_with_seq(&self.doc_dir(doc))?;
        entries.sort_by_key(|(seq, _)| *seq);
        Ok(entries.into_iter().map(|(_, s)| s).collect())
    }

    async fn drop_all(&self, doc: &DocId) -> SyncResult<()> {
        let dir = self.doc_dir(doc);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SyncError::from(e)),
        }
    }
}

/// On-disk record: `[u32 be vv_len][vv bytes][snapshot bytes]`. Reading stops with
/// an error if the buffer is shorter than the declared lengths.
fn encode_snapshot_file(snapshot: &Snapshot) -> Vec<u8> {
    let vv = snapshot.vv.encode();
    let vv_len = u32::try_from(vv.len()).expect("version vector length fits in u32");
    let mut out = Vec::with_capacity(4 + vv.len() + snapshot.bytes.len());
    out.extend_from_slice(&vv_len.to_be_bytes());
    out.extend_from_slice(&vv);
    out.extend_from_slice(&snapshot.bytes);
    out
}

fn decode_snapshot_file(buf: &[u8]) -> Result<Snapshot, SyncError> {
    const HEADER: usize = 4;
    if buf.len() < HEADER {
        return Err(SyncError::Storage(format!(
            "snapshot file truncated in header ({} bytes)",
            buf.len()
        )));
    }
    let vv_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let vv_end = HEADER
        .checked_add(vv_len)
        .ok_or_else(|| SyncError::Storage("snapshot vv length overflows".into()))?;
    if buf.len() < vv_end {
        return Err(SyncError::Storage(format!(
            "snapshot file truncated in vv (declared {vv_len}, have {})",
            buf.len().saturating_sub(HEADER)
        )));
    }
    let vv = VersionVector::decode(&buf[HEADER..vv_end])?;
    let bytes = buf[vv_end..].to_vec();
    Ok(Snapshot { vv, bytes })
}

/// Parse every `*.snap` under `dir`, skipping corrupt files with a warning.
/// Missing directory → empty.
fn read_all_valid(dir: &Path) -> SyncResult<Vec<Snapshot>> {
    Ok(read_all_valid_with_seq(dir)?
        .into_iter()
        .map(|(_, s)| s)
        .collect())
}

fn read_all_valid_with_seq(dir: &Path) -> SyncResult<Vec<(u64, Snapshot)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(SyncError::from(e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(SyncError::from)?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "snap") {
            continue;
        }
        let Some(seq) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
        else {
            // Unknown filename in the doc dir; leave it alone but ignore it.
            continue;
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping unreadable snapshot");
                continue;
            }
        };
        match decode_snapshot_file(&bytes) {
            Ok(s) => out.push((seq, s)),
            Err(e) => {
                // A torn write (crash mid-rename) or a partial file must not
                // break recovery for the rest of the document's snapshots.
                tracing::warn!(path = %path.display(), error = %e, "skipping corrupt snapshot");
            }
        }
    }
    Ok(out)
}

fn existing_seqs(dir: &Path) -> std::io::Result<Vec<u64>> {
    let mut seqs: Vec<u64> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "snap")
            && let Some(n) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|stem| stem.parse::<u64>().ok())
        {
            seqs.push(n);
        }
    }
    seqs.sort_unstable();
    Ok(seqs)
}

/// Pick the snapshot with the greatest version vector.
///
/// For a single authority the VVs are totally ordered, so `partial_cmp` is
/// decisive. The op-count sum fallback only matters for the (impossible for one
/// authority) incomparable case, where it gives a deterministic, sensible pick
/// rather than panicking.
fn pick_latest_by_vv(snaps: Vec<Snapshot>) -> Option<Snapshot> {
    let mut best: Option<Snapshot> = None;
    for s in snaps {
        best = Some(match best {
            None => s,
            Some(b) => pick_newer(b, s),
        });
    }
    best
}

fn pick_newer(a: Snapshot, b: Snapshot) -> Snapshot {
    use std::cmp::Ordering;
    match a.vv.partial_cmp(&b.vv) {
        Some(Ordering::Less) => b,
        Some(Ordering::Greater | Ordering::Equal) => a,
        // Incomparable VVs: fall back to total op counter sum (larger wins; tie
        // keeps the incumbent `a` for determinism).
        None => {
            if vv_op_count(&b.vv) > vv_op_count(&a.vv) {
                b
            } else {
                a
            }
        }
    }
}

fn vv_op_count(vv: &VersionVector) -> u64 {
    vv.values()
        .map(|c| u64::try_from((*c).max(0)).unwrap_or(0))
        .sum()
}

/// In-memory [`SnapshotStore`] for tests and dev. Not durable across restarts.
///
/// `latest` uses the same metadata-based selection as the filesystem store so the
/// two are observably equivalent. One store instance is shared across every
/// document by the registry, so entries remain doc-keyed.
#[derive(Default)]
pub struct MemorySnapshotStore {
    store: std::sync::Mutex<std::collections::HashMap<String, Vec<(u64, Snapshot)>>>,
    seq: AtomicU64,
}

impl MemorySnapshotStore {
    fn entry_for(
        &self,
        doc: &DocId,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Vec<(u64, Snapshot)>>> {
        let mut store = self.store.lock().expect("mem snapshot poisoned");
        store.entry(doc.as_str().to_string()).or_default();
        store
    }
}

#[async_trait::async_trait]
impl SnapshotStore for MemorySnapshotStore {
    async fn put(&self, doc: &DocId, snapshot: Snapshot) -> SyncResult<()> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut store = self.entry_for(doc);
        store
            .get_mut(doc.as_str())
            .expect("entry initialised")
            .push((seq, snapshot));
        Ok(())
    }

    async fn latest(&self, doc: &DocId) -> SyncResult<Option<Snapshot>> {
        let store = self.store.lock().expect("mem snapshot poisoned");
        Ok(store
            .get(doc.as_str())
            .and_then(|v| pick_latest_by_vv(v.iter().map(|(_, s)| s.clone()).collect())))
    }

    async fn list(&self, doc: &DocId) -> SyncResult<Vec<Snapshot>> {
        let store = self.store.lock().expect("mem snapshot poisoned");
        let Some(entries) = store.get(doc.as_str()) else {
            return Ok(Vec::new());
        };
        let mut entries: Vec<(u64, Snapshot)> =
            entries.iter().map(|(seq, s)| (*seq, s.clone())).collect();
        entries.sort_by_key(|(seq, _)| *seq);
        Ok(entries.into_iter().map(|(_, s)| s).collect())
    }

    async fn drop_all(&self, doc: &DocId) -> SyncResult<()> {
        self.store
            .lock()
            .expect("mem snapshot poisoned")
            .remove(doc.as_str());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loro::{ExportMode, LoroDoc};
    use tempfile::tempdir;

    fn sample_snapshot(text: &str) -> Snapshot {
        let d = LoroDoc::new();
        d.set_peer_id(1).unwrap();
        d.get_text("text").insert(0, text).unwrap();
        d.commit();
        let bytes = d.export(ExportMode::Snapshot).unwrap();
        Snapshot {
            vv: d.oplog_vv(),
            bytes,
        }
    }

    fn restore_text(snap: &Snapshot) -> String {
        let d = LoroDoc::new();
        d.import(&snap.bytes).unwrap();
        d.get_text("text").to_string()
    }

    #[tokio::test]
    async fn put_and_latest_roundtrip() {
        let dir = tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path()).unwrap();
        let doc = DocId::new("d1").unwrap();
        let s = sample_snapshot("hello");
        store.put(&doc, s.clone()).await.unwrap();
        let got = store.latest(&doc).await.unwrap().unwrap();
        assert_eq!(restore_text(&got), "hello");
        assert_eq!(got.vv, s.vv);
    }

    #[tokio::test]
    async fn latest_picks_most_recent() {
        let dir = tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path()).unwrap();
        let doc = DocId::new("d1").unwrap();
        store.put(&doc, sample_snapshot("a")).await.unwrap();
        store.put(&doc, sample_snapshot("ab")).await.unwrap();
        let got = store.latest(&doc).await.unwrap().unwrap();
        assert_eq!(restore_text(&got), "ab");
    }

    #[tokio::test]
    async fn missing_doc_is_none() {
        let dir = tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path()).unwrap();
        let doc = DocId::new("ghost").unwrap();
        assert!(store.latest(&doc).await.unwrap().is_none());
        assert!(store.list(&doc).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sequence_is_monotonic_and_survives_gaps() {
        let dir = tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path()).unwrap();
        let doc = DocId::new("d1").unwrap();
        store.put(&doc, sample_snapshot("a")).await.unwrap();
        store.put(&doc, sample_snapshot("ab")).await.unwrap();
        store.put(&doc, sample_snapshot("abc")).await.unwrap();

        let doc_dir = dir.path().join("d1");
        let seqs_before = existing_seqs(&doc_dir).unwrap();
        assert_eq!(seqs_before, vec![0, 1, 2]);

        std::fs::remove_file(doc_dir.join("2.snap")).unwrap();
        store.put(&doc, sample_snapshot("abcd")).await.unwrap();
        store.put(&doc, sample_snapshot("abcde")).await.unwrap();
        let seqs_after = existing_seqs(&doc_dir).unwrap();
        assert_eq!(seqs_after, vec![0, 1, 2, 3]);
        assert!(std::fs::read_dir(&doc_dir).unwrap().all(|e| {
            std::path::Path::new(&e.unwrap().file_name())
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("snap"))
        }));
    }

    #[tokio::test]
    async fn latest_ignores_corrupt_and_partial_files() {
        let dir = tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path()).unwrap();
        let doc = DocId::new("d1").unwrap();
        store.put(&doc, sample_snapshot("good")).await.unwrap();

        let doc_dir = dir.path().join("d1");
        std::fs::write(doc_dir.join("9.snap"), b"\x00\x00\x00\x10short").unwrap();
        std::fs::write(doc_dir.join("10.snap"), b"\xff\xff\xff\xffgarbage").unwrap();

        let got = store.latest(&doc).await.unwrap().unwrap();
        assert_eq!(restore_text(&got), "good");
    }

    #[tokio::test]
    async fn put_leaves_no_partial_state_visible() {
        let dir = tempdir().unwrap();
        let store = FsSnapshotStore::new(dir.path()).unwrap();
        let doc = DocId::new("d1").unwrap();
        for i in 0..5 {
            store
                .put(&doc, sample_snapshot(&"x".repeat(i + 1)))
                .await
                .unwrap();
        }
        let doc_dir = dir.path().join("d1");
        let files: Vec<_> = std::fs::read_dir(&doc_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            files.iter().all(|f| {
                std::path::Path::new(f)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("snap"))
            }),
            "unexpected files left behind: {files:?}"
        );
        assert_eq!(store.list(&doc).await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn memory_store_latest_uses_vv_not_insertion_order() {
        let store = MemorySnapshotStore::default();
        let doc = DocId::new("d1").unwrap();
        let newer = sample_snapshot("newer-state");
        store.put(&doc, newer.clone()).await.unwrap();
        let older_doc = LoroDoc::new();
        older_doc.set_peer_id(1).unwrap();
        older_doc.get_text("text").insert(0, "older").unwrap();
        older_doc.commit();
        let older = Snapshot {
            vv: older_doc.oplog_vv(),
            bytes: older_doc.export(ExportMode::Snapshot).unwrap(),
        };
        assert!(older.vv < newer.vv);
        store.put(&doc, older).await.unwrap();
        let got = store.latest(&doc).await.unwrap().unwrap();
        assert_eq!(got.vv, newer.vv);
    }
}
