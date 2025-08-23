#!/bin/bash

# Comprehensive ICC & Networking Test Suite
# Tests all ICC and networking features with REAL validation - NO FALSE POSITIVES
# Validates network setup, DNS, container communication, and ICC commands

set +e  # Don't exit on errors - we handle them manually

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
NC='\033[0m'

# Test configuration
SERVER_PID=""
TEST_ID=$(date +%s)
TEST_IMAGE="./nixos-minimal.tar.gz"
DEV_IMAGE="./nixos-dev.tar.gz"
LOG_FILE="icc_test_${TEST_ID}.log"
BRIDGE_NAME="quilt0"
DNS_PORT="53"

# Test counters
TOTAL_TESTS=0
PASSED_TESTS=0
FAILED_TESTS=0

# Timing measurements
declare -A TIMINGS

# Helper functions
info() {
    echo -e "${BLUE}[INFO]${NC} $1" | tee -a "$LOG_FILE"
}

success() {
    echo -e "${GREEN}[PASS]${NC} $1" | tee -a "$LOG_FILE"
}

fail() {
    echo -e "${RED}[FAIL]${NC} $1" | tee -a "$LOG_FILE"
    echo -e "  ${YELLOW}Details: $2${NC}" | tee -a "$LOG_FILE"
}

debug() {
    echo -e "${CYAN}[DEBUG]${NC} $1" >> "$LOG_FILE"
}

perf() {
    echo -e "${MAGENTA}[PERF]${NC} $1" | tee -a "$LOG_FILE"
}

run_test() {
    local test_name="$1"
    local test_cmd="$2"
    local validation_cmd="$3"
    local expected="$4"
    
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    echo -n "  Testing: $test_name... "
    
    # Execute test command
    local start_time=$(date +%s%3N)
    debug "Running: $test_cmd"
    local output=$(eval "$test_cmd" 2>&1)
    local test_result=$?
    local end_time=$(date +%s%3N)
    local duration=$((end_time - start_time))
    
    debug "Output: $output"
    debug "Exit code: $test_result"
    
    # Run validation if provided
    if [ ! -z "$validation_cmd" ]; then
        debug "Validating: $validation_cmd"
        if eval "$validation_cmd"; then
            echo -e "${GREEN}PASSED${NC} (${duration}ms)"
            PASSED_TESTS=$((PASSED_TESTS + 1))
            return 0
        else
            echo -e "${RED}FAILED${NC}"
            echo -e "    Command: $test_cmd"
            echo -e "    Validation: $validation_cmd"
            echo -e "    Expected: $expected"
            echo -e "    Output: $output"
            FAILED_TESTS=$((FAILED_TESTS + 1))
            return 1
        fi
    else
        # Just check exit code
        if [ $test_result -eq 0 ]; then
            echo -e "${GREEN}PASSED${NC} (${duration}ms)"
            PASSED_TESTS=$((PASSED_TESTS + 1))
            return 0
        else
            echo -e "${RED}FAILED${NC}"
            echo -e "    Command: $test_cmd"
            echo -e "    Exit code: $test_result"
            echo -e "    Output: $output"
            FAILED_TESTS=$((FAILED_TESTS + 1))
            return 1
        fi
    fi
}

