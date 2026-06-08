# obleth benchmark harness

Four scenarios, one GPU-free fixture backend. Together they let you stress the
gateway and the example backend, and put real numbers behind claims like
"N req/s" or "Mx faster than <other gateway>".

| Scenario | Script | Question it answers |
| --- | --- | --- |
| Fairshare | `run-benchmark.mjs` | Does weighted fair queuing keep a boosted tenant moving without starving a baseline tenant under staggered contention? |
| Max throughput | `throughput.mjs` | How many req/s can obleth sustain, and how much latency does it add over hitting the backend directly? |
| Max push | `max.mjs` | How high can req/s go when the load generator is fanned out across cores so the gateway - not one Node event loop - is the bottleneck? |
| Soak (mixed) | `soak.mjs` | Does the gateway + backend stay healthy for a long window under many models, tenants, and usage types, with the ledger reconciling? |

Generated keys, run metadata, and samples are written to `BENCH_OUT_DIR`
(default `/tmp/obleth-bench`). Nothing generated should land in this source
directory.

Start the stack first (all scenarios need it):

```bash
docker compose -f deploy/docker/docker-compose.yml --profile benchmark --profile edge up --build -d
```

## How we substantiate the claims

Marketing numbers like "50x faster than <gateway>" or "5,000 req/s" only mean
something if they are reproducible and apples-to-apples. The harness is built to
be honest about that:

- **req/s** comes from `throughput.mjs`: a closed-loop driver with a tuned
  keep-alive connection pool, a warmup window that is discarded, and small
  outputs against the fast `bench-turbo` profile so the *backend is not the
  bottleneck*. The reported number is steady-state completions / measured
  seconds.
- **"faster than X"** is the *gateway-added latency*: run `MODE=both` to measure
  the backend directly, then through obleth, on identical hardware and backend.
  The honest figure is `gateway_p50 - direct_p50` (overhead), and the throughput
  retained vs direct. To compare against another gateway, point its upstream at
  this same fixture and run the same probe.
- **fleet realism** comes from per-model latency profiles in the fixture
  backend: model names containing `turbo/flash/mini/fast/small` are quicker,
  `large/70b/xl/opus/heavy` are slower, `embed` returns vectors with no
  per-token delay. One container emulates a heterogeneous fleet.

## The fixture backend

`benchmark-backend` is an OpenAI-compatible server with no GPU. It streams SSE
chat completions with configurable TTFT/per-token latency, serves buffered chat
and `/v1/embeddings`, and exposes `GET /stats` with process-wide request/token
counters so you can measure exactly how many requests reached the upstream
(e.g. to quantify gateway cache offload). A global slot semaphore simulates a
saturated cluster so fairshare is observable.

Env knobs (legacy `MOCK_*` names still accepted): `BENCHMARK_BACKEND_TTFT_MS`,
`BENCHMARK_BACKEND_TOKEN_MS`, `BENCHMARK_BACKEND_DEFAULT_OUTPUT`,
`BENCHMARK_BACKEND_CONCURRENCY`, `BENCHMARK_BACKEND_LISTEN`.

---

## Scenario 1: fairshare (`run-benchmark.mjs`)

One command proves the behavior we care about: a lower-weight workload can flood
the gateway first, a boosted workload can join later, and fairshare still keeps
the boosted tenant moving without starving the baseline tenant.

### Run it

```bash
node bench/run-benchmark.mjs
```

You can also put benchmark settings in `bench/.env` (ignored by git):

```bash
MODEL=gemma4-31b-it
CAPACITY=16
DURATION_S=120
CONC=64
OUTPUT_TOKENS=150
PROXY_BASE=http://localhost
ADMIN_BASE=http://localhost:9090
```

On Node 22 you do not need `--env-file`; the runner loads `bench/.env`
automatically. Shell environment variables still win over values in the file.

The runner will:

1. Register `benchmark-endpoint` when `MODEL=benchmark-endpoint`, or verify your real model is already registered.
2. Create/update fairshare groups and tenants: `chatbot` weight 500, `api-batch` weight 50, and optional extra tenants.
3. Reuse the saved tenant keys in `$BENCH_OUT_DIR/keys.json` when they still exist, otherwise mint fresh `sk_*` keys.
4. Set the live gateway capacity with `PUT /api/v1/capacity`.
5. Run staggered load: `api-batch` starts first, then `chatbot` joins under contention.
6. Sample `GET /api/v1/fairshare/live` into `$BENCH_OUT_DIR/fairshare-samples.jsonl`.
7. Query ClickHouse usage, compare client results with the ledger, and exit non-zero on failure.

## Real backend run

Register the model in the control plane first, then set `MODEL` to that
registered model name:

```bash
CAPACITY=16 \
DURATION_S=120 \
OUTPUT_TOKENS=150 \
MODEL=gemma4-31b-it \
CONC=64 \
PROXY_BASE=http://localhost \
node bench/run-benchmark.mjs
```

