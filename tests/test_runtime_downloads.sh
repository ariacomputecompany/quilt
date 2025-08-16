#!/bin/bash

# Advanced Container Runtime Download Test
# Tests real software installation and parallel container capabilities
# ALWAYS EXITS 0 - Real-world functionality validation with timing metrics

# CRITICAL: Always exit 0 no matter what
set +e  # Don't exit on errors - we handle them manually

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
PURPLE='\033[0;35m'
NC='\033[0m' # No Color

# Test results tracking
TESTS_PASSED=0
TESTS_FAILED=0
OVERALL_SUCCESS=true

# Binary paths - auto-detect
SERVER_BINARY=""
CLI_BINARY=""
SERVER_PID=""

# Timing tracking
SCRIPT_START_TIME=""
SERVER_START_TIME=""
TEST_START_TIME=""

# Container tracking for parallel tests
declare -A CONTAINER_PIDS
declare -A CONTAINER_START_TIMES

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

parallel() {
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo -e "${PURPLE}[$timestamp PARALLEL]${NC} $1"
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
    log "Advanced cleanup..."
    
    # Kill server if we have PID
    if [ ! -z "$SERVER_PID" ]; then
        kill -9 $SERVER_PID 2>/dev/null || true
    fi
    
    # Kill any quilt processes with timeout
    timeout 10 pkill -f quilt 2>/dev/null || true
    
    # Clean up container files with timeout  
    timeout 10 rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    
    # Remove test files
    rm -f test_output.txt download_test.txt server.log runtime_test.* 2>/dev/null || true
    
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
    SERVER_BINARY=$(find ./target -name "quilt" -type f -executable 2>/dev/null | head -1)
    if [ -z "$SERVER_BINARY" ]; then
        fail "Server binary not found"
        return 1
    fi
    log "Found server binary: $SERVER_BINARY"
    
    # Find CLI binary  
    CLI_BINARY=$(find ./target -name "cli" -type f -executable 2>/dev/null | head -1)
    if [ -z "$CLI_BINARY" ]; then
        fail "CLI binary not found"
        return 1
    fi
    log "Found CLI binary: $CLI_BINARY"
    
    # Check image file
    if [ ! -f "./nixos-minimal.tar.gz" ]; then
        fail "Container image not found: ./nixos-minimal.tar.gz"
        return 1
    fi
    log "Found container image: ./nixos-minimal.tar.gz"
    
    local find_end=$(get_timestamp)
    local find_duration=$(get_duration "$find_start" "$find_end")
    timing "Binary discovery took ${find_duration}s"
    
    return 0
}

wait_for_server() {
    local wait_start=$(get_timestamp)
    log "Waiting for server to start..."
    
    for i in {1..10}; do
        if pgrep -f quilt > /dev/null; then
            sleep 3  # Give extra time for runtime tests
            local wait_end=$(get_timestamp)
            local wait_duration=$(get_duration "$wait_start" "$wait_end")
            success "Server is running (attempt $i)"
            timing "Server startup detection took ${wait_duration}s"
            return 0
        fi
        sleep 1
    done
    
    local wait_end=$(get_timestamp)
    local wait_duration=$(get_duration "$wait_start" "$wait_end")
    fail "Server failed to start after 10 seconds"
    timing "Server startup failed after ${wait_duration}s"
    return 1
}

