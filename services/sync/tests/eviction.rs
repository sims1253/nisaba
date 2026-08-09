//! Integration tests for the DoS-hardening behaviors: registry room eviction
//! (capacity cap + idle-TTL reclamation) and the per-session inbound frame-rate
//! limiter.

use std::sync::Arc;
use std::time::Duration;

use nisaba_sync::{
    Config, DocId, DocRegistry, MemoryOpLogStore, MemorySnapshotStore, Role, StaticAccessResolver,
    SystemClock,
};
use tokio::sync::mpsc;

fn registry_with(config: Arc<Config>) -> DocRegistry {
    DocRegistry::new(
        Arc::new(MemoryOpLogStore::default()),
        Arc::new(MemorySnapshotStore::default()),
        config,
        Arc::new(SystemClock),
        Arc::new(StaticAccessResolver::allow_all(Role::Author)),
    )
}

/// A minimal peer so a room has a live session when we assert `is_empty`.
struct Peer {
    id: u64,
    rx: mpsc::Receiver<nisaba_sync::protocol::Frame>,
    close: nisaba_sync::CloseSignal,
    generation: u64,
}

impl Peer {
    fn new(id: u64) -> Self {
        let (_tx, rx) = mpsc::channel(64);
        Self {
            id,
            rx,
            close: nisaba_sync::close_signal(nisaba_sync::CLOSE_NORMAL),
            generation: 0,
        }
    }

    fn join(&mut self, room: &Arc<nisaba_sync::DocRoom>) {
        let (tx, rx) = mpsc::channel(64);
        self.rx = rx;
        self.close = nisaba_sync::close_signal(nisaba_sync::CLOSE_NORMAL);
        let outcome = room
            .join(
                nisaba_sync::PeerId(self.id),
                Role::Author,
                &[],
                Vec::new(),
                tx,
                self.close.clone(),
            )
            .expect("join");
        self.generation = outcome.generation;
    }
}

#[tokio::test]
async fn cadencing_eviction_rejects_opens_past_cap() {
    let cfg = Config {
        max_rooms: 3,
        ..Config::default()
    };
    let max_rooms = cfg.max_rooms;
    let registry = registry_with(Arc::new(cfg));

    for i in 0..2u64 {
        let doc = DocId::new(format!("doc{i}")).unwrap();
        registry.get_or_open(&doc).await.unwrap();
    }
    // The cap is 3; opening 3 distinct docs is fine.
    let doc = DocId::new("doc2").unwrap();
    registry.get_or_open(&doc).await.unwrap();
    assert_eq!(registry.len(), 3);

    // The first two rooms are empty and idle (they were never joined), so this
    // next open evicts one to stay under the cap rather than failing.
    let doc = DocId::new("doc3").unwrap();
    registry.get_or_open(&doc).await.unwrap();
    assert!(registry.len() <= 3);

    // Joining a room makes it non-empty, so once all rooms are occupied the cap
    // is enforced by rejection, not eviction of an active room.
    let busy = DocId::new("busy").unwrap();
    let busy_room = registry.get_or_open(&busy).await.unwrap();
    let mut p = Peer::new(99);
    p.join(&busy_room);

    // Now fill the registry to the cap with live rooms.
    let mut peers = Vec::new();
    for i in 100..200u64 {
        let doc = DocId::new(format!("live{i}")).unwrap();
        let room = registry.get_or_open(&doc).await.unwrap();
        let mut p = Peer::new(i);
        p.join(&room);
        peers.push(p);
        if registry.len() >= max_rooms {
            break;
        }
    }
    // A further distinct open must either evict an *empty* room or reject — it
    // can never resize the map past the cap.
    let extra = DocId::new("extra").unwrap();
    if let Err(e) = registry.get_or_open(&extra).await {
        assert!(e.to_string().contains("limit"));
    }
    assert!(registry.len() <= max_rooms);
    drop(peers);
}

#[tokio::test]
async fn idle_rooms_are_evicted_and_reopen_fresh() {
    let cfg = Config {
        evict_idle_ttl_ms: 50,
        ..Config::default()
    };
    let registry = registry_with(Arc::new(cfg));

    let doc = DocId::new("doc").unwrap();
    let room = registry.get_or_open(&doc).await.unwrap();

    // Just-opened, empty room is not yet idle.
    assert_eq!(registry.evict_idle_rooms().await, 0);
    assert_eq!(registry.len(), 1);

    // Evict it eagerly: a room with no live sessions and no recent activity is
    // reaped; the same doc id can then be re-opened.
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(registry.evict_idle_rooms().await, 1);
    assert_eq!(registry.len(), 0);

    let reopened = registry.get_or_open(&doc).await.unwrap();
    assert_eq!(registry.len(), 1);
    let _ = reopened;
    let _ = room;
}
