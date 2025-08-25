# Quilt

Rust container runtime with SQLite-based sync engine.

## Overview

Quilt is a container runtime that uses Linux namespaces and cgroups for isolation. It provides a gRPC API for container management and a CLI client for interaction.

## Architecture

- **quilt**: gRPC server daemon managing containers
- **cli**: Command-line client
- **SQLite backend**: Non-blocking state management
- **Namespaces**: PID, mount, UTS, IPC, network isolation
- **Cgroups**: Memory and CPU resource limits

## Requirements

- Linux kernel with namespace support
- cgroup v1 or v2
- Rust 1.70+
- gcc
- pkg-config
- protobuf compiler

## Build

```bash
cargo build --release
```

## Usage

### Start Server
```bash
./target/release/quilt
```

### Generate Container Image
```bash
./scripts/dev.sh generate minimal
```

### Create Container
```bash
./target/release/cli create \
  --image-path ./nixos-minimal.tar.gz \
  --memory-limit 512 \
  --cpu-limit 50.0 \
  --enable-all-namespaces \
  -- /bin/sh -c "echo hello"
```

### Container Operations
```bash
# Status
./target/release/cli status <container-id>

# Logs
./target/release/cli logs <container-id>

# Execute command
./target/release/cli exec <container-id> <command>

# Stop
./target/release/cli stop <container-id>

# Remove
./target/release/cli remove <container-id>
```

### Inter-Container Communication
```bash
# Ping between containers
./target/release/cli icc ping <container-1> <container-2>

# Execute via ICC
./target/release/cli icc exec <container-id> <command>
```

## Testing

```bash
# Basic functionality tests
./tests/test_container_functionality.sh       # Core container features (~18s)
./tests/test_sync_engine.sh                   # SQLite sync engine
./tests/test_icc.sh                           # Inter-container communication

# Advanced tests
./tests/test_runtime_downloads.sh             # Real software downloads (~25s)
./tests/test_volumes_comprehensive.sh         # Volume functionality
./tests/test_production_containers.sh         # Production readiness

# Development helper
./scripts/dev.sh test                          # Run comprehensive test suite
```

## Project Structure

```
src/
├── main.rs           # gRPC server
├── cli/              # CLI client
├── daemon/           # Container runtime
├── sync/             # SQLite state engine
├── icc/              # Inter-container communication
└── utils/            # Shared utilities

proto/
└── quilt.proto       # gRPC service definitions

scripts/
└── dev.sh            # Development helper script

tests/                # Test scripts
```

## Performance

- Container creation: ~200ms
- Status queries: <1ms
- Command execution: <10ms
- Supports parallel container operations

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/name`)
3. Commit changes (`git commit -am 'Add feature'`)
4. Push to branch (`git push origin feature/name`)
5. Create Pull Request

### Guidelines

- Run `cargo fmt` before committing
- Run `cargo clippy` and fix warnings
- Add tests for new features
- Update documentation as needed
- Keep commits focused and atomic

## License

MIT OR Apache-2.0
