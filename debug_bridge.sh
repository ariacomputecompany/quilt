#!/bin/bash

echo "🔍 Bridge Debugging Script"
echo "=========================="

# Kill any existing server
echo "🧹 Cleaning up existing processes..."
pkill -f "target/debug/quilt" || true
sleep 2

# Clean up any existing bridges
echo "🧹 Cleaning up existing bridges..."
ip link delete quilt0 2>/dev/null || true

# Monitor bridge state
echo "🔍 Starting bridge monitoring in background..."
monitor_bridge() {
    while true; do
        timestamp=$(date '+%H:%M:%S.%3N')
        if ip link show quilt0 >/dev/null 2>&1; then
            status=$(ip link show quilt0 | head -1)
            echo "[$timestamp] ✅ Bridge exists: $status" >> bridge_monitor.log
        else
            echo "[$timestamp] ❌ Bridge does not exist" >> bridge_monitor.log
        fi
        sleep 0.5
    done
}

# Start monitoring in background
> bridge_monitor.log
monitor_bridge &
MONITOR_PID=$!

# Start the server
echo "🚀 Starting Quilt server with bridge monitoring..."
cargo run --bin quilt 2>&1 | tee server_bridge_debug.log &
SERVER_PID=$!

# Wait for server to initialize
sleep 5

echo "🔍 Bridge state after server startup:"
if ip link show quilt0 >/dev/null 2>&1; then
    echo "✅ Bridge exists"
    ip link show quilt0
    ip addr show quilt0
else
    echo "❌ Bridge does not exist after server startup"
fi

# Wait a bit more to see if bridge persists
echo "⏳ Waiting 5 seconds to see if bridge persists..."
sleep 5

echo "🔍 Bridge state after 5 second wait:"
if ip link show quilt0 >/dev/null 2>&1; then
    echo "✅ Bridge still exists"
    ip link show quilt0
else
    echo "❌ Bridge disappeared within 5 seconds"
fi

# Try creating a container
echo "📦 Attempting to create container..."
cargo run --bin cli create \
    --image-path ./nixos-minimal.tar.gz \
    --enable-network-namespace \
    -- sh -c "echo 'Container ready'; sleep 10" 2>&1 | tee container_creation.log &

CONTAINER_PID=$!

# Monitor during container creation
echo "⏳ Monitoring bridge during container creation..."
sleep 10

echo "🔍 Final bridge state:"
if ip link show quilt0 >/dev/null 2>&1; then
    echo "✅ Bridge exists after container creation"
    ip link show quilt0
else
    echo "❌ Bridge does not exist after container creation"
fi

# Stop monitoring
kill $MONITOR_PID 2>/dev/null

# Stop server
kill $SERVER_PID 2>/dev/null
wait $CONTAINER_PID 2>/dev/null

echo ""
echo "📊 Bridge monitoring log:"
cat bridge_monitor.log

echo ""
echo "🔍 Analysis:"
echo "Looking for patterns in when bridge disappears..."

# Analyze the log for patterns
if grep -q "❌ Bridge does not exist" bridge_monitor.log; then
    echo "Bridge disappeared at least once during testing"
    
    # Find first disappearance
    first_disappear=$(grep "❌ Bridge does not exist" bridge_monitor.log | head -1)
    echo "First disappearance: $first_disappear"
    
    # Check if it comes back
    after_disappear=$(grep -A 10 "❌ Bridge does not exist" bridge_monitor.log | grep "✅ Bridge exists")
    if [ -n "$after_disappear" ]; then
        echo "Bridge reappeared after disappearing"
    else
        echo "Bridge never reappeared once it disappeared"
    fi
else
    echo "Bridge remained stable throughout testing"
fi

# Check for network namespace issues
echo ""
echo "🔍 Checking current network namespaces:"
ls -la /var/run/netns/ 2>/dev/null || echo "No network namespaces found"

echo ""
echo "🔍 Current network interfaces:"
ip link show | grep -E "(quilt|bridge)"

echo ""
echo "🔍 Check for any quilt processes still running:"
ps aux | grep quilt | grep -v grep 