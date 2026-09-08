# Nisaba sync wire protocol

Reference for the binary framing used over the `sync` service WebSocket. This is
the canonical, versioned contract any client (the CodeMirror editor in `web/`, a
test harness, or a third-party peer) must speak. The implementation of record is
`services/sync/src/protocol.rs`; this document is derived from it.

- **Transport:** a single WebSocket connection per document per peer. All frames
  are **binary** WebSocket messages; text frames are ignored.
- **Protocol version:** `2` (carried in every `HELLO`). The server also accepts
  legacy version `1`, which does not receive persistence receipts.
- **Encoding:** big-endian integers; variable-length fields are
  `[u32 be length][bytes]`. A length-prefixed string is a length-prefixed UTF-8
  blob.

## Frame layout

```
[u8 tag][tag-specific payload]
```

| Tag | Name        | Direction    | Layout                                                                                  |
|----:|-------------|--------------|-----------------------------------------------------------------------------------------|
| 1   | `HELLO`     | client→srv   | `[u8 proto][str doc_id][u64 peer][str token][bytes last_vv]`                            |
| 2   | `WELCOME`   | srv→client   | `[u8 status][str note][u8 catchup_tag][catchup bytes?]`                                 |
| 3   | `UPDATE`    | both         | `[bytes]` — opaque Loro CRDT update                                                      |
| 4   | `SNAPSHOT`  | srv→client   | `[bytes]` — opaque Loro snapshot                                                         |
| 5   | `PRESENCE`  | both         | client: `[bytes state]`; server: `[bytes roster]`|
| 6   | `HEARTBEAT` | both         | *(empty)*                                                                                |
| 7   | `ERROR`     | srv→client   | `[u16 code][str msg]`                                                                    |
| 8   | `BYE`       | client→srv   | *(empty)*                                                                                |

- `status` (WELCOME): `0 = Ok` (catch-up attached), `1 = OkNoCatchUp`.
- `catchup_tag` (WELCOME): `0 = None`, `1 = Updates(bytes)`, `2 = Snapshot(bytes)`.
- `last_vv` (HELLO): the peer's last version vector, encoded with
  `loro::VersionVector::encode`. An empty `last_vv` requests a full snapshot.

The server roster is `[u32 count]([u64 peer][bytes state])*`, enclosed in
the frame's outer `[u32 length][roster bytes]` field. An empty server roster
is therefore `[u8 5][u32 4][u32 0]`.

## Application error codes

| Code | Meaning                                   |
|-----:|-------------------------------------------|
| 4000 | protocol error (bad frame / wrong order / undecodable CRDT update)  |
| 4001 | bad document id / peer id                 |
| 4003 | access denied (invalid/expired token, changed access, missing capability, or review-policy violation) |
| 4029 | limit exceeded (peer cap or frame rate)    |
| 4091 | resync required after server eviction     |
| 4130 | payload too large                         |
| 4500 | internal error                            |

## Lifecycle

1. Client opens `GET /sync/{doc_id}` (WebSocket upgrade). A bad `doc_id` is
   rejected with HTTP 400 before any upgrade.
2. Client sends `HELLO` with its peer id, an opaque role `token`, and its last
   version vector (empty for a brand-new peer).
3. Server resolves the token to a role (`author` / `reviewer` / `read-only`)
   through the injected `AccessResolver`, then replies `WELCOME` with either an
   incremental `Updates` payload (since `last_vv`) or a full `Snapshot` (when
   `last_vv` is empty or the gap cannot be filled incrementally).
4. Steady state: the client ships local edits as `UPDATE`; the server imports
   them into the authority and forwards the **same opaque bytes** to every other
   peer. With protocol 2, the sender receives the same `UPDATE` bytes as a
   persistence receipt. Presence is carried out-of-band via `PRESENCE` / `HEARTBEAT`.
5. Reconnect: a peer that kept its replica sends `HELLO` with its retained
   `last_vv`; the server replies with the incremental delta.
6. Graceful leave: send `BYE`. A slow peer or one whose presence expires is
   evicted. The server sends an `ERROR` frame with code 4091, then closes the
   WebSocket with code 4091. Reconnect with the retained version vector to
   catch up before sending more updates.

## Update handling

Accepted updates are relayed as their original bytes. The authority imports
updates and inspects reviewer changes to enforce the review policy: a text
change must have a corresponding review record. This check is not a complete
validation of review semantics; see [security](../../docs/security.md).
Read-only peers cannot send updates.

For protocol 2, the server echoes an accepted update to its sender only after
its bytes are durable. A retry whose operations are already in the authority
receives the same receipt without another log append. Rejected updates receive
an error and no receipt. New updates waiting for dependencies are still logged.

The browser keeps local update batches until their matching receipts arrive.
It sends one batch at a time, preserving transaction boundaries and the 4 MiB
per-update limit. Reconnect retries these original batches, never an aggregate
of the document's history or other peers' updates. This queue lives in memory;
it does not protect unsent edits when the tab closes.

Deploy the protocol-2 relay before the new web client. Legacy clients can still
connect to the upgraded relay; an old relay rejects protocol-2 clients explicitly.

Presence is ephemeral. The service does not store it in the op log or snapshots.
