#!/bin/bash

# Test busybox ping output format
echo "Testing busybox ping output format..."

# Kill any existing servers
pkill -f quilt || true
sleep 1

# Start server
echo "Starting server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!
sleep 3

# Create a container
echo "Creating container..."
CONTAINER_ID=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 120 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
echo "Container ID: $CONTAINER_ID"

# Wait for container to start
echo "Waiting for container to start..."
sleep 5

# Test ping to localhost
echo -e "\n=== Testing ping output ==="
echo "Running: ping -c 1 127.0.0.1"
./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 1 127.0.0.1" --capture-output

echo -e "\n=== Testing ping with grep ==="
OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 1 127.0.0.1" --capture-output 2>&1)
echo "Full output:"
echo "$OUTPUT"
echo ""
echo "Testing grep pattern '1 packets transmitted, 1 received':"
if echo "$OUTPUT" | grep -q '1 packets transmitted, 1 received'; then
    echo "✓ Pattern matched!"
else
    echo "✗ Pattern did not match"
    echo "Looking for alternative patterns..."
    echo "$OUTPUT" | grep -i "transmitted\|received" || echo "No match found"
fi

# Cleanup
kill $SERVER_PID 2>/dev/null || true