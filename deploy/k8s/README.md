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

## Upgrading an existing release

To apply chart or values changes to a release that already exists, use `helm
upgrade --install` — **not** `helm install`. A plain `helm install` against a
live release fails with `cannot re-use a name that is still in use`.

```bash
helm upgrade --install obleth deploy/k8s/obleth \
  --namespace obleth \
  -f my-values.yaml \
  --wait
```

Point it at the **same chart source you installed from**. The command above uses
the local chart path; to track a published release instead, use the OCI chart
pinned to a version:

```bash
helm upgrade --install obleth oci://ghcr.io/thediymaker/charts/obleth \
  --version 0.5.0 \
  --namespace obleth \
  -f my-values.yaml \
  --wait
```

The two sources are not interchangeable: editing the chart's own
`deploy/k8s/obleth/values.yaml` only changes the **local** path — the OCI
artifact carries the values published with that version. Keep per-deployment
settings in your own `-f` values file so they apply regardless of source.
`--wait` blocks until the rolled resources report Ready.

> When a value that renders into the chart's obleth Secret changes
> (`adminToken`, the database URL, the Redis URL, `postgres.password`,
> `redis.password`, `clickhouse.password`, `encryptionKey`, `apiKeyPepper`),
> the gateway and the bundled Redis roll automatically (`checksum/secret` pod
> annotation). Other pods do not: after changing `adminToken`, restart
> `deployment/obleth-control-plane` (and `deployment/obleth-provisioner` if
> enabled), and restart the control-plane if its `DATABASE_URL` changed. With
> `obleth.existingSecret`, the chart cannot see Secret changes, so restart the
> affected deployments yourself, e.g. `kubectl rollout restart
> deployment/obleth-obleth -n obleth` (the gateway is `<release>-obleth`).

### Upgrading to a release with Redis authentication

The bundled Redis now requires a password, and `redis.password` is a required
value whenever `redis.enabled=true`. Generate it once and keep it with your
other deployment secrets, then pass the same value on every upgrade:

```bash
REDIS_PASSWORD="$(openssl rand -hex 32)"   # store this; reuse it on later upgrades
helm upgrade --install obleth deploy/k8s/obleth \
  --namespace obleth \
  -f my-values.yaml \
  --set redis.password="$REDIS_PASSWORD" \
  --wait
```

The gateway now reads `OBLETH_REDIS_URL` from the obleth Secret (rendered as
`redis://:<password>@<release>-redis:6379`) instead of a plain environment
value, and the bundled datastores read their passwords from the same Secret.
With `obleth.existingSecret`, add these keys to the pre-created Secret before
upgrading:

- `OBLETH_REDIS_URL` — always; embed the password for the bundled Redis, or
  use your external Redis URL.
- `REDIS_PASSWORD` — when `redis.enabled=true`; must match the password in
  `OBLETH_REDIS_URL`.
- `POSTGRES_PASSWORD` — when `postgres.enabled=true`.
- `OBLETH_CLICKHOUSE_PASSWORD` (already required) is also read by the bundled
  ClickHouse when `clickhouse.enabled=true`.

Use URL-safe characters for the Redis password (hex from `openssl rand -hex`),
since it is embedded in the connection URL.

## Pick a storage scenario

The bundled Postgres, Redis, and ClickHouse support three storage models. Ready-
made values files live in [`obleth/examples/`](obleth/examples/).

| Scenario | When to use | Values file | Data on restart |
| --- | --- | --- | --- |
| **Persistent (PVC)** | Self-hosted single cluster, no external DBs yet | [`values-persistent.yaml`](obleth/examples/values-persistent.yaml) | **Survives** (PVCs) |
| **Ephemeral** | Throwaway demo / CI / kicking the tires | [`values-ephemeral.yaml`](obleth/examples/values-ephemeral.yaml) | **LOST** (emptyDir) |
| **External** | Self-hosted with your own managed datastores | [`values-external.yaml`](obleth/examples/values-external.yaml) | Managed by you |
| **Production** | Redundant prod: external datastores + full hardening | [`values-production.yaml`](obleth/examples/values-production.yaml) | Managed by you |

