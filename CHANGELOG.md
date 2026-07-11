# Changelog

The release workflow uses the matching `## vX.Y.Z` section below as the GitHub
Release notes. Add a section here when cutting a release; if none exists, the
workflow falls back to auto-generated notes.

## v0.9.5

Slurm replica endpoints stop depending on flaky per-request DNS, and replicas whose endpoint vanished out-of-band now heal themselves.

- **Node address overrides.** Slurm settings gain a node hostname → IP override list, editable in the dashboard, for clusters where the pods running obleth resolve compute-node names unreliably. The provisioner registers each replica's endpoint by IP and probes by IP, so neither health checks nor proxied requests depend on per-request DNS — a single missed lookup previously surfaced as an instant `502 upstream request failed`. Edits take effect on the next provisioner tick without a restart; leave the list empty to keep resolving node names through DNS.
- **Existing endpoints migrate to their resolved address in place.** Endpoints already registered by node name are rewritten to the resolved IP on the next tick — keeping their name, priority, weight, and enabled flag — so running replicas stop depending on DNS immediately after upgrading, without being re-provisioned.
- **Node names resolve with retry and a cache even without overrides.** Un-aliased hostnames are resolved at promotion with a short retry, a per-node success cache, and a last-known-address fallback, so one transient DNS miss no longer decides where an endpoint points. Model warm-up requests also target the resolved address.
- **Replicas with a vanished endpoint re-register instead of serving a phantom.** A replica marked healthy whose endpoint was removed out of band — a manual delete in the Reliability tab, or a cancellation that only half-landed — used to sit "healthy" forever while the model ran one endpoint short. The provisioner now detects the dangling reference and registers a fresh endpoint on the next tick.

## v0.9.4

The v0.9.3 self-heal fix now actually reaches deployments, and failed job cancellations say why.

- **Deployment defaults no longer undo the v0.9.3 self-heal fix.** The docker-compose fallback, the Helm chart default, and the env example all still pinned `OBLETH_PROVISIONER_RESTART_AFTER_FAILURES=3`, silently overriding the binary's new default — so provisioners deployed from them kept cancelling healthy replicas after ~45 seconds of probe flaps. All three now default to 20 (~5 minutes of sustained failure at the default tick interval). If you set the variable to 3 yourself, remove or raise it when upgrading.
- **Cancel failures report the real reason.** When slurmrestd refuses to cancel a job, the provisioner now logs the response body (e.g. `Access/permission denied` from `slurm_kill_job2`) instead of only the HTTP status — a bare 500 previously hid actionable causes like a JWT user / job owner mismatch.

## v0.9.3

Slurm-provisioned replicas stop getting killed in a loop, and jobs with a walltime submit cleanly.

- **Healthy replicas are no longer cancelled on transient probe blips.** A busy single-threaded inference server (e.g. llama.cpp) that briefly missed a health probe and passed the next used to accumulate toward the self-heal threshold and get cancelled — then resubmitted, flap, and die again in a loop. The failure counter now decays on every passing probe, so only a *sustained* outage restarts a replica; the default window is ~5 minutes (`OBLETH_PROVISIONER_RESTART_AFTER_FAILURES`, now 20). The gateway's separate inference-based zombie check is unchanged.
- **Jobs with a time limit submit without a `slurmrestd` error.** A managed model's walltime (e.g. `0-04:00:00`) is now converted to the integer minutes `slurmrestd` expects, instead of being sent as a date string that failed the submit with a 500. Unparseable values are omitted so the partition default applies.

## v0.9.2

Managed-model config catches bad inputs before Slurm does, provisioning-error notices can be dismissed, and a dead replica pool can no longer ride a stale success to "healthy".

- **Managed-model settings validate before saving.** The Placement and Service fields now check their inputs client-side — time limit format, port range, replica counts, node/CPU numbers — and highlight the offending field with an inline hint, instead of forwarding a malformed value and surfacing an opaque `slurmrestd` 500. Each field also carries a short format hint.
- **Provisioning-error banners are dismissible.** When the provisioner rejects a job (bad account / partition / QoS), the model's error banner can now be cleared once you've fixed the cause — it returns on its own if the next launch also fails.
- **A model whose replicas have all died no longer reports "healthy".** For Slurm-provisioned / dynamic-endpoint models, a recent passive success from the usage ledger could stand in as the health verdict even after every replica behind it went away — a success served just before the pool emptied. The passive shortcut now only settles a pool that still has a live endpoint serving; an empty or fully-dead pool reports its own reality.

## v0.9.1

Benchmarks report tokens-per-second, and benchmark traffic finally stays out of your real usage numbers.

