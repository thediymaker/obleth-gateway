# compressor: sentence-scoring sidecar

An optional sidecar that gives each sentence of a prompt an importance score,
for extractive prose compression. The gateway sends sentences and gets one
score per sentence back. Thresholds and reconstruction stay in the gateway.

## Model

`chopratejas/kompress-v2-base`: an Apache-2.0 ModernBERT token compressor. It
gives each token a keep probability; the sidecar scores a sentence as the mean
over its content tokens. The image ships the repo's pre-built ONNX file and runs
it with onnxruntime and tokenizers. There is no PyTorch or Transformers at
runtime.

The repo ships two files:

| File | Speed (4-core pod, mixed prose) | Peak memory | Choice |
|------|------|------|------|
| `onnx/kompress-fp32.onnx` | 34.5 sentences/s | 934 MiB | default |
| `onnx/kompress-int8-wo.onnx` | 24.7 sentences/s | 474 MiB | when memory is short |

int8 ranks sentences almost as fp32 does (measured 2026-11-20 on 600 sentences
of the docs in 30 segments: the half kept per segment overlaps fp32's by 0.977
on average, 0.90 at worst; Spearman 0.995 on average; scores differ by at most
0.04). It is 30% slower on these CPUs, and the gateway sizes each call by speed
(see below), so fp32 stays the default. Build with
`--build-arg ONNX_FILE=onnx/kompress-int8-wo.onnx` to halve the memory.

## How a request is scored

All of a request's sentences, across its segments, are tokenized once (512
tokens at most), sorted by length and run in chunks of `COMPRESSOR_MAX_BATCH`.
Each chunk is padded to its longest sentence, so short sentences are not padded
out to a long one. Chunks of 8 were fastest on mixed prose (33 sentences/s,
against 25 at 32 and 17 before sorting).

A pod scores `COMPRESSOR_CONCURRENCY` requests at a time; each already uses
every thread. Up to `COMPRESSOR_MAX_WAITING` more wait for a turn, and past that
the pod answers `503` at once. The gateway sends its timeout in
`x-obleth-timeout-ms`; a request still waiting or scoring when it passes is
dropped with `504`, so the pod never works on a request the gateway has given
up on. The gateway records `503` and `504` as `busy` and compacts that request
by its heuristic.

The gateway also sends no more sentences than a pod can score within its
timeout (about 30 a second; 24 at the default 800 ms).

## HTTP contract

### `POST /score`

Request (header `x-obleth-timeout-ms` optional):
```json
{
  "segments": [
    {"sentences": ["First sentence.", "Second sentence."]},
    {"sentences": ["Only one here."]}
  ]
}
```

Response: one `scores` array per segment, same length, index-aligned:
```json
{
  "results": [
    {"scores": [0.91, 0.12]},
    {"scores": [0.74]}
  ]
}
```

- Empty `segments: []` gives `{"results": []}`.
- A segment with `sentences: []` gives `scores: []`.
- Scores are floats in `[0, 1]`.
- `503`: the queue is full. `504`: the timeout passed.

### `GET /health`

```json
{"status": "ok", "model": "chopratejas/kompress-v2-base", "revision": "<git-sha>"}
```

`revision` is the Hub commit written to `revision.txt` at build time, or
`"unknown"`.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `COMPRESSOR_MODEL_DIR` | `/models` | Holds `model.onnx`, `tokenizer.json`, `revision.txt` |
| `COMPRESSOR_MODEL_NAME` | the build's `MODEL_ID` | Name reported by `/health` |
| `COMPRESSOR_NUM_THREADS` | `4` in the image | onnxruntime intra-op threads; match the CPU limit. Unset: every host core |
| `COMPRESSOR_MAX_BATCH` | `8` | Sentences per ONNX call |
| `COMPRESSOR_CONCURRENCY` | `1` | Requests scored at once |
| `COMPRESSOR_MAX_WAITING` | `8` | Requests that may wait before `503` |

Helm sets the last two from `compressor.concurrency` and
`compressor.maxWaiting`, and the threads from `compressor.numThreads`.

## Building the image

`deploy/docker/obleth-compressor.Dockerfile`, from the repo root. A first stage
runs `fetch_model.py` to put the model in `/models`; the runtime stage copies
only that directory.

| Build arg | Default | Description |
|-----------|---------|-------------|
| `COMPRESSOR_SOURCE` | `onnx` | `onnx` downloads a pre-built file; `export` exports a token-classification model with torch (for example LLMLingua-2) |
| `MODEL_ID` | `chopratejas/kompress-v2-base` | Hub repo |
| `ONNX_FILE` | `onnx/kompress-fp32.onnx` | File to download when the source is `onnx` |

```sh
docker build -f deploy/docker/obleth-compressor.Dockerfile -t compressor .
```

## Running locally

```sh
pip install -r compressor/requirements.txt huggingface_hub
python compressor/fetch_model.py --out-dir /tmp/models
cd compressor && COMPRESSOR_MODEL_DIR=/tmp/models uvicorn app:app --port 8080
```

## Tests (no model needed)

```sh
pip install fastapi "uvicorn[standard]" pydantic pytest httpx
pytest -q compressor/tests/
```

## Load testing

See [bench/README.md](bench/README.md).
