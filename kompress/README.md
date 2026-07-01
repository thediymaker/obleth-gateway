# kompress — sentence-scoring sidecar

Optional, horizontally-scalable sidecar that assigns per-sentence importance
scores for extractive prose compression.  The gateway POSTs sentence batches
and receives back floating-point scores; all business logic (thresholding,
reconstruction) lives in the gateway, not here.

## Model

`chopratejas/kompress-v2-base` — Apache-2.0 ModernBERT extractive
sentence-importance classifier, exported to ONNX at image build time.
**No PyTorch or Transformers at runtime.**

## HTTP contract

### `POST /score`

Request:
```json
{
  "segments": [
    {"sentences": ["First sentence.", "Second sentence."]},
    {"sentences": ["Only one here."]}
  ]
}
```

Response (one `scores` array per segment, same length, index-aligned):
```json
{
  "results": [
    {"scores": [0.91, 0.12]},
    {"scores": [0.74]}
  ]
}
```

- Empty `segments: []` → `{"results": []}`.
- A segment with `sentences: []` → that result has `scores: []`.
- Scores are floats in `[0, 1]`.

### `GET /health`

```json
{"status": "ok", "model": "kompress-v2-base", "revision": "<git-sha>"}
```

`revision` is the Hub commit SHA written to `revision.txt` by `export_model.py`,
or `"unknown"` if the file is absent.

## Environment variables

| Variable              | Default    | Description                                   |
|-----------------------|------------|-----------------------------------------------|
| `KOMPRESS_MODEL_DIR`  | `/models`  | Directory containing `model.onnx`, `tokenizer.json`, `revision.txt` |
| `PORT`                | `8080`     | Listening port (set in the Dockerfile CMD)    |

## Baking the model into an image

The model is downloaded and converted to ONNX once at build time:

```dockerfile
# Build deps (not needed at runtime)
RUN pip install transformers "optimum[exporters]" torch
RUN python export_model.py --model-id chopratejas/kompress-v2-base --out-dir /models

# Runtime deps only
RUN pip install -r requirements.txt
```

## Running locally

```sh
pip install -r requirements.txt
KOMPRESS_MODEL_DIR=/path/to/exported/models uvicorn app:app --port 8080
```

## Running tests (no model needed)

```sh
pip install fastapi "uvicorn[standard]" pydantic pytest httpx
pytest -q tests/
```
