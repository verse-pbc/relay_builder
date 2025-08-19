# This Dockerfile should be run from the parent directory (relay_repos)
# Build command: docker build -f relay_builder/Dockerfile -t relay-builder .

# Build stage
FROM rust:1.80-bookworm AS builder

WORKDIR /app

# Copy both projects
COPY websocket_builder ./websocket_builder
COPY relay_builder ./relay_builder

# Build the minimal relay example
WORKDIR /app/relay_builder
RUN cargo build --release --example 01_minimal_relay --features axum

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /app/relay_builder/target/release/examples/01_minimal_relay /usr/local/bin/relay

# Create data directory
RUN mkdir -p /data

# Environment variables
ENV RELAY_DATA_DIR=/data
ENV RUST_LOG=info

# Expose port
EXPOSE 8080

# Run the relay
CMD ["relay"]