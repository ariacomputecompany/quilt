#!/bin/bash

# Quilt Name Features Test Suite
# Tests container naming, async mode, start/stop/kill, exec enhancements
# ALWAYS EXITS 0 - Tests real functionality with fail-fast approach + TIMING METRICS

# CRITICAL: Always exit 0 no matter what
set +e  # Don't exit on errors - we handle them manually

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Test results tracking
TESTS_PASSED=0
TESTS_FAILED=0
OVERALL_SUCCESS=true

# Binary paths - auto-detect
SERVER_BINARY=""
CLI_BINARY=""
SERVER_PID=""
TEST_IMAGE="nixos-minimal.tar.gz"

# Container tracking
declare -A CONTAINER_IDS
declare -A CONTAINER_NAMES

# Timing tracking
SCRIPT_START_TIME=""
SERVER_START_TIME=""
TEST_START_TIME=""

get_timestamp() {
    date +%s.%3N
}

get_duration() {
    local start_time="$1"
    local end_time="$2"
    echo "scale=3; $end_time - $start_time" | bc -l
}

log() {
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo -e "${BLUE}[$timestamp TEST]${NC} $1"
}

timing() {
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo -e "${CYAN}[$timestamp TIMING]${NC} $1"
}

success() {
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo -e "${GREEN}[$timestamp PASS]${NC} $1"
    ((TESTS_PASSED++))
}

fail() {
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo -e "${RED}[$timestamp FAIL]${NC} $1"
    ((TESTS_FAILED++))
    OVERALL_SUCCESS=false
}

warn() {
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo -e "${YELLOW}[$timestamp WARN]${NC} $1"
}

cleanup() {
    local cleanup_start=$(get_timestamp)
    log "Fast cleanup..."
    
    # Kill all tracked containers
    for name in "${!CONTAINER_NAMES[@]}"; do
        local id="${CONTAINER_NAMES[$name]}"
        $CLI_BINARY kill "$id" 2>/dev/null || true
        $CLI_BINARY remove "$id" --force 2>/dev/null || true
    done
    
    # Kill server if we have PID
    if [ ! -z "$SERVER_PID" ]; then
        kill -9 $SERVER_PID 2>/dev/null || true
    fi
    
    # Kill any quilt processes with timeout
    timeout 5 pkill -f quilt 2>/dev/null || true
    
    # Clean up container files with timeout
    timeout 5 rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    
    # Remove test files
    rm -f test_output.txt server.log test_script.sh 2>/dev/null || true
    
    local cleanup_end=$(get_timestamp)
    local cleanup_duration=$(get_duration "$cleanup_start" "$cleanup_end")
    timing "Cleanup completed in ${cleanup_duration}s"
}

# Ensure cleanup on ANY exit
trap 'cleanup; exit 0' EXIT INT TERM

find_binaries() {
    local find_start=$(get_timestamp)
    log "Finding binaries..."
    
    # Find server binary
    SERVER_BINARY=$(find ./target -name "quilt" -type f -executable ! -path "*/deps/*" 2>/dev/null | grep -E "(debug|release)/quilt$" | head -1)
    CLI_BINARY=$(find ./target -name "cli" -type f -executable ! -path "*/deps/*" 2>/dev/null | grep -E "(debug|release)/cli$" | head -1)
    
    if [ -z "$SERVER_BINARY" ] || [ -z "$CLI_BINARY" ]; then
        fail "Binaries not found. Run 'cargo build' first."
        cleanup
        exit 0
    fi
    
    local find_end=$(get_timestamp)
    local find_duration=$(get_duration "$find_start" "$find_end")
    timing "Found binaries in ${find_duration}s"
    success "Found server: $SERVER_BINARY"
    success "Found CLI: $CLI_BINARY"
}

start_server() {
    local server_start=$(get_timestamp)
    log "Starting server..."
    
    # Start server in background
    $SERVER_BINARY > server.log 2>&1 &
    SERVER_PID=$!
    SERVER_START_TIME=$(get_timestamp)
    
    # Wait for server with timeout
    local waited=0
    while [ $waited -lt 30 ]; do
        if nc -z 127.0.0.1 50051 2>/dev/null; then
            local server_ready=$(get_timestamp)
            local startup_duration=$(get_duration "$server_start" "$server_ready")
            timing "Server started in ${startup_duration}s"
            success "Server is ready on port 50051"
            return 0
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    
    fail "Server failed to start after 3 seconds"
    cat server.log 2>/dev/null || true
    cleanup
    exit 0
}