For a quick low-load check against a real backend, lower the duration and output:

```bash
CAPACITY=8 DURATION_S=30 OUTPUT_TOKENS=32 MODEL=gemma4-31b-it node bench/run-benchmark.mjs
```

## Optional chaos

Enable chaos inside the same benchmark run:

```bash
CHAOS=1 node bench/run-benchmark.mjs
```

The runner pauses ClickHouse, then Redis, while load is active. Requests should
continue: ClickHouse telemetry spills to the WAL and Redis failures use the
in-process cache/fail-open budget path.

With Podman Compose:

```bash
CONTAINER_CLI=podman CHAOS=1 node bench/run-benchmark.mjs
```

## Environment

| Env | Default | Purpose |
| --- | --- | --- |
| `ADMIN_BASE` | `http://localhost:9090` | Management API base URL |
| `ADMIN_TOKEN` | `dev-admin-token` | Management API bearer token |
| `PROXY_BASE` | `http://localhost` | Data-plane base URL, usually HAProxy |
| `MODEL` | `benchmark-endpoint` | Registered model name to request |
| `BENCHMARK_API_BASE` | `http://benchmark-backend:8081` | Upstream base used when auto-creating/updating `benchmark-endpoint` |
| `MOCK_API_BASE` | unset | Legacy alias used when `BENCHMARK_API_BASE` is not set |
| `CAPACITY` | `8` | Live global in-flight limit set before load |
| `DURATION_S` | `60` | Overlap duration after all active tenants have joined |
| `STAGGER_CHATBOT_S` | `10` | Seconds `api-batch` gets to flood before `chatbot` joins |
| `CONC` | `32` | Worker count per active tenant |
| `OUTPUT_TOKENS` | `150` | `max_tokens` per request |
| `STREAM` | `1` | Request SSE streaming (`stream:true`). Set `0` for buffered responses; streaming yields realistic TTFT (first-token) instead of full-generation TTFT |
| `INCLUDE_CHATBOT2` | unset | Set `1` to add a second tenant in the `chatbot` group |
| `INCLUDE_ANALYTICS` | unset | Set `1` to add an `analytics` group tenant |
| `BENCH_KEY_NAME` | `bench` | Name assigned to generated benchmark keys |
| `BENCH_REUSE_KEYS` | `1` | Set `0` to force fresh benchmark keys |
| `BENCH_PRUNE_KEYS` | `1` | Set `0` to keep older `bench` keys for the benchmark tenants |
| `MIN_COMPLETION_RATIO` | `2` | Minimum chatbot/api-batch overlap completion ratio |
| `MAX_ERROR_RATE` | `0.05` | Maximum client transport/upstream error rate per tenant |
| `LEDGER_TOLERANCE` | `0.2` | Allowed ClickHouse/client completion delta |
| `REQUIRE_SATURATION` | `1` | Set `0` for low-load runs that may not fill the queue |
| `CHAOS` | unset | Set `1` to pause ClickHouse and Redis during the run |
| `CONTAINER_CLI` | `docker` | Compose CLI used by chaos mode (`docker` or `podman`) |
| `COMPOSE_FILE` | `deploy/docker/docker-compose.yml` | Compose file used by chaos mode |
| `BENCH_OUT_DIR` | `/tmp/obleth-bench` | Output directory for generated artifacts |

## What good looks like

The command exits with `PASS` when:

- both tenants complete requests during the overlap window;
- `api-batch` is not starved;
- `chatbot` completes materially more overlap work than `api-batch`;
- the scheduler is saturated or fairshare samples prove active contention;
- ClickHouse usage is close to the client-observed completions; and
- client error rates stay below the threshold.

The old split scripts were easy to misuse: the global target mode could let the
faster tenant consume the shared request budget, and the chaos script had to be
coordinated by hand. `run-benchmark.mjs` keeps the setup, load, live sampling,
ledger check, and optional chaos in one reproducible path.

---

## Scenario 2: max throughput (`throughput.mjs`)

Finds the sustained req/s ceiling and the latency obleth adds over hitting the
backend directly. A closed-loop driver fires `CONC` workers back-to-back; a
warmup window is discarded so the number reflects steady state.

```bash
# gateway only
node bench/throughput.mjs

# baseline (direct) + gateway + overhead delta
MODE=both node bench/throughput.mjs

# push for the ceiling
MODE=both CONC=512 DURATION_S=60 node bench/throughput.mjs
```

Defaults target the fast `bench-turbo` profile with tiny outputs and a huge
backend slot count so the *gateway* is what you are measuring, not generation
time. Output goes to `$BENCH_OUT_DIR/throughput-meta.json`.

Reading the result:

- `req/s` — steady-state completions / `DURATION_S`.
- `ttfb p50/p99` — time to first byte (the latency a streaming client feels).
- `MODE=both` adds **overhead**: `added p50 ttfb` (gateway minus direct) and
  `throughput retained` (gateway req/s as a fraction of direct). These are the
  honest, apples-to-apples figures for a "faster than X" comparison — point the
  other gateway at the same fixture and run the same probe.

| Env | Default | Purpose |
| --- | --- | --- |
| `MODE` | `gateway` | `gateway`, `direct`, or `both` |
| `CONC` | `256` | Concurrent closed-loop workers |
| `DURATION_S` | `30` | Measured window (after warmup) |
| `WARMUP_S` | `3` | Discarded ramp-up seconds |
| `OUTPUT_TOKENS` | `4` | `max_tokens` per request (keep small) |
| `STREAM` | `0` | Set `1` for SSE; default buffered for pure overhead |
| `CAPACITY` | `100000` | Gateway global in-flight (set high to not gate) |
| `MODEL` | `bench-turbo` | Registered model; `turbo` = fast backend profile |
| `BACKEND_BASE` | `http://localhost:8081` | Direct backend URL for the baseline |
| `MAX_SOCKETS` | `CONC*2` | Keep-alive pool size |
| `MAX_ERROR_RATE` | `0.01` | Fail above this client error rate |

`PASS` when the measured error rate stays under `MAX_ERROR_RATE`.

---

## Scenario 3: max push (`max.mjs`)

`throughput.mjs` runs a single closed-loop driver, which is one Node event loop
and tops out at a few thousand req/s before the *generator* - not obleth -
becomes the bottleneck. `max.mjs` fans the same fast-path load out across
`WORKERS` worker threads so the load generator scales with cores and the gateway
is what saturates. It targets the fast `bench-turbo` profile with tiny outputs
and decouples gateway `CAPACITY` from `CONC` so admission never gates - the goal
is to find obleth's req/s ceiling.

```bash
# auto workers (CPU-1), push for the ceiling
node bench/max.mjs

# explicit fan-out
WORKERS=8 CONC=4096 DURATION_S=60 node bench/max.mjs
```

The combined req/s is printed live and percentiles are computed over the merged
population of all workers (not an average of averages). Output goes to
`$BENCH_OUT_DIR/max-meta.json`.

To go past one host's cores, run `max.mjs` on several machines against the same
`PROXY_BASE` and sum the reported req/s.

| Env | Default | Purpose |
| --- | --- | --- |
| `WORKERS` | `CPU count - 1` | Worker threads to fan the load across |
| `CONC` | `2048` | Total concurrent lanes, split evenly across workers |
| `DURATION_S` | `30` | Measured window (after warmup) |
| `WARMUP_S` | `3` | Discarded ramp-up seconds |
| `OUTPUT_TOKENS` | `4` | `max_tokens` per request (keep small) |
| `STREAM` | `0` | Set `1` for SSE; default buffered for pure req/s |
| `CAPACITY` | `100000` | Gateway global in-flight (set high to not gate) |
| `MODEL` | `bench-turbo` | Registered model; `turbo` = fast backend profile |
| `MAX_ERROR_RATE` | `0.01` | Fail above this client error rate |

`PASS` when the measured error rate stays under `MAX_ERROR_RATE`.

---

## Scenario 4: soak / mixed traffic (`soak.mjs`)

A long, configurable run that stresses both the gateway and the example backend
the way a busy fleet would: 5 models with different latency profiles, 5 tenants
across 3 fairshare groups, and 6 usage types (streaming chat, buffered chat,
large generations, code, and embeddings). It samples fairshare and throughput
over time, then reconciles client counts against the ClickHouse ledger.

```bash
# ~10 min default
node bench/soak.mjs

# 1 hour soak
DURATION_S=3600 node bench/soak.mjs
```

Output: `$BENCH_OUT_DIR/soak-meta.json` (full breakdown) and
`$BENCH_OUT_DIR/soak-timeline.jsonl` (per-interval req/s, in-flight, queued — so
a slow throughput or latency drift over time is visible).

The single fixture backend emulates the fleet via per-model latency profiles
(model names carry the keyword: `bench-turbo`, `bench-base`, `bench-code`,
`bench-large`, `bench-embed`).

| Env | Default | Purpose |
| --- | --- | --- |
| `DURATION_S` | `600` | Soak length in seconds |
| `CONC` | `64` | Concurrent workers across all tenants/models |
| `CAPACITY` | `64` | Gateway global in-flight limit |
| `PROGRESS_S` | `10` | Sampling + progress-log interval |
| `MAX_ERROR_RATE` | `0.02` | Fail above this overall error rate |
| `LEDGER_TOLERANCE` | `0.2` | Allowed ClickHouse/client request delta |

`PASS` when error rate stays under threshold, fairshare samples were captured,
and the ClickHouse ledger reconciles with client attempts.