- **Tokens-per-second across the benchmark suite.** Every concurrency step now reports aggregate output tok/s and per-stream decode rate (p50/p10) — in the `obench` CLI summary, the live TUI, the capacity scorecard, and Charo's inline benchmark card.
- **Benchmark and test traffic no longer pollutes usage stats.** Charo's model-test console and in-dashboard benchmarks reach the gateway through a reserved internal identity, which is now marked synthetic — so its requests are tagged as benchmark traffic and excluded by default from the overview's request, token, and tokens-per-second figures. Previously a single capacity run could bury a model's real numbers under thousands of synthetic requests. Existing pre-upgrade rows age out of the rolling window on their own; internal traffic stays viewable on demand.
- **Charo hides models' hidden reasoning.** Chain-of-thought that some models emit inline (`<think>…</think>`) is now stripped from Charo's answers and from the transcript sent back upstream, so you see the reply and its tool cards, not the scratchpad.
- **The chat panel, refined.** Assistant replies render as clean chat bubbles, the mascot imagery is retired for a tighter layout, and the typing indicator sits in-bubble so a pending reply never reads as dead air.

## v0.9.0

Charo grows from a testing console into a working colleague: guided activities, direct model chat, MCP verification, documentation-grounded answers with citations, and a redesigned chat panel.

- **Guided activities, opened conversationally.** Testing a model's capabilities, chatting with a specific model, and benchmarking are now step-by-step workflow cards in the chat thread — pick a model and options inline. Ask Charo in plain language ("test gemma4") and it opens the right workflow itself.
- **Probe a model's capabilities from the chat.** The capability test fires each configured boon through the gateway — quick ping, tools/web search, forced JSON, vision — with live pass/warn/fail rows, per-test output, and the request trace. The vision probe now requires a real image you attach (picker or drag-and-drop) instead of a bundled placeholder.
- **Chat with any model directly.** A raw, persona-free line to the model you pick — a banner shows who you're talking to, and exiting returns you to Charo.
- **MCP servers verified end-to-end.** A `test_mcp` tool runs the real MCP handshake through the gateway and lists each server's tools; the dashboard MCP tab auto-probes servers and gains a Test button. Deleting an MCP server now strips its grant from every model that had it.
- **Ask the docs.** Charo answers how-to and configuration questions grounded in the official documentation, with cited source pages linked under the answer — and says so plainly when the docs don't cover something.
- **The chat panel, redesigned.** Assistant replies render markdown properly; results hang off a clean rail instead of stacked gray boxes; answers stream below their sources so nothing hides off-screen; scrolling up mid-stream no longer gets yanked back; images attach via paperclip or drag-and-drop; compact type fits more in the small window. The panel is titled Gateway Chat.
- **Charo sounds like a person.** Personality calibration, a stop button for in-flight runs, a typing indicator, and greetings no longer deflected as chit-chat.
- **Dashboard fixes.** Model edit forms no longer revert to stale values on save; the container's `.next/cache` is writable by the runtime user.

## v0.8.1

Health badges you can trust: non-chat models stop showing a false "degraded", recovered models clear themselves, and Charo opens as a pop-out modal from anywhere.

- **Embedding, TTS, transcription, and image models no longer show a false "degraded".** The scheduled health worker was probing every model against the chat completions endpoint regardless of its type, so any non-chat model was rejected (HTTP 404) and left sitting "degraded — model_type may be misconfigured" even when correctly configured and serving. Scheduled checks now probe each model's real modality endpoint. (Manual "Check now" and bulk checks were already correct — only the background sweep was affected.)
- **Recovered models clear themselves instead of staying stuck "unhealthy".** A window of stale upstream errors with no recent successes could stand in as the health verdict and suppress the very active probe whose success would have cleared it — pinning a recovered model "unhealthy" until the window aged out. Only an observed success now settles a check for free from the usage ledger; anything short of that defers to a live probe as ground truth.
- **Charo opens as a pop-out modal from anywhere.** The dedicated `/charo` page is replaced by a centered pop-out modal, so the model-testing console can be summoned over whichever dashboard view you're on without navigating away.

## v0.8.0

Know your deployment is sound, and prove it: an agentic model-testing console (Charo), a graded system scorecard (`obench score`), honest health for every model type, Slurm state you can trust at a glance, and benchmark traffic kept out of your numbers.

