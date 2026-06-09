// Long-running mixed-traffic soak for obleth.
//
// Where throughput.mjs measures a peak number, this stresses BOTH the gateway
// and the example backend the way a busy tenant fleet would: many models with
// different latency profiles, many tenants across fairshare groups, and a mix
// of usage types (streaming chat, buffered chat, large generations, and
// embeddings). It runs for a long, configurable window, samples fairshare and
// throughput over time, and reconciles client counts against the ClickHouse
// ledger so a drift or slow leak shows up.
//
//   node bench/soak.mjs                 # ~10 min default
//   DURATION_S=3600 node bench/soak.mjs # 1 hour soak
//
// The single fixture backend emulates the fleet via per-model latency profiles
// (model names carry the profile keyword: turbo/large/embed/...).

import { appendFileSync, writeFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import "./env.mjs";
import { benchPaths, BENCH_OUT_DIR } from "./paths.mjs";
import { randomPrompt } from "./prompts.mjs";
import {
  LoadClient,
  adminApi,
  ensureModel,
  ensureGroup,
  ensureTenant,
  mintKey,
  setCapacity,
  newStats,
  record,
  summarizeStats,
  formatCounts,
  weightedPick,
  sleep,
} from "./lib.mjs";

const ADMIN_BASE = process.env.ADMIN_BASE ?? "http://localhost:9180";
const ADMIN_TOKEN = process.env.ADMIN_TOKEN ?? "dev-admin-token";
const PROXY_BASE = process.env.PROXY_BASE ?? "http://localhost";
const BENCHMARK_API_BASE = process.env.BENCHMARK_API_BASE ?? "http://benchmark-backend:8081";

const CONC = Number(process.env.CONC ?? 64);
const DURATION_S = Number(process.env.DURATION_S ?? 600);
const CAPACITY = Number(process.env.CAPACITY ?? 64);
const PROGRESS_S = Number(process.env.PROGRESS_S ?? 10);
const MAX_SOCKETS = Number(process.env.MAX_SOCKETS ?? CONC * 2);
const KEY_NAME = process.env.BENCH_KEY_NAME ?? "soak";
const MAX_ERROR_RATE = Number(process.env.MAX_ERROR_RATE ?? 0.02);
const LEDGER_TOLERANCE = Number(process.env.LEDGER_TOLERANCE ?? 0.2);

const api = adminApi(ADMIN_BASE, ADMIN_TOKEN);

// Model fleet emulated by the fixture. The name carries the latency-profile
// keyword the backend keys off (turbo/large/embed); upstream == name.
const MODELS = [
  { name: "bench-turbo", weight: 100, kind: "chat" },
  { name: "bench-base", weight: 100, kind: "chat" },
  { name: "bench-code", weight: 100, kind: "chat" },
  { name: "bench-large", weight: 100, kind: "chat" },
  { name: "bench-embed", weight: 100, kind: "embed" },
];

// Tenants across fairshare groups, with their share of generated traffic.
const GROUPS = [
  { name: "chatbot", weight: 500 },
  { name: "api", weight: 50 },
  { name: "analytics", weight: 100 },
];
const TENANTS = [
  { name: "soak-chatbot", group: "chatbot", weight: 500, traffic: 35 },
  { name: "soak-chatbot-2", group: "chatbot", weight: 500, traffic: 25 },
  { name: "soak-api-batch", group: "api", weight: 50, traffic: 20 },
  { name: "soak-analytics", group: "analytics", weight: 100, traffic: 15 },
  { name: "soak-embeddings", group: "api", weight: 50, traffic: 5 },
];

// Usage-type catalog: what kind of request, against which model, how big.
const TRAFFIC = [
  { id: "chat-fast-stream", model: "bench-turbo", endpoint: "chat", stream: true, outputTokens: 64, weight: 25 },
  { id: "chat-base-stream", model: "bench-base", endpoint: "chat", stream: true, outputTokens: 128, weight: 20 },
  { id: "chat-base-buffered", model: "bench-base", endpoint: "chat", stream: false, outputTokens: 96, weight: 10 },
  { id: "chat-large-stream", model: "bench-large", endpoint: "chat", stream: true, outputTokens: 256, weight: 10 },
  { id: "chat-code-stream", model: "bench-code", endpoint: "chat", stream: true, outputTokens: 200, weight: 10 },
  { id: "embed-batch", model: "bench-embed", endpoint: "embed", inputs: 8, weight: 25 },
];

async function seed() {
  for (const m of MODELS) {
    await ensureModel(api, {
      model_name: m.name,
      upstream_model: m.name,
      api_base: BENCHMARK_API_BASE,
      context_window: 8192,
      admission_weight: 100,
    });
  }
  for (const g of GROUPS) await ensureGroup(api, g.name, g.weight);

  const secrets = {};
  const tenantTraffic = [];
  for (const t of TENANTS) {
    const tenant = await ensureTenant(api, {
      name: t.name,
      weight: t.weight,
      tokensPerMinute: 1_000_000_000,
      fairshareGroup: t.group,
    });
    secrets[t.name] = await mintKey(api, tenant, KEY_NAME);
    tenantTraffic.push({ value: t.name, weight: t.traffic });
  }
  const cap = await setCapacity(api, CAPACITY);
  console.log(`seeded ${MODELS.length} models, ${TENANTS.length} tenants, capacity max_in_flight=${cap}`);
  return { secrets, tenantTraffic };
}

const tenantPicks = [];
const trafficPicks = TRAFFIC.map((spec) => ({ value: spec, weight: spec.weight }));

function buildRequest(spec) {
  const nonce = randomUUID();
  if (spec.endpoint === "embed") {
    const input = Array.from({ length: spec.inputs }, () => randomPrompt(nonce));
    return { path: "/v1/embeddings", body: JSON.stringify({ model: spec.model, input }) };
  }
  return {
    path: "/v1/chat/completions",
    body: JSON.stringify({
      model: spec.model,
      messages: [
        { role: "system", content: `Soak session ${nonce}. Answer concisely.` },
        { role: "user", content: randomPrompt(nonce) },
      ],
      max_tokens: spec.outputTokens,
      stream: spec.stream,
    }),
  };
}

async function queryLedger(sinceMs) {
  const [tenants, usage] = await Promise.all([api("/tenants"), api(`/usage?since_ms=${sinceMs}`)]);
  const byId = new Map(usage.map((row) => [row.tenant_id, row]));
  const counts = {};
  for (const t of TENANTS) {
    const tenant = tenants.find((x) => x.name === t.name);
    const row = tenant ? byId.get(tenant.id) : undefined;
    counts[t.name] = Number(row?.requests ?? 0);
  }
  return counts;
}

async function main() {
  console.log("obleth soak benchmark");
  console.log(`  duration=${DURATION_S}s conc=${CONC} capacity=${CAPACITY} progress=${PROGRESS_S}s`);
  console.log(`  models=${MODELS.map((m) => m.name).join(",")}`);
  console.log(`  proxy=${PROXY_BASE} admin=${ADMIN_BASE} output=${BENCH_OUT_DIR}`);

  if (process.env.DRY_RUN === "1") return;

  const { secrets, tenantTraffic } = await seed();
  tenantPicks.push(...tenantTraffic);

  const client = new LoadClient({ maxSockets: MAX_SOCKETS });
  const global = newStats();
  const byTenant = Object.fromEntries(TENANTS.map((t) => [t.name, newStats()]));
  const byModel = Object.fromEntries(MODELS.map((m) => [m.name, newStats()]));
  const byType = Object.fromEntries(TRAFFIC.map((t) => [t.id, newStats()]));

  const startedAt = Date.now();
  const endAt = startedAt + DURATION_S * 1000;
  writeFileSync(benchPaths.soakTimeline, "");

  async function worker() {
    while (Date.now() < endAt) {
      const tenantName = weightedPick(tenantPicks);
      const spec = weightedPick(trafficPicks);
      const req = buildRequest(spec);
      const res = await client.request({
        url: `${PROXY_BASE}${req.path}`,
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${secrets[tenantName]}` },
        body: req.body,
      });
      record(global, res);
      record(byTenant[tenantName], res);
      record(byModel[spec.model], res);
      record(byType[spec.id], res);
    }
  }

  const sampler = sampleTimeline(global, endAt);
  await Promise.all(Array.from({ length: CONC }, () => worker()));
  const timeline = await sampler;
  client.destroy();

  console.log("\nreconciling with ClickHouse ledger...");
  await sleep(3000);
  const ledger = await queryLedger(startedAt);

  const tenantRows = TENANTS.map((t) => ({ tenant: t.name, group: t.group, ...summarizeStats(byTenant[t.name]), req_per_s: Math.round(byTenant[t.name].ok / DURATION_S) }));
  const modelRows = MODELS.map((m) => ({ model: m.name, ...summarizeStats(byModel[m.name]), req_per_s: Math.round(byModel[m.name].ok / DURATION_S) }));
  const typeRows = TRAFFIC.map((t) => ({ type: t.id, ...summarizeStats(byType[t.id]) }));
  const overall = summarizeStats(global);
  overall.req_per_s = Math.round(global.ok / DURATION_S);

  console.log("\nper tenant:");
  console.table(tenantRows.map(slimTenant));
  console.log("\nper model:");
  console.table(modelRows.map(slimModel));
  console.log("\nper usage type:");
  console.table(typeRows.map(slimType));
  console.log("\nledger requests (ClickHouse):");
  console.table(ledger);

  const clientTotal = overall.attempts;
  const ledgerTotal = Object.values(ledger).reduce((sum, v) => sum + v, 0);
  const ledgerDelta = clientTotal > 0 ? Math.abs(clientTotal - ledgerTotal) / clientTotal : 1;

  console.log("\noverall:");
  console.log(`  ${overall.completed} ok / ${overall.attempts} attempts  (${overall.req_per_s} req/s avg)`);
  console.log(`  errors ${(overall.error_rate * 100).toFixed(2)}%   429 ${global.rejected}   statuses ${formatCounts(global.statuses)}`);
  console.log(`  ttfb ms p50=${overall.p50_ttfb_ms} p90=${overall.p90_ttfb_ms} p99=${overall.p99_ttfb_ms}`);
  console.log(`  fairshare: samples=${timeline.samples} max_in_flight=${timeline.maxInFlight} max_queued=${timeline.maxQueued}`);
  console.log(`  ledger: client=${clientTotal} clickhouse=${ledgerTotal} delta=${(ledgerDelta * 100).toFixed(1)}%`);

  const issues = [];
  if (overall.error_rate > MAX_ERROR_RATE) issues.push(`error rate ${(overall.error_rate * 100).toFixed(2)}% > ${(MAX_ERROR_RATE * 100).toFixed(2)}%`);
  if (timeline.samples === 0) issues.push("no fairshare samples captured");
  if (ledgerTotal === 0) issues.push("ledger reported no requests");
  else if (ledgerDelta > LEDGER_TOLERANCE) issues.push(`ledger delta ${(ledgerDelta * 100).toFixed(1)}% > ${(LEDGER_TOLERANCE * 100).toFixed(1)}%`);

  writeFileSync(
    benchPaths.soakMeta,
    JSON.stringify(
      {
        started_at_ms: startedAt,
        finished_at_ms: Date.now(),
        config: { duration_s: DURATION_S, conc: CONC, capacity: CAPACITY },
        overall,
        tenants: tenantRows,
        models: modelRows,
        types: typeRows,
        ledger,
        timeline,
        pass: issues.length === 0,
        issues,
      },
      null,
      2,
    ),
  );
  console.log(`\nwrote ${benchPaths.soakMeta}`);
  console.log(`wrote ${benchPaths.soakTimeline}`);

  if (issues.length) {
    console.log("\nFAIL:");
    for (const i of issues) console.log(`  - ${i}`);
    process.exit(1);
  }
  console.log("\nPASS: gateway and backend stayed healthy under sustained mixed load");
}

// One interval drives progress logging, fairshare sampling, and the timeline
// JSONL so a regression in throughput or latency over time is visible.
function sampleTimeline(global, endAt) {
  let resolveDone;
  const done = new Promise((r) => (resolveDone = r));
  let samples = 0;
  let maxQueued = 0;
  let maxInFlight = 0;
  let lastOk = 0;
  let lastTs = Date.now();

  const timer = setInterval(async () => {
    const now = Date.now();
    let snap = {};
    try {
      snap = await api("/fairshare/live");
      samples++;
      maxQueued = Math.max(maxQueued, Number(snap.global_queued ?? 0));
      maxInFlight = Math.max(maxInFlight, Number(snap.global_in_flight ?? 0));
    } catch {
      // Sampling must not perturb the load test.
    }
    const windowOk = global.ok - lastOk;
    const rps = Math.round((windowOk / (now - lastTs)) * 1000);
    lastOk = global.ok;
    lastTs = now;
    const remaining = Math.max(0, Math.round((endAt - now) / 1000));
    console.log(`  ${rps} req/s  ok=${global.ok} err=${global.error} 429=${global.rejected}  in_flight=${snap.global_in_flight ?? "?"} queued=${snap.global_queued ?? "?"}  ${remaining}s left`);
    appendFileSync(
      benchPaths.soakTimeline,
      `${JSON.stringify({ ts: now, rps, ok: global.ok, err: global.error, rejected: global.rejected, ...snap })}\n`,
    );

    if (now >= endAt) {
      clearInterval(timer);
      resolveDone({ samples, maxQueued, maxInFlight });
    }
  }, PROGRESS_S * 1000);

  return done;
}

function slimTenant(row) {
  return {
    tenant: row.tenant,
    group: row.group,
    ok: row.completed,
    err: row.errors,
    "429": row.rejected,
    rps: row.req_per_s,
    p50_ttfb: row.p50_ttfb_ms,
    p99_ttfb: row.p99_ttfb_ms,
  };
}

function slimModel(row) {
  return {
    model: row.model,
    ok: row.completed,
    err: row.errors,
    "429": row.rejected,
    rps: row.req_per_s,
    p50_ttfb: row.p50_ttfb_ms,
    p99_ttfb: row.p99_ttfb_ms,
  };
}

function slimType(row) {
  return {
    type: row.type,
    ok: row.completed,
    err: row.errors,
    "429": row.rejected,
    p50_ttfb: row.p50_ttfb_ms,
    p99_ttfb: row.p99_ttfb_ms,
    p99_total: row.p99_total_ms,
  };
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
