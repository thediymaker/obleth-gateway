# Kubernetes / Helm

Install the chart from the repo root:

```bash
helm install obleth deploy/k8s/obleth \
  --namespace obleth \
  --create-namespace \
  -f my-values.yaml
```

`my-values.yaml` must supply required secrets (`obleth.adminToken`, datastore
passwords, dashboard credentials). Use placeholders in git; inject real values
via `--set`, a local untracked file, or a secrets manager.

Full value reference: [docs.obleth.dev — Helm Values](https://docs.obleth.dev/docs/reference/helm-values).

## What the chart starts

A self-contained demo install brings up:

| Workload | Purpose |
| --- | --- |
| obleth (3 replicas + HPA) | Data plane, Management API, metrics |
| control-plane | Dashboard |
| postgres, redis, clickhouse | Bundled datastores |
| benchmark-backend | Default upstream when no models are registered |

All pods should reach `Running` before post-install configuration.

## Post-install (required)

Helm does **not** register models or create tenant API keys. A fresh install has
an empty model registry until you configure it.

### 1. Reach the Management API

```bash
kubectl port-forward -n obleth svc/obleth 9090:9090
```

Use your cluster Ingress or an internal Service URL instead if you already expose
`:9090`.

### 2. Create a tenant and mint a key

There is no shared proxy key. Each client needs a tenant-scoped `sk_...` secret:

```bash
TOKEN=<your-OBLETH_ADMIN_TOKEN>

TID=$(curl -s -X POST http://localhost:9090/api/v1/tenants \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"my-team","weight":200,"tokens_per_minute":500000}' \
  | jq -r .id)

SECRET=$(curl -s -X POST "http://localhost:9090/api/v1/tenants/$TID/keys" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"prod"}' \
  | jq -r .secret)
```

Store `SECRET` immediately — it is shown once.

### 3. Register models

```bash
curl -s -X POST http://localhost:9090/api/v1/models \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "model_name": "my-model",
    "upstream_model": "meta-llama/Llama-3-8b-instruct",
    "api_base": "http://aibrix-gateway.aibrix.svc.cluster.local:8080/v1",
    "enabled": true
  }'
```

| Field | Rule |
| --- | --- |
| `api_base` | Provider **base** URL ending in `/v1`, not a full endpoint path |
| `upstream_model` | Bare model id sent to the upstream (as vLLM/Aibrix expect it) |
| `model_name` | Client-facing alias; what callers pass as `"model"` |

You can also import models from the control-plane dashboard (Models → Import).

### 4. Call the data plane

```bash
curl -s http://localhost:8080/v1/chat/completions \
  -H "Authorization: Bearer $SECRET" \
  -H "Content-Type: application/json" \
  -d '{"model":"my-model","messages":[{"role":"user","content":"hi"}],"max_tokens":32}'
```

Port-forward `svc/obleth` on `:8080`, or use your Ingress on `ingress.servicePort`.

## Common gotchas

### SSRF and in-cluster `api_base`

By default, obleth allows private/LAN upstream addresses when registering models.
Cluster DNS names such as `*.svc.cluster.local` typically resolve to RFC1918
addresses and work without `obleth.allowedPrivateCidrs`.

If you enable **strict SSRF** (`OBLETH_BLOCK_PRIVATE_NETWORKS=1`), private
targets are rejected unless listed:

```yaml
obleth:
  allowedPrivateCidrs: "10.0.0.0/8"
```

Model registration failures from the SSRF guard return `400` from the Management
API with a blocked-host message.

### Secrets in values files

Do not commit production tokens or passwords. Prefer:

```bash
helm install obleth deploy/k8s/obleth \
  --set obleth.adminToken="$(openssl rand -hex 32)" \
  --set postgres.password="$(openssl rand -hex 16)" \
  ...
```

Or maintain a local `values-prod.yaml` that stays out of git.

## Production topology

Disable bundled dependencies and point at managed services:

```yaml
postgres:
  enabled: false
  external:
    url: postgres://user:pass@my-pg:5432/obleth
redis:
  enabled: false
  external:
    url: redis://my-redis:6379
clickhouse:
  enabled: false
  external:
    url: http://my-clickhouse:8123
benchmarkBackend:
  enabled: false
```

See [Modular deploy](https://docs.obleth.dev/docs/guides/modular-deploy) on
docs.obleth.dev.
