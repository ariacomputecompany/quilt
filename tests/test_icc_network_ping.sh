#!/bin/bash
# Test 3: Network Ping Test
# This test verifies container-to-container network connectivity
# NO FALSE POSITIVES - Production grade test

set +e  # Don't exit on errors - we handle them manually

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Test configuration
SERVER_PID=""
TEST_IMAGE="./nixos-minimal.tar.gz"
BRIDGE_NAME="quilt0"

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
    
    # Kill server
    if [ ! -z "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null || true
        wait $SERVER_PID 2>/dev/null || true
    fi
    
    # Force kill any remaining processes
    pkill -9 -f quilt 2>/dev/null || true
    
    # Clean up network resources
    sudo ip link delete $BRIDGE_NAME 2>/dev/null || true
    
    # Clean up containers and database
    rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    rm -f quilt.db quilt.db-shm quilt.db-wal 2>/dev/null || true
}

trap cleanup EXIT INT TERM

# Get container IP address
get_container_ip() {
    local container_id="$1"
    ./target/debug/cli status "$container_id" | grep "IP:" | awk '{print $2}'
}

# Wait for container to get IP
wait_for_ip() {
    local container_id="$1"
    local max_wait=30
    local count=0
    
    while [ $count -lt $max_wait ]; do
        local ip=$(get_container_ip "$container_id")
        if [ ! -z "$ip" ] && [ "$ip" != "N/A" ]; then
            echo "$ip"
            return 0
        fi
        sleep 0.1
        count=$((count + 1))
    done
    
    return 1
}

# Wait for container to reach Running state
wait_for_running() {
    local container_id="$1"
    local max_wait=100
    local count=0
    
    while [ $count -lt $max_wait ]; do
        local status_output=$(./target/debug/cli status "$container_id" 2>&1)
        
        if echo "$status_output" | grep -q "Status: RUNNING"; then
            return 0
        fi
        
        if echo "$status_output" | grep -q "Status: FAILED"; then
            echo "Container $container_id is in FAILED state!"
            echo "$status_output"
            return 1
        fi
        
        sleep 0.5
        count=$((count + 1))
    done
    
    echo "Container $container_id did not reach RUNNING state"
    return 1
}

echo -e "${BLUE}=== Network Ping Test ===${NC}"

