# obleth-provisioner

## What it is

`obleth-provisioner` is an **optional plugin service**, not part of the obleth
core stack. It keeps *managed* models running on a [Slurm](https://slurm.schedmd.com/)
cluster by talking to `slurmrestd`.

It runs a reconcile loop: for each enabled managed model it holds
`target_replicas` replicas on Slurm, replaces jobs that end, and promotes each
healthy replica into obleth's model endpoint pool so the gateway can route
traffic to it.

**Nothing cluster-specific is compiled in.** The slurmrestd connection (URL,
API version, user, JWT, node addresses), the cluster defaults (weight cache,
images, job logs folder, setup lines, Hugging Face token) and the master enable
switch live in system-wide settings, set in the dashboard under **Deployments ›
Set up Slurm** (`/deployments?slurm=1`) and fetched from the Management API
every tick, so they change without restarting this service. Every per-model
value (partition, GRES, nodes, script, serving port, health path, account, QoS,
constraints, replica count, timeouts) is set per model through the Management
API. Only the Management API endpoint and token and a few tuning knobs come from
this service's environment (below).

## How it works

Every tick (`OBLETH_PROVISIONER_INTERVAL_SECS`, default 15 s):

1. **Read the Slurm settings.** If Slurm is not configured or switched off, the
   tick idles: nothing is submitted, cancelled or drained, and running jobs are
   left as they are. Otherwise it connects to slurmrestd, resolving the API
   version when it is `auto` (the newest of v0.0.40 to v0.0.45 the server
   lists, cached for 10 minutes), and makes one authenticated read (the
   partition list) so a rejected token fails the tick even with no replicas.
   A pinned version the server doesn't serve falls back to the newest one it
   does serve, with a warning; a pin that works is kept.
2. **Look up the tracked jobs by id**: only the jobs the replica rows point at,
   never a listing of the whole controller. A clean "not found" means the job
   is gone. A transport error, or a rejected token, holds the whole tick with no
   destructive action. When every tracked job is missing at once, the
   accounting database is asked before any of them is believed gone.
3. **Probe** each running job on its batch host: the port the job recorded in
   its comment (`obleth-port=<port>`), or else every port of the replica's
   window. A port passes when the model's `health_path` answers 2xx and the
   server's `GET /v1/models`, when it lists models, includes the model's
   upstream name.
4. **Plan and apply** per model:
   - **Submit** when fewer replicas are live than `target_replicas` and the
     model isn't paused. Each replica gets a port window
     (`OBLETH_PORT_SPAN` wide from the serving port) that overlaps no other
     live replica's, across all models. Jobs are submitted with requeue off.
   - **Promote** a replica whose probe passes: register its endpoint at the
     node's resolved IP, then send a detached 1-token warm-up request
     (`OBLETH_PROVISIONER_WARMUP_TIMEOUT_SECS=0` turns it off).
   - **Demote** a serving replica whose job is queued again or now runs
     elsewhere: its endpoint is removed until it serves again.
   - **Mark lost** a replica whose job ended, remove its endpoint, and submit
     a replacement.
   - **Give up** on a running job that is not healthy `startup_timeout_mins`
     after it started, or on a queued job held for a reason Slurm will not
     clear, and count it as a failed launch.
   - **Replace before draining.** A replica near its time limit
     (`handoff_mins`), one the operator asked to restart or roll, and the last
     serving replica that fails its checks get a successor first; they are
     drained once enough replicas serve without them.
   - **Self-heal.** A serving replica whose probe has failed for
     `OBLETH_PROVISIONER_RESTART_AFTER_FAILURES` net ticks (the counter decays
     on a pass) is cancelled when another replica serves, at most one per model
     per tick. One the gateway's own inference check has failed twice in the
     last 30 minutes is replaced surge-first. When every probed replica fails
     at once, nothing restarts and the tick warns about the network path.
   - **Scale down** queued replicas newest first, then loading ones newest
     first, then serving ones oldest first. A serving replica is drained: its
     endpoint is removed, it waits 60 s, then its job is cancelled.
   - **Back off.** Failed launches (jobs that ended before serving, the
     startup timeout, stuck in the queue) count toward `max_job_failures`;
     reaching it, or a submit Slurm rejects, pauses submits for 5, 15, 30, 60
     minutes, then hourly. A launch that serves resets it.
5. **Drain models that left the managed set** (paused or removed): their jobs
   are drained and cancelled and their rows cleaned up.

**Lost submit answers.** When a submit times out or slurmrestd fails
mid-answer, the model's submits wait up to 3 minutes while the accounting
database is searched for the job by name and user; a job found there is adopted
as the replica. If recording a submitted job's replica fails, the job is
cancelled at once.

**Tick outcome reporting.** Each settings fetch carries the previous tick's
outcome (`ok`, `idle` or `error` with detail) and its warnings to the gateway.
The dashboard shows them on the Slurm connection, and the gateway raises
`slurm_reconcile_held` when ticks have failed for 10 minutes.

## Configuration

| Env var | Required? | Default | Meaning |
|---|---|---|---|
| `OBLETH_ADMIN_TOKEN` | **required** | | Bearer token for the obleth Management API |
| `OBLETH_ADMIN_BASE_URL` | optional | `http://localhost:9180` | obleth Management API base URL |
| `OBLETH_PROVISIONER_INTERVAL_SECS` | optional | `15` | reconcile tick interval |
| `OBLETH_PROVISIONER_HEALTH_TIMEOUT_SECS` | optional | `5` | per-port health probe timeout |
| `OBLETH_PROVISIONER_WARMUP_TIMEOUT_SECS` | optional | `600` | budget for the post-promotion warm-up request; `0` turns warm-up off |
| `OBLETH_PROVISIONER_LOST_RETENTION_SECS` | optional | `900` | how long `lost` replica rows are kept after they end |
| `OBLETH_PROVISIONER_RESTART_AFTER_FAILURES` | optional | `20` | net failing probes before a serving replica is replaced (about 5 minutes at the default interval); `0` turns self-heal off |
| `OBLETH_PORT_SPAN` | optional | `8` | width of each replica's port window |
| `OBLETH_PROVISIONER_JOB_PREFIX` | optional | `obleth-` | job-name prefix of this gateway's jobs |

**Slurm connection settings** are set under Deployments › Set up Slurm, not in
env vars: `enabled`, `slurmrestd_url`, `slurmrestd_api_version` (default
`auto`), `slurm_user`, `slurm_jwt`, `node_aliases`, `cluster_defaults` and
`hf_token`. The JWT and the Hugging Face token are encrypted at rest only when
the gateway has `OBLETH_ENCRYPTION_KEY` set; without it they are stored in
plaintext. A plain `scontrol token` lasts 30 minutes, so mint one with
`scontrol token lifespan=2592000` (30 days) and paste a new one before it
expires. **Test the connection** on that sheet checks the API version, the
token, and slurmctld.

**Per-model values** are set with `PUT /api/v1/models/:id/managed`: `partition`,
`gres`, `nodes`, `constraints`, `exclude`, `account`, `qos`, `time_limit`,
`cpus_per_task`, `mem`, `image`, `preamble`, `launch_command`, `script_body`,
`log_output_dir`, `serving_port`, `health_path`, `target_replicas`,
`min_replicas`, `max_job_failures`, `startup_timeout_mins` and `handoff_mins`.

## Network

The provisioner needs to reach the Management API, slurmrestd, and every
compute node on the replicas' port windows (probes and warm-up). The gateway
itself also reaches slurmrestd (connection test and resource discovery) and the
same node ports at the IP the provisioner resolved (requests and health
checks). Node names resolve through the operator's node addresses, then
Slurm's `NodeAddr`, then DNS.

## Running it

### Cargo (from the `obleth/` directory)

```sh
OBLETH_ADMIN_TOKEN=… \
OBLETH_ADMIN_BASE_URL=http://localhost:9180 \
cargo run -p obleth-provisioner
```

Then configure the Slurm connection in the dashboard (Deployments › Set up
Slurm) and turn it on; the running provisioner picks it up on the next tick.

### Docker Compose

The provisioner is gated behind the `slurm` profile. Opt in via **config**: add
`slurm` to `COMPOSE_PROFILES` in `deploy/docker/.env` (see `.env.example`), and
it builds and starts with the same one command as the rest of the stack:

```sh
cd deploy/docker
docker compose up -d --build      # builds + starts everything, provisioner included
docker compose down               # tears it all down
```

Running from `deploy/docker/` (no `-f` flags) lets Compose auto-load `.env` (so
the profile activates) and auto-merge a local `docker-compose.override.yml` if
you use one. The container only needs `OBLETH_ADMIN_TOKEN` (already set for the
core stack); the Slurm connection is configured in the dashboard, not in `.env`.
Its `stop_grace_period` lets a tick in progress finish before the container
stops.

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

Setup, the job script contract, replica lifecycle and troubleshooting are in
the docs: [Launch on Slurm](https://obleth.com/docs/deployments/slurm-provisioning),
[Job scripts](https://obleth.com/docs/deployments/job-script) and
[Replicas and failures](https://obleth.com/docs/deployments/slurm-replicas).
