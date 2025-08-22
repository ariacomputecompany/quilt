#!/bin/bash

# Kill any existing servers
pkill -f quilt || true
sleep 1

# Start server
echo "Starting server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!

# Wait for server
sleep 3

# Create container
echo "Creating container..."
CONTAINER_ID=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 60 2>&1 | grep "Container ID:" | awk '{print $NF}')
echo "Container ID: $CONTAINER_ID"

# Wait and check status
echo "Waiting for container to start..."
for i in {1..10}; do
    sleep 1
    STATUS=$(./target/debug/cli status "$CONTAINER_ID" 2>&1)
    echo "Attempt $i:"
    echo "$STATUS" | grep -E "(Status:|IP:)"
    if echo "$STATUS" | grep -q "Status: RUNNING"; then
        echo "Container is running!"
        break
    fi
done

# Check server logs
echo -e "\nLast 20 lines of server log:"
tail -20 server.log

# Cleanup
kill $SERVER_PID 2>/dev/null || true