cleanup() {
    info "Cleaning up test environment..."
    
    # Kill server
    if [ ! -z "$SERVER_PID" ]; then
        kill $SERVER_PID 2>/dev/null || true
        wait $SERVER_PID 2>/dev/null || true
    fi
    
    # Force kill any remaining processes
    pkill -9 -f quilt 2>/dev/null || true
    
    # Clean up network resources
    sudo ip link delete $BRIDGE_NAME 2>/dev/null || true
    
    # Clean up iptables rules
    sudo iptables -D FORWARD -i $BRIDGE_NAME -j ACCEPT 2>/dev/null || true
    sudo iptables -D FORWARD -o $BRIDGE_NAME -j ACCEPT 2>/dev/null || true
    sudo iptables -D INPUT -i $BRIDGE_NAME -p udp --dport 53 -j ACCEPT 2>/dev/null || true
    sudo iptables -D INPUT -i $BRIDGE_NAME -p tcp --dport 53 -j ACCEPT 2>/dev/null || true
    sudo iptables -D INPUT -i $BRIDGE_NAME -p tcp --dport 50051 -j ACCEPT 2>/dev/null || true
    sudo iptables -t nat -D POSTROUTING -s 10.42.0.1/16 ! -o $BRIDGE_NAME -j MASQUERADE 2>/dev/null || true
    
    # Clean up test files
    rm -f server.log "$LOG_FILE" 2>/dev/null || true
    
    # Clean up containers
    rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    
    # Clean up database
    rm -f quilt.db 2>/dev/null || true
    
    # Restart systemd-resolved if we stopped it
    if ! systemctl is-active --quiet systemd-resolved 2>/dev/null; then
        sudo systemctl start systemd-resolved 2>/dev/null || true
    fi
}

trap cleanup EXIT INT TERM

