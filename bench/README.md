# obleth benchmark harness

One command proves the behavior we care about: a lower-weight workload can flood
the gateway first, a boosted workload can join later, and fairshare still keeps
the boosted tenant moving without starving the baseline tenant.

Generated keys, run metadata, and fairshare samples are written to
`BENCH_OUT_DIR` (default `/tmp/obleth-bench`). Nothing generated should land in
this source directory.

## Run it

Start the stack first:

```bash
docker compose -f deploy/docker/docker-compose.yml --profile benchmark --profile edge up --build -d
```

Then run the benchmark:

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
