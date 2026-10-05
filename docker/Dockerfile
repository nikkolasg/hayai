# syntax=docker/dockerfile:1
#
# hayaid image. Build from the repository root:
#
#   docker build -f docker/Dockerfile -t hayaid:local .
#   docker build -f docker/Dockerfile --build-arg CRYPTO_BACKEND=zakura -t hayaid:zakura .
#
# Stages:
#   chef     toolchain, clang for librocksdb-sys, cargo-chef
#   planner  dependency recipe of the workspace
#   builder  dependencies (a cached layer), then hayaid
#   runtime  debian-slim, user hayai (10001), hayaid
#
# The runtime reads its configuration from /etc/hayai/hayaid.toml (mount it read-only)
# and keeps its state in /var/lib/hayai. docker/config/ has one file per network.

# One toolchain for the image and CI: the lowest that builds the whole workspace. The node
# crates need 1.88 with the upstream backend and 1.91 with the zakura backend (workspace
# `rust-version`); hayai-bench needs 1.97 (zakura-chain 9.0.0, its `rust-version`).
ARG RUST_VERSION=1.97.1
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-${DEBIAN_RELEASE} AS chef
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# upstream (the upstream Zcash crates) or zakura (the zakura-* forks).
ARG CRYPTO_BACKEND=upstream
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json \
        -p hayaid --no-default-features --features "${CRYPTO_BACKEND}"
COPY . .
# The release profile keeps debug info (debug = 1, about 480 MB); the image keeps the
# symbol table for profilers and drops the DWARF sections.
RUN cargo build --release --locked -p hayaid --no-default-features --features "${CRYPTO_BACKEND}" \
    && strip --strip-debug -o /usr/local/bin/hayaid target/release/hayaid

FROM debian:${DEBIAN_RELEASE}-slim AS runtime
ARG CRYPTO_BACKEND=upstream
LABEL org.opencontainers.image.title="hayaid" \
      org.opencontainers.image.description="hayai node (${CRYPTO_BACKEND} crypto backend)"
# curl serves the health check only.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 hayai \
    && useradd --uid 10001 --gid 10001 --home-dir /var/lib/hayai --no-create-home \
        --shell /usr/sbin/nologin hayai \
    && install -d -o hayai -g hayai -m 0750 /var/lib/hayai \
    && install -d -m 0755 /etc/hayai
COPY --from=builder /usr/local/bin/hayaid /usr/local/bin/hayaid
USER hayai
WORKDIR /var/lib/hayai
VOLUME ["/var/lib/hayai"]
# Defaults of `hayaid config`: Testnet P2P 18333, Regtest P2P 18344, Regtest RPC 18345,
# metrics 19101. The configuration file decides which ones listen.
EXPOSE 18333 18344 18345 19101
# The metrics endpoint answers while the node runs. The address is the one of the files
# in docker/config/.
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD ["curl", "-fsS", "-o", "/dev/null", "http://127.0.0.1:19101/metrics"]
ENTRYPOINT ["hayaid"]
CMD ["start", "-c", "/etc/hayai/hayaid.toml"]
