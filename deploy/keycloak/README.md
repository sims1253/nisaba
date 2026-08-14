# Keycloak (local OIDC) — DEVELOPMENT ONLY

> **The imported realm is a local-dev fixture, not a production config.**
> `nisaba-realm.json` ships a **public** PKCE client (no secret),
> `sslRequired: "none"`, and three demo users with trivial passwords. **Every
> one of these must be replaced for production** (see the checklist below). The
> realm `displayName` is set to a loud reminder of this.

The local stack runs Keycloak in `start-dev` mode (HTTP, relaxed hostname),
backed by a dedicated database (`keycloak`) and role inside the shared Postgres
instance. On first boot it imports `nisaba-realm.json`.
The small `nisaba` login theme only constrains the oversized development realm
label on mobile so it cannot cover the sign-in controls.

## Realm: `nisaba`

- **Client:** `nisaba-web` — **public** client, authorization code flow + PKCE
  (`S256`). There is **no client secret** exposed to the browser and none is
  read by the app. `directAccessGrantsEnabled` (the password grant) is **off**.
- **Token claims:** roles are mapped into a **top-level `roles`** claim (the app
  reads `roles`, *not* Keycloak's conventional `realm_access.roles`), and a
  `nisaba-audience` mapper emits **two** values into the access-token `aud`:
  `nisaba-web` (the client audience Keycloak adds natively) and `nisaba` (a
  custom audience). The app validates `NISABA_OIDC_AUDIENCE` against that
  claim, so **both settings work**: `.env.example` sets `nisaba-web`, while the
  app's built-in default is `nisaba`. Change either only in step with the
  realm's mapper.
- **Roles:** `author`, `reviewer`, `read-only`. `reviewer` is the
  suggesting/accept-reject role; `read-only` cannot edit.
- **Demo users (local dev only):**

  | username  | password   | roles               |
  |-----------|------------|---------------------|
  | `demo`    | `demo`     | author, reviewer    |
  | `reviewer`| `reviewer` | reviewer            |
  | `reader`  | `reader`   | read-only           |

## Web client — public + PKCE (no secret)

`nisaba-web` is a **public** OIDC client. The browser performs the
authorization code flow with a PKCE code challenge (`S256`), so **no client
secret is ever shipped to the browser** and none is configured in `.env`.

The **app service verifies tokens by signature, not by client secret.** It reads
the signing keys **inline** from `NISABA_OIDC_JWKS_JSON` at startup (it does not
fetch a discovery URL today). An empty or unset variable is the safe deny-all
default: the app **boots normally and rejects every token** (an empty value is
treated exactly like an unset one — it must not be parsed as a JWKS document).
To accept tokens in local dev, populate it with the realm JWKS:

```bash
curl -fsS http://127.0.0.1:8090/realms/nisaba/protocol/openid-connect/certs
```

…or use the self-contained dev token (`uv run deploy/dev-token.py` / `just e2e`), which
mints a key + JWKS + JWT triple and injects the JWKS into the app's env.

## Redirect URIs

Configured for local origins (`http://127.0.0.1:5173`, `:8103` and their
`localhost` aliases). Vite dev runs on `:5173`; the built nginx image on `:8103`.

## Issuer split (read this if auth fails)

The browser and the `app` container address Keycloak by different hostnames.
Configure the app with:

- `NISABA_OIDC_ISSUER`         — the value inside tokens (`iss`), browser-facing.
- `NISABA_OIDC_JWKS_JSON`      — the JWKS the app verifies signatures against,
                                 read **inline** at startup (see above).
- `NISABA_OIDC_DISCOVERY_URL`  — internal URL the production adapter will fetch
                                 JWKS from (not read by the app today).

See [`docs/architecture.md`](../../docs/architecture.md) §6. In production behind
one TLS hostname these collapse to a single URL.

## Production checklist (replace EVERY default)

The dev realm is intentionally insecure. Before any non-local deployment you
MUST:

- [ ] Keep `nisaba-web` a **public** client with PKCE (`S256`) enabled; never
      re-introduce a static client secret into the browser bundle.
- [ ] Confirm the `roles` (top-level) and `nisaba-audience` (aud: both
      `nisaba-web` and `nisaba`) mappers are present on the production client,
      or the app will reject tokens / see no roles.
- [ ] Populate the app's `NISABA_OIDC_JWKS_JSON` (or wire the adapter to fetch
      `NISABA_OIDC_DISCOVERY_URL`) from the production IdP, never the dev JWKS.
- [ ] Delete the `demo` / `reviewer` / `reader` users (or replace with real
      accounts, strong passwords, and MFA for `reviewer`).
- [ ] Set `sslRequired` to `"external"` or `"all"` (dev is `"none"`).
- [ ] Keep `directAccessGrantsEnabled` off (the password grant); prefer the
      authorization code + PKCE flow. (The dev realm already disables it.)
- [ ] Run Keycloak with `start --optimized` behind a TLS-terminating reverse
      proxy on a single hostname, collapsing `NISABA_OIDC_ISSUER` and
      `NISABA_OIDC_DISCOVERY_URL` into one URL.
- [ ] Supply `KEYCLOAK_ADMIN_PASSWORD` (and the DB role passwords) from a
      secrets manager, not `.env`.
- [ ] Back up the `keycloak` database separately (it is excluded from the dev
      `deploy/backup/backup.sh` scope).

## Re-importing the realm

`start-dev --import-realm` imports on first boot only (when the DB is empty).
To apply realm changes to an existing volume:

```bash
just down-volumes && just up    # destroys ALL data; dev only
# or, from the admin console at http://127.0.0.1:8090/admin, import manually.
```
