# Build context must be the repo root (the build needs both obleth/ and schema/).
FROM rust:1.95-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY schema ./schema
COPY obleth ./obleth
WORKDIR /app/obleth
RUN cargo build --release --bin obleth

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 obleth \
    && useradd --system --uid 10001 --gid obleth --no-create-home obleth
COPY --from=builder /app/obleth/target/release/obleth /usr/local/bin/obleth
USER 10001:10001
EXPOSE 8080 9090 9091
ENTRYPOINT ["obleth"]