The four profiles differ along two independent axes — **datastore durability**
(where state lives) and **workload posture** (how the stateless obleth pods are
hardened and made redundant):

| Profile | Datastores | Durability / HA | obleth replicas | Secrets | Use case |
| --- | --- | --- | --- | --- | --- |
| Ephemeral | Bundled, emptyDir | None — wiped on restart | 1 | `--set` | Demos, CI |
| Persistent | Bundled, PVC | Survives restarts; **no backups/HA** | 1 | `--set` | Single-cluster self-host |
| External | Yours | Whatever you operate | 3, ceiling above pool sum | `--set` | Self-host w/ managed DBs |
| Production | Yours (operator/managed) | Backups + HA + PITR (your tooling) | 3, ceiling above pool sum + PDB + anti-affinity | **existingSecret** | Redundant production |

> **Replicas share the fairshare limits.** Fairshare runs one scheduling pool
> per model (default `obleth.defaultModelMaxInFlight` slots, or the model's own
> `max_in_flight`), and `obleth.globalMaxInFlight` is a total ceiling across
> those pools — not a fairness input. Admission runs in each gateway process,
> but every replica heartbeats into Redis
> (`obleth.fairshareReplicaHeartbeatSecs`, default 5 s) and enforces its share
> of each limit: the configured value divided by the live replica count,
> rounded up, never below 1. That covers pool sizes, the global ceiling, and
> per-tenant and per-key max in flight; weights are ratios and are not
> divided. So the numbers you set are fleet-wide whatever `obleth.replicas` is,
> and the HPA can scale the gateway without moving the concurrency that
> reaches your upstream. The limits of this:
>
> - Rounding up lets the fleet run up to one slot per replica over a limit (a
>   pool of 8 over 3 replicas is 3 + 3 + 3). Every pool rounds up on its own,
>   so keep `obleth.globalMaxInFlight` at least one slot per enabled model per
>   replica above the pool sum; the gateway warns at start-up if it is not.
> - Fairness is decided per replica, on its share. The fleet matches the
>   configured numbers, and tenants get their weighted share, only as far as
>   the Service spreads requests evenly; replicas cannot lend each other idle
>   capacity, so one replica can queue while another has free slots.
> - A new replica is counted within one heartbeat. A crashed one is counted
>   until its heartbeat expires (`obleth.fairshareReplicaTtlSecs`, default
>   15 s), so the survivors run below the configured total for up to that
>   long; a replica that shuts down cleanly deregisters once it has drained.
>   Resizing never interrupts in-flight requests.
> - If Redis is unreachable, each replica keeps the last count it read (or 1,
>   the per-replica behaviour, if it never read one) and logs the failure once.
>
> `obleth.fairshareReplicaAware: false` restores per-replica limits, where N
> replicas admit N × every number. The dashboard's fairshare page shows the
> answering replica's own counts and its share of each limit, with the live
> replica count; its history is per replica as well
> (`obleth.fairshareHistorySecs`, in memory), so with several replicas the
> chart shows whichever replica answered.

> The bundled datastores are single plain Deployments with no replication or
> backups — intentionally. Making them HA is the job of purpose-built operators
> (CloudNativePG, Altinity ClickHouse, an HA Redis), so production points obleth
> at external datastores rather than reimplementing stateful HA in this chart.


