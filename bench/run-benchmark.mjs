// End-to-end obleth benchmark.
//
// One command seeds tenants, sets capacity, runs staggered contention, samples
// fairshare state, verifies the client and ledger signals, and exits non-zero
// when the fairshare story is not visible.
//
//   node bench/run-benchmark.mjs
//
// Outputs are written to BENCH_OUT_DIR (default /tmp/obleth-bench).

import { appendFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { loadedEnvFiles } from "./env.mjs";
import { benchPaths, BENCH_OUT_DIR } from "./paths.mjs";
import { randomPrompt } from "./prompts.mjs";

const ADMIN_BASE = process.env.ADMIN_BASE ?? "http://localhost:9090";
const ADMIN_TOKEN = process.env.ADMIN_TOKEN ?? "dev-admin-token";
const PROXY_BASE = process.env.PROXY_BASE ?? "http://localhost";
const BENCHMARK_MODEL = "benchmark-endpoint";
const LEGACY_BENCHMARK_MODEL = "mock-model";
const MODEL = process.env.MODEL ?? BENCHMARK_MODEL;
const BENCHMARK_API_BASE =
  process.env.BENCHMARK_API_BASE ?? process.env.MOCK_API_BASE ?? "http://benchmark-backend:8081";
const CAPACITY = Number(process.env.CAPACITY ?? 8);
const DURATION_S = Number(process.env.DURATION_S ?? 60);
const STAGGER_CHATBOT_S = Number(process.env.STAGGER_CHATBOT_S ?? 10);
const STAGGER_ANALYTICS_S = Number(process.env.STAGGER_ANALYTICS_S ?? 0);
const STAGGER_CHATBOT2_S = Number(process.env.STAGGER_CHATBOT2_S ?? 0);
const INCLUDE_ANALYTICS = process.env.INCLUDE_ANALYTICS === "1";
const INCLUDE_CHATBOT2 = process.env.INCLUDE_CHATBOT2 === "1";
const BENCH_KEY_NAME = process.env.BENCH_KEY_NAME ?? "bench";
const BENCH_REUSE_KEYS = process.env.BENCH_REUSE_KEYS !== "0";
const BENCH_PRUNE_KEYS = process.env.BENCH_PRUNE_KEYS !== "0";
const CONC = Number(process.env.CONC ?? 32);
const OUTPUT_TOKENS = Number(process.env.OUTPUT_TOKENS ?? 150);
const SAMPLE_MS = Number(process.env.SAMPLE_MS ?? 500);
const VERIFY_DELAY_MS = Number(process.env.VERIFY_DELAY_MS ?? 3000);
const MIN_COMPLETION_RATIO = Number(process.env.MIN_COMPLETION_RATIO ?? 2);
const MAX_ERROR_RATE = Number(process.env.MAX_ERROR_RATE ?? 0.05);
const LEDGER_TOLERANCE = Number(process.env.LEDGER_TOLERANCE ?? 0.2);
const REQUIRE_SATURATION = process.env.REQUIRE_SATURATION !== "0";
const CHAOS = process.env.CHAOS === "1";
const CONTAINER_CLI = process.env.CONTAINER_CLI ?? "docker";
const COMPOSE_FILE = process.env.COMPOSE_FILE ?? "deploy/docker/docker-compose.yml";

function stat() {
  return {
    ok: 0,
    rejected: 0,
    error: 0,
    latencies: [],
    overlapOk: 0,
    overlapRejected: 0,
    overlapError: 0,
    overlapLatencies: [],
    statuses: {},
    overlapStatuses: {},
    sampleErrors: [],
  };
}

function percentile(values, p) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return Math.round(sorted[Math.min(sorted.length - 1, Math.floor((p / 100) * sorted.length))]);
}

