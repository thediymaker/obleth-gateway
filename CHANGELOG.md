# Changelog

The release workflow uses the matching `## vX.Y.Z` section below as the GitHub
Release notes. Add a section here when cutting a release; if none exists, the
workflow falls back to auto-generated notes.

## v0.5.2
The audit log now records who made each change, in a redesigned, filterable view.

- **Every change is attributed to the person who made it.** Dashboard and self-service portal actions are now recorded in the audit log against the signed-in user's email instead of a generic `admin`; changes made automatically by the provisioner are recorded as `system`. This covers tenants, API keys, models, endpoints, replicas, MCP servers, and settings.
- **Redesigned audit log.** Filter by actor, action, or target; each event shows an inline summary that expands to full detail; tenant ids resolve to tenant names; and the view adds page-size control, paging, and a mobile layout. Summary cards show event, actor, and target counts plus the latest event.

## v0.5.1
Conversations stay together — for routing and for tracing — with nothing extra from callers.

- **Automatic conversation grouping.** The gateway derives a stable conversation id from each request (or uses a client-supplied `x-session-id` header, or a `session_id` / `metadata.session_id` body field, when present) — no client changes required. The dashboard request log now shows the id, tagged **client** (caller-supplied) or **derived** (inferred).
- **`session_hash` routing actually sticks now.** With a conversation id available, the `session_hash` endpoint-selection mode pins a conversation to the same upstream replica for cache warmth, instead of silently falling back to load-balancing.
- **Conversation id in traces and usage.** The id is recorded on usage rows and spans and exported as the `session.id` attribute on OpenTelemetry/Jaeger traces, so a whole conversation's spend and spans can be grouped — not just a single request.
- **Fixed: the "Endpoint selection" setting could crash the model page.** Choosing `session_hash` (and saving the timeout / retry fields alongside it) failed against an outdated database constraint; the setting now saves correctly.
- **Fixed: endpoints stuck "unhealthy" after a Slurm replica restarted.** A reachable endpoint now clears a stale unhealthy state on its own, and the gateway re-checks health immediately when a replica is added or removed — so a restarted replica returns to rotation without a manual health check.
- **Endpoint health reason on the Reliability tab.** Each endpoint row shows its latest health-check message, so a degraded or unhealthy endpoint explains itself at a glance.

## v0.5.0
Dashboard single sign-on (OIDC) and a break-glass admin — with an email-based login that needs a one-time config change.

> **⚠️ Required action when upgrading.** The dashboard now signs in with an **email address**, not a username. Before you upgrade, set `DASHBOARD_ADMIN_EMAIL` to a real email address and make sure `DASHBOARD_PASSWORD` is at least 8 characters. If you don't, the admin account is not created on first boot and **you will be locked out of the dashboard**. The old `DASHBOARD_USERNAME` setting is no longer used and can be deleted.

- **OIDC single sign-on** for the dashboard: sign in with Globus, CILogon, or any discovery-capable identity provider. Configure providers via `OIDC_PROVIDERS`; leave it empty to keep SSO off.
- **Break-glass local admin.** A local email + password admin is seeded on first boot from `DASHBOARD_ADMIN_EMAIL` / `DASHBOARD_PASSWORD` and keeps working even when the identity provider is unreachable.
- **Approval flow for new users.** People who sign in via SSO start with no access until an admin assigns them a role and a tenant on the dashboard's **Users** screen. Users with the `user` role get a self-service portal (model list, their own API keys and usage) instead of the full dashboard.
- **Email-based sign-in** replaces the old username form (see the required action above).

Setup, a local test identity provider, user management, and Kubernetes configuration are covered in the [Dashboard SSO guide](https://obleth.com/docs/guides/dashboard-sso).

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
