# Build context: repo root (obleth-gateway/). This stage provisions the model.
# The runtime image ships only onnxruntime — no torch/transformers.
#
# The neural model is swappable via build args:
#   COMPRESSOR_SOURCE=onnx    (default) download a pre-built ONNX (kompress-v2-base
#                           ships one). Only needs huggingface_hub. Fast-ish.
#   COMPRESSOR_SOURCE=export  export a token-classification model (e.g. LLMLingua-2)
#                           to ONNX with torch. Heavier build.
# e.g. LLMLingua-2:
#   --build-arg COMPRESSOR_SOURCE=export \
#   --build-arg MODEL_ID=microsoft/llmlingua-2-bert-base-multilingual-cased-meetingbank
FROM python:3.12-slim AS fetch
WORKDIR /build
ARG COMPRESSOR_SOURCE=onnx
ARG MODEL_ID=chopratejas/kompress-v2-base
# For source=onnx: swap to onnx/kompress-int8-wo.onnx for a smaller/faster image.
ARG ONNX_FILE=onnx/kompress-fp32.onnx
# Install only the deps the chosen source needs (export drags in a CPU torch).
RUN if [ "$COMPRESSOR_SOURCE" = "export" ]; then \
        pip install --no-cache-dir torch --index-url https://download.pytorch.org/whl/cpu && \
        pip install --no-cache-dir "transformers>=4.48,<5" onnx huggingface_hub; \
    else \
        pip install --no-cache-dir huggingface_hub; \
    fi
# torch's ONNX exporter imports onnxscript at runtime (export source only).
RUN if [ "$COMPRESSOR_SOURCE" = "export" ]; then pip install --no-cache-dir onnxscript; fi
COPY compressor/fetch_model.py ./
RUN python fetch_model.py --source "$COMPRESSOR_SOURCE" --model-id "$MODEL_ID" \
        --onnx-file "$ONNX_FILE" --out-dir /models

FROM python:3.12-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 compressor \
    && useradd --system --uid 10001 --gid compressor --no-create-home compressor
WORKDIR /app
COPY compressor/requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY --from=fetch /models /models
COPY compressor/app.py compressor/model.py ./
# Report the baked model in /health (surfaced in the dashboard sidecar status).
ARG MODEL_ID=chopratejas/kompress-v2-base
# COMPRESSOR_NUM_THREADS caps onnxruntime's intra-op thread pool. Without it the
# sidecar grabs EVERY host core per inference — it is genuinely CPU-hungry. A
# single inference saturates near ~4 cores (8/16/24 barely beat it), so keep this
# SMALL and scale with more replicas, not fatter pods. 8 is a safe near-peak
# default; deploys pin it to the container's CPU limit (compose `cpus:`, Helm
# resources + compressor.numThreads) — matched so each replica cleanly owns N cores.
ENV COMPRESSOR_MODEL_DIR=/models \
    COMPRESSOR_MODEL_NAME=${MODEL_ID} \
    COMPRESSOR_NUM_THREADS=8
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1
USER 10001:10001
CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080"]
