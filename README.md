# obleth-gateway

[![CI](https://github.com/thediymaker/obleth-gateway/actions/workflows/ci.yml/badge.svg)](https://github.com/thediymaker/obleth-gateway/actions/workflows/ci.yml)
[![License: ELv2](https://img.shields.io/badge/license-ELv2-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)

![obleth dashboard overview](.github/assets/dashboard.png)

**obleth is a multi-tenant gateway that sits between your users and your LLMs and
decides, under load, who gets to send and at what priority.**

Point your clients at obleth and obleth at any OpenAI-compatible provider (vLLM,
Aibrix, OpenAI, Together, your own). It owns the layer load balancers and inference
routers leave open: tenant identity, weighted fair queuing, model routing, token-
accurate cost accounting, and graceful degradation when there isn't enough capacity
for everyone.

```
   clients ──▶ obleth ──▶ any OpenAI-compatible provider
               │          (vLLM · Aibrix · OpenAI · Together · your own)
        who gets to send,
       at what priority, in
         whose fair share
```

## Who it's for

You run a shared GPU fleet or a shared API budget across more than one team, app,
or customer, and you need fairness and control the upstream doesn't give you. If a
single provider key behind a basic proxy is enough, you don't need obleth. If a
batch job can starve your chatbot, or one tenant can burn the whole budget, you do.

## What it does

- **Weighted fair queuing.** Each tenant has a weight. When demand exceeds
  capacity, throughput is divided proportionally to weight, measured in tokens,
  with starvation-free guarantees. A high-weight chatbot key keeps its share under
  a flood of batch traffic. Weights are tunable at runtime, no restart.
- **Auto model routing.** Send `model: "auto"` and obleth picks a model by live
  capacity, cost, and operator-assigned tags (`coding`, `reasoning`, `vision`, …).
  An optional tiny-model classifier maps each request to the best-matching tags,
  and degrades gracefully to heuristics if the classifier is unavailable.
- **Time-of-use scheduling.** Give a tenant an activation window and/or recurring
  weekly windows in its own timezone. Outside its windows the tenant doesn't admit
  traffic — useful for off-peak batch tenants or time-boxed access.
- **Token-accurate cost accounting.** Estimate at admission, reserve atomically,
  reconcile the true token cost after the stream completes. Per-tenant token/USD
  budgets with lifetime, monthly, or term reset.
- **Graceful degradation.** Under saturation, low-priority traffic is browned out
  (capped `max_tokens`) instead of rejected. If Redis or ClickHouse blink, the data
  plane fails open from its in-process cache and replays telemetry on recovery.
- **Observability.** Per-model throughput (tok/s), TTFT/E2E latency (avg + p50),
  prompt/generation token averages, and unique users — all computed internally
  from obleth's own usage ledger, plus a live fairshare view in the dashboard.

The data plane is a thin Rust service on the request path. The control plane
(dashboard + Management API) configures everything out of band and never touches
the hot path. Three datastores by design: Postgres (config source of truth), Redis
(hot key cache + atomic token budgets), and ClickHouse (async usage/cost ledger).

## Quick start (Docker)

```bash
docker compose -f deploy/docker/docker-compose.yml --profile benchmark --profile edge --profile observability up --build -d
```

Services: HAProxy (`:80`), obleth proxy (`:8080`), Management API (`:9090`),
metrics (`:9091`), dashboard (`:3000`), Postgres, Redis, ClickHouse,
benchmark fixture backend (`:8081`), Prometheus (`:9095`), Grafana (`:3001`).

Open the dashboard at <http://localhost:3000>.

### Create a tenant + key, then call the gateway

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

# call the gateway (use "auto" as the model to let obleth pick one)
curl -s localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $SECRET" -H 'Content-Type: application/json' \
  -d '{"model":"benchmark-endpoint","messages":[{"role":"user","content":"hi"}],"max_tokens":32}'
```

Full API spec: `GET http://localhost:9090/api/v1/openapi.json`.

### Kubernetes

```bash
helm install obleth deploy/k8s/obleth
```

Ships the obleth Deployment + HPA + Services, an optional ServiceMonitor, and
bundled demo dependencies. For production, point at CloudNativePG, an operator-
managed ClickHouse, and HA Redis (`postgres.enabled=false`, etc.).

## Configuration

obleth is configured through environment variables. The essentials:

| Variable | Purpose |
| --- | --- |
| `OBLETH_UPSTREAM_BASE_URL` | your OpenAI-compatible provider |
| `OBLETH_DATABASE_URL` / `OBLETH_REDIS_URL` / `OBLETH_CLICKHOUSE_URL` | the three datastores |
| `OBLETH_ADMIN_TOKEN` | Management API bearer token (**required**) |
| `OBLETH_FAIL_OPEN` | admit when Redis is down (default `true`) |

The credentials in `*.env.example`, `docker-compose.yml`, and `values.yaml` are
**development examples only** — replace them and front the gateway with TLS before
deploying anywhere real.

## Documentation

Architecture, the fairshare engine internals, auto routing and the classifier,
scheduling, budgets, brownout tuning, secrets, SSRF allow-lists, alerting,
dashboard auth, and the full configuration reference live at
**[obleth.com](https://obleth.com)**.

## License

[Elastic License 2.0](LICENSE) (ELv2). The codebase is **source-available**, not
OSI open source.

You may use, modify, and run obleth in production for your own workloads. You may
not provide the software to third parties as a hosted or managed service where
users get access to a substantial set of the gateway's features (see ELv2).
Contact the maintainers for alternative licensing.
