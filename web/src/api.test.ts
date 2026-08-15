import { beforeEach, describe, expect, it, vi } from "vitest"
import { Effect } from "effect"
import * as api from "./api"

const ok = (body: unknown): Response => new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } })

const project = {
  id: "p1",
  name: "Workspace",
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z"
}

const document = {
  id: "d1",
  project_id: "p1",
  path: "chapters/main.typ",
  title: "Main",
  body: "= Heading",
  data: { template: "article" },
  revision: 3,
  updated_at: "2026-01-01T00:00:00Z"
}

describe("app service contract", () => {
  beforeEach(() => {
    vi.unstubAllGlobals()
    vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => undefined, removeItem: () => undefined })
  })

  it("lists flat projects", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok([project])))
    const projects = await Effect.runPromise(api.listProjects())
    expect(projects[0]).toEqual(project)
    expect(fetch).toHaveBeenCalledWith("/api/projects", expect.anything())
  })

  it("lists documents directly under a project", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok([document])))
    const documents = await Effect.runPromise(api.listDocuments("p1"))
    expect(documents[0]).toMatchObject({ id: "d1", project_id: "p1", path: "chapters/main.typ", title: "Main" })
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/documents", expect.anything())
  })

  it("accepts redacted share-link list entries without a plaintext token", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok([{
      token_hash: "abc123", project_id: "p1", role: "reviewer", created_by: "demo",
      created_at: "2026-01-01T00:00:00Z", expires_at: null, label: null
    }])))
    const links = await Effect.runPromise(api.listShareLinks("p1"))
    expect(links[0]).toMatchObject({ project_id: "p1", role: "reviewer", token_hash: "abc123" })
    expect(links[0]?.token).toBeUndefined()
  })

  it("gets a document from the flat document route", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok(document)))
    expect((await Effect.runPromise(api.getDocument("p1", "d1"))).body).toBe("= Heading")
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/documents/d1", expect.anything())
  })

  it("creates a document with path, title, body, and data", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok(document)))
    await Effect.runPromise(api.createDocument("p1", { path: "chapters/main.typ", title: "Main", body: "= Heading", data: { template: "article" } }))
    const [url, request] = vi.mocked(fetch).mock.calls[0] ?? []
    expect(url).toBe("/api/projects/p1/documents")
    expect(request?.method).toBe("POST")
    expect(JSON.parse(String(request?.body))).toEqual({ path: "chapters/main.typ", title: "Main", body: "= Heading", data: { template: "article" } })
  })

  it("sends expected_revision on a flat document PATCH", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok({ ...document, body: "new", revision: 4 })))
    await Effect.runPromise(api.saveDocument("p1", "d1", "new", 3))
    const [url, request] = vi.mocked(fetch).mock.calls[0] ?? []
    expect(url).toBe("/api/projects/p1/documents/d1")
    expect(request?.method).toBe("PATCH")
    expect(JSON.parse(String(request?.body))).toEqual({ body: "new", expected_revision: 3 })
  })

  it("deletes a document from the flat document route", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(null, { status: 204 })))
    await Effect.runPromise(api.deleteDocument("p1", "d1"))
    expect(fetch).toHaveBeenCalledWith("/api/projects/p1/documents/d1", expect.objectContaining({ method: "DELETE" }))
  })

  it("uploads full text as base64 JSON via PUT", async () => {
    vi.stubGlobal("btoa", (value: string) => Buffer.from(value, "binary").toString("base64"))
    vi.stubGlobal("fetch", vi.fn(async () => ok({
      reference_id: "r1", blob_ref: "fulltext/r1", filename: "a.pdf", content_type: "application/pdf",
      size_bytes: 5, checksum_sha256: null, uploaded_at: "2026-01-01T00:00:00Z"
    })))
    const bytes = Uint8Array.from([0x25, 0x50, 0x44, 0x46, 0x2d])
    const file = { name: "a.pdf", arrayBuffer: async () => bytes.buffer } as unknown as File
    await Effect.runPromise(api.uploadFulltext("p1", "r1", file))
    const [url, request] = vi.mocked(fetch).mock.calls[0] ?? []
    expect(url).toBe("/api/projects/p1/references/r1/fulltext")
    expect(request?.method).toBe("PUT")
    expect(JSON.parse(String(request?.body))).toEqual({
      filename: "a.pdf", content_type: "application/pdf", size_bytes: 5, contents_base64: "JVBERi0="
    })
  })

  it("posts the generic project export request", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ok({
      compile: { pdf_base64: null, span_map: [], diagnostics: [], outline: [], build_id: "b1" },
      references: { files: [] }, zip_base64: null, zip_filename: null
    })))
    const result = await Effect.runPromise(api.exportProject("p1", "chapters/main.typ", "proposed"))
    expect(result.compile.build_id).toBe("b1")
    const [url, request] = vi.mocked(fetch).mock.calls[0] ?? []
    expect(url).toBe("/api/projects/p1/exports")
    expect(JSON.parse(String(request?.body))).toEqual({ entry: "chapters/main.typ", view: "proposed" })
  })

  it("compiles and attaches the bearer token", async () => {
    vi.stubGlobal("localStorage", {
      getItem: () => JSON.stringify({ accessToken: "token-1" }), setItem: () => undefined, removeItem: () => undefined
    })
    vi.stubGlobal("fetch", vi.fn(async () => ok({
      pdf_base64: "JVBERg==", span_map: [], diagnostics: [], outline: [], build_id: "b2"
    })))
    await Effect.runPromise(api.compile({ projectId: "p1", entry: "main.typ", sources: { "main.typ": "= Hi" } }))
    const [url, request] = vi.mocked(fetch).mock.calls[0] ?? []
    expect(url).toBe("/api/compile")
    expect(new Headers(request?.headers).get("authorization")).toBe("Bearer token-1")
  })

  it("surfaces structured API errors", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(
      JSON.stringify({ error: { code: "conflict", message: "revision conflict" } }), { status: 409 }
    )))
    const failure = await Effect.runPromise(Effect.flip(api.getDocument("p1", "d1")))
    expect(failure.status).toBe(409)
    expect(failure.message).toBe("revision conflict")
  })

  it("surfaces an AbortError as an ApiError", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { const error = new Error("aborted"); error.name = "AbortError"; throw error }))
    const failure = await Effect.runPromise(Effect.flip(api.listProjects()))
    expect(failure.message).toMatch(/timed out/i)
  })
})
