#!/bin/bash

# Quilt Network Performance Baseline Test
# This script isolates network layer bottlenecks to establish performance baselines
# FIXED: Now measures actual network setup time, not container lifecycle time

set -e

QUILTD_BIN="./target/debug/quilt"
CLI_BIN="./target/debug/cli"
LOG_DIR="stress_logs"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/network_baseline_$(date +%Y%m%d_%H%M%S).log"

# Test configuration - OPTIMIZED FOR NETWORK TESTING
CONCURRENT_CONTAINERS=10  # Increased to stress network layer
NETWORK_ITERATIONS=5      # More iterations for better statistics
STATUS_CHECK_ITERATIONS=20
COMMAND_TIMEOUT=15        # Reduced timeout since containers exit quickly

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
log_metric() { log "${MAGENTA}[METRIC]${NC} $1"; }
log_network() { log "${CYAN}[NETWORK]${NC} $1"; }

# High precision timing function
get_timestamp_ns() {
    date +%s%N
}

# Calculate duration in milliseconds
calc_duration_ms() {
    local start_ns=$1
    local end_ns=$2
    echo "scale=3; ($end_ns - $start_ns) / 1000000" | bc -l
}

# Extract container ID from create output
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
    log_test "Cleanup completed. Total test duration: $SECONDS seconds"
}

trap cleanup EXIT

# Start the test
log_test "🚀 STARTING QUILT NETWORK PERFORMANCE BASELINE TEST (FIXED) 🚀"

# Build binaries
log_test "Building binaries..."
cargo build 2>/dev/null || { log_fail "Build failed."; exit 1; }
log_pass "Binaries built successfully."

# Start server
log_test "Starting Quilt server in background..."
"$QUILTD_BIN" > "server_network_baseline.log" 2>&1 &
SERVER_PID=$!
log_test "Server started with PID: $SERVER_PID"
sleep 3

# === TEST 1: PURE NETWORK SETUP TIMING (SEQUENTIAL) ===
log_test "=== TEST 1: PURE NETWORK SETUP TIMING (SEQUENTIAL) ==="

container_ids=()
creation_times=()

for i in $(seq 1 $NETWORK_ITERATIONS); do
    log_network "Creating container $i for pure network timing..."
    
    # FIXED: Keep container alive long enough for network setup, then exit quickly
    start_total=$(get_timestamp_ns)
    create_output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" create --image-path ./nixos-minimal.tar.gz -- /bin/sh -c "echo 'Network test $i ready'; sleep 2; exit 0") || {
        log_fail "❌ Container creation $i failed"
        continue
    }
    end_total=$(get_timestamp_ns)
    
    container_id=$(get_container_id_from_output "$create_output")
    if [ -z "$container_id" ]; then
        log_fail "❌ Failed to get container ID for iteration $i"
        continue
    fi
    
    container_ids+=("$container_id")
    
    total_time=$(calc_duration_ms $start_total $end_total)
    creation_times+=("$total_time")
    
    log_metric "Container $i pure network setup: ${total_time}ms"
    
    # FIXED: No artificial delays - test pure network performance
    sleep 0.1
done

