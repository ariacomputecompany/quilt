# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Development Commands

### Build Commands
```bash
# Build debug binaries (server and CLI)
cargo build

# Build release binaries (optimized)
cargo build --release

# Build with specific target
cargo build --release --target x86_64-unknown-linux-gnu
```

### Running the System
```bash
# Start the Quilt server (daemon)
./target/debug/quilt
# or for release:
./target/release/quilt

# Use CLI to interact with server
./target/debug/cli [command]
# or:
cargo run --bin cli [command]
```

### Testing Commands
All test scripts are located in the `tests/` directory:

```bash
# Basic functionality test (~18s, always exits 0)
./tests/test_container_functionality.sh

# Runtime test with real software downloads (~25s)
./tests/test_runtime_downloads.sh

# Test sync engine performance
./tests/test_sync_engine.sh

# Test Inter-Container Communication (ICC)
./tests/test_icc.sh

# Production readiness test
./tests/test_production_containers.sh

# Volume functionality tests
./tests/test_volumes_comprehensive.sh

# Stress tests
./tests/stress_test_full_e2e.sh
./tests/stress_test_icc.sh
./tests/stress_test_network_baseline.sh
```

### Running Rust Tests
```bash
# Run all Rust tests
cargo test

# Run sync engine tests only
cargo test sync::

# Run a specific test
cargo test sync::engine::tests::test_container_lifecycle_integration

# Run tests with output
cargo test -- --nocapture
```

### Code Quality Commands
```bash
# Format code
cargo fmt

# Check linting
cargo clippy

# Type check without building
cargo check
```

### Development Helper
```bash
# Development script with various utilities
./dev.sh [command]

# Available dev.sh commands:
./dev.sh build                    # Build both binaries
./dev.sh server                   # Start server in foreground
./dev.sh server-bg                # Start server in background
./dev.sh cli [args]               # Run CLI with arguments
./dev.sh test                     # Run comprehensive tests
./dev.sh generate [type]          # Generate rootfs (minimal, dev, python, nodejs, rust)
./dev.sh clean                    # Stop server and cleanup
./dev.sh status                   # Show development status
```

## High-Level Architecture

### Core Design Decision: Non-Blocking Sync Engine
The project uses a SQLite-based sync engine to avoid blocking operations:
- **Problem**: Container process monitoring blocked the main server thread for hours, causing 5-30s timeouts
- **Solution**: All state operations now use async SQLite queries with <1ms response times
- **Impact**: Server remains responsive even with long-running containers

### System Components

1. **Quilt Server (`src/main.rs`)**: 
   - gRPC service implementation using Tonic
   - Manages container lifecycle through sync engine
   - Handles all container operations asynchronously

2. **CLI Client (`src/cli/`)**: 
   - Command-line interface for container management
   - Supports create, status, logs, stop, remove, exec, and ICC commands
   - Communicates with server via gRPC on port 50051

3. **Sync Engine (`src/sync/`)**: 
   - SQLite-based persistent state management
   - Background services for process monitoring and cleanup
   - Non-blocking container operations
   - Key files: `engine.rs`, `containers.rs`, `monitor.rs`

4. **Container Runtime (`src/daemon/`)**:
   - Linux namespace management (PID, mount, UTS, IPC, network)
   - Cgroup-based resource limits (v1/v2 support)
   - Container lifecycle management
   - Key files: `runtime.rs`, `namespace.rs`, `cgroup.rs`

5. **Inter-Container Communication (`src/icc/`)**:
   - Network bridge management for container-to-container communication
   - DNS resolution between containers
   - Key files: `network.rs`, `messaging.rs`, `dns.rs`

### Database Schema
The sync engine uses SQLite with tables for:
- `containers`: Container metadata and state
- `container_processes`: Process monitoring information  
- `container_logs`: Log storage
- `container_cleanup`: Cleanup task tracking
- `icc_registrations`: Inter-container communication registry
- `volumes`: Named volume management
- `container_mounts`: Container mount configurations
- `schema_migrations`: Database version management

