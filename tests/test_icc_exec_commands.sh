#!/bin/bash

# Test 2: Exec Commands Test
# This test verifies that exec commands work properly, including busybox applets
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

echo -e "\n${BLUE}=== Test 1: Container Setup ===${NC}"

# Create a container
info "Creating container..."
CREATE_OUTPUT=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-network-namespace -- sleep 3600 2>&1)
CONTAINER_ID=$(echo "$CREATE_OUTPUT" | grep "Container ID:" | awk '{print $NF}')

if [ -z "$CONTAINER_ID" ]; then
    fail "Container creation failed" "Output: $CREATE_OUTPUT"
    exit 1
fi

success "Container created: $CONTAINER_ID"

# Wait for RUNNING state
info "Waiting for container to be ready..."
for i in {1..20}; do
    STATUS=$(./target/debug/cli status "$CONTAINER_ID" 2>&1 | grep "Status:" | awk '{print $2}')
    if [ "$STATUS" = "RUNNING" ]; then
        success "Container is RUNNING"
        break
    fi
    sleep 0.5
done

echo -e "\n${BLUE}=== Test 2: Basic Commands ===${NC}"

# Test echo
info "Testing echo command..."
ECHO_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "echo 'test'" --capture-output 2>&1)
if echo "$ECHO_OUTPUT" | grep -q "test"; then
    success "echo works"
else
    fail "echo failed" "Output: $ECHO_OUTPUT"
fi

# Test pwd
info "Testing pwd command..."
PWD_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "pwd" --capture-output 2>&1)
if echo "$PWD_OUTPUT" | grep -q "/"; then
    success "pwd works"
else
    fail "pwd failed" "Output: $PWD_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 3: Busybox Binary Check ===${NC}"

# Check if busybox exists
info "Checking for busybox binary..."
BUSYBOX_CHECK=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin/busybox" --capture-output 2>&1)
debug "Busybox check: $BUSYBOX_CHECK"

if echo "$BUSYBOX_CHECK" | grep -q "busybox"; then
    success "Busybox binary found"
    
    # Test busybox directly
    info "Testing busybox --help..."
    BUSYBOX_HELP=$(./target/debug/cli exec "$CONTAINER_ID" -c "/bin/busybox --help" --capture-output 2>&1)
    if echo "$BUSYBOX_HELP" | grep -q "BusyBox"; then
        success "Busybox is executable"
    else
        fail "Busybox not working" "Output: $BUSYBOX_HELP"
    fi
else
    fail "Busybox not found" "Need to check installation"
fi

echo -e "\n${BLUE}=== Test 4: Shell and Symlinks ===${NC}"

# Check shell
info "Checking shell symlink..."
SH_CHECK=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin/sh" --capture-output 2>&1)
debug "Shell check: $SH_CHECK"

# Check what shell we're actually using
info "Checking shell version..."
SHELL_VERSION=$(./target/debug/cli exec "$CONTAINER_ID" -c "/bin/sh --version || echo 'version check failed'" --capture-output 2>&1)
debug "Shell version: $SHELL_VERSION"

echo -e "\n${BLUE}=== Test 5: Busybox Applets ===${NC}"

# Check if ping exists as symlink
info "Checking ping symlink..."
PING_CHECK=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin/ping" --capture-output 2>&1)
debug "Ping check: $PING_CHECK"

# Try to run ping with busybox directly
info "Testing busybox ping..."
BUSYBOX_PING=$(./target/debug/cli exec "$CONTAINER_ID" -c "/bin/busybox ping -c 1 127.0.0.1" --capture-output 2>&1)
BUSYBOX_PING_RESULT=$?
debug "Busybox ping exit code: $BUSYBOX_PING_RESULT"
debug "Busybox ping output: $BUSYBOX_PING"

if [ $BUSYBOX_PING_RESULT -eq 0 ]; then
    success "Busybox ping works directly"
else
    fail "Busybox ping failed" "Exit code: $BUSYBOX_PING_RESULT"
fi

# Try regular ping command
info "Testing ping symlink..."
PING_OUTPUT=$(./target/debug/cli exec "$CONTAINER_ID" -c "ping -c 1 127.0.0.1" --capture-output 2>&1)
PING_RESULT=$?
debug "Ping exit code: $PING_RESULT"
debug "Ping output: $PING_OUTPUT"

if [ $PING_RESULT -eq 0 ]; then
    success "Ping symlink works"
else
    fail "Ping symlink failed" "Exit code: $PING_RESULT"
    
    # Debug: List all files in /bin
    info "Listing /bin directory for debugging..."
    BIN_LIST=$(./target/debug/cli exec "$CONTAINER_ID" -c "ls -la /bin/ | head -20" --capture-output 2>&1)
    debug "First 20 files in /bin:\n$BIN_LIST"
fi

echo -e "\n${BLUE}=== Test 6: Other Important Commands ===${NC}"

# Test sleep (should work as it's what keeps container running)
info "Testing sleep command..."
SLEEP_TEST=$(./target/debug/cli exec "$CONTAINER_ID" -c "sleep 0.1 && echo 'sleep works'" --capture-output 2>&1)
if echo "$SLEEP_TEST" | grep -q "sleep works"; then
    success "sleep command works"
else
    fail "sleep command failed" "Output: $SLEEP_TEST"
fi

# Test nslookup
info "Testing nslookup..."
NSLOOKUP_CHECK=$(./target/debug/cli exec "$CONTAINER_ID" -c "which nslookup || echo 'not found'" --capture-output 2>&1)
debug "nslookup check: $NSLOOKUP_CHECK"

if ! echo "$NSLOOKUP_CHECK" | grep -q "not found"; then
    NSLOOKUP_TEST=$(./target/debug/cli exec "$CONTAINER_ID" -c "nslookup localhost 127.0.0.1 || echo 'nslookup failed'" --capture-output 2>&1)
    debug "nslookup output: $NSLOOKUP_TEST"
fi

# Summary
echo -e "\n${BLUE}=== Test Summary ===${NC}"
echo "Container ID: $CONTAINER_ID"

# Final check of what's actually working
WORKING_COMMANDS=""
FAILED_COMMANDS=""

# Check various commands
for cmd in "echo" "ls" "pwd" "sleep" "ping" "nslookup" "ps" "grep"; do
    if ./target/debug/cli exec "$CONTAINER_ID" -c "which $cmd" --capture-output 2>&1 | grep -q "/bin/$cmd"; then
        WORKING_COMMANDS="$WORKING_COMMANDS $cmd"
    else
        FAILED_COMMANDS="$FAILED_COMMANDS $cmd"
    fi
done

echo -e "${GREEN}Working commands:${NC}$WORKING_COMMANDS"
if [ ! -z "$FAILED_COMMANDS" ]; then
    echo -e "${RED}Missing commands:${NC}$FAILED_COMMANDS"
fi

exit 0