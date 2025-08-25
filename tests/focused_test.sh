#!/bin/bash

# Simple focused test with detailed logging
# No complex features, just test core functionality

echo "=== FOCUSED QUILT TEST ==="
echo "Time: $(date)"

# Step 1: Kill ALL processes
echo -e "\n[CLEANUP] Killing all quilt processes..."
pkill -9 -f quilt 2>/dev/null || true
pkill -9 -f cli 2>/dev/null || true
sleep 1

# Verify cleanup
REMAINING=$(ps aux | grep -E "(quilt|cli)" | grep -v grep | wc -l)
echo "[CLEANUP] Remaining processes: $REMAINING"
if [ $REMAINING -gt 0 ]; then
    ps aux | grep -E "(quilt|cli)" | grep -v grep
fi

# Step 2: Start server
echo -e "\n[SERVER] Starting server..."
rm -f server.log
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!
echo "[SERVER] Started with PID: $SERVER_PID"
sleep 3

# Check if server started
nc -z 127.0.0.1 50051
if [ $? -ne 0 ]; then
    echo "[SERVER] ERROR: Server not listening on port 50051"
    echo "[SERVER] Server log:"
    cat server.log
    kill $SERVER_PID 2>/dev/null || true
    exit 1
fi
echo "[SERVER] ✓ Server is listening on port 50051"

# Use unique container name with timestamp
CONTAINER_NAME="test-$(date +%s)"
echo -e "\n[TEST] Using container name: $CONTAINER_NAME"

# Step 3: Create container
echo -e "\n[CREATE] Creating container '$CONTAINER_NAME'..."
CMD="./target/debug/cli create -n $CONTAINER_NAME --async-mode --image-path nixos-minimal.tar.gz"
echo "[CREATE] Command: $CMD"
OUTPUT=$($CMD 2>&1)
CREATE_EXIT=$?
echo "[CREATE] Exit code: $CREATE_EXIT"
echo "[CREATE] Output:"
echo "$OUTPUT"

if [ $CREATE_EXIT -ne 0 ]; then
    echo "[CREATE] ERROR: Failed to create container"
    kill $SERVER_PID 2>/dev/null || true
    exit 1
fi

CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
echo "[CREATE] Container ID: $CONTAINER_ID"

# Step 4: Get status and PID
echo -e "\n[STATUS] Getting container status..."
sleep 3  # Give container time to start
CMD="./target/debug/cli status $CONTAINER_NAME -n"
echo "[STATUS] Command: $CMD"
STATUS=$($CMD 2>&1)
STATUS_EXIT=$?
echo "[STATUS] Exit code: $STATUS_EXIT"
echo "[STATUS] Output:"
echo "$STATUS"

PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')
echo "[STATUS] Extracted PID: $PID"

if [ -z "$PID" ] || [ "$PID" = "0" ]; then
    echo "[STATUS] ERROR: No PID found"
    # Check server log for sleep infinity error
    if grep -q "sleep: invalid number 'infinity'" server.log; then
        echo "[STATUS] ERROR: Container failed with 'sleep infinity' error"
        echo "[STATUS] Last 10 lines of server log:"
        tail -10 server.log
    fi
else
    # Check if process exists
    ps -p $PID > /dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "[STATUS] ✓ Process $PID exists"
        ps -fp $PID
    else
        echo "[STATUS] ERROR: Process $PID not found"
        # Check server log for errors
        if grep -q "sleep: invalid number 'infinity'" server.log; then
            echo "[STATUS] ERROR: Container failed with 'sleep infinity' error"
        fi
    fi
fi

# Step 5: Stop container
echo -e "\n[STOP] Stopping container..."
CMD="./target/debug/cli stop $CONTAINER_NAME -n"
echo "[STOP] Command: $CMD"
OUTPUT=$($CMD 2>&1)
STOP_EXIT=$?
echo "[STOP] Exit code: $STOP_EXIT"
echo "[STOP] Output:"
echo "$OUTPUT"

# Wait and check if process is gone
echo "[STOP] Waiting for process to terminate..."
sleep 2

if [ ! -z "$PID" ] && [ "$PID" != "0" ]; then
    ps -p $PID > /dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "[STOP] ERROR: Process $PID still exists after stop"
        ps -fp $PID
    else
        echo "[STOP] ✓ Process $PID terminated"
    fi
fi

# Step 6: Check status after stop
echo -e "\n[STATUS] Checking status after stop..."
CMD="./target/debug/cli status $CONTAINER_NAME -n"
STATUS=$($CMD 2>&1)
echo "[STATUS] Output:"
echo "$STATUS"

if echo "$STATUS" | grep -q "Status: EXITED"; then
    echo "[STATUS] ✓ Container is in EXITED state"
else
    echo "[STATUS] ERROR: Container not in EXITED state"
fi

# Step 7: Start container again
echo -e "\n[START] Starting stopped container..."
CMD="./target/debug/cli start $CONTAINER_NAME -n"
echo "[START] Command: $CMD"
OUTPUT=$($CMD 2>&1)
START_EXIT=$?
echo "[START] Exit code: $START_EXIT"
echo "[START] Output:"
echo "$OUTPUT"

# Verify it's restarting, not creating new
if echo "$OUTPUT" | grep -q "Creating container"; then
    echo "[START] ERROR: Start command is creating new container instead of restarting!"
else
    echo "[START] ✓ Container restarted without recreation"
fi

# Get new status
sleep 2
echo "[START] Getting new status..."
STATUS=$(./target/debug/cli status $CONTAINER_NAME -n 2>&1)
NEW_PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')
echo "[START] New PID: $NEW_PID"

if [ ! -z "$NEW_PID" ] && [ "$NEW_PID" != "0" ]; then
    ps -p $NEW_PID > /dev/null 2>&1
    if [ $? -eq 0 ]; then
        echo "[START] ✓ New process $NEW_PID exists"
        ps -fp $NEW_PID
    else
        echo "[START] ERROR: New process $NEW_PID not found"
    fi
else
    echo "[START] ERROR: No new PID after start"
fi

# Step 8: Kill container
echo -e "\n[KILL] Killing container..."
CMD="./target/debug/cli kill $CONTAINER_NAME -n"
echo "[KILL] Command: $CMD"
OUTPUT=$($CMD 2>&1)
KILL_EXIT=$?
echo "[KILL] Exit code: $KILL_EXIT"
echo "[KILL] Output:"
echo "$OUTPUT"

# Step 9: Cleanup
echo -e "\n[CLEANUP] Final cleanup..."
./target/debug/cli remove $CONTAINER_NAME -n --force 2>&1
kill $SERVER_PID 2>/dev/null || true
pkill -9 -f quilt 2>/dev/null || true

echo -e "\n=== TEST COMPLETED ==="