async function api(path, method = "GET", body) {
  const res = await fetch(`${ADMIN_BASE}/api/v1${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${ADMIN_TOKEN}`,
      "Content-Type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`${method} ${path} -> ${res.status}: ${await res.text()}`);
  return res.status === 204 ? null : res.json();
}

async function ensureGroup(name, weight) {
  const existing = (await api("/fairshare/groups")).find((g) => g.name === name);
  if (existing) {
    await api(`/fairshare/groups/${encodeURIComponent(name)}/weight`, "PATCH", { weight });
    return { ...existing, weight };
  }
  return api("/fairshare/groups", "POST", { name, weight });
}

async function ensureTenant(name, weight, tokensPerMinute, fairshareGroup) {
  const existing = (await api("/tenants")).find((t) => t.name === name);
  if (existing) {
    await api(`/tenants/${existing.id}/weight`, "PATCH", { weight });
    await api(`/tenants/${existing.id}/quota`, "PUT", {
      tokens_per_minute: tokensPerMinute,
      max_in_flight: null,
    });
    if (existing.fairshare_group !== fairshareGroup) {
      await api(`/tenants/${existing.id}/group`, "PATCH", { fairshare_group: fairshareGroup });
    }
    return { ...existing, weight, tokens_per_minute: tokensPerMinute, fairshare_group: fairshareGroup };
  }
  return api("/tenants", "POST", {
    name,
    weight,
    tokens_per_minute: tokensPerMinute,
    fairshare_group: fairshareGroup,
  });
}

async function ensureModel() {
  const models = await api("/models");
  const existing = models.find((m) => m.model_name === MODEL);
  if (existing) {
    if (isBenchmarkEndpoint(MODEL) && shouldUpdateBenchmarkEndpoint(existing)) {
      console.log(`updating ${MODEL} route to ${BENCHMARK_API_BASE}`);
      return updateBenchmarkEndpoint(existing);
    }
    return existing;
  }
  if (!isBenchmarkEndpoint(MODEL)) {
    throw new Error(`model '${MODEL}' is not registered; add it in the control plane before benchmarking`);
  }
  return api("/models", "POST", {
    model_name: MODEL,
    upstream_model: MODEL,
    api_base: BENCHMARK_API_BASE,
    context_window: 8192,
    admission_weight: 100,
  });
}

function isBenchmarkEndpoint(modelName) {
  return modelName === BENCHMARK_MODEL || modelName === LEGACY_BENCHMARK_MODEL;
}

function shouldUpdateBenchmarkEndpoint(model) {
  return (
    model.api_base !== BENCHMARK_API_BASE ||
    model.upstream_model !== MODEL ||
    model.enabled === false
  );
}

async function updateBenchmarkEndpoint(existing) {
  return api(`/models/${existing.id}`, "PUT", {
    upstream_model: MODEL,
    api_base: BENCHMARK_API_BASE,
    api_key: existing.api_key ?? null,
    input_cost_per_token: existing.input_cost_per_token ?? 0,
    output_cost_per_token: existing.output_cost_per_token ?? 0,
    context_window: existing.context_window ?? 8192,
    admission_weight: existing.admission_weight ?? 100,
    supports_function_calling: existing.supports_function_calling ?? false,
    supports_system_messages: existing.supports_system_messages ?? true,
    supports_response_schema: existing.supports_response_schema ?? false,
    supports_tool_choice: existing.supports_tool_choice ?? false,
    enabled: true,
  });
}

async function seed() {
  await ensureModel();
  await ensureGroup("chatbot", 500);
  await ensureGroup("api", 50);
  await ensureGroup("analytics", 100);

  const tokenBudget = 100_000_000;
  const chatbot = await ensureTenant("chatbot", 500, tokenBudget, "chatbot");
  const chatbot2 = await ensureTenant("chatbot-2", 500, tokenBudget, "chatbot");
  const apiBatch = await ensureTenant("api-batch", 50, tokenBudget, "api");
  const analytics = await ensureTenant("analytics", 100, tokenBudget, "analytics");
  const tenants = { chatbot, chatbot2, apiBatch, analytics };
  const keyInventory = await api("/keys");

  const reused = BENCH_REUSE_KEYS ? reusableKeysForTenants(tenants, keyInventory) : null;
  if (reused) {
    writeFileSync(benchPaths.keys, JSON.stringify(reused, null, 2));
    if (BENCH_PRUNE_KEYS) await pruneBenchKeys(tenants, reused, keyInventory);
    console.log(`reused benchmark keys from ${benchPaths.keys}`);
    console.log(`seeded tenants, groups, keys, and model '${MODEL}'`);
    return reused;
  }

  const minted = {
    chatbot: await api(`/tenants/${chatbot.id}/keys`, "POST", { name: BENCH_KEY_NAME }),
    chatbot2: await api(`/tenants/${chatbot2.id}/keys`, "POST", { name: BENCH_KEY_NAME }),
    apiBatch: await api(`/tenants/${apiBatch.id}/keys`, "POST", { name: BENCH_KEY_NAME }),
    analytics: await api(`/tenants/${analytics.id}/keys`, "POST", { name: BENCH_KEY_NAME }),
  };

  const keys = {
    chatbot: tenantKey(chatbot, minted.chatbot.secret, minted.chatbot.key),
    chatbot2: tenantKey(chatbot2, minted.chatbot2.secret, minted.chatbot2.key),
    apiBatch: tenantKey(apiBatch, minted.apiBatch.secret, minted.apiBatch.key),
    analytics: tenantKey(analytics, minted.analytics.secret, minted.analytics.key),
  };
  writeFileSync(benchPaths.keys, JSON.stringify(keys, null, 2));
  if (BENCH_PRUNE_KEYS) await pruneBenchKeys(tenants, keys, keyInventory);
  console.log(`seeded tenants, groups, keys, and model '${MODEL}'`);
  console.log(`wrote ${benchPaths.keys}`);
  return keys;
}

function reusableKeysForTenants(tenants, keyInventory) {
  if (!existsSync(benchPaths.keys)) return null;

  let saved;
  try {
    saved = JSON.parse(readFileSync(benchPaths.keys, "utf8"));
  } catch {
    return null;
  }

  const byId = new Map(keyInventory.map((key) => [key.id, key]));
  const byTenantPrefix = new Map(keyInventory.map((key) => [`${key.tenant_id}:${key.key_prefix}`, key]));
  const keys = {};

  for (const [label, tenant] of Object.entries(tenants)) {
    const existing = saved?.[label];
    if (!existing || existing.id !== tenant.id || typeof existing.secret !== "string") return null;

    const prefix = existing.key_prefix ?? keyPrefixFromSecret(existing.secret);
    let keyRecord = existing.key_id ? byId.get(existing.key_id) : undefined;
    if (!keyRecord && prefix) keyRecord = byTenantPrefix.get(`${tenant.id}:${prefix}`);
    if (!keyRecord || keyRecord.tenant_id !== tenant.id || keyRecord.disabled) return null;

    keys[label] = tenantKey(tenant, existing.secret, keyRecord);
  }

  return keys;
}

async function pruneBenchKeys(tenants, currentKeys, keyInventory) {
  const tenantIds = new Set(Object.values(tenants).map((tenant) => tenant.id));
  const keepIds = new Set(Object.values(currentKeys).map((key) => key.key_id).filter(Boolean));
  const keepTenantPrefixes = new Set(
    Object.values(currentKeys)
      .map((key) => key.key_prefix && `${key.id}:${key.key_prefix}`)
      .filter(Boolean),
  );
  const stale = keyInventory.filter((key) => {
    if (!tenantIds.has(key.tenant_id) || key.name !== BENCH_KEY_NAME) return false;
    if (keepIds.has(key.id)) return false;
    if (keepTenantPrefixes.has(`${key.tenant_id}:${key.key_prefix}`)) return false;
    return true;
  });

  if (stale.length === 0) return;
  let deleted = 0;
  let failed = 0;
  for (const chunk of chunks(stale, 25)) {
    const results = await Promise.allSettled(chunk.map((key) => api(`/keys/${key.id}`, "DELETE")));
    for (const result of results) {
      if (result.status === "fulfilled") deleted++;
      else failed++;
    }
  }
  console.log(`pruned ${deleted} stale benchmark keys${failed ? ` (${failed} failed)` : ""}`);
}

function chunks(items, size) {
  const out = [];
  for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
  return out;
}

function keyPrefixFromSecret(secret) {
  return secret.startsWith("sk_") && secret.length >= 18 ? secret.slice(0, 18) : null;
}

function tenantKey(tenant, secret, keyRecord) {
  return {
    id: tenant.id,
    key_id: keyRecord?.id,
    key_prefix: keyRecord?.key_prefix,
    tenant: tenant.name,
    group: tenant.fairshare_group,
    weight: tenant.weight,
    secret,
  };
}

async function setCapacity() {
  const body = await api("/capacity", "PUT", { max_in_flight: CAPACITY });
  console.log(`capacity set to max_in_flight=${body.max_in_flight}`);
}

async function oneRequest(tenant, overlapStart, stats) {
  const startedAt = Date.now();
  const started = performance.now();
  const nonce = randomUUID();
  try {
    const res = await fetch(`${PROXY_BASE}/v1/chat/completions`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${tenant.secret}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({
        model: MODEL,
        messages: [
          { role: "system", content: `Benchmark session ${nonce}. Answer concisely.` },
          { role: "user", content: randomPrompt(nonce) },
        ],
        max_tokens: OUTPUT_TOKENS,
        stream: false,
      }),
    });
    const body = await res.text();
    recordStatus(stats, res.status, performance.now() - started, startedAt >= overlapStart, body);
  } catch (error) {
    recordStatus(stats, 0, 0, startedAt >= overlapStart, error.message);
  }
}

