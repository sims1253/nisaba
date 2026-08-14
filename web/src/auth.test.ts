import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import { Effect } from "effect"
import {
  type AuthTokenService,
  createPkceAuthorizationRequest,
  createOidcClient,
  currentUserDisplayName,
  readStoredToken,
  refreshAccessToken,
  scheduleTokenRefresh
} from "./auth"

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
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, val: string) => { store[key] = val },
      removeItem: (key: string) => { delete store[key] },
    })
    expect(currentUserDisplayName()).toBe("alice")
  })

  it("returns anonymous for a non-JWT token", () => {
    const store: Record<string, string> = { "nisaba.auth.token": JSON.stringify({ accessToken: "opaque-token-no-dots" }) }
    vi.stubGlobal("localStorage", {
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
    // The pending OIDC state lives in sessionStorage; the token in localStorage.
    vi.stubGlobal("sessionStorage", {
      getItem: (key: string) => store[key] ?? null,
      setItem: (key: string, val: string) => { store[key] = val },
      removeItem: (key: string) => { delete store[key] },
    })
    vi.stubGlobal("localStorage", {
      getItem: () => null, setItem: () => undefined, removeItem: () => undefined,
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
    vi.stubGlobal("localStorage", {
      getItem: () => null, setItem: () => undefined, removeItem: () => undefined,
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

describe("refreshAccessToken", () => {
  // The jsdom environment used by this suite has no real localStorage (Bun's
  // node-compat leaves the global undefined), so both storages are stubbed with
  // in-memory maps — the same approach as the api.test.ts suite.
  const localStore: Record<string, string> = {}
  const sessionStore: Record<string, string> = {}
  const memoryStorage = (store: Record<string, string>): Storage => ({
    getItem: (key: string) => store[key] ?? null,
    setItem: (key: string, val: string) => { store[key] = val },
    removeItem: (key: string) => { delete store[key] },
    clear: () => { for (const key of Object.keys(store)) delete store[key] },
    key: (index: number) => Object.keys(store)[index] ?? null,
    get length() { return Object.keys(store).length }
  })

  beforeEach(() => {
    vi.stubEnv("VITE_OIDC_ISSUER", "https://id.example/realms/nisaba")
    vi.stubEnv("VITE_OIDC_CLIENT_ID", "web")
    vi.stubGlobal("localStorage", memoryStorage(localStore))
    vi.stubGlobal("sessionStorage", memoryStorage(sessionStore))
  })

  afterEach(() => {
    vi.unstubAllEnvs()
    vi.unstubAllGlobals()
    vi.useRealTimers()
    for (const key of Object.keys(localStore)) delete localStore[key]
    for (const key of Object.keys(sessionStore)) delete sessionStore[key]
  })

  /**
   * Seeds a stored token the way a completed sign-in would: localStorage (the
   * channel every read path uses), never sessionStorage.
   */
  function seedStoredToken(): void {
    localStore["nisaba.auth.token"] = JSON.stringify({
      accessToken: "old-access",
      refreshToken: "old-refresh",
      expiresAt: Date.now() + 120_000
    })
  }

  it("persists the refreshed token to localStorage (the channel reads use)", async () => {
    seedStoredToken()
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      access_token: "new-access",
      expires_in: 300,
      token_type: "Bearer",
      refresh_token: "new-refresh"
    }), { status: 200, headers: { "content-type": "application/json" } }))
    vi.stubGlobal("fetch", fetchMock)

    const token = await refreshAccessToken()

    expect(token?.accessToken).toBe("new-access")
    expect(token?.refreshToken).toBe("new-refresh")
    // The refreshed token must be readable through the normal read path: it
    // lives in localStorage under the shared key (the original bug wrote it to
    // sessionStorage, so background refresh was a no-op and sessions died at
    // token expiry).
    const stored = readStoredToken()
    expect(stored?.accessToken).toBe("new-access")
    expect(stored?.refreshToken).toBe("new-refresh")
    expect(sessionStore["nisaba.auth.token"]).toBeUndefined()
    // The refresh_token grant carried the refresh token over the wire.
    const [, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit]
    expect(String(init.body)).toContain("grant_type=refresh_token")
    expect(String(init.body)).toContain("refresh_token=old-refresh")
  })

  it("keeps the previous refresh token when the IdP does not rotate it", async () => {
    seedStoredToken()
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({
      access_token: "new-access",
      expires_in: 300
    }), { status: 200, headers: { "content-type": "application/json" } })))

    const token = await refreshAccessToken()

    expect(token?.refreshToken).toBe("old-refresh")
  })

  it("returns undefined and drops the stored token on a failed refresh", async () => {
    seedStoredToken()
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 400 })))

    const token = await refreshAccessToken()

    expect(token).toBeUndefined()
    expect(localStore["nisaba.auth.token"]).toBeUndefined()
  })

  it("re-arms the refresh schedule after a successful refresh", async () => {
    vi.useFakeTimers()
    seedStoredToken()
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({
      access_token: "rotated-access",
      expires_in: 300,
      refresh_token: "rotated-refresh"
    }), { status: 200, headers: { "content-type": "application/json" } }))
    vi.stubGlobal("fetch", fetchMock)

    // The seeded token expires 120s from now, so the first refresh fires 60s in.
    scheduleTokenRefresh()
    await vi.advanceTimersByTimeAsync(60_000)
    expect(fetchMock).toHaveBeenCalledTimes(1)

    // The refreshed token expires 300s after the first refresh, i.e. another
    // 240s after the 60s-early mark. If the schedule re-arms (rather than being
    // one-shot at sign-in), a second refresh fires then.
    await vi.advanceTimersByTimeAsync(240_000)
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(readStoredToken()?.accessToken).toBe("rotated-access")
  })
})
