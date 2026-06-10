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

Full value reference: [obleth.com — Helm Values](https://obleth.com/docs/reference/helm-values).

## Pick a storage scenario

The bundled Postgres, Redis, and ClickHouse support three storage models. Ready-
made values files live in [`obleth/examples/`](obleth/examples/).

| Scenario | When to use | Values file | Data on restart |
| --- | --- | --- | --- |
| **Persistent (PVC)** | Self-hosted single cluster, no external DBs yet | [`values-persistent.yaml`](obleth/examples/values-persistent.yaml) | **Survives** (PVCs) |
| **Ephemeral** | Throwaway demo / CI / kicking the tires | [`values-ephemeral.yaml`](obleth/examples/values-ephemeral.yaml) | **LOST** (emptyDir) |
| **External** | Production; managed or self-hosted datastores | [`values-external.yaml`](obleth/examples/values-external.yaml) | Managed by you |

```bash
helm install obleth deploy/k8s/obleth -n obleth --create-namespace \
  -f deploy/k8s/obleth/examples/values-persistent.yaml \
  --set obleth.adminToken="$(openssl rand -hex 32)" \
  --set postgres.password="$(openssl rand -hex 16)" \
  --set clickhouse.password="$(openssl rand -hex 16)" \
  --set controlPlane.dashboardPassword="$(openssl rand -hex 16)" \
  --set controlPlane.dashboardSessionSecret="$(openssl rand -hex 32)"
```

### 1. Persistent (PVC) — recommended for self-hosting

Each bundled datastore has a `persistence` block. When `enabled: true` (the
default) the chart provisions a PVC and uses the `Recreate` rollout strategy so
data survives pod restarts and reschedules. Requires a `StorageClass` (most
clusters ship a default; `kubectl get storageclass` to check).

```yaml
postgres:
  persistence:
    enabled: true
    size: 10Gi
    storageClass: ""        # "" = cluster default; set a name to pin one
    accessMode: ReadWriteOnce
redis:
  persistence: { enabled: true, size: 1Gi }
clickhouse:
  persistence: { enabled: true, size: 20Gi }
```

### 2. Ephemeral (test only) — data is lost on restart

Set `persistence.enabled: false` to use `emptyDir`. **Every datastore pod
restart wipes all tenants, keys, models, config, and usage history.** Use only
for demos/CI or clusters with no `StorageClass`.

```yaml
postgres:   { persistence: { enabled: false } }
redis:      { persistence: { enabled: false } }
clickhouse: { persistence: { enabled: false } }
```

### 3. External — managed or self-hosted datastores

Disable the bundled containers and point obleth at endpoints you operate. This is
the recommended production topology — stateful systems get real backups/HA.

```yaml
postgres:
  enabled: false
  external: { url: "postgres://obleth:pass@my-pg:5432/obleth" }
redis:
  enabled: false
  external: { url: "redis://my-redis:6379" }
clickhouse:
  enabled: false
  user: obleth
  password: "pass"          # obleth authenticates with these even when external
  db: obleth
  external: { url: "http://my-clickhouse:8123" }
benchmarkBackend:
  enabled: false
```

Need to stand the external datastores up quickly in Docker? Use
[`deploy/docker/datastores.compose.yml`](../docker/datastores.compose.yml):

```bash
cd deploy/docker
cp datastores.env.example datastores.env   # edit the passwords
docker compose --env-file datastores.env -f datastores.compose.yml up -d
```

See [obleth.com — Self-Hosting](https://obleth.com/docs/guides/self-hosting) for
the full walkthrough.

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
kubectl port-forward -n obleth svc/obleth 9180:9180
```

Use your cluster Ingress or an internal Service URL instead if you already expose
`:9180`.

### 2. Create a tenant and mint a key

There is no shared proxy key. Each client needs a tenant-scoped `sk_...` secret:

```bash
TOKEN=<your-OBLETH_ADMIN_TOKEN>

TID=$(curl -s -X POST http://localhost:9180/api/v1/tenants \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"my-team","weight":200,"tokens_per_minute":500000}' \
  | jq -r .id)

SECRET=$(curl -s -X POST "http://localhost:9180/api/v1/tenants/$TID/keys" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"prod"}' \
  | jq -r .secret)
```

Store `SECRET` immediately — it is shown once.

### 3. Register models

```bash
curl -s -X POST http://localhost:9180/api/v1/models \
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

### Datastore pods stuck `Pending` (PVC never binds)

A bundled datastore pod that stays `Pending` after install almost always means
its PVC did not bind. Check:

```bash
kubectl get pvc -n obleth
kubectl describe pvc <release>-postgres -n obleth
```

If the PVC shows `no persistent volumes available for this claim and no storage
class is set`, the cluster has **no default StorageClass** and you left
`persistence.storageClass` empty (the chart then omits `storageClassName`, so
the claim matches nothing). The persistent scenario assumes either a default
StorageClass or an explicit class. Fix with one of:

```bash
# Pin a class per datastore (find names with: kubectl get storageclass)
--set postgres.persistence.storageClass=local-path \
--set redis.persistence.storageClass=local-path \
--set clickhouse.persistence.storageClass=local-path

# ...or mark one StorageClass the cluster default (one-time, cluster-wide)
kubectl patch storageclass <name> \
  -p '{"metadata":{"annotations":{"storageclass.kubernetes.io/is-default-class":"true"}}}'
```

`local-path` (k3s/k0s) and similar node-local provisioners use
`volumeBindingMode: WaitForFirstConsumer` — the PVC binds only once a consuming
pod is scheduled, and the volume is then pinned to that node. That is fine for a
single-cluster self-host, but the data does **not** move if the node is lost;
use the External scenario for true HA.

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

For production, use the **External** storage scenario above: disable the bundled
datastores and point obleth at managed/operator endpoints (CloudNativePG for
Postgres, an operator/managed ClickHouse, and HA Redis). Keep
`benchmarkBackend.enabled: false` and set a real `obleth.upstreamBaseUrl`.

See [Modular deploy](https://obleth.com/docs/guides/modular-deploy) and
[Self-Hosting](https://obleth.com/docs/guides/self-hosting) on obleth.com.
