#!/bin/bash

# Build the Docker image with the correct context
# This script should be run from the relay_builder directory

echo "Building relay-builder Docker image..."
echo "This requires websocket_builder to be available in the parent directory"

# Go to parent directory to include both projects in build context
cd ..

# Build using the Dockerfile from relay_builder
docker build -f relay_builder/Dockerfile -t relay-builder .

if [ $? -eq 0 ]; then
    echo "✅ Docker image built successfully: relay-builder"
    echo "Run with: docker run -p 8080:8080 -v ./data:/data relay-builder"
else
    echo "❌ Docker build failed"
    exit 1
fi