function recordStatus(stats, status, latency, isOverlap, body = "") {
  const key = String(status);
  stats.statuses[key] = (stats.statuses[key] ?? 0) + 1;
  if (isOverlap) stats.overlapStatuses[key] = (stats.overlapStatuses[key] ?? 0) + 1;

  if (status === 200) {
    stats.ok++;
    stats.latencies.push(latency);
    if (isOverlap) {
      stats.overlapOk++;
      stats.overlapLatencies.push(latency);
    }
  } else if (status === 429) {
    stats.rejected++;
    if (isOverlap) stats.overlapRejected++;
  } else {
    stats.error++;
    if (isOverlap) stats.overlapError++;
    if (stats.sampleErrors.length < 3) {
      stats.sampleErrors.push({
        status,
        body: truncate(String(body).replace(/\s+/g, " "), 240),
      });
    }
  }
}

async function worker(tenant, stats, startAt, endAt, overlapStart) {
  while (Date.now() < startAt) await sleep(50);
  while (Date.now() < endAt) await oneRequest(tenant, overlapStart, stats);
}

async function sampleFairshare(until) {
  writeFileSync(benchPaths.fairshareSamples, "");
  let maxQueued = 0;
  let maxInFlight = 0;
  let samples = 0;
  while (Date.now() < until) {
    try {
      const snap = await api("/fairshare/live");
      samples++;
      maxQueued = Math.max(maxQueued, Number(snap.global_queued ?? 0));
      maxInFlight = Math.max(maxInFlight, Number(snap.global_in_flight ?? 0));
      appendFileSync(benchPaths.fairshareSamples, `${JSON.stringify({ ts: Date.now(), ...snap })}\n`);
    } catch {
      // Sampling should not perturb the load test.
    }
    await sleep(SAMPLE_MS);
  }
  return { samples, maxQueued, maxInFlight };
}

