/**
 * Typed client for the Nisaba app service.
 *
 * The shapes here mirror `services/app/src/lib.rs` exactly, including its
 * snake_case wire names and its flat project → document resource model.
 * References and fulltext remain project-scoped resources.
 *
 * nginx (and the Vite dev proxy) strip the `/api` prefix before forwarding, so
 * `/api/projects` reaches the app as `/projects`. `/api/compile` is the one route
 * the app serves under that prefix itself, and is forwarded verbatim.
 *
 * Every response is decoded through an Effect schema, so a contract drift surfaces
 * as a typed `ApiError` at the call site instead of `[object Object]` in the DOM.
 */
import { Data, Effect, Schema } from "effect"
import { handleAuthFailure, readStoredAccessToken } from "./auth"

export class ApiError extends Data.TaggedError("ApiError")<{ readonly message: string; readonly status?: number }> {}

// ---------------------------------------------------------------------------
// Wire schemas (snake_case, mirroring the Rust serde representation)
// ---------------------------------------------------------------------------

const Project = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  created_at: Schema.String,
  updated_at: Schema.String
})
export type Project = typeof Project.Type

// Matches services/app `MembershipRole` (serde rename_all = "kebab-case"). The
// role is project-scoped (from membership), distinct from the IdP roles in the
// JWT: it's what the client uses to gate reviewer UX (suggesting lock, export).
export type MembershipRole = "owner" | "author" | "reviewer" | "read-only"
const Membership = Schema.Struct({
  project_id: Schema.String,
  subject: Schema.String,
  // String on the wire (kebab-case enum); narrowed to MembershipRole at the
  // call site, where the client already knows the closed set.
  role: Schema.String,
  created_at: Schema.String
})
export type Membership = Omit<typeof Membership.Type, "role"> & { role: MembershipRole }

const NisabaDocument = Schema.Struct({
  id: Schema.String,
  project_id: Schema.String,
  path: Schema.String,
  title: Schema.String,
  body: Schema.String,
  data: Schema.Record(Schema.String, Schema.String),
  revision: Schema.Number,
  updated_at: Schema.String
})
export type NisabaDocument = typeof NisabaDocument.Type

const ReferenceMetadata = Schema.Struct({
  title: Schema.String,
  authors: Schema.Array(Schema.String),
  year: Schema.NullOr(Schema.Number),
  doi: Schema.NullOr(Schema.String),
  pmid: Schema.NullOr(Schema.String),
  journal: Schema.NullOr(Schema.String),
  extra: Schema.Record(Schema.String, Schema.String)
})
export type ReferenceMetadata = typeof ReferenceMetadata.Type

const Reference = Schema.Struct({
  id: Schema.String,
  project_id: Schema.String,
  metadata: ReferenceMetadata,
  created_at: Schema.String,
  updated_at: Schema.String
})
export type Reference = typeof Reference.Type

const Fulltext = Schema.Struct({
  reference_id: Schema.String,
  blob_ref: Schema.String,
  filename: Schema.String,
  content_type: Schema.String,
  size_bytes: Schema.Number,
  checksum_sha256: Schema.NullOr(Schema.String),
  uploaded_at: Schema.String
})
export type Fulltext = typeof Fulltext.Type

const CompileResponse = Schema.Struct({
  pdf_base64: Schema.NullOr(Schema.String),
  frames: Schema.Array(Schema.Unknown),
  span_map: Schema.Array(Schema.Unknown),
  diagnostics: Schema.Array(Schema.Unknown),
  outline: Schema.Array(Schema.Unknown),
  build_id: Schema.String
})
export type CompileResponse = typeof CompileResponse.Type

/** `POST /projects/{id}/exports` returns the compiled document *and* the reference bundle. */
const ExportResponse = Schema.Struct({
  compile: CompileResponse,
  references: Schema.Struct({
    files: Schema.Array(Schema.Struct({ path: Schema.String, content_base64: Schema.String }))
  }),
  // The export bundle (PDF, per-document RIS, fulltext tree) and its filename.
  // Absent when export is blocked.
  zip_base64: Schema.NullOr(Schema.String),
  zip_filename: Schema.NullOr(Schema.String)
})
export type ExportResponse = typeof ExportResponse.Type

