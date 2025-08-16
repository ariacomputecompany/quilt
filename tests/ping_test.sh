#!/bin/bash
# Ping connectivity test

echo "🏓 TESTING INTER-CONTAINER PING CONNECTIVITY"
cargo build > /dev/null 2>&1
./target/debug/quilt > ping_server.log 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"
sleep 3

echo ""
echo "📊 Creating 2 test containers..."

# Create container A
echo -n "Container A: "
CONTAINER_A=$(timeout 30 ./target/debug/cli create --image-path ./nixos-minimal.tar.gz -- sleep 300 | grep "Container ID:" | awk '{print $NF}')
if [ -n "$CONTAINER_A" ]; then
    echo "✅ $CONTAINER_A"
else
    echo "❌ Failed to create"
    exit 1
fi

# Create container B  
echo -n "Container B: "
CONTAINER_B=$(timeout 30 ./target/debug/cli create --image-path ./nixos-minimal.tar.gz -- sleep 300 | grep "Container ID:" | awk '{print $NF}')
if [ -n "$CONTAINER_B" ]; then
    echo "✅ $CONTAINER_B"
else
    echo "❌ Failed to create"
    exit 1
fi

echo ""
echo "🛡️ Waiting 15 seconds for network stabilization..."
sleep 15

echo ""
echo "🏓 Testing ping A → B:"
timeout 20 ./target/debug/cli icc ping "$CONTAINER_A" "$CONTAINER_B" --count 2 --timeout 15

echo ""
echo "🏓 Testing ping B → A:"
timeout 20 ./target/debug/cli icc ping "$CONTAINER_B" "$CONTAINER_A" --count 2 --timeout 15

echo ""
echo "📋 Container status:"
./target/debug/cli status

echo ""
echo "🔍 Bridge status:"
ip link show quilt0

# Cleanup
kill $SERVER_PID 2>/dev/null
sleep 1
pkill -f "./target/debug/quilt" 2>/dev/null
echo "�� Cleanup completed" 