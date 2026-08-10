import { Context, Data, Effect, Layer } from "effect"

export class AuthError extends Data.TaggedError("AuthError")<{ readonly message: string }> {}

export interface OidcConfig {
  readonly issuer: string
  readonly clientId: string
  readonly redirectUri: string
  readonly scope: string
  readonly authorizationEndpoint?: string
  readonly tokenEndpoint?: string
}

export interface PkceAuthorizationRequest {
  readonly url: string
  readonly state: string
  readonly codeVerifier: string
}

function randomUrlSafe(length = 32): string {
  const bytes = new Uint8Array(length)
  crypto.getRandomValues(bytes)
  return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "")
}

function base64Url(bytes: ArrayBuffer): string {
  return btoa(String.fromCharCode(...new Uint8Array(bytes))).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "")
}

/** The browser-side OIDC client is public: it uses PKCE and never accepts a client secret. */
export async function createPkceAuthorizationRequest(config: OidcConfig): Promise<PkceAuthorizationRequest> {
  const codeVerifier = randomUrlSafe(48)
  const state = randomUrlSafe(24)
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(codeVerifier))
  const endpoint = config.authorizationEndpoint ?? `${config.issuer.replace(/\/$/, "")}/protocol/openid-connect/auth`
  const params = new URLSearchParams({
    response_type: "code",
    client_id: config.clientId,
    redirect_uri: config.redirectUri,
    scope: config.scope,
    state,
    code_challenge: base64Url(digest),
    code_challenge_method: "S256"
  })
  return { url: `${endpoint}?${params}`, state, codeVerifier }
}

export interface AuthToken {
  readonly accessToken: string
  readonly expiresAt?: number
  readonly tokenType?: string
  readonly refreshToken?: string
}

export interface AuthTokenService {
  readonly get: () => Effect.Effect<AuthToken | undefined, AuthError>
  readonly set: (token: AuthToken) => Effect.Effect<void, AuthError>
  readonly clear: () => Effect.Effect<void, AuthError>
}

export const AuthTokenService = Context.Service<AuthTokenService>("AuthTokenService")
const storageKey = "nisaba.auth.token"
const pendingKey = "nisaba.oidc.pending"

// The access token lives in localStorage (NOT sessionStorage): sessionStorage
// is per-tab, so a second tab/window of the collaborative editor signed the
// user out (found by the 2026-08-09 author-agent's two-tab sync test). Tabs
// share the token, the refresh timer, and the session. The token is
// short-lived (5 minutes in dev) and refreshed in the background; the OIDC
// PKCE pending state stays in sessionStorage (transient per-login data).
function storage(): Storage | undefined {
  try { return globalThis.localStorage } catch { return undefined }
}

export function readStoredAccessToken(): string | undefined {
  return readStoredToken()?.accessToken
}

/** The full stored token, or `undefined` when absent, malformed, or expired. */
export function readStoredToken(): AuthToken | undefined {
  try {
    const raw = storage()?.getItem(storageKey)
    if (!raw) return undefined
    const token = JSON.parse(raw) as AuthToken
    if (typeof token.accessToken !== "string") return undefined
    if (isTokenExpired(token)) {
      clearStoredToken()
      return undefined
    }
    return token
  } catch { return undefined }
}

/** A token without an `expiresAt` (unexpected but tolerated) is never "expired". */
export function isTokenExpired(token: AuthToken): boolean {
  return token.expiresAt !== undefined && Date.now() >= token.expiresAt
}

export function clearStoredToken(): void {
  try { storage()?.removeItem(storageKey) } catch { /* storage unavailable */ }
}

type AuthFailureListener = () => void
const authFailureListeners = new Set<AuthFailureListener>()

/** Registers a 401 handler. Returns an unsubscribe function. */
export function onAuthFailure(listener: AuthFailureListener): () => void {
  authFailureListeners.add(listener)
  return () => { authFailureListeners.delete(listener) }
}

/**
 * Central 401 handling: the stored token is no longer valid, so it is dropped and
 * every registered listener (the UI's sign-in state) is told to react.
 */
export function handleAuthFailure(): void {
  clearStoredToken()
  for (const listener of authFailureListeners) listener()
}

