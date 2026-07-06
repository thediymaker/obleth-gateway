# obleth-provisioner

## What it is

`obleth-provisioner` is an **optional plugin service** — it is **not** part of the
obleth core stack. It keeps your *managed* models alive on a preemptible
[Slurm](https://slurm.schedmd.com/) cluster by talking to `slurmrestd`.

It runs a simple reconcile loop: for each enabled managed model it holds
`target_replicas` healthy replicas on Slurm, replaces preempted/lost jobs
("whack-a-mole"), and promotes each healthy replica into obleth's existing model
endpoint pool so the gateway can route traffic to it.

It is a community tool. **Nothing cluster-specific is compiled in.** The
slurmrestd **connection details** (URL, version, user, JWT) and the master
**enable switch** live in system-wide settings (dashboard → Settings → Slurm),
stored encrypted in Postgres and fetched from the Management API each tick — so
they can be changed without restarting this service. Every **per-model** specific
(partition, GRES, node counts, image, launch command, serving port, health path,
account, QoS, constraints, target replicas, etc.) is configured **per model**
through the obleth Management API. Only the Management API endpoint/token and a
few cadence knobs come from this service's environment (below). The same binary
works on any Slurm cluster reachable via `slurmrestd`.

## How it works

Every tick (`OBLETH_PROVISIONER_INTERVAL_SECS`, default 15s):

0. **Read Slurm settings** from the Management API. If Slurm is **not configured**
   or the master switch is **disabled**, the provisioner idles this tick (takes no
   action). Otherwise it builds a slurmrestd client from the current settings and
   proceeds. *(When Slurm is globally disabled the provisioner idles and does
   **not** drain existing replicas — drain-on-global-disable is a future
   enhancement. Per-model disable still drains; see step 4.)*
1. **List enabled managed models** from the obleth Management API.
2. **Look up the tracked Slurm jobs** in `slurmrestd`, **by job id** — only the
   jobs the gateway's replica rows point at, never a listing of the whole
   controller (which on a busy cluster is huge). A clean "not found" means the
   job is gone; a transport error holds the whole tick (see fail-safe below).
3. For each model:
   - **Probe** replicas whose Slurm job is `Running`: every port in the
     replica's window (`serving_port + slot*OBLETH_PORT_SPAN`) is checked
     concurrently against the model's `health_path`, and the lowest healthy
     port wins. This covers replicas awaiting promotion **and** already-healthy
     ones (for self-healing, below).
   - **Reconcile** to `target_replicas`:
     - **Submit** new Slurm jobs when below target.
     - **Promote** a probed-healthy `starting` replica into obleth's model
       endpoint pool (registers its `api_base`), then fire a throwaway 1-token
       **warmup** inference at it (detached) so the slow cold first-token cost —
       which `/health` returning 200 does *not* cover — is paid here instead of
       by the first real user. Best-effort; disabled by
       `OBLETH_PROVISIONER_WARMUP_TIMEOUT_SECS=0`.
     - **Mark lost** replicas whose Slurm job vanished or finished (preempted),
       and detach their endpoint; the next pass resubmits to restore target.
     - **Self-heal zombie jobs.** A `healthy` replica whose job still reports
       RUNNING is restarted (endpoint deregistered, job cancelled, fresh
       replica submitted) on either of two signals: it fails
       `OBLETH_PROVISIONER_RESTART_AFTER_FAILURES` consecutive provisioner
       probes (default 3; `0` disables self-heal entirely), **or** the
       gateway's own health check of its registered endpoint — a real 1-token
       inference — has been `unhealthy` for 2+ consecutive recent checks. The
       second signal catches servers that still answer metadata GETs but hang
       on actual inference. At most **one** self-heal restart per model per
       tick, so a probe-side network problem rolls a fleet gradually instead
       of mass-cancelling it.
     - **Cancel** excess jobs when above target (pending first, then starting,
       then healthy; oldest first within a tier).
     - **GC** `lost` replica rows older than
       `OBLETH_PROVISIONER_LOST_RETENTION_SECS`.
4. **Drain models that left the managed set.** Any replica whose model has been
   **disabled** (`enabled = false`) or had its managed spec **deleted** is
   reconciled toward target 0: its jobs are cancelled, its endpoint detached,
   and its rows GC'd. Disabling a model is a safe, reversible "stop hosting" —
   the spec is kept so it can be re-enabled later.

**Orphan jobs are prevented at the source**, not swept: if a submit succeeds but
recording its replica row fails, the executor immediately cancels the
just-submitted job. There is no periodic cluster-wide orphan scan (that would
mean listing the whole controller).

**Tick outcome reporting.** Each settings fetch carries the previous tick's
outcome (`ok` / `idle` / `error` + detail) to the gateway, which tracks the last
*successful* reconcile. The dashboard uses this to distinguish "provisioner
alive and reconciling" from "alive but every tick failing — replica state
frozen", and the gateway raises an alert when reconciliation has been failing
for more than 10 minutes.

**Fail-safe.** If either the Management API or `slurmrestd` is unreachable, the
tick bails out *before* computing any plan and takes **no destructive action on
stale data** — it simply logs a warning and retries on the next tick. The
provisioner never cancels or marks-lost based on data it could not refresh.

## Configuration

The slurmrestd **connection** (URL, version, user, JWT) and the master **enable**
switch are configured in the dashboard (**Settings → Slurm**), persisted encrypted
in Postgres and fetched from the Management API each tick. Only the Management API
endpoint/token and cadence knobs come from the environment:

| Env var | Required? | Default | Meaning |
|---|---|---|---|
| `OBLETH_ADMIN_TOKEN` | **required** | — | Bearer token for the obleth Management API |
| `OBLETH_ADMIN_BASE_URL` | optional | `http://localhost:9180` | obleth admin API base URL |
| `OBLETH_PROVISIONER_INTERVAL_SECS` | optional | `15` | reconcile tick interval |
| `OBLETH_PROVISIONER_HEALTH_TIMEOUT_SECS` | optional | `5` | per-replica health probe timeout |
| `OBLETH_PROVISIONER_WARMUP_TIMEOUT_SECS` | optional | `600` | budget for the post-promotion warmup inference; `0` disables warmup |
| `OBLETH_PROVISIONER_LOST_RETENTION_SECS` | optional | `900` | how long `lost` replica rows are kept before GC |
| `OBLETH_PROVISIONER_RESTART_AFTER_FAILURES` | optional | `3` | restart a healthy replica after this many consecutive failed probes while its job reports RUNNING (zombie job); `0` disables self-heal |
| `OBLETH_PORT_SPAN` | optional | `8` | width of each replica's port window (`serving_port + slot*span`) |
| `OBLETH_PROVISIONER_JOB_PREFIX` | optional | `obleth-` | job-name prefix used to tag this gateway's jobs |

**Slurm connection settings (dashboard → Settings → Slurm), not env vars:**
`enabled`, `slurmrestd_url`, `slurmrestd_api_version`, `slurm_user`, `slurm_jwt`.
The JWT is encrypted at rest with the gateway's `OBLETH_ENCRYPTION_KEY` (the same
envelope cipher used for upstream provider keys); set that on the gateway for
production. Use the **Test connection** button there to check the JWT expiry and
ping slurmrestd.

