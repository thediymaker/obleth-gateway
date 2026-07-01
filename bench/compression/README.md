# Compression boon — back-to-back A/B benchmark

`ab.py` sends the same requests through the gateway three ways and diffs the
result, to answer two questions: **how much does the compression boon save**, and
**does it make requests faster or slower**.

Three arms per sample:

| arm | header | what runs |
|---|---|---|
| `off` | `x-obleth-boons: off` | nothing — baseline |
| `default` | *(none)* | lossless structural + cross-turn dedup + log template-collapse (+ code, per policy) |
| `lossy` | `x-obleth-boons: lossy` | the above **plus** the neural/heuristic lossy prose pass |

Token counts come from the `x-obleth-compression` response header
(`before=…;after=…;saved=…`), which the gateway emits whenever the compression
boon runs — so you can do the same A/B by hand with two `curl`s.

## Measured vs modeled

The bundled `benchmark-backend` fixture returns canned output and its latency
does **not** scale with prompt size, so it cannot show the upstream prefill time
saved by a shorter prompt. Therefore:

- **Measured (real, reproducible):** tokens saved %, per-content-type ratio,
  gateway-added latency (`arm − off`, with `max_tokens` pinned so the fixture's
  own response time is constant), and cost saved.
- **Modeled (labelled):** `upstream_ms_saved = tokens_saved / prefill_tokens_per_sec`,
  swept across a few prefill rates → the input size where net latency turns
  positive (the crossover). Point `OBLETH_PROXY_URL` at a real vLLM instead for a
  true end-to-end number.

## Prerequisites

- The stack running (`docker compose -f deploy/docker/docker-compose.yml up -d`),
  including the `compressor` profile if you want the neural lossy pass.
- The compression boon **enabled globally** (Settings → Compression, or the run
  does it for you when `OBLETH_ADMIN_TOKEN` is set).
- A **model granted the `compression` boon** and an **API key** that can call it.
  For a clean lossy A/B, use a tenant whose `allow_lossy` is **off** (a fresh
  tenant inherits the global default) so the `default` arm excludes lossy and the
  `lossy` header shows its true marginal effect.

## Run

```bash
OBLETH_API_KEY=sk-...            \
MODEL=your-model-with-compression \
OBLETH_PROXY_URL=http://localhost:8088 \
OBLETH_ADMIN_URL=http://localhost:9180 \
OBLETH_ADMIN_TOKEN=your-admin-token    \
python bench/compression/ab.py
```

`OBLETH_ADMIN_TOKEN` is optional; when set, the run enables the boon globally and
temporarily lowers the per-segment `min_tokens` floor so the size sweep is
visible, then restores it.

Other knobs (env): `PRICE_IN_PER_MTOK` (default 0.30), `PREFILL_TPS`
(default `500,2000,8000`), `MAX_TOKENS` (default 1), `REPS` (default 5),
`BENCH_MIN_TOKENS` (default 64), `OUT` (default `bench/compression/report.md`).

The report is written to `OUT` (UTF-8) and echoed to stdout. A committed
`report.md` shows an example run against the fixture backend.

## Reading the report

- **Bottom line / Token savings** — the deterministic passes (logs, json, dedup)
  save a lot at ~0 ms overhead and keep the answer identical; treat them as free.
- **Cost saved** — deterministic, applies to every request.
- **Net latency** — in each crossover table, a `net` column is positive where
  compression makes the request faster end-to-end at that upstream prefill rate.
  The deterministic sweep is a win at any size; the neural (lossy) sweep only wins
  once `tokens_saved / prefill_rate` beats the ~0.5–0.8 s sidecar overhead — i.e.
  large prompts on slower/cheaper upstreams. Small prompts through the lossy pass
  are a net latency loss (you'd use it there only for the token/cost savings).
