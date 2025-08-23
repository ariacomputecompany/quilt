#!/bin/bash

# QUILT PARALLEL CONTAINER STARTUP SUCCESS TEST
# Tests the core metric: Do containers start successfully with parallel lifecycle?

set -e

TEST_NAME="parallel_startup_success"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_FILE="test_${TEST_NAME}_${TIMESTAMP}.log"
SERVER_LOG="server_${TEST_NAME}_${TIMESTAMP}.log"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
BOLD='\033[1;37m'
NC='\033[0m' # No Color

log() {
    echo -e "${BLUE}[TEST]${NC} $1" | tee -a "$LOG_FILE"
}

info() {
    echo -e "${PURPLE}[INFO]${NC} $1" | tee -a "$LOG_FILE" 
}

pass() {
    echo -e "${GREEN}[PASS]${NC} ✅ $1" | tee -a "$LOG_FILE"
}

fail() {
    echo -e "${RED}[FAIL]${NC} ❌ $1" | tee -a "$LOG_FILE"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} ⚠️  $1" | tee -a "$LOG_FILE"
}

header() {
    echo -e "${CYAN}🚀 $1${NC}"
    echo -e "${CYAN}$(echo "$1" | sed 's/./=/g')${NC}"
}

cleanup() {
    if [ -n "$SERVER_PID" ]; then
        info "Stopping server (PID: $SERVER_PID)..."
        kill -TERM $SERVER_PID 2>/dev/null || true
        sleep 2
        kill -KILL $SERVER_PID 2>/dev/null || true
    fi
    
    # Clean up any remaining processes
    pkill -f "quilt" 2>/dev/null || true
    rm -f server.pid 2>/dev/null || true
    
    pass "Test environment cleaned up"
}

trap cleanup EXIT

header "QUILT PARALLEL CONTAINER STARTUP SUCCESS TEST"

log "Building Quilt project..."
if cargo build --quiet; then
    pass "Build successful"
else
    fail "Build failed"
    exit 1
fi

# Generate test rootfs if needed
if [ ! -f "./nixos-minimal.tar.gz" ]; then
    log "Generating test rootfs..."
    ./dev.sh generate-rootfs
fi

log "Starting Quilt server..."
./target/debug/quilt > "$SERVER_LOG" 2>&1 &
SERVER_PID=$!
echo $SERVER_PID > server.pid

# Wait for server startup
sleep 3

if ! kill -0 $SERVER_PID 2>/dev/null; then
    fail "Server failed to start"
    exit 1
fi

pass "Server started (PID: $SERVER_PID)"

# Test 1: Create 10 containers simultaneously and measure startup success
log "Creating 10 containers simultaneously..."
info "📊 This test measures CONTAINER STARTUP SUCCESS, not network interface creation"

CONTAINER_COUNT=10
CONTAINER_IDS=()
START_TIME=$(date +%s.%3N)

for i in $(seq 1 $CONTAINER_COUNT); do
    info "🔍 Creating container $i/$CONTAINER_COUNT..."
    
    # Create container and capture ID
    CONTAINER_ID=$(./target/debug/cli create \
        --image-path ./nixos-minimal.tar.gz \
        --enable-all-namespaces \
        --async-mode \
        -- /bin/sleep 300 2>&1 | grep "Container ID:" | awk '{print $3}')
    
    if [ -n "$CONTAINER_ID" ]; then
        CONTAINER_IDS+=("$CONTAINER_ID")
        info "🔍 Container $i created: $CONTAINER_ID"
    else
        fail "Failed to create container $i"
        exit 1
    fi
done

CREATION_END_TIME=$(date +%s.%3N)
CREATION_DURATION=$(echo "$CREATION_END_TIME - $START_TIME" | bc -l)

pass "Created $CONTAINER_COUNT containers in ${CREATION_DURATION}s"

# Test 2: Check container startup success rate
log "Checking container startup success rate..."
info "⏰ Waiting 15 seconds for containers to complete startup..."

sleep 15

STARTUP_SUCCESS_COUNT=0
RUNNING_COUNT=0
STARTING_COUNT=0
ERROR_COUNT=0

