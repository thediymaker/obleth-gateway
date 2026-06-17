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
2. **List owned Slurm jobs** from `slurmrestd`, filtered to this gateway's jobs by
   job-name prefix (`OBLETH_PROVISIONER_JOB_PREFIX`, default `obleth-`).
3. For each model:
   - **Probe** any `starting` replicas whose Slurm job is `Running` (single HTTP
     health check against the replica's `health_path` on its node + serving port).
   - **Reconcile** to `target_replicas`:
     - **Submit** new Slurm jobs when below target.
     - **Promote** a probed-healthy `starting` replica into obleth's model
       endpoint pool (registers its `api_base`).
     - **Mark lost** replicas whose Slurm job vanished or finished (preempted),
       and detach their endpoint; the next pass resubmits to restore target.
     - **Cancel** excess jobs when above target (pending first, then starting,
       then healthy; oldest first within a tier).
     - **GC** `lost` replica rows older than
       `OBLETH_PROVISIONER_LOST_RETENTION_SECS`.
4. **Drain models that left the managed set.** Any replica whose model has been
   **disabled** (`enabled = false`) or had its managed spec **deleted** is
   reconciled toward target 0: its jobs are cancelled, its endpoint detached,
   and its rows GC'd. Disabling a model is a safe, reversible "stop hosting" —
   the spec is kept so it can be re-enabled later.
5. **Cancel orphan jobs.** Any Slurm job owned by this gateway (matched by name
   prefix) that has **no replica row tracking it** is cancelled. This recovers
   the rare case where a prior tick submitted a job but failed to record its
   replica row, which would otherwise leak a live GPU allocation until its time
   limit.

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
| `OBLETH_PROVISIONER_LOST_RETENTION_SECS` | optional | `900` | how long `lost` replica rows are kept before GC |
| `OBLETH_PROVISIONER_JOB_PREFIX` | optional | `obleth-` | job-name prefix used to find this gateway's jobs |

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
`account`, `qos`, `time_limit`, `image`, `launch_command`, `serving_port`,
`health_path`, and `target_replicas`. Nothing cluster-specific is compiled into
the binary — change the spec, not the code.

## slurmrestd version note

> **Verify the submit payload against your `slurmrestd` version's schema** at
> `/openapi/v3`, and set `OBLETH_SLURMRESTD_API_VERSION` to match (default
> `v0.0.40`). The version-sensitive JSON is isolated in `src/slurm.rs`.

`slurmrestd`'s job-submit schema changes between API versions. If submits fail
with schema/validation errors, compare the payload built in `src/slurm.rs`
against your cluster's `/openapi/v3` and adjust the version segment accordingly.

## v1 limitations

- **Fixed serving port per model.** The serving port comes from the model spec;
  there is no replica self-registration yet, so every replica of a model serves
  on the same port.
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


