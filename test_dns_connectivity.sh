#\!/bin/bash

# Test DNS connectivity issue
echo "Testing DNS connectivity issue..."

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

# Test connectivity to DNS server
echo -e "\n=== Test ping to DNS server (10.42.0.1) ==="
./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 1 10.42.0.1" --capture-output

echo -e "\n=== Test if we can reach DNS port ==="
./target/debug/cli exec "$CONTAINER_ID" -c "timeout 2 nc -v 10.42.0.1 53" --capture-output

echo -e "\n=== Test nslookup with timeout and verbose ==="
./target/debug/cli exec "$CONTAINER_ID" -c "timeout 5 nslookup -debug test 10.42.0.1" --capture-output

echo -e "\n=== Check iptables rules for DNS ==="
sudo iptables -L INPUT -n -v | grep 53
sudo iptables -L FORWARD -n -v | head -10

# Cleanup
kill $SERVER_PID 2>/dev/null || true
