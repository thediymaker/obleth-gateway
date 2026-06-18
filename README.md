# obleth-gateway

[![CI](https://github.com/thediymaker/obleth-gateway/actions/workflows/ci.yml/badge.svg)](https://github.com/thediymaker/obleth-gateway/actions/workflows/ci.yml)
[![License: BSL 1.1](https://img.shields.io/badge/license-BSL%201.1-blue.svg)](LICENSE)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-dea584.svg?logo=rust&logoColor=white)](https://www.rust-lang.org/)

![obleth dashboard overview](.github/assets/dashboard.png)

**obleth is a multi-tenant AI gateway that helps teams share GPU-backed models across clusters, on-prem infrastructure, and cloud environments.**

Point your clients at obleth and register your models. obleth adds identity, fairshare admission, intelligent routing, and cost accounting on top of any OpenAI-compatible backend (vLLM, Aibrix, OpenAI, Together, or your own). For HPC clusters, the Slurm provisioner manages inference server jobs directly — obleth submits, monitors, and routes to them automatically.

```
   clients ──▶ obleth ──▶ any OpenAI-compatible backend(s)
                          (vLLM · Aibrix · OpenAI · Together · your own)
                     └──▶ Slurm-managed inference nodes (via provisioner)
```

## What makes it different

### Weighted fairshare scheduler
Admission is controlled by a purpose-built weighted fair-queuing scheduler written in Rust. Each tenant has a weight; when demand exceeds capacity, throughput is divided proportionally — no single tenant can starve another. Tenants can be organized into groups so capacity splits between groups first, then among tenants within each group. Weights and group assignments update live with no restart.

### Auto router
Send `model: "auto"` and obleth picks the best available model from your registered fleet. Hard filters remove models that are down, over capacity, or missing a required capability (function calling, JSON schema, context window). The remaining candidates are scored by spare capacity and cost. An optional small-model classifier maps requests to intent tags (`coding`, `reasoning`, `vision`, `long-context`, …) to prefer the right specialist; heuristics cover it when the classifier is unavailable.

### Slurm provisioner
A companion service manages the lifecycle of inference servers running on HPC clusters via `slurmrestd`. Register a model with a partition, GRES, container image, and launch command — the provisioner submits Slurm jobs, health-probes each node as it comes up, promotes healthy replicas into the gateway's routing table, and resubmits automatically when a job is preempted or dies. Scale up or down by changing `target_replicas` in the dashboard; the provisioner reconciles without any manual job management.

### Per-request flow view
Every request can be traced through the gateway's own span recorder: auth resolve → auto route → fairshare admission → cache lookup → boon execution → upstream call. The dashboard renders these as an interactive node graph, with duration bars and attributes on each step. No external trace collector is required; spans are stored in ClickHouse alongside usage data.

### Local-first by default
Private, LAN, and loopback addresses are valid upstream targets out of the box — no allowlist needed for cluster-internal inference endpoints. Only link-local and cloud-metadata addresses (169.254.x.x, 100.64.x.x, IMDSv2) are blocked by default. Flip `OBLETH_BLOCK_PRIVATE_NETWORKS=1` for strict mode and add explicit CIDRs if needed.

### Time-of-use access windows
Tenant keys can carry an activation date, an expiry date, and recurring weekly windows evaluated in the tenant's own timezone. Outside those windows the key is rejected with `403` — useful for off-peak batch tenants, time-boxed trial access, or scheduled maintenance.

### Live configuration — no restarts
Model weights, tenant weights, rate limits, budgets, boon settings, and model health windows all take effect immediately from the dashboard or Management API. The data plane reads configuration from an in-process cache that reloads in the background; there is no hot path coupling to the control plane.

### Model boons
Runtime-toggleable capabilities the gateway grants to models that lack native support — off by default, no restart to change. **Vision** relays image parts to a designator model and swaps in the description. **Structured output** enforces `response_format` JSON schemas and optionally repairs the response. **Gateway tool loop** injects registered MCP server tools into plain chat requests, executes tool calls against the MCP upstream, and loops until a final answer — streamed live. Any boon failure leaves the request unchanged.

### Performance
The data plane is a thin async Rust service. The model registry uses a lock-free `ArcSwap` so reads never contend with refreshes. The fairshare scheduler runs in a single Tokio task with O(tenants) dispatch. If Redis or ClickHouse become unavailable, the data plane fails open from its in-process cache and replays telemetry on recovery.

## Quick start (Docker)

```bash
cd deploy/docker && cp .env.example .env
docker compose --profile benchmark --profile edge --profile observability up -d
```

Open the dashboard at <http://localhost:3002>.

### Create a tenant and call the gateway

```bash
TOKEN=dev-admin-token
TID=$(curl -s -XPOST localhost:9180/api/v1/tenants \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"chatbot","weight":500,"tokens_per_minute":2000000}' | jq -r .id)

SECRET=$(curl -s -XPOST localhost:9180/api/v1/tenants/$TID/keys \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"name":"prod"}' | jq -r .secret)

curl -s localhost/v1/chat/completions \
  -H "Authorization: Bearer $SECRET" -H 'Content-Type: application/json' \
  -d '{"model":"benchmark-endpoint","messages":[{"role":"user","content":"hi"}],"max_tokens":32}'
```

Full API spec: `GET http://localhost:9180/api/v1/openapi.json`.

### Kubernetes

```bash
helm install obleth oci://ghcr.io/thediymaker/charts/obleth --version <X.Y.Z> -f my-values.yaml
```

See `deploy/k8s/README.md` for post-install steps and production configuration.

## Configuration

| Variable | Purpose |
| --- | --- |
| `OBLETH_UPSTREAM_BASE_URL` | default upstream (each model can override with its own `api_base`) |
| `OBLETH_DATABASE_URL` / `OBLETH_REDIS_URL` / `OBLETH_CLICKHOUSE_URL` | the three datastores |
| `OBLETH_ADMIN_TOKEN` | Management API bearer token (required) |
| `OBLETH_FAIL_OPEN` | admit when Redis is down (default `true`) |

Credentials in `*.env.example` and `values.yaml` are development examples only — replace them and add TLS before deploying anywhere real.

## Documentation

Full architecture, scheduler internals, Slurm provisioner setup, auto routing and the classifier, budgets, boons, MCP integration, alerting, and the configuration reference live at **[obleth.com](https://obleth.com)**.

For contribution workflow and security reporting, see [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

## License

[Business Source License 1.1](LICENSE). Source-available; each release converts to [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0) four years after publication. Free for internal, academic, research, and educational use. You may not offer obleth as a hosted service or distribute a competing gateway product derived from it until the Change Date. Contact the maintainers for commercial licensing.