```bash
helm install obleth deploy/k8s/obleth -n obleth --create-namespace \
  -f deploy/k8s/obleth/examples/values-persistent.yaml \
  --set obleth.adminToken="$(openssl rand -hex 32)" \
  --set postgres.password="$(openssl rand -hex 16)" \
  --set redis.password="$(openssl rand -hex 32)" \
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
  external: { url: "redis://:pass@my-redis:6379" }
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

## Production hardening

These apply to every profile but are tuned for the **Production** one. All are
toggles in `values.yaml`, on by sensible defaults.

- **Restricted Pod Security Standard.** The stateless workloads (obleth,
  control-plane, benchmark-backend) run with `runAsNonRoot`, `seccompProfile:
  RuntimeDefault`, `allowPrivilegeEscalation: false`, and all capabilities
  dropped (`podSecurityContext` / `securityContext`). UID is not pinned — the
  images already ship distinct non-root users (obleth 10001, control-plane
  1000). The bundled datastores are excluded on purpose (their official images
  manage their own users).
- **Pre-created Secrets (`existingSecret`).** Point the chart at a Secret you
  created out-of-band so real credentials never enter values files or
  `--set`/CLI history. `obleth.existingSecret` must carry `OBLETH_ADMIN_TOKEN`,
  `OBLETH_DATABASE_URL`, `OBLETH_REDIS_URL`, `OBLETH_CLICKHOUSE_PASSWORD`,
  `OBLETH_ENCRYPTION_KEY`, `OBLETH_API_KEY_PEPPER`, `OBLETH_SLACK_WEBHOOK_URL`,
  (optional — only if you want the JWT bearer path) `OBLETH_JWT_ISSUERS`, and,
  for each bundled datastore you keep enabled, `POSTGRES_PASSWORD` /
  `REDIS_PASSWORD`;
  `controlPlane.existingSecret` must carry `DASHBOARD_PASSWORD`,
  `DASHBOARD_SESSION_SECRET`, `DATABASE_URL`, and (for the break-glass admin and
  SSO) `DASHBOARD_ADMIN_EMAIL`, `BETTER_AUTH_URL`, `OIDC_PROVIDERS`. See
  [`values-production.yaml`](obleth/examples/values-production.yaml).
- **Spread + disruption protection.** `affinity.antiAffinity` (`soft`/`hard`)
  spreads obleth replicas across nodes; `podDisruptionBudget` keeps a minimum
  available during drains/upgrades (rendered only when the minimum replica
  count is above 1: `hpa.minReplicas > 1` with the HPA enabled, otherwise
  `replicas > 1`; a PDB over a single pod would block node drains).
- **NetworkPolicy (opt-in).** `networkPolicy.enabled: true` restricts bundled
  Postgres to the obleth and control-plane pods, bundled Redis and ClickHouse
  to the obleth pods, and the Management API port (9180) to the control-plane
  and provisioner. The data-plane port (8080) stays open to clients and ingress
  controllers, and the metrics port (9091) stays open so a Prometheus in
  another namespace can scrape it. Requires a CNI that enforces NetworkPolicy;
  inert otherwise. The datastore policies are a no-op for external datastores.

## What the chart starts

A self-contained demo install brings up:

| Workload | Purpose |
| --- | --- |
| obleth | Data plane, Management API, metrics |
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

## Dashboard login & SSO

The dashboard signs in with an **email address and password**. A local
"break-glass" admin is seeded on first boot and always works, even when an
identity provider is unreachable. OIDC single sign-on is optional and layered on
top.

### Break-glass admin (required)

| Value | Maps to | Notes |
| --- | --- | --- |
| `controlPlane.dashboardAdminEmail` | `DASHBOARD_ADMIN_EMAIL` | Must be a real email address — a bare username is rejected. Defaults to `admin@example.com`. |
| `controlPlane.dashboardPassword` | `DASHBOARD_PASSWORD` | At least 8 characters, or the admin is not seeded. |

The account is created only on first boot when no admin exists yet. If you set an
invalid email or a too-short password, **no admin is created and you cannot log
in** — fix the values and reinstall/restart.

### OIDC single sign-on (optional)

```yaml
controlPlane:
  # External, browser-facing dashboard URL — used to build OIDC redirect URIs.
  betterAuthUrl: "https://dashboard.example.com"
  oidcProviders: |
    [{"providerId":"globus","displayName":"Globus","discoveryUrl":"https://auth.globus.org/.well-known/openid-configuration","clientId":"ID","clientSecret":"SECRET","scopes":["openid","email","profile"]}]
```

### Mapping user fields from non-standard claims

An optional `claims` object chooses which OIDC claim each user field is read
from, overriding the standard claim of the same name:

```yaml
controlPlane:
  oidcProviders: |
    [{"providerId":"globus","displayName":"Globus",
      "discoveryUrl":"https://auth.globus.org/.well-known/openid-configuration",
      "clientId":"ID","clientSecret":"SECRET",
      "scopes":["openid","email","profile"],
      "claims":{"email":"preferred_username"}}]