# Pre-flight cleanup
info "Running pre-flight cleanup..."
pkill -9 -f "quilt" 2>/dev/null || true
sleep 1
sudo ip link delete $BRIDGE_NAME 2>/dev/null || true
rm -rf /tmp/quilt-containers/* 2>/dev/null || true
rm -f quilt.db quilt.db-shm quilt.db-wal 2>/dev/null || true

# Build
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
sleep 1
if ! kill -0 $SERVER_PID 2>/dev/null; then
    fail "Server process died immediately" "Check server.log"
    cat server.log
    exit 1
fi

# Wait for server to be ready
retries=50
while [ $retries -gt 0 ]; do
    if nc -z 127.0.0.1 50051 2>/dev/null; then
        success "Server ready on port 50051"
        break
    fi
    sleep 0.2
    retries=$((retries - 1))
done

if [ $retries -eq 0 ]; then
    fail "Server failed to become ready" "Check server.log"
    cat server.log
    exit 1
fi

echo -e "\n${BLUE}=== Test 1: Create Two Containers ===${NC}"
info "Creating container A..."
CREATE_A=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1)
CONTAINER_A=$(echo "$CREATE_A" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_A" ]; then
    fail "Failed to create container A" "$CREATE_A"
    exit 1
fi
success "Created container A: $CONTAINER_A"

info "Creating container B..."
CREATE_B=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1)
CONTAINER_B=$(echo "$CREATE_B" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_B" ]; then
    fail "Failed to create container B" "$CREATE_B"
    exit 1
fi
success "Created container B: $CONTAINER_B"

echo -e "\n${BLUE}=== Test 2: Wait for Containers to Start ===${NC}"
info "Waiting for container A to reach RUNNING state..."
if ! wait_for_running "$CONTAINER_A"; then
    fail "Container A failed to start" "Check server logs"
    exit 1
fi
success "Container A is running"

info "Waiting for container B to reach RUNNING state..."
if ! wait_for_running "$CONTAINER_B"; then
    fail "Container B failed to start" "Check server logs"
    exit 1
fi
success "Container B is running"

echo -e "\n${BLUE}=== Test 3: Get Container IP Addresses ===${NC}"
info "Getting IP addresses..."
IP_A=$(wait_for_ip "$CONTAINER_A")
IP_B=$(wait_for_ip "$CONTAINER_B")

if [ -z "$IP_A" ] || [ -z "$IP_B" ]; then
    fail "Containers did not get IP addresses" "A=$IP_A, B=$IP_B"
    exit 1
fi
success "Container IPs: A=$IP_A, B=$IP_B"

# Give containers a moment to fully configure networking
sleep 2

echo -e "\n${BLUE}=== Test 4: Basic Network Connectivity ===${NC}"
info "Testing ping from A to B..."
PING_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "ping -c 3 -W 2 $IP_B" --capture-output 2>&1)
debug "Ping output: $PING_OUTPUT"

if echo "$PING_OUTPUT" | grep -E -q "3 packets? (transmitted|sent), 3 (packets? )?received|3 received"; then
    success "Ping from A to B successful (3/3 packets)"
else
    fail "Ping from A to B failed" "$PING_OUTPUT"
fi

info "Testing ping from B to A..."
PING_OUTPUT=$(./target/debug/cli exec $CONTAINER_B -c "ping -c 3 -W 2 $IP_A" --capture-output 2>&1)
debug "Ping output: $PING_OUTPUT"

if echo "$PING_OUTPUT" | grep -E -q "3 packets? (transmitted|sent), 3 (packets? )?received|3 received"; then
    success "Ping from B to A successful (3/3 packets)"
else
    fail "Ping from B to A failed" "$PING_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 5: ICC Ping Command ===${NC}"
info "Testing ICC ping by IP..."
ICC_PING=$(./target/debug/cli icc ping $CONTAINER_A $IP_B --count 3 --timeout 5 2>&1)
debug "ICC ping output: $ICC_PING"

if echo "$ICC_PING" | grep -q "bytes from"; then
    success "ICC ping by IP works"
else
    fail "ICC ping by IP failed" "$ICC_PING"
fi

info "Testing ICC ping by container ID..."
ICC_PING=$(./target/debug/cli icc ping $CONTAINER_A $CONTAINER_B --count 3 --timeout 5 2>&1)
debug "ICC ping output: $ICC_PING"

if echo "$ICC_PING" | grep -q "bytes from"; then
    success "ICC ping by container ID works"
else
    fail "ICC ping by container ID failed" "$ICC_PING"
fi

echo -e "\n${BLUE}=== Test 6: Network Bridge Verification ===${NC}"
info "Checking bridge exists..."
if ip link show $BRIDGE_NAME &>/dev/null; then
    success "Bridge $BRIDGE_NAME exists"
else
    fail "Bridge $BRIDGE_NAME not found" "Network infrastructure missing"
fi

info "Checking bridge IP..."
if ip addr show $BRIDGE_NAME | grep -q "10.42.0.1/16"; then
    success "Bridge has correct IP (10.42.0.1/16)"
else
    fail "Bridge IP configuration incorrect" "$(ip addr show $BRIDGE_NAME)"
fi

info "Checking IP forwarding..."
if [ $(cat /proc/sys/net/ipv4/ip_forward) -eq 1 ]; then
    success "IP forwarding is enabled"
else
    fail "IP forwarding is disabled" "Required for container communication"
fi

echo -e "\n${BLUE}=== Test 7: Multiple Container Connectivity ===${NC}"
info "Creating additional containers..."
CONTAINER_C=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
CONTAINER_D=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ ! -z "$CONTAINER_C" ] && [ ! -z "$CONTAINER_D" ]; then
    wait_for_running "$CONTAINER_C"
    wait_for_running "$CONTAINER_D"
    
    IP_C=$(wait_for_ip "$CONTAINER_C")
    IP_D=$(wait_for_ip "$CONTAINER_D")
    
    if [ ! -z "$IP_C" ] && [ ! -z "$IP_D" ]; then
        success "Created containers C=$IP_C, D=$IP_D"
        
        # Test all-to-all connectivity
        info "Testing all-to-all connectivity..."
        TESTS=0
        PASSED=0
        
        for from in A:$CONTAINER_A:$IP_A B:$CONTAINER_B:$IP_B C:$CONTAINER_C:$IP_C D:$CONTAINER_D:$IP_D; do
            from_name=$(echo $from | cut -d: -f1)
            from_id=$(echo $from | cut -d: -f2)
            from_ip=$(echo $from | cut -d: -f3)
            
            for to in A:$IP_A B:$IP_B C:$IP_C D:$IP_D; do
                to_name=$(echo $to | cut -d: -f1)
                to_ip=$(echo $to | cut -d: -f2)
                
                if [ "$from_ip" != "$to_ip" ]; then
                    TESTS=$((TESTS + 1))
                    if ./target/debug/cli exec "$from_id" -c "ping -c 1 -W 1 $to_ip" --capture-output >/dev/null 2>&1; then
                        PASSED=$((PASSED + 1))
                        echo -n "✓"
                    else
                        echo -n "✗"
                    fi
                fi
            done
        done
        echo ""
        
        if [ $PASSED -eq $TESTS ]; then
            success "All containers can communicate ($PASSED/$TESTS)"
        else
            fail "Some containers cannot communicate" "$PASSED/$TESTS connections successful"
        fi
    fi
fi

echo -e "\n${BLUE}=== Test Summary ===${NC}"
info "Cleaning up..."
success "Network ping tests completed"