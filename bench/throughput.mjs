// Max-throughput benchmark for obleth.
//
// Answers "how many req/s can the gateway sustain, and what latency does it add
// on top of the upstream?" - the questions behind marketing claims like
// "N req/s" or "Mx faster than <other gateway>". It does this honestly:
//
//   1. (optional) drive the backend DIRECTLY to establish a no-gateway baseline;
//   2. drive the same load THROUGH obleth;
//   3. report sustained req/s and the latency obleth adds over the baseline.
//
// The gateway-added latency (gateway p50 - direct p50) is the apples-to-apples
// number; a competing gateway on identical hardware/backend can be measured the
// same way to substantiate a relative-speed claim.
//
//   node bench/throughput.mjs            # gateway only
//   MODE=both node bench/throughput.mjs  # baseline + gateway + overhead delta
//
// Designed so the BACKEND is not the bottleneck: small outputs, fast model
// profile, and a high backend slot count. Push CONC high to find the ceiling.

import { writeFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import "./env.mjs";
import { benchPaths, BENCH_OUT_DIR } from "./paths.mjs";
import {
  LoadClient,
  adminApi,
  ensureModel,
  ensureTenant,
  mintKey,
  setCapacity,
  newStats,
  record,
  summarizeStats,
  formatCounts,
  sleep,
} from "./lib.mjs";

const ADMIN_BASE = process.env.ADMIN_BASE ?? "http://localhost:9090";
const ADMIN_TOKEN = process.env.ADMIN_TOKEN ?? "dev-admin-token";
const PROXY_BASE = process.env.PROXY_BASE ?? "http://localhost";
const BACKEND_BASE = process.env.BACKEND_BASE ?? "http://localhost:8081";
const BENCHMARK_API_BASE = process.env.BENCHMARK_API_BASE ?? "http://benchmark-backend:8081";

// "turbo" so the backend picks its fast latency profile; the point is to expose
// gateway overhead, not upstream generation time.
const MODEL = process.env.MODEL ?? "bench-turbo";
const UPSTREAM_MODEL = process.env.UPSTREAM_MODEL ?? "bench-turbo";
const MODE = process.env.MODE ?? "gateway"; // gateway | direct | both
const CONC = Number(process.env.CONC ?? 256);
const DURATION_S = Number(process.env.DURATION_S ?? 30);
const WARMUP_S = Number(process.env.WARMUP_S ?? 3);
const OUTPUT_TOKENS = Number(process.env.OUTPUT_TOKENS ?? 4);
const STREAM = process.env.STREAM === "1"; // default off for pure overhead
const CAPACITY = Number(process.env.CAPACITY ?? 100_000);
const MAX_SOCKETS = Number(process.env.MAX_SOCKETS ?? CONC * 2);
const TENANT_NAME = process.env.TENANT_NAME ?? "throughput";
const KEY_NAME = process.env.BENCH_KEY_NAME ?? "throughput";
const MAX_ERROR_RATE = Number(process.env.MAX_ERROR_RATE ?? 0.01);

const api = adminApi(ADMIN_BASE, ADMIN_TOKEN);

function body() {
  const nonce = randomUUID();
  return JSON.stringify({
    model: MODEL,
    messages: [
      { role: "system", content: "Throughput probe. Reply with one word." },
      { role: "user", content: `ping ${nonce}` },
    ],
    max_tokens: OUTPUT_TOKENS,
    stream: STREAM,
  });
}

async function seed() {
  await ensureModel(api, {
    model_name: MODEL,
    upstream_model: UPSTREAM_MODEL,
    api_base: BENCHMARK_API_BASE,
    context_window: 8192,
    admission_weight: 100,
  });
  const tenant = await ensureTenant(api, {
    name: TENANT_NAME,
    weight: 100,
    tokensPerMinute: 1_000_000_000,
    fairshareGroup: null,
  });
  const secret = await mintKey(api, tenant, KEY_NAME);
  const cap = await setCapacity(api, CAPACITY);
  console.log(`seeded model '${MODEL}', tenant '${TENANT_NAME}', capacity max_in_flight=${cap}`);
  return secret;
}

// Closed-loop driver: CONC workers fire back-to-back until endAt. Stats during
// the warmup window are discarded so the number reflects steady state.
async function drive({ url, headers, label }) {
  const client = new LoadClient({ maxSockets: MAX_SOCKETS });
  const stats = newStats();
  const startedAt = Date.now();
  const measureFrom = startedAt + WARMUP_S * 1000;
  const endAt = measureFrom + DURATION_S * 1000;

  async function worker() {
    while (Date.now() < endAt) {
      const res = await client.request({ url, method: "POST", headers, body: body() });
      if (res.startedAt >= measureFrom) record(stats, res);
    }
  }

  const workers = Array.from({ length: CONC }, () => worker());
  const progress = logProgress(label, stats, endAt);
  await Promise.all(workers);
  clearInterval(progress);
  client.destroy();

  const summary = summarizeStats(stats);
  summary.label = label;
  summary.req_per_s = Math.round(summary.completed / DURATION_S);
  summary.status_counts = formatCounts(stats.statuses);
  summary.sample_errors = stats.sampleErrors;
  return summary;
}

function logProgress(label, stats, endAt) {
  let last = stats.ok;
  let lastTs = Date.now();
  return setInterval(() => {
    const now = Date.now();
    const delta = stats.ok - last;
    const rps = Math.round((delta / (now - lastTs)) * 1000);
    last = stats.ok;
    lastTs = now;
    const remaining = Math.max(0, Math.round((endAt - now) / 1000));
    console.log(`  [${label}] ${rps} req/s  ok=${stats.ok} err=${stats.error} 429=${stats.rejected}  ${remaining}s left`);
  }, 2000);
}

function printSummary(title, s) {
  console.log(`\n${title}`);
  console.log(`  requests:   ${s.completed} ok / ${s.attempts} attempted  (${s.req_per_s} req/s)`);
  console.log(`  errors:     ${s.errors} (${(s.error_rate * 100).toFixed(2)}%)   429: ${s.rejected}`);
  console.log(`  ttfb ms:    p50=${s.p50_ttfb_ms}  p90=${s.p90_ttfb_ms}  p99=${s.p99_ttfb_ms}`);
  console.log(`  total ms:   p50=${s.p50_total_ms}  p90=${s.p90_total_ms}  p99=${s.p99_total_ms}`);
  console.log(`  statuses:   ${s.status_counts}`);
  if (s.sample_errors.length) {
    for (const e of s.sample_errors) console.log(`    err ${e.status}: ${e.body}`);
  }
}

async function main() {
  console.log("obleth throughput benchmark");
  console.log(`  mode=${MODE} model=${MODEL} conc=${CONC} duration=${DURATION_S}s warmup=${WARMUP_S}s`);
  console.log(`  output_tokens=${OUTPUT_TOKENS} stream=${STREAM} capacity=${CAPACITY}`);
  console.log(`  proxy=${PROXY_BASE} backend=${BACKEND_BASE} admin=${ADMIN_BASE}`);
  console.log(`  output=${BENCH_OUT_DIR}`);

  if (process.env.DRY_RUN === "1") return;

  const secret = await seed();

  let direct;
  let gateway;

  if (MODE === "direct" || MODE === "both") {
    console.log("\ndriving backend directly (no gateway)...");
    direct = await drive({
      url: `${BACKEND_BASE}/v1/chat/completions`,
      headers: { "content-type": "application/json" },
      label: "direct",
    });
    printSummary("direct backend baseline", direct);
    if (MODE === "both") await sleep(1000);
  }

  if (MODE === "gateway" || MODE === "both") {
    console.log("\ndriving through obleth...");
    gateway = await drive({
      url: `${PROXY_BASE}/v1/chat/completions`,
      headers: { "content-type": "application/json", authorization: `Bearer ${secret}` },
      label: "gateway",
    });
    printSummary("through obleth gateway", gateway);
  }

  let overhead = null;
  if (direct && gateway) {
    overhead = {
      added_p50_ttfb_ms: gateway.p50_ttfb_ms - direct.p50_ttfb_ms,
      added_p99_ttfb_ms: gateway.p99_ttfb_ms - direct.p99_ttfb_ms,
      throughput_retention:
        direct.req_per_s > 0 ? Number((gateway.req_per_s / direct.req_per_s).toFixed(3)) : null,
    };
    console.log("\noverhead (gateway vs direct):");
    console.log(`  added p50 ttfb: ${overhead.added_p50_ttfb_ms} ms`);
    console.log(`  added p99 ttfb: ${overhead.added_p99_ttfb_ms} ms`);
    console.log(`  throughput retained: ${(overhead.throughput_retention * 100).toFixed(1)}% of direct`);
  }

  const result = gateway ?? direct;
  const pass = result.error_rate <= MAX_ERROR_RATE;

  writeFileSync(
    benchPaths.throughputMeta,
    JSON.stringify(
      {
        finished_at_ms: Date.now(),
        config: { mode: MODE, model: MODEL, conc: CONC, duration_s: DURATION_S, warmup_s: WARMUP_S, output_tokens: OUTPUT_TOKENS, stream: STREAM, capacity: CAPACITY },
        direct,
        gateway,
        overhead,
        pass,
      },
      null,
      2,
    ),
  );
  console.log(`\nwrote ${benchPaths.throughputMeta}`);

  if (!pass) {
    console.log(`\nFAIL: error rate ${(result.error_rate * 100).toFixed(2)}% > ${(MAX_ERROR_RATE * 100).toFixed(2)}%`);
    process.exit(1);
  }
  console.log(`\nPASS: ${result.req_per_s} req/s sustained at ${(result.error_rate * 100).toFixed(2)}% errors`);
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
