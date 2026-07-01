# Compression boon — back-to-back A/B

Model `bench-compress` via `http://localhost:8088` · 5 reps (median) · max_tokens=1 · input price $0.3/1M tok.

Three arms per sample: **off** (`x-obleth-boons: off`), **default** (lossless + dedup + log passes), **lossy** (`x-obleth-boons: lossy`, adds the prose pass). Token counts come from the `x-obleth-compression` response header.

## Bottom line

- **Lossless/near-lossless is a free lunch.** logs **98%**, json **44%**, cross-turn dedup **29%** — exact same answer, ~0 ms gateway overhead. Cheaper and no downside; leave it on.
- **Lossy prose is a real trade.** prose **82%** but **+569 ms** gateway overhead (the neural sidecar hop) and it can change wording. Worth it for big, low-value-density text on a slow/expensive upstream; not for small prompts.
- **Why use it:** token cost drops on every request (deterministic), and for large compressible inputs the shorter prompt also cuts upstream prefill time — see the crossover below.

## Token savings (measured)

| corpus | arm | tokens in | after | saved % | gateway +ms |
|---|---|--:|--:|--:|--:|
| logs (repetitive) | off | - | - | 0.0% | +0.0 |
|  | default | 2640 | 46 | 98.3% | +0.7 |
|  | lossy | 2640 | 46 | 98.3% | -1.4 |
| json (uniform array) | off | - | - | 0.0% | +0.0 |
|  | default | 2553 | 1432 | 43.9% | +0.3 |
|  | lossy | 2553 | 1432 | 43.9% | +11.4 |
| code (whitespace) | off | - | - | 0.0% | +0.0 |
|  | default | - | - | 0.0% | +19.6 |
|  | lossy | 1580 | 79 | 95.0% | +791.6 |
| prose (human) | off | - | - | 0.0% | +0.0 |
|  | default | - | - | 0.0% | +0.5 |
|  | lossy | 1465 | 266 | 81.8% | +568.5 |
| repeated (dedup) | off | - | - | 0.0% | +0.0 |
|  | default | 2970 | 2100 | 29.3% | -11.2 |
|  | lossy | 2970 | 2100 | 29.3% | -1.2 |

## Cost saved per request (measured)

Input tokens removed × $0.3/1M. Deterministic — applies every request.

| corpus | arm | tokens saved | $/req saved | $ / 1M req |
|---|---|--:|--:|--:|
| logs (repetitive) | default | 2594 | $0.000778 | $778.20 |
|  | lossy | 2594 | $0.000778 | $778.20 |
| json (uniform array) | default | 1121 | $0.000336 | $336.30 |
|  | lossy | 1121 | $0.000336 | $336.30 |
| code (whitespace) | default | 0 | $0.000000 | $0.00 |
|  | lossy | 1501 | $0.000450 | $450.30 |
| prose (human) | default | 0 | $0.000000 | $0.00 |
|  | lossy | 1199 | $0.000360 | $359.70 |
| repeated (dedup) | default | 870 | $0.000261 | $261.00 |
|  | lossy | 870 | $0.000261 | $261.00 |

## Net latency: measured overhead vs modeled upstream saving

The fixture upstream does not scale latency with prompt size, so upstream saving is **modeled**: `upstream_ms_saved = tokens_saved / prefill_tps`. Net = `upstream_saved − gateway_overhead`. Positive = compression makes the request faster end-to-end.

Size sweep on **logs (deterministic)** (measured on the `default` arm):

| size | tokens saved | gateway +ms | net @ 500 tok/s | net @ 2000 tok/s | net @ 8000 tok/s |
|--:|--:|--:|--:|--:|--:|
| 20 | 395 | +0.8 | +789 | +197 | +49 |
| 60 | 1273 | +9.4 | +2537 | +627 | +150 |
| 120 | 2594 | +2.3 | +5186 | +1295 | +322 |
| 300 | 6588 | -9.5 | +13185 | +3303 | +833 |
| 600 | 13244 | -17.8 | +26506 | +6640 | +1673 |

Size sweep on **prose (neural lossy)** (measured on the `lossy` arm):

| size | tokens saved | gateway +ms | net @ 500 tok/s | net @ 2000 tok/s | net @ 8000 tok/s |
|--:|--:|--:|--:|--:|--:|
| 2 | 0 | +172.8 | -173 | -173 | -173 |
| 4 | 713 | +393.2 | +1033 | -37 | -304 |
| 8 | 1686 | +782.6 | +2589 | +60 | -572 |
| 16 | 3634 | +794.4 | +6474 | +1023 | -340 |
| 32 | 7530 | +814.4 | +14246 | +2951 | +127 |

> Reading it: where a `net` column turns positive, compression is a latency win at that upstream prefill rate; below it, the gateway overhead dominates and you're paying for the token/cost savings only. The deterministic sweep is ~free at any size; the neural sweep only wins once tokens saved / prefill-rate beats the sidecar overhead — i.e. big prompts on slower upstreams.
