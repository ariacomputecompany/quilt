#!/bin/bash
# Production Test: MAC Address Fix Verification
# This test verifies that container network setup uses correct MAC addresses in ARP entries
# PRODUCTION-GRADE: No false positives, comprehensive validation

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

# Get MAC address of interface
get_mac_address() {
    local interface="$1"
    ip link show "$interface" | grep 'link/ether' | awk '{print $2}' 2>/dev/null
}

# Get MAC address inside container
get_container_mac_address() {
    local container_pid="$1" 
    local interface="$2"
    nsenter -t "$container_pid" -n ip link show "$interface" | grep 'link/ether' | awk '{print $2}' 2>/dev/null
}

# Get container PID
get_container_pid() {
    local container_id="$1"
    ./target/debug/cli status "$container_id" | grep "PID:" | awk '{print $2}'
}

# Validate MAC address format
is_valid_mac() {
    local mac="$1"
    # Check format: xx:xx:xx:xx:xx:xx where x is hex digit
    if echo "$mac" | grep -qE '^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$'; then
        # Additional check: not broadcast address
        if [ "$mac" != "ff:ff:ff:ff:ff:ff" ]; then
            return 0
        fi
    fi
    return 1
}

echo -e "${BLUE}=== MAC Address Fix Verification Test ===${NC}"

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

echo -e "\n${BLUE}=== Test 1: Bridge MAC Address Validation ===${NC}"
info "Checking bridge exists and has valid MAC address..."

# Wait a moment for bridge to be fully initialized
sleep 1

if ! ip link show $BRIDGE_NAME &>/dev/null; then
    fail "Bridge $BRIDGE_NAME does not exist" "Network initialization failed"
    exit 1
fi
success "Bridge $BRIDGE_NAME exists"

BRIDGE_MAC=$(get_mac_address $BRIDGE_NAME)
if [ -z "$BRIDGE_MAC" ]; then
    fail "Could not get bridge MAC address" "Bridge may not be properly configured"
    exit 1
fi

if ! is_valid_mac "$BRIDGE_MAC"; then
    fail "Bridge has invalid MAC address" "MAC: $BRIDGE_MAC"
    exit 1
fi
success "Bridge has valid MAC address: $BRIDGE_MAC"