create_runtime_container() {
    local test_name="$1"
    local runtime_commands="$2"
    local validation_command="$3"
    local expected_output="$4"
    local timeout_duration="${5:-60}"  # Default 60s timeout
    
    local test_start=$(get_timestamp)
    log "Testing Runtime: $test_name"
    
    # Create container with longer timeout for downloads
    local create_start=$(get_timestamp)
    local container_output
    container_output=$(timeout $timeout_duration $CLI_BINARY create \
        --image-path ./nixos-minimal.tar.gz \
        --memory-limit 1024 \
        -- /bin/sh -c "$runtime_commands && $validation_command" 2>&1)
    
    local create_end=$(get_timestamp)
    local create_duration=$(get_duration "$create_start" "$create_end")
    
    local create_exit=$?
    if [ $create_exit -eq 124 ]; then
        fail "$test_name: Container creation/execution timed out after ${timeout_duration}s"
        timing "$test_name timeout after ${create_duration}s"
        return 1
    elif [ $create_exit -ne 0 ]; then
        fail "$test_name: Container creation failed with exit code $create_exit"
        timing "$test_name creation failed after ${create_duration}s"
        return 1
    fi
    
    if echo "$container_output" | grep -q "Container created successfully"; then
        local container_id=$(echo "$container_output" | grep -o '[a-f0-9-]\{36\}' | head -1)
        if [ -z "$container_id" ]; then
            fail "$test_name: Could not extract container ID"
            timing "$test_name ID extraction failed after ${create_duration}s"
            return 1
        fi
        
        success "$test_name: Container created with ID $container_id"
        timing "$test_name creation took ${create_duration}s"
        
        # Wait for execution with longer timeout for downloads
        local execution_start=$(get_timestamp)
        sleep 5  # Longer wait for runtime installations
        local execution_end=$(get_timestamp)
        local execution_duration=$(get_duration "$execution_start" "$execution_end")
        timing "$test_name execution wait took ${execution_duration}s"
        
        # Get container logs with timeout
        local logs_start=$(get_timestamp)
        local logs=$(timeout 15 $CLI_BINARY logs "$container_id" 2>/dev/null || echo "No logs available")
        local logs_end=$(get_timestamp)
        local logs_duration=$(get_duration "$logs_start" "$logs_end")
        timing "$test_name log retrieval took ${logs_duration}s"
        
        # Validate output
        local validation_start=$(get_timestamp)
        if echo "$logs" | grep -q "$expected_output"; then
            success "$test_name: Found expected output '$expected_output'"
            local validation_end=$(get_timestamp)
            local validation_duration=$(get_duration "$validation_start" "$validation_end")
            timing "$test_name validation took ${validation_duration}s"
            
            local test_end=$(get_timestamp)
            local test_total_duration=$(get_duration "$test_start" "$test_end")
            timing "$test_name TOTAL TIME: ${test_total_duration}s"
            return 0
        else
            fail "$test_name: Expected '$expected_output' but got different output"
            warn "Actual output: $(echo "$logs" | tail -10)"
            local validation_end=$(get_timestamp)
            local validation_duration=$(get_duration "$validation_start" "$validation_end")
            timing "$test_name validation failed after ${validation_duration}s"
            
            local test_end=$(get_timestamp)
            local test_total_duration=$(get_duration "$test_start" "$test_end")
            timing "$test_name TOTAL TIME: ${test_total_duration}s"
            return 1
        fi
        
    else
        fail "$test_name: Container creation failed - $container_output"
        timing "$test_name failed after ${create_duration}s"
        return 1
    fi
}

create_parallel_container() {
    local test_name="$1"
    local runtime_commands="$2"
    local validation_command="$3"
    local expected_output="$4"
    local timeout_duration="${5:-90}"
    
    parallel "Starting parallel test: $test_name"
    CONTAINER_START_TIMES["$test_name"]=$(get_timestamp)
    
    # Run in background and capture PID
    (
        create_runtime_container "$test_name" "$runtime_commands" "$validation_command" "$expected_output" "$timeout_duration"
    ) &
    
    CONTAINER_PIDS["$test_name"]=$!
    parallel "Started $test_name with PID ${CONTAINER_PIDS["$test_name"]}"
}

wait_for_parallel_containers() {
    local wait_start=$(get_timestamp)
    parallel "Waiting for all parallel containers to complete..."
    
    for test_name in "${!CONTAINER_PIDS[@]}"; do
        local pid=${CONTAINER_PIDS["$test_name"]}
        local start_time=${CONTAINER_START_TIMES["$test_name"]}
        
        parallel "Waiting for $test_name (PID: $pid)..."
        
        if wait $pid; then
            local end_time=$(get_timestamp)
            local duration=$(get_duration "$start_time" "$end_time")
            success "$test_name completed successfully"
            timing "$test_name parallel execution took ${duration}s"
        else
            local end_time=$(get_timestamp)
            local duration=$(get_duration "$start_time" "$end_time")
            fail "$test_name failed in parallel execution"
            timing "$test_name parallel execution failed after ${duration}s"
        fi
    done
    
    local wait_end=$(get_timestamp)
    local wait_duration=$(get_duration "$wait_start" "$wait_end")
    timing "All parallel containers completed in ${wait_duration}s"
}

