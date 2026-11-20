# Build context: benchmark-backend/
#
# Dependencies compile in their own layer (cargo-chef), as in obleth.Dockerfile:
# a source-only change reuses the cooked layer.
FROM rust:1.95-slim AS chef
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.78

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.source="https://github.com/thediymaker/obleth-gateway"
RUN groupadd --system --gid 10001 bench \
    && useradd --system --uid 10001 --gid bench --no-create-home bench
COPY --from=builder /app/target/release/benchmark-backend /usr/local/bin/benchmark-backend
USER 10001:10001
EXPOSE 8081
ENTRYPOINT ["benchmark-backend"]
