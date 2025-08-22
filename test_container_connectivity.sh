#!/bin/bash

# Test container-to-container connectivity
echo "Testing container-to-container connectivity..."

# Kill any existing servers
pkill -f quilt || true
sleep 1

# Start server
echo "Starting server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!
sleep 3

# Create two containers
echo "Creating container A..."
CONTAINER_A=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 300 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
echo "Container A ID: $CONTAINER_A"

echo "Creating container B..."
CONTAINER_B=$(./target/debug/cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- sleep 300 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
echo "Container B ID: $CONTAINER_B"

# Wait for containers to start
echo "Waiting for containers to start..."
sleep 5

# Get IPs
echo -e "\n=== Getting container IPs ==="
IP_A=$(./target/debug/cli status "$CONTAINER_A" | grep "IP:" | awk '{print $2}')
IP_B=$(./target/debug/cli status "$CONTAINER_B" | grep "IP:" | awk '{print $2}')
echo "Container A IP: $IP_A"
echo "Container B IP: $IP_B"

# Test network interfaces
echo -e "\n=== Container A network interfaces ==="
./target/debug/cli exec "$CONTAINER_A" -c "ip addr show" --capture-output

echo -e "\n=== Container B network interfaces ==="
./target/debug/cli exec "$CONTAINER_B" -c "ip addr show" --capture-output

# Test ping from A to B
echo -e "\n=== Testing ping from A to B ==="
echo "Command: ping -c 3 -W 2 $IP_B"
./target/debug/cli exec "$CONTAINER_A" -c "ping -c 3 -W 2 $IP_B" --capture-output

# Test ping from B to A
echo -e "\n=== Testing ping from B to A ==="
echo "Command: ping -c 3 -W 2 $IP_A"
./target/debug/cli exec "$CONTAINER_B" -c "ping -c 3 -W 2 $IP_A" --capture-output

# Check iptables rules
echo -e "\n=== Host iptables rules ==="
sudo iptables -L -n -v | grep -E "quilt|10.42" || echo "No quilt-related rules found"

# Check bridge status
echo -e "\n=== Bridge status ==="
ip link show quilt0
ip addr show quilt0
bridge fdb show br quilt0 2>/dev/null || echo "bridge command not available"

# Cleanup
kill $SERVER_PID 2>/dev/null || true