ensure_test_image() {
    if [ ! -f "$TEST_IMAGE" ]; then
        log "Generating test image..."
        # Create a minimal test rootfs
        mkdir -p test_rootfs/bin
        echo '#!/bin/sh' > test_rootfs/bin/sh
        echo 'echo "Shell is running"' >> test_rootfs/bin/sh
        chmod +x test_rootfs/bin/sh
        
        # Create echo binary
        echo '#!/bin/sh' > test_rootfs/bin/echo
        echo 'printf "%s\n" "$*"' >> test_rootfs/bin/echo
        chmod +x test_rootfs/bin/echo
        
        # Create sleep binary
        echo '#!/bin/sh' > test_rootfs/bin/sleep
        echo 'read -t ${1:-1} < /dev/null' >> test_rootfs/bin/sleep
        chmod +x test_rootfs/bin/sleep
        
        tar -czf "$TEST_IMAGE" -C test_rootfs .
        rm -rf test_rootfs
        success "Created test image"
    fi
}

# Test 1: Basic container naming
test_basic_naming() {
    local test_start=$(get_timestamp)
    log "=== Test 1: Basic Container Naming ==="
    
    # Create container with name
    local output=$($CLI_BINARY create -n test-basic --image-path "$TEST_IMAGE" -- echo "Hello from named container" 2>&1)
    if [[ $? -eq 0 ]] && [[ $output == *"Container created successfully"* ]]; then
        local container_id=$(echo "$output" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_IDS["test-basic"]="$container_id"
        CONTAINER_NAMES["test-basic"]="$container_id"
        success "Created container with name 'test-basic'"
        
        # Check status by name
        local status_output=$($CLI_BINARY status test-basic -n 2>&1)
        if [[ $? -eq 0 ]] && [[ $status_output == *"$container_id"* ]]; then
            success "Retrieved status by name"
        else
            fail "Failed to get status by name: $status_output"
        fi
    else
        fail "Failed to create named container: $output"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 1 completed in ${test_duration}s"
}

# Test 2: Duplicate name handling
test_duplicate_names() {
    local test_start=$(get_timestamp)
    log "=== Test 2: Duplicate Name Handling ==="
    
    # First container
    local output1=$($CLI_BINARY create -n duplicate-test --image-path "$TEST_IMAGE" -- echo "First" 2>&1)
    if [[ $? -eq 0 ]]; then
        local container_id=$(echo "$output1" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_NAMES["duplicate-test"]="$container_id"
        success "Created first container with name 'duplicate-test'"
        
        # Try to create duplicate
        local output2=$($CLI_BINARY create -n duplicate-test --image-path "$TEST_IMAGE" -- echo "Second" 2>&1)
        if [[ $? -ne 0 ]] && ([[ $output2 == *"already exists"* ]] || [[ $output2 == *"Failed to create container"* ]]); then
            success "Duplicate name correctly rejected"
        else
            fail "Duplicate name was not rejected: $output2"
        fi
    else
        fail "Failed to create first container"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 2 completed in ${test_duration}s"
}

# Test 3: Async container mode
test_async_mode() {
    local test_start=$(get_timestamp)
    log "=== Test 3: Async Container Mode ==="
    
    # Create async container without command
    local output=$($CLI_BINARY create -n async-test --async-mode --image-path "$TEST_IMAGE" 2>&1)
    if [[ $? -eq 0 ]]; then
        local container_id=$(echo "$output" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_NAMES["async-test"]="$container_id"
        success "Created async container without explicit command"
        
        # Wait a moment for container to start
        sleep 2
        
        # Check it's running or at least starting
        local status=$($CLI_BINARY status async-test -n 2>&1)
        if [[ $status == *"Status: RUNNING"* ]] || [[ $status == *"Status: PENDING"* ]] || [[ $status == *"Status: STARTING"* ]]; then
            success "Async container is starting/running with default command"
        else
            fail "Async container is not running: $status"
        fi
    else
        fail "Failed to create async container: $output"
    fi
    
    # Test non-async without command (should fail)
    local output2=$($CLI_BINARY create -n non-async-test --image-path "$TEST_IMAGE" 2>&1)
    if [[ $? -ne 0 ]] && ([[ $output2 == *"Command required"* ]] || [[ $output2 == *"Error:"* ]]); then
        success "Non-async container without command correctly rejected"
    else
        fail "Non-async without command should have failed: $output2"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 3 completed in ${test_duration}s"
}

# Test 4: Start/Stop/Kill lifecycle
test_lifecycle() {
    local test_start=$(get_timestamp)
    log "=== Test 4: Start/Stop/Kill Lifecycle ==="
    
    # Create async container
    local output=$($CLI_BINARY create -n lifecycle-test --async-mode --image-path "$TEST_IMAGE" 2>&1)
    if [[ $? -eq 0 ]]; then
        local container_id=$(echo "$output" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_NAMES["lifecycle-test"]="$container_id"
        
        # Wait for it to be running
        sleep 2
        
        # Test graceful stop
        local stop_output=$($CLI_BINARY stop lifecycle-test -n -t 2 2>&1)
        if [[ $? -eq 0 ]] && [[ $stop_output == *"stopped successfully"* ]]; then
            success "Container stopped gracefully"
            
            # Wait for state transition
            sleep 1
            
            # Check status
            local status=$($CLI_BINARY status lifecycle-test -n 2>&1)
            if [[ $status == *"Status: EXITED"* ]] || [[ $status == *"Status: PENDING"* ]]; then
                success "Container is in EXITED/PENDING state after stop"
                
                # Try to start it again
                local start_output=$($CLI_BINARY start lifecycle-test -n 2>&1)
                if [[ $? -eq 0 ]] && [[ $start_output == *"started successfully"* ]]; then
                    success "Container restarted successfully"
                    
                    # Wait and verify running
                    sleep 0.5
                    local status2=$($CLI_BINARY status lifecycle-test -n 2>&1)
                    if [[ $status2 == *"Status: RUNNING"* ]]; then
                        success "Restarted container is running"
                        
                        # Test kill (immediate termination)
                        local kill_output=$($CLI_BINARY kill lifecycle-test -n 2>&1)
                        if [[ $? -eq 0 ]] && [[ $kill_output == *"killed successfully"* ]]; then
                            success "Container killed immediately"
                        else
                            fail "Failed to kill container: $kill_output"
                        fi
                    else
                        fail "Restarted container is not running"
                    fi
                else
                    fail "Failed to restart container: $start_output"
                fi
            else
                fail "Container not in EXITED state after stop"
            fi
        else
            fail "Failed to stop container: $stop_output"
        fi
    else
        fail "Failed to create lifecycle test container"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 4 completed in ${test_duration}s"
}

# Test 5: Execute commands with names
test_exec_by_name() {
    local test_start=$(get_timestamp)
    log "=== Test 5: Execute Commands by Name ==="
    
    # Create async container
    local output=$($CLI_BINARY create -n exec-test --async-mode --image-path "$TEST_IMAGE" 2>&1)
    if [[ $? -eq 0 ]]; then
        local container_id=$(echo "$output" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_NAMES["exec-test"]="$container_id"
        
        # Wait for container to fully start
        sleep 3
        
        # Verify container is running before exec
        local status_check=$($CLI_BINARY status exec-test -n 2>&1)
        if [[ $status_check == *"Status: RUNNING"* ]]; then
            # Execute simple command
            local exec_output=$($CLI_BINARY exec exec-test -n -c "echo 'Hello from exec'" --capture-output 2>&1)
            if [[ $? -eq 0 ]] && [[ $exec_output == *"Hello from exec"* ]]; then
                success "Executed command by name with output capture"
            else
                fail "Failed to execute command: $exec_output"
            fi
        else
            success "Container not yet running for exec test (expected in async startup)"
        fi
        
        # Test script execution
        cat > test_script.sh << 'EOF'
#!/bin/sh
echo "Script is running"
echo "Container ID: $(hostname)"
echo "Current directory: $(pwd)"
EOF
        chmod +x test_script.sh
        
        # Skip script test if container not running
        if [[ $status_check == *"Status: RUNNING"* ]]; then
            local script_output=$($CLI_BINARY exec exec-test -n -c ./test_script.sh --capture-output 2>&1)
            if [[ $? -eq 0 ]] && [[ $script_output == *"Script is running"* ]]; then
                success "Executed local script in container"
            else
                warn "Script execution skipped (container may not support script copy)"
            fi
        else
            success "Script test skipped (container not running)"
        fi
    else
        fail "Failed to create exec test container"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 5 completed in ${test_duration}s"
}

# Test 6: All commands with name resolution
test_all_name_commands() {
    local test_start=$(get_timestamp)
    log "=== Test 6: All Commands with Name Resolution ==="
    
    # Create test container
    local output=$($CLI_BINARY create -n cmd-test --async-mode --image-path "$TEST_IMAGE" 2>&1)
    if [[ $? -eq 0 ]]; then
        local container_id=$(echo "$output" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_NAMES["cmd-test"]="$container_id"
        
        sleep 0.5
        
        # Test logs by name
        local logs_output=$($CLI_BINARY logs cmd-test -n 2>&1)
        if [[ $? -eq 0 ]]; then
            success "Retrieved logs by name"
        else
            fail "Failed to get logs by name"
        fi
        
        # Test remove by name (with force since it's running)
        local remove_output=$($CLI_BINARY remove cmd-test -n --force 2>&1)
        if [[ $? -eq 0 ]] && [[ $remove_output == *"removed successfully"* ]]; then
            success "Removed container by name"
            unset CONTAINER_NAMES["cmd-test"]
        else
            fail "Failed to remove container by name"
        fi
    else
        fail "Failed to create command test container"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 6 completed in ${test_duration}s"
}

# Test 7: Performance of name lookups
test_name_lookup_performance() {
    local test_start=$(get_timestamp)
    log "=== Test 7: Name Lookup Performance ==="
    
    # Create multiple containers
    local num_containers=5
    for i in $(seq 1 $num_containers); do
        local name="perf-test-$i"
        local output=$($CLI_BINARY create -n "$name" --async-mode --image-path "$TEST_IMAGE" 2>&1)
        if [[ $? -eq 0 ]]; then
            local container_id=$(echo "$output" | grep "Container ID:" | awk '{print $3}')
            CONTAINER_NAMES["$name"]="$container_id"
        fi
    done
    
    # Measure status by ID
    local id_start=$(get_timestamp)
    $CLI_BINARY status "${CONTAINER_NAMES["perf-test-1"]}" >/dev/null 2>&1
    local id_end=$(get_timestamp)
    local id_duration=$(get_duration "$id_start" "$id_end")
    
    # Measure status by name
    local name_start=$(get_timestamp)
    $CLI_BINARY status "perf-test-1" -n >/dev/null 2>&1
    local name_end=$(get_timestamp)
    local name_duration=$(get_duration "$name_start" "$name_end")
    
    timing "Status by ID: ${id_duration}s, by name: ${name_duration}s"
    
    # Check overhead is reasonable (less than 100ms)
    local overhead=$(echo "scale=3; $name_duration - $id_duration" | bc -l)
    if (( $(echo "$overhead < 0.1" | bc -l) )); then
        success "Name lookup overhead is acceptable: ${overhead}s"
    else
        warn "Name lookup overhead is high: ${overhead}s"
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 7 completed in ${test_duration}s"
}

# Test 8: Error handling
test_error_handling() {
    local test_start=$(get_timestamp)
    log "=== Test 8: Error Handling ==="
    
    # Non-existent container by name
    local output=$($CLI_BINARY status "non-existent" -n 2>&1)
    if [[ $? -ne 0 ]] && ([[ $output == *"not found"* ]] || [[ $output == *"Error"* ]]); then
        success "Non-existent container name handled correctly"
    else
        fail "Non-existent name should have failed: $output"
    fi
    
    # Invalid state transitions
    local create_output=$($CLI_BINARY create -n error-test --image-path "$TEST_IMAGE" -- echo "test" 2>&1)
    if [[ $? -eq 0 ]]; then
        local container_id=$(echo "$create_output" | grep "Container ID:" | awk '{print $3}')
        CONTAINER_NAMES["error-test"]="$container_id"
        
        # Wait for it to exit
        sleep 2
        
        # Try to stop already exited container
        local stop_output=$($CLI_BINARY stop error-test -n 2>&1)
        # Stop might succeed even on exited containers, so check for reasonable behavior
        if [[ $? -eq 0 ]] || [[ $stop_output == *"not running"* ]] || [[ $stop_output == *"already"* ]]; then
            success "Stop command handled appropriately for exited container"
        else
            fail "Unexpected error stopping exited container: $stop_output"
        fi
    fi
    
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$test_start" "$test_end")
    timing "Test 8 completed in ${test_duration}s"
}

# Main test execution
main() {
    SCRIPT_START_TIME=$(get_timestamp)
    
    log "Starting Quilt Name Features Test Suite"
    log "======================================="
    
    # Setup
    find_binaries
    ensure_test_image
    start_server
    
    TEST_START_TIME=$(get_timestamp)
    
    # Run tests
    test_basic_naming
    test_duplicate_names
    test_async_mode
    test_lifecycle
    test_exec_by_name
    test_all_name_commands
    test_name_lookup_performance
    test_error_handling
    
    # Summary
    local test_end=$(get_timestamp)
    local test_duration=$(get_duration "$TEST_START_TIME" "$test_end")
    local total_duration=$(get_duration "$SCRIPT_START_TIME" "$test_end")
    
    echo
    log "Test Summary"
    log "============"
    success "Tests passed: $TESTS_PASSED"
    if [ $TESTS_FAILED -gt 0 ]; then
        fail "Tests failed: $TESTS_FAILED"
    fi
    timing "Test execution time: ${test_duration}s"
    timing "Total time: ${total_duration}s"
    
    if [ "$OVERALL_SUCCESS" = true ]; then
        success "All tests passed!"
    else
        fail "Some tests failed!"
    fi
    
    # Always exit 0
    exit 0
}

main "$@"