export type CompileMode = "document" | "full"
export type CompileView = "baseline" | "proposed" | "redline" | "public"

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

const path = (...segments: readonly string[]): string => `/api/${segments.map(encodeURIComponent).join("/")}`

/**
 * One request helper for the whole client.
 *
 * The stored OIDC access token is attached to every call: the app authorises each
 * route, so an unauthenticated request is a 401, not an anonymous read.
 */
type Decode<T> = (value: unknown) => T

/** Turns a schema into a decoder, keeping the generic plumbing in one place. */
const decoder = <S extends Schema.Top & { readonly DecodingServices: never }>(schema: S): Decode<S["Type"]> =>
  Schema.decodeUnknownSync(schema)

/** How long a single API request may run before it is aborted. A hung server
 *  (e.g. a compile service stuck on an infinite-loop document) must never leave
 *  the UI pinned in a loading state forever: the timeout lets a request fail so
 *  the user can retry instead of reloading. */
const REQUEST_TIMEOUT_MS = 30_000

function request<T>(url: string, decode: Decode<T> | undefined, init: RequestInit = {}): Effect.Effect<T, ApiError> {
  return Effect.tryPromise({
    try: async () => {
      // AbortController + timer are created inside the try callback so the
      // countdown only starts when the effect actually executes (not when it is
      // merely constructed).
      const controller = new AbortController()
      const timeoutId = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS)
      try {
        const headers = new Headers(init.headers)
        if (init.body !== undefined && !headers.has("content-type")) headers.set("content-type", "application/json")
        const token = readStoredAccessToken()
        if (token) headers.set("authorization", `Bearer ${token}`)
        const response = await fetch(url, { ...init, headers, signal: controller.signal })
        if (response.status === 401) handleAuthFailure()
        if (!response.ok) {
          throw new ApiError({ message: await errorMessage(response), status: response.status })
        }
        if (decode === undefined || response.status === 204) return undefined as T
        return decode(await response.json())
      } finally {
        clearTimeout(timeoutId)
      }
    },
    catch: (error) => {
      if (error instanceof ApiError) return error
      // fetch rejects with an AbortError when the AbortController fires. Surface
      // it as a real ApiError so the UI can recover instead of hanging forever.
      if (error instanceof Error && error.name === "AbortError") {
        return new ApiError({ message: `The request timed out after ${REQUEST_TIMEOUT_MS / 1000} seconds` })
      }
      return new ApiError({ message: error instanceof Error ? error.message : "The API request failed" })
    }
  })
}

/**
 * Surfaces the app's own error text so the UI shows a real reason.
 *
 * The app answers `{"error": {"code", "message"}}` for every failure.
 */
const ErrorBody = Schema.Struct({ error: Schema.Struct({ code: Schema.String, message: Schema.String }) })

async function errorMessage(response: Response): Promise<string> {
  const fallback = `The API returned HTTP ${response.status}`
  try {
    const body = await response.text()
    if (!body) return fallback
    return Schema.decodeUnknownSync(ErrorBody)(JSON.parse(body)).error.message
  } catch {
    return fallback
  }
}

const json = (body: unknown): RequestInit => ({ method: "POST", body: JSON.stringify(body) })
const patch = (body: unknown): RequestInit => ({ method: "PATCH", body: JSON.stringify(body) })

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

export const listProjects = (): Effect.Effect<readonly Project[], ApiError> =>
  request(path("projects"), decoder(Schema.Array(Project)))

export const createProject = (name: string): Effect.Effect<Project, ApiError> =>
  request(path("projects"), decoder(Project), json({ name }))

export const renameProject = (projectId: string, name: string): Effect.Effect<Project, ApiError> =>
  request(path("projects", projectId), decoder(Project), patch({ name }))

