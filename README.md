# obleth-gateway

**obleth** is a lightweight, high-performance, fairshare-first AI gateway. It sits
between your front door (HAProxy) and your inference backend (Aibrix / vLLM) and
owns the layer those tools deliberately don't: **multi-tenant identity,
contention-based weighted fair queuing, token-accurate cost accounting, and
reliability.**

**Documentation:** [obleth-docs](https://github.com/thediymaker/obleth-docs) — run locally with `npm run dev` (port 3003).

obleth decides *who* gets to send and at *what priority*. Aibrix decides *which
pod* serves it. They compose; obleth does not re-implement pod routing.

> Source-available core. The differentiator is real-time **fairshare with priority
> boosts**: give your chatbot key a weight boost so a flood of API/batch traffic
> can never choke it out — and adjust it live from the dashboard or API.

## Why obleth

Existing self-hostable gateways (LiteLLM, etc.) are provider-abstraction proxies
with per-key rate limits; under heavy load they degrade or fall over, and none
offer true contention-based weighted fairness for a shared GPU fleet. obleth is
built in Rust around a single idea: when the cluster is saturated, capacity is
divided **proportionally to tenant weight**, measured in **tokens**, with
starvation-free guarantees.

## Architecture

```mermaid
flowchart LR
  client[Clients] --> haproxy[HAProxy: SSL + round-robin]
  haproxy --> obleth[obleth Rust pod]
  subgraph dataplane [obleth data plane]
    auth[Auth: moka + Redis] --> est[Tokenize + estimate]
    est --> admit[Fairshare admit: weighted queue + concurrency]
    admit --> reserve[Redis Lua: reserve budget]
    reserve --> proxy[Stream proxy]
    proxy --> recon[Reconcile actual cost]
    recon --> telem[Async telemetry]
  end
  obleth --> aibrix[Aibrix router] --> vllm[vLLM replicas]
  obleth -. hot reads .-> redis[(Redis)]
  telem -. batched .-> ch[(ClickHouse)]
  telem -. fail-open .-> wal[(local WAL)]
  subgraph control [obleth control surface]
    api["Management API /api/v1"]
  end
  cp[Next.js dashboard] --> api
  cli[CLI / Terraform] --> api
  api -- write + audit --> pg[(Postgres)]
  api -- sync + invalidate --> redis
  api -- usage reads --> ch
```

### Datastores (three, by design)
- **Postgres** — relational source of truth for config + audit (tenants, keys,
  weights, quotas, change history). Off the hot path.
- **Redis** — hot read-cache of resolved keys + atomic token-bucket budgets.
  The data plane reads only Redis.
- **ClickHouse** — append-only usage/cost ledger, async-inserted, never blocking
  a request.

Writes are single-sourced: **Management API → Postgres → Redis (sync + pub/sub
invalidate)**.

## The fairshare engine

- A single scheduler task owns admission (no lock races, deterministic order).
- A pluggable `CapacityProvider` sets the global in-flight budget. v1 is a
  runtime-tunable static limit; the trait is the seam for metrics-driven
  (vLLM/Aibrix queue depth, KV-cache util) or SLO-driven capacity later.
- Under contention, freed permits go to the tenant **most behind its weighted
  fair share** (start-time fair queuing). Higher `weight` = larger share; idle
  tenants can't bank credit and burst.
- Fairness is in **tokens**: estimate at admission, atomically reserve in Redis
  (Lua), reconcile the true cost after the stream completes.
- **Brownout, not 429**: under saturation low-priority traffic is degraded
  (capped `max_tokens`) instead of rejected.
- **Fail-open**: if Redis/ClickHouse blink, serve from the in-process key cache
  and spill telemetry to a WAL that replays on recovery.

## Quickstart (Docker)

```bash
docker compose -f deploy/docker/docker-compose.yml --profile benchmark --profile edge --profile observability up --build -d
```

Services: HAProxy (`:80`), obleth proxy (`:8080`), Management API (`:9090`),
metrics (`:9091`), dashboard (`:3000`), Postgres, Redis, ClickHouse,
benchmark fixture backend (`:8081`), Prometheus (`:9095`), Grafana (`:3001`).

Open the dashboard at <http://localhost:3000>.

### Create a tenant + key via the API

```bash
TOKEN=dev-admin-token
# create a boosted "chatbot" tenant
TID=$(curl -s -XPOST localhost:9090/api/v1/tenants \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"chatbot","weight":500,"tokens_per_minute":2000000}' | jq -r .id)

# mint a key (secret shown once)
SECRET=$(curl -s -XPOST localhost:9090/api/v1/tenants/$TID/keys \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"prod"}' | jq -r .secret)

# call the gateway
curl -s localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $SECRET" -H 'Content-Type: application/json' \
  -d '{"model":"benchmark-endpoint","messages":[{"role":"user","content":"hi"}],"max_tokens":32}'
```

Full API: `GET http://localhost:9090/api/v1/openapi.json`.

## Prove it: fairshare under load

```bash
node bench/run-benchmark.mjs
```

The benchmark seeds weighted tenants, runs staggered contention, samples the live
fairshare state, verifies ClickHouse usage, and exits non-zero if the fairshare
story is not visible. Add `CHAOS=1` to pause Redis and ClickHouse during the same
run. See [bench/README.md](bench/README.md).

## Kubernetes

```bash
helm install obleth deploy/k8s/obleth
```

Ships the obleth Deployment + HPA + Services, an optional ServiceMonitor, and
bundled demo dependencies. For production, point at CloudNativePG, an operator-
managed ClickHouse, and HA Redis (`postgres.enabled=false`, etc.).

## Configuration (env)

| Variable | Default | Purpose |
| --- | --- | --- |
| `OBLETH_PROXY_LISTEN` | `0.0.0.0:8080` | data-plane listener |
| `OBLETH_ADMIN_LISTEN` | `0.0.0.0:9090` | Management API listener |
| `OBLETH_METRICS_LISTEN` | `0.0.0.0:9091` | Prometheus metrics |
| `OBLETH_DATABASE_URL` | `postgres://obleth:obleth@127.0.0.1:5432/obleth` | config SoT |
| `OBLETH_REDIS_URL` | `redis://127.0.0.1:6379` | hot cache + budgets |
| `OBLETH_CLICKHOUSE_URL` | `http://127.0.0.1:8123` | usage ledger |
| `OBLETH_UPSTREAM_BASE_URL` | `http://127.0.0.1:8081` | Aibrix/vLLM or benchmark fixture backend |
| `OBLETH_ADMIN_TOKEN` | **required** | Management API bearer token (no default; service refuses to start if unset) |
| `OBLETH_ENCRYPTION_KEY` | unset | base64 of 32 bytes; AES-256-GCM-encrypts upstream secrets (model `api_key`, MCP `auth_header`) at rest. Unset = plaintext storage |
| `OBLETH_ALLOWED_PRIVATE_CIDRS` | unset | comma-separated CIDRs that admin-registered upstream URLs may resolve to. By default private/loopback/link-local (incl. cloud metadata) are blocked |
| `OBLETH_API_KEY_PEPPER` | unset | optional server-side pepper mixed into API-key hashes; changing it invalidates issued keys |
| `OBLETH_GLOBAL_MAX_IN_FLIGHT` | `256` | static capacity (v1) |
| `OBLETH_BROWNOUT_WAIT_MS` | `750` | queue-wait before degradation |
| `OBLETH_FAIL_OPEN` | `true` | If Redis is unavailable, admit requests (graceful) rather than reject. Default suits self-hosting; set `false` for multi-tenant/cloud where quota enforcement must never be bypassed |
| `OBLETH_SLACK_WEBHOOK_URL` | unset | Slack incoming-webhook URL for gateway alerts |
| `OBLETH_SLACK_ALERT_MIN_INTERVAL_SECS` | `300` | per-issue Slack alert cooldown |

The dashboard (control plane) requires `DASHBOARD_USERNAME`, a password
(`DASHBOARD_PASSWORD_HASH` bcrypt — recommended — or `DASHBOARD_PASSWORD`), and a
`DASHBOARD_SESSION_SECRET` of at least 32 characters; it fails closed if any are
missing. The credentials committed in `*.env.example`, `docker-compose.yml`, and
`values.yaml` are **development examples only** — replace them before deploying,
and front the gateway with TLS termination (HAProxy/ingress/managed LB).

Newly minted API keys use the `sk_<random>` format. Existing keys remain valid
because only the SHA-256 hash of the full secret is stored.

## Repository layout

```
obleth/                 Rust workspace (data plane + management API)
  crates/obleth-config      shared config + domain types + key hashing
  crates/obleth-tokenizer   token counting + output estimation
  crates/obleth-fairshare   weighted fair-queuing admission scheduler
  crates/obleth-redis       hot cache + Lua budget scripts + pub/sub
  crates/obleth-store       Postgres config SoT + audit
  crates/obleth-telemetry   async ClickHouse writer + WAL fallback
  crates/obleth-admin       versioned Management API (axum + OpenAPI)
  crates/obleth-proxy       the `obleth` binary (3 listeners)
control-plane/        Next.js dashboard (consumes the Management API)
benchmark-backend/    OpenAI-compatible benchmark fixture backend
  bench/                benchmark harness
deploy/docker/        docker-compose + Dockerfiles + HAProxy/Prometheus
deploy/k8s/obleth/      Helm chart
schema/               Postgres + ClickHouse schema
```

Documentation lives in the separate [obleth-docs](https://github.com/thediymaker/obleth-docs) repository (Nextra site).

## Development

```bash
cd obleth && cargo test --workspace        # unit + fairness tests (infra-free)
# integration tests run when OBLETH_TEST_DATABASE_URL / OBLETH_TEST_REDIS_URL are set
cd control-plane && npm install && npm run build
```

## Roadmap

- Metrics-driven `CapacityProvider` (vLLM queue depth / KV-cache utilization).
- Per-tenant SLO targets + attainment view.
- Real BPE tokenizer (tiktoken / HF) behind the existing `Tokenizer` trait.
- Pingora-based data plane for extreme connection counts.
- CLI + Terraform provider generated from the OpenAPI spec.

## License

[Elastic License 2.0](LICENSE) (ELv2). The codebase is **source-available**, not
OSI open source.

You may use, modify, and run obleth in production for your own workloads. You may
not provide the software to third parties as a hosted or managed service where
users get access to a substantial set of the gateway's features (see ELv2).
Contact the maintainers for alternative licensing.
