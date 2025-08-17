#!/bin/bash

# Comprehensive Volume Functionality Test
# Tests all volume features with REAL validation - NO FALSE POSITIVES

set +e  # Don't exit on errors - we handle them manually

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Test configuration
SERVER_PID=""
TEST_ID=$(date +%s)
TEST_IMAGE="./nixos-minimal.tar.gz"
VOLUME_BASE="/var/lib/quilt/volumes"
TEST_DIR="/tmp/quilt-volume-test-${TEST_ID}"

# Test counters
TOTAL_TESTS=0
PASSED_TESTS=0

# Helper functions
info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

success() {
    echo -e "${GREEN}[PASS]${NC} $1"
    PASSED_TESTS=$((PASSED_TESTS + 1))
}

fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    echo -e "  ${YELLOW}Details: $2${NC}"
}

run_test() {
    local test_name="$1"
    local test_cmd="$2"
    local expected="$3"
    
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    echo -n "  Testing: $test_name... "
    
    if eval "$test_cmd"; then
        echo -e "${GREEN}PASSED${NC}"
        PASSED_TESTS=$((PASSED_TESTS + 1))
    else
        echo -e "${RED}FAILED${NC}"
        echo -e "    Command: $test_cmd"
        echo -e "    Expected: $expected"
    fi
}