export const deleteProject = (projectId: string): Effect.Effect<void, ApiError> =>
  request(path("projects", projectId), undefined, { method: "DELETE" })

// Returns the caller's OWN membership (role) for the project. Any member can
// read their own role; non-members get 403. Drives the reviewer UX gates (H1/M4).
export const getMembership = (projectId: string): Effect.Effect<Membership, ApiError> =>
  request(path("projects", projectId, "membership"), decoder(Membership)).pipe(
    // The wire role is a kebab-case string; narrow to the known union. An
    // unexpected value falls back to "read-only" (least privilege) so a future
    // server role never accidentally grants a reviewer author powers.
    Effect.map((m) => ({ ...m, role: (["owner", "author", "reviewer", "read-only"] as const).includes(m.role as MembershipRole) ? (m.role as MembershipRole) : "read-only" }))
  )

// Manage project membership (Share/Invite UI, L3). Both routes require the
// caller to have Manage permission (author/owner), enforced server-side.
export const listMembers = (projectId: string): Effect.Effect<readonly Membership[], ApiError> =>
  request(path("projects", projectId, "members"), decoder(Schema.Array(Membership))).pipe(
    Effect.map((members) => members.map((m) => ({
      ...m,
      role: (["owner", "author", "reviewer", "read-only"] as const).includes(m.role as MembershipRole) ? (m.role as MembershipRole) : "read-only"
    })))
  )

export const addMember = (projectId: string, subject: string, role: MembershipRole): Effect.Effect<Membership, ApiError> =>
  request(path("projects", projectId, "members"), decoder(Membership), json({ subject, role })).pipe(
    Effect.map((m) => ({
      ...m,
      role: (["owner", "author", "reviewer", "read-only"] as const).includes(m.role as MembershipRole) ? (m.role as MembershipRole) : "read-only"
    }))
  )

export const listDocuments = (projectId: string): Effect.Effect<readonly NisabaDocument[], ApiError> =>
  request(path("projects", projectId, "documents"), decoder(Schema.Array(NisabaDocument)))

const documentPath = (projectId: string, documentId?: string): string =>
  documentId === undefined
    ? path("projects", projectId, "documents")
    : path("projects", projectId, "documents", documentId)

export const getDocument = (projectId: string, documentId: string): Effect.Effect<NisabaDocument, ApiError> =>
  request(documentPath(projectId, documentId), decoder(NisabaDocument))

export const createDocument = (
  projectId: string,
  input: { readonly path: string; readonly title: string; readonly body?: string; readonly data?: Readonly<Record<string, string>> }
): Effect.Effect<NisabaDocument, ApiError> =>
  request(
    documentPath(projectId),
    decoder(NisabaDocument),
    json({ path: input.path, title: input.title, body: input.body ?? "", data: input.data ?? {} })
  )

/** Saves document fields conditionally when `expectedRevision` is provided. */
export const updateDocument = (
  projectId: string,
  documentId: string,
  input: {
    readonly path?: string
    readonly title?: string
    readonly body?: string
    readonly data?: Readonly<Record<string, string>>
    readonly expectedRevision?: number
  }
): Effect.Effect<NisabaDocument, ApiError> => {
  const { expectedRevision, ...fields } = input
  return request(
    documentPath(projectId, documentId),
    decoder(NisabaDocument),
    patch(expectedRevision === undefined ? fields : { ...fields, expected_revision: expectedRevision })
  )
}

export const saveDocument = (
  projectId: string,
  documentId: string,
  body: string,
  expectedRevision?: number
): Effect.Effect<NisabaDocument, ApiError> =>
  updateDocument(projectId, documentId, { body, expectedRevision })

export const deleteDocument = (projectId: string, documentId: string): Effect.Effect<void, ApiError> =>
  request(documentPath(projectId, documentId), undefined, { method: "DELETE" })

