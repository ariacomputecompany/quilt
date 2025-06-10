#!/bin/bash
# Quick network diagnostic test

echo "🔧 DIAGNOSTIC: Testing optimized network setup..."
cargo build > /dev/null 2>&1
./target/debug/quilt > diagnostic.log 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"
sleep 2

echo ""
echo "📊 Creating 3 containers with timing..."
for i in {1..3}; do
    echo -n "Container $i: "
    start_time=$(date +%s%3N)
    timeout 45 ./target/debug/cli create --image-path ./nixos-minimal.tar.gz -- sleep 30 > create_$i.log 2>&1
    end_time=$(date +%s%3N)
    duration=$((end_time - start_time))
    
    if [ $? -eq 0 ]; then
        echo "✅ ${duration}ms"
    else
        echo "❌ Failed (${duration}ms)"
        cat create_$i.log | tail -3
    fi
done

echo ""
echo "🔍 Network bridge status:"
ip link show quilt0 2>/dev/null || echo "No bridge found"

echo ""
echo "📝 Server log (last 20 lines):"
tail -20 diagnostic.log

# Cleanup
kill $SERVER_PID 2>/dev/null
sleep 1
pkill -f "./target/debug/quilt" 2>/dev/null 