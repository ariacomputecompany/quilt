#\!/bin/bash

# Test bridge connectivity from container
echo "Testing bridge connectivity..."

# Kill any existing servers
pkill -f quilt || true
sleep 1

# Start server
echo "Starting server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$\!
sleep 3

# Create a container
echo "Creating container..."
CONTAINER_ID=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 120 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
echo "Container ID: $CONTAINER_ID"

# Wait for container to start
echo "Waiting for container to start..."
sleep 5

# Test basic network connectivity
echo -e "\n=== Test IP address assignment ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ip addr show" --capture-output

echo -e "\n=== Test route table ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ip route show" --capture-output

echo -e "\n=== Test ping to bridge IP ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 3 10.42.0.1" --capture-output

echo -e "\n=== Test DNS query with timeout ==="
./target/debug/cli exec "$CONTAINER_ID" -c "timeout 2 nslookup test 10.42.0.1" --capture-output

echo -e "\n=== Test connectivity from host to container ==="
IP=$(./target/debug/cli status "$CONTAINER_ID" | grep "IP:" | awk '{print $2}')
echo "Container IP: $IP"
if [ \! -z "$IP" ] && [ "$IP" \!= "N/A" ]; then
    echo "Pinging container from host..."
    ping -c 1 -W 1 "$IP"
fi

# Cleanup
kill $SERVER_PID 2>/dev/null || true
