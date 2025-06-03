#!/bin/bash

# Quilt Development Script
# Usage: ./dev.sh [command]

set -e

QUILTD_BIN="./target/x86_64-unknown-linux-gnu/debug/quilt"
CLI_BIN="./quilt-cli/target/debug/quilt-cli"

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
    log "Running quick test..."
    
    # Ensure server is running
    if ! pgrep -f quilt > /dev/null; then
        warn "Server not running, starting it..."
        server-bg
    fi
    
    # Check if alpine.tar.gz exists
    if ! [ -f "alpine.tar.gz" ]; then
        error "alpine.tar.gz not found. Please ensure you have an Alpine rootfs tarball."
    fi
    
    log "Creating test container..."
    CONTAINER_ID=$(cli create --image-tarball-path alpine.tar.gz -- /bin/echo "Hello from Quilt dev test!" | grep "Container created successfully" | grep -o '[a-f0-9-]\{36\}')
    
    if [ -z "$CONTAINER_ID" ]; then
        error "Failed to create container"
    fi
    
    log "Container created: $CONTAINER_ID"
    
    # Wait a moment for execution
    sleep 3
    
    log "Checking container status..."
    cli status "$CONTAINER_ID"
    
    log "Getting container logs..."
    cli logs "$CONTAINER_ID"
    
    log "Test complete!"
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
    echo "  test        Run a quick test (create container, check status, get logs)"
    echo "  clean       Stop server and clean up containers"
    echo "  status      Show development environment status"
    echo "  help        Show this help message"
    echo ""
    echo "Examples:"
    echo "  ./dev.sh build"
    echo "  ./dev.sh server-bg"
    echo "  ./dev.sh cli create --image-tarball-path alpine.tar.gz -- /bin/echo 'Hello World'"
    echo "  ./dev.sh test"
    echo "  ./dev.sh clean"
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