# Build context: benchmark-backend/
FROM rust:1.95-slim AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
RUN groupadd --system --gid 10001 bench \
    && useradd --system --uid 10001 --gid bench --no-create-home bench
COPY --from=builder /app/target/release/benchmark-backend /usr/local/bin/benchmark-backend
USER 10001:10001
EXPOSE 8081
ENTRYPOINT ["benchmark-backend"]
