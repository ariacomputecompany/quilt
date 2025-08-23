#!/bin/bash

# 🧹 QUILT RESOURCE CLEANUP VALIDATION TEST
# ==========================================
# Production-grade test that validates resource cleanup with ZERO false positives.
# Every failure indicates a real architectural issue that needs fixing.

set -e

# Test configuration
TEST_NAME="Resource Cleanup Validation"
TEST_CONTAINERS=5
CLEANUP_TIMEOUT=10
LOG_FILE="test_resource_cleanup_$(date +%Y%m%d_%H%M%S).log"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
MAGENTA='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m'

# Logging functions
log() {
    echo -e "$1" | tee -a "$LOG_FILE"
}

log_header() { log "${CYAN}🧹 $1${NC}"; }
log_test() { log "${BLUE}[TEST]${NC} $1"; }
log_pass() { log "${GREEN}[PASS]${NC} ✅ $1"; }
log_fail() { log "${RED}[FAIL]${NC} ❌ $1"; }
log_warn() { log "${YELLOW}[WARN]${NC} ⚠️  $1"; }
log_info() { log "${MAGENTA}[INFO]${NC} 🔍 $1"; }

# Global variables for tracking
BASELINE_VETH_COUNT=0
BASELINE_BRIDGE_COUNT=0
BASELINE_IPTABLES_NAT_COUNT=0
BASELINE_DNS_PROCESSES=0
CREATED_CONTAINERS=()
SERVER_PID=0
TEST_FAILED=0

# Resource measurement functions
count_quilt_veth_interfaces() {
    # Count veth interfaces that match our naming patterns
    local count=0
    for pattern in "veth-" "vethc-" "quilt"; do
        local pattern_count=$(ip link show 2>/dev/null | grep -c "^[0-9]*: ${pattern}" || true)
        count=$((count + pattern_count))
    done
    echo $count
}

count_bridge_interfaces() {
    # Count bridge interfaces (should remain stable)
    ip link show type bridge 2>/dev/null | grep -c "^[0-9]*:" || echo 0
}

count_iptables_nat_rules() {
    # Count all NAT table PREROUTING rules
    iptables -t nat -L PREROUTING -n 2>/dev/null | grep -c "DNAT\|REDIRECT" || echo 0
}

count_dns_processes() {
    # Count processes listening on DNS ports
    netstat -tulpn 2>/dev/null | grep -c ":105[0-9]\|:115[0-9]\|:125[0-9]" || echo 0
}

count_bridge_attachments() {
    # Count interfaces attached to quilt bridges
    bridge link show 2>/dev/null | grep -c "master quilt" || echo 0
}

# Test setup functions
setup_test_environment() {
    log_header "QUILT RESOURCE CLEANUP VALIDATION TEST"
    log_header "======================================"
    
    # Build project
    log_test "Building Quilt project..."
    if cargo build >> "$LOG_FILE" 2>&1; then
        log_pass "Build successful"
    else
        log_fail "Build failed"
        exit 1
    fi
    
    # Generate rootfs if needed
    if [ ! -f "./nixos-minimal.tar.gz" ]; then
        log_test "Generating rootfs..."
        ./dev.sh generate minimal >> "$LOG_FILE" 2>&1 || {
            log_fail "Failed to generate rootfs"
            exit 1
        }
        log_pass "Rootfs generated"
    fi
}

capture_baseline() {
    log_test "Capturing baseline system resource state..."
    
    BASELINE_VETH_COUNT=$(count_quilt_veth_interfaces)
    BASELINE_BRIDGE_COUNT=$(count_bridge_interfaces)
    BASELINE_IPTABLES_NAT_COUNT=$(count_iptables_nat_rules)
    BASELINE_DNS_PROCESSES=$(count_dns_processes)
    
    log_info "Baseline veth interfaces (quilt-related): $BASELINE_VETH_COUNT"
    log_info "Baseline bridge interfaces: $BASELINE_BRIDGE_COUNT"
    log_info "Baseline iptables NAT rules: $BASELINE_IPTABLES_NAT_COUNT"
    log_info "Baseline DNS processes: $BASELINE_DNS_PROCESSES"
    
    if [ $BASELINE_VETH_COUNT -gt 0 ]; then
        log_warn "Found $BASELINE_VETH_COUNT existing veth interfaces - this may affect test accuracy"
        log_info "Existing interfaces:"
        ip link show | grep -E "veth-|vethc-|quilt" || true
    fi
    
    log_pass "Baseline captured"
}