- **Charo grows into an agentic model-testing console.** Charo — Charon, the ferryman — carries an operator's prompt to any configured model and brings the answer back with its toll: latency, token counts, and a trace of which boons actually fired. It now runs a real agent loop over a tool framework (an admin-gated deterministic tool-run surface, plus a streaming brain-and-tools loop with confirm-to-run handoff and an iteration cap), and gets a dedicated `/dashboard/charo` workspace with chat, run history, settings, and a tools rail alongside the existing corner panel. Its brain model, enabled tools, and benchmark caps are configurable in Settings.
- **Run a load benchmark from the chat.** Charo ships a `run_benchmark` tool — a concurrency-ramp executor with cap enforcement, knee detection (error + p99 latency gates), percentile/step summaries, a routing-identity config fingerprint, and a blended score with grade and findings — rendered inline as a capacity-curve card.
- **`obench score` — a graded readiness scorecard for the whole gateway.** A new subcommand runs six sections — capacity ramp, gateway overhead (proxy tax), streaming quality (jitter + stalls), overload behavior, resilience (health-probe MTTD/MTTR via fault injection), and fair-share dynamics (Jain index, convergence, starvation) — rolls them into a weighted, letter-graded scorecard, stores a baseline, and diffs later runs for regressions. Runnable from the interactive TUI wizard too. The fixture backend gains a runtime `POST /control` for fault injection so the resilience section can measure real detect/recover times.
- **Every model type now gets an honest health signal.** Text-to-speech and transcription models are verified with real minimal inference probes (one character of speech; a 0.1-second silence clip). Image models are checked against the upstream's model catalog. Previously these types could only ever show "unchecked" — or worse, sit falsely unhealthy when mis-typed.
- **Configuration mistakes no longer masquerade as outages.** When a probe is rejected but the upstream's catalog still lists the model, the model is marked degraded with a pointed message ("model_type may be misconfigured") instead of counting toward failure alerts. A model genuinely missing upstream still alerts, now with catalog evidence in the message.
- **Fixing a model's connection takes effect immediately.** Changing a model's API base, upstream id, or type clears the old failure streak and alert state and re-checks within seconds — no more stale "down" badges after a config fix. Creating or editing a model also pre-flights the config, warning when the upstream doesn't list the model id, the catalog can't be verified (wildcard pass-through), or the model type isn't recognized — the save always succeeds; the warnings tell you what to fix.
- **Busy models are no longer probed needlessly.** The passive traffic window now follows the model's check interval, so any model with recent successful traffic is settled from the usage ledger for free. Wildcard upstream catalogs (`/models` returning `*`) are explicitly treated as unverifiable and can never produce a false healthy badge.
- **Frozen Slurm replica state is now visible, loudly.** The provisioner reports each reconcile tick's outcome to the gateway. When it can't reach Slurm (or is idle), the model's Replicas panel shows a warning with the failure reason and how long states have been frozen, state badges gray out with a `?`, and Settings → Slurm distinguishes "running" from "running but failing since X" — previously a week-old "Healthy" pill was indistinguishable from a live one. If a successful reconcile hasn't happened for 10 minutes while Slurm provisioning is enabled, a deduplicated alert fires (with a recovery notice when it clears).
- **Zombie Slurm jobs self-heal.** A replica whose Slurm job still reports RUNNING but whose server is dead is restarted automatically, on either of two signals: 3 consecutive failed provisioner probes (`OBLETH_PROVISIONER_RESTART_AFTER_FAILURES`, 0 disables), or the gateway's real-inference endpoint check staying unhealthy — the latter catches servers that still answer metadata requests but hang on actual inference. Capped at one restart per model per tick so a probe-side network problem can never mass-cancel a fleet. Replica rows now say "updated Xh ago" (never a liveness "seen"), draining rows say *why*, and disabling Slurm in Settings warns that running jobs are not cancelled by it.
- **Synthetic-tenant tagging keeps test traffic out of the numbers.** Tenants can be flagged synthetic (obench seeds its fixture tenants that way); their traffic is recorded as benchmark traffic and, together with health probes, excluded from usage and cost stats by default (`include_internal=true` opts back in). Benchmark traffic never enters the permanent daily rollup.
- **Reach the dashboard from any host.** A new `TRUSTED_ORIGINS` setting (comma-separated origins; `*` on trusted private networks only) lets better-auth accept logins from a LAN IP or alternate hostname, not just the exact `BETTER_AUTH_URL` — fixing the invalid-origin login failure on self-hosted Docker/K8s deploys. Wired through `.env.example`, docker-compose, and the Helm chart.

## v0.7.2

Chargeback-ready reports: filter and group historical usage by team and API key, with spend visible everywhere — plus a compression fix for fenced code.