### Key Architectural Patterns

1. **Async Everything**: All I/O operations are async using Tokio
2. **Database as Source of Truth**: No in-memory state caching that can block
3. **Background Services**: Separate tasks for monitoring, cleanup, and maintenance
4. **Fail-Fast Design**: Operations timeout quickly to prevent hanging
5. **Resource Cleanup**: Automatic cleanup of containers and resources

## Container Image Support

- Supports rootfs tarballs (`.tar.gz`)
- Users must generate their own container images
- Use `./dev.sh generate-rootfs` to create test images
- Automatic binary fixing for Nix-generated containers with broken symlinks
- Custom shell binary compiled during build for environments with broken symlinks

## Volume and Mount Support

### Mount Types Supported
- **Bind Mounts**: `-v /host/path:/container/path[:ro]`
- **Named Volumes**: `-v volume-name:/container/path` (auto-created in `/var/lib/quilt/volumes`)
- **Tmpfs**: `--mount type=tmpfs,target=/container/path,size=10m`

### Volume Commands
```bash
# Create container with bind mount
./target/debug/cli create \
  --image-path ./nixos-minimal.tar.gz \
  -v /host/path:/container/path \
  --async-mode

# Create with read-only mount
./target/debug/cli create \
  --image-path ./nixos-minimal.tar.gz \
  -v /host/path:/container/path:ro \
  --async-mode

# Create with named volume (auto-created)
./target/debug/cli create \
  --image-path ./nixos-minimal.tar.gz \
  -v my-data:/app/data \
  --async-mode

# Advanced mount syntax
./target/debug/cli create \
  --image-path ./nixos-minimal.tar.gz \
  --mount type=bind,source=/host/path,target=/container/path,readonly \
  --async-mode
```

## Protocol Buffers

- Service definitions in `proto/quilt.proto`
- Auto-generated with `tonic-build` during compilation
- Defines all gRPC service methods and message types

## CLI Commands

### Create Container
```bash
# First generate a container image if needed
./dev.sh generate-rootfs

# Create a container
./target/debug/cli create \
  --image-path ./nixos-minimal.tar.gz \
  --memory-limit 512 \
  --cpu-limit 50.0 \
  --enable-all-namespaces \
  -- /bin/sh -c "echo 'Hello World'"
```

### Container Operations
```bash
# Check container status
./target/debug/cli status <container-id>

# View container logs
./target/debug/cli logs <container-id>

# Stop a container
./target/debug/cli stop <container-id>

# Remove a container
./target/debug/cli remove <container-id> [--force]

# Execute command in container
./target/debug/cli exec <container-id> <command>

# Execute with output capture
./target/debug/cli exec <container-id> -c "ls -la" --capture-output
```

### Inter-Container Communication
```bash
# Ping between containers
./target/debug/cli icc ping <container-id-1> <container-id-2>

# Execute command via ICC
./target/debug/cli icc exec <container-id> <command>
```

## Important Implementation Details

- Memory management uses CString for proper lifetime handling
- Container creation averages ~200ms
- Process monitoring runs in detached Tokio tasks
- Network namespace requires special handling for container connectivity
- Cgroup v1/v2 compatibility is handled automatically
- Custom shell binary is built in `build.rs` for Nix environments
- Shell command execution in containers uses double quotes to allow redirects and pipes
- Volume security validation blocks path traversal and sensitive system paths
- Mounts are setup before chroot to ensure visibility in container

## Build Configuration

- **Linting**: Project uses `deny` warnings - all warnings must be fixed
- **Build profiles**: Optimized release profile with LTO and symbol stripping
- **Dependencies**: Uses Tokio for async runtime, SQLx for database, Tonic for gRPC
- **Binary targets**: `quilt` (server) and `cli` (client) defined in Cargo.toml