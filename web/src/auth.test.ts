import { beforeEach, describe, expect, it, vi } from "vitest"
import { Effect } from "effect"
import { type AuthTokenService, createPkceAuthorizationRequest, createOidcClient, currentUserDisplayName } from "./auth"

describe("OIDC public client", () => {
  it("creates a stateful S256 PKCE request without a client secret", async () => {
    vi.stubGlobal("crypto", { getRandomValues: (bytes: Uint8Array) => { bytes.fill(7); return bytes }, subtle: { digest: async () => Uint8Array.from([1, 2, 3]).buffer } })
    const request = await createPkceAuthorizationRequest({ issuer: "https://id.example/realms/nisaba", clientId: "web", redirectUri: "https://app/callback", scope: "openid" })
    const url = new URL(request.url)
    expect(url.searchParams.get("client_id")).toBe("web")
    expect(url.searchParams.get("code_challenge_method")).toBe("S256")
    expect(url.searchParams.get("code_challenge")).toBe("AQID")
    expect(url.searchParams.has("client_secret")).toBe(false)
    expect(request.state).toBeTruthy()
  })
})

describe("currentUserDisplayName", () => {
  beforeEach(() => {
    vi.stubGlobal("atob", (value: string) => Buffer.from(value, "base64").toString("binary"))
  })

  it("decodes a JWT with base64url payload (containing '-' or '_')", () => {
    // Construct a JWT whose base64url payload actually contains URL-safe chars
    // (- or _) that atob() cannot decode without conversion. The "?" characters
    // (0x3F) in the sub value force the base64 encoding to produce "_" chars.
    const payload = JSON.stringify({ preferred_username: "alice", sub: "?????????????????", email: "a@b.c" })
    const b64url = Buffer.from(payload, "utf-8").toString("base64url")
    // Sanity: the payload really does use base64url-only chars (otherwise the
    // test is vacuous — standard base64 and base64url would agree).
    expect(b64url).toMatch(/[_-]/)
    const jwt = `header.${b64url}.signature`
    const store: Record<string, string> = { "nisaba.auth.token": JSON.stringify({ accessToken: jwt }) }
    vi.stubGlobal("sessionStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, val: string) => { store[key] = val },
      removeItem: (key: string) => { delete store[key] },
    })
    expect(currentUserDisplayName()).toBe("alice")
  })

  it("returns anonymous for a non-JWT token", () => {
    const store: Record<string, string> = { "nisaba.auth.token": JSON.stringify({ accessToken: "opaque-token-no-dots" }) }
    vi.stubGlobal("sessionStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, val: string) => { store[key] = val },
      removeItem: (key: string) => { delete store[key] },
    })
    expect(currentUserDisplayName()).toBe("anonymous")
  })
})

describe("OIDC completeCallback cleanup", () => {
  const config = {
    issuer: "https://id.example/realms/nisaba",
    clientId: "web",
    redirectUri: "https://app/callback",
    scope: "openid",
  }

  beforeEach(() => {
    vi.stubGlobal("atob", (value: string) => Buffer.from(value, "base64").toString("binary"))
  })

  it("clears pending PKCE state on a state mismatch so the app can start a fresh login", async () => {
    const store: Record<string, string> = {
      "nisaba.oidc.pending": JSON.stringify({ state: "expected-state", codeVerifier: "verifier" }),
    }
    vi.stubGlobal("sessionStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, val: string) => { store[key] = val },
      removeItem: (key: string) => { delete store[key] },
    })
    const noopStore: AuthTokenService = {
      get: () => Effect.succeed(undefined),
      set: () => Effect.succeed(undefined as void),
      clear: () => Effect.succeed(undefined as void),
    }
    const client = createOidcClient(config, noopStore)
    const failure = await Effect.runPromise(Effect.flip(
      client.completeCallback("https://app/callback?code=abc&state=WRONG")
    ))
    expect(failure.message).toMatch(/state does not match/i)
    // The pending PKCE state must be gone so isOidcCallback() returns false.
    expect(store["nisaba.oidc.pending"]).toBeUndefined()
  })

  it("surfaces IdP error responses instead of treating them as incomplete", async () => {
    const store: Record<string, string> = {
      "nisaba.oidc.pending": JSON.stringify({ state: "s", codeVerifier: "v" }),
    }
    vi.stubGlobal("sessionStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, val: string) => { store[key] = val },
      removeItem: (key: string) => { delete store[key] },
    })
    const noopStore: AuthTokenService = {
      get: () => Effect.succeed(undefined),
      set: () => Effect.succeed(undefined as void),
      clear: () => Effect.succeed(undefined as void),
    }
    const client = createOidcClient(config, noopStore)
    const failure = await Effect.runPromise(Effect.flip(
      client.completeCallback("https://app/callback?error=access_denied&error_description=User+cancelled")
    ))
    expect(failure.message).toMatch(/access_denied/i)
    expect(store["nisaba.oidc.pending"]).toBeUndefined()
  })
})
