# Changelog

The release workflow uses the matching `## vX.Y.Z` section below as the GitHub
Release notes. Add a section here when cutting a release; if none exists, the
workflow falls back to auto-generated notes.

## v0.6.1
Optional neural prose compression: shrink long prose more aggressively than the built-in heuristic, served by a model you run yourself — same governance, still fully reversible.

- **Neural extractive prose compaction.** The compression boon's lossy pass can now score sentences with a trained extractive model instead of the built-in heuristic, keeping the most load-bearing sentences and dropping filler. It only ever selects existing sentences — nothing is rewritten or invented — and every original stays fully recoverable.
- **Runs on your own infrastructure.** The model is served by an optional sidecar you deploy alongside the gateway: a plain container that scales horizontally behind a Service, with no Slurm or GPU required and no data leaving your network. Ships as a published image plus a Helm switch (`compressor.enabled`) and an opt-in Docker Compose profile.
- **Off by default, fails open.** Nothing changes until you deploy the sidecar and point the gateway at it. If it is absent, slow, or unavailable, the gateway silently falls back to the existing heuristic — a request is never delayed or failed because of it. A tunable keep-ratio dials how aggressively prose is trimmed.

## v0.6.0
Context compression: shrink oversized tool output, logs, and repeated context before it reaches the model — losslessly by default, with per-tenant control.

- **New compression boon.** Grant `compression` to a model and the gateway compacts oversized message segments before they go upstream, cutting tokens (and cost/latency) without changing answers. Deterministic, runs locally (no helper model), and fails open — a segment is rewritten only when the result provably reconstructs to the same value and is strictly smaller.
- **Lossless JSON structural compaction, everywhere.** Arrays of like objects become a compact table form (columns once, one row each) wherever they appear — standalone, wrapped in an object, several per payload, embedded in a prose message or a ```json fence, and with sparse/non-uniform keys. Typically 35–45% on JSON payloads, fully reversible.
- **Near-lossless log compaction (opt-in).** Enable **Log compaction** for a tenant and repeated, structurally-identical log lines collapse to a single representative with a count, while every ERROR/WARN line is kept verbatim. Reversible, and a separate switch from lossy prose.
- **Cross-turn deduplication (opt-in).** Large blocks re-sent verbatim across turns (a pasted document, a system prompt, a prior tool result) are replaced after the first occurrence with a compact reference — commonly 60%+ on re-grounded conversations, fully recoverable.
- **Per-tenant compression policy in the dashboard.** A Compression tab per tenant: master switch plus independent toggles for code-whitespace compaction, cross-turn dedup, log compaction, and lossy prose — enable the safe pieces without opting into anything lossy.
- **Visible in request traces.** The `boon:compression` span reports what each pass did (JSON compacted, dedup refs, log segments, tokens before/after/saved).
- **More faithful float handling.** JSON floating-point numbers now round-trip exactly through the gateway (correctly-rounded parsing).

## v0.5.5
Import models straight from an OpenAI-compatible provider — no file to hand-write.

- **Import models by pointing obleth at a provider.** On the Models page, **Import from provider** takes any OpenAI-compatible base URL (and an optional API key), lists the models that provider actually serves, and lets you import the ones you don't already have. After fetching you see the discovered models right away — each with an editable name and per-model overrides — on top of batch defaults (type, context window, costs) you set once. The catalog is fetched server-side, so the API key never reaches the browser.
- **Re-run it any time to see what's new.** Models you've already imported are detected — by name, or by the same provider URL and upstream id — and shown as already-imported instead of being offered again, so running the importer periodically surfaces only genuinely new models. The flow only ever creates new routes; nothing existing is changed.

## v0.5.4
Optional upstream-failure diagnostics, plus warmup for freshly-started Slurm replicas.

- **Slurm-managed replicas are warmed up the moment they go healthy.** A new replica passes its health check as soon as the inference server answers `/health`, but its very first request can still be slow — the model has to do its first forward pass (graph capture, cache warmup), which on a cold box can take long enough to surface to a user as a 502/504. The provisioner now fires one throwaway request at each replica right after it's promoted, so that cold first-token cost is paid by the gateway instead of by the first real user. On by default; tune or disable with `OBLETH_PROVISIONER_WARMUP_TIMEOUT_SECS` (default 600s, `0` disables).
- **Turn on "Debug upstream failures" for a model** (Reliability → Delivery) and whenever a request to it gives up with a 502/504, the gateway runs a quick read-only check of the upstream — does its hostname still resolve in DNS, and is the port reachable — and records the result in the request trace. This turns the intermittent "it's up, but I got a 502" cases into concrete evidence (e.g. a DNS blip) instead of a guess. Off by default; no effect on models without it enabled.
- **Fixed: the Endpoint selection dropdown snapped back after saving.** The Delivery panel's dropdown briefly reverted to its previous value on save (the saved value reappeared on reload). It now stays on the value you chose.

## v0.5.3
The model page now shows the timeout and retry settings you actually saved.

- **Fixed: the Delivery settings — request timeout, max retries, retry backoff, and endpoint selection — appeared to snap back to defaults after saving.** The values were always stored and applied by the gateway; only the model page was wrong, because it re-read those fields through queries that didn't select them and so always redisplayed defaults. The dashboard now shows the saved values (no action needed — any settings you'd previously "lost" have been in effect all along).

## v0.5.2
A redesigned audit log that records who made each change, steadier upstream connections, and provisioner build visibility.

- **Every change is attributed to the person who made it.** Dashboard and self-service portal actions are now recorded in the audit log against the signed-in user's email instead of a generic `admin`; changes made automatically by the provisioner are recorded as `system`. This covers tenants, API keys, models, endpoints, replicas, MCP servers, and settings.
- **Redesigned audit log.** Filter by actor, action, or target; each event shows an inline summary that expands to full detail; tenant ids resolve to tenant names; and the view adds page-size control, paging, and a mobile layout. Summary cards show event, actor, and target counts plus the latest event.
- **Fewer upstream 502s from stale connections.** The gateway now retries a connection-level send failure once on a fresh connection — the classic case where a pooled keep-alive socket was already closed by the inference server. Idle-pool lifetime and TCP keep-alive are tunable via `OBLETH_UPSTREAM_POOL_IDLE_SECS` (default 15) and `OBLETH_UPSTREAM_TCP_KEEPALIVE_SECS` (default 30).
- **Provisioner build shown on the Slurm settings tab.** When the provisioner is running, its reported version (and short commit, when built with it) appears next to its status, making a stale provisioner deployment obvious since it ships as its own image.

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
