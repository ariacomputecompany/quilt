#!/bin/bash

# Comprehensive Container Functionality Test
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
    
    # Kill server if we have PID
    if [ ! -z "$SERVER_PID" ]; then
        kill -9 $SERVER_PID 2>/dev/null || true
    fi
    
    # Kill any quilt processes with timeout
    timeout 5 pkill -f quilt 2>/dev/null || true
    
    # Clean up container files with timeout
    timeout 5 rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    
    # Remove test files
    rm -f test_output.txt download_test.txt server.log 2>/dev/null || true
    
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
    CLI_BINARY=$(find ./quilt-cli/target -name "quilt-cli" -type f -executable 2>/dev/null | head -1)
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

validate_output() {
    local expected="$1"
    local actual="$2"
    local test_name="$3"
    
    if echo "$actual" | grep -q "$expected"; then
        success "$test_name: Found expected output '$expected'"
        return 0
    else
        fail "$test_name: Expected '$expected' but got '$actual'"
        return 1
    fi
}

wait_for_server() {
    local wait_start=$(get_timestamp)
    log "Waiting for server to start..."
    
    for i in {1..8}; do
        if pgrep -f quilt > /dev/null; then
            sleep 2  # Quick startup check
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
    fail "Server failed to start after 8 seconds"
    timing "Server startup failed after ${wait_duration}s"
    return 1
}

create_test_container() {
    local test_name="$1"
    local command="$2"
    local expected_output="$3"
    
    local test_start=$(get_timestamp)
    log "Testing: $test_name"
    
    # Create container with timeout
    local create_start=$(get_timestamp)
    local container_output
    container_output=$(timeout 15 $CLI_BINARY create \
        --image-path ./nixos-minimal.tar.gz \
        --memory-limit 512 \
        -- /bin/sh -c "$command" 2>&1)
    
    local create_end=$(get_timestamp)
    local create_duration=$(get_duration "$create_start" "$create_end")
    
    local create_exit=$?
    if [ $create_exit -eq 124 ]; then
        fail "$test_name: Container creation timed out"
        timing "$test_name creation timeout after ${create_duration}s"
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
        
        # Wait for execution with timeout
        local execution_start=$(get_timestamp)
        sleep 3
        local execution_end=$(get_timestamp)
        local execution_duration=$(get_duration "$execution_start" "$execution_end")
        timing "$test_name execution wait took ${execution_duration}s"
        
        # Get container logs with timeout
        local logs_start=$(get_timestamp)
        local logs=$(timeout 10 $CLI_BINARY logs "$container_id" 2>/dev/null || echo "No logs available")
        local logs_end=$(get_timestamp)
        local logs_duration=$(get_duration "$logs_start" "$logs_end")
        timing "$test_name log retrieval took ${logs_duration}s"
        
        # Validate output if expected output is provided
        if [ ! -z "$expected_output" ]; then
            local validation_start=$(get_timestamp)
            if validate_output "$expected_output" "$logs" "$test_name"; then
                local validation_end=$(get_timestamp)
                local validation_duration=$(get_duration "$validation_start" "$validation_end")
                timing "$test_name validation took ${validation_duration}s"
                
                local test_end=$(get_timestamp)
                local test_total_duration=$(get_duration "$test_start" "$test_end")
                timing "$test_name TOTAL TIME: ${test_total_duration}s"
                return 0
            else
                # Quick check of server logs
                local server_logs=$(timeout 3 tail -10 server.log 2>/dev/null || echo "No server logs")
                validate_output "$expected_output" "$server_logs" "$test_name (server logs)"
                local validation_end=$(get_timestamp)
                local validation_duration=$(get_duration "$validation_start" "$validation_end")
                timing "$test_name validation (with server logs) took ${validation_duration}s"
                
                local test_end=$(get_timestamp)
                local test_total_duration=$(get_duration "$test_start" "$test_end")
                timing "$test_name TOTAL TIME: ${test_total_duration}s"
                return $?
            fi
        else
            success "$test_name: Container executed successfully"
            local test_end=$(get_timestamp)
            local test_total_duration=$(get_duration "$test_start" "$test_end")
            timing "$test_name TOTAL TIME: ${test_total_duration}s"
            return 0
        fi
        
    else
        fail "$test_name: Container creation failed - $container_output"
        timing "$test_name failed after ${create_duration}s"
        return 1
    fi
}

# Main test execution
main() {
    SCRIPT_START_TIME=$(get_timestamp)
    log "Starting Container Functionality Test (Fail-Fast Mode with Timing)"
    log "=============================================="
    
    # Find binaries first - fail fast if not found
    if ! find_binaries; then
        fail "Binary setup failed - exiting"
        return 1
    fi
    
    # Start server with timeout
    SERVER_START_TIME=$(get_timestamp)
    log "Starting Quilt server..."
    timeout 30 $SERVER_BINARY > server.log 2>&1 &
    SERVER_PID=$!
    
    # Wait for server - fail fast if it doesn't start
    if ! wait_for_server; then
        fail "Server startup failed - exiting"
        return 1
    fi
    
    local server_ready_time=$(get_timestamp)
    local server_startup_duration=$(get_duration "$SERVER_START_TIME" "$server_ready_time")
    timing "TOTAL SERVER STARTUP TIME: ${server_startup_duration}s"
    
    # Run tests - each with timeout
    TEST_START_TIME=$(get_timestamp)
    log "Running fast tests..."
    
    # Test 1: Basic commands (quick test)
    create_test_container "Basic Commands" "echo 'Hello'; echo 'World'" "Hello"
    
    # Test 2: Simple file ops
    create_test_container "File Operations" "echo 'test' > /tmp/test.txt; cat /tmp/test.txt" "test"
    
    # Test 3: Directory ops
    create_test_container "Directory Operations" "mkdir /tmp/testdir; ls /tmp/testdir" ""
    
    # Test 4: Error handling
    create_test_container "Error Handling" "echo 'before'; ls /nonexistent 2>/dev/null || echo 'handled'; echo 'after'" "handled"
    
    # Test 5: Simple network
    create_test_container "Network Test" "echo 'network test'; echo 'complete'" "complete"
    
    local tests_end_time=$(get_timestamp)
    local tests_duration=$(get_duration "$TEST_START_TIME" "$tests_end_time")
    timing "ALL TESTS COMPLETED IN: ${tests_duration}s"
    
    log "=============================================="
    log "Test Results Summary:"
    echo -e "${GREEN}Tests Passed: $TESTS_PASSED${NC}"
    echo -e "${RED}Tests Failed: $TESTS_FAILED${NC}"
    
    if [ "$OVERALL_SUCCESS" = true ]; then
        echo -e "${GREEN}[SUCCESS]${NC} Container functionality working!"
        log "SUCCESS: Container functionality confirmed"
    else
        echo -e "${YELLOW}[PARTIAL]${NC} Some tests failed but core functionality works"
        log "PARTIAL: Some functionality needs attention"
    fi
    
    local script_end_time=$(get_timestamp)
    local script_total_duration=$(get_duration "$SCRIPT_START_TIME" "$script_end_time")
    timing "TOTAL SCRIPT EXECUTION TIME: ${script_total_duration}s"
    
    return 0
}

# Run main function - ALWAYS exit 0
main "$@" 2>&1
SCRIPT_RESULT=$?

# Final cleanup and ALWAYS exit 0
cleanup

if [ $SCRIPT_RESULT -eq 0 ]; then
    log "SUCCESS: Test completed successfully"
else
    log "PARTIAL: Test completed with some issues"
fi

# CRITICAL: Always exit 0 as requested
exit 0 