export const AuthTokenLive = Layer.succeed(AuthTokenService, {
  get: () => Effect.try({
    try: () => {
      const raw = storage()?.getItem(storageKey)
      return raw ? JSON.parse(raw) as AuthToken : undefined
    },
    catch: (error) => new AuthError({ message: error instanceof Error ? error.message : "Unable to read the access token" })
  }),
  set: (token: AuthToken) => Effect.try({
    try: () => storage()?.setItem(storageKey, JSON.stringify(token)),
    catch: (error) => new AuthError({ message: error instanceof Error ? error.message : "Unable to store the access token" })
  }).pipe(Effect.asVoid),
  clear: () => Effect.try({
    try: () => storage()?.removeItem(storageKey),
    catch: (error) => new AuthError({ message: error instanceof Error ? error.message : "Unable to clear the access token" })
  }).pipe(Effect.asVoid)
})

export interface OidcClient {
  readonly login: () => Effect.Effect<void, AuthError>
  readonly completeCallback: (url?: string) => Effect.Effect<AuthToken, AuthError>
  readonly logout: () => Effect.Effect<void, AuthError>
}

export const OidcClient = Context.Service<OidcClient>("OidcClient")

/**
 * True when the current URL is an OIDC redirect that still needs exchanging.
 *
 * The pending-request marker is required as well as `code`: without it there is no
 * PKCE verifier to complete the exchange, and a bare `?code=` in the address bar is
 * not a callback this app started.
 */
export function isOidcCallback(url = window.location.href): boolean {
  try {
    return new URL(url).searchParams.has("code") && sessionStorage.getItem(pendingKey) !== null
  } catch {
    return false
  }
}

export function oidcConfigFromEnv(): OidcConfig | undefined {
  const issuer = import.meta.env.VITE_OIDC_ISSUER
  const clientId = import.meta.env.VITE_OIDC_CLIENT_ID
  if (!issuer || !clientId) return undefined
  return {
    issuer,
    clientId,
    redirectUri: import.meta.env.VITE_OIDC_REDIRECT_URI ?? `${window.location.origin}${window.location.pathname}`,
    scope: import.meta.env.VITE_OIDC_SCOPE ?? "openid profile email"
  }
}

export function createOidcClient(config: OidcConfig, tokenStore: AuthTokenService): OidcClient {
  return {
    login: () => Effect.tryPromise({
      try: async () => {
        const request = await createPkceAuthorizationRequest(config)
        sessionStorage.setItem(pendingKey, JSON.stringify({ state: request.state, codeVerifier: request.codeVerifier }))
        window.location.assign(request.url)
      },
      catch: (error) => new AuthError({ message: error instanceof Error ? error.message : "Unable to start sign-in" })
    }),
    completeCallback: (callbackUrl = window.location.href) => Effect.tryPromise({
      try: async () => {
        const url = new URL(callbackUrl)
        // Surface IdP error responses (e.g. access_denied) instead of treating
        // them as an incomplete callback with a generic message.
        const oidcError = url.searchParams.get("error")
        if (oidcError) {
          const description = url.searchParams.get("error_description")
          sessionStorage.removeItem(pendingKey)
          throw new Error(`OIDC error: ${oidcError}${description ? ` — ${description}` : ""}`)
        }
        const code = url.searchParams.get("code")
        const state = url.searchParams.get("state")
        const pendingRaw = sessionStorage.getItem(pendingKey)
        if (!code || !state || !pendingRaw) throw new Error("OIDC callback is incomplete")
        // Clear the pending PKCE state immediately: the verifier is now in
        // memory, and the authorization code is single-use. If anything below
        // fails (state mismatch, network error, malformed token response), a
        // retry with the stale pending data can never succeed — the code is
        // already consumed. Clearing ensures isOidcCallback() returns false so
        // the app starts a fresh login flow instead of looping on a dead
        // callback.
        sessionStorage.removeItem(pendingKey)
        const pending = JSON.parse(pendingRaw) as { state: string; codeVerifier: string }
        if (pending.state !== state) throw new Error("OIDC state does not match")
        const endpoint = config.tokenEndpoint ?? `${config.issuer.replace(/\/$/, "")}/protocol/openid-connect/token`
        const response = await fetch(endpoint, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: new URLSearchParams({ grant_type: "authorization_code", client_id: config.clientId, redirect_uri: config.redirectUri, code, code_verifier: pending.codeVerifier }) })
        if (!response.ok) throw new Error(`OIDC token exchange returned HTTP ${response.status}`)
        const body = await response.json() as { access_token?: string; expires_in?: number; token_type?: string; refresh_token?: string }
        if (!body.access_token) throw new Error("OIDC token response did not contain an access token")
        const token: AuthToken = {
          accessToken: body.access_token,
          ...(body.expires_in === undefined ? {} : { expiresAt: Date.now() + body.expires_in * 1000 }),
          ...(body.token_type === undefined ? {} : { tokenType: body.token_type }),
          ...(body.refresh_token === undefined ? {} : { refreshToken: body.refresh_token })
        }
        await Effect.runPromise(tokenStore.set(token))
        return token
      },
      catch: (error) => new AuthError({ message: error instanceof Error ? error.message : "OIDC callback failed" })
    }),
    logout: () => tokenStore.clear()
  }
}