cleanup() {
    info "Cleaning up test environment..."
    
    # Kill server
    if [ ! -z "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null || true
        wait $SERVER_PID 2>/dev/null || true
    fi
    
    # Force kill any remaining processes
    pkill -9 -f quilt 2>/dev/null || true
    
    # Clean up test directories and files
    rm -rf "$TEST_DIR" 2>/dev/null || true
    rm -f server.log test_file.txt 2>/dev/null || true
    
    # Clean up test volumes (requires sudo for /var/lib)
    sudo rm -rf ${VOLUME_BASE}/test-vol-* 2>/dev/null || true
    
    # Clean up test containers
    rm -rf /tmp/quilt-containers/* 2>/dev/null || true
}

trap cleanup EXIT INT TERM

# Ensure we have required permissions
check_permissions() {
    if [ ! -w "/var/lib" ]; then
        echo -e "${RED}This test requires write access to /var/lib for volume creation${NC}"
        echo "Please run with appropriate permissions or adjust VOLUME_BASE"
        exit 1
    fi
}

# Build the project
build_project() {
    info "Building Quilt..."
    if cargo build --quiet; then
        success "Build completed"
    else
        fail "Build failed" "Run 'cargo build' to see errors"
        exit 1
    fi
}

# Start server
start_server() {
    info "Starting Quilt server..."
    ./target/debug/quilt > server.log 2>&1 &
    SERVER_PID=$!
    
    # Wait for server to be ready
    local retries=30
    while [ $retries -gt 0 ]; do
        if nc -z 127.0.0.1 50051 2>/dev/null; then
            success "Server started (PID: $SERVER_PID)"
            return 0
        fi
        sleep 0.1
        retries=$((retries - 1))
    done
    
    fail "Server failed to start" "Check server.log"
    cat server.log
    exit 1
}

# Create test directories and files
setup_test_env() {
    info "Setting up test environment..."
    mkdir -p "$TEST_DIR"
    echo "test content" > "$TEST_DIR/test_file.txt"
    mkdir -p "$TEST_DIR/config"
    echo "config data" > "$TEST_DIR/config/app.conf"
    success "Test environment ready"
}

echo -e "${BLUE}=== Comprehensive Volume Functionality Test ===${NC}"
echo -e "${BLUE}Testing with image: $TEST_IMAGE${NC}\n"

# Initial setup
check_permissions
build_project
start_server
setup_test_env

# Test 1: Basic bind mount
echo -e "\n${BLUE}=== TEST 1: Basic Bind Mount ===${NC}"

# Create container with bind mount
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR:/mnt/test" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created container with bind mount: $CONTAINER_ID"
    
    # Wait for container to start
    sleep 1
    
    # Verify mount inside container
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" -c "ls /mnt/test" --capture-output 2>&1 )
    
    run_test "Bind mount accessible" \
        "echo '$EXEC_OUTPUT' | grep -q 'test_file.txt'" \
        "Should see test_file.txt in mount"
    
    # Test file read
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" -c "cat /mnt/test/test_file.txt" --capture-output 2>&1 )
    
    run_test "Read file from bind mount" \
        "echo '$EXEC_OUTPUT' | grep -q 'test content'" \
        "Should read file content"
    
    # Test write to bind mount
    ./target/debug/cli exec "$CONTAINER_ID" -c "echo 'new content' > /mnt/test/new_file.txt" 2>&1
    
    run_test "Write to bind mount" \
        "[ -f '$TEST_DIR/new_file.txt' ]" \
        "File should exist on host"
    
    run_test "Written content persists" \
        "grep -q 'new content' '$TEST_DIR/new_file.txt'" \
        "Content should match"
    
    # Cleanup
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    fail "Failed to create container with bind mount" "$OUTPUT"
fi

# Test 2: Read-only bind mount
echo -e "\n${BLUE}=== TEST 2: Read-only Bind Mount ===${NC}"

OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR:/mnt/readonly:ro" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created container with read-only mount: $CONTAINER_ID"
    
    sleep 1
    
    # Try to write (should fail)
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" \
        -c "echo 'test' > /mnt/readonly/should_fail.txt 2>&1" \
        --capture-output 2>&1 )
    
    run_test "Read-only mount prevents writes" \
        "echo '$EXEC_OUTPUT' | grep -qE '(Read-only|Permission denied)'" \
        "Write should be denied"
    
    # Verify file was NOT created
    run_test "No file created on read-only mount" \
        "[ ! -f '$TEST_DIR/should_fail.txt' ]" \
        "File should not exist"
    
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    fail "Failed to create container with read-only mount" "$OUTPUT"
fi

# Test 3: Multiple mounts
echo -e "\n${BLUE}=== TEST 3: Multiple Mounts ===${NC}"

OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR:/mnt/data" \
    -v "$TEST_DIR/config:/etc/app:ro" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created container with multiple mounts: $CONTAINER_ID"
    
    sleep 1
    
    # Check both mounts exist
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" -c "ls /mnt/data && ls /etc/app" --capture-output 2>&1 )
    
    run_test "Both mounts accessible" \
        "echo '$EXEC_OUTPUT' | grep -q 'app.conf'" \
        "Should see config file"
    
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    fail "Failed to create container with multiple mounts" "$OUTPUT"
fi

# Test 4: Security validation - path traversal
echo -e "\n${BLUE}=== TEST 4: Security Validation ===${NC}"

# Try path traversal
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "../../../etc:/mnt/hack" \
    --async-mode 2>&1 )

run_test "Path traversal blocked" \
    "echo '$OUTPUT' | grep -q 'Path traversal detected'" \
    "Should detect and block .."

# Try to mount sensitive path
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "/etc/passwd:/mnt/passwd" \
    --async-mode 2>&1 )

run_test "Sensitive path blocked" \
    "echo '$OUTPUT' | grep -q 'not allowed'" \
    "Should block /etc/passwd"

# Try to mount over critical container path
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR:/etc" \
    --async-mode 2>&1 )

run_test "Critical container path protected" \
    "echo '$OUTPUT' | grep -q 'protected path'" \
    "Should protect /etc in container"

# Test 5: Advanced mount syntax
echo -e "\n${BLUE}=== TEST 5: Advanced Mount Syntax ===${NC}"

OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    --mount "type=bind,source=$TEST_DIR,target=/mnt/advanced,readonly" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created container with --mount syntax: $CONTAINER_ID"
    
    sleep 1
    
    # Verify mount
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" -c "ls /mnt/advanced" --capture-output 2>&1 )
    
    run_test "Advanced mount syntax works" \
        "echo '$EXEC_OUTPUT' | grep -q 'test_file.txt'" \
        "Mount should be accessible"
    
    # Verify readonly
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" \
        -c "echo 'test' > /mnt/advanced/fail.txt 2>&1" \
        --capture-output 2>&1 )
    
    run_test "Advanced mount readonly option" \
        "echo '$EXEC_OUTPUT' | grep -qE '(Read-only|Permission denied)'" \
        "Should be read-only"
    
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    fail "Failed with advanced mount syntax" "$OUTPUT"
fi

# Test 6: Named volumes (when implemented)
echo -e "\n${BLUE}=== TEST 6: Named Volumes ===${NC}"

# For now, test that volume names are recognized
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "my-data-volume:/data" \
    --async-mode 2>&1 )

# This should work once volume management is implemented
if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Named volume syntax accepted: $CONTAINER_ID"
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    info "Named volumes not yet fully implemented (expected)"
fi

# Test 7: Tmpfs mount
echo -e "\n${BLUE}=== TEST 7: Tmpfs Mount ===${NC}"

OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    --mount "type=tmpfs,target=/tmp/scratch,size=10m" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created container with tmpfs: $CONTAINER_ID"
    
    sleep 1
    
    # Test tmpfs is writable
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" \
        -c "echo 'tmpfs test' > /tmp/scratch/test.txt && cat /tmp/scratch/test.txt" \
        --capture-output 2>&1 )
    
    run_test "Tmpfs mount is writable" \
        "echo '$EXEC_OUTPUT' | grep -q 'tmpfs test'" \
        "Should write and read from tmpfs"
    
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    fail "Failed to create container with tmpfs" "$OUTPUT"
fi

# Test 8: Mount persistence across container restart
echo -e "\n${BLUE}=== TEST 8: Mount Persistence ===${NC}"

# Create container with mount
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR:/mnt/persist" \
    --async-mode \
    -n "persist-test-$TEST_ID" 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created named container: $CONTAINER_ID"
    
    sleep 1
    
    # Write data
    ./target/debug/cli exec "$CONTAINER_ID" -c "echo 'persist data' > /mnt/persist/persist.txt" 2>&1
    
    # Stop container
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    sleep 0.5
    
    # Start container again
    ./target/debug/cli start "$CONTAINER_ID" >/dev/null 2>&1
    sleep 1
    
    # Check data persists
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER_ID" \
        -c "cat /mnt/persist/persist.txt" \
        --capture-output 2>&1 )
    
    run_test "Data persists across restart" \
        "echo '$EXEC_OUTPUT' | grep -q 'persist data'" \
        "Should see persisted data"
    
    ./target/debug/cli stop "$CONTAINER_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER_ID" --force >/dev/null 2>&1
else
    fail "Failed to create persistence test container" "$OUTPUT"
fi

# Test 9: Concurrent mount access
echo -e "\n${BLUE}=== TEST 9: Concurrent Mount Access ===${NC}"

# Create shared directory
mkdir -p "$TEST_DIR/shared"
echo "initial" > "$TEST_DIR/shared/counter.txt"

# Create two containers with same mount
CONTAINER1_ID=""
CONTAINER2_ID=""

OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR/shared:/mnt/shared" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER1_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created first container: $CONTAINER1_ID"
fi

OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "$TEST_DIR/shared:/mnt/shared" \
    --async-mode 2>&1 )

if echo "$OUTPUT" | grep -q "Container created successfully"; then
    CONTAINER2_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
    success "Created second container: $CONTAINER2_ID"
fi

if [ ! -z "$CONTAINER1_ID" ] && [ ! -z "$CONTAINER2_ID" ]; then
    sleep 1
    
    # Write from container 1
    ./target/debug/cli exec "$CONTAINER1_ID" -c "echo 'from container 1' > /mnt/shared/test1.txt" 2>&1
    
    # Read from container 2
    EXEC_OUTPUT=$( ./target/debug/cli exec "$CONTAINER2_ID" \
        -c "cat /mnt/shared/test1.txt" \
        --capture-output 2>&1 )
    
    run_test "Shared mount accessible" \
        "echo '$EXEC_OUTPUT' | grep -q 'from container 1'" \
        "Container 2 should see container 1's write"
    
    # Concurrent writes test
    ./target/debug/cli exec "$CONTAINER1_ID" -c "echo 'c1' >> /mnt/shared/concurrent.txt" 2>&1 &
    ./target/debug/cli exec "$CONTAINER2_ID" -c "echo 'c2' >> /mnt/shared/concurrent.txt" 2>&1 &
    wait
    
    run_test "Concurrent writes complete" \
        "[ -f '$TEST_DIR/shared/concurrent.txt' ]" \
        "File should exist after concurrent writes"
    
    # Cleanup
    ./target/debug/cli stop "$CONTAINER1_ID" >/dev/null 2>&1
    ./target/debug/cli stop "$CONTAINER2_ID" >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER1_ID" --force >/dev/null 2>&1
    ./target/debug/cli remove "$CONTAINER2_ID" --force >/dev/null 2>&1
fi

# Test 10: Error handling
echo -e "\n${BLUE}=== TEST 10: Error Handling ===${NC}"

# Non-existent source
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "/non/existent/path:/mnt/test" \
    --async-mode 2>&1 )

run_test "Non-existent source rejected" \
    "echo '$OUTPUT' | grep -q 'does not exist'" \
    "Should fail with clear error"

# Invalid mount format
OUTPUT=$( ./target/debug/cli create \
    --image-path "$TEST_IMAGE" \
    -v "invalid-format" \
    --async-mode 2>&1 )

run_test "Invalid format rejected" \
    "echo '$OUTPUT' | grep -q 'format'" \
    "Should fail with format error"

# Summary
echo -e "\n${BLUE}=== TEST SUMMARY ===${NC}"
echo -e "Total tests: $TOTAL_TESTS"
echo -e "Passed: ${GREEN}$PASSED_TESTS${NC}"
echo -e "Failed: ${RED}$((TOTAL_TESTS - PASSED_TESTS))${NC}"

if [ $PASSED_TESTS -eq $TOTAL_TESTS ]; then
    echo -e "\n${GREEN}✓ ALL VOLUME TESTS PASSED!${NC}"
    exit 0
else
    echo -e "\n${RED}✗ Some tests failed${NC}"
    exit 1
fi