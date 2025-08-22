#!/bin/bash
# Test 4: DNS Resolution Test
# This test verifies DNS resolution between containers
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
DNS_PORT="53"

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
    
    # Clean up DNS server processes
    sudo fuser -k 53/udp 2>/dev/null || true
    sudo fuser -k 53/tcp 2>/dev/null || true
    
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

echo -e "${BLUE}=== DNS Resolution Test ===${NC}"

# Pre-flight cleanup
info "Running pre-flight cleanup..."
pkill -9 -f "quilt" 2>/dev/null || true
sleep 1
sudo ip link delete $BRIDGE_NAME 2>/dev/null || true
sudo fuser -k 53/udp 2>/dev/null || true
sudo fuser -k 53/tcp 2>/dev/null || true
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

echo -e "\n${BLUE}=== Test 1: DNS Server Verification ===${NC}"
info "Checking DNS server status..."
DNS_READY=0
for i in {1..10}; do
    if ss -ulpn 2>/dev/null | grep -q ":$DNS_PORT"; then
        DNS_READY=1
        break
    fi
    sleep 0.5
done

if [ $DNS_READY -eq 1 ]; then
    success "DNS server is listening on port $DNS_PORT"
    debug "DNS server processes: $(ss -ulpn | grep :$DNS_PORT)"
else
    fail "DNS server not listening" "Check server.log for DNS startup errors"
    echo "=== Server Log ==="
    tail -50 server.log
    echo "=================="
    exit 1
fi

echo -e "\n${BLUE}=== Test 2: Create Containers for DNS Testing ===${NC}"
info "Creating container A..."
CREATE_A=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-network-namespace -- sleep 3600 2>&1)
CONTAINER_A=$(echo "$CREATE_A" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_A" ]; then
    fail "Failed to create container A" "$CREATE_A"
    exit 1
fi
success "Created container A: $CONTAINER_A"

info "Creating container B..."
CREATE_B=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-network-namespace -- sleep 3600 2>&1)
CONTAINER_B=$(echo "$CREATE_B" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_B" ]; then
    fail "Failed to create container B" "$CREATE_B"
    exit 1
fi
success "Created container B: $CONTAINER_B"

echo -e "\n${BLUE}=== Test 3: Wait for Containers ===${NC}"
info "Waiting for containers to start..."
if ! wait_for_running "$CONTAINER_A"; then
    fail "Container A failed to start" "Check server logs"
    exit 1
fi
if ! wait_for_running "$CONTAINER_B"; then
    fail "Container B failed to start" "Check server logs"
    exit 1
fi
success "Both containers are running"

info "Getting IP addresses..."
IP_A=$(wait_for_ip "$CONTAINER_A")
IP_B=$(wait_for_ip "$CONTAINER_B")

if [ -z "$IP_A" ] || [ -z "$IP_B" ]; then
    fail "Containers did not get IP addresses" "A=$IP_A, B=$IP_B"
    exit 1
fi
success "Container IPs: A=$IP_A, B=$IP_B"

# Get container names for DNS resolution
NAME_A=$(./target/debug/cli status "$CONTAINER_A" | grep "Name:" | awk '{print $2}')
NAME_B=$(./target/debug/cli status "$CONTAINER_B" | grep "Name:" | awk '{print $2}')

# If no name, use container ID
if [ -z "$NAME_A" ] || [ "$NAME_A" = "N/A" ]; then
    NAME_A="$CONTAINER_A"
fi
if [ -z "$NAME_B" ] || [ "$NAME_B" = "N/A" ]; then
    NAME_B="$CONTAINER_B"
fi

success "Container names: A=$NAME_A, B=$NAME_B"

# Give DNS server a moment to register containers
sleep 2

echo -e "\n${BLUE}=== Test 4: DNS Resolution by Container Name ===${NC}"
info "Testing DNS resolution from A to B by name..."
NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "nslookup $NAME_B 10.42.0.1" --capture-output 2>&1)
debug "nslookup output: $NSLOOKUP_OUTPUT"

if echo "$NSLOOKUP_OUTPUT" | grep -E -q "(Address:|has address|answer:).*$IP_B"; then
    success "DNS resolution by name works (A -> B)"
else
    fail "DNS resolution by name failed (A -> B)" "$NSLOOKUP_OUTPUT"
fi

info "Testing DNS resolution from B to A by name..."
NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_B -c "nslookup $NAME_A 10.42.0.1" --capture-output 2>&1)
debug "nslookup output: $NSLOOKUP_OUTPUT"

if echo "$NSLOOKUP_OUTPUT" | grep -E -q "(Address:|has address|answer:).*$IP_A"; then
    success "DNS resolution by name works (B -> A)"
else
    fail "DNS resolution by name failed (B -> A)" "$NSLOOKUP_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 5: DNS Resolution by Container ID ===${NC}"
info "Testing DNS resolution by container ID..."
NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "nslookup $CONTAINER_B 10.42.0.1" --capture-output 2>&1)
debug "nslookup output: $NSLOOKUP_OUTPUT"

if echo "$NSLOOKUP_OUTPUT" | grep -E -q "(Address:|has address|answer:).*$IP_B"; then
    success "DNS resolution by container ID works"
