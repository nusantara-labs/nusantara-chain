# Stage 1: Install cargo-chef
FROM rust:1.93-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /build

# Stage 2: Prepare recipe (dependency graph only)
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Cook dependencies (cached unless Cargo.toml/lock change)
FROM chef AS builder

RUN apt-get update && apt-get install -y \
    libclang-dev cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Stage 4: Build application (only source code changes trigger this)
COPY . .
RUN cargo build --release --bin nusantara-validator --bin nusantara

# Stage 5: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/nusantara-validator /usr/local/bin/
COPY --from=builder /build/target/release/nusantara /usr/local/bin/
COPY genesis.toml /etc/nusantara/genesis.toml
COPY scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Gossip, TPU, TPU-forward, Turbine, Repair
EXPOSE 8000-8004
# RPC
EXPOSE 8899
# Metrics
EXPOSE 9090

ENV RUST_LOG=info

ENTRYPOINT ["docker-entrypoint.sh"]
CMD ["--ledger-path", "/data/ledger", "--genesis-config", "/etc/nusantara/genesis.toml"]
