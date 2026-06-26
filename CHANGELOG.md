# Changelog

The release workflow uses the matching `## vX.Y.Z` section below as the GitHub
Release notes. Add a section here when cutting a release; if none exists, the
workflow falls back to auto-generated notes.

## v0.4.12
Steadier health status and cleaner replica teardown.

- **Health status stops flapping to "down"** on a single failed probe. The model badge now only reads unhealthy once it crosses the configured failure threshold — matching when an alert actually fires — instead of flipping on the latest check. The active probe also makes one extra attempt with a short backoff, so brief blips (common on cold Slurm nodes) don't register at all.
- **Deleting a managed model no longer strands its Slurm jobs.** A model's replicas now drain properly after the model is deleted — the provisioner cancels their jobs and cleans up — instead of the jobs being left running on the cluster with nothing tracking them.
- **Replicas no longer get stuck "draining."** Once a cancelled replica's Slurm job ends, its row is removed instead of lingering in the panel indefinitely.
- **Provisioning tab refresh:** the settings form and replica list are restyled with clearer per-replica status, and saving provisioning settings now shows inline confirmation.

## v0.4.11
Restart a replica from the dashboard.

- **Restart action** on each Slurm-backed endpoint (Reliability tab): cancels that replica's Slurm job, and the provisioner launches a fresh one to hold target. Static endpoints are unaffected.

## v0.4.10
Cleaner replica cycling and a Charo input fix.

- **No more 502s when cycling a replica** — cancelling a replica deregisters its endpoint before the Slurm job is killed, so it leaves the routing pool immediately instead of a tick later.
- **Charo chat keeps focus** while a response streams (the input was disabled mid-stream, dropping focus).
- The Provisioning tab's replica list is now read-only status; serving endpoints live on the Reliability tab, removing the duplication.

## v0.4.9
Reaching Slurm nodes from Kubernetes, and per-replica endpoint visibility.

- **`nodeResolution`** values (static `hostAliases` map and/or custom `dnsConfig`) on the gateway and provisioner pods, so deployments whose cluster DNS can't resolve compute-node hostnames can bridge it — no application change required.
- The replica panel shows each replica's resolved **endpoint** (`node:port`).

## v0.4.8
Provisioning visibility, and a fix for replicas that came up "healthy" but couldn't serve.

- **Stranded-replica fix:** a transient endpoint-registration failure during promotion could leave a replica marked healthy with no endpoint (model permanently "unhealthy"). The failure now surfaces and retries, the endpoint is linked before the replica is marked healthy, and the provisioner self-heals any replica stuck in that state.
- **Provisioning-error banner** on the model page when a submit is rejected (e.g. an invalid Slurm account / partition / QoS combination).
- **Live Slurm job status** (e.g. `PENDING — Resources`, `RUNNING`) shown per replica.

## v0.4.7
Provisioner reliability on busy clusters.

- **No more out-of-memory on large clusters** — the reconcile loop looks up only its own jobs by id instead of fetching and buffering every job on the controller.
- **Orphaned jobs prevented** — a job is cancelled immediately if recording its replica fails, replacing the periodic cluster-wide scan. An unreachable Slurm still safely holds the tick.

## v0.4.6
Slurm launch and provisioning fixes, plus Kubernetes support for the provisioner.

- **Deploy saved templates** — database-backed recipe templates now deploy correctly (previously failed with "recipe not found").
- **Template name is respected** (no longer overridden by the recipe frontmatter), and **Save template** returns to the recipe list instead of closing the wizard.
- **The Slurm provisioner ships in the Helm chart** (off by default via `provisioner.enabled`) and its image is published.
- **Settings → Slurm** shows whether the provisioner process is running.