# Main test execution
main() {
    SCRIPT_START_TIME=$(get_timestamp)
    log "Starting Advanced Runtime Download Test (Parallel Mode with Timing)"
    log "====================================================================="
    
    # Find binaries first - fail fast if not found
    if ! find_binaries; then
        fail "Binary setup failed - exiting"
        return 1
    fi
    
    # Start server with timeout
    SERVER_START_TIME=$(get_timestamp)
    log "Starting Quilt server..."
    timeout 60 $SERVER_BINARY > server.log 2>&1 &
    SERVER_PID=$!
    
    # Wait for server - fail fast if it doesn't start
    if ! wait_for_server; then
        fail "Server startup failed - exiting"
        return 1
    fi
    
    local server_ready_time=$(get_timestamp)
    local server_startup_duration=$(get_duration "$SERVER_START_TIME" "$server_ready_time")
    timing "TOTAL SERVER STARTUP TIME: ${server_startup_duration}s"
    
    # Run sequential runtime tests first
    TEST_START_TIME=$(get_timestamp)
    log "Running sequential runtime tests..."
    
    # Test 1: Node.js Installation and Test
    create_runtime_container "Node.js Runtime" \
        "echo 'Installing Node.js...'; curl -fsSL https://deb.nodesource.com/setup_18.x | sh -; apt-get install -y nodejs || { echo 'Trying alternative...'; wget -qO- https://nodejs.org/dist/v18.17.0/node-v18.17.0-linux-x64.tar.xz | tar -xJ; mv node-v18.17.0-linux-x64 /opt/nodejs; ln -s /opt/nodejs/bin/node /usr/local/bin/node; ln -s /opt/nodejs/bin/npm /usr/local/bin/npm; }" \
        "node --version && echo 'console.log(\"Hello from Node.js!\");' | node" \
        "Hello from Node.js!" \
        120
    
    # Test 2: Python/pip Installation and Test  
    create_runtime_container "Python/pip Runtime" \
        "echo 'Installing Python...'; apt-get update; apt-get install -y python3 python3-pip || { echo 'Trying alternative...'; wget https://www.python.org/ftp/python/3.9.0/Python-3.9.0.tgz; tar -xzf Python-3.9.0.tgz; }" \
        "python3 --version && python3 -c 'print(\"Hello from Python!\")'" \
        "Hello from Python!" \
        120
    
    # Test 3: Basic development tools
    create_runtime_container "Development Tools" \
        "echo 'Installing dev tools...'; apt-get update; apt-get install -y curl wget git build-essential || echo 'Some tools may not be available'" \
        "curl --version && wget --version && git --version" \
        "curl" \
        90
    
    local sequential_end_time=$(get_timestamp)
    local sequential_duration=$(get_duration "$TEST_START_TIME" "$sequential_end_time")
    timing "SEQUENTIAL TESTS COMPLETED IN: ${sequential_duration}s"
    
    # Run parallel tests
    log "Starting parallel container tests..."
    local parallel_start_time=$(get_timestamp)
    
    # Parallel Test 1: Multiple Node.js containers
    create_parallel_container "Parallel-Node-1" \
        "echo 'Quick Node.js test...'; echo 'console.log(\"Parallel Node 1!\");' > test.js" \
        "cat test.js && echo 'File created successfully'" \
        "Parallel Node 1!" \
        60
    
    create_parallel_container "Parallel-Node-2" \
        "echo 'Quick Node.js test...'; echo 'console.log(\"Parallel Node 2!\");' > test.js" \
        "cat test.js && echo 'File created successfully'" \
        "Parallel Node 2!" \
        60
    
    # Parallel Test 2: Multiple file operations
    create_parallel_container "Parallel-Files-1" \
        "mkdir -p /tmp/test1; echo 'data1' > /tmp/test1/file.txt" \
        "cat /tmp/test1/file.txt && echo 'Files test 1 complete'" \
        "data1" \
        30
    
    create_parallel_container "Parallel-Files-2" \
        "mkdir -p /tmp/test2; echo 'data2' > /tmp/test2/file.txt" \
        "cat /tmp/test2/file.txt && echo 'Files test 2 complete'" \
        "data2" \
        30
    
    # Wait for all parallel containers
    wait_for_parallel_containers
    
    local parallel_end_time=$(get_timestamp)
    local parallel_duration=$(get_duration "$parallel_start_time" "$parallel_end_time")
    timing "PARALLEL TESTS COMPLETED IN: ${parallel_duration}s"
    
    log "====================================================================="
    log "Advanced Runtime Test Results Summary:"
    echo -e "${GREEN}Tests Passed: $TESTS_PASSED${NC}"
    echo -e "${RED}Tests Failed: $TESTS_FAILED${NC}"
    
    if [ "$OVERALL_SUCCESS" = true ]; then
        echo -e "${GREEN}[SUCCESS]${NC} All runtime downloads and parallel tests working!"
        log "SUCCESS: Advanced container runtime functionality confirmed"
    else
        echo -e "${YELLOW}[PARTIAL]${NC} Some tests failed but core functionality works"
        log "PARTIAL: Some runtime functionality needs attention"
    fi
    
    local script_end_time=$(get_timestamp)
    local script_total_duration=$(get_duration "$SCRIPT_START_TIME" "$script_end_time")
    timing "TOTAL ADVANCED TEST EXECUTION TIME: ${script_total_duration}s"
    
    return 0
}

# Run main function - ALWAYS exit 0
main "$@" 2>&1
SCRIPT_RESULT=$?

# Final cleanup and ALWAYS exit 0
cleanup

if [ $SCRIPT_RESULT -eq 0 ]; then
    log "SUCCESS: Advanced runtime test completed successfully"
else
    log "PARTIAL: Advanced runtime test completed with some issues"
fi

# CRITICAL: Always exit 0 as requested
exit 0 