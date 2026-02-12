# Build stage
FROM rust:1.86-bookworm AS builder

WORKDIR /app

# Copy source
COPY . .

# Build the relay binary
RUN cargo build --release --bin relay

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/relay /usr/local/bin/relay

RUN mkdir -p /data

ENV RELAY_DATA_DIR=/data
ENV RELAY_PORT=8080
ENV RELAY_BIND=0.0.0.0
ENV RUST_LOG=info

EXPOSE 8080

CMD ["relay"]
