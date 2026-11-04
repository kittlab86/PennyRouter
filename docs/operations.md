# Operations

## Run it

Local run needs a Rust toolchain (rustup, stable); the container needs
only Docker.

```sh
cargo run -p pennyrouter-gateway   # serves :3000, PORT=... to override
curl localhost:3000/health         # {"status":"ok"}
```

Settings live in `.env` (copy [`.env.example`](../.env.example)); every
variable has a sane default so the commands below also work without one.

```sh
cp -n .env.example .env                 # once; optional
docker compose up --build               # local dev: builds from source
```

Server deployment pulls the CI-built images from GitHub Packages
and never builds — one command copies the pull-only compose file
plus the repo-root `.env` to the server, pulls, restarts, and
health-checks:

```sh
DEPLOY_HOST=user@example.com make deploy   # or ./scripts/deploy.sh
```

A dev deployment ships both services (gateway + catalog-sync) from
the same pull-only images under its own directory and host port:

```sh
COMPOSE_FILE=compose.dev.yaml DEPLOY_DIR=/opt/stacks/pennyrouter-dev \
  HOST_PORT=8003 DEPLOY_HOST=user@example.com ./scripts/deploy.sh
```

Deploy targets are env vars (see the header of
[`scripts/deploy.sh`](../scripts/deploy.sh)): `DEPLOY_HOST`
(default `apps.lan`), `DEPLOY_DIR` (default
`/opt/stacks/pennyrouter`), `COMPOSE_FILE` (default
`compose.dev.yaml`); app config comes from the shipped root `.env`.
`DRY_RUN=1` prints what would run (env redacted) without touching
the server. The dev compose file runs `catalog-sync` beside the
gateway on the shared volume; it is still a separate process that
shares only the versioned DB artifact (see
[architecture.md](architecture.md)). `compose.prod.yaml` is
gateway-only but mounts the same volume at `CATALOG_DB=/catalog`:
the separately-deployed catalog-sync ships artifacts there, and an
empty volume means standby passthrough until the first one lands
(Q14-A). All gateway env (`UPSTREAM_MAX_*`, `RELOAD_INTERVAL_SECS`,
`ALLOW_MANAGEMENT_PROXY`, `CLASSIFIER_*`) is forwarded in all three
compose files with code defaults, so shipped `.env` values apply.

CI builds `ghcr.io/lsk242/pennyrouter:latest` and smoke-tests `/health`
on every PR, and pushes it on merges to main. It also rejects any
`build:` section in `compose.prod.yaml`, keeping deploys pull-only.

## Local development

Docker-based local dev (see `docker/`):

- One command to build and run the gateway locally.
- A resource-capped profile matching the ~1 CPU / 1 GB target for perf
  runs (`compose.perf.yaml`, full harness in Iter 3).
- A health-only load scaffold (`tests/load/run.sh`) for session, latency,
  and resource smoke runs before chat workloads exist.
- An overhead comparison harness (`tests/load/compare.sh`): same chat
  workload against a mock backend directly and through the capped
  gateway, producing a markdown/JSON report. Re-run it to prove later
  iterations did not regress the baseline.
- An opt-in live integration check (`tests/integration/<lang>/`, each
  with its own README) that calls OpenRouter directly and through the
  gateway, then compares. It needs `OPENROUTER_API_KEY` in that
  folder's `.env` (gitignored, never committed) and a running gateway;
  without a key it skips. Never runs in CI.
- No secrets in the repo: backend keys travel with requests, not config.

## Configuration

Keep config minimal and env-based (`.env`, see [`.env.example`](../.env.example)):
`PORT` (container port), `HOST_PORT` (published port), `IMAGE` (dev:
tag for the local build; prod: image to pull), `OPENROUTER_BASE_URL`
(upstream for chat completions, default `https://openrouter.ai/api`),
`MODAL_BASE_URL` (upstream for `modal/…` routes, unset by default),
`BASETEN_BASE_URL` (upstream for `baseten/…` routes, default
`https://inference.baseten.co`, no `/v1` prefix — the gateway appends
the request path itself and strips a trailing `/v1`), `GATEWAY_UI` (set to `1` to
register the read-only `GET /ui` debug page on the gateway, unset
means the route does not exist),
`CATALOG_DB` (catalog artifact for routing: directory, manifest, or DB
file — unset means plain passthrough). A directory also serves the
newest verified ranking artifact when one exists (see
[ranking-index.md](ranking-index.md)).
`UPSTREAM_MAX_CONCURRENT` (default 64: max simultaneous upstream calls)
and `UPSTREAM_MAX_QUEUED` (default 128: max requests waiting for a
slot); beyond both, the gateway sheds with OpenAI-shaped 503 +
`Retry-After`, counted in `pennyrouter_load_shed_total`.
`ALLOW_MANAGEMENT_PROXY` (unset default: management writes through the
fallback proxy — key creation, credit purchases, workspace budget
changes — answer 403 `management_proxy_disabled` without touching
upstream; set to `1` to proxy them. Reads are unaffected).
`RELOAD_INTERVAL_SECS` (catalog reload cadence, default 60; 0 disables
the background reload).
Smart classifier leg (Iter 26): `CLASSIFIER_MODEL` (unset disables the
smart ids, which fail closed; every other id keeps pure-local
routing), `CLASSIFIER_BASE_URL` (default: the OpenRouter base),
`CLASSIFIER_TIMEOUT_MS`, `CLASSIFIER_FALLBACK_TIER`
(`low`/`lowest`), `CLASSIFIER_MAX_TOKENS`, `CLASSIFIER_MAX_INPUT_CHARS`.
No stored key, ever (Q08): the leg uses the caller's `Authorization`
header, optionally overridden per-request by
`x-pennyrouter-classifier-key`.
Catalog pipeline: `CATALOG_IMAGE` (sync image tag), `SYNC_INTERVAL`
(seconds between sync cycles, 0 = run once), `FETCH_OPENROUTER`
(1 = live fetch, 0 = baked fixtures), `EXTRA_MODEL_IDS`
(comma-separated ids fetched individually), judge keys
(`BASETEN_API_KEY`, `MODAL_KEY_ID`/`MODAL_KEY_SECRET`,
`MODAL_JUDGE_URL`; unset = recorded-fixture judging), `CATALOG_UI`
(`1` lets the catalog-ui browser start). Exact defaults live in
[`.env.example`](../.env.example), which stays canonical.

