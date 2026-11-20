# compressor load-test harness

Stress-test the neural compression sidecar to find its per-pod capacity and tune
runtime knobs. Everything here is stdlib Python + Docker — no extra deps.

## What it answers

- **Do we need to scale, and to what?** Find the concurrency where p95 latency
  crosses your SLO and throughput flattens — that's one pod's capacity. Size the
  HPA target a bit below it.
- **Which runtime settings are best?** Sweep `COMPRESSOR_MAX_BATCH`,
  `COMPRESSOR_NUM_THREADS`, or the fp32-vs-int8 model and compare on identical
  load.

## Quick start

```bash
# 1. Start a sidecar (fp32, default batch size):
docker run -d --name compressor -p 8899:8080 \
  ghcr.io/thediymaker/obleth-gateway/obleth-compressor:latest

# 2. Sweep concurrency, sampling the container's CPU%:
python loadtest.py --url http://127.0.0.1:8899 \
  --concurrency 1,2,4,8,16,32 --requests 200 --sentences 40 \
  --docker-name compressor
```

Output columns: `conc` (concurrency), `req/s`, `sent/s` (sentences/s — the real
unit of work), `p50/p95/p99 ms`, `err`, and `cpu a/m` (avg/max CPU% when
`--docker-name` is set).

Uniform sentences flatter batching: real prose mixes 3-word and 60-word
sentences, so measure with `--mixed SEGMENTS` (3-60 words, spread over that many
segments), and add `--deadline-ms 800` to send the gateway's timeout header, so
requests the sidecar sheds (`503`) or drops late (`504`) count as `err`:

```bash
python loadtest.py --url http://127.0.0.1:8899 --concurrency 1,4,8,16 \
  --requests 100 --sentences 24 --mixed 6 --deadline-ms 800
```

**Reading it:** throughput rises with concurrency until the pod saturates, then
flattens while p95/p99 climb. That inflection is per-pod capacity. If p95 crosses
your SLO at concurrency N, set the HPA to scale out before you reach N per pod.

## Tuning sweeps

`sweep.sh` starts a fresh container per knob value, load-tests it, and tears it
down — so comparisons are clean.

```bash
# Chunk size (8 is the default; measured fastest on mixed prose):
ARGS="--concurrency 1,4 --requests 100 --sentences 40 --mixed 8" ./sweep.sh

# onnxruntime intra-op threads (the image sets 4):
KNOB=COMPRESSOR_NUM_THREADS VALUES="1 2 4 8" ./sweep.sh

# int8 vs fp32 (int8 measured 30% slower at half the memory; see
# ../README.md): build an int8 image first, then point IMAGE at it:
#   docker build -f deploy/docker/obleth-compressor.Dockerfile \
#     --build-arg ONNX_FILE=onnx/kompress-int8-wo.onnx -t compressor:int8 .
IMAGE=compressor:int8 ./sweep.sh
```

## Notes

- Load-test from a machine that is **not** competing with the container for CPU,
  or the numbers understate capacity.
- `sent/s` is the metric to optimize — a request's cost scales with its sentence
  count, not the request itself.
- This measures the sidecar in isolation. End-to-end, the upstream LLM dominates
  latency; the sidecar just needs to stay off the critical path (it fails open).
- At the defaults a pod scores one request at a time and queues 8, so past
  concurrency 9 the extra requests are answered `503` at once; latency of the
  rest stays bounded. Raise `COMPRESSOR_MAX_WAITING` to measure the queue.
