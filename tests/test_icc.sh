#!/bin/bash

# Inter-Container Communication (ICC) Network Test
# Validates container-to-container networking and process spawning.
# This script is designed to be fail-fast and will exit non-zero on any error.

set -eo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# Binary paths - auto-detect
SERVER_BINARY=""
CLI_BINARY=""
SERVER_PID=""

log() {
    echo -e "${BLUE}[TEST]${NC} $1"
}

success() {
    echo -e "${GREEN}[PASS]${NC} $1"
}

fail() {
    echo -e "${RED}[FAIL]${NC} $1" >&2
    exit 1
}

cleanup() {
    log "ICC cleanup..."
    
    log "Cleaning up server process..."
    if [ ! -z "$SERVER_PID" ] && [ "$SERVER_PID" != "" ]; then
        log "Killing server PID: $SERVER_PID"
        kill -9 -- -$SERVER_PID 2>/dev/null || true
        SERVER_PID=""
    else
        log "No server PID to clean up"
    fi
    
    log "Cleaning up stray processes..."
    pkill -f quilt 2>/dev/null || true
    
    log "Cleaning up temporary files..."
    rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    rm -f server.log 2>/dev/null || true
    
    log "Cleanup completed"
}

trap 'cleanup' EXIT INT TERM

find_binaries() {
    SERVER_BINARY=$(find ./target -name "quilt" -type f -executable 2>/dev/null | head -1)
    CLI_BINARY=$(find ./target -name "cli" -type f -executable 2>/dev/null | head -1)
    
    if [ -z "$SERVER_BINARY" ] || [ -z "$CLI_BINARY" ]; then
        fail "Binaries not found. Build the project first with 'cargo build'."
    fi
    success "Found server binary: $SERVER_BINARY"
    success "Found CLI binary: $CLI_BINARY"
}

start_server() {
    log "Starting Quilt server in the background..."
    
    # Start server in its own process group and capture PID
    setsid $SERVER_BINARY > server.log 2>&1 &
    SERVER_PID=$!
    
    if [ -z "$SERVER_PID" ]; then
        fail "Failed to start server - no PID obtained"
    fi
    
    log "Server started with PID: $SERVER_PID"
    
    # Verify the process is actually running
    if ! kill -0 $SERVER_PID 2>/dev/null; then
        fail "Server process $SERVER_PID is not running"
    fi
    
    success "Server process $SERVER_PID is running"
}

wait_for_server() {
    log "Waiting for server to be ready..."
    
    for i in {1..15}; do
        if nc -z 127.0.0.1 50051 2>/dev/null; then
            success "Server is up and listening on port 50051."
            return 0
        fi
        
        # Check if server process is still alive
        if ! kill -0 $SERVER_PID 2>/dev/null; then
            log "Server process died, checking logs..."
            if [ -f server.log ]; then
                echo "=== Server Log ==="
                cat server.log
                echo "=================="
            fi
            fail "Server process $SERVER_PID died during startup"
        fi
        
        log "Server not ready yet, waiting... (attempt $i/15)"
        sleep 1
    done
    
    log "Server failed to become ready, checking logs..."
    if [ -f server.log ]; then
        echo "=== Server Log ==="
        cat server.log
        echo "=================="
    fi
    fail "Server failed to start or is not responsive after 15 seconds."
}

main() {
    log "Starting ICC Network Test"
    log "======================="
    
    find_binaries
    cleanup
    
    start_server
    wait_for_server
    
    # Test 1: Container-to-container communication
    log "TEST 1: Container-to-Container Ping"
    
    # Create Container A
    log "Creating container A..."
    CONTAINER_A_ID=$(timeout 20 $CLI_BINARY create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- /bin/sh -c "sleep 60" | grep "Container ID" | awk '{print $3}')
    if [ -z "$CONTAINER_A_ID" ]; then 
        fail "Failed to create container A - no container ID returned"; 
    fi
    success "Container A created: $CONTAINER_A_ID"
    
    # Create Container B
    log "Creating container B..."
    CONTAINER_B_ID=$(timeout 20 $CLI_BINARY create --image-path ./nixos-minimal.tar.gz --enable-network-namespace -- /bin/sh -c "sleep 60" | grep "Container ID" | awk '{print $3}')
    if [ -z "$CONTAINER_B_ID" ]; then 
        fail "Failed to create container B - no container ID returned"; 
    fi
    success "Container B created: $CONTAINER_B_ID"
    
    # Get IPs
    log "Waiting for containers to get IP addresses..."
    sleep 7 
    
    IP_A=$(timeout 10 $CLI_BINARY status $CONTAINER_A_ID | grep "IP:" | awk '{print $2}')
    IP_B=$(timeout 10 $CLI_BINARY status $CONTAINER_B_ID | grep "IP:" | awk '{print $2}')
    
    if [ -z "$IP_A" ] || [ -z "$IP_B" ]; then
        log "Failed to get container IPs. Container A status:"
        $CLI_BINARY status $CONTAINER_A_ID || true
        log "Container B status:"
        $CLI_BINARY status $CONTAINER_B_ID || true
        log "Server logs:"
        cat server.log || true
        fail "Failed to get container IPs. Check server logs above."
    fi
    success "Container A IP: $IP_A, Container B IP: $IP_B"
    
    # Ping from A to B
    log "Pinging from A to B..."
    if timeout 15 $CLI_BINARY exec $CONTAINER_A_ID -c "ping -c 3 $IP_B"; then
        success "Container A successfully pinged B"
    else
        fail "Container A failed to ping B"
    fi
    
    # Ping from B to A
    log "Pinging from B to A..."
    if timeout 15 $CLI_BINARY exec $CONTAINER_B_ID -c "ping -c 3 $IP_A"; then
        success "Container B successfully pinged A"
    else
        fail "Container B failed to ping A"
    fi
    
    log "======================="
    echo -e "${GREEN}[SUCCESS] All ICC tests passed!${NC}"
}

main "$@" 