async function runChaos(startAt, endAt) {
  if (!CHAOS) return;
  await sleep(Math.max(0, startAt - Date.now() + 5000));
  if (Date.now() >= endAt) return;

  console.log("chaos: pausing ClickHouse for 10s");
  await pauseService("clickhouse", 10_000);
  await sleep(3000);
  if (Date.now() >= endAt) return;

  console.log("chaos: pausing Redis for 6s");
  await pauseService("redis", 6000);
}

async function pauseService(service, durationMs) {
  try {
    execFileSync(CONTAINER_CLI, ["compose", "-f", COMPOSE_FILE, "pause", service], { stdio: "inherit" });
    await sleep(durationMs);
    execFileSync(CONTAINER_CLI, ["compose", "-f", COMPOSE_FILE, "unpause", service], { stdio: "inherit" });
  } catch (error) {
    console.error(`chaos: failed for ${service}: ${error.message}`);
  }
}

async function queryLedger(keys, sinceMs) {
  const [tenants, usage] = await Promise.all([
    api("/tenants"),
    api(`/usage?since_ms=${sinceMs}`),
  ]);
  const byName = new Map(tenants.map((tenant) => [tenant.name, tenant.id]));
  const byTenant = new Map(usage.map((row) => [row.tenant_id, row]));
  return Object.fromEntries(
    Object.entries(keys).map(([key, value]) => {
      const tenantId = byName.get(value.tenant);
      const row = tenantId ? byTenant.get(tenantId) : undefined;
      return [key, Number(row?.requests ?? 0)];
    }),
  );
}

