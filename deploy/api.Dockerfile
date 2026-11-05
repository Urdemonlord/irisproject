# syntax=docker/dockerfile:1
# IRIS API — multi-stage Rust build. Build context is the repo root
# (see deploy/compose/docker-compose.yaml).
FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY apps apps
COPY crates crates
RUN cargo build --release -p iris-api

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /data && chown nobody:nogroup /data
COPY --from=builder /src/target/release/iris-api /usr/local/bin/iris-api
USER nobody
WORKDIR /data
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/iris-api"]
