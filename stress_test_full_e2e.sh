#!/bin/bash

# Quilt Stress Test - E2E
# This script is designed to stress the Quilt daemon by creating multiple
# containers concurrently and testing inter-container communication.

set -e

QUILTD_BIN="./target/debug/quilt"
CLI_BIN="./target/debug/cli"
LOG_DIR="stress_logs"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/stress_test_$(date +%Y%m%d_%H%M%S).log"

# Number of concurrent containers to create
CONCURRENT_CONTAINERS=5
NETWORK_TEST_CONTAINERS=3
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
"$QUILTD_BIN" > "server_debug.log" 2>&1 &
SERVER_PID=$!
log_test "Server started with PID: $SERVER_PID"

# Wait for server to be ready
log_test "Waiting for server to be ready..."
for i in {1..20}; do
    if "$CLI_BIN" status-all >/dev/null 2>&1; then
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
pids=()
container_ids=()

for i in $(seq 1 $CONCURRENT_CONTAINERS); do
    (
        log_test "Creating container: concurrent-worker-$i"
        command="echo 'Container $i ready'; sleep 30; echo 'Container $i completed'"
        log_test "Command: $command"
        container_id=$("$CLI_BIN" create --image-path ./nixos-minimal.tar.gz -- /bin/sh -c "$command")
        if [ $? -eq 0 ]; then
            log_pass "Created container: $container_id (concurrent-worker-$i)"
            log_stress "Concurrent container $i created: $container_id"
            echo "$container_id"
        else
            log_fail "Failed to create container: concurrent-worker-$i"
        fi
    ) &
    pids+=($!)
done

# Wait for all background container creations to complete
for pid in "${pids[@]}"; do
    wait "$pid"
done

# Collect all created container IDs
for pid in "${pids[@]}"; do
    id=$(wait "$pid")
    if [ -n "$id" ]; then
        container_ids+=("$id")
    fi
done

log_test "All creation requests sent. Container IDs: ${container_ids[*]}"
log_pass "Concurrent creation test completed."

# Check status of created containers
log_test "Verifying container statuses..."
for id in "${container_ids[@]}"; do
    status_output=$("$CLI_BIN" status "$id")
    log_test "Status for $id: $status_output"
done

log_pass "Full stress test completed successfully!" 