## OpenRouter-native clients (Iter 25)

Point OpenAI-compatible harnesses at the gateway with an explicit
`baseURL` — no DNS tricks needed (detail:
[proposals/2026-11-01-openrouter-interface.md](proposals/2026-11-01-openrouter-interface.md)):

- OpenAI-shaped clients: `baseURL http://gateway:3000/v1` (curated
  `openrouter/pair/…` menu, routing, budget, retry).
- OpenRouter-native SDKs: `baseURL http://gateway:3000/api/v1` (same
  routed chat/responses handler; account/catalog paths proxy to
  OpenRouter; `GET /api/v1/models` returns the full upstream catalog).

DNS-intercepting `openrouter.ai` toward the gateway stays a local-dev
trick only: the client expects a real `openrouter.ai` certificate, so
it needs either plain `http://` (LAN-only, many SDKs refuse non-TLS)
or a privately distributed CA (e.g. Caddy `tls internal`). Prefer the
explicit `baseURL` above.
No local key store:
backend keys travel with requests, never in config or fixtures. The
one exception is catalog judging (Phase 4): the offline
`catalog-process --judge-backend baseten|modal` reads judge keys from
the environment (`BASETEN_API_KEY`, `MODAL_JUDGE_URL`, `MODAL_KEY_ID`,
`MODAL_KEY_SECRET` — see [`.env.example`](../.env.example)). The
gateway never reads them; without them judging stays on recorded
fixtures. Never commit real values.

## Decision-model catalog (Iter 15)

Which model judges is config, not tribal knowledge. A judges file
(`catalog-process --judges <file>`) lists one entry per backend —
`recorded`, `baseten`, `modal` — with the judge model, base URL, the
*names* of the secret variables, the expected prompt version, and
the optional known per-call cost; plus a policy section
(`sample_rate`, `only_models`/`only_providers` scope, `max_calls`,
`max_spend_usd`). Recording paths resolve from the working
directory. Selection rules:

- Recorded judging is the default: free, deterministic, full runs,
  no caps. Live judges run only when explicitly selected with
  `--judge-backend`, and only on the policy-selected subset
  (deterministic per-endpoint sampling, so rebuilds re-judge the
  same endpoints).
- Call and spend caps are checked *before* any judgment is written:
  a breached cap fails closed with the DB untouched. A spend cap
  with unknown per-call cost fails rather than guessing. Every live
  rebuild prints one spend line (`calls x unit = total (caps …)`).
- Every judgment stores its audit identity (`judge_model`,
  `judge_prompt` columns; `judge_backend`, `judge_model`,
  `judge_prompt`, `judge_calls`, `judge_spend_usd` in `sync_meta`),
  so a quality tier always traces back to the exact judge + prompt
  behind it. Rows judged before Iter 15 read back `NULL` provenance.
- `--sample-rate`, `--max-calls`, `--max-spend-usd` override the
  file policy. Judges never run on the request path — this is all
  offline batch, before publish.

## Debug UIs (Iter 17)

Both UIs are read-only and completely off unless explicitly enabled
with an env var — no routes, no background work, no request-path
code runs when they are off.

- Gateway: `GATEWAY_UI=1` registers `GET /ui` on the gateway port:
  snapshot version and counts, the virtual menu, per-model
  offerings with the ranking's default-orientation provider order,
  free offers, a routing inspector, and the metrics text. Rendered
  on demand from short read locks; routing never waits on it and it
  never calls upstream. Unset (the default) leaves `/ui` (and
  `/ui/preview`) unregistered — the paths fall through to the
  fallback proxy like any unknown path. `GET /` always serves a
  fixed landing page (version, build hash, config status, links)
  instead of proxying upstream. The gateway binds 0.0.0.0: `/ui`,
  `/metrics`, and `/` are unauthenticated by design, so expose the
  port beyond localhost only behind your own firewall (same rule as
  catalog-ui above); beyond a LAN, terminate TLS in front.
