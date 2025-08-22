#!/bin/bash

# DNS Debug Test
set -e

echo "Starting debug test for DNS and exec..."

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

# Check status
echo -e "\n=== Container Status ==="
./target/debug/cli status "$CONTAINER_ID"

# Get IP
IP=$(./target/debug/cli status "$CONTAINER_ID" | grep "IP:" | awk '{print $2}')
echo -e "\nContainer IP: $IP"

# Test basic exec
echo -e "\n=== Testing basic exec ==="
./target/debug/cli exec "$CONTAINER_ID" -c "echo 'Hello from container'"

# Test network interfaces
echo -e "\n=== Network interfaces in container ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ip addr show"

# Test DNS configuration
echo -e "\n=== DNS configuration ==="
./target/debug/cli exec "$CONTAINER_ID" -c "cat /etc/resolv.conf"

# Test if nslookup exists
echo -e "\n=== Testing nslookup availability ==="
./target/debug/cli exec "$CONTAINER_ID" -c "which nslookup || echo 'nslookup not found'"

# Test if ping exists
echo -e "\n=== Testing ping availability ==="
./target/debug/cli exec "$CONTAINER_ID" -c "which ping || echo 'ping not found'"

# Check server logs
echo -e "\n=== Last 20 lines of server log ==="
tail -20 server.log

# Cleanup
kill $SERVER_PID 2>/dev/null || true