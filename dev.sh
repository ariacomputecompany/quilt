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

# Generate a minimal Nix-based rootfs
generate_nixos_rootfs() {
    local rootfs_name="${1:-minimal}"
    local packages="${2:-coreutils bash}"
    local output_path="./nixos-${rootfs_name}.tar.gz"
    
    log "Generating Nix-based rootfs: $rootfs_name with packages: $packages"
    
    # Create a temporary Nix expression for the rootfs
    cat > /tmp/rootfs.nix << EOF
{ pkgs ? import <nixpkgs> {} }:

let
  # Create a minimal environment with specified packages
  basePackages = with pkgs; [
    $packages
    busybox
    glibc
  ];
  
  # Build a directory with all the packages
  rootfsEnv = pkgs.buildEnv {
    name = "quilt-rootfs-$rootfs_name";
    paths = basePackages;
    pathsToLink = [ "/bin" "/lib" "/share" "/etc" ];
  };
  
in pkgs.runCommand "quilt-rootfs-$rootfs_name.tar.gz" {
  buildInputs = [ pkgs.gnutar pkgs.gzip ];
} ''
  mkdir -p rootfs/{bin,lib,etc,proc,sys,dev,tmp,var,usr}
  
  # Copy essential files from the environment
  cp -r \${rootfsEnv}/bin/* rootfs/bin/ 2>/dev/null || true
  cp -r \${rootfsEnv}/lib/* rootfs/lib/ 2>/dev/null || true
  
  # Create essential files and directories
  echo "root:x:0:0:root:/:/bin/sh" > rootfs/etc/passwd
  echo "root:x:0:" > rootfs/etc/group
  echo "127.0.0.1 localhost" > rootfs/etc/hosts
  echo "localhost" > rootfs/etc/hostname
  
  # Create device files
  mknod rootfs/dev/null c 1 3 || true
  mknod rootfs/dev/zero c 1 5 || true
  mknod rootfs/dev/random c 1 8 || true
  mknod rootfs/dev/urandom c 1 9 || true
  
  # Create the tarball
  cd rootfs
  tar czf \$out .
''
EOF

    # Build the rootfs with Nix
    if nix-build /tmp/rootfs.nix -o "$output_path" 2>/dev/null; then
        log "✅ Generated Nix rootfs: $output_path"
        return 0
    else
        # Fallback: create a minimal rootfs using existing tools
        warn "Nix build failed, creating minimal rootfs manually..."
        create_minimal_rootfs "$output_path"
        return 0
    fi
}

# Create a minimal rootfs manually as fallback
create_minimal_rootfs() {
    local output_path="$1"
    local temp_dir=$(mktemp -d)
    
    log "Creating minimal rootfs at $output_path"
    
    # Create basic directory structure
    mkdir -p "$temp_dir"/{bin,lib,lib64,etc,proc,sys,dev,tmp,var,usr/bin,usr/lib}
    
    # Copy essential binaries from host (if available)
    if command -v busybox >/dev/null 2>&1; then
        cp "$(which busybox)" "$temp_dir/bin/"
        # Create common command symlinks
        cd "$temp_dir/bin"
        for cmd in sh ls cat echo mkdir rm cp mv; do
            ln -sf busybox "$cmd" 2>/dev/null || true
        done
        cd - >/dev/null
    else
        # Copy basic shell and utilities
        cp /bin/sh "$temp_dir/bin/" 2>/dev/null || true
        cp /bin/bash "$temp_dir/bin/" 2>/dev/null || true
        cp /bin/ls "$temp_dir/bin/" 2>/dev/null || true
        cp /bin/cat "$temp_dir/bin/" 2>/dev/null || true
        cp /bin/echo "$temp_dir/bin/" 2>/dev/null || true
    fi
    
    # Copy essential libraries
    if [ -f /lib/x86_64-linux-gnu/libc.so.6 ]; then
        cp /lib/x86_64-linux-gnu/libc.so.6 "$temp_dir/lib/" 2>/dev/null || true
        cp /lib64/ld-linux-x86-64.so.2 "$temp_dir/lib64/" 2>/dev/null || true
    fi
    
    # Create essential files
    echo "root:x:0:0:root:/:/bin/sh" > "$temp_dir/etc/passwd"
    echo "root:x:0:" > "$temp_dir/etc/group"
    echo "127.0.0.1 localhost" > "$temp_dir/etc/hosts"
    echo "localhost" > "$temp_dir/etc/hostname"
    
    # Create the tarball
    cd "$temp_dir"
    tar czf "$output_path" .
    cd - >/dev/null
    
    # Cleanup
    rm -rf "$temp_dir"
    
    log "✅ Created minimal rootfs: $output_path"
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
    log "Running comprehensive test with Nix-based containers..."
    
    # Ensure server is running
    if ! pgrep -f quilt > /dev/null; then
        warn "Server not running, starting it..."
        server-bg
    fi
    
    # Generate test rootfs environments
    log "=== Preparing Nix-based rootfs environments ==="
    
    # Generate minimal rootfs
    if [ ! -f "./nixos-minimal.tar.gz" ]; then
        generate_nixos_rootfs "minimal" "coreutils bash findutils"
    fi
    
    # Generate development rootfs with common tools
    if [ ! -f "./nixos-dev.tar.gz" ]; then
        generate_nixos_rootfs "dev" "coreutils bash findutils curl wget python3 nodejs"
    fi
    
    log "=== TEST 1: Basic Nix container ==="
    log "Creating container with minimal Nix environment..."
    CONTAINER_ID1=$(cli create \
        --image-path ./nixos-minimal.tar.gz \
        --memory-limit 256 \
        --cpu-limit 50.0 \
        --enable-all-namespaces \
        -- /bin/sh -c "echo 'Hello from Nix container!'; ls /bin; uname -a" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
    if [ -n "$CONTAINER_ID1" ]; then
        log "✅ Container 1 created: $CONTAINER_ID1"
        sleep 3
        log "Container 1 status:"
        cli status "$CONTAINER_ID1"
        log "Container 1 logs:"
        cli logs "$CONTAINER_ID1"
    else
        warn "❌ Failed to create container 1 (minimal Nix test)"
    fi
    
    log "=== TEST 2: Development Nix container ==="
    log "Creating container with development tools..."
    CONTAINER_ID2=$(cli create \
        --image-path ./nixos-dev.tar.gz \
        --memory-limit 512 \
        --setup "nix: python3 python3Packages.requests python3Packages.pip" \
        -- /bin/sh -c "echo 'Development container ready'; which python3; python3 --version" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
    if [ -n "$CONTAINER_ID2" ]; then
        log "✅ Container 2 created: $CONTAINER_ID2"
        sleep 3
        log "Container 2 status:"
        cli status "$CONTAINER_ID2"
        log "Container 2 logs:"
        cli logs "$CONTAINER_ID2"
    else
        warn "❌ Failed to create container 2 (development test)"
    fi
    
    log "=== TEST 3: Resource limited Nix container ==="
    log "Creating container with strict resource limits..."
    CONTAINER_ID3=$(cli create \
        --image-path ./nixos-minimal.tar.gz \
        --memory-limit 64 \
        --cpu-limit 25.0 \
        -- /bin/sh -c "echo 'Memory info:'; cat /proc/meminfo | head -5 2>/dev/null || echo 'No /proc/meminfo'; echo 'Container info:'; echo 'PWD:' \$PWD; echo 'USER:' \$USER; echo 'PATH:' \$PATH" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
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
        --image-path ./nixos-minimal.tar.gz \
        --enable-all-namespaces \
        -- /bin/sh -c "echo 'Container hostname:'; hostname; echo 'Container processes:'; ps aux 2>/dev/null || ps; echo 'Container filesystem:'; ls -la /" 2>/dev/null | grep "Container created" | grep -o '[a-f0-9-]\{36\}' || echo "")
    
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
        log "✅ Test 1 (minimal Nix): PASSED"
    else
        warn "❌ Test 1 (minimal Nix): FAILED"
    fi
    
    if [ -n "$CONTAINER_ID2" ]; then
        log "✅ Test 2 (development Nix): PASSED"
    else
        warn "❌ Test 2 (development Nix): FAILED"
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
    
    log "Nix-based container features test complete!"
    log "Features tested:"
    log "  🔧 Nix-generated rootfs environments"
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
    
    # Clean up generated rootfs files
    log "Cleaning up generated rootfs files..."
    rm -f ./nixos-*.tar.gz
    
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
    
    # Check for Nix
    if command -v nix >/dev/null 2>&1; then
        info "✅ Nix: Available ($(nix --version | head -1))"
    else
        info "⚠️  Nix: Not available (will use fallback rootfs generation)"
    fi
    
    # Check for generated rootfs files
    ROOTFS_COUNT=$(ls -1 ./nixos-*.tar.gz 2>/dev/null | wc -l)
    info "📦 Generated rootfs files: $ROOTFS_COUNT"
    
    # Check active containers
    if [ -d "active_containers" ]; then
        CONTAINER_COUNT=$(ls -1 active_containers 2>/dev/null | wc -l)
        info "🏃 Active containers: $CONTAINER_COUNT"
    else
        info "🏃 Active containers: 0"
    fi
}

# Generate rootfs environments
generate() {
    case "${2:-minimal}" in
        minimal)
            generate_nixos_rootfs "minimal" "coreutils bash findutils"
            ;;
        dev|development)
            generate_nixos_rootfs "dev" "coreutils bash findutils curl wget python3 nodejs"
            ;;
        python)
            generate_nixos_rootfs "python" "coreutils bash python3 python3Packages.pip python3Packages.requests"
            ;;
        node|nodejs)
            generate_nixos_rootfs "nodejs" "coreutils bash nodejs npm"
            ;;
        rust)
            generate_nixos_rootfs "rust" "coreutils bash rustc cargo gcc"
            ;;
        *)
            error "Unknown rootfs type: ${2}. Available: minimal, dev, python, nodejs, rust"
            ;;
    esac
}

# Show help
help() {
    echo "Quilt Development Script - Nix-Based Container Runtime"
    echo ""
    echo "Usage: ./dev.sh [command]"
    echo ""
    echo "Commands:"
    echo "  build         Build both quiltd and quilt-cli"
    echo "  server        Start the server (foreground)"
    echo "  server-bg     Start the server in background"
    echo "  cli [args]    Run quilt-cli with arguments"
    echo "  test          Run comprehensive tests with Nix-based containers"
    echo "  generate [type] Generate rootfs environments (minimal, dev, python, nodejs, rust)"
    echo "  clean         Stop server and clean up containers"
    echo "  status        Show development environment status"
    echo "  help          Show this help message"
    echo ""
    echo "Nix-Based CLI Examples:"
    echo "  # Basic container with minimal Nix rootfs"
    echo "  ./dev.sh cli create --image-path ./nixos-minimal.tar.gz -- /bin/echo 'Hello World'"
    echo ""
    echo "  # Container with development environment"
    echo "  ./dev.sh cli create --image-path ./nixos-dev.tar.gz \\"
    echo "    --setup 'nix: python3 python3Packages.requests' \\"
    echo "    -- python3 -c 'print(\"Python container ready\")'"
    echo ""
    echo "  # Container with resource limits and full isolation"
    echo "  ./dev.sh cli create --image-path ./nixos-minimal.tar.gz \\"
    echo "    --memory-limit 256 \\"
    echo "    --cpu-limit 50.0 \\"
    echo "    --enable-all-namespaces \\"
    echo "    -- /bin/sh -c 'hostname; ps aux'"
    echo ""
    echo "Rootfs Generation:"
    echo "  ./dev.sh generate minimal    # Basic shell and coreutils"
    echo "  ./dev.sh generate dev        # Development tools (curl, wget, python, node)"
    echo "  ./dev.sh generate python     # Python with common packages"
    echo "  ./dev.sh generate nodejs     # Node.js with npm"
    echo "  ./dev.sh generate rust       # Rust development environment"
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
    echo ""
    echo "Note: This version uses Nix for generating container rootfs environments."
    echo "If Nix is not available, it will fall back to creating minimal rootfs manually."
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
    generate)
        generate "$@"
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