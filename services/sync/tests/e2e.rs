//! End-to-end tests over real `WebSocket`s: clients edit one document through the
//! axum server and converge; a reconnecting client keeps its replica and catches
//! up via version-vector incremental sync; the health endpoints and document-id
//! validation are exercised over plain HTTP.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{FutureExt, SinkExt, StreamExt};
use loro::LoroDoc;
use nisaba_sync::protocol::{CatchUp, Frame, PROTOCOL_VERSION};
use nisaba_sync::{
    AccessResolver, AuthError, Config, DocId, DocRegistry, MemoryOpLogStore, MemorySnapshotStore,
    Role, StaticAccessResolver, SystemClock,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{connect_async, tungstenite::Message};

async fn spawn_server() -> std::net::SocketAddr {
    spawn_server_with(Arc::new(StaticAccessResolver::allow_all(Role::Author))).await
}

#[tokio::test]
async fn oversized_hello_token_and_version_vector_are_rejected() {
    let addr = spawn_server().await;
    // Frame decode alone bounds each blob at 4 MiB; the HELLO token (16 KiB
    // cap) and version vector (64 KiB cap) have tighter limits that must be
    // enforced before the token reaches the access resolver.
    for (token, last_vv) in [
        (
            "t".repeat(nisaba_sync::config::MAX_TOKEN_BYTES + 1),
            Vec::new(),
        ),
        (
            "dev".to_string(),
            vec![0u8; nisaba_sync::config::MAX_VV_BYTES + 1],
        ),
    ] {
        let mut ws = dial(addr, "oversized_hello").await;
        send(
            &mut ws,
            Frame::Hello {
                proto: PROTOCOL_VERSION,
                doc_id: "oversized_hello".into(),
                peer: 42,
                token,
                last_vv,
            },
        )
        .await;
        let mut reply = None;
        while let Some(msg) = ws.next().await {
            if let Ok(Message::Binary(b)) = msg {
                reply = Some(Frame::decode(&b, 1 << 24).unwrap());
                break;
            }
        }
        match reply {
            Some(Frame::Error { code, .. }) if code == nisaba_sync::session::codes::TOO_LARGE => {}
            other => panic!("expected TOO_LARGE error frame, got {other:?}"),
        }
    }
}

/// Spawn a server with an explicit access resolver, so the deny-by-default path
/// can be exercised end to end.
async fn spawn_server_with(access: Arc<dyn nisaba_sync::AccessResolver>) -> std::net::SocketAddr {
    let registry = DocRegistry::new(
        Arc::new(MemoryOpLogStore::default()),
        Arc::new(MemorySnapshotStore::default()),
        Arc::new(Config::default()),
        Arc::new(SystemClock),
        access,
    );
    let router = nisaba_sync::server::build(registry, Arc::new(Config::default()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

#[derive(Clone)]
struct MutableAccessResolver {
    role: Arc<Mutex<Option<Role>>>,
}

impl MutableAccessResolver {
    fn new(role: Role) -> Self {
        Self {
            role: Arc::new(Mutex::new(Some(role))),
        }
    }

    fn revoke(&self) {
        *self.role.lock().unwrap() = None;
    }
}

#[async_trait::async_trait]
impl AccessResolver for MutableAccessResolver {
    async fn resolve(&self, _doc: &DocId, _token: &str) -> Result<Role, AuthError> {
        self.role
            .lock()
            .unwrap()
            .ok_or_else(|| AuthError::Unauthenticated("membership removed".to_string()))
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// A WebSocket sync client wrapping its own Loro replica.
struct Client {
    ws: Ws,
    doc: LoroDoc,
    pending: Arc<Mutex<Vec<Vec<u8>>>>,
    _sub: loro::Subscription,
}

fn make_doc(peer: u64) -> (LoroDoc, Arc<Mutex<Vec<Vec<u8>>>>, loro::Subscription) {
    let doc = LoroDoc::new();
    doc.set_peer_id(peer).unwrap();
    let pending = Arc::new(Mutex::new(Vec::new()));
    let cb = Arc::clone(&pending);
    let sub = doc.subscribe_local_update(Box::new(move |u: &Vec<u8>| {
        cb.lock().unwrap().push(u.clone());
        true
    }));
    (doc, pending, sub)
}

async fn dial(addr: std::net::SocketAddr, doc_id: &str) -> Ws {
    let url = format!("ws://{addr}/sync/{doc_id}");
    let (ws, _resp) = connect_async(url).await.unwrap();
    ws
}

async fn send(ws: &mut Ws, frame: Frame) {
    ws.send(Message::Binary(Bytes::from(frame.encode())))
        .await
        .unwrap();
}

impl Client {
    /// Fresh client: empty vv → server sends a snapshot in the welcome.
    async fn connect(addr: std::net::SocketAddr, doc_id: &str, peer: u64) -> Self {
        let (doc, pending, sub) = make_doc(peer);
        Self::connect_with(addr, doc_id, peer, doc, pending, sub, Vec::new()).await
    }

    /// Reconnecting client: reuses an existing replica and sends its last vv so
    /// the server replies with an incremental catch-up.
    async fn reconnect(
        addr: std::net::SocketAddr,
        doc_id: &str,
        peer: u64,
        doc: LoroDoc,
        pending: Arc<Mutex<Vec<Vec<u8>>>>,
        last_vv: Vec<u8>,
    ) -> Self {
        // Re-subscribe on the retained doc so future local edits are captured.
        let cb = Arc::clone(&pending);
        let sub = doc.subscribe_local_update(Box::new(move |u: &Vec<u8>| {
            cb.lock().unwrap().push(u.clone());
            true
        }));
        Self::connect_with(addr, doc_id, peer, doc, pending, sub, last_vv).await
    }

    async fn connect_with(
        addr: std::net::SocketAddr,
        doc_id: &str,
        peer: u64,
        doc: LoroDoc,
        pending: Arc<Mutex<Vec<Vec<u8>>>>,
        sub: loro::Subscription,
        last_vv: Vec<u8>,
    ) -> Self {
        let mut ws = dial(addr, doc_id).await;
        send(
            &mut ws,
            Frame::Hello {
                proto: PROTOCOL_VERSION,
                doc_id: doc_id.to_string(),
                peer,
                token: "dev".to_string(),
                last_vv,
            },
        )
        .await;
        let mut c = Self {
            ws,
            doc,
            pending,
            _sub: sub,
        };
        // Apply the welcome catch-up.
        if let Some(Frame::Welcome { catchup, .. }) = c.recv_frame().await {
            match catchup {
                CatchUp::None => {}
                CatchUp::Updates(b) | CatchUp::Snapshot(b) => {
                    c.doc.import(&b).unwrap();
                }
            }
        }
        c
    }

    async fn recv_frame(&mut self) -> Option<Frame> {
        while let Some(msg) = self.ws.next().await {
            if let Ok(Message::Binary(b)) = msg {
                return Some(Frame::decode(&b, 1 << 24).unwrap());
            }
        }
        None
    }

    /// Insert text and immediately ship the resulting local update.
    async fn edit(&mut self, pos: usize, text: &str) {
        self.doc.get_text("text").insert(pos, text).unwrap();
        self.doc.commit();
        self.flush().await;
    }

    async fn flush(&mut self) {
        let updates = std::mem::take(&mut *self.pending.lock().unwrap());
        for u in updates {
            send(&mut self.ws, Frame::Update(u)).await;
        }
    }

    /// Import every Update frame waiting on the socket right now.
    fn drain_updates(&mut self) {
        while let Some(Some(Ok(Message::Binary(b)))) = self.ws.next().now_or_never() {
            if let Frame::Update(u) = Frame::decode(&b, 1 << 24).unwrap() {
                self.doc.import(&u).unwrap();
            }
        }
    }

    fn text(&self) -> String {
        self.doc.get_text("text").to_string()
    }

    fn vv(&self) -> Vec<u8> {
        self.doc.oplog_vv().encode()
    }

    fn snapshot_bytes(&self) -> Vec<u8> {
        use loro::ExportMode;
        self.doc.export(ExportMode::Snapshot).unwrap()
    }
}

/// Wait until `cond(&c)` holds or we time out, draining updates each tick.
async fn settle_until<F>(c: &mut Client, cond: F)
where
    F: Fn(&Client) -> bool,
{
    for _ in 0..50 {
        c.drain_updates();
        if cond(c) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    c.drain_updates();
}

#[tokio::test]
async fn two_clients_converge_over_websocket() {
    let addr = spawn_server().await;
    let mut a = Client::connect(addr, "e2e", 1).await;
    let mut b = Client::connect(addr, "e2e", 2).await;

    a.edit(0, "hello").await;
    settle_until(&mut b, |c| c.text() == "hello").await;
    assert_eq!(a.text(), "hello");
    assert_eq!(b.text(), "hello");

    b.edit(5, " world").await;
    settle_until(&mut a, |c| c.text() == "hello world").await;
    assert_eq!(a.text(), "hello world");
    assert_eq!(b.text(), "hello world");
}

#[tokio::test]
async fn reconnect_catches_up_over_websocket() {
    let addr = spawn_server().await;

    // Client 1 seeds the document.
    let mut a = Client::connect(addr, "rc", 1).await;
    a.edit(0, "seed").await;

    // Client 2 comes online, converges on "seed", then goes offline.
    let mut b = Client::connect(addr, "rc", 2).await;
    settle_until(&mut b, |c| c.text() == "seed").await;
    let b_vv = b.vv();
    let b_snap = b.snapshot_bytes();
    drop(b);
    // Let the server notice the closed socket and remove peer 2's session.
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;

    // While B is offline, client 1 edits more.
    a.edit(4, "MORE").await;

    // B reconnects: a fresh replica seeded from B's last snapshot, carrying its
    // last version vector so the server sends an incremental catch-up.
    let (b2_doc, b2_pending, b2_sub) = make_doc(2);
    b2_doc.import(&b_snap).unwrap();
    let mut b2 = Client::reconnect(addr, "rc", 2, b2_doc, b2_pending, b_vv).await;
    let _ = b2_sub;
    settle_until(&mut b2, |c| c.text() == a.text()).await;
    assert_eq!(b2.text(), a.text());
    assert_eq!(b2.text(), "seedMORE");
}

#[tokio::test]
async fn health_endpoint_reports_status() {
    let addr = spawn_server().await;
    let body = http_get(addr, "/health").await;
    assert!(body.contains("\"status\":\"ok\""), "{body}");
    assert!(body.contains("nisaba-sync"), "{body}");
    assert!(body.contains("\"protocol\""), "{body}");

    let ready = http_get(addr, "/health/ready").await;
    assert!(ready.contains("\"status\":\"ready\""), "{ready}");
}

#[tokio::test]
async fn healthz_liveness_alias_works() {
    // `/healthz` is the conventional k8s liveness path and must be served.
    let addr = spawn_server().await;
    let body = http_get(addr, "/healthz").await;
    let status = body.lines().next().unwrap_or("");
    assert!(status.contains("200"), "expected 200, got: {status}");
    assert!(body.contains("\"status\":\"ok\""), "{body}");
}

#[tokio::test]
async fn production_default_denies_every_token() {
    // With the production default resolver (no grants, no allow-all), every HELLO
    // must be denied — the server is safe by default.
    let addr = spawn_server_with(Arc::new(StaticAccessResolver::new())).await;
    let mut ws = dial(addr, "locked").await;
    send(
        &mut ws,
        Frame::Hello {
            proto: PROTOCOL_VERSION,
            doc_id: "locked".to_string(),
            peer: 1,
            token: "any-token".to_string(),
            last_vv: Vec::new(),
        },
    )
    .await;
    let frame = ws.next().await;
    let denied = match frame {
        Some(Ok(Message::Binary(b))) => matches!(
            Frame::decode(&b, 1 << 24).unwrap(),
            Frame::Error { code, .. } if code == nisaba_sync::session::codes::FORBIDDEN
        ),
        _ => false,
    };
    assert!(denied, "expected FORBIDDEN under deny-by-default");
}

#[tokio::test]
async fn revoking_membership_terminates_an_existing_author_session() {
    let access = MutableAccessResolver::new(Role::Author);
    let addr = spawn_server_with(Arc::new(access.clone())).await;
    let mut client = Client::connect(addr, "revoked-live-session", 1).await;
    let mut observer = Client::connect(addr, "revoked-live-session", 2).await;

    access.revoke();
    client.edit(0, "must not reach the relay").await;

    let denied = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match client.recv_frame().await {
                Some(frame @ Frame::Error { .. }) => break frame,
                Some(_) => {}
                None => panic!("relay closed before sending the revocation error"),
            }
        }
    })
    .await
    .expect("relay should answer the revoked update promptly");
    assert!(
        matches!(
            denied,
            Frame::Error { code, ref msg }
                if code == nisaba_sync::session::codes::FORBIDDEN
                    && msg.contains("access was revoked")
        ),
        "expected an explicit access-revoked denial, got {denied:?}"
    );

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    observer.drain_updates();
    assert_eq!(observer.text(), "", "revoked update reached the relay");
}

#[tokio::test]
async fn heartbeat_detects_revocation_before_the_user_edits() {
    let access = MutableAccessResolver::new(Role::Author);
    let addr = spawn_server_with(Arc::new(access.clone())).await;
    let mut client = Client::connect(addr, "idle-revoked-session", 1).await;

    access.revoke();
    send(&mut client.ws, Frame::Heartbeat).await;

    let denied = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match client.recv_frame().await {
                Some(frame @ Frame::Error { .. }) => break frame,
                Some(_) => {}
                None => panic!("relay closed before sending the revocation error"),
            }
        }
    })
    .await
    .expect("heartbeat should detect revoked access promptly");
    assert!(
        matches!(
            denied,
            Frame::Error { code, ref msg }
                if code == nisaba_sync::session::codes::FORBIDDEN
                    && msg.contains("access was revoked")
        ),
        "expected an explicit access-revoked denial, got {denied:?}"
    );
}

#[tokio::test]
async fn bad_doc_id_is_rejected_before_upgrade() {
    let addr = spawn_server().await;
    // `..` is a forbidden document id.
    let status = http_get(addr, "/sync/..").await;
    let first_line = status.lines().next().unwrap_or("");
    assert!(
        first_line.contains("400"),
        "expected HTTP 400, got: {first_line}"
    );
}

async fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf).to_string()
}

