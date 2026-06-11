# Build context must be the repo root (the build needs both obleth/ and schema/).
FROM rust:1.95-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY schema ./schema
COPY obleth ./obleth
WORKDIR /app/obleth
# Release builds inject the commit + timestamp, surfaced by /api/v1/version.
# Must be set before cargo build: option_env! reads them at compile time.
ARG GIT_SHA
ARG BUILD_TIMESTAMP
ENV OBLETH_BUILD_SHA=$GIT_SHA OBLETH_BUILD_TIMESTAMP=$BUILD_TIMESTAMP
RUN cargo build --release --bin obleth

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 obleth \
    && useradd --system --uid 10001 --gid obleth --no-create-home obleth
COPY --from=builder /app/obleth/target/release/obleth /usr/local/bin/obleth
USER 10001:10001
EXPOSE 8080 9180 9091
# Probe the proxy's /health endpoint (default OBLETH_PROXY_LISTEN :8080).
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1
ENTRYPOINT ["obleth"]