export const listReferences = (projectId: string): Effect.Effect<readonly Reference[], ApiError> =>
  request(path("projects", projectId, "references"), decoder(Schema.Array(Reference)))

export const createReference = (
  projectId: string,
  metadata: Partial<ReferenceMetadata> & { readonly title: string }
): Effect.Effect<Reference, ApiError> =>
  request(
    path("projects", projectId, "references"),
    decoder(Reference),
    json({
      metadata: {
        title: metadata.title,
        authors: metadata.authors ?? [],
        year: metadata.year ?? null,
        doi: metadata.doi ?? null,
        pmid: metadata.pmid ?? null,
        journal: metadata.journal ?? null,
        extra: metadata.extra ?? {}
      },
      provenance: null
    })
  )

export const deleteReference = (projectId: string, referenceId: string): Effect.Effect<void, ApiError> =>
  request(path("projects", projectId, "references", referenceId), undefined, { method: "DELETE" })

/** Full-text metadata for the whole project, so the library renders in one round trip. */
export const listFulltexts = (projectId: string): Effect.Effect<readonly Fulltext[], ApiError> =>
  request(path("projects", projectId, "fulltexts"), decoder(Schema.Array(Fulltext)))

/**
 * Attaches a PDF. The app takes base64 in a JSON body (not multipart) and validates
 * the declared size, media type, `%PDF-` header and checksum against the bytes.
 */
export const uploadFulltext = (projectId: string, referenceId: string, file: File): Effect.Effect<Fulltext, ApiError> =>
  Effect.tryPromise({
    try: () => file.arrayBuffer(),
    catch: () => new ApiError({ message: "The selected file could not be read" })
  }).pipe(
    Effect.flatMap((buffer) =>
      request(
        path("projects", projectId, "references", referenceId, "fulltext"),
        decoder(Fulltext),
        {
          method: "PUT",
          body: JSON.stringify({
            filename: file.name,
            content_type: "application/pdf",
            size_bytes: buffer.byteLength,
            contents_base64: base64FromBytes(new Uint8Array(buffer))
          })
        }
      )
    )
  )

export const deleteFulltext = (projectId: string, referenceId: string): Effect.Effect<void, ApiError> =>
  request(path("projects", projectId, "references", referenceId, "fulltext"), undefined, { method: "DELETE" })

export const exportProject = (
  projectId: string,
  entry: string,
  mode: CompileMode = "full",
  view: CompileView = "proposed"
): Effect.Effect<ExportResponse, ApiError> =>
  request(path("projects", projectId, "exports"), decoder(ExportResponse), json({ entry, mode, view }))

/**
 * Compiles sources to a PDF.
 *
 * Marks travel with the request: the app applies the projection for `view` server-side
 * and forwards only the projected text to the compile service, which never sees marks.
 */
export const compile = (input: {
  readonly projectId: string
  readonly entry: string
  readonly sources: Readonly<Record<string, string>>
  readonly marks?: Readonly<Record<string, readonly MarkInput[]>>
  readonly mode?: CompileMode
  readonly view?: CompileView
}): Effect.Effect<CompileResponse, ApiError> =>
  request(
    "/api/compile",
    decoder(CompileResponse),
    json({
      project_id: input.projectId,
      entry: input.entry,
      sources: input.sources,
      marks: input.marks ?? {},
      mode: input.mode ?? "document",
      view: input.view ?? "proposed"
    })
  )

export interface MarkInput {
  readonly id?: number
  readonly start: number
  readonly end: number
  readonly kind: "insert" | "delete" | "comment" | "secret"
  readonly author: string
  readonly timestamp: number
}

// ---------------------------------------------------------------------------
// Document history (version snapshots for diffs)
// ---------------------------------------------------------------------------

const DocumentRevision = Schema.Struct({
  id: Schema.String,
  document_id: Schema.String,
  project_id: Schema.String,
  body: Schema.String,
  revision: Schema.Number,
  author: Schema.NullOr(Schema.String),
  created_at: Schema.String
})
export type DocumentRevision = typeof DocumentRevision.Type

