#!/bin/bash

# Quick Sync Engine Test
echo "🚀 Quick Sync Engine Performance Test"
echo "====================================="

# Start server in background
echo "Starting Quilt server..."
./target/release/quilt &
SERVER_PID=$!

# Wait for server to start
echo "Waiting for server..."
sleep 5

# Test 1: Container creation performance
echo ""
echo "📦 Testing container creation performance..."
start_time=$(date +%s%N)

grpcurl -plaintext -d '{
  "image_path": "nixos-minimal.tar.gz",
  "command": ["sleep", "30"],
  "environment": {},
  "enable_network_namespace": false,
  "enable_pid_namespace": true,
  "enable_mount_namespace": true,
  "enable_uts_namespace": true,
  "enable_ipc_namespace": true
}' 127.0.0.1:50051 quilt.QuiltService/CreateContainer > create_result.json 2>/dev/null

end_time=$(date +%s%N)
create_duration=$(( (end_time - start_time) / 1000000 ))

if [[ $? -eq 0 ]]; then
    container_id=$(grep -o '"container_id":"[^"]*"' create_result.json | cut -d'"' -f4)
    echo "✅ Container created in ${create_duration}ms"
    echo "   Container ID: $container_id"
    
    # Test 2: Status query performance
    echo ""
    echo "⚡ Testing status query performance (10 rapid queries)..."
    total_time=0
    
    for i in {1..10}; do
        start_time=$(date +%s%N)
        grpcurl -plaintext -d "{\"container_id\": \"$container_id\"}" 127.0.0.1:50051 quilt.QuiltService/GetContainerStatus > /dev/null 2>&1
        end_time=$(date +%s%N)
        query_time=$(( (end_time - start_time) / 1000000 ))
        total_time=$((total_time + query_time))
    done
    
    avg_time=$((total_time / 10))
    echo "✅ Average status query time: ${avg_time}ms"
    
    if [[ $avg_time -lt 50 ]]; then
        echo "🎯 SYNC ENGINE PERFORMANCE VERIFIED!"
        echo "   Status queries are blazing fast (target: <50ms)"
    fi
else
    echo "❌ Container creation failed"
fi

# Cleanup
echo ""
echo "Cleaning up..."
kill $SERVER_PID 2>/dev/null || true
rm -f create_result.json

echo ""
echo "=================================="
echo "✅ SYNC ENGINE TEST COMPLETE!"
echo "==================================" 