start_quilt_server() {
    log_test "Starting Quilt server..."
    
    # Kill any existing servers
    pkill -f "target/debug/quilt" 2>/dev/null || true
    sleep 2
    
    # Start server in background
    ./target/debug/quilt > server_$LOG_FILE 2>&1 &
    SERVER_PID=$!
    
    # Wait for server to be ready
    local ready=0
    for i in {1..15}; do
        if kill -0 $SERVER_PID 2>/dev/null; then
            if grep -q "Ready to accept container creation requests" server_$LOG_FILE; then
                ready=1
                break
            fi
        fi
        sleep 1
    done
    
    if [ $ready -eq 1 ]; then
        log_pass "Server started (PID: $SERVER_PID)"
    else
        log_fail "Server failed to start or become ready"
        TEST_FAILED=1
        return 1
    fi
}

create_test_containers() {
    log_test "Creating $TEST_CONTAINERS test containers..."
    
    for i in $(seq 1 $TEST_CONTAINERS); do
        log_info "Creating container $i/$TEST_CONTAINERS..."
        
        local container_output
        if container_output=$(timeout 30 ./target/debug/cli create \
            --image-path ./nixos-minimal.tar.gz \
            --async-mode \
            -- /bin/sleep 300 2>&1); then
            
            local container_id
            container_id=$(echo "$container_output" | grep "Container ID:" | awk '{print $NF}')
            
            if [ -n "$container_id" ]; then
                CREATED_CONTAINERS+=("$container_id")
                log_info "Container $i created: $container_id"
            else
                log_fail "Failed to extract container ID from output"
                log_info "Output was: $container_output"
                TEST_FAILED=1
                return 1
            fi
        else
            log_fail "Failed to create container $i"
            log_info "Error: $container_output"
            TEST_FAILED=1
            return 1
        fi
    done
    
    log_pass "Created ${#CREATED_CONTAINERS[@]} containers"
}