# Pre-flight cleanup
pre_flight_cleanup() {
    info "Running pre-flight cleanup..."
    
    # Kill any existing Quilt processes more thoroughly
    pkill -9 -f "quilt" 2>/dev/null || true
    pkill -9 -f "target/debug/quilt" 2>/dev/null || true
    sleep 2
    
    # Clean up any existing bridge
    sudo ip link delete $BRIDGE_NAME 2>/dev/null || true
    
    # Clean up any leftover containers
    rm -rf /tmp/quilt-containers/* 2>/dev/null || true
    
    # Clean up database and WAL files
    rm -f quilt.db quilt.db-shm quilt.db-wal 2>/dev/null || true
    
    # Check if anything is using port 53 on bridge IP using ss
    # Note: We check after deleting bridge, so we check on the IP we'll create
    if sudo ss -tulpn | grep -q ":53 "; then
        info "Port 53 is in use, checking for conflicts..."
        # Check specifically for our bridge IP (will exist after bridge creation)
        # For now, just ensure systemd-resolved isn't interfering
        if systemctl is-active --quiet systemd-resolved 2>/dev/null; then
            info "systemd-resolved is active, it may interfere with DNS server"
            # We don't stop it here as it might break system DNS
        fi
    fi
    
    # Kill any lingering DNS server processes on our port
    sudo fuser -k 53/udp 2>/dev/null || true
    sudo fuser -k 53/tcp 2>/dev/null || true
    
    sleep 1
    
    success "Pre-flight cleanup completed"
}

# Build the project
build_project() {
    info "Building Quilt..."
    if cargo build --quiet; then
        success "Build completed"
    else
        fail "Build failed" "Run 'cargo build' to see errors"
        exit 1
    fi
}

# Start server
start_server() {
    info "Starting Quilt server..."
    
    # Start server with output to server.log
    ./target/debug/quilt > server.log 2>&1 &
    SERVER_PID=$!
    
    # Give server a moment to start
    sleep 1
    
    # Check if server process is still running
    if ! kill -0 $SERVER_PID 2>/dev/null; then
        fail "Server process died immediately" "Check server.log for errors"
        echo "=== Server Log ==="
        cat server.log
        echo "=================="
        exit 1
    fi
    
    # Wait for server to be ready (listening on port)
    local retries=50
    while [ $retries -gt 0 ]; do
        if nc -z 127.0.0.1 50051 2>/dev/null; then
            success "Server started (PID: $SERVER_PID)"
            # Check for DNS server status in log
            if grep -q "DNS server started" server.log; then
                success "DNS server initialized"
            elif grep -q "Failed to start DNS server" server.log; then
                fail "DNS server failed to start" "Check server.log"
                echo "=== Server Log ==="
                tail -50 server.log
                echo "=================="
                exit 1
            fi
            return 0
        fi
        sleep 0.2
        retries=$((retries - 1))
    done
    
    fail "Server failed to become ready" "Check server.log"
    echo "=== Server Log ==="
    cat server.log
    echo "=================="
    exit 1
}

# Extract container ID from create output
get_container_id() {
    echo "$1" | grep "Container ID:" | awk '{print $NF}'
}

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
    
    debug "Waiting for container $container_id to reach RUNNING state..."
    while [ $count -lt $max_wait ]; do
        local status_output=$(./target/debug/cli status "$container_id" 2>&1)
        debug "[$count/$max_wait] Container $container_id status check: $(echo "$status_output" | grep -E "(Status:|Exit Code:|Error:)" | tr '\n' ' ')"
        
        if echo "$status_output" | grep -q "Status: RUNNING"; then
            debug "Container $container_id reached RUNNING state after $count checks"
            return 0
        fi
        
        # Check for failed state
        if echo "$status_output" | grep -q "Status: FAILED"; then
            debug "Container $container_id is in FAILED state!"
            echo "Full container status:"
            echo "$status_output"
            
            # Try to get logs
            debug "Attempting to get container logs..."
            local logs=$(./target/debug/cli logs "$container_id" 2>&1 || echo "Failed to get logs")
            debug "Container logs: $logs"
            
            fail "Container $container_id reached FAILED state" "Check logs above"
            return 1
        fi
        
        sleep 0.5
        count=$((count + 1))
    done
    
    # Show container status for debugging
    echo "Container status after timeout:"
    ./target/debug/cli status "$container_id"
    fail "Container $container_id did not reach RUNNING state" "Timeout after 50 seconds"
    return 1
}

echo -e "${BLUE}=== Comprehensive ICC & Networking Test Suite ===${NC}"
echo -e "${BLUE}Test ID: $TEST_ID${NC}"
echo -e "${BLUE}Log file: $LOG_FILE${NC}\n"

# Initial setup
pre_flight_cleanup
build_project
start_server

# Test 1: Network Infrastructure
echo -e "\n${BLUE}=== TEST 1: Network Infrastructure ===${NC}"

run_test "Bridge creation" \
    "ip link show $BRIDGE_NAME 2>&1" \
    "ip link show $BRIDGE_NAME | grep -q -E '(state UP|state DOWN)'" \
    "Bridge should be created"

run_test "Bridge IP configuration" \
    "ip addr show $BRIDGE_NAME 2>&1" \
    "ip addr show $BRIDGE_NAME | grep -q '10.42.0.1/16'" \
    "Bridge should have IP 10.42.0.1/16"

run_test "IP forwarding enabled" \
    "cat /proc/sys/net/ipv4/ip_forward" \
    "[ \$(cat /proc/sys/net/ipv4/ip_forward) -eq 1 ]" \
    "IP forwarding should be enabled"

# DNS server might take a moment to start, so check with retry
info "Checking DNS server..."
DNS_READY=0
for i in {1..10}; do
    if ss -ulpn 2>/dev/null | grep -q ":$DNS_PORT"; then
        DNS_READY=1
        break
    fi
    sleep 0.5
done

if [ $DNS_READY -eq 1 ]; then
    run_test "DNS server listening" \
        "ss -ulpn | grep :$DNS_PORT 2>&1" \
        "ss -ulpn | grep -q ':$DNS_PORT'" \
        "DNS server should be listening on port $DNS_PORT"
else
    fail "DNS server not listening" "DNS server should be listening on port $DNS_PORT"
    FAILED_TESTS=$((FAILED_TESTS + 1))
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
fi

# Test 2: Basic Container Communication
echo -e "\n${BLUE}=== TEST 2: Basic Container-to-Container Communication ===${NC}"

info "Creating test containers..."

# Create container A with detailed error checking
debug "Creating container A with command: sleep 3600"
CREATE_OUTPUT_A=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1)
CREATE_RESULT_A=$?
debug "Container A creation exit code: $CREATE_RESULT_A"
debug "Container A creation output: $CREATE_OUTPUT_A"
CONTAINER_A=$(echo "$CREATE_OUTPUT_A" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_A" ]; then
    fail "Failed to create container A" "Exit code: $CREATE_RESULT_A, Output: $CREATE_OUTPUT_A"
    echo "=== Last 50 lines of server.log ==="
    tail -50 server.log
    echo "=== End of server.log ==="
    exit 1
fi
debug "Container A created with ID: $CONTAINER_A"

# Check container A status immediately
debug "Checking container A status immediately after creation..."
STATUS_A=$(./target/debug/cli status "$CONTAINER_A" 2>&1)
debug "Container A initial status: $STATUS_A"

# Create container B with detailed error checking  
debug "Creating container B with command: sleep 3600"
CREATE_OUTPUT_B=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 3600 2>&1)
CREATE_RESULT_B=$?
debug "Container B creation exit code: $CREATE_RESULT_B"
debug "Container B creation output: $CREATE_OUTPUT_B"
CONTAINER_B=$(echo "$CREATE_OUTPUT_B" | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_B" ]; then
    fail "Failed to create container B" "Exit code: $CREATE_RESULT_B, Output: $CREATE_OUTPUT_B"
    echo "=== Last 50 lines of server.log ==="
    tail -50 server.log
    echo "=== End of server.log ==="
    exit 1
fi
debug "Container B created with ID: $CONTAINER_B"

# Check container B status immediately
debug "Checking container B status immediately after creation..."
STATUS_B=$(./target/debug/cli status "$CONTAINER_B" 2>&1)
debug "Container B initial status: $STATUS_B"

success "Created containers: A=$CONTAINER_A, B=$CONTAINER_B"

# Give containers a moment to transition from Created to Starting
sleep 1

# Wait for containers to reach Running state
info "Waiting for containers to start..."
if ! wait_for_running "$CONTAINER_A"; then
    fail "Container A failed to start" "Check server logs"
    exit 1
fi
if ! wait_for_running "$CONTAINER_B"; then
    fail "Container B failed to start" "Check server logs"  
    exit 1
fi

# Wait for IPs
info "Waiting for network configuration..."
IP_A=$(wait_for_ip "$CONTAINER_A")
IP_B=$(wait_for_ip "$CONTAINER_B")

if [ -z "$IP_A" ] || [ -z "$IP_B" ]; then
    fail "Containers did not get IP addresses" "A=$IP_A, B=$IP_B"
    exit 1
fi

success "Container IPs: A=$IP_A, B=$IP_B"

# Give containers a moment to fully settle after network configuration
info "Waiting for containers to be fully ready..."
sleep 2

run_test "Ping A to B (IP)" \
    "./target/debug/cli exec $CONTAINER_A -c \"ping -c 3 -W 2 $IP_B\" --capture-output" \
    "./target/debug/cli exec $CONTAINER_A -c \"ping -c 1 -W 2 $IP_B\" --capture-output | grep -E -q '1 packets? (transmitted|sent), 1 (packets? )?received|1 received'" \
    "Container A should ping B successfully"

run_test "Ping B to A (IP)" \
    "./target/debug/cli exec $CONTAINER_B -c \"ping -c 3 -W 2 $IP_A\" --capture-output" \
    "./target/debug/cli exec $CONTAINER_B -c \"ping -c 1 -W 2 $IP_A\" --capture-output | grep -E -q '1 packets? (transmitted|sent), 1 (packets? )?received|1 received'" \
    "Container B should ping A successfully"

# Test 3: DNS Resolution
echo -e "\n${BLUE}=== TEST 3: DNS Resolution ===${NC}"

# Get container names - containers without explicit names use their ID as name
NAME_A=$(./target/debug/cli status "$CONTAINER_A" | grep "Name:" | awk '{print $2}')
NAME_B=$(./target/debug/cli status "$CONTAINER_B" | grep "Name:" | awk '{print $2}')

# If no name, use container ID
if [ -z "$NAME_A" ] || [ "$NAME_A" = "N/A" ]; then
    NAME_A="$CONTAINER_A"
fi
if [ -z "$NAME_B" ] || [ "$NAME_B" = "N/A" ]; then
    NAME_B="$CONTAINER_B"
fi

run_test "DNS resolution by container name (A to B)" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup $NAME_B 10.42.0.1\" --capture-output" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup $NAME_B 10.42.0.1 2>&1\" --capture-output | grep -E -q \"(Address:|has address|answer:).*$IP_B\"" \
    "Should resolve $NAME_B to $IP_B"

run_test "DNS resolution by container ID" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup $CONTAINER_B 10.42.0.1\" --capture-output" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup $CONTAINER_B 10.42.0.1 2>&1\" --capture-output | grep -E -q \"(Address:|has address|answer:).*$IP_B\"" \
    "Should resolve $CONTAINER_B to $IP_B"

run_test "DNS resolution FQDN" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup $NAME_B.quilt.local 10.42.0.1\" --capture-output" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup $NAME_B.quilt.local 10.42.0.1 2>&1\" --capture-output | grep -E -q \"(Address:|has address|answer:).*$IP_B\"" \
    "Should resolve $NAME_B.quilt.local to $IP_B"

run_test "Ping by container name" \
    "./target/debug/cli exec $CONTAINER_A -c \"ping -c 3 -W 2 $NAME_B\" --capture-output" \
    "./target/debug/cli exec $CONTAINER_A -c \"ping -c 1 -W 2 $NAME_B\" --capture-output | grep -E -q '1 packets? (transmitted|sent), 1 (packets? )?received|1 received'" \
    "Should ping by container name"

# Test 4: ICC Ping Command
echo -e "\n${BLUE}=== TEST 4: ICC Ping Command ===${NC}"

run_test "ICC ping by IP" \
    "./target/debug/cli icc ping $CONTAINER_A $IP_B --count 3 --timeout 5" \
    "./target/debug/cli icc ping $CONTAINER_A $IP_B --count 1 --timeout 5 | grep -q 'bytes from'" \
    "ICC ping should work with IP"

run_test "ICC ping by container ID" \
    "./target/debug/cli icc ping $CONTAINER_A $CONTAINER_B --count 3 --timeout 5" \
    "./target/debug/cli icc ping $CONTAINER_A $CONTAINER_B --count 1 --timeout 5 | grep -q 'bytes from'" \
    "ICC ping should work with container ID"

run_test "ICC ping by container name" \
    "./target/debug/cli icc ping $CONTAINER_A $NAME_B --count 3 --timeout 5" \
    "./target/debug/cli icc ping $CONTAINER_A $NAME_B --count 1 --timeout 5 | grep -q 'bytes from'" \
    "ICC ping should work with container name"

# Test 5: ICC Exec Command
echo -e "\n${BLUE}=== TEST 5: ICC Exec Command ===${NC}"

# Create a container with CLI binary
info "Creating container with CLI binary for ICC exec tests..."
CONTAINER_C=$(./target/debug/cli create --image-path "$DEV_IMAGE" --enable-all-namespaces --async-mode --setup "copy:./target/debug/cli:/usr/bin/quilt-cli" -- sleep 3600 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')

if [ -z "$CONTAINER_C" ]; then
    fail "Failed to create container C" "Check server logs"
else
    success "Created container C: $CONTAINER_C"
    
    # Wait for IP
    IP_C=$(wait_for_ip "$CONTAINER_C")
    
    run_test "ICC exec simple command" \
        "./target/debug/cli icc exec $CONTAINER_C -- echo 'Hello from ICC'" \
        "./target/debug/cli icc exec $CONTAINER_C -- echo 'Hello from ICC' | grep -q 'Hello from ICC'" \
        "Should execute command via ICC"
    
    run_test "ICC exec with working directory" \
        "./target/debug/cli icc exec $CONTAINER_C --workdir /tmp -- pwd" \
        "./target/debug/cli icc exec $CONTAINER_C --workdir /tmp -- pwd | grep -q '/tmp'" \
        "Should execute in specified working directory"
    
    run_test "ICC exec with environment variables" \
        "./target/debug/cli icc exec $CONTAINER_C --env TEST_VAR=hello -- sh -c 'echo \$TEST_VAR'" \
        "./target/debug/cli icc exec $CONTAINER_C --env TEST_VAR=hello -- sh -c 'echo \$TEST_VAR' | grep -q 'hello'" \
        "Should pass environment variables"
    
    run_test "ICC exec create nested container" \
        "./target/debug/cli icc exec $CONTAINER_C -- /usr/bin/quilt-cli create --image-path /nixos-minimal.tar.gz --enable-all-namespaces --async-mode -- echo 'Nested container'" \
        "./target/debug/cli icc exec $CONTAINER_C -- /usr/bin/quilt-cli create --image-path /nixos-minimal.tar.gz --enable-all-namespaces --async-mode -- echo 'Nested' | grep -q 'Container ID:'" \
        "Should create nested container via ICC"
fi

# Test 6: Network Stress Test
echo -e "\n${BLUE}=== TEST 6: Network Stress Test ===${NC}"

info "Creating multiple containers for stress test..."
STRESS_CONTAINERS=()
for i in {1..5}; do
    container_id=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 120 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
    if [ ! -z "$container_id" ]; then
        STRESS_CONTAINERS+=("$container_id")
        debug "Created stress container $i: $container_id"
    fi
done

success "Created ${#STRESS_CONTAINERS[@]} containers for stress test"

# Test all-to-all connectivity
info "Testing all-to-all connectivity..."
CONNECTIVITY_TESTS=0
CONNECTIVITY_PASSED=0

for from_container in "${STRESS_CONTAINERS[@]}"; do
    from_ip=$(wait_for_ip "$from_container")
    if [ -z "$from_ip" ]; then
        continue
    fi
    
    for to_container in "${STRESS_CONTAINERS[@]}"; do
        if [ "$from_container" != "$to_container" ]; then
            to_ip=$(wait_for_ip "$to_container")
            if [ ! -z "$to_ip" ]; then
                CONNECTIVITY_TESTS=$((CONNECTIVITY_TESTS + 1))
                if ./target/debug/cli exec "$from_container" -c "ping -c 1 -W 1 $to_ip" --capture-output >/dev/null 2>&1; then
                    CONNECTIVITY_PASSED=$((CONNECTIVITY_PASSED + 1))
                    debug "✓ $from_container -> $to_container"
                else
                    debug "✗ $from_container -> $to_container"
                fi
            fi
        fi
    done
done

if [ $CONNECTIVITY_TESTS -gt 0 ]; then
    perf "All-to-all connectivity: $CONNECTIVITY_PASSED/$CONNECTIVITY_TESTS passed"
    if [ $CONNECTIVITY_PASSED -eq $CONNECTIVITY_TESTS ]; then
        success "All containers can communicate"
    else
        fail "Some containers cannot communicate" "$CONNECTIVITY_PASSED/$CONNECTIVITY_TESTS connections successful"
    fi
fi

# Test 7: DNS Performance
echo -e "\n${BLUE}=== TEST 7: DNS Performance ===${NC}"

if [ ! -z "$CONTAINER_A" ] && [ ! -z "$NAME_B" ]; then
    info "Testing DNS lookup performance..."
    
    # Measure single lookup
    start_time=$(date +%s%3N)
    ./target/debug/cli exec "$CONTAINER_A" -c "nslookup $NAME_B 10.42.0.1" >/dev/null 2>&1
    end_time=$(date +%s%3N)
    single_lookup=$((end_time - start_time))
    
    perf "Single DNS lookup: ${single_lookup}ms"
    
    # Measure 10 lookups
    start_time=$(date +%s%3N)
    for i in {1..10}; do
        ./target/debug/cli exec "$CONTAINER_A" -c "nslookup $NAME_B 10.42.0.1" >/dev/null 2>&1
    done
    end_time=$(date +%s%3N)
    ten_lookups=$((end_time - start_time))
    avg_lookup=$((ten_lookups / 10))
    
    perf "Average DNS lookup (10 queries): ${avg_lookup}ms"
    
    if [ $avg_lookup -lt 100 ]; then
        success "DNS performance is good (<100ms average)"
    else
        fail "DNS performance is slow" "Average lookup time: ${avg_lookup}ms"
    fi
fi

# Test 8: Error Handling
echo -e "\n${BLUE}=== TEST 8: Error Handling ===${NC}"

run_test "ICC ping invalid container" \
    "./target/debug/cli icc ping invalid-container $IP_A --count 1 --timeout 2 2>&1" \
    "./target/debug/cli icc ping invalid-container $IP_A --count 1 --timeout 2 2>&1 | grep -q -E '(not found|does not exist|failed)'" \
    "Should fail gracefully with invalid container"

run_test "ICC exec on non-existent container" \
    "./target/debug/cli icc exec non-existent -- echo test 2>&1" \
    "./target/debug/cli icc exec non-existent -- echo test 2>&1 | grep -q -E '(not found|does not exist|failed)'" \
    "Should fail gracefully with non-existent container"

run_test "DNS lookup for non-existent name" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup non-existent-container 10.42.0.1\" --capture-output 2>&1" \
    "./target/debug/cli exec $CONTAINER_A -c \"nslookup non-existent-container 10.42.0.1\" --capture-output 2>&1 | grep -i -E '(NXDOMAIN|can.*t find|not found|no answer|Non-existent domain)'" \
    "Should return NXDOMAIN for non-existent names"

# Test 9: Container Lifecycle and DNS
echo -e "\n${BLUE}=== TEST 9: Container Lifecycle and DNS ===${NC}"

info "Creating container for lifecycle test..."
LIFECYCLE_CONTAINER=$(./target/debug/cli create --image-path "$TEST_IMAGE" --enable-all-namespaces --async-mode -- sleep 60 2>&1 | grep "Container ID:" | tail -1 | awk '{print $NF}')
LIFECYCLE_IP=$(wait_for_ip "$LIFECYCLE_CONTAINER")
LIFECYCLE_NAME=$(./target/debug/cli status "$LIFECYCLE_CONTAINER" | grep "Name:" | awk '{print $2}')

if [ ! -z "$LIFECYCLE_CONTAINER" ] && [ ! -z "$LIFECYCLE_NAME" ] && [ ! -z "$CONTAINER_A" ]; then
    run_test "DNS resolution before removal" \
        "./target/debug/cli exec $CONTAINER_A -c \"nslookup $LIFECYCLE_NAME 10.42.0.1\"" \
        "./target/debug/cli exec $CONTAINER_A -c \"nslookup $LIFECYCLE_NAME 10.42.0.1\" | grep -q 'Address.*$LIFECYCLE_IP'" \
        "Should resolve container before removal"
    
    # Remove container
    ./target/debug/cli remove "$LIFECYCLE_CONTAINER" --force >/dev/null 2>&1
    sleep 2
    
    run_test "DNS cleanup after removal" \
        "./target/debug/cli exec $CONTAINER_A -c \"nslookup $LIFECYCLE_NAME 10.42.0.1\" 2>&1" \
        "./target/debug/cli exec $CONTAINER_A -c \"nslookup $LIFECYCLE_NAME 10.42.0.1\" 2>&1 | grep -i -E '(NXDOMAIN|can.*t find|not found|no answer|Non-existent domain)'" \
        "DNS entry should be removed after container removal"
fi

# Final summary
echo -e "\n${BLUE}=== Test Summary ===${NC}"
echo -e "Total tests: $TOTAL_TESTS"
echo -e "Passed: ${GREEN}$PASSED_TESTS${NC}"
echo -e "Failed: ${RED}$FAILED_TESTS${NC}"

if [ $FAILED_TESTS -eq 0 ]; then
    echo -e "\n${GREEN}✓ All tests passed!${NC}"
    exit 0
else
    echo -e "\n${RED}✗ Some tests failed. Check $LOG_FILE for details.${NC}"
    exit 1
fi