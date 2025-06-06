#!/bin/bash

# Quilt ICC Stress Test
# This script is designed to stress the Inter-Container Communication (ICC)
# and recursive container creation capabilities of Quilt.

set -e

QUILTD_BIN="./target/debug/quilt"
CLI_BIN="./target/debug/cli"
LOG_DIR="stress_logs"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/stress_test_icc_$(date +%Y%m%d_%H%M%S).log"

# Test configuration
PARENT_CONTAINERS=2
CHILD_CONTAINERS_PER_PARENT=3
NETWORK_PEERS=3
STRESS_DURATION=90
COMMAND_TIMEOUT=20 # 20 second timeout for CLI commands

# Colors for logging
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
MAGENTA='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

log() {
    echo -e "$1" | tee -a "$LOG_FILE"
}

log_test() { log "${BLUE}[TEST]${NC} $1"; }
log_pass() { log "${GREEN}[PASS]${NC} $1"; }
log_fail() { log "${RED}[FAIL]${NC} $1"; }
log_warn() { log "${YELLOW}[WARN]${NC} $1"; }
log_stress() { log "${MAGENTA}[STRESS]${NC} $1"; }
log_icc() { log "${CYAN}[ICC]${NC} $1"; }

# Function to extract container ID from create output
get_container_id_from_output() {
    echo "$1" | grep "Container ID:" | awk '{print $NF}'
}

# Cleanup function
cleanup() {
    log_test "Starting comprehensive cleanup..."
    if pgrep -f "$QUILTD_BIN" > /dev/null; then
        log_test "Stopping server PID: $(pgrep -f "$QUILTD_BIN")"
        pkill -9 -f "$QUILTD_BIN" || true
        sleep 2
    fi
    if [ -d "active_containers" ]; then
        log_test "Cleaning up container directories..."
        rm -rf active_containers/*
    fi
    log_test "Cleanup completed. Test duration: $SECONDS seconds"
}

trap cleanup EXIT

# Start the test
log_test "🚀 STARTING QUILT ICC & RECURSIVE STRESS TEST 🚀"
set -x # Enable verbose command logging

# Build binaries
log_test "Building binaries..."
cargo build --quiet || { log_fail "Build failed."; exit 1; }
log_pass "Binaries built successfully."

# Start server
log_test "Starting Quilt server in background..."
"$QUILTD_BIN" > "server_debug_icc.log" 2>&1 &
SERVER_PID=$!
log_test "Server started with PID: $SERVER_PID"

sleep 3 # Give server time to start

# === TEST 1: Basic Network Ping Test ===
log_test "=== TEST 1: NETWORK CONNECTIVITY (PING) ==="
peer_ids=()
for i in $(seq 1 $NETWORK_PEERS); do
    log_icc "Attempting to create network peer 'peer-$i'..."
    start_time=$(date +%s)
    
    create_output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" create --image-path ./nixos-minimal.tar.gz -- /bin/sh -c "echo 'Peer $i ready'; sleep $STRESS_DURATION") || {
        log_fail "❌ Create command for peer-$i timed out or failed."
        continue
    }
    
    end_time=$(date +%s)
    duration=$((end_time - start_time))
    
    peer_id=$(get_container_id_from_output "$create_output")
    if [ -z "$peer_id" ]; then
        log_fail "❌ Failed to get ID for peer-$i. Output: $create_output"
        continue
    fi
    
    peer_ids+=("$peer_id")
    log_icc "Created network peer 'peer-$i' with ID: $peer_id in ${duration}s"
done

# Ping between peers
for from_id in "${peer_ids[@]}"; do
    for to_id in "${peer_ids[@]}"; do
        if [ "$from_id" != "$to_id" ]; then
            log_icc "Pinging from $from_id to $to_id..."
            if output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" icc ping "$from_id" "$to_id" --count 1); then
                log_pass "✅ Ping from $from_id to $to_id successful."
            else
                log_fail "❌ Ping from $from_id to $to_id failed. Exit code: $?. Output: $output"
            fi
        fi
    done
done
log_pass "Basic network ping test completed."

# === TEST 2: Recursive Container Creation ===
log_test "=== TEST 2: RECURSIVE CONTAINER CREATION (EXEC) ==="
parent_pids=()
for i in $(seq 1 $PARENT_CONTAINERS); do
    (
        log_icc "Creating parent container parent-$i..."
        start_time=$(date +%s)
        parent_create_output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" create --image-path ./nixos-dev.tar.gz --setup "copy:$CLI_BIN:/usr/bin/quilt-cli" -- /bin/sh -c "sleep $STRESS_DURATION")
        parent_id=$(get_container_id_from_output "$parent_create_output")
        end_time=$(date +%s)
        duration=$((end_time - start_time))

        if [ -z "$parent_id" ]; then
            log_fail "❌ Failed to create parent container 'parent-$i' in ${duration}s. Aborting this thread."
            continue
        fi
        log_pass "Parent container 'parent-$i' created with ID: $parent_id in ${duration}s"

        for j in $(seq 1 $CHILD_CONTAINERS_PER_PARENT); do
            child_name="child-$i-$j"
            log_icc "Parent $parent_id spawning child '$child_name'..."
            
            exec_cmd="/usr/bin/quilt-cli create --image-path ./nixos-minimal.tar.gz -- /bin/sh -c 'echo \"Hello from $child_name\"; sleep 15'"
            
            start_time=$(date +%s)
            output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" icc exec --container-id "$parent_id" -- /bin/sh -c "$exec_cmd")
            child_id=$(get_container_id_from_output "$output")
            end_time=$(date +%s)
            duration=$((end_time - start_time))

            if [ -n "$child_id" ]; then
                log_pass "✅ Parent $parent_id successfully spawned child '$child_name' with ID: $child_id in ${duration}s"

                sleep 1 
                log_icc "Pinging new child $child_id from peer ${peer_ids[0]}..."
                if ping_output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" icc ping "${peer_ids[0]}" "$child_id" --count 1); then
                     log_pass "✅ Ping to child $child_id successful."
                else
                     log_fail "❌ Ping to child $child_id failed. Exit code: $?. Output: $ping_output"
                fi

            else
                log_fail "❌ Parent $parent_id failed to spawn child '$child_name' in ${duration}s. Output: $output"
            fi
        done
    ) &
    parent_pids+=($!)
done

# Wait for all parent processes to finish
for pid in "${parent_pids[@]}"; do
    wait "$pid"
done

log_pass "Recursive container creation test completed."

log_test "Waiting for all containers to finish..."
sleep $((STRESS_DURATION + 5))
log_pass "All tests concluded."
set +x 