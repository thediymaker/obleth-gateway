# Build context must be the repo root (mirrors obleth.Dockerfile's context so the
# two builds are interchangeable). The provisioner doesn't read schema/, but we
# keep the COPY to keep the build context identical and harmless.
FROM rust:1.95-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY schema ./schema
COPY obleth ./obleth
WORKDIR /app/obleth
# Release builds inject the commit + timestamp, reported to the gateway on each
# poll and surfaced under Settings -> Slurm. Must be set before cargo build:
# option_env! reads them at compile time.
ARG GIT_SHA
ARG BUILD_TIMESTAMP
ENV OBLETH_BUILD_SHA=$GIT_SHA OBLETH_BUILD_TIMESTAMP=$BUILD_TIMESTAMP
RUN cargo build --release --bin obleth-provisioner

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
# ca-certificates for outbound HTTPS to slurmrestd / the Management API.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 obleth \
    && useradd --system --uid 10001 --gid obleth --no-create-home obleth
COPY --from=builder /app/obleth/target/release/obleth-provisioner /usr/local/bin/obleth-provisioner
USER 10001:10001
# No EXPOSE / HEALTHCHECK: the provisioner makes only outbound calls and has no
# inbound endpoint.
ENTRYPOINT ["obleth-provisioner"]