/// Spawn a server with real readiness probes wired (see `server::Readiness`).
async fn spawn_server_with_readiness(
    readiness: nisaba_sync::server::Readiness,
) -> std::net::SocketAddr {
    let registry = DocRegistry::new(
        Arc::new(MemoryOpLogStore::default()),
        Arc::new(MemorySnapshotStore::default()),
        Arc::new(Config::default()),
        Arc::new(SystemClock),
        Arc::new(StaticAccessResolver::new()),
    );
    let router =
        nisaba_sync::server::build_with_readiness(registry, Arc::new(Config::default()), readiness);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

#[tokio::test]
async fn readiness_fails_while_jwks_cache_is_stale() {
    // An empty JWKS cache fails closed: every token would be denied, so the
    // endpoint must report 503 until the background refresher lands keys.
    let jwks = Arc::new(nisaba_sync::JwksCache::empty(
        std::time::Duration::from_secs(3600),
        Arc::new(SystemClock),
    ));
    let addr = spawn_server_with_readiness(nisaba_sync::server::Readiness {
        jwks: Some(jwks),
        data_dir: None,
    })
    .await;
    let body = http_get(addr, "/health/ready").await;
    assert!(body.contains("503"), "{body}");
    assert!(body.contains("jwks"), "{body}");
}

#[tokio::test]
async fn readiness_fails_when_data_dir_is_not_writable() {
    // A path under a regular file can never be created, so the probe fails
    // without depending on filesystem permissions (which root ignores).
    let file = tempfile::NamedTempFile::new().unwrap();
    let blocked = file.path().join("subdir");
    let addr = spawn_server_with_readiness(nisaba_sync::server::Readiness {
        jwks: None,
        data_dir: Some(blocked),
    })
    .await;
    let body = http_get(addr, "/health/ready").await;
    assert!(body.contains("503"), "{body}");
    assert!(body.contains("not writable"), "{body}");
}

#[tokio::test]
async fn readiness_passes_when_probes_pass() {
    let dir = tempfile::tempdir().unwrap();
    let addr = spawn_server_with_readiness(nisaba_sync::server::Readiness {
        jwks: None,
        data_dir: Some(dir.path().to_path_buf()),
    })
    .await;
    let body = http_get(addr, "/health/ready").await;
    assert!(body.contains("200"), "{body}");
    assert!(body.contains("\"status\":\"ready\""), "{body}");
}