- **Filter reports by team and key.** The Reports page gains team/key filters that re-scope every KPI, chart, and table — answer "what did this course spend this month" without leaving the dashboard.
- **Breakdown table with grouping.** The daily table can now group by day, team, key, or model — chargeback views sort by spend, and key rows show the key's name with its prefix.
- **Spend, finally visible.** A Spend KPI and a per-row Spend column join the reports (frozen completion-time cost, summed — never recomputed), and the CSV export now carries `cost_usd` and `key_name` by default.
- **Export exactly the report you're looking at.** The CSV dialog inherits the active team/key filter and defaults its row grouping to the table's current grouping, with per-key+model, day, team, key, and model options.
- **Compression: fenced code behind a preamble now compacts.** A code block introduced by prose (e.g. "Here's the patch:" followed by a ``` fence) was classified as prose and skipped by the deterministic code compactor; it is now recognized as code. The obench compression corpus was updated to exercise this path.

## v0.7.1

Per-request energy and carbon accounting for self-hosted clusters: watt-hours, electricity cost, and CO₂ tracked alongside token cost — per request, per tenant, per model.

### Added
- **Energy & carbon on every request.** Each completed request is charged its wall-time share of a serving slot's power draw, priced with your electricity rate and grid carbon intensity. The three values (`energy_wh`, `energy_cost_usd`, `co2_g`) land in the usage ledger next to `cost_usd` and follow the same rule: frozen at completion — changing rates or settings later never rewrites history.
- **Bring your own power metrics.** Settings → Energy points the gateway at your Prometheus with any PromQL expression that returns per-node power (Habana, DCGM, and IPMI exporters all work — the gateway only averages across nodes). Set $/kWh, gCO₂/kWh, and an optional PUE multiplier for facility overhead. A **Test query** button shows live "X kW across N nodes" before you enable anything.
- **Per-model saturation, declared by you.** Each model gets an "energy slots per node" count — how many concurrent requests saturate one node. Node power is split across those slots; queue time is never charged, and idle power is never attributed, so totals understate your bill rather than overstate it. Leave it at 0 (the default, e.g. for external API models) and the model stays out of energy accounting entirely.
- **Visible everywhere cost is.** Energy column in the request log (cost and CO₂ in the detail view), energy/carbon totals and per-tenant/per-model breakdowns in reports, and CSV exports include the new columns.
- **Off by default, fails open.** Nothing changes until you configure it. If Prometheus is unreachable, requests are never delayed or failed — the gateway keeps the last power reading, records zeros when it has none, and raises a deduplicated alert.

### Changed
- Consolidated the compression A/B benchmark into `obench` as `obench compression`
  (and a "compression savings" option in the interactive TUI). The standalone
  `bench/compression` Python harness has been removed. Reports now write to
  `BENCH_OUT_DIR` instead of the source tree.

## v0.7.0
Optional neural prose compression you host yourself, a per-request switch to A/B it from the API, sharper log compaction, and an access-control fix for the dashboard's admin API.

- **Neural extractive prose compaction.** The compression boon's lossy pass can now score sentences with a trained extractive model instead of the built-in heuristic, keeping the most load-bearing sentences and dropping filler. It only ever selects existing sentences — nothing is rewritten or invented — and every original stays fully recoverable.
- **Run the model on your own infrastructure.** It is served by an optional sidecar you deploy alongside the gateway: a plain CPU container (no GPU, no Slurm) that scales horizontally behind a Service, with no data leaving your network. Ships as a published image plus a Helm switch (`compressor.enabled`) and an opt-in Docker Compose profile, and the model is swappable at build time.
- **Off by default, fails open.** Nothing changes until you deploy the sidecar and point the gateway at it. If it is absent, slow, or unavailable, the gateway silently falls back to the existing heuristic — a request is never delayed or failed because of it. A tunable keep-ratio dials how aggressively prose is trimmed.
- **A/B compression from the API.** Send `x-obleth-boons: lossy` to force the lossy pass on for a single request (even where it is otherwise off), and read the `x-obleth-compression` response header — `before`/`after`/`saved` tokens — to compare a request with and without compression, no dashboard round-trip.
- **Sharper log compaction.** Repeated log lines now collapse via template mining (Drain-style), catching structurally-identical lines that differ only in their variable fields, and syslog `Mon DD HH:MM:SS` timestamps are recognized as logs.
- **Security: admin-only enforcement on the dashboard's live API.** Every `/api/live/*` route now independently verifies the caller is an active admin. Previously these routes checked only for a signed-in session, so a non-admin (for example a tenant-portal user) could reach admin-only data such as config backups or other tenants' usage by calling them directly. No upgrade action is required — access simply tightens.

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
