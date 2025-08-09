#!/bin/bash

# Test subscription indexes with multiple connections
# This script creates 100 instances of 5 different connection types (500 total)

RELAY_URL="${1:-wss://communities2.nos.social}"
echo "Testing subscription indexes with relay: $RELAY_URL"
echo "Creating 100 instances of 5 different filter types (500 connections total)..."
echo ""

# Function to cleanup all nak processes on exit
cleanup() {
    echo ""
    echo "Cleaning up..."
    pkill -f 'nak req' 2>/dev/null
    sleep 1
    remaining=$(pgrep -f "nak req" | wc -l)
    if [ "$remaining" -gt 0 ]; then
        echo "Force killing remaining $remaining nak processes..."
        pkill -9 -f 'nak req' 2>/dev/null
    fi
    echo "Cleanup complete"
    exit 0
}

# Trap various signals to ensure cleanup happens
trap cleanup EXIT INT TERM

# Function to create connections in background
create_connections() {
    local filter_type=$1
    shift
    local cmd="$@"
    
    for i in {1..100}; do
        # Run in background and redirect output to /dev/null to avoid clutter
        $cmd >/dev/null 2>&1 &
        
        # Small delay to avoid overwhelming the relay
        if [ $((i % 10)) -eq 0 ]; then
            echo "  $filter_type: Started $i/100 connections..."
            sleep 0.1
        fi
    done
}

echo "Starting connections..."
echo ""

# 1. Author Index connections
echo "1. Creating 100 Author Index connections..."
create_connections "Author" nak req --stream -a 3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d -l 10 "$RELAY_URL"

# 2. Kind Index connections  
echo "2. Creating 100 Kind Index connections..."
create_connections "Kind" nak req --stream -k 1 -k 30023 -l 10 "$RELAY_URL"

# 3. Tag Index connections
echo "3. Creating 100 Tag Index connections..."
create_connections "Tag" nak req --stream -t t=nostr -t t=bitcoin -l 10 "$RELAY_URL"

# 4. Event ID Index connections
echo "4. Creating 100 Event ID Index connections..."
create_connections "EventID" nak req --stream -i 5c83da77af1dec6d7289834998ad7aafbd9e2191396d75ec3cc27f5a77226f36 "$RELAY_URL"

# 5. Combined filters connections
echo "5. Creating 100 Combined Filter connections..."
create_connections "Combined" nak req --stream -k 1 -a 3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d -t t=nostr -l 10 "$RELAY_URL"

echo ""
echo "All 500 connections started!"
echo "Active nak processes: $(pgrep -f "nak req" | wc -l)"
echo ""
echo "To monitor relay diagnostics, check the relay logs for:"
echo "  === Relay Health Check ==="
echo ""
echo "To stop all connections, run:"
echo "  pkill -f 'nak req'"
echo ""
echo "Press Ctrl+C or Enter to stop all connections and exit"

# Wait for user input
read -r

# Cleanup will be called automatically by the trap