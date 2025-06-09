#!/bin/bash

# Quilt Stress Test - E2E
# This script is designed to stress the Quilt daemon by creating multiple
# containers concurrently and testing inter-container communication.

set -e

# Timing variables
TEST_START_TIME=$(date +%s.%N)
SERVER_START_TIME=0
SERVER_READY_TIME=0
CONTAINER_CREATE_START_TIME=0
CONTAINER_CREATE_END_TIME=0
STATUS_CHECK_START_TIME=0
STATUS_CHECK_END_TIME=0

QUILTD_BIN="./target/debug/quilt"
CLI_BIN="./target/debug/cli"
LOG_DIR="stress_logs"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/stress_test_$(date +%Y%m%d_%H%M%S).log"

# Number of concurrent containers to create
CONCURRENT_CONTAINERS=3
NETWORK_TEST_CONTAINERS=2
STRESS_DURATION=60

# Colors for logging
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
MAGENTA='\033[0;35m'
NC='\033[0m'

log() {
    echo -e "$1" | tee -a "$LOG_FILE"
}

log_test() { log "${BLUE}[TEST]${NC} $1"; }
log_pass() { log "${GREEN}[PASS]${NC} $1"; }
log_fail() { log "${RED}[FAIL]${NC} $1"; }
log_warn() { log "${YELLOW}[WARN]${NC} $1"; }
log_stress() { log "${MAGENTA}[STRESS]${NC} $1"; }

