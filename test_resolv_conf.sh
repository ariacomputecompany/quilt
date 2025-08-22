#!/bin/bash

# Test resolv.conf in container
echo "Testing resolv.conf in container..."

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

# Check resolv.conf
echo -e "\n=== Checking /etc/resolv.conf in container ==="
./target/debug/cli exec "$CONTAINER_ID" -c "cat /etc/resolv.conf" --capture-output

echo -e "\n=== Check if file exists ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /etc/resolv.conf" --capture-output

echo -e "\n=== Test network connectivity to DNS server ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 1 10.42.0.1" --capture-output

echo -e "\n=== Check routes ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ip route show" --capture-output

# Cleanup
kill $SERVER_PID 2>/dev/null || true