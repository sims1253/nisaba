import { defineConfig, loadEnv } from "vite"

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "")
  // Compose publishes app on 8100 and sync on 8101 (docker-compose.yml).
  const app = env.VITE_APP_URL || "http://localhost:8100"
  const allowedHosts = (env.VITE_ALLOWED_HOSTS ?? "")
    .split(",")
    .map((host) => host.trim())
    .filter((host) => host.length > 0)
  // Optional dev-only OIDC proxy (see the "^/realms" rule below). Left unset by
  // default: nothing in the repo serves a fallback target, and an unconditional
  // rule with a dead default would 502 confusingly.
  const oidcProxyTarget = env.VITE_OIDC_PROXY_TARGET
  return {
    resolve: {
      alias: [
        // Keep every runtime import (including loro-codemirror's) behind the
        // same explicitly initialized WASM boundary.
        { find: /^loro-crdt$/, replacement: "/src/loro.ts" }
      ]
    },
    server: {
      port: 5173,
      // Vite always permits localhost and IP literals. Additional tunnel hosts
      // must be opted into explicitly as a comma-separated allowlist.
      allowedHosts,
      proxy: {
        // Mirrors deploy/web/nginx.conf: the app serves unprefixed CRUD routes
        // (/projects, ...), so the /api prefix is stripped — except /api/compile,
        // which the app serves under that exact path and which is forwarded
        // verbatim. If these two rules drift from the nginx config, dev and
        // production disagree about the wire contract.
        "^/api/compile$": { target: app, changeOrigin: true },
        "^/api/": { target: app, changeOrigin: true, rewrite: (path: string) => path.replace(/^\/api/, "") },
        "/sync": { target: env.VITE_SYNC_URL || "ws://localhost:8101", ws: true, changeOrigin: true },
        // Dev-only tunnel helper, opt-in via VITE_OIDC_PROXY_TARGET (no default):
        // when the dev server is reached through a tunnel, the browser cannot
        // address the developer's localhost Keycloak (docker-compose.yml runs it
        // on 8090). Set VITE_OIDC_ISSUER to <tunnel-origin>/realms/... and
        // VITE_OIDC_PROXY_TARGET to the IdP (e.g. http://127.0.0.1:8090) so the
        // auth endpoints forward through Vite. Production has no equivalent
        // (deploy/web/nginx.conf): browsers there reach the issuer directly.
        ...(oidcProxyTarget ? { "^/realms": { target: oidcProxyTarget, changeOrigin: true } } : {})
      }
    },
    worker: { format: "es" },
    build: { target: "es2022" }
  }
})
