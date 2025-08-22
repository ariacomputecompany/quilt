#!/bin/bash

# Test DNS nslookup functionality
echo "Testing DNS nslookup functionality..."

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

# Test nslookup command
echo -e "\n=== Testing nslookup command ==="
echo "Command: nslookup google.com 8.8.8.8"
./target/debug/cli exec "$CONTAINER_ID" -c "nslookup google.com 8.8.8.8" --capture-output

echo -e "\n=== Testing nslookup to our DNS server ==="
echo "Command: nslookup test 10.42.0.1"
./target/debug/cli exec "$CONTAINER_ID" -c "nslookup test 10.42.0.1" --capture-output

echo -e "\n=== Check if nslookup exists ==="
./target/debug/cli exec "$CONTAINER_ID" -c "which nslookup" --capture-output

echo -e "\n=== List bin directory ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin/nslookup" --capture-output

echo -e "\n=== Test busybox nslookup directly ==="
./target/debug/cli exec "$CONTAINER_ID" -c "/bin/busybox nslookup --help 2>&1 || echo 'nslookup help failed'" --capture-output

# Cleanup
kill $SERVER_PID 2>/dev/null || true