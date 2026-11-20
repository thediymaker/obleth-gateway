# Build context must be the repo root (the build needs both obleth/ and schema/).
#
# Dependencies compile in their own layer (cargo-chef): the recipe holds only
# the manifests and Cargo.lock, so a source-only change reuses the cooked
# layer and recompiles just the workspace crates. Keep the stages above the
# ARGs below free of anything that changes per commit.
FROM rust:1.95-slim AS chef
WORKDIR /app/obleth
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.78

FROM chef AS planner
COPY obleth ./
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/obleth/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json --bin obleth
COPY schema /app/schema
COPY obleth ./
# Release builds inject the commit + timestamp, surfaced by /api/v1/version.
# Must be set before cargo build: option_env! reads them at compile time.
# BUILD_TIMESTAMP is the commit time, so one commit always reports one time.
ARG GIT_SHA
ARG BUILD_TIMESTAMP
ENV OBLETH_BUILD_SHA=$GIT_SHA OBLETH_BUILD_TIMESTAMP=$BUILD_TIMESTAMP
RUN cargo build --release --locked --bin obleth

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 obleth \
    && useradd --system --uid 10001 --gid obleth --no-create-home obleth \
    && mkdir -p /var/lib/obleth && chown 10001:10001 /var/lib/obleth
COPY --from=builder /app/obleth/target/release/obleth /usr/local/bin/obleth
# Writable working directory for the runtime user, so the default relative
# OBLETH_WAL_PATH (./obleth-telemetry.wal) resolves to a path it can write.
WORKDIR /var/lib/obleth
USER 10001:10001
EXPOSE 8080 9180 9091
# Probe the proxy's /health endpoint (default OBLETH_PROXY_LISTEN :8080).
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/health || exit 1
ENTRYPOINT ["obleth"]
