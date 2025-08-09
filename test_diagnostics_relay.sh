#!/bin/bash
echo "Testing diagnostics with minimal relay..."
echo "Starting relay with diagnostics enabled (1-minute interval)..."
echo ""

# Start the relay in background
RUST_LOG=relay_builder=info cargo run --example 01_minimal_relay --features axum &
RELAY_PID=$!

# Wait for relay to start
sleep 3

echo ""
echo "Connecting a client and creating subscriptions..."
# Connect and create subscription
echo '["REQ","sub1",{"kinds":[1],"limit":10}]' | websocat ws://localhost:8080 -1 &

# Send an event
sleep 1
echo "Sending a test event..."
echo '["EVENT",{"id":"test1","pubkey":"test","created_at":1234567890,"kind":1,"tags":[],"content":"test","sig":"test"}]' | websocat ws://localhost:8080 -1 &

# Wait to see diagnostics with connections
echo ""
echo "Waiting 65 seconds for next diagnostic cycle..."
sleep 65

# Clean up
kill $RELAY_PID 2>/dev/null
echo "Test complete!"