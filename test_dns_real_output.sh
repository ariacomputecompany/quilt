#!/bin/bash

# Test actual DNS nslookup output
echo "Testing actual DNS nslookup output..."

# Kill any existing servers
pkill -f quilt || true
sleep 1

# Start server
echo "Starting server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!
sleep 3

# Create two containers
echo "Creating containers..."
CONTAINER_A=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 120 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
CONTAINER_B=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 120 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')

echo "Container A: $CONTAINER_A"
echo "Container B: $CONTAINER_B"

# Wait for containers
sleep 5

# Get IPs
IP_B=$(./target/debug/cli status "$CONTAINER_B" | grep "IP:" | awk '{print $2}')
echo "Container B IP: $IP_B"

# Test DNS query
echo -e "\n=== Testing DNS query for container B ==="
OUTPUT=$(./target/debug/cli exec "$CONTAINER_A" -c "nslookup $CONTAINER_B 10.42.0.1 2>&1 || echo 'Exit code:' \$?" --capture-output 2>&1)
echo "Full output:"
echo "$OUTPUT"

echo -e "\n=== Extract just the command output ==="
echo "$OUTPUT" | sed -n '/Standard Output:/,/^$/p'

echo -e "\n=== Testing if Address pattern matches ==="
if echo "$OUTPUT" | grep -q "Address.*$IP_B"; then
    echo "✓ Pattern 'Address.*$IP_B' matches!"
else
    echo "✗ Pattern 'Address.*$IP_B' does not match"
    echo "Looking for alternative patterns..."
    echo "$OUTPUT" | grep -i "address" || echo "No address found"
fi

# Cleanup
kill $SERVER_PID 2>/dev/null || true