# obleth-gateway

[![CI](https://github.com/thediymaker/obleth-gateway/actions/workflows/ci.yml/badge.svg)](https://github.com/thediymaker/obleth-gateway/actions/workflows/ci.yml)
[![License: BSL 1.1](https://img.shields.io/badge/license-BSL%201.1-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)

![obleth dashboard overview](.github/assets/dashboard.png)

**obleth is a multi-tenant gateway between your clients and your LLMs. It handles
authentication, decides which requests to admit when capacity is tight, and routes
each one to the right OpenAI-compatible backend.**

Point your clients at obleth and register your models, each mapped to its own
upstream. Backends can be anything OpenAI-compatible (vLLM, Aibrix, OpenAI,
Together, or your own), and different models can target different providers. obleth
adds the layer that load balancers and inference routers don't: per-tenant
identity, weighted fair queuing, model routing, token-accurate cost accounting, and
defined behavior when demand exceeds capacity.

```
   clients ──▶ obleth ──▶ any OpenAI-compatible provider(s)
                          (vLLM · Aibrix · OpenAI · Together · your own)
```

## Who it's for

obleth is for teams running LLM capacity (or a budget) shared across multiple
tenants — teams, apps, or customers — where you need to control how that shared
capacity is divided. It prevents a batch job from starving an interactive workload
and stops a single tenant from consuming the entire budget. A single-tenant setup
behind one provider key does not need it.

## What it does

- **Weighted fair queuing.** Each tenant has a weight. When demand exceeds
  capacity, throughput is divided proportionally to weight, measured in tokens,
  with starvation-free guarantees. A higher-weight tenant keeps its share while
  lower-weight traffic is held back. Weights are adjustable at runtime, no restart.
  Tenants can be grouped so capacity is split between groups first, then between
  tenants within a group.
- **Auto model routing.** Send `model: "auto"` and obleth selects a model based on
  live capacity, cost, and operator-assigned tags (`coding`, `reasoning`, `vision`,
  …), across every registered backend. An optional small-model classifier maps each
  request to the best-matching tags, and falls back to heuristics if the classifier
  is unavailable.
- **Access windows.** A tenant can have an activation date, an expiry date, and/or
  recurring weekly windows, evaluated in its own timezone. Outside those windows its
  keys admit no traffic — useful for off-peak batch tenants or time-boxed access.
- **Limits and budgets.** Per-tenant tokens-per-minute rate limits, per-tenant and
  per-model in-flight concurrency caps, and per-tenant token and USD budgets that
  reset on a lifetime, monthly, or term basis. Each tenant can also be restricted to
  an allowlist of models.
- **Token-accurate cost accounting.** obleth estimates tokens at admission, reserves
  them atomically, and reconciles the true cost against per-model input/output
  pricing after the stream completes.
- **Model health and uptime tracking.** For each model, obleth first checks recent
  client traffic in the ClickHouse usage ledger (passive signal); when there is no
  recent traffic it runs a token-free liveness probe (`GET {api_base}/models`)
  directly at the upstream. Health status and consecutive failures are tracked;
  unhealthy models drop out of `auto` routing rotation. Operators can set
  maintenance windows to suppress alerts during planned downtime.
- **Per-model capacity and auto-tune.** Each model can carry its own `max_in_flight`
  slot cap (inside the global scheduler limit). A bounded ramp probe drives real load
  directly at the upstream — bypassing gateway admission — to find the throughput/latency
  knee for chat and embedding models. The probe is recommend-only; operators apply the
  suggested slots from the dashboard or Management API and can mark a model `static`
  (operator-set) or `tuned` (probe-derived).
- **Alerting.** Health failures, budget exhaustion, and other operational events are
  dispatched to Slack webhooks and email (SMTP).
- **Response caching.** Optional per-model exact-match response caching in Redis,
  with an operator-controlled TTL.
- **MCP proxying.** Register Model Context Protocol servers and expose them through
  obleth's single authenticated endpoint.
- **Audit log.** Every management action records the actor, entity, and a JSON
  detail payload.
- **Defined behavior under load.** When saturated, requests queue in the weighted
  fairshare scheduler until a slot opens — they are not dropped or degraded.
  Hard stops are explicit: `429` when the per-minute token budget is empty, `403`
  when a term budget is exhausted or the tenant is outside its access window, and
  `503` when the scheduler is unavailable. If Redis or ClickHouse become
  unavailable, the data plane fails open from its in-process cache and replays
  telemetry on recovery.
- **Observability.** Per-model throughput (tok/s), TTFT and end-to-end latency
  (average and p50), prompt/generation token averages, and unique users — all
  computed from obleth's own usage ledger — plus a live fairshare view in the
  dashboard.
- **SSRF protection.** Admin-registered upstreams (model `api_base`, MCP URLs) are
  validated on create/update. By default (local-first), private/LAN/loopback targets
  are allowed; link-local and cloud-metadata addresses are always blocked. Set
  `OBLETH_BLOCK_PRIVATE_NETWORKS=1` for strict mode, then allow specific internal
  CIDRs via `OBLETH_ALLOWED_PRIVATE_CIDRS`.

The data plane is a thin Rust service on the request path. The control plane
(dashboard and Management API) configures everything out of band and never touches
the hot path. obleth uses three datastores: Postgres (config source of truth),
Redis (hot key cache and atomic token budgets), and ClickHouse (async usage and
cost ledger).

## Quick start (Docker)

Pull the published images (recommended — no toolchain needed):

```bash
cd deploy/docker && cp .env.example .env
docker compose --profile benchmark --profile edge --profile observability pull
docker compose --profile benchmark --profile edge --profile observability up -d
```

Images are published to `ghcr.io/thediymaker/obleth-gateway/*` on every
release; pin one by setting `OBLETH_VERSION=vX.Y.Z` in `.env` (defaults to
`latest`). To build from source instead, add `--build` to the `up` command.

Services: HAProxy (`:80`), obleth data plane (`:8088` on the host, `:8080` inside
the network), Management API (`:9180`), metrics (`:9091`), dashboard (`:3002` on
the host), Postgres, Redis, ClickHouse, benchmark fixture backend (`:8081`),
Prometheus (`:9090`), Grafana (`:3001`).

Open the dashboard at <http://localhost:3002>.

### Grafana dashboards

The `observability` profile auto-provisions Grafana (<http://localhost:3001>,
anonymous admin) with a Prometheus datasource and a pre-built **Obleth** folder
of dashboards: the gateway data plane (`obleth_*` metrics), plus full
PostgreSQL, Redis, ClickHouse, and HAProxy dashboards. Metrics are sourced from:

| Source | Exporter / endpoint | Scrape target |
| --- | --- | --- |
| obleth | built-in `:9091/metrics` | `obleth:9091` |
| Postgres | `prometheuscommunity/postgres-exporter` | `postgres-exporter:9187` |
| Redis | `oliver006/redis_exporter` | `redis-exporter:9121` |
| ClickHouse | built-in Prometheus endpoint | `clickhouse:9363` |
| HAProxy | built-in Prometheus exporter (`edge` profile) | `haproxy:8404` |

Dashboard JSON and provisioning live in `deploy/docker/grafana/`; edit the JSON
files and Grafana hot-reloads them. The HAProxy dashboard only has data when the
`edge` profile is also enabled.

### Create a tenant + key, then call the gateway

```bash
TOKEN=dev-admin-token
# create a boosted "chatbot" tenant
TID=$(curl -s -XPOST localhost:9180/api/v1/tenants \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"chatbot","weight":500,"tokens_per_minute":2000000}' | jq -r .id)

# mint a key (secret shown once)
SECRET=$(curl -s -XPOST localhost:9180/api/v1/tenants/$TID/keys \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"prod"}' | jq -r .secret)

# call through HAProxy (edge profile) or use localhost:8088 for direct data-plane access
curl -s localhost/v1/chat/completions \
  -H "Authorization: Bearer $SECRET" -H 'Content-Type: application/json' \
  -d '{"model":"benchmark-endpoint","messages":[{"role":"user","content":"hi"}],"max_tokens":32}'
```

Full API spec: `GET http://localhost:9180/api/v1/openapi.json`.

### Kubernetes

Install the published chart (each release pins its own image tags):

```bash
helm install obleth oci://ghcr.io/thediymaker/charts/obleth --version <X.Y.Z> -f my-values.yaml
```

Or install from a checkout:

```bash
helm install obleth deploy/k8s/obleth -f my-values.yaml
```

Ships the obleth Deployment + HPA + Services, an optional ServiceMonitor, and
bundled demo dependencies. For production, point at CloudNativePG, an operator-
managed ClickHouse, and HA Redis (`postgres.enabled=false`, etc.). See
`deploy/k8s/README.md` for post-install steps.

A fresh `helm install` starts with **no models and no tenant keys**. After pods
are Running:

1. Port-forward the Management API: `kubectl port-forward svc/obleth 9180:9180`
2. Create a tenant and mint a key (`POST /api/v1/tenants`, then
   `POST /api/v1/tenants/{id}/keys`) — there is no shared "open" proxy key.
3. Register models via `POST /api/v1/models`. Set `api_base` to the provider base
   ending in `/v1`; set `upstream_model` to the bare name the backend expects.

By default, private cluster addresses (e.g. `*.svc.cluster.local` → `10.x`) are
allowed for `api_base` without extra configuration. If you enable strict SSRF
(`OBLETH_BLOCK_PRIVATE_NETWORKS=1`), set `obleth.allowedPrivateCidrs` to your
pod CIDR (e.g. `10.0.0.0/8`). Do not commit real secrets in values files — use
`--set` or inject a Kubernetes Secret instead.

## Configuration

obleth is configured through environment variables. The essentials:

| Variable | Purpose |
| --- | --- |
| `OBLETH_UPSTREAM_BASE_URL` | default upstream for requests without a registered model route (each model can override with its own `api_base`) |
| `OBLETH_DATABASE_URL` / `OBLETH_REDIS_URL` / `OBLETH_CLICKHOUSE_URL` | the three datastores |
| `OBLETH_ADMIN_TOKEN` | Management API bearer token (**required**) |
| `OBLETH_FAIL_OPEN` | admit when Redis is down (default `true`) |

The credentials in `*.env.example`, `docker-compose.yml`, and `values.yaml` are
**development examples only** — replace them and front the gateway with TLS before
deploying anywhere real.

## Documentation

Architecture, the fairshare engine internals, auto routing and the classifier,
scheduling, budgets, secrets, SSRF policy, alerting, dashboard auth, and the
full configuration reference live at **[obleth.com](https://obleth.com)**.

For contribution workflow, security reporting, and expected collaboration
standards, see [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md),
and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## License

[Business Source License 1.1](LICENSE) (BSL 1.1). The codebase is
**source-available**, not OSI open source — but each released version
automatically converts to the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0)
on its Change Date (four years after that version's first publication).

You may use, modify, and run obleth in production for your own workloads, and
use it freely for internal, academic, research, and educational purposes. You
may **not** offer obleth to third parties as a hosted or managed service, nor
distribute or sell a competing gateway product derived from it, until the
Change Date (see [LICENSE](LICENSE) for the full Additional Use Grant).
Contributions back to the project are welcome and encouraged. Contact the
maintainers for alternative commercial licensing.
