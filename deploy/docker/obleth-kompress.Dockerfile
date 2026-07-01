# Build context: repo root (obleth-gateway/). The model repo already ships a
# pre-built ONNX artefact (onnx/kompress-fp32.onnx), so this stage just downloads
# it (~600 MB) — no torch/transformers export. The build is slow only because of
# the download size and needs network. The runtime image ships only onnxruntime.
FROM python:3.12-slim AS fetch
WORKDIR /build
# Sole build-time dep (never carried into the runtime image): the Hub client.
RUN pip install --no-cache-dir huggingface_hub
COPY kompress/fetch_model.py ./
ARG MODEL_ID=chopratejas/kompress-v2-base
# Swap to onnx/kompress-int8-wo.onnx for a smaller, faster, slightly-lossier image.
ARG ONNX_FILE=onnx/kompress-fp32.onnx
RUN python fetch_model.py --model-id $MODEL_ID --out-dir /models --onnx-file $ONNX_FILE

FROM python:3.12-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 kompress \
    && useradd --system --uid 10001 --gid kompress --no-create-home kompress
WORKDIR /app
COPY kompress/requirements.txt .
RUN pip install --no-cache-dir -r requirements.txt
COPY --from=fetch /models /models
COPY kompress/app.py kompress/model.py ./
ENV KOMPRESS_MODEL_DIR=/models
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1
USER 10001:10001
CMD ["uvicorn", "app:app", "--host", "0.0.0.0", "--port", "8080"]
