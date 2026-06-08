// Max-push benchmark for obleth - "how hard can we drive the gateway?"
//
// throughput.mjs answers "what does the gateway add over the backend" with a
// single closed-loop driver. That driver is one Node event loop, so it tops out
// at a few thousand req/s before the GENERATOR (not obleth) becomes the
// bottleneck. This script fans the load out across multiple worker threads so
// the load generator scales with cores and the gateway is what saturates.
//
//   node bench/max.mjs                       # auto workers, push for the ceiling
//   WORKERS=8 CONC=4096 node bench/max.mjs    # explicit fan-out
//   DURATION_S=60 node bench/max.mjs          # longer measured window
//
// Like throughput.mjs it targets the fast `bench-turbo` profile with tiny
// outputs and a huge backend slot count, and decouples gateway CAPACITY from
// CONC so admission never gates - the point is to find obleth's req/s ceiling.
//
// To go beyond one host's cores, run this on several machines against the same
// PROXY_BASE and sum the reported req/s.

import { writeFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { cpus } from "node:os";
import {
  Worker,
  isMainThread,
  parentPort,
  workerData,
} from "node:worker_threads";
import { fileURLToPath } from "node:url";
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
} from "./lib.mjs";

const ADMIN_BASE = process.env.ADMIN_BASE ?? "http://localhost:9090";
const ADMIN_TOKEN = process.env.ADMIN_TOKEN ?? "dev-admin-token";
const PROXY_BASE = process.env.PROXY_BASE ?? "http://localhost";
const BENCHMARK_API_BASE = process.env.BENCHMARK_API_BASE ?? "http://benchmark-backend:8081";

// "turbo" picks the backend's fast latency profile; tiny outputs keep the
// upstream off the critical path so obleth is what we measure.
const MODEL = process.env.MODEL ?? "bench-turbo";
const UPSTREAM_MODEL = process.env.UPSTREAM_MODEL ?? "bench-turbo";
const WORKERS = Number(process.env.WORKERS ?? Math.max(1, (cpus()?.length ?? 4) - 1));
const CONC = Number(process.env.CONC ?? 2048);
const DURATION_S = Number(process.env.DURATION_S ?? 30);
const WARMUP_S = Number(process.env.WARMUP_S ?? 3);
const OUTPUT_TOKENS = Number(process.env.OUTPUT_TOKENS ?? 4);
const STREAM = process.env.STREAM === "1"; // default off: pure req/s, not TTFT
const CAPACITY = Number(process.env.CAPACITY ?? 100_000);
const TENANT_NAME = process.env.TENANT_NAME ?? "maxpush";
const KEY_NAME = process.env.BENCH_KEY_NAME ?? "maxpush";
const MAX_ERROR_RATE = Number(process.env.MAX_ERROR_RATE ?? 0.01);

// ---------------------------------------------------------------------------
// Worker thread: owns a slice of the total concurrency, its own keep-alive pool,
// and reports periodic completion ticks plus a final raw-stats payload so the
// main thread can compute true global percentiles (not an average of averages).
// ---------------------------------------------------------------------------
if (!isMainThread) {
  await runWorker(workerData);
}

async function runWorker(cfg) {
  const client = new LoadClient({ maxSockets: cfg.maxSockets });
  const stats = newStats();

  function body() {
    const nonce = randomUUID();
    return JSON.stringify({
      model: cfg.model,
      messages: [
        { role: "system", content: "Max-push probe. Reply with one word." },
        { role: "user", content: `ping ${nonce}` },
      ],
      max_tokens: cfg.outputTokens,
      stream: cfg.stream,
    });
  }

  // Report completions since the last tick so the parent can show live, combined
  // req/s without each worker shipping its full state every interval.
  let lastReported = 0;
  const ticker = setInterval(() => {
    const delta = stats.ok - lastReported;
    lastReported = stats.ok;
    parentPort.postMessage({ type: "tick", ok: delta });
  }, 1000);

  async function lane() {
    while (Date.now() < cfg.endAt) {
      const res = await client.request({
        url: cfg.url,
        method: "POST",
        headers: cfg.headers,
        body: body(),
      });
      // Discard warmup so the number reflects steady state.
      if (res.startedAt >= cfg.measureFrom) record(stats, res);
    }
  }

  await Promise.all(Array.from({ length: cfg.lanes }, () => lane()));
  clearInterval(ticker);
  client.destroy();

  // Ship raw latency samples + counters so the parent merges exact percentiles.
  parentPort.postMessage({
    type: "done",
    payload: {
      ok: stats.ok,
      rejected: stats.rejected,
      error: stats.error,
      bytes: stats.bytes,
      ttfb: stats.ttfb,
      total: stats.total,
      statuses: stats.statuses,
      sampleErrors: stats.sampleErrors,
    },
  });
}

// ---------------------------------------------------------------------------
// Main thread: seed, spawn the fan-out, aggregate live ticks and final stats.
// ---------------------------------------------------------------------------
const api = adminApi(ADMIN_BASE, ADMIN_TOKEN);

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

// Spread CONC lanes across WORKERS threads as evenly as possible.
function laneSplit(total, workers) {
  const base = Math.floor(total / workers);
  const extra = total % workers;
  return Array.from({ length: workers }, (_, i) => base + (i < extra ? 1 : 0)).filter((n) => n > 0);
}

