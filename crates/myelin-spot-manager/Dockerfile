# Build stage
FROM rust:latest as builder

WORKDIR /app

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./

# Copy all workspace members
COPY spot_sentinel ./spot_sentinel

# Build the specific package
RUN cargo build --release --package spot_sentinel

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/spot_sentinel /usr/local/bin/spot_sentinel

CMD ["spot_sentinel"]
