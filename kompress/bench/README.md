# kompress load-test harness

Stress-test the neural compression sidecar to find its per-pod capacity and tune
runtime knobs. Everything here is stdlib Python + Docker — no extra deps.

## What it answers

- **Do we need to scale, and to what?** Find the concurrency where p95 latency
  crosses your SLO and throughput flattens — that's one pod's capacity. Size the
  HPA target a bit below it.
- **Which runtime settings are best?** Sweep `KOMPRESS_MAX_BATCH`, ONNX thread
  counts, or the fp32-vs-int8 model and compare on identical load.

## Quick start

```bash
# 1. Start a sidecar (fp32, default batch size):
docker run -d --name kompress -p 8899:8080 \
  ghcr.io/thediymaker/obleth-gateway/obleth-kompress:latest

# 2. Sweep concurrency, sampling the container's CPU%:
python loadtest.py --url http://127.0.0.1:8899 \
  --concurrency 1,2,4,8,16,32 --requests 200 --sentences 40 \
  --docker-name kompress
```

Output columns: `conc` (concurrency), `req/s`, `sent/s` (sentences/s — the real
unit of work), `p50/p95/p99 ms`, `err`, and `cpu a/m` (avg/max CPU% when
`--docker-name` is set).

**Reading it:** throughput rises with concurrency until the pod saturates, then
flattens while p95/p99 climb. That inflection is per-pod capacity. If p95 crosses
your SLO at concurrency N, set the HPA to scale out before you reach N per pod.

## Tuning sweeps

`sweep.sh` starts a fresh container per knob value, load-tests it, and tears it
down — so comparisons are clean.

```bash
# Batch size (biggest lever; 1 == old sequential behavior):
./sweep.sh

# ONNX intra-op threads:
KNOB=OMP_NUM_THREADS VALUES="1 2 4 8" ./sweep.sh

# int8 vs fp32: build an int8 image first, then point IMAGE at it:
#   docker build -f deploy/docker/obleth-kompress.Dockerfile \
#     --build-arg ONNX_FILE=onnx/kompress-int8-wo.onnx -t kompress:int8 .
IMAGE=kompress:int8 ./sweep.sh
```

## Notes

- Load-test from a machine that is **not** competing with the container for CPU,
  or the numbers understate capacity.
- `sent/s` is the metric to optimize — a request's cost scales with its sentence
  count, not the request itself.
- This measures the sidecar in isolation. End-to-end, the upstream LLM dominates
  latency; the sidecar just needs to stay off the critical path (it fails open).