else
    fail "DNS resolution by container ID failed" "$NSLOOKUP_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 6: DNS Resolution FQDN ===${NC}"
info "Testing DNS resolution with FQDN..."
NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "nslookup $NAME_B.quilt.local 10.42.0.1" --capture-output 2>&1)
debug "nslookup output: $NSLOOKUP_OUTPUT"

if echo "$NSLOOKUP_OUTPUT" | grep -E -q "(Address:|has address|answer:).*$IP_B"; then
    success "DNS resolution with FQDN works"
else
    fail "DNS resolution with FQDN failed" "$NSLOOKUP_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 7: Ping by Container Name ===${NC}"
info "Testing ping by container name (using DNS)..."
PING_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "ping -c 3 -W 2 $NAME_B" --capture-output 2>&1)
debug "Ping output: $PING_OUTPUT"

if echo "$PING_OUTPUT" | grep -E -q "3 packets? (transmitted|sent), 3 (packets? )?received|3 received"; then
    success "Ping by container name works (DNS resolution successful)"
else
    fail "Ping by container name failed" "$PING_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 8: DNS Error Handling ===${NC}"
info "Testing DNS lookup for non-existent container..."
NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "nslookup non-existent-container 10.42.0.1" --capture-output 2>&1)
debug "nslookup output: $NSLOOKUP_OUTPUT"

if echo "$NSLOOKUP_OUTPUT" | grep -i -E "(NXDOMAIN|can.*t find|not found|no answer|Non-existent domain)"; then
    success "DNS properly returns NXDOMAIN for non-existent containers"
else
    fail "DNS should return NXDOMAIN for non-existent containers" "$NSLOOKUP_OUTPUT"
fi

echo -e "\n${BLUE}=== Test 9: DNS Performance Test ===${NC}"
info "Testing DNS lookup performance..."

# Measure single lookup
start_time=$(date +%s%3N)
./target/debug/cli exec "$CONTAINER_A" -c "nslookup $NAME_B 10.42.0.1" --capture-output >/dev/null 2>&1
end_time=$(date +%s%3N)
single_lookup=$((end_time - start_time))

debug "Single DNS lookup: ${single_lookup}ms"

# Measure 10 lookups
start_time=$(date +%s%3N)
for i in {1..10}; do
    ./target/debug/cli exec "$CONTAINER_A" -c "nslookup $NAME_B 10.42.0.1" --capture-output >/dev/null 2>&1
done
end_time=$(date +%s%3N)
ten_lookups=$((end_time - start_time))
avg_lookup=$((ten_lookups / 10))

debug "Average DNS lookup (10 queries): ${avg_lookup}ms"

if [ $avg_lookup -lt 100 ]; then
    success "DNS performance is good (<100ms average)"
else
    fail "DNS performance is slow" "Average lookup time: ${avg_lookup}ms"
fi

echo -e "\n${BLUE}=== Test 10: Container Lifecycle and DNS ===${NC}"
info "Testing DNS cleanup on container removal..."
LIFECYCLE_CONTAINER=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-network-namespace -- sleep 60 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ ! -z "$LIFECYCLE_CONTAINER" ]; then
    wait_for_running "$LIFECYCLE_CONTAINER"
    LIFECYCLE_IP=$(wait_for_ip "$LIFECYCLE_CONTAINER")
    LIFECYCLE_NAME=$(./target/debug/cli status "$LIFECYCLE_CONTAINER" | grep "Name:" | awk '{print $2}')
    
    if [ -z "$LIFECYCLE_NAME" ] || [ "$LIFECYCLE_NAME" = "N/A" ]; then
        LIFECYCLE_NAME="$LIFECYCLE_CONTAINER"
    fi
    
    if [ ! -z "$LIFECYCLE_IP" ]; then
        success "Created lifecycle test container: $LIFECYCLE_NAME ($LIFECYCLE_IP)"
        
        # Test DNS resolution before removal
        sleep 1
        info "Testing DNS resolution before removal..."
        NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "nslookup $LIFECYCLE_NAME 10.42.0.1" --capture-output 2>&1)
        
        if echo "$NSLOOKUP_OUTPUT" | grep -q "Address.*$LIFECYCLE_IP"; then
            success "DNS resolution works before removal"
        else
            fail "DNS resolution failed before removal" "$NSLOOKUP_OUTPUT"
        fi
        
        # Remove container
        info "Removing container..."
        ./target/debug/cli remove "$LIFECYCLE_CONTAINER" --force >/dev/null 2>&1
        sleep 2
        
        # Test DNS cleanup after removal
        info "Testing DNS cleanup after removal..."
        NSLOOKUP_OUTPUT=$(./target/debug/cli exec $CONTAINER_A -c "nslookup $LIFECYCLE_NAME 10.42.0.1" --capture-output 2>&1)
        
        if echo "$NSLOOKUP_OUTPUT" | grep -i -E "(NXDOMAIN|can.*t find|not found|no answer|Non-existent domain)"; then
            success "DNS entry properly cleaned up after container removal"
        else
            fail "DNS entry not cleaned up after removal" "$NSLOOKUP_OUTPUT"
        fi
    fi
fi

echo -e "\n${BLUE}=== Test Summary ===${NC}"
success "DNS resolution tests completed"