- Public surface (Q09-A): no gateway route requires an operator
  credential — routing is pure BYOK (the caller's key rides each
  request), smart/preview need only the caller's key, and `/metrics`
  is always open. That is the product choice, stated loudly: run it
  on localhost, or behind firewall + TLS with source CIDRs (the
  Terraform stage does this). There is no shared-secret gate; do not
  invent one per deploy.
- Inspector (Iter 29): `/ui` shows ten fixed non-smart virtual combos
  top-3 plus a custom-model box. Paste the exact `Authorization`
  header value; the page sends it as the `Authorization` header to
  `GET /ui/preview?model=<id>&top=N`, which hashes it with the same
  seeded hash the request path pins with and returns the ordered
  candidates with every stored column (prices, effective cost,
  class, tier/score, tools, decisions, promo, free, availability,
  rank) plus the sticky pick (with this key) vs the fresh pick (no
  session) and the session hash. The raw key is never logged and
  never stored; previews never create pins. Plain-request view: tools,
  budget, and only/ignore constraints are not applied, so a capped
  request can fail closed where the preview shows a pick. Smart ids
  preview as an explanatory rejection (their tier comes from the live
  classifier leg). Missing `model` answers 400, no router answers 503.
- Catalog: `CATALOG_UI=1 catalog-ui --dir <artifact-dir>
  [--port 8080] [--log-file <path>]` serves a browser over the
  published artifacts: run history (manifests newest-first with
  verify status), models, endpoints (with judge provenance),
  ranking order, rebuild/audit meta, and the last 200 lines of the
  log file when given. The binary refuses to start without the flag.
  Nothing is cached or written, so it can run against the live
  artifact dir safely. Bind is localhost-only; expose further only
  behind your own firewall. In Docker the `catalog-ui` compose
  service (profile `ui`, off by default) serves the shared volume:
  `docker compose --profile ui up catalog-ui --build`, then open
  http://localhost:8080 (host port from `CATALOG_UI_PORT`). The
  container port comes from `PORT` (default 8080, `--port` wins).

## Ranking

From Iter 6: `catalog-rank` rebuilds the provider ranking offline from
the catalog DB plus optional trends and recorded AI adjustments, and
publishes a versioned `ranking-<version>.json` artifact beside the DB.
The sync container runs it every cycle (`TRENDS`/`ASSIST`/`PROMOS`
inputs default to the `/input` fixtures when present). The gateway
re-resolves its `CATALOG_DB` directory every `RELOAD_INTERVAL_SECS`
(default 60s, 0 disables) and atomically publishes the new snapshot
and ranking, keeping the last good ones when a reload fails or an
artifact is missing; reload attempts are counted in
`pennyrouter_catalog_reload_total{result="ok|error"}`. Last-good
retention and joint atomicity are different properties: each of
snapshot and ranking keeps its own last good copy, but the two swap
in sequence, so a reader can briefly see a new snapshot with the
previous ranking.
From Iter 7, "cheapest" means effective cost under the balanced
workload profile (see [pricing.md](pricing.md)), and `openrouter/free`
routes to any free offering.

## Guards and timeouts

From Iter 2: request bodies estimating over 1M tokens (~4 MiB at 4
bytes/token) are rejected with 413 before contacting the backend.
From Iter 11, uploads are also bounded in time (30s idle per chunk,
60s total; stalled uploads answer 408) and admitted before
collection, so below-cap incomplete uploads cannot accumulate.
Upstream budgets are connect 10s, first response 30s, stream idle 60s
per chunk, and 300s total for buffered responses; timeouts answer 504.
Buffered upstream bodies are capped at 16 MiB (oversize answers 502).
Upstream JSON errors pass through; anything else is wrapped in an
OpenAI-shaped error keeping the upstream status. End-to-end response
headers (encoding, range, retry-after, auth challenges, validators,
redirect targets) are preserved; rewritten bodies keep negotiation
metadata but get truthful representation headers. Redirects pass
through with `Location` instead of being followed. A stalled or
broken stream ends with a terminal SSE `error` event after the intact
prefix — truncation never looks like clean EOF. Beyond the
concurrency/queue bounds the gateway sheds with OpenAI-shaped 503 +
`Retry-After`.

## Observability

From Iter 3: every request logs one JSON line to stdout with request ID
(incoming `x-request-id` or generated, also echoed back), method, route,
status, latency, and outcome. `GET /metrics` serves Prometheus counters
(requests by route/status, upstream errors by kind, guard rejections,
in-flight gauge) plus a latency histogram. The chosen provider rides
the `x-pennyrouter-provider` response header, not the log line.
Never log bodies, prompts, completions, or Authorization headers. From Iter 8, fallback-proxied
calls emit an extra `{"fallback_proxy":true,...}` line with method and
full path (see [api.md](api.md)). Later, readiness/snapshot health
may complement `/health` without changing its liveness contract.