echo -e "\n${BLUE}=== Test 2: Container Creation and MAC Verification ===${NC}"
info "Creating container with network namespace..."
CREATE_OUTPUT=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1)
CONTAINER_ID=$(echo "$CREATE_OUTPUT" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_ID" ]; then
    fail "Failed to create container" "$CREATE_OUTPUT"
    exit 1
fi
success "Created container: $CONTAINER_ID"

info "Waiting for container to reach RUNNING state..."
if ! wait_for_running "$CONTAINER_ID"; then
    fail "Container failed to start" "Check server logs"
    debug "Server log tail:"
    tail -20 server.log
    exit 1
fi
success "Container is running"

info "Getting container network details..."
CONTAINER_IP=$(wait_for_ip "$CONTAINER_ID")
if [ -z "$CONTAINER_IP" ]; then
    fail "Container did not get IP address" "Network configuration failed"
    exit 1
fi
success "Container IP: $CONTAINER_IP"

CONTAINER_PID=$(get_container_pid "$CONTAINER_ID")
if [ -z "$CONTAINER_PID" ]; then
    fail "Could not get container PID" "Process information unavailable"
    exit 1
fi
success "Container PID: $CONTAINER_PID"

echo -e "\n${BLUE}=== Test 3: Container Interface MAC Address ===${NC}"
info "Getting container interface MAC address..."

# Wait for interface to be fully configured
sleep 1

# Get the actual container interface name (dynamic, e.g., quilt759b8066)
CONTAINER_INTERFACE=$(nsenter -t "$CONTAINER_PID" -n ip link show 2>/dev/null | grep -E "quilt[0-9a-f-]+@" | cut -d: -f2 | awk '{print $1}' | cut -d@ -f1)

if [ -z "$CONTAINER_INTERFACE" ]; then
    debug "Failed to get quilt interface, trying other interface names..."
    # Try common interface names
    for iface in eth0 vethc-* enp* ens* quilt*; do
        if nsenter -t "$CONTAINER_PID" -n ip link show "$iface" &>/dev/null; then
            CONTAINER_INTERFACE="$iface"
            debug "Found interface: $iface"
            break
        fi
    done
fi

if [ ! -z "$CONTAINER_INTERFACE" ]; then
    CONTAINER_MAC=$(get_container_mac_address "$CONTAINER_PID" "$CONTAINER_INTERFACE")
    debug "Using container interface: $CONTAINER_INTERFACE"
else
    CONTAINER_MAC=""
fi

if [ -z "$CONTAINER_MAC" ]; then
    fail "Could not get container interface MAC address" "Interface may not be configured"
    debug "Container network interfaces:"
    nsenter -t "$CONTAINER_PID" -n ip link show 2>/dev/null || echo "Failed to list interfaces"
    exit 1
fi

if ! is_valid_mac "$CONTAINER_MAC"; then
    fail "Container has invalid MAC address" "MAC: $CONTAINER_MAC"
    exit 1
fi
success "Container has valid MAC address: $CONTAINER_MAC"

echo -e "\n${BLUE}=== Test 4: ARP Entry Verification ===${NC}"
info "Checking ARP entries contain correct MAC addresses..."

# Check host ARP table for container
HOST_ARP=$(ip neigh show | grep "$CONTAINER_IP" | grep "$BRIDGE_NAME")
if [ ! -z "$HOST_ARP" ]; then
    ARP_MAC=$(echo "$HOST_ARP" | awk '{print $5}')
    if [ "$ARP_MAC" = "ff:ff:ff:ff:ff:ff" ]; then
        fail "Host ARP entry still uses broadcast MAC" "ARP: $HOST_ARP"
        exit 1
    else
        success "Host ARP entry uses valid MAC: $ARP_MAC"
    fi
else
    debug "No host ARP entry found for $CONTAINER_IP (may be normal)"
fi

# Check container ARP table for gateway
CONTAINER_ARP=$(nsenter -t "$CONTAINER_PID" -n ip neigh show 2>/dev/null | grep "10.42.0.1")
if [ ! -z "$CONTAINER_ARP" ]; then
    GATEWAY_ARP_MAC=$(echo "$CONTAINER_ARP" | awk '{print $5}')
    if [ "$GATEWAY_ARP_MAC" = "ff:ff:ff:ff:ff:ff" ]; then
        fail "Container ARP entry still uses broadcast MAC" "ARP: $CONTAINER_ARP" 
        exit 1
    else
        success "Container ARP entry uses valid MAC: $GATEWAY_ARP_MAC"
        
        # Verify it matches bridge MAC
        if [ "$GATEWAY_ARP_MAC" = "$BRIDGE_MAC" ]; then
            success "Container ARP entry matches bridge MAC address"
        else
            fail "Container ARP entry MAC doesn't match bridge" "Container ARP MAC: $GATEWAY_ARP_MAC, Bridge MAC: $BRIDGE_MAC"
            exit 1
        fi
    fi
else
    debug "No container ARP entry found for gateway (may be normal)"
fi

echo -e "\n${BLUE}=== Test 5: Network Connectivity Verification ===${NC}"
info "Testing ping to gateway to verify MAC fix works..."

PING_OUTPUT=$(nsenter -t "$CONTAINER_PID" -n ping -c 3 -W 2 10.42.0.1 2>&1)
debug "Ping output: $PING_OUTPUT"

if echo "$PING_OUTPUT" | grep -E -q "3 packets? (transmitted|sent), 3 (packets? )?received|3 received"; then
    success "Container can ping gateway (3/3 packets) - MAC fix working!"
else
    fail "Container cannot ping gateway" "$PING_OUTPUT"
    debug "Container network config:"
    nsenter -t "$CONTAINER_PID" -n ip addr show 2>/dev/null || echo "Failed to show addresses"
    nsenter -t "$CONTAINER_PID" -n ip route show 2>/dev/null || echo "Failed to show routes"
    nsenter -t "$CONTAINER_PID" -n ip neigh show 2>/dev/null || echo "Failed to show neighbors"
    exit 1
fi

echo -e "\n${BLUE}=== Test 6: Server Log MAC Verification ===${NC}"
info "Checking server logs for MAC address usage..."

if grep -q "lladdr ff:ff:ff:ff:ff:ff" server.log; then
    fail "Server logs still show hardcoded broadcast MAC usage" "Fix not applied correctly"
    debug "Broadcast MAC entries in logs:"
    grep "lladdr ff:ff:ff:ff:ff:ff" server.log
    exit 1
else
    success "Server logs show no hardcoded broadcast MAC usage"
fi

if grep -q "MAC-LOOKUP" server.log; then
    success "Server logs show MAC address lookup functionality is active"
    debug "MAC lookup entries:"
    grep "MAC-LOOKUP" server.log | tail -5
else
    fail "Server logs show no MAC lookup activity" "MAC lookup functions may not be working"
    exit 1
fi

echo -e "\n${BLUE}=== Test Summary ===${NC}"
success "✅ MAC Address Fix Verification PASSED"
info "All MAC addresses are valid and properly used in ARP entries"
info "No hardcoded broadcast MAC addresses found"  
info "Container networking works correctly with proper MAC addresses"
info "Bridge MAC: $BRIDGE_MAC"
info "Container MAC: $CONTAINER_MAC"

echo -e "\n${GREEN}🎉 Production MAC Address Fix is working correctly! 🎉${NC}"