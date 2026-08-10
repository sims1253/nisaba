import type { LoroDoc } from "loro-crdt"
import { VersionVector } from "loro-crdt"
import { decodeSyncFrame, encodeSyncFrame } from "./protocol"

export type SyncStatus = "connecting" | "connected" | "disconnected" | "unsupported"

export interface SyncConnection {
  readonly close: () => void
}

/**
 * True while a remote update is being imported into the replica.
 *
 * `doc.import()` fires Loro subscriptions synchronously, and the loro-codemirror
 * listener in turn dispatches to CodeMirror synchronously, so any suggestion
 * recording that runs in that same synchronous chain sees this set. It lets
 * `updateReviewItems` tell a peer-originated edit apart from the user's own
 * typing without relying on text-diff timing, which the relay destabilises.
 */
let importingRemote = false

export function isImportingRemote(): boolean { return importingRemote }

export interface SyncOptions {
  readonly documentId: string
  readonly token?: string
  /**
   * The persisted document body, used when the relay is empty: the replica is
   * seeded post-WELCOME and pushed as the single authoritative origin. See
   * connectSync's welcome handler.
   */
  readonly seedBody?: string
  /**
   * Invoked once, synchronously, immediately AFTER the welcome handler has given
   * the replica its definitive content (imported from the relay or seeded as the
   * origin). The caller attaches the CRDT binding here.
   */
  readonly onReady?: () => void
  readonly onStatus?: (status: SyncStatus, detail?: string) => void
}

/** Presence TTL on the relay is 30s, so a 10s heartbeat keeps us from being evicted. */
const HEARTBEAT_INTERVAL_MS = 10_000
const RECONNECT_BASE_MS = 500
const RECONNECT_MAX_MS = 10_000
/**
 * How long to wait for the relay's welcome frame after sending hello. Without
 * this, a relay that accepts the WebSocket but never sends welcome (e.g. it
 * silently dropped the hello, or authenticated then stalled) leaves the UI
 * stuck on "Connecting…" forever — the socket stays open so no close event
 * fires. The timeout treats a missing welcome as a failed connection so the
 * status turns honest and the normal reconnect backoff applies.
 */
const WELCOME_TIMEOUT_MS = 8_000

function syncUrl(documentId: string): string {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:"
  return `${protocol}//${window.location.host}/sync/${encodeURIComponent(documentId)}`
}

/**
 * Imports peer bytes while flagging the import so the editor can classify the
 * resulting CodeMirror transaction as remote. Loro delivers the event (and the
 * editor dispatch it triggers) synchronously inside this call, so the flag is
 * reliably observed and always cleared again on the way out.
 */
function importRemote(doc: LoroDoc, bytes: Uint8Array): void {
  importingRemote = true
  try {
    doc.import(bytes)
  } finally {
    importingRemote = false
  }
}

/**
 * Connect a Loro replica to the Nisaba binary relay. This is intentionally a
 * small adapter: the server owns framing, while Loro owns CRDT update bytes.
 * A failed handshake/import is reported as unsupported, never as a successful
 * local-only connection.
 *
 * Bug N1 (CRDT duplication): the caller seeds the persisted body into both
 * CodeMirror and the replica before calling this (so the editor shows content
 * immediately and the loro-codemirror binding's init reconcile does not blank
 * it). That local seed carries a fresh peer id, so two clients seeding the same
 * body and both pushing would merge at the relay as concurrent inserts and
 * duplicate the text. The welcome handler guarantees a single authoritative
 * origin instead: the first client to reach an empty relay pushes its seed
 * (becoming the origin); any later client clears its local seed (driven through
 * CodeMirror via `onBeforeAdopt`, so the binding keeps CM and the replica in
 * sync) and adopts the relay's snapshot. The clear is rebaselined below
 * `syncFrom`, so it is never exported back to the relay — it only discards the
 * duplicate local seed.
 *
 * The connection is self-healing: a dropped socket reconnects with exponential
 * backoff, a heartbeat keeps the peer alive past the presence TTL, and after
 * every reconnect the replica re-imports server catch-up so it reconverges.
 */
