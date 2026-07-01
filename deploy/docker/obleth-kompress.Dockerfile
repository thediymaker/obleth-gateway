# Build context: repo root (obleth-gateway/). The builder stage downloads the
# ONNX model from Hugging Face (~600 MB) — image builds are slow and require
# network. The runtime image does NOT include torch; only onnxruntime is shipped.
FROM python:3.12-slim AS builder
WORKDIR /build
# Install build-time deps: optimum exporters, transformers, and torch (CPU).
# torch is heavy but stays only in the builder layer; it is NOT carried into
# the runtime image.
RUN pip install --no-cache-dir "optimum[exporters]" transformers torch
COPY kompress/export_model.py ./
ARG MODEL_ID=chopratejas/kompress-v2-base
RUN python export_model.py --model-id $MODEL_ID --out-dir /models

FROM python:3.12-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 kompress \
    && useradd --system --uid 10001 --gid kompress --no-create-home kompress
WORKDIR /app
COPY kompress/requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY --from=builder /models /models
COPY kompress/app.py kompress/model.py ./
ENV KOMPRESS_MODEL_DIR=/models
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1
USER 10001:10001
CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080"]