**All cluster specifics are per-model, not env vars.** Set them on each model via:

```
PUT /api/v1/models/:id/managed
```

The managed spec carries: `partition`, `gres`, `nodes`, `constraints`, `exclude`,
`account`, `qos`, `time_limit`, `image`, `launch_command`, `script_body`,
`serving_port`, `health_path`, `target_replicas`, `min_replicas` (health floor),
and `max_job_failures` (resubmit breaker). Nothing cluster-specific is compiled
into the binary — change the spec, not the code.

## slurmrestd version note

> **Verify the submit payload against your `slurmrestd` version's schema** at
> `/openapi/v3`, and set the **API version** in the dashboard (Settings → Slurm)
> to match (default `v0.0.40`). The version-sensitive JSON is isolated in
> `src/slurm.rs`.

`slurmrestd`'s job-submit schema changes between API versions. If submits fail
with schema/validation errors, compare the payload built in `src/slurm.rs`
against your cluster's `/openapi/v3` and adjust the version segment accordingly.

## v1 limitations

- **Single-node health probe.** The health probe targets the first node of a
  job. Multi-node nodelist bracket-ranges (e.g. `gpu[01-04]`) are not expanded —
  they fall back to the raw nodelist string.
- **No autoscaling.** `target_replicas` is a fixed target. The provisioner only
  maintains that count and replaces preempted jobs (whack-a-mole); it does not
  scale up or down based on load.

## Running it

### Cargo (from the `obleth/` directory)

```sh
OBLETH_ADMIN_TOKEN=… \
OBLETH_ADMIN_BASE_URL=http://localhost:9180 \
cargo run -p obleth-provisioner
```

Then configure the Slurm connection in the dashboard (Settings → Slurm) and flip
the enable switch — the running provisioner picks it up on the next tick.

### Docker Compose

The provisioner is gated behind the `slurm` profile. Opt in via **config** — add
`slurm` to `COMPOSE_PROFILES` in `deploy/docker/.env` (see `.env.example`) — and
it builds and starts with the same one command as the rest of the stack:

```sh
cd deploy/docker
docker compose up -d --build      # builds + starts everything, provisioner included
docker compose down               # tears it all down
```

Running from `deploy/docker/` (no `-f` flags) lets Compose auto-load `.env` (so
the profile activates) and auto-merge a local `docker-compose.override.yml` if
you use one. The container only needs `OBLETH_ADMIN_TOKEN` (already set for the
core stack) — the Slurm connection details are configured in the dashboard, not
in `.env`.

### systemd

A ready-to-use unit ships at `deploy/systemd/obleth-provisioner.service`. Put the
`OBLETH_*` variables in `/etc/obleth/provisioner.env`, then:

```sh
sudo cp deploy/systemd/obleth-provisioner.service /etc/systemd/system/
sudo cp obleth/target/release/obleth-provisioner /usr/local/bin/
sudo systemctl daemon-reload
sudo systemctl enable --now obleth-provisioner
```

## Documentation

Full setup, Apptainer image requirements, the per-model spec reference, and
troubleshooting (node reachability, the two health systems, the `/v1` endpoint
convention) live in the docs:
[Slurm Provisioning guide](https://github.com/thediymaker/obleth-gateway) →
`obleth-docs` · `contents/docs/guides/slurm-provisioning`.