function summarize(key, tenant, stats) {
  const attempts = stats.ok + stats.rejected + stats.error;
  const overlapAttempts = stats.overlapOk + stats.overlapRejected + stats.overlapError;
  return {
    key,
    tenant: tenant.tenant,
    group: tenant.group,
    weight: tenant.weight,
    completed: stats.ok,
    rejected: stats.rejected,
    errors: stats.error,
    attempts,
    overlap_completed: stats.overlapOk,
    overlap_rejected: stats.overlapRejected,
    overlap_errors: stats.overlapError,
    overlap_attempts: overlapAttempts,
    p50_ms: percentile(stats.latencies, 50),
    p90_ms: percentile(stats.latencies, 90),
    p99_ms: percentile(stats.latencies, 99),
    overlap_p99_ms: percentile(stats.overlapLatencies, 99),
    status_counts: formatCounts(stats.statuses),
    overlap_status_counts: formatCounts(stats.overlapStatuses),
    error_rate: attempts ? stats.error / attempts : 0,
  };
}

function evaluate(rows, sampleSummary, ledgerCounts) {
  const chatbot = rows.find((row) => row.key === "chatbot");
  const apiBatch = rows.find((row) => row.key === "apiBatch");
  const issues = [];

  if (!chatbot?.overlap_completed) issues.push("chatbot had no overlap completions");
  if (!apiBatch?.overlap_completed) issues.push("api-batch was starved during overlap");

  const completionRatio =
    apiBatch?.overlap_completed > 0 ? chatbot.overlap_completed / apiBatch.overlap_completed : Infinity;
  if (Number.isFinite(completionRatio) && completionRatio < MIN_COMPLETION_RATIO) {
    issues.push(
      `chatbot/api-batch overlap completion ratio ${completionRatio.toFixed(2)} < ${MIN_COMPLETION_RATIO}`,
    );
  }

  for (const row of rows) {
    if (row.error_rate > MAX_ERROR_RATE) {
      issues.push(`${row.tenant} error rate ${(row.error_rate * 100).toFixed(1)}% > ${(MAX_ERROR_RATE * 100).toFixed(1)}%`);
    }
  }

  if (REQUIRE_SATURATION && sampleSummary.samples === 0) {
    issues.push("no fairshare samples captured");
  } else if (
    REQUIRE_SATURATION &&
    sampleSummary.maxQueued === 0 &&
    sampleSummary.maxInFlight < CAPACITY
  ) {
    issues.push("benchmark did not saturate the scheduler");
  }

  const clientTotal = rows.reduce((sum, row) => sum + row.attempts, 0);
  const ledgerTotal = Object.values(ledgerCounts).reduce((sum, value) => sum + value, 0);
  if (clientTotal > 0 && ledgerTotal > 0) {
    const delta = Math.abs(clientTotal - ledgerTotal) / clientTotal;
    if (delta > LEDGER_TOLERANCE) {
      issues.push(`ClickHouse ledger differs from client attempts by ${(delta * 100).toFixed(1)}%`);
    }
  } else {
    issues.push("ledger did not report benchmark attempts");
  }

  return { pass: issues.length === 0, issues, completionRatio, clientTotal, ledgerTotal };
}