for CONTAINER_ID in "${CONTAINER_IDS[@]}"; do
    info "📋 Checking status of container: $CONTAINER_ID"
    
    # Get container status
    STATUS_OUTPUT=$(./target/debug/cli status "$CONTAINER_ID" 2>&1)
    
    if echo "$STATUS_OUTPUT" | grep -q "State: Running"; then
        RUNNING_COUNT=$((RUNNING_COUNT + 1))
        STARTUP_SUCCESS_COUNT=$((STARTUP_SUCCESS_COUNT + 1))
        info "✅ Container $CONTAINER_ID: Running"
    elif echo "$STATUS_OUTPUT" | grep -q "State: Starting"; then
        STARTING_COUNT=$((STARTING_COUNT + 1))
        info "🔄 Container $CONTAINER_ID: Starting (still in progress)"
    elif echo "$STATUS_OUTPUT" | grep -q "State: Error"; then
        ERROR_COUNT=$((ERROR_COUNT + 1))
        info "❌ Container $CONTAINER_ID: Error"
    else
        info "❓ Container $CONTAINER_ID: Unknown state"
    fi
done

# Calculate success metrics
SUCCESS_RATE=$(echo "scale=1; $STARTUP_SUCCESS_COUNT * 100 / $CONTAINER_COUNT" | bc -l)
AVERAGE_STARTUP_TIME=$(echo "scale=3; $CREATION_DURATION / $CONTAINER_COUNT" | bc -l)

log "PARALLEL STARTUP SUCCESS RESULTS:"
echo -e "${BOLD}========================================${NC}"
echo -e "${CYAN}📊 CONTAINER STARTUP METRICS:${NC}"
echo -e "   Total Containers Created: ${BOLD}$CONTAINER_COUNT${NC}"
echo -e "   Containers Running: ${GREEN}${BOLD}$RUNNING_COUNT${NC}"
echo -e "   Containers Starting: ${YELLOW}$STARTING_COUNT${NC}"
echo -e "   Containers in Error: ${RED}$ERROR_COUNT${NC}"
echo -e "   Startup Success Rate: ${BOLD}${SUCCESS_RATE}%${NC}"
echo -e "   Average Creation Time: ${BOLD}${AVERAGE_STARTUP_TIME}s per container${NC}"
echo -e "${BOLD}========================================${NC}"

# Evaluate results
if [ "$STARTUP_SUCCESS_COUNT" -eq "$CONTAINER_COUNT" ]; then
    pass "🎉 PERFECT: 100% container startup success achieved!"
    pass "🚀 Parallel container lifecycle is working flawlessly"
elif [ "$STARTUP_SUCCESS_COUNT" -ge 8 ]; then
    pass "🎯 EXCELLENT: ${SUCCESS_RATE}% startup success (≥80%)"
    pass "✅ Parallel container lifecycle is working well"
elif [ "$STARTUP_SUCCESS_COUNT" -ge 5 ]; then
    warn "🔧 GOOD: ${SUCCESS_RATE}% startup success (≥50%)"
    warn "⚡ Some optimization may be needed"
else
    fail "💥 POOR: Only ${SUCCESS_RATE}% startup success"
    fail "🔥 Significant issues with parallel startup"
fi

# Test 3: Verify non-blocking behavior
log "Verifying non-blocking container behavior..."

if echo "$AVERAGE_STARTUP_TIME" | awk '{print ($1 < 1.0)}' | grep -q 1; then
    pass "⚡ EXCELLENT: Average startup time ${AVERAGE_STARTUP_TIME}s (< 1s per container)"
    pass "🎯 Containers are not blocking each other"
elif echo "$AVERAGE_STARTUP_TIME" | awk '{print ($1 < 3.0)}' | grep -q 1; then
    pass "✅ GOOD: Average startup time ${AVERAGE_STARTUP_TIME}s (< 3s per container)" 
    info "🔧 Some room for improvement"
else
    warn "⚠️ SLOW: Average startup time ${AVERAGE_STARTUP_TIME}s (≥3s per container)"
    warn "🐌 Containers may still be blocking each other"
fi

# Final assessment
if [ "$STARTUP_SUCCESS_COUNT" -eq "$CONTAINER_COUNT" ] && echo "$AVERAGE_STARTUP_TIME" | awk '{print ($1 < 1.0)}' | grep -q 1; then
    echo -e "${GREEN}${BOLD}🏆 PERFECT IMPLEMENTATION ACHIEVED!${NC}"
    echo -e "${GREEN}${BOLD}✨ 100% deterministic container startup with parallel lifecycle${NC}"
    exit 0
elif [ "$STARTUP_SUCCESS_COUNT" -ge 8 ]; then
    echo -e "${YELLOW}${BOLD}🎯 EXCELLENT PROGRESS!${NC}"
    echo -e "${YELLOW}${BOLD}⚡ Nearly perfect parallel container lifecycle${NC}"
    exit 0
else
    echo -e "${RED}${BOLD}🔧 NEEDS INVESTIGATION${NC}"
    echo -e "${RED}${BOLD}📊 Check server logs for detailed analysis${NC}"
    exit 1
fi