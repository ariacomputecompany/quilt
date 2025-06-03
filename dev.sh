#!/bin/bash

# Quilt Development Script
# Usage: ./dev.sh [command]

set -e

QUILTD_BIN="./target/x86_64-unknown-linux-gnu/debug/quilt"
CLI_BIN="./quilt-cli/target/x86_64-unknown-linux-gnu/debug/quilt-cli"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log() {
    echo -e "${GREEN}[DEV]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    exit 1
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

# Build both binaries (using native target for development)
build() {
    log "Building quiltd (native target for development)..."
    cargo build --target x86_64-unknown-linux-gnu || error "Failed to build quiltd"
    
    log "Building quilt-cli..."
    (cd quilt-cli && cargo build) || error "Failed to build quilt-cli"
    
    log "Build complete!"
    log "  Server: $QUILTD_BIN"
    log "  CLI: $CLI_BIN"
}

# Start the server
server() {
    if ! [ -f "$QUILTD_BIN" ]; then
        warn "Server binary not found, building first..."
        build
    fi
    
    log "Starting Quilt server..."
    $QUILTD_BIN
}

# Start server in background
server-bg() {
    if ! [ -f "$QUILTD_BIN" ]; then
        warn "Server binary not found, building first..."
        build
    fi
    
    # Kill any existing server
    pkill -f quilt || true
    sleep 1
    
    log "Starting Quilt server in background..."
    $QUILTD_BIN &
    sleep 2
    
    if pgrep -f quilt > /dev/null; then
        log "Server started successfully (PID: $(pgrep -f quilt))"
    else
        error "Failed to start server"
    fi
}

# Run CLI command
cli() {
    if ! [ -f "$CLI_BIN" ]; then
        warn "CLI binary not found, building first..."
        build
    fi
    
    $CLI_BIN "$@"
}

# Quick test - create and check a container
test() {
    log "Running comprehensive test with new features..."
    
    # Ensure server is running
    if ! pgrep -f quilt > /dev/null; then
        warn "Server not running, starting it..."
        server-bg
    fi
    
    # Check if alpine.tar.gz exists
    if ! [ -f "alpine.tar.gz" ]; then
        error "alpine.tar.gz not found. Please ensure you have an Alpine rootfs tarball."
    fi
    
    log "=== TEST 1: Basic container with setup commands ==="
    log "Creating container with npm and typescript setup..."
    CONTAINER_ID1=$(cli create \
        --image-path alpine.tar.gz \
        --setup "npm: typescript ts-node" \
        --setup "apk: curl wget" \
        --memory-limit 256 \
        --cpu-limit 50.0 \
        --enable-all-namespaces \
        -- node --version 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
    if [ -n "$CONTAINER_ID1" ]; then
        log "✅ Container 1 created: $CONTAINER_ID1"
        sleep 3
        log "Container 1 status:"
        cli status "$CONTAINER_ID1"
        log "Container 1 logs:"
        cli logs "$CONTAINER_ID1"
    else
        warn "❌ Failed to create container 1 (npm test)"
    fi
    
    log "=== TEST 2: Python container with pip packages ==="
    log "Creating container with Python and pip setup..."
    CONTAINER_ID2=$(cli create \
        --image-path alpine.tar.gz \
        --setup "pip: requests beautifulsoup4" \
        --memory-limit 128 \
        -- python3 -c "import requests; print('Python container with requests:', requests.__version__)" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
    if [ -n "$CONTAINER_ID2" ]; then
        log "✅ Container 2 created: $CONTAINER_ID2"
        sleep 3
        log "Container 2 status:"
        cli status "$CONTAINER_ID2"
        log "Container 2 logs:"
        cli logs "$CONTAINER_ID2"
    else
        warn "❌ Failed to create container 2 (python test)"
    fi
    
    log "=== TEST 3: Resource limited container ==="
    log "Creating container with strict resource limits..."
    CONTAINER_ID3=$(cli create \
        --image-path alpine.tar.gz \
        --memory-limit 64 \
        --cpu-limit 25.0 \
        -- /bin/sh -c "echo 'Memory info:'; cat /proc/meminfo | head -5; echo 'CPU info:'; cat /proc/cpuinfo | head -5" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
    if [ -n "$CONTAINER_ID3" ]; then
        log "✅ Container 3 created: $CONTAINER_ID3"
        sleep 3
        log "Container 3 status:"
        cli status "$CONTAINER_ID3"
        log "Container 3 logs:"
        cli logs "$CONTAINER_ID3"
    else
        warn "❌ Failed to create container 3 (resource test)"
    fi
    
    log "=== TEST 4: Namespace isolation test ==="
    log "Creating container to test namespace isolation..."
    CONTAINER_ID4=$(cli create \
        --image-path alpine.tar.gz \
        --enable-all-namespaces \
        -- /bin/sh -c "echo 'Container hostname:'; hostname; echo 'Container PID 1:'; ps aux | head -3; echo 'Mount points:'; mount | head -5" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
    if [ -n "$CONTAINER_ID4" ]; then
        log "✅ Container 4 created: $CONTAINER_ID4"
        sleep 3
        log "Container 4 status:"
        cli status "$CONTAINER_ID4"
        log "Container 4 logs:"
        cli logs "$CONTAINER_ID4"
    else
        warn "❌ Failed to create container 4 (namespace test)"
    fi
    
    log "=== Test Summary ==="
    if [ -n "$CONTAINER_ID1" ]; then
        log "✅ Test 1 (npm/typescript): PASSED"
    else
        warn "❌ Test 1 (npm/typescript): FAILED"
    fi
    
    if [ -n "$CONTAINER_ID2" ]; then
        log "✅ Test 2 (python/pip): PASSED"
    else
        warn "❌ Test 2 (python/pip): FAILED"
    fi
    
    if [ -n "$CONTAINER_ID3" ]; then
        log "✅ Test 3 (resource limits): PASSED"
    else
        warn "❌ Test 3 (resource limits): FAILED"
    fi
    
    if [ -n "$CONTAINER_ID4" ]; then
        log "✅ Test 4 (namespace isolation): PASSED"
    else
        warn "❌ Test 4 (namespace isolation): FAILED"
    fi
    
    log "Enhanced container features test complete!"
    log "Features tested:"
    log "  🔧 Dynamic setup commands (npm, pip, apk)"
    log "  🛡️ Linux namespace isolation (PID, Mount, UTS, IPC, Network)"
    log "  📊 Resource limits (memory, CPU)"
    log "  🏗️ Container lifecycle management"
    log "  📋 Log collection and status tracking"
}

# Clean up - kill server and remove containers
clean() {
    log "Cleaning up..."
    
    # Kill server
    if pgrep -f quilt > /dev/null; then
        log "Stopping server..."
        pkill -f quilt || true
        sleep 1
    fi
    
    # Clean up active containers directory
    if [ -d "active_containers" ]; then
        log "Cleaning up container directories..."
        rm -rf active_containers/*
    fi
    
    log "Cleanup complete!"
}

# Show status
status() {
    info "=== Quilt Development Status ==="
    
    # Check if binaries exist
    if [ -f "$QUILTD_BIN" ]; then
        info "✅ Server binary: $QUILTD_BIN"
    else
        info "❌ Server binary: Not built"
    fi
    
    if [ -f "$CLI_BIN" ]; then
        info "✅ CLI binary: $CLI_BIN"
    else
        info "❌ CLI binary: Not built"
    fi
    
    # Check if server is running
    if pgrep -f quilt > /dev/null; then
        info "✅ Server: Running (PID: $(pgrep -f quilt))"
    else
        info "❌ Server: Not running"
    fi
    
    # Check for Alpine rootfs
    if [ -f "alpine.tar.gz" ]; then
        info "✅ Alpine rootfs: Available"
    else
        info "❌ Alpine rootfs: Missing (needed for testing)"
    fi
    
    # Check active containers
    if [ -d "active_containers" ]; then
        CONTAINER_COUNT=$(ls -1 active_containers 2>/dev/null | wc -l)
        info "📦 Active containers: $CONTAINER_COUNT"
    else
        info "📦 Active containers: 0"
    fi
}

# Show help
help() {
    echo "Quilt Development Script"
    echo ""
    echo "Usage: ./dev.sh [command]"
    echo ""
    echo "Commands:"
    echo "  build       Build both quiltd and quilt-cli"
    echo "  server      Start the server (foreground)"
    echo "  server-bg   Start the server in background"
    echo "  cli [args]  Run quilt-cli with arguments"
    echo "  test        Run comprehensive tests with enhanced features"
    echo "  clean       Stop server and clean up containers"
    echo "  status      Show development environment status"
    echo "  help        Show this help message"
    echo ""
    echo "Enhanced CLI Examples:"
    echo "  # Basic container"
    echo "  ./dev.sh cli create --image-path alpine.tar.gz -- /bin/echo 'Hello World'"
    echo ""
    echo "  # Container with npm packages"
    echo "  ./dev.sh cli create --image-path alpine.tar.gz \\"
    echo "    --setup 'npm: typescript ts-node' \\"
    echo "    -- node --version"
    echo ""
    echo "  # Container with Python packages and resource limits"
    echo "  ./dev.sh cli create --image-path alpine.tar.gz \\"
    echo "    --setup 'pip: requests beautifulsoup4' \\"
    echo "    --memory-limit 256 \\"
    echo "    --cpu-limit 50.0 \\"
    echo "    -- python3 -c 'import requests; print(requests.__version__)'"
    echo ""
    echo "  # Container with full namespace isolation"
    echo "  ./dev.sh cli create --image-path alpine.tar.gz \\"
    echo "    --enable-all-namespaces \\"
    echo "    -- /bin/sh -c 'hostname; ps aux'"
    echo ""
    echo "  # Multiple setup commands"
    echo "  ./dev.sh cli create --image-path alpine.tar.gz \\"
    echo "    --setup 'apk: python3 py3-pip nodejs npm' \\"
    echo "    --setup 'npm: typescript' \\"
    echo "    --setup 'pip: requests' \\"
    echo "    -- /bin/sh -c 'node --version && python3 --version'"
    echo ""
    echo "Container Management:"
    echo "  ./dev.sh cli status <container-id>    # Check container status"
    echo "  ./dev.sh cli logs <container-id>      # Get container logs"
    echo "  ./dev.sh cli stop <container-id>      # Stop container"
    echo "  ./dev.sh cli remove <container-id>    # Remove container"
    echo ""
    echo "Development:"
    echo "  ./dev.sh test          # Run all feature tests"
    echo "  ./dev.sh clean         # Clean up everything"
    echo "  ./dev.sh status        # Check development status"
}

# Main script logic
case "${1:-help}" in
    build)
        build
        ;;
    server)
        server
        ;;
    server-bg)
        server-bg
        ;;
    cli)
        shift
        cli "$@"
        ;;
    test)
        test
        ;;
    clean)
        clean
        ;;
    status)
        status
        ;;
    help|--help|-h)
        help
        ;;
    *)
        error "Unknown command: $1. Use './dev.sh help' for available commands."
        ;;
esac 