```

Mappable fields are `email`, `name` and `image` — the only profile fields
better-auth writes onto a user.

**Why you will probably want this.** Institutional IdPs routinely release an
`email` that is not the identifier the institution keys accounts on. Globus is
a clear case: for an Example University identity it sends

```
email               Jane.Doe@university.example   <- a display alias
preferred_username  user@university.example       <- the canonical institutional id
```

Without a mapping, obleth keys the account on the alias, so the same human
signs in and arrives as a second, unrecognised user — and if a local admin
already exists under the canonical address, the two never connect.

To see what your IdP actually sends, decode the stored ID token rather than
guessing:

```bash
psql "$DATABASE_URL" -At -c \
  "select \"idToken\" from account where \"providerId\"='globus' limit 1" \
  | cut -d. -f2 | base64 -d 2>/dev/null | python3 -m json.tool
```

A missing, empty, or non-string mapped claim falls back to the standard claim,
so a mapping can never blank out a field the IdP did supply. An unknown field
name is rejected with an explicit error rather than accepted and ignored.

`overrideUserInfo: true` re-applies the profile (and therefore the mapping) to
an **existing** user on every sign-in, instead of only at sign-up. Set it when
correcting a mapping after users already exist — otherwise the fix only applies
to accounts created from then on, and anyone who already signed in keeps the
wrong address.

> Changing `claims.email` changes the identity users are keyed on. Existing
> accounts are **not** merged: obleth finds a returning user by the provider's
> `sub`, so they keep their original row and (unless `overrideUserInfo` is set)
> their original email. Linking an SSO login to a pre-existing local account
> with the same address is a separate feature — better-auth's
> `account.accountLinking`, which obleth does not enable, and which deserves
> care because auto-linking on an unverified email is an account-takeover
> vector.

An optional `authentication` field selects how the client authenticates to the
IdP's **token** endpoint: omit it for the default, which sends
`client_id`/`client_secret` in the request body (`client_secret_post`), or set
`"basic"` for an IdP that only accepts HTTP Basic (`client_secret_basic`).

Most providers accept both regardless of what their discovery document
advertises — Globus, for one, advertises only `client_secret_basic` but
authenticates body credentials fine, so the example above needs no override.
Reach for this only when a provider genuinely refuses.

The symptom that points here is distinctive: the IdP login succeeds, the
browser comes back to the dashboard, and *then* the callback errors — because
the failure is in the server-to-server code exchange, not in anything visible
in the browser flow. If you see that, check the provider's
`token_endpoint_auth_methods_supported` and set `authentication` to match. Only
`"basic"` and `"post"` are accepted; the discovery-document spellings
(`client_secret_basic` / `client_secret_post`) are rejected with an explicit
error rather than silently falling back to the body.

An optional `pkce: true` sends a PKCE code challenge with the authorization
request and the matching verifier at the token exchange (RFC 7636), so an
intercepted authorization code cannot be redeemed on its own. It is off by
default, matching better-auth; turn it on for any provider that supports it
(Dex, Keycloak, Globus, Entra ID, and Okta all do).

Register this redirect URI with your identity provider (one per `providerId`):

```
https://<betterAuthUrl host>/api/auth/oauth2/callback/<providerId>
```

### New users start pending

When someone signs in via SSO for the first time, their account is created with
**no access**. An admin grants access on the dashboard's **Users** screen by
assigning a role (`admin` or `user`) and a tenant. Users with the `user` role
get a self-service portal (model list, their own API keys and usage); admins get
the full dashboard.

### Production (`existingSecret`)

With `controlPlane.existingSecret` set, the chart renders **no** control-plane
Secret, so the pre-created Secret must carry every key: `DASHBOARD_PASSWORD`,
`DASHBOARD_SESSION_SECRET`, `DATABASE_URL`, `DASHBOARD_ADMIN_EMAIL`,
`BETTER_AUTH_URL`, and `OIDC_PROVIDERS`. `DASHBOARD_SESSION_SECRET` is reused as
`BETTER_AUTH_SECRET` when the latter is absent. See
[`values-production.yaml`](obleth/examples/values-production.yaml).

Full configuration reference and screenshots: the
[Dashboard SSO guide](https://obleth.com/docs/guides/dashboard-sso).

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
  --set redis.password="$(openssl rand -hex 32)" \
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