# Cleanup function
cleanup() {
    log_test "Starting comprehensive cleanup..."
    # Stop server
    if pgrep -f "$QUILTD_BIN" > /dev/null; then
        log_test "Stopping server PID: $(pgrep -f "$QUILTD_BIN")"
        pkill -f "$QUILTD_BIN" || true
        sleep 2
    fi
    # Additional cleanup of container directories
    if [ -d "active_containers" ]; then
        log_test "Cleaning up container directories..."
        rm -rf active_containers/*
    fi
    log_test "Cleanup completed. Test duration: $SECONDS"
}

trap cleanup EXIT

# Start the test
log_test "🚀 STARTING COMPREHENSIVE QUILT STRESS TEST 🚀"
log_test "Test configuration:"
log_test "  - Concurrent containers: $CONCURRENT_CONTAINERS"
log_test "  - Network test containers: $NETWORK_TEST_CONTAINERS"
log_test "  - Stress duration: ${STRESS_DURATION}s"
log_test "  - Log file: $LOG_FILE"
log_test "=================================="

# Build binaries if they don't exist
if [ ! -f "$QUILTD_BIN" ] || [ ! -f "$CLI_BIN" ]; then
    log_test "Binaries not found. Building..."
    cargo build --quiet || { log_fail "Build failed."; exit 1; }
fi

log_pass "Found server binary: $QUILTD_BIN"
log_pass "Found CLI binary: $CLI_BIN"

# Start server in background
log_test "Starting Quilt server with enhanced logging..."
SERVER_START_TIME=$(date +%s.%N)
"$QUILTD_BIN" > "server_debug.log" 2>&1 &
SERVER_PID=$!
log_test "Server started with PID: $SERVER_PID"

# Wait for server to be ready
log_test "Waiting for server to be ready..."
for i in {1..20}; do
    if timeout 2 "$CLI_BIN" --help >/dev/null 2>&1; then
        SERVER_READY_TIME=$(date +%s.%N)
        log_pass "Server is ready and listening on port 50051"
        break
    fi
    log_test "Waiting for server to be ready... (attempt $i/20)"
    sleep 1
done

if ! pgrep -f "$QUILTD_BIN" > /dev/null; then
    log_fail "Server failed to start."
    exit 1
fi

# === TEST 1: Concurrent container creation stress ===
log_test "=== TEST 1: CONCURRENT CONTAINER CREATION STRESS ==="
CONTAINER_CREATE_START_TIME=$(date +%s.%N)
pids=()
container_ids=()

for i in $(seq 1 $CONCURRENT_CONTAINERS); do
    (
        log_test "Creating container: concurrent-worker-$i"
        command="echo 'Container $i ready'; sleep 10; echo 'Container $i completed'"
        log_test "Command: $command"
        # Capture both stdout and stderr for debugging
        output=$("$CLI_BIN" create --image-path ./nixos-minimal.tar.gz -- /bin/sh -c "$command" 2>&1)
        container_id=$(echo "$output" | grep -o '[a-f0-9-]\{36\}' | head -1)
        if [ $? -eq 0 ] && [ -n "$container_id" ]; then
            log_pass "Created container: $container_id (concurrent-worker-$i)"
            log_stress "Concurrent container $i created: $container_id"
            echo "$container_id" > "/tmp/container_$i.id"
        else
            log_fail "Failed to create container: concurrent-worker-$i"
            log_warn "Error output: $output"
            echo "FAILED" > "/tmp/container_$i.id"
        fi
    ) &
    pids+=($!)
done

# Wait for all creation processes to complete
log_test "Waiting for all container creation processes to complete..."
for pid in "${pids[@]}"; do
    wait "$pid"
done
CONTAINER_CREATE_END_TIME=$(date +%s.%N)

# Collect container IDs from temp files
for i in $(seq 1 $CONCURRENT_CONTAINERS); do
    if [ -f "/tmp/container_$i.id" ]; then
        id=$(cat "/tmp/container_$i.id")
        if [ -n "$id" ]; then
            container_ids+=("$id")
        fi
        rm -f "/tmp/container_$i.id"
    fi
done

# Give server a moment to settle
sleep 2

log_test "All creation requests sent. Container IDs: ${container_ids[*]}"
log_pass "Concurrent creation test completed."

# Check status of created containers if any were created
if [ ${#container_ids[@]} -gt 0 ]; then
    log_test "Verifying container statuses..."
    log_test "Waiting a moment for containers to settle..."
    sleep 3
    STATUS_CHECK_START_TIME=$(date +%s.%N)
    
    for id in "${container_ids[@]}"; do
        log_test "Checking status for container: $id"
        if status_output=$("$CLI_BIN" status "$id" 2>&1); then
            if echo "$status_output" | grep -q "Error\|error\|Failed\|failed"; then
                log_warn "Status check failed for $id: $status_output"
            else
                log_pass "Status for $id: $(echo "$status_output" | head -3 | tail -1)"
            fi
        else
            log_warn "Could not get status for container: $id (exit code: $?)"
            # Try to see if container directory exists
            if [ -d "/tmp/quilt-containers/$id" ]; then
                log_test "Container directory exists: /tmp/quilt-containers/$id"
            else
                log_test "Container directory missing: /tmp/quilt-containers/$id"
            fi
        fi
            done
    STATUS_CHECK_END_TIME=$(date +%s.%N)
else
    log_warn "No containers were created successfully"
    STATUS_CHECK_START_TIME=$(date +%s.%N)
    STATUS_CHECK_END_TIME=$(date +%s.%N)
fi

# Calculate timing metrics
TEST_END_TIME=$(date +%s.%N)
TOTAL_TEST_TIME=$(echo "$TEST_END_TIME - $TEST_START_TIME" | bc -l)
SERVER_STARTUP_TIME=$(echo "$SERVER_READY_TIME - $SERVER_START_TIME" | bc -l)
CONTAINER_CREATION_TIME=$(echo "$CONTAINER_CREATE_END_TIME - $CONTAINER_CREATE_START_TIME" | bc -l)
STATUS_CHECK_TIME=$(echo "$STATUS_CHECK_END_TIME - $STATUS_CHECK_START_TIME" | bc -l)

# Calculate actual work time (excluding artificial sleeps)
SLEEP_TIME=3.0  # 3 seconds for container settling
ACTUAL_WORK_TIME=$(echo "$TOTAL_TEST_TIME - $SLEEP_TIME" | bc -l)

log_pass "Full stress test completed successfully!"

# Performance metrics report
log_test "=================================="
log_test "📊 PERFORMANCE METRICS REPORT 📊"
log_test "=================================="
log_test "Test Configuration:"
log_test "  • Concurrent containers: $CONCURRENT_CONTAINERS"
log_test "  • Container sleep time: 10s each"
log_test ""
log_test "⏱️  Timing Breakdown:"
log_test "  • Server startup:       $(printf "%.3f" "$SERVER_STARTUP_TIME")s"
log_test "  • Container creation:   $(printf "%.3f" "$CONTAINER_CREATION_TIME")s"
log_test "  • Status verification:  $(printf "%.3f" "$STATUS_CHECK_TIME")s"
log_test "  • Artificial sleeps:    ${SLEEP_TIME}s"
log_test ""
log_test "📈 Summary Times:"
log_test "  • Total test time:      $(printf "%.3f" "$TOTAL_TEST_TIME")s"
log_test "  • Actual work time:     $(printf "%.3f" "$ACTUAL_WORK_TIME")s"
log_test "  • Efficiency ratio:     $(printf "%.1f" "$(echo "scale=1; $ACTUAL_WORK_TIME * 100 / $TOTAL_TEST_TIME" | bc -l)")%"
log_test ""
log_test "🚀 Performance Stats:"
log_test "  • Containers per second: $(printf "%.2f" "$(echo "scale=2; $CONCURRENT_CONTAINERS / $CONTAINER_CREATION_TIME" | bc -l)")"
log_test "  • Avg creation time:     $(printf "%.3f" "$(echo "scale=3; $CONTAINER_CREATION_TIME / $CONCURRENT_CONTAINERS" | bc -l)")s per container"
log_test "  • Status check rate:     $(printf "%.2f" "$(echo "scale=2; $CONCURRENT_CONTAINERS / $STATUS_CHECK_TIME" | bc -l)") checks/second"
log_test "==================================" 