async function main() {
  console.log("obleth benchmark");
  console.log(`  model=${MODEL} capacity=${CAPACITY} output_tokens=${OUTPUT_TOKENS}`);
  console.log(`  duration=${DURATION_S}s after last tenant joins; conc=${CONC}/tenant`);
  console.log(`  proxy=${PROXY_BASE} admin=${ADMIN_BASE}`);
  console.log(`  output=${BENCH_OUT_DIR}`);
  if (loadedEnvFiles.length) console.log(`  env=${loadedEnvFiles.join(", ")}`);

  if (process.env.DRY_RUN === "1") return;

  const keys = await seed();
  await setCapacity();

  const startedAt = Date.now();
  const chatbotStart = startedAt + STAGGER_CHATBOT_S * 1000;
  const activeTenants = [
    ["apiBatch", keys.apiBatch, startedAt],
    ["chatbot", keys.chatbot, chatbotStart],
  ];
  if (INCLUDE_CHATBOT2) {
    activeTenants.push([
      "chatbot2",
      keys.chatbot2,
      startedAt + (STAGGER_CHATBOT2_S || STAGGER_CHATBOT_S) * 1000,
    ]);
  }
  if (INCLUDE_ANALYTICS) {
    activeTenants.push([
      "analytics",
      keys.analytics,
      startedAt + (STAGGER_ANALYTICS_S || STAGGER_CHATBOT_S) * 1000,
    ]);
  }

  const overlapStart = Math.max(...activeTenants.map(([, , startAt]) => startAt));
  const endAt = overlapStart + DURATION_S * 1000;
  console.log("  schedule:");
  for (const [name, tenant, startAt] of activeTenants) {
    console.log(`    ${name.padEnd(9)} group=${tenant.group.padEnd(9)} weight=${tenant.weight} start=+${Math.round((startAt - startedAt) / 1000)}s`);
  }

  const stats = Object.fromEntries(activeTenants.map(([name]) => [name, stat()]));
  const tasks = [];
  for (let i = 0; i < CONC; i++) {
    for (const [name, tenant, startAt] of activeTenants) {
      tasks.push(worker(tenant, stats[name], startAt, endAt, overlapStart));
    }
  }

  const sampler = sampleFairshare(endAt + VERIFY_DELAY_MS);
  const chaos = runChaos(overlapStart, endAt);
  await Promise.all([...tasks, chaos]);
  const sampleSummary = await sampler;

  await sleep(VERIFY_DELAY_MS);
  const ledgerCounts = await queryLedger(keys, startedAt);
  const rows = activeTenants.map(([name, tenant]) => summarize(name, tenant, stats[name]));
  const result = evaluate(rows, sampleSummary, ledgerCounts);

  console.log("\nclient results:");
  console.table(rows);
  printResponseDiagnostics(activeTenants, stats);
  console.log("\nledger requests:");
  console.table(ledgerCounts);
  console.log(
    `fairshare samples: ${sampleSummary.samples}  max_in_flight=${sampleSummary.maxInFlight}  max_queued=${sampleSummary.maxQueued}`,
  );
  console.log(`chatbot/api-batch overlap ratio: ${result.completionRatio.toFixed(2)}x`);

  writeFileSync(
    benchPaths.runMeta,
    JSON.stringify(
      {
        started_at_ms: startedAt,
        finished_at_ms: Date.now(),
        config: {
          model: MODEL,
          capacity: CAPACITY,
          duration_s: DURATION_S,
          concurrency: CONC,
          output_tokens: OUTPUT_TOKENS,
          proxy_base: PROXY_BASE,
          chaos: CHAOS,
        },
        rows,
        diagnostics: Object.fromEntries(
          activeTenants.map(([name]) => [
            name,
            {
              status_counts: stats[name].statuses,
              overlap_status_counts: stats[name].overlapStatuses,
              sample_errors: stats[name].sampleErrors,
            },
          ]),
        ),
        ledger_counts: ledgerCounts,
        fairshare_samples: sampleSummary,
        result,
      },
      null,
      2,
    ),
  );
  console.log(`wrote ${benchPaths.runMeta}`);
  console.log(`wrote ${benchPaths.fairshareSamples}`);

  if (!result.pass) {
    console.log("\nFAIL:");
    for (const issue of result.issues) console.log(`  - ${issue}`);
    process.exit(1);
  }

  console.log("\nPASS: fairshare stayed visible under staggered contention");
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function formatCounts(counts) {
  const entries = Object.entries(counts).sort(([a], [b]) => Number(a) - Number(b));
  return entries.map(([status, count]) => `${status}:${count}`).join(" ");
}

function printResponseDiagnostics(activeTenants, stats) {
  const rows = activeTenants.flatMap(([name]) =>
    stats[name].sampleErrors.map((sample, index) => ({
      tenant: name,
      sample: index + 1,
      status: sample.status || "network",
      body: sample.body,
    })),
  );
  if (!rows.length) return;
  console.log("\nnon-200/429 response samples:");
  console.table(rows);
}

function truncate(value, max) {
  return value.length <= max ? value : `${value.slice(0, max - 1)}...`;
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
