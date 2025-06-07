#!/bin/bash

echo "🔍 Network Setup Debugging"
echo "=========================="

# Start server
echo "🚀 Starting server..."
pkill -f "target/debug/quilt" || true
sleep 2

cargo run --bin quilt &
SERVER_PID=$!
sleep 3

echo "📊 Initial bridge state:"
ip link show quilt0

echo ""
echo "🧪 Creating container while monitoring bridge..."

# Monitor bridge in background
{
    while true; do
        timestamp=$(date '+%H:%M:%S.%3N')
        if ip link show quilt0 >/dev/null 2>&1; then
            echo "[$timestamp] ✅ Bridge exists"
        else
            echo "[$timestamp] ❌ Bridge missing!"
            # When bridge goes missing, capture more debug info
            echo "[$timestamp] 🔍 Network namespaces:"
            ip netns list 2>/dev/null || echo "No netns command or empty"
            echo "[$timestamp] 🔍 All bridges:"
            ip link show type bridge 2>/dev/null || echo "No bridges found"
            echo "[$timestamp] 🔍 Process tree:"
            ps -eLf | grep -E "(quilt|ip|bridge)" | grep -v grep || echo "No relevant processes"
        fi
        sleep 0.1
    done
} > bridge_debug.log &
MONITOR_PID=$!

# Create container
echo "Creating container..."
RESULT=$(cargo run --bin cli create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- echo "test" 2>&1)
echo "$RESULT"

# Stop monitoring
sleep 2
kill $MONITOR_PID 2>/dev/null

echo ""
echo "📊 Final bridge state:"
ip link show quilt0

echo ""
echo "📋 Bridge monitoring log:"
tail -20 bridge_debug.log

# Cleanup
kill $SERVER_PID 2>/dev/null
echo ""
echo "🧹 Test completed" 