// Merge raw per-worker payloads into one stats object so percentiles are
// computed over the whole population, not averaged per worker.
function mergeStats(payloads) {
  const merged = newStats();
  for (const p of payloads) {
    merged.ok += p.ok;
    merged.rejected += p.rejected;
    merged.error += p.error;
    merged.bytes += p.bytes;
    merged.ttfb.push(...p.ttfb);
    merged.total.push(...p.total);
    for (const [status, count] of Object.entries(p.statuses)) {
      merged.statuses[status] = (merged.statuses[status] ?? 0) + count;
    }
    for (const e of p.sampleErrors) {
      if (merged.sampleErrors.length < 3) merged.sampleErrors.push(e);
    }
  }
  return merged;
}

function main() {
  console.log("obleth max-push benchmark");
  console.log(`  workers=${WORKERS} conc=${CONC} duration=${DURATION_S}s warmup=${WARMUP_S}s`);
  console.log(`  model=${MODEL} output_tokens=${OUTPUT_TOKENS} stream=${STREAM} capacity=${CAPACITY}`);
  console.log(`  proxy=${PROXY_BASE} admin=${ADMIN_BASE} output=${BENCH_OUT_DIR}`);

  if (process.env.DRY_RUN === "1") return Promise.resolve();

  return run();
}

async function run() {
  const secret = await seed();

  const lanesPerWorker = laneSplit(CONC, WORKERS);
  const workerCount = lanesPerWorker.length;
  const totalLanes = lanesPerWorker.reduce((sum, n) => sum + n, 0);

  // Aligned clock so every worker shares the same warmup/measure window.
  const startAt = Date.now() + 500;
  const measureFrom = startAt + WARMUP_S * 1000;
  const endAt = measureFrom + DURATION_S * 1000;
  const filename = fileURLToPath(import.meta.url);

  console.log(`\nfanning out ${totalLanes} lanes across ${workerCount} workers...`);

  const payloads = [];
  let liveOk = 0;
  let lastLiveOk = 0;
  let lastTs = Date.now();
  let started = false;

  const progress = setInterval(() => {
    if (Date.now() < measureFrom) return;
    if (!started) {
      started = true;
      lastLiveOk = liveOk;
      lastTs = Date.now();
      return;
    }
    const now = Date.now();
    const rps = Math.round(((liveOk - lastLiveOk) / (now - lastTs)) * 1000);
    lastLiveOk = liveOk;
    lastTs = now;
    const remaining = Math.max(0, Math.round((endAt - now) / 1000));
    console.log(`  ${rps} req/s combined  ok=${liveOk}  ${remaining}s left`);
  }, 2000);

  await Promise.all(
    lanesPerWorker.map(
      (lanes) =>
        new Promise((resolve, reject) => {
          const worker = new Worker(filename, {
            workerData: {
              url: `${PROXY_BASE}/v1/chat/completions`,
              headers: { "content-type": "application/json", authorization: `Bearer ${secret}` },
              model: MODEL,
              outputTokens: OUTPUT_TOKENS,
              stream: STREAM,
              lanes,
              maxSockets: lanes * 2,
              measureFrom,
              endAt,
            },
          });
          worker.on("message", (msg) => {
            if (msg.type === "tick") liveOk += msg.ok;
            else if (msg.type === "done") payloads.push(msg.payload);
          });
          worker.on("error", reject);
          worker.on("exit", (code) =>
            code === 0 ? resolve() : reject(new Error(`worker exited with code ${code}`)),
          );
        }),
    ),
  );
  clearInterval(progress);

  const merged = mergeStats(payloads);
  const summary = summarizeStats(merged);
  summary.req_per_s = Math.round(summary.completed / DURATION_S);
  summary.status_counts = formatCounts(merged.statuses);

  console.log("\nmax-push result (through obleth):");
  console.log(`  workers:    ${workerCount}  lanes:  ${totalLanes}`);
  console.log(`  requests:   ${summary.completed} ok / ${summary.attempts} attempted  (${summary.req_per_s} req/s)`);
  console.log(`  errors:     ${summary.errors} (${(summary.error_rate * 100).toFixed(2)}%)   429: ${summary.rejected}`);
  console.log(`  ttfb ms:    p50=${summary.p50_ttfb_ms}  p90=${summary.p90_ttfb_ms}  p99=${summary.p99_ttfb_ms}`);
  console.log(`  total ms:   p50=${summary.p50_total_ms}  p90=${summary.p90_total_ms}  p99=${summary.p99_total_ms}`);
  console.log(`  statuses:   ${summary.status_counts}`);
  for (const e of merged.sampleErrors) console.log(`    err ${e.status}: ${e.body}`);

  const pass = summary.error_rate <= MAX_ERROR_RATE;
  writeFileSync(
    benchPaths.maxMeta,
    JSON.stringify(
      {
        finished_at_ms: Date.now(),
        config: {
          workers: workerCount,
          conc: totalLanes,
          duration_s: DURATION_S,
          warmup_s: WARMUP_S,
          output_tokens: OUTPUT_TOKENS,
          stream: STREAM,
          capacity: CAPACITY,
          model: MODEL,
        },
        result: summary,
        pass,
      },
      null,
      2,
    ),
  );
  console.log(`\nwrote ${benchPaths.maxMeta}`);

  if (!pass) {
    console.log(`\nFAIL: error rate ${(summary.error_rate * 100).toFixed(2)}% > ${(MAX_ERROR_RATE * 100).toFixed(2)}%`);
    process.exit(1);
  }
  console.log(`\nPASS: ${summary.req_per_s} req/s sustained at ${(summary.error_rate * 100).toFixed(2)}% errors`);
}

if (isMainThread) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
