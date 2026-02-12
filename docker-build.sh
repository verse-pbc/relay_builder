#!/bin/bash

# Build the Docker image from the current directory

echo "Building relay-builder Docker image..."

docker build -t relay-builder .

if [ $? -eq 0 ]; then
    echo "Docker image built successfully: relay-builder"
    echo "Run with: docker run -p 8080:8080 -v ./data:/data relay-builder"
else
    echo "Docker build failed"
    exit 1
fi
