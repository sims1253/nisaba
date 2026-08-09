import { defineConfig, loadEnv } from "vite"

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "")
  // Compose publishes app on 8100 and sync on 8101 (docker-compose.yml).
  const app = env.VITE_APP_URL || "http://localhost:8100"
  const allowedHosts = (env.VITE_ALLOWED_HOSTS ?? "")
    .split(",")
    .map((host) => host.trim())
    .filter((host) => host.length > 0)
  return {
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
        // Proxy the mock OIDC provider so external users (via tunnel) can reach
        // the auth endpoints through Vite instead of a localhost-only address.
        "^/realms": { target: env.VITE_OIDC_PROXY_TARGET || "http://127.0.0.1:8095", changeOrigin: true }
      }
    },
    worker: { format: "es" },
    build: { target: "es2022" }
  }
})