export const listDocumentHistory = (
  projectId: string,
  documentId: string
): Effect.Effect<readonly DocumentRevision[], ApiError> =>
  request(
    path("projects", projectId, "documents", documentId, "history"),
    decoder(Schema.Array(DocumentRevision))
  )

export const getDocumentRevision = (
  projectId: string,
  documentId: string,
  revisionId: string
): Effect.Effect<DocumentRevision, ApiError> =>
  request(
    path("projects", projectId, "documents", documentId, "history", revisionId),
    decoder(DocumentRevision)
  )

// ---------------------------------------------------------------------------
// Shareable links
// ---------------------------------------------------------------------------

/** The role a share link grants on redemption. A closed subset of
 *  {@link MembershipRole} (an "owner" link cannot be minted). Mirrors the CHECK
 *  constraint in migrations/0003_history_and_sharing.sql and the roles the
 *  redeem handler accepts, so the client never persists a link that can never be
 *  redeemed. */
export type ShareLinkRole = "author" | "reviewer" | "read-only"
const SHARE_LINK_ROLES: readonly ShareLinkRole[] = ["author", "reviewer", "read-only"]
const asShareLinkRole = (raw: string): ShareLinkRole =>
  (SHARE_LINK_ROLES as readonly string[]).includes(raw) ? (raw as ShareLinkRole) : "read-only"

const ShareLink = Schema.Struct({
  token: Schema.String,
  project_id: Schema.String,
  role: Schema.String,
  created_by: Schema.String,
  created_at: Schema.String,
  expires_at: Schema.NullOr(Schema.String),
  label: Schema.NullOr(Schema.String)
})
export type ShareLink = Omit<typeof ShareLink.Type, "role"> & { role: ShareLinkRole }

export const listShareLinks = (projectId: string): Effect.Effect<readonly ShareLink[], ApiError> =>
  request(path("projects", projectId, "share-links"), decoder(Schema.Array(ShareLink))).pipe(
    // The wire role is an unconstrained string; narrow to the known union. An
    // unexpected value falls back to "read-only" (least privilege) so a stale or
    // tampered link never advertises more than view access to the UI.
    Effect.map((links) => links.map((link) => ({ ...link, role: asShareLinkRole(link.role) })))
  )

export const createShareLink = (
  projectId: string,
  role: string,
  label?: string
): Effect.Effect<ShareLink, ApiError> =>
  // Validate before sending: the backend's in-memory store (tests / local dev)
  // accepts any role string and would mint a link the redeem handler then
  // rejects as "invalid share link role". Failing at the client boundary surfaces
  // the mistake instead of producing a silently-broken, unredeemable link.
  (SHARE_LINK_ROLES as readonly string[]).includes(role)
    ? request(path("projects", projectId, "share-links"), decoder(ShareLink), json({ role, label: label ?? null })).pipe(
        Effect.map((link) => ({ ...link, role: asShareLinkRole(link.role) }))
      )
    : Effect.fail(new ApiError({ message: `Invalid share-link role: ${role}` }))

export const deleteShareLink = (projectId: string, token: string): Effect.Effect<void, ApiError> =>
  request(path("projects", projectId, "share-links", token), undefined, { method: "DELETE" })

/** Redeems a share token: adds the caller as a member and returns the project ID. */
export const redeemShareLink = (token: string): Effect.Effect<{ readonly project_id: string }, ApiError> =>
  request(path("share", token, "redeem"), decoder(Schema.Struct({ project_id: Schema.String })), { method: "POST" })

// ---------------------------------------------------------------------------
// base64 helpers
// ---------------------------------------------------------------------------

/** Chunked so a large PDF does not blow the argument limit of `String.fromCharCode`. */
export function base64FromBytes(bytes: Uint8Array): string {
  let binary = ""
  const chunk = 0x8000
  for (let index = 0; index < bytes.length; index += chunk) {
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk))
  }
  return btoa(binary)
}