export function connectSync(doc: LoroDoc, options: SyncOptions): SyncConnection {
  let socket: WebSocket | undefined
  let closed = false
  let unsupported = false
  let unsubscribe: (() => void) | undefined
  let reconnectTimer: ReturnType<typeof setTimeout> | undefined
  let heartbeatTimer: ReturnType<typeof setTimeout> | undefined
  let welcomeTimer: ReturnType<typeof setTimeout> | undefined
  let handshakeComplete = false
  // True only until the very FIRST welcome arrives, then set false and NEVER
  // reset — unlike `handshakeComplete`, which teardownSocket resets on every
  // socket close. Branching on this flag (not `!handshakeComplete`) lets a
  // reconnect take the merge path instead of re-entering the first-connection
  // clear+adopt path that would wipe the editor and lose edits typed during the
  // disconnect gap.
  let firstHandshake = true
  let retries = 0
  // The empty baseline: everything committed above it (the locally-seeded body)
  // is eligible to be exported to the relay. On the first welcome it is either
  // (a) used wholesale to push the seed as the origin, then advanced to the
  // post-push vector; or (b) discarded by clearing the seed and rebaselining to
  // the post-import vector (the relay is the origin). After that, live local
  // edits stream via subscribeLocalUpdates and only genuinely-new ops are sent.
  // A fresh empty vector is correct because the replica was created empty and
  // the seed is the only thing committed before connect.
  let syncFrom = new VersionVector(undefined)
  const status = (value: SyncStatus, detail?: string): void => options.onStatus?.(value, detail)

  const send = (data: Uint8Array): void => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(data as BufferSource)
  }

  const sendUpdate = (bytes: Uint8Array): void => {
    if (bytes.byteLength === 0) return
    send(encodeSyncFrame({ type: "update", bytes }))
  }

  const stopHeartbeat = (): void => {
    if (heartbeatTimer !== undefined) clearTimeout(heartbeatTimer)
    heartbeatTimer = undefined
  }

  const startHeartbeat = (): void => {
    stopHeartbeat()
    heartbeatTimer = setTimeout(() => {
      if (closed) return
      if (socket?.readyState === WebSocket.OPEN) send(encodeSyncFrame({ type: "heartbeat" }))
      startHeartbeat()
    }, HEARTBEAT_INTERVAL_MS)
  }

  const scheduleReconnect = (): void => {
    if (closed || unsupported) return
    clearTimeout(reconnectTimer)
    const backoff = Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** retries)
    const jittered = backoff * (0.8 + Math.random() * 0.4)
    retries += 1
    reconnectTimer = setTimeout(connect, jittered)
  }

  const stopWelcomeTimer = (): void => {
    if (welcomeTimer !== undefined) clearTimeout(welcomeTimer)
    welcomeTimer = undefined
  }

  const teardownSocket = (): void => {
    stopHeartbeat()
    stopWelcomeTimer()
    unsubscribe?.()
    unsubscribe = undefined
    handshakeComplete = false
  }

  const connect = (): void => {
    if (closed || unsupported) return
    status("connecting")
    socket = new WebSocket(syncUrl(options.documentId))
    socket.binaryType = "arraybuffer"
    socket.addEventListener("open", () => {
      if (closed || unsupported) {
        socket?.close()
        return
      }
      retries = 0
      try {
        socket?.send(encodeSyncFrame({
          type: "hello",
          proto: 1,
          documentId: options.documentId,
          peer: doc.peerId,
          token: options.token ?? "",
          lastVersionVector: doc.oplogVersion().encode()
        }) as BufferSource)
        startHeartbeat()
        // C2: if no welcome arrives within the deadline, the relay accepted the
        // socket but isn't handshaking — close and let the reconnect backoff retry
        // so the status turns honest instead of hanging on "Connecting…".
        stopWelcomeTimer()
        welcomeTimer = setTimeout(() => {
          if (closed || unsupported || handshakeComplete) return
          status("disconnected", "No response from the sync server; reconnecting…")
          socket?.close()
        }, WELCOME_TIMEOUT_MS)
      } catch (error) {
        unsupported = true
        status("unsupported", error instanceof Error ? error.message : "Unable to encode sync handshake")
        socket?.close()
      }
    })
    socket.addEventListener("message", (event) => {
      try {
        if (!(event.data instanceof ArrayBuffer) && !(event.data instanceof Uint8Array)) throw new Error("Sync server sent a text frame")
        const frame = decodeSyncFrame(event.data)
        if (frame.type === "welcome") {
          // Both WELCOME statuses are successful: 0 carries catch-up, 1 means already current.
          if (frame.status !== 0 && frame.status !== 1) throw new Error("Invalid sync welcome status")
          const relayHadContent = frame.catchup.type !== "none"
          if (firstHandshake) {
            firstHandshake = false
            handshakeComplete = true
            stopWelcomeTimer()
            if (relayHadContent) {
              // The relay owns the authoritative body. Import its snapshot into
              // the (empty) replica — no local seed to conflict with.
              importRemote(doc, frame.catchup.bytes)
              syncFrom = doc.oplogVersion()
              unsubscribe = doc.subscribeLocalUpdates(sendUpdate)
              options.onReady?.()
            } else if (options.seedBody !== undefined && options.seedBody.length > 0) {
              // Brand-new document: the relay is empty. Seed the replica NOW and
              // push it as the single authoritative origin.
              doc.getText("text").updateByLine(options.seedBody)
              doc.commit({ origin: "load" })
              sendUpdate(doc.export({ mode: "update", from: syncFrom }))
              syncFrom = doc.oplogVersion()
              unsubscribe = doc.subscribeLocalUpdates(sendUpdate)
              options.onReady?.()
            } else {
              // Empty relay, no seed: a genuinely new, empty document. Just stream.
              unsubscribe = doc.subscribeLocalUpdates(sendUpdate)
              options.onReady?.()
            }
          } else {
            // A reconnect welcome: the local replica still holds edits the user
            // typed during the disconnect gap. Do NOT call onBeforeAdopt (do NOT
            // clear the editor) — instead import the relay's catch-up so the CRDT
            // merges relay state with our local gap edits, preserving both.
            // Push our gap edits first so the relay and other peers converge too.
            handshakeComplete = true
            stopWelcomeTimer()
            sendUpdate(doc.export({ mode: "update", from: syncFrom }))
            if (relayHadContent) importRemote(doc, frame.catchup.bytes)
            syncFrom = doc.oplogVersion()
          }
          status("connected")
          return
        }
        if (frame.type === "update" || frame.type === "snapshot") {
          if (!handshakeComplete) throw new Error("Sync update arrived before welcome")
          importRemote(doc, frame.bytes)
          return
        }
        if (frame.type === "heartbeat") return
        if (frame.type === "error") throw new Error(`Sync server error ${frame.code}: ${frame.message}`)
      } catch (error) {
        // A protocol-level failure will not fix itself on retry; report and stop.
        unsupported = true
        teardownSocket()
        status("unsupported", error instanceof Error ? error.message : "Unsupported sync protocol")
        socket?.close()
      }
    })
    socket.addEventListener("close", () => {
      teardownSocket()
      if (closed || unsupported) return
      status("disconnected", "Reconnecting…")
      scheduleReconnect()
    })
    socket.addEventListener("error", () => {
      if (!closed && !unsupported) status("disconnected", "Sync connection failed")
    })
  }

  connect()

  return {
    close: () => {
      closed = true
      clearTimeout(reconnectTimer)
      teardownSocket()
      socket?.close()
    }
  }
}
