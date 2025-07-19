#!/bin/bash

# Build script for Docker-based minimal relay

echo "Building minimal relay for Docker..."

# Build the binary on the host first (faster than building in Docker)
RUSTFLAGS="-C target-cpu=native -C opt-level=3 -C codegen-units=1" \
    cargo build --release --example 01_minimal_relay --features axum

if [ $? -ne 0 ]; then
    echo "Failed to build minimal relay"
    exit 1
fi

# Build the Docker image using the pre-built binary
docker build -f Dockerfile.minimal_relay_prebuilt -t minimal-relay:latest .

if [ $? -ne 0 ]; then
    echo "Failed to build Docker image"
    exit 1
fi

echo "Docker image 'minimal-relay:latest' built successfully"
echo ""
echo "To run the container:"
echo "  docker run -d --name minimal-relay -p 8080:8080 -v /tmp/minimal-relay-data:/app/relay-data minimal-relay:latest"
echo ""
echo "To run with custom port:"
echo "  docker run -d --name minimal-relay -p 8081:8080 -v /tmp/minimal-relay-data:/app/relay-data minimal-relay:latest"