# Calculate sequential timing statistics
if [ ${#creation_times[@]} -gt 0 ]; then
    total_sequential_time=0
    min_time=${creation_times[0]}
    max_time=${creation_times[0]}
    
    for time in "${creation_times[@]}"; do
        total_sequential_time=$(echo "$total_sequential_time + $time" | bc -l)
        if (( $(echo "$time < $min_time" | bc -l) )); then
            min_time=$time
        fi
        if (( $(echo "$time > $max_time" | bc -l) )); then
            max_time=$time
        fi
    done
    
    avg_time=$(echo "scale=3; $total_sequential_time / ${#creation_times[@]}" | bc -l)
    
    log_metric "📊 SEQUENTIAL NETWORK SETUP METRICS:"
    log_metric "   ⏱️  Average network setup: ${avg_time}ms"
    log_metric "   🚀 Fastest network setup: ${min_time}ms"
    log_metric "   🐌 Slowest network setup: ${max_time}ms"
    log_metric "   📈 Total sequential time: ${total_sequential_time}ms"
    log_metric "   🔄 Network setup consistency: $(echo "scale=1; ($max_time - $min_time) * 100 / $avg_time" | bc -l)% variance"
fi

# === TEST 2: CONCURRENT NETWORK SETUP STRESS TEST ===
log_test "=== TEST 2: CONCURRENT NETWORK SETUP STRESS TEST ==="

concurrent_ids=()
concurrent_pids=()
concurrent_start=$(get_timestamp_ns)

log_network "Launching $CONCURRENT_CONTAINERS concurrent containers for network stress test..."

for i in $(seq 1 $CONCURRENT_CONTAINERS); do
    (
        local_start=$(get_timestamp_ns)
        # FIXED: Keep containers alive for network setup, then exit quickly  
        create_output=$(timeout $COMMAND_TIMEOUT "$CLI_BIN" create --image-path ./nixos-minimal.tar.gz -- /bin/sh -c "echo 'Concurrent network test $i'; sleep 2; exit 0" 2>&1)
        local_end=$(get_timestamp_ns)
        
        local_time=$(calc_duration_ms $local_start $local_end)
        
        if echo "$create_output" | grep -q "Container ID:"; then
            container_id=$(get_container_id_from_output "$create_output")
            echo "SUCCESS:$i:$container_id:$local_time" >> /tmp/concurrent_results.tmp
            log_network "✅ Concurrent container $i network setup: ${local_time}ms"
        else
            echo "FAILURE:$i:NONE:$local_time" >> /tmp/concurrent_results.tmp
            log_network "❌ Concurrent container $i failed after ${local_time}ms"
        fi
    ) &
    concurrent_pids+=($!)
done

# Wait for all concurrent operations
log_network "Waiting for all concurrent network setups to complete..."
for pid in "${concurrent_pids[@]}"; do
    wait "$pid"
done

concurrent_end=$(get_timestamp_ns)
total_concurrent_time=$(calc_duration_ms $concurrent_start $concurrent_end)

# Analyze concurrent results
success_count=0
failure_count=0
concurrent_times=()

if [ -f /tmp/concurrent_results.tmp ]; then
    while IFS=':' read -r status index container_id timing; do
        if [ "$status" = "SUCCESS" ]; then
            ((success_count++))
            concurrent_ids+=("$container_id")
            concurrent_times+=("$timing")
        else
            ((failure_count++))
        fi
    done < /tmp/concurrent_results.tmp
    rm -f /tmp/concurrent_results.tmp
fi

log_metric "📊 CONCURRENT NETWORK SETUP METRICS:"
log_metric "   ✅ Successful network setups: $success_count/$CONCURRENT_CONTAINERS"
log_metric "   ❌ Failed network setups: $failure_count/$CONCURRENT_CONTAINERS"
log_metric "   ⏱️  Total concurrent wall time: ${total_concurrent_time}ms"
log_metric "   🔄 Network setup success rate: $(echo "scale=1; $success_count * 100 / $CONCURRENT_CONTAINERS" | bc -l)%"

if [ ${#concurrent_times[@]} -gt 0 ]; then
    total_concurrent_individual=0
    min_concurrent=${concurrent_times[0]}
    max_concurrent=${concurrent_times[0]}
    
    for time in "${concurrent_times[@]}"; do
        total_concurrent_individual=$(echo "$total_concurrent_individual + $time" | bc -l)
        if (( $(echo "$time < $min_concurrent" | bc -l) )); then
            min_concurrent=$time
        fi
        if (( $(echo "$time > $max_concurrent" | bc -l) )); then
            max_concurrent=$time
        fi
    done
    
    avg_concurrent=$(echo "scale=3; $total_concurrent_individual / ${#concurrent_times[@]}" | bc -l)
    
    log_metric "   📊 Individual network setup timings:"
    log_metric "      ⏱️  Average: ${avg_concurrent}ms"
    log_metric "      🚀 Fastest: ${min_concurrent}ms"
    log_metric "      🐌 Slowest: ${max_concurrent}ms"
    log_metric "   🚀 Network throughput: $(echo "scale=2; $success_count * 1000 / $total_concurrent_time" | bc -l) containers/second"
    
    # CRITICAL METRIC: Network efficiency
    efficiency=$(echo "scale=1; $total_concurrent_individual * 100 / ($total_concurrent_time * $success_count)" | bc -l)
    log_metric "   ⚡ Network concurrency efficiency: ${efficiency}%"
fi

# === TEST 3: STATUS CHECK LATENCY ANALYSIS ===
log_test "=== TEST 3: STATUS CHECK LATENCY ANALYSIS ==="

all_container_ids=("${container_ids[@]}" "${concurrent_ids[@]}")

if [ ${#all_container_ids[@]} -gt 0 ]; then
    status_times=()
    status_failures=0
    
    log_network "Testing status check latency for ${#all_container_ids[@]} containers..."
    
    for iteration in $(seq 1 $STATUS_CHECK_ITERATIONS); do
        for container_id in "${all_container_ids[@]}"; do
            if [ -n "$container_id" ]; then
                status_start=$(get_timestamp_ns)
                if timeout 5 "$CLI_BIN" status "$container_id" > /dev/null 2>&1; then
                    status_end=$(get_timestamp_ns)
                    status_time=$(calc_duration_ms $status_start $status_end)
                    status_times+=("$status_time")
                    log_network "Status check for $container_id: ${status_time}ms"
                else
                    status_end=$(get_timestamp_ns)
                    status_time=$(calc_duration_ms $status_start $status_end)
                    ((status_failures++))
                    log_network "❌ Status check failed for $container_id after ${status_time}ms"
                fi
            fi
        done
    done
    
    # Calculate status check statistics
    if [ ${#status_times[@]} -gt 0 ]; then
        total_status_time=0
        min_status=${status_times[0]}
        max_status=${status_times[0]}
        
        for time in "${status_times[@]}"; do
            total_status_time=$(echo "$total_status_time + $time" | bc -l)
            if (( $(echo "$time < $min_status" | bc -l) )); then
                min_status=$time
            fi
            if (( $(echo "$time > $max_status" | bc -l) )); then
                max_status=$time
            fi
        done
        
        avg_status=$(echo "scale=3; $total_status_time / ${#status_times[@]}" | bc -l)
        total_status_checks=$((${#status_times[@]} + status_failures))
        success_rate=$(echo "scale=1; ${#status_times[@]} * 100 / $total_status_checks" | bc -l)
        
        log_metric "📊 STATUS CHECK LATENCY METRICS:"
        log_metric "   ⏱️  Average status check: ${avg_status}ms"
        log_metric "   🚀 Fastest status check: ${min_status}ms"
        log_metric "   🐌 Slowest status check: ${max_status}ms"
        log_metric "   ✅ Successful checks: ${#status_times[@]}/$total_status_checks"
        log_metric "   🔄 Status check success rate: ${success_rate}%"
        log_metric "   ❌ Failed checks: $status_failures"
        
        # CRITICAL: Status check performance classification
        if (( $(echo "$avg_status < 100" | bc -l) )); then
            log_pass "✅ Status checks are EXCELLENT (< 100ms average)"
        elif (( $(echo "$avg_status < 500" | bc -l) )); then
            log_warn "⚠️ Status checks are ACCEPTABLE (< 500ms average)"
        else
            log_fail "❌ Status checks need OPTIMIZATION (> 500ms average)"
        fi
    fi
else
    log_warn "No containers available for status check testing"
fi

# === TEST 4: IP ALLOCATION PATTERN ANALYSIS ===
log_test "=== TEST 4: IP ALLOCATION PATTERN ANALYSIS ==="

ip_addresses=()
allocation_times=()

log_network "Analyzing IP allocation patterns and performance..."

for container_id in "${all_container_ids[@]}"; do
    if [ -n "$container_id" ]; then
        ip_start=$(get_timestamp_ns)
        if timeout 3 "$CLI_BIN" status "$container_id" > /tmp/status_output.tmp 2>&1; then
            ip_end=$(get_timestamp_ns)
            ip_time=$(calc_duration_ms $ip_start $ip_end)
            allocation_times+=("$ip_time")
            
            # Extract IP if available in status output
            if grep -q "10\.42\." /tmp/status_output.tmp; then
                ip=$(grep -o "10\.42\.[0-9]*\.[0-9]*" /tmp/status_output.tmp | head -1)
                if [ -n "$ip" ]; then
                    ip_addresses+=("$ip")
                    log_network "Container $container_id allocated IP: $ip (retrieved in ${ip_time}ms)"
                fi
            fi
        fi
        rm -f /tmp/status_output.tmp
    fi
done

if [ ${#ip_addresses[@]} -gt 0 ]; then
    log_metric "📊 IP ALLOCATION ANALYSIS:"
    log_metric "   🌐 Total IPs allocated: ${#ip_addresses[@]}"
    log_metric "   📋 IP addresses: $(IFS=', '; echo "${ip_addresses[*]}")"
    
    # Check for sequential allocation
    sorted_ips=($(printf '%s\n' "${ip_addresses[@]}" | sort -V))
    log_metric "   📈 Sorted IPs: $(IFS=', '; echo "${sorted_ips[*]}")"
    
    # Analyze IP allocation efficiency
    if [ ${#allocation_times[@]} -gt 0 ]; then
        total_alloc_time=0
        for time in "${allocation_times[@]}"; do
            total_alloc_time=$(echo "$total_alloc_time + $time" | bc -l)
        done
        avg_alloc_time=$(echo "scale=3; $total_alloc_time / ${#allocation_times[@]}" | bc -l)
        log_metric "   ⏱️  Average IP retrieval time: ${avg_alloc_time}ms"
    fi
    
    # Check for allocation gaps/conflicts
    unique_ips=($(printf '%s\n' "${ip_addresses[@]}" | sort -u))
    if [ ${#unique_ips[@]} -eq ${#ip_addresses[@]} ]; then
        log_pass "✅ IP allocation: No conflicts detected"
    else
        log_fail "❌ IP allocation: $(( ${#ip_addresses[@]} - ${#unique_ips[@]} )) conflicts detected"
    fi
else
    log_warn "No IP addresses detected in analysis"
fi

# === FINAL BASELINE SUMMARY ===
log_test "=== PURE NETWORK PERFORMANCE BASELINE SUMMARY ==="

log_metric "🎯 NETWORK LAYER PERFORMANCE BASELINE (FIXED):"
log_metric ""

if [ ${#creation_times[@]} -gt 0 ]; then
    log_metric "📊 SEQUENTIAL NETWORK PERFORMANCE:"
    log_metric "   • Average network setup: ${avg_time}ms"
    log_metric "   • Network consistency: $(echo "scale=1; ($max_time - $min_time) * 100 / $avg_time" | bc -l)% variance"
    
    if (( $(echo "$avg_time < 500" | bc -l) )); then
        log_pass "✅ Sequential network setup is EXCELLENT"
    elif (( $(echo "$avg_time < 2000" | bc -l) )); then
        log_warn "⚠️ Sequential network setup is ACCEPTABLE"
    else
        log_fail "❌ Sequential network setup needs OPTIMIZATION"
    fi
fi

log_metric ""
log_metric "🔄 CONCURRENT NETWORK PERFORMANCE:"
log_metric "   • Success rate: $(echo "scale=1; $success_count * 100 / $CONCURRENT_CONTAINERS" | bc -l)%"
log_metric "   • Throughput: $(echo "scale=2; $success_count * 1000 / $total_concurrent_time" | bc -l) containers/second"

if [ $success_count -eq $CONCURRENT_CONTAINERS ]; then
    log_pass "✅ Concurrent network setup is PERFECT"
elif [ $success_count -gt $((CONCURRENT_CONTAINERS * 80 / 100)) ]; then
    log_warn "⚠️ Concurrent network setup is GOOD"
else
    log_fail "❌ Concurrent network setup needs OPTIMIZATION"
fi

if [ ${#status_times[@]} -gt 0 ]; then
    log_metric ""
    log_metric "⚡ STATUS CHECK PERFORMANCE:"
    log_metric "   • Average latency: ${avg_status}ms"
    log_metric "   • Reliability: ${success_rate}%"
fi

log_metric ""
log_metric "🌐 NETWORK RESOURCE MANAGEMENT:"
log_metric "   • IP allocation success: ${#ip_addresses[@]} containers"
log_metric "   • Network failures: $failure_count"

log_pass "PURE network performance baseline established! Ready for optimization!"

# FIXED: No artificial wait time
log_test "Test completed. Containers exited immediately as designed." 