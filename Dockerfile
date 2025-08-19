# Build stage
FROM rust:1.75-bookworm as builder

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src
COPY examples ./examples

# Build the minimal relay example
RUN cargo build --release --example 01_minimal_relay --features axum

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /app/target/release/examples/01_minimal_relay /usr/local/bin/relay

# Create data directory
RUN mkdir -p /data

# Environment variables
ENV RELAY_DATA_DIR=/data
ENV RUST_LOG=info

# Expose port
EXPOSE 8080

# Run the relay
CMD ["relay"]