export const OidcClientLive = Layer.effect(OidcClient, AuthTokenService.use((tokens) => {
  const config = oidcConfigFromEnv()
  if (!config) return Effect.succeed({ login: () => Effect.fail(new AuthError({ message: "Sign-in is not configured" })), completeCallback: () => Effect.fail(new AuthError({ message: "Sign-in is not configured" })), logout: () => tokens.clear() })
  return Effect.succeed(createOidcClient(config, tokens))
}))

/**
 * Decodes the JWT access token and returns the display name used to attribute
 * review items. The payload is NOT verified here: the app's backend verifies
 * every request server-side, so client-side decoding is only for attribution
 * and UI labels, never for trust. Falls back through `preferred_username`,
 * `email`, `sub`, then `"anonymous"` when there is no token or the payload is
 * unreadable (e.g. opaque tokens in some IdPs).
 */
export function currentUserDisplayName(): string {
  const payload = decodedTokenPayload()
  if (!payload) return "anonymous"
  const name = payload.preferred_username ?? payload.email ?? payload.sub
  return typeof name === "string" ? name : "anonymous"
}

/**
 * Decodes the JWT access token payload (base64url → JSON). The payload is NOT
 * verified here: the app's backend verifies every request server-side, so
 * client-side decoding is only for attribution and UI labels, never for trust.
 * Returns undefined when there is no token or the payload is unreadable (e.g.
 * opaque tokens in some IdPs).
 */
export function decodedTokenPayload(): Record<string, unknown> | undefined {
  const token = readStoredAccessToken()
  if (!token) return undefined
  try {
    // JWT payloads are base64url-encoded (using "-" and "_" instead of "+"
    // and "/", with no "=" padding). atob() only understands standard base64,
    // so a payload containing a "-" or "_" would throw and silently fall back
    // to "anonymous". Convert to standard base64 with padding first.
    const part = token.split(".")[1]
    if (!part) return undefined
    const b64 = part.replaceAll("-", "+").replaceAll("_", "/") + "=".repeat((4 - part.length % 4) % 4)
    return JSON.parse(atob(b64)) as Record<string, unknown>
  } catch { return undefined }
}


/** Refresh the access token using the stored refresh_token grant. */
export async function refreshAccessToken(): Promise<AuthToken | undefined> {
  const config = oidcConfigFromEnv()
  if (!config) return undefined
  const stored = readStoredToken()
  if (!stored?.refreshToken) return undefined
  const endpoint = config.tokenEndpoint ?? `${config.issuer.replace(/\/$/, "")}/protocol/openid-connect/token`
  const response = await fetch(endpoint, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "refresh_token",
      client_id: config.clientId,
      refresh_token: stored.refreshToken
    })
  })
  if (!response.ok) {
    clearStoredToken()
    return undefined
  }
  const body = await response.json() as { access_token?: string; expires_in?: number; token_type?: string; refresh_token?: string }
  if (!body.access_token) return undefined
  const token: AuthToken = {
    accessToken: body.access_token,
    ...(body.expires_in === undefined ? {} : { expiresAt: Date.now() + body.expires_in * 1000 }),
    ...(body.token_type === undefined ? {} : { tokenType: body.token_type }),
    ...(body.refresh_token ?? stored.refreshToken === undefined ? {} : { refreshToken: body.refresh_token ?? stored.refreshToken })
  }
  try { sessionStorage.setItem(storageKey, JSON.stringify(token)) } catch { /* storage unavailable */ }
  return token
}

/**
 * Proactively refreshes the access token ~60s before it expires, using the
 * OIDC refresh_token grant. Call once on sign-in. Cancels any previous timer.
 */
let refreshTimer: ReturnType<typeof setTimeout> | undefined

export function scheduleTokenRefresh(): void {
  if (refreshTimer !== undefined) clearTimeout(refreshTimer)
  const stored = readStoredToken()
  if (!stored?.expiresAt || !stored?.refreshToken) return
  const msUntilRefresh = stored.expiresAt - Date.now() - 60_000
  if (msUntilRefresh <= 0) {
    void refreshAccessToken()
    return
  }
  refreshTimer = setTimeout(() => void refreshAccessToken(), msUntilRefresh)
}
