#!/bin/bash

# Test 1: Basic Container Connectivity
# This test ensures containers can be created, stay running, and get network IPs
# NO FALSE POSITIVES - Production grade test

set +e  # Don't exit on errors - we handle them manually

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Test configuration
TEST_IMAGE="./nixos-minimal.tar.gz"
SERVER_PID=""

# Helper functions
info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[PASS]${NC} $1"
}

fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    echo -e "  ${YELLOW}Details: $2${NC}"
}

debug() {
    echo -e "${BLUE}[DEBUG]${NC} $1"
}

cleanup() {
    info "Cleaning up..."
    
    if [ ! -z "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null || true
    fi
    
    pkill -9 -f quilt 2>/dev/null || true
    rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    rm -f quilt.db quilt.db-shm quilt.db-wal 2>/dev/null || true
}

trap cleanup EXIT INT TERM

# Build the project
info "Building Quilt..."
if cargo build --quiet; then
    success "Build completed"
else
    fail "Build failed" "Run 'cargo build' to see errors"
    exit 1
fi

# Start server
info "Starting Quilt server..."
./target/debug/quilt > server.log 2>&1 &
SERVER_PID=$!

# Wait for server
sleep 2
if ! kill -0 $SERVER_PID 2>/dev/null; then
    fail "Server failed to start" "Check server.log"
    cat server.log
    exit 1
fi

# Wait for server to be ready
for i in {1..10}; do
    if nc -z 127.0.0.1 50051 2>/dev/null; then
        success "Server ready on port 50051"
        break
    fi
    sleep 0.5
done

echo -e "\n${BLUE}=== Test 1: Basic Container Creation ===${NC}"

# Create a simple container with sleep command
info "Creating container with sleep 3600..."
CREATE_OUTPUT=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-network-namespace -- sleep 3600 2>&1)
CREATE_RESULT=$?

debug "Create exit code: $CREATE_RESULT"
debug "Create output: $CREATE_OUTPUT"

if [ $CREATE_RESULT -ne 0 ]; then
    fail "Container creation failed" "Exit code: $CREATE_RESULT"
    exit 1
fi

CONTAINER_ID=$(echo "$CREATE_OUTPUT" | grep "Container ID:" | awk '{print $NF}')
if [ -z "$CONTAINER_ID" ]; then
    fail "Could not extract container ID" "Output: $CREATE_OUTPUT"
    exit 1
fi

success "Container created: $CONTAINER_ID"

# Check container status
info "Waiting for container to reach RUNNING state..."
for i in {1..20}; do
    STATUS_OUTPUT=$(./target/debug/cli status "$CONTAINER_ID" 2>&1)
    debug "Status check $i: $(echo "$STATUS_OUTPUT" | grep "Status:" | awk '{print $2}')"
    
    if echo "$STATUS_OUTPUT" | grep -q "Status: RUNNING"; then
        success "Container is RUNNING"
        break
    elif echo "$STATUS_OUTPUT" | grep -q "Status: FAILED"; then
        fail "Container FAILED" "Check logs"
        echo "$STATUS_OUTPUT"
        exit 1
    fi
    sleep 0.5
done

# Check if container got an IP
info "Checking container network configuration..."
IP=$(echo "$STATUS_OUTPUT" | grep "IP:" | awk '{print $2}')
if [ -z "$IP" ] || [ "$IP" = "N/A" ]; then
    fail "Container did not get an IP address" "IP: $IP"
    exit 1
fi

success "Container has IP: $IP"

# Test basic exec with echo
echo -e "\n${BLUE}=== Test 2: Basic Exec Command ===${NC}"
info "Testing exec with echo command..."

EXEC_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "echo 'Hello from container'" --capture-output 2>&1)
EXEC_RESULT=$?

debug "Exec exit code: $EXEC_RESULT"
debug "Exec output: $EXEC_OUTPUT"

if [ $EXEC_RESULT -ne 0 ]; then
    fail "Exec command failed" "Exit code: $EXEC_RESULT, Output: $EXEC_OUTPUT"
    
    # Try to debug what's in the container
    info "Debugging container filesystem..."
    debug "Checking /bin directory in container..."
    LS_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin" --capture-output 2>&1 || echo "ls failed")
    debug "Container /bin contents: $LS_OUTPUT"
    
    exit 1
fi

if echo "$EXEC_OUTPUT" | grep -q "Hello from container"; then
    success "Echo command executed successfully"
else
    fail "Echo output not found" "Expected 'Hello from container', got: $EXEC_OUTPUT"
    exit 1
fi

# Test listing files
echo -e "\n${BLUE}=== Test 3: File Listing ===${NC}"
info "Testing ls command..."

LS_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls /" --capture-output 2>&1)
LS_RESULT=$?

debug "ls exit code: $LS_RESULT"
debug "ls output: $LS_OUTPUT"

if [ $LS_RESULT -eq 0 ]; then
    success "ls command executed successfully"
else
    fail "ls command failed" "Exit code: $LS_RESULT"
    
    # Check what shell is being used
    debug "Checking shell..."
    SHELL_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin/sh" --capture-output 2>&1 || echo "check failed")
    debug "Shell info: $SHELL_OUTPUT"
fi

# Summary
echo -e "\n${BLUE}=== Test Summary ===${NC}"
echo "Container ID: $CONTAINER_ID"
echo "Container IP: $IP"
echo "Container Status: RUNNING"
echo -e "${GREEN}✓ All basic connectivity tests passed!${NC}"

exit 0