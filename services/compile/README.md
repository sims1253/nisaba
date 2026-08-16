# Nisaba compile service

The compile service accepts already-projected Typst sources. It does not know about
CRDTs, marks, review semantics, or projection rules. `view` is retained as an open
projection label and is not interpreted here; callers may send `baseline`, `proposed`,
`redline`, `public`, or a future label.

## API

### `GET /healthz`

Open liveness endpoint. Returns `200 ok` and never requires authentication.

### `POST /compile`

Requires `Authorization: Bearer <token>` unless the service is explicitly running in
`development` or `test` mode.

Request:

```json
{
  "project_id": "project-1",
  "entry": "m3/3-2-1.typ",
  "sources": {"chapters/intro.typ": "= Introduction"},
  "view": "public"
}
```

`sources` is a complete in-memory projection. The entry must be present. Virtual paths
must be relative, use `/`, and must not escape their virtual root. The response contains
PDF (base64), source span map, diagnostics, outline, build ID, and
instrumentation including RSS when available.

## Runtime configuration

| Variable | Default | Description |
|---|---:|---|
| `NISABA_COMPILE_ADDR` | `0.0.0.0:8080` | Full listen socket address; takes precedence over `PORT`. |
| `PORT` | — | Numeric port used as `0.0.0.0:<PORT>` when `NISABA_COMPILE_ADDR` is absent. |
| `NISABA_COMPILE_MODE` | `production` | `production`, `development`, or `test`; only the latter two explicitly disable auth. |
| `NISABA_COMPILE_TOKEN` | — | Shared internal Bearer token. Required in production. |
| `NISABA_COMPILE_TIMEOUT_MS` | `120000` | Request compile timeout. |
| `NISABA_COMPILE_MAX_BODY_BYTES` | `8388608` | Axum request body limit. |
| `NISABA_COMPILE_MAX_SOURCES` | `256` | Maximum number of source files. |
| `NISABA_COMPILE_MAX_SOURCE_BYTES` | `4194304` | Maximum sum of UTF-8 source bytes. |
| `NISABA_COMPILE_MAX_WORKERS` | `256` | Maximum cached Typst workers (LRU + idle-TTL evicted). |
| `NISABA_COMPILE_WORKER_IDLE_TTL_MS` | `1800000` | Idle TTL before a cached worker is evicted. |
| `NISABA_COMPILE_MAX_CONCURRENT_COMPILES` | `8` | Global cap on concurrently running compiles. |

The production default deliberately listens on `0.0.0.0:8080` for deployment. Put the
service behind the deployment network boundary and configure a strong token; this token
is an internal service credential, not an end-user OIDC credential.

Workers are cached by `project_id` and evicted by an LRU + idle-TTL policy under the
`NISABA_COMPILE_MAX_WORKERS` cap, with a global concurrency semaphore
(`NISABA_COMPILE_MAX_CONCURRENT_COMPILES`). The global worker map is only held while
looking up or inserting an entry — building a new worker (parsing every source) happens
outside the lock. Compilation runs on Tokio's blocking pool and holds only that project's
mutex, so projects can compile concurrently while requests for the same project
serialize. RSS and worker/cache counters remain in response instrumentation.

The timeout bounds how long the HTTP request waits, but `spawn_blocking` tasks cannot be
force-killed safely. A timed-out Typst compile may continue consuming a blocking-pool
thread and the project's worker lock until it returns; size limits and deployment-level
resource limits are still required.

## Security behavior

- `/healthz` is open for load-balancer/container probes.
- `/compile` requires an exact `Authorization: Bearer <configured-token>` in production.
- Missing or malformed credentials receive `401`; no token is logged or returned.
- JSON bodies are bounded before deserialization, and source count, aggregate source
  bytes, entry presence, and virtual paths are validated before a worker is created.
- The service never shells out to Typst and never writes submitted sources to disk.

## Tests and checks

```bash
cargo fmt --all -- --check
cargo test -p nisaba-compile
cargo clippy -p nisaba-compile --all-targets -- -D warnings
```