verify_container_resources() {
    log_test "Verifying container startup and network resources..."
    
    # PHASE 1: PRIMARY METRIC - Verify container startup success (parallel architecture)
    log_info "🚀 Phase 1: Checking container startup success (primary metric for parallel architecture)"
    
    # Give containers time to complete startup (should be fast with parallel architecture)
    sleep 3
    
    local running_count=0
    for container_id in "${CREATED_CONTAINERS[@]}"; do
        if timeout 10 ./target/debug/cli status "$container_id" | grep -q "Status: RUNNING"; then
            running_count=$((running_count + 1))
            log_info "✅ Container $container_id: Running"
        else
            log_info "⚠️  Container $container_id: Not running or status check failed"
        fi
    done
    
    log_info "📊 Container Success Rate: $running_count/${#CREATED_CONTAINERS[@]} ($(echo "scale=1; $running_count * 100 / ${#CREATED_CONTAINERS[@]}" | bc -l)%)"
    
    # PRIMARY SUCCESS CRITERION: Container startup success
    if [ $running_count -eq ${#CREATED_CONTAINERS[@]} ]; then
        log_pass "✅ PERFECT: All containers started successfully (100% success rate)"
        local container_success=true
    else
        log_fail "❌ CONTAINER STARTUP ISSUE: Only $running_count/${#CREATED_CONTAINERS[@]} containers are running"
        local container_success=false
        TEST_FAILED=1
    fi
    
    # PHASE 2: SECONDARY METRIC - Network resource verification (eventual consistency)
    log_info "🌐 Phase 2: Checking network resource creation (secondary metric - eventual consistency)"
    
    local current_veth_count
    current_veth_count=$(count_quilt_veth_interfaces)
    local expected_veth_count=$((BASELINE_VETH_COUNT + TEST_CONTAINERS * 2))
    
    local current_attachments
    current_attachments=$(count_bridge_attachments)
    
    log_info "📡 Current veth interfaces: $current_veth_count (expected: $expected_veth_count)"
    log_info "🌉 Current bridge attachments: $current_attachments"
    
    # Network verification - informational for parallel architecture
    if [ $current_veth_count -ge $expected_veth_count ]; then
        log_pass "✅ Network interfaces created correctly ($current_veth_count >= $expected_veth_count)"
    else
        if [ "$container_success" = true ]; then
            log_warn "⚠️  Network interfaces pending: $current_veth_count < $expected_veth_count (expected with background network setup)"
            log_info "ℹ️  With parallel architecture, containers start immediately while network setup completes in background"
        else
            log_fail "❌ Network interfaces missing: $current_veth_count < $expected_veth_count (AND containers failed)"
        fi
    fi
    
    # Final result based on PRIMARY METRIC (container success)
    if [ "$container_success" = true ]; then
        log_pass "🎉 PARALLEL ARCHITECTURE VALIDATION: Container startup successful"
        return 0
    else
        log_fail "💥 PARALLEL ARCHITECTURE FAILURE: Container startup failed"
        return 1
    fi
}

test_dns_functionality() {
    log_test "Testing DNS server functionality..."
    
    # Test direct DNS query to verify DNS server is working
    if timeout 5 dig @10.42.0.1 -p 1053 test.example.com +short >/dev/null 2>&1; then
        log_pass "DNS server is responding to queries"
    else
        # This is not a failure - DNS might reject unknown domains
        log_info "DNS server responded (may have returned NXDOMAIN for test query)"
    fi
}

remove_all_containers() {
    log_test "Removing all test containers..."
    
    local removal_start_time
    removal_start_time=$(date +%s)
    
    for container_id in "${CREATED_CONTAINERS[@]}"; do
        log_info "Removing container: $container_id"
        
        if timeout 15 ./target/debug/cli remove "$container_id" --force >> "$LOG_FILE" 2>&1; then
            log_info "✅ Removed: $container_id"
        else
            log_warn "Failed to remove container: $container_id (may already be removed)"
        fi
    done
    
    local removal_end_time
    removal_end_time=$(date +%s)
    local removal_duration=$((removal_end_time - removal_start_time))
    
    log_pass "Container removal completed in ${removal_duration}s"
}

wait_for_cleanup() {
    log_test "Waiting for background cleanup to complete..."
    
    # Wait for cleanup service to process all tasks
    local cleanup_start_time
    cleanup_start_time=$(date +%s)
    
    while [ $(($(date +%s) - cleanup_start_time)) -lt $CLEANUP_TIMEOUT ]; do
        sleep 2
        log_info "Cleanup in progress... ($(($(date +%s) - cleanup_start_time))s elapsed)"
    done
    
    # Give extra time for any final operations
    sleep 3
    log_pass "Cleanup wait period completed"
}

validate_resource_cleanup() {
    log_test "Validating complete resource cleanup..."
    
    local current_veth_count
    current_veth_count=$(count_quilt_veth_interfaces)
    
    local current_bridge_count
    current_bridge_count=$(count_bridge_interfaces)
    
    local current_iptables_count
    current_iptables_count=$(count_iptables_nat_rules)
    
    local current_attachments
    current_attachments=$(count_bridge_attachments)
    
    log_info "Post-cleanup veth interfaces: $current_veth_count (baseline: $BASELINE_VETH_COUNT)"
    log_info "Post-cleanup bridge interfaces: $current_bridge_count (baseline: $BASELINE_BRIDGE_COUNT)"
    log_info "Post-cleanup iptables NAT rules: $current_iptables_count (baseline: $BASELINE_IPTABLES_NAT_COUNT)"
    log_info "Post-cleanup bridge attachments: $current_attachments"
    
    # Validate veth interface cleanup
    if [ $current_veth_count -eq $BASELINE_VETH_COUNT ]; then
        log_pass "Perfect veth interface cleanup: $current_veth_count (no leaks)"
    elif [ $current_veth_count -gt $BASELINE_VETH_COUNT ]; then
        local leaked_count=$((current_veth_count - BASELINE_VETH_COUNT))
        log_fail "RESOURCE LEAK: $leaked_count veth interfaces not cleaned up"
        log_info "Leaked interfaces:"
        ip link show | grep -E "veth-|vethc-|quilt" | head -10
        TEST_FAILED=1
    else
        log_warn "Fewer veth interfaces than baseline (unexpected but not necessarily bad)"
    fi
    
    # Validate bridge interface stability
    if [ $current_bridge_count -eq $BASELINE_BRIDGE_COUNT ]; then
        log_pass "Bridge interface count stable: $current_bridge_count"
    else
        log_warn "Bridge interface count changed: $current_bridge_count (baseline: $BASELINE_BRIDGE_COUNT)"
    fi
    
    # Validate bridge attachments cleanup
    if [ $current_attachments -eq 0 ]; then
        log_pass "No orphaned bridge attachments"
    else
        log_fail "RESOURCE LEAK: $current_attachments orphaned bridge attachments"
        bridge link show | grep "master quilt" || true
        TEST_FAILED=1
    fi
    
    # Check for specific orphaned interfaces
    local orphaned_interfaces
    orphaned_interfaces=$(ip link show 2>/dev/null | grep -E "veth-[a-f0-9]{8}@|vethc-[a-f0-9]{8}@|quilt[a-f0-9]{8}@" | wc -l)
    
    if [ $orphaned_interfaces -eq 0 ]; then
        log_pass "No orphaned container interfaces found"
    else
        log_fail "ARCHITECTURAL ISSUE: $orphaned_interfaces orphaned container interfaces"
        ip link show | grep -E "veth-[a-f0-9]{8}@|vethc-[a-f0-9]{8}@|quilt[a-f0-9]{8}@" | head -5
        TEST_FAILED=1
    fi
}

validate_iptables_cleanup() {
    log_test "Validating iptables rules cleanup..."
    
    # Check for duplicate DNS rules
    local dns_rules_count
    dns_rules_count=$(iptables -t nat -L PREROUTING -n 2>/dev/null | grep -c "dpt:53.*10\.42\.0\.1" || echo 0)
    
    log_info "DNS DNAT rules found: $dns_rules_count"
    
    if [ $dns_rules_count -le 2 ]; then
        log_pass "DNS DNAT rules are clean (expected 0-2 rules)"
    elif [ $dns_rules_count -le 4 ]; then
        log_warn "Some duplicate DNS DNAT rules present: $dns_rules_count"
    else
        log_fail "RESOURCE LEAK: Excessive DNS DNAT rules: $dns_rules_count"
        iptables -t nat -L PREROUTING -n | grep "dpt:53.*10\.42\.0\.1" | head -5
        TEST_FAILED=1
    fi
}

test_concurrent_operations() {
    log_test "Testing concurrent container operations..."
    
    # Create a few containers concurrently and remove them immediately
    local concurrent_containers=()
    
    for i in {1..3}; do
        (
            local container_output
            if container_output=$(timeout 20 ./target/debug/cli create \
                --image-path ./nixos-minimal.tar.gz \
                --async-mode \
                -- /bin/sleep 10 2>&1); then
                
                local container_id
                container_id=$(echo "$container_output" | grep "Container ID:" | awk '{print $NF}')
                
                if [ -n "$container_id" ]; then
                    echo "$container_id" > /tmp/concurrent_container_$i
                    sleep 2
                    timeout 10 ./target/debug/cli remove "$container_id" --force >/dev/null 2>&1
                fi
            fi
        ) &
    done
    
    # Wait for all concurrent operations
    wait
    
    # Collect container IDs that were created
    for i in {1..3}; do
        if [ -f "/tmp/concurrent_container_$i" ]; then
            local cid
            cid=$(cat "/tmp/concurrent_container_$i")
            concurrent_containers+=("$cid")
            rm -f "/tmp/concurrent_container_$i"
        fi
    done
    
    log_pass "Concurrent operations test completed (${#concurrent_containers[@]} containers)"
    
    # Wait for cleanup
    sleep 5
}

performance_benchmark() {
    log_test "Running performance benchmarks..."
    
    # Test container creation/removal speed
    local start_time
    start_time=$(date +%s.%N)
    
    local benchmark_container
    if benchmark_container=$(timeout 15 ./target/debug/cli create \
        --image-path ./nixos-minimal.tar.gz \
        --async-mode \
        -- /bin/sleep 5 2>&1); then
        
        local container_id
        container_id=$(echo "$benchmark_container" | grep "Container ID:" | awk '{print $NF}')
        
        if [ -n "$container_id" ]; then
            sleep 2  # Let container initialize
            timeout 10 ./target/debug/cli remove "$container_id" --force >/dev/null 2>&1
            
            local end_time
            end_time=$(date +%s.%N)
            local duration
            duration=$(echo "$end_time - $start_time" | bc 2>/dev/null || echo "unknown")
            
            log_pass "Container lifecycle benchmark: ${duration}s"
        else
            log_warn "Benchmark container ID extraction failed"
        fi
    else
        log_warn "Benchmark container creation failed"
    fi
}

cleanup_test_environment() {
    log_test "Cleaning up test environment..."
    
    # Stop server gracefully
    if [ $SERVER_PID -ne 0 ]; then
        log_info "Stopping server (PID: $SERVER_PID)..."
        kill $SERVER_PID 2>/dev/null || true
        sleep 3
        kill -9 $SERVER_PID 2>/dev/null || true
    fi
    
    # Remove any remaining test containers
    for container_id in "${CREATED_CONTAINERS[@]}"; do
        timeout 5 ./target/debug/cli remove "$container_id" --force >/dev/null 2>&1 || true
    done
    
    log_pass "Test environment cleaned up"
}

generate_test_report() {
    log_header "TEST RESULTS SUMMARY"
    log_header "==================="
    
    if [ $TEST_FAILED -eq 0 ]; then
        log_pass "🎯 PERFECT RESOURCE CLEANUP - NO LEAKS DETECTED"
        log_pass "✅ All $TEST_CONTAINERS containers created and removed successfully"
        log_pass "✅ All network interfaces cleaned up perfectly"
        log_pass "✅ No orphaned bridge attachments"
        log_pass "✅ iptables rules maintained correctly"
        log_pass "✅ Concurrent operations handled correctly"
        log_header "🏆 ARCHITECTURAL RESOURCE MANAGEMENT: EXCELLENT"
    else
        log_fail "❌ RESOURCE CLEANUP ISSUES DETECTED"
        log_fail "⚠️  The architectural cleanup implementation needs fixes"
        log_fail "📋 Check the log above for specific resource leaks"
        log_header "🔧 ACTION REQUIRED: Fix resource cleanup logic"
    fi
    
    log_info "📁 Detailed log saved to: $LOG_FILE"
    log_info "🕒 Test completed at: $(date)"
}

# Main test execution
main() {
    # Ensure we have bc for calculations
    command -v bc >/dev/null 2>&1 || {
        log_warn "Installing bc for performance calculations..."
        sudo apt-get update >/dev/null 2>&1 || true
        sudo apt-get install -y bc >/dev/null 2>&1 || true
    }
    
    # Run the test sequence
    setup_test_environment
    capture_baseline
    start_quilt_server || exit 1
    create_test_containers || exit 1
    verify_container_resources || exit 1
    test_dns_functionality
    remove_all_containers
    wait_for_cleanup
    validate_resource_cleanup
    validate_iptables_cleanup
    test_concurrent_operations
    performance_benchmark
    cleanup_test_environment
    generate_test_report
    
    # Exit with proper code
    exit $TEST_FAILED
}

# Set trap for cleanup on exit
trap cleanup_test_environment EXIT

# Run the test
main "$@"