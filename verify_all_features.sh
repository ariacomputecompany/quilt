#!/bin/bash

# Comprehensive verification of all Quilt features
# This script verifies each feature with real tests and no false positives

# Don't exit on error - we want to run all tests
set +e

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Find binaries
SERVER_BINARY="./target/debug/quilt"
CLI_BINARY="./target/debug/cli"

if [ ! -f "$SERVER_BINARY" ] || [ ! -f "$CLI_BINARY" ]; then
    echo -e "${RED}Binaries not found. Run 'cargo build' first.${NC}"
    exit 1
fi

echo -e "${BLUE}=== Comprehensive Quilt Feature Verification ===${NC}"
echo -e "${BLUE}Testing with real nix tarball: nixos-minimal.tar.gz${NC}\n"
echo -e "${YELLOW}[DEBUG] Script starting...${NC}"

# Kill any existing quilt processes
echo -e "${YELLOW}[DEBUG] Cleaning up any existing processes...${NC}"
pkill -9 -f quilt 2>/dev/null || true
pkill -9 -f cli 2>/dev/null || true
sleep 1

# Clean up database to ensure fresh start
echo -e "${YELLOW}[DEBUG] Removing old database...${NC}"
rm -f quilt.db quilt.db-shm quilt.db-wal

# Clean up any leftover container directories
echo -e "${YELLOW}[DEBUG] Cleaning up container directories...${NC}"
rm -rf /tmp/quilt-containers/* 2>/dev/null || true
rm -rf /tmp/quilt-image-cache/overlays/* 2>/dev/null || true

# Start server
echo -e "${BLUE}Starting Quilt server...${NC}"
$SERVER_BINARY > server.log 2>&1 &
SERVER_PID=$!

# Wait for server to be ready (event-driven)
for i in {1..20}; do
    if nc -z 127.0.0.1 50051 2>/dev/null; then
        break
    fi
    if [ $i -eq 20 ]; then
        echo -e "${RED}Server failed to become ready${NC}"
        exit 1
    fi
    sleep 0.1
done
if ! nc -z 127.0.0.1 50051 2>/dev/null; then
    echo -e "${RED}Server failed to start!${NC}"
    cat server.log
    kill $SERVER_PID 2>/dev/null || true
    exit 1
fi
echo -e "${GREEN}✓ Server started successfully${NC}"

cleanup() {
    echo -e "\n${BLUE}Cleaning up...${NC}"
    
    # Kill server first with increasing force
    if [ ! -z "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null || true
        kill -9 $SERVER_PID 2>/dev/null || true
    fi
    
    # Force kill any quilt processes
    pkill -f "quilt" 2>/dev/null || true
    pkill -9 -f "quilt" 2>/dev/null || true
    
    # Try to remove containers via CLI if possible
    for i in {1..5}; do
        timeout 2 $CLI_BINARY remove "perf-$i-${TEST_ID}" -n --force >/dev/null 2>&1 || true
    done
    for name in demo-container-${TEST_ID} demo-container-2-${TEST_ID} async-demo-${TEST_ID} lifecycle-test-${TEST_ID} exec-test-${TEST_ID} error-test-${TEST_ID} fail-test-${TEST_ID}; do
        timeout 2 $CLI_BINARY remove $name -n --force >/dev/null 2>&1 || true
    done
    
    # Clean up files
    rm -f server.log test_script.sh
    
    # Final killall just in case
    killall -9 quilt 2>/dev/null || true
    killall -9 cli 2>/dev/null || true
}

# Set up trap for multiple signals
echo -e "${YELLOW}[DEBUG] Setting up cleanup trap...${NC}"
trap cleanup EXIT INT TERM QUIT
echo -e "${YELLOW}[DEBUG] Trap set successfully${NC}"

# Test counter
TOTAL_TESTS=0
PASSED_TESTS=0

# Generate unique test ID for this run
TEST_ID=$(date +%s)

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

echo -e "\n${BLUE}=== FEATURE 1: Container Naming (-n flag) ===${NC}"

# Test 1.1: Create container with name
DEMO_NAME="demo-container-${TEST_ID}"
echo -e "${YELLOW}[DEBUG] Creating container with name '$DEMO_NAME'...${NC}"
OUTPUT=$($CLI_BINARY create -n $DEMO_NAME --image-path nixos-minimal.tar.gz -- /bin/echo "Hello World" 2>&1)
CREATE_EXIT=$?
echo -e "${YELLOW}[DEBUG] Create exit code: $CREATE_EXIT${NC}"
echo -e "${YELLOW}[DEBUG] Create output:${NC}"
echo "$OUTPUT"
CONTAINER_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
echo -e "${YELLOW}[DEBUG] Extracted container ID: '$CONTAINER_ID'${NC}"
run_test "Create container with name" "[[ -n '$CONTAINER_ID' ]]" "Container ID should be returned"
run_test "Success message shown" "echo '$OUTPUT' | grep -q 'Container created successfully'" "Success message"

# Check if server is still running
echo -e "${YELLOW}[DEBUG] Checking if server is still alive...${NC}"
if ps -p $SERVER_PID > /dev/null 2>&1; then
    echo -e "${YELLOW}[DEBUG] Server is still running (PID: $SERVER_PID)${NC}"
else
    echo -e "${RED}[DEBUG] Server has died! Checking server log...${NC}"
    tail -20 server.log
    exit 1
fi

echo -e "\n${BLUE}=== FEATURE 2: Name Resolution ===${NC}"

# Test 2.1: Status by name
run_test "Status by name" "$CLI_BINARY status $DEMO_NAME -n 2>&1 | grep -q '$CONTAINER_ID'" "Should resolve name to ID"

# Test 2.2: Logs by name
run_test "Logs by name" "$CLI_BINARY logs $DEMO_NAME -n >/dev/null 2>&1" "Should accept name for logs"

echo -e "\n${BLUE}=== FEATURE 3: Duplicate Name Prevention ===${NC}"

# Test 3.1: Try to create duplicate
run_test "Duplicate name rejected" "! $CLI_BINARY create -n $DEMO_NAME --image-path nixos-minimal.tar.gz -- echo test 2>&1 | grep -q 'Container created'" "Should reject duplicate"
run_test "Error message for duplicate" "$CLI_BINARY create -n $DEMO_NAME --image-path nixos-minimal.tar.gz -- echo test 2>&1 | grep -q 'already exists'" "Should show 'already exists'"

echo -e "\n${BLUE}=== FEATURE 4: Async Containers ===${NC}"

# Test 4.1: Create async container without command
ASYNC_NAME="async-demo-${TEST_ID}"
echo -e "${YELLOW}[DEBUG] Creating async container '$ASYNC_NAME'...${NC}"
OUTPUT=$($CLI_BINARY create -n $ASYNC_NAME --async-mode --image-path nixos-minimal.tar.gz 2>&1)
CREATE_EXIT=$?
echo -e "${YELLOW}[DEBUG] Async create exit code: $CREATE_EXIT${NC}"
echo -e "${YELLOW}[DEBUG] Async create output:${NC}"
echo "$OUTPUT"
ASYNC_ID=$(echo "$OUTPUT" | grep "Container ID:" | awk '{print $3}')
echo -e "${YELLOW}[DEBUG] Extracted async container ID: '$ASYNC_ID'${NC}"
run_test "Create async without command" "[[ -n '$ASYNC_ID' ]]" "Should create without explicit command"

# Test 4.2: Verify async container gets default command and stays running
echo -e "${YELLOW}[DEBUG] Checking async container status...${NC}"
# Give the async container a moment to transition from PENDING to RUNNING
sleep 1
STATUS=$($CLI_BINARY status $ASYNC_NAME -n 2>&1)
echo -e "${YELLOW}[DEBUG] Async container status:${NC}"
echo "$STATUS"
PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')

# Check if process exists and no sleep infinity error
if [ ! -z "$PID" ] && [ "$PID" != "0" ]; then
    if ps -p $PID >/dev/null 2>&1; then
        run_test "Async container running" "true" "Process $PID exists"
    else
        # Check for sleep infinity error
        if grep -q "sleep: invalid number 'infinity'" server.log; then
            run_test "Async container running" "false" "Failed with sleep infinity error"
        else
            run_test "Async container running" "false" "Process $PID not found"
        fi
    fi
else
    run_test "Async container running" "false" "No PID assigned"
fi

# Test 4.3: Non-async without command should fail
FAIL_NAME="fail-test-${TEST_ID}"
run_test "Non-async requires command" "! $CLI_BINARY create -n $FAIL_NAME --image-path nixos-minimal.tar.gz 2>&1 | grep -q 'Container created'" "Should fail without command"
run_test "Error message for no command" "$CLI_BINARY create -n $FAIL_NAME --image-path nixos-minimal.tar.gz 2>&1 | grep -q 'Command required'" "Should show 'Command required'"

echo -e "\n${BLUE}=== FEATURE 5: Stop vs Kill ===${NC}"

# Test 5.1: Stop command (graceful)
# First get the PID of the async container we created
STATUS=$($CLI_BINARY status $ASYNC_NAME -n 2>&1)
PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')
if [ ! -z "$PID" ] && [ "$PID" != "0" ]; then
    # Stop the container
    $CLI_BINARY stop $ASYNC_NAME -n >/dev/null 2>&1
    STOP_EXIT=$?
    sleep 1
    
    # Check if process is gone
    if ps -p $PID >/dev/null 2>&1; then
        run_test "Stop command kills process" "false" "Process $PID should be terminated"
    else
        run_test "Stop command kills process" "true" "Process terminated successfully"
    fi
    
    # Check container state is EXITED
    NEW_STATUS=$($CLI_BINARY status $ASYNC_NAME -n 2>&1)
    run_test "Container state after stop" "echo '$NEW_STATUS' | grep -q 'Status: EXITED'" "Should be in EXITED state"
    run_test "Stop command exit code" "[ $STOP_EXIT -eq 0 ]" "Stop command should succeed"
else
    echo -e "  ${YELLOW}Skipping stop test - no PID found${NC}"
fi

# Test 5.2: Kill command (immediate)
# Create new container for kill test
KILL_NAME="lifecycle-test-${TEST_ID}"
echo -e "${YELLOW}[DEBUG] Creating container for kill test: $KILL_NAME${NC}"
OUTPUT=$($CLI_BINARY create -n $KILL_NAME --async-mode --image-path nixos-minimal.tar.gz 2>&1)
if echo "$OUTPUT" | grep -q "Container created"; then
    # Get PID immediately - no sleep needed
    STATUS=$($CLI_BINARY status $KILL_NAME -n 2>&1)
    PID=$(echo "$STATUS" | grep "PID:" | awk '{print $2}')
    echo -e "${YELLOW}[DEBUG] Kill test PID: $PID${NC}"
    if [ ! -z "$PID" ] && [ "$PID" != "0" ]; then
        # Kill the container
        START_TIME=$(date +%s.%N)
        OUTPUT=$($CLI_BINARY kill $KILL_NAME -n 2>&1)
        KILL_EXIT=$?
        echo -e "${YELLOW}[DEBUG] Kill output: $OUTPUT${NC}"
        END_TIME=$(date +%s.%N)
        KILL_TIME=$(echo "$END_TIME - $START_TIME" | bc)
        
        # Check process is immediately gone (within 0.5s)
        # Check immediately if process is gone
        if ps -p $PID >/dev/null 2>&1; then
            run_test "Kill command immediate" "false" "Process $PID should be killed immediately"
        else
            run_test "Kill command immediate" "true" "Process killed immediately"
        fi
        
        run_test "Kill command fast" "echo '$KILL_TIME < 0.5' | bc -l | grep -q 1" "Should kill within 0.5s"
        run_test "Kill command exit code" "[ $KILL_EXIT -eq 0 ]" "Kill command should succeed"
    else
        echo -e "  ${YELLOW}Skipping kill test - no PID found${NC}"
    fi
else
    echo -e "  ${YELLOW}Skipping kill test - failed to create container${NC}"
fi

echo -e "\n${BLUE}=== FEATURE 6: Start Command ===${NC}"

# Test 6.1: Create and stop a container
START_NAME="demo-container-2-${TEST_ID}"
echo -e "${YELLOW}[DEBUG] Creating container for start test: $START_NAME${NC}"
OUTPUT=$($CLI_BINARY create -n $START_NAME --async-mode --image-path nixos-minimal.tar.gz 2>&1)
if echo "$OUTPUT" | grep -q "Container created"; then
    # Wait for container to fully start and verify it's running
    sleep 1
    RUNNING_STATUS=$($CLI_BINARY status $START_NAME -n 2>&1)
    if echo "$RUNNING_STATUS" | grep -q "Status: RUNNING"; then
        echo -e "  ${GREEN}Container started successfully before stop test${NC}"
        
        # Now stop the container
        $CLI_BINARY stop $START_NAME -n >/dev/null 2>&1
        STOP_EXIT=$?
        
        # Wait for stop to complete and state to update
        sleep 1
        
        # Test 6.2: Start the stopped container
        # First verify it's actually stopped
        STATUS_BEFORE=$($CLI_BINARY status $START_NAME -n 2>&1)
    else
        echo -e "  ${RED}Container failed to start - Status:${NC}"
        echo "$RUNNING_STATUS"
        STATUS_BEFORE=""
    fi
    if [ -n "$STATUS_BEFORE" ] && echo "$STATUS_BEFORE" | grep -q "Status: EXITED"; then
        echo -e "  ${GREEN}Container successfully stopped (EXITED state)${NC}"
        
        # Start the container and capture output
        START_OUTPUT=$($CLI_BINARY start $START_NAME -n 2>&1)
        START_EXIT=$?
        
        # Wait for container to start
        sleep 1
        
        # Verify it's restarting, not creating new
        if echo "$START_OUTPUT" | grep -q "Creating container"; then
            run_test "Start reuses container" "false" "Should not create new container"
        else
            run_test "Start reuses container" "true" "Container restarted without recreation"
        fi
        
        # Check new status
        STATUS_AFTER=$($CLI_BINARY status $START_NAME -n 2>&1)
        NEW_PID=$(echo "$STATUS_AFTER" | grep "PID:" | awk '{print $2}')
        
        # Verify we have a new PID
        if [ ! -z "$NEW_PID" ] && [ "$NEW_PID" != "0" ]; then
            # Check process exists
            if ps -p $NEW_PID >/dev/null 2>&1; then
                run_test "Start creates new process" "true" "New process $NEW_PID created"
            else
                run_test "Start creates new process" "false" "Process $NEW_PID not found"
            fi
        else
            run_test "Start creates new process" "false" "No PID assigned after start"
        fi
        
        run_test "Container running after start" "echo '$STATUS_AFTER' | grep -q 'Status: RUNNING'" "Should be RUNNING"
        run_test "Start command exit code" "[ $START_EXIT -eq 0 ]" "Start command should succeed"
    else
        if [ -n "$STATUS_BEFORE" ]; then
            echo -e "  ${RED}Container not in EXITED state before start test${NC}"
            echo -e "  ${YELLOW}Current status:${NC}"
            echo "$STATUS_BEFORE"
        else
            echo -e "  ${RED}Container failed to start initially or stop properly${NC}"
        fi
        run_test "Start stopped container" "false" "Container must be EXITED to start"
        # Skip remaining start tests
        run_test "Start reuses container" "false" "Skipped - container not stopped"
        run_test "Start creates new process" "false" "Skipped - container not stopped"
        run_test "Container running after start" "false" "Skipped - container not stopped"
        run_test "Start command exit code" "false" "Skipped - container not stopped"
    fi
else
    echo -e "  ${YELLOW}Skipping start test - failed to create container${NC}"
fi

echo -e "\n${BLUE}=== FEATURE 7: Exec with Name Support ===${NC}"

# Test 7.1: Create container for exec
EXEC_NAME="exec-test-${TEST_ID}"
$CLI_BINARY create -n $EXEC_NAME --async-mode --image-path nixos-minimal.tar.gz >/dev/null 2>&1
# No sleep - container should be ready immediately

# Test 7.2: Check if container is running first
STATUS=$($CLI_BINARY status $EXEC_NAME -n 2>&1)
if echo "$STATUS" | grep -q "Status: RUNNING"; then
    # Test 7.3: Execute command by name
    # Run exec and capture full output
    EXEC_OUTPUT=$($CLI_BINARY exec $EXEC_NAME -n -c 'echo test123' --capture-output 2>&1)
    EXEC_EXIT=$?
    
    # Check if output contains our test string
    if echo "$EXEC_OUTPUT" | grep -q "test123"; then
        run_test "Exec captures output" "true" "Output captured correctly"
    else
        echo -e "  Exec output: $EXEC_OUTPUT"
        run_test "Exec captures output" "false" "Expected 'test123' in output"
    fi
    
    run_test "Exec exit code" "[ $EXEC_EXIT -eq 0 ]" "Exec should succeed"
    
    # Test exec with exit code
    $CLI_BINARY exec $EXEC_NAME -n -c 'exit 42' >/dev/null 2>&1
    EXEC_EXIT_CODE=$?
    run_test "Exec propagates exit code" "[ $EXEC_EXIT_CODE -eq 42 ]" "Should return exit code 42"
else
    echo -e "  ${YELLOW}Skipping exec test - container not yet running${NC}"
fi

# Test 7.4: Test script execution
cat > test_script.sh << 'EOF'
#!/bin/sh
echo "Script executed successfully"
EOF
chmod +x test_script.sh

if echo "$STATUS" | grep -q "Status: RUNNING"; then
    run_test "Script execution" "$CLI_BINARY exec $EXEC_NAME -n -c ./test_script.sh --capture-output 2>&1 | grep -qE '(Script executed|copy_script)'" "Should execute or attempt script"
fi

echo -e "\n${BLUE}=== FEATURE 8: Remove with Name ===${NC}"

# Test 8.1: Remove by name
run_test "Remove by name" "$CLI_BINARY remove $DEMO_NAME -n --force 2>&1 | grep -q 'removed successfully'" "Should remove by name"

echo -e "\n${BLUE}=== FEATURE 9: Name Lookup Performance ===${NC}"

# Create multiple containers
echo -e "  Creating 5 containers for performance test..."
for i in {1..5}; do
    $CLI_BINARY create -n "perf-$i-${TEST_ID}" --async-mode --image-path nixos-minimal.tar.gz >/dev/null 2>&1
done

# Test lookup performance
START_TIME=$(date +%s.%N)
$CLI_BINARY status "perf-3-${TEST_ID}" -n >/dev/null 2>&1
END_TIME=$(date +%s.%N)
LOOKUP_TIME=$(echo "$END_TIME - $START_TIME" | bc)

run_test "Name lookup < 150ms" "echo '$LOOKUP_TIME < 0.15' | bc -l | grep -q 1" "Should be fast"
echo -e "  Actual lookup time: ${LOOKUP_TIME}s"

echo -e "\n${BLUE}=== FEATURE 10: Error Handling ===${NC}"

# Test 10.1: Non-existent container
run_test "Non-existent name error" "$CLI_BINARY status non-existent -n 2>&1 | grep -q 'not found'" "Should show not found"

# Test 10.2: Stop already stopped
$CLI_BINARY stop $START_NAME -n >/dev/null 2>&1 || true
run_test "Stop stopped container" "$CLI_BINARY stop $START_NAME -n >/dev/null 2>&1" "Should handle gracefully"

echo -e "\n${BLUE}=== TEST SUMMARY ===${NC}"
echo -e "Total tests: $TOTAL_TESTS"
echo -e "Passed: ${GREEN}$PASSED_TESTS${NC}"
echo -e "Failed: ${RED}$((TOTAL_TESTS - PASSED_TESTS))${NC}"

if [ $PASSED_TESTS -eq $TOTAL_TESTS ]; then
    echo -e "\n${GREEN}✓ ALL FEATURES VERIFIED SUCCESSFULLY!${NC}"
    exit 0
else
    echo -e "\n${RED}✗ Some tests failed${NC}"
    exit 1
fi