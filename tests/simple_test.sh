#!/bin/bash
set -e

echo "Starting simple test..."

# Kill any existing processes
pkill -9 -f quilt 2>/dev/null || true
pkill -9 -f cli 2>/dev/null || true
sleep 1

# Start server
echo "Starting server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!
sleep 2

# Check server is running
nc -z 127.0.0.1 50051
if [ $? -ne 0 ]; then
    echo "Server failed to start"
    cat server.log
    exit 1
fi

echo "Server running on PID $SERVER_PID"

# Test 1: Create container with name
echo ""
echo "Test 1: Create container with name"
OUTPUT=$(./target/debug/cli create -n test-container --async-mode --image-path nixos-minimal.tar.gz 2>&1)
if echo "$OUTPUT" | grep -q "Container created successfully"; then
    echo "✓ Container created"
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    echo "  ID: $CONTAINER_ID"
else
    echo "✗ Failed to create container"
    echo "$OUTPUT"
fi

# Test 2: Check status and get PID
echo ""
echo "Test 2: Get status and PID"
sleep 2
STATUS=$(./target/debug/cli status test-container -n 2>&1)
echo "$STATUS"
PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')
if [ ! -z "$PID" ] && [ "$PID" != "0" ]; then
    echo "✓ Container has PID: $PID"
    if ps -p $PID > /dev/null 2>&1; then
        echo "✓ Process $PID is running"
    else
        echo "✗ Process $PID not found"
    fi
else
    echo "✗ No PID found"
fi

# Test 3: Stop container
echo ""
echo "Test 3: Stop container"
./target/debug/cli stop test-container -n
sleep 1
if [ ! -z "$PID" ] && [ "$PID" != "0" ]; then
    if ps -p $PID > /dev/null 2>&1; then
        echo "✗ Process $PID still running after stop"
    else
        echo "✓ Process $PID terminated"
    fi
fi

# Test 4: Check status after stop
echo ""
echo "Test 4: Status after stop"
STATUS=$(./target/debug/cli status test-container -n 2>&1)
echo "$STATUS"
if echo "$STATUS" | grep -q "Status: EXITED"; then
    echo "✓ Container is EXITED"
else
    echo "✗ Container not in EXITED state"
fi

# Test 5: Start container again
echo ""
echo "Test 5: Start stopped container"
./target/debug/cli start test-container -n
sleep 2
STATUS=$(./target/debug/cli status test-container -n 2>&1)
echo "$STATUS"
NEW_PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')
if [ ! -z "$NEW_PID" ] && [ "$NEW_PID" != "0" ] && [ "$NEW_PID" != "$PID" ]; then
    echo "✓ New PID: $NEW_PID"
    if ps -p $NEW_PID > /dev/null 2>&1; then
        echo "✓ New process is running"
    else
        echo "✗ New process not found"
    fi
else
    echo "✗ Failed to start with new PID"
fi

# Cleanup
echo ""
echo "Cleaning up..."
./target/debug/cli kill test-container -n 2>/dev/null || true
./target/debug/cli remove test-container -n --force 2>/dev/null || true
kill $SERVER_PID 2>/dev/null || true
pkill -9 -f quilt 2>/dev/null || true

echo ""
echo "Test completed!"