# Stage 1: Build Orion binaries
FROM rust:slim-bookworm AS builder

WORKDIR /build

# Install build dependencies (OpenSSL for reqwest)
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Copy full source (simpler, avoids stub/lib mismatch issues)
COPY . .

# Build both binaries
RUN cargo build --release --bin orion-reth-demo --bin orion-reth-bench

# Stage 2: Minimal runtime image
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/orion-reth-demo /usr/local/bin/orion-reth-demo
COPY --from=builder /build/target/release/orion-reth-bench /usr/local/bin/orion-reth-bench

ENTRYPOINT ["orion-reth-demo"]
