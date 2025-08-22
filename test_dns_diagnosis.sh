#\!/bin/bash

# Quick DNS diagnosis
echo "=== DNS Diagnosis ==="

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

# Wait for container
sleep 5

# Check DNS server from host
echo -e "\n=== DNS server check from host ==="
ss -ulpn | grep :53

echo -e "\n=== Test DNS from host ==="
dig @10.42.0.1 test +short +timeout=2 || echo "DNS query from host failed"

echo -e "\n=== Container network info ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ip addr show" --capture-output
./target/debug/cli exec "$CONTAINER_ID" -c "ip route show" --capture-output

echo -e "\n=== Test DNS tools in container ==="
./target/debug/cli exec "$CONTAINER_ID" -c "which nslookup" --capture-output
./target/debug/cli exec "$CONTAINER_ID" -c "which nc" --capture-output

echo -e "\n=== Test connectivity to DNS server ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 1 10.42.0.1" --capture-output

echo -e "\n=== Test DNS port connectivity ==="
./target/debug/cli exec "$CONTAINER_ID" -c "nc -v -u -z -w 1 10.42.0.1 53" --capture-output

echo -e "\n=== Server logs ==="
tail -50 server.log | grep -E "(DNS|dns)"

# Cleanup
kill $SERVER_PID 2>/dev/null || true
