# Quilt

## Overview

Quilt is a lightweight container runtime built in Rust, designed specifically for agentic firmware environments. It provides a minimal yet powerful containerization solution optimized for efficiency and security.

## Architecture

### Core Components

1. **quiltd** - The container runtime daemon
   - gRPC server for container management
   - Linux namespace and cgroup management
   - Container lifecycle orchestration

2. **quilt-cli** - Command-line interface
   - Container creation and management
   - Real-time status monitoring
   - Log streaming capabilities

### System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                           Host System                            │
├─────────────────────────────────────────────────────────────────┤
│                        Quilt Runtime                           │
│  ┌───────────────┐ ┌───────────────┐ ┌───────────────────────┐   │
│  │   quiltd      │ │ quilt-cli     │ │  Container Manager    │   │
│  │  (gRPC API)   │ │ (CLI Client)  │ │                       │   │
│  └───────────────┘ └───────────────┘ └───────────────────────┘   │
├─────────────────────────────────────────────────────────────────┤
│                     Container Layer                            │
│  ┌───────────────────────────────────────────────────────────┐   │
│  │                Container Instance                         │   │
│  │  ┌─────────────┐ ┌─────────────┐ ┌─────────────────────┐ │   │
│  │  │  Firmware   │ │    OS       │ │   Application       │ │   │
│  │  │   Layer     │ │             │ │     Logic           │ │   │
│  │  └─────────────┘ └─────────────┘ └─────────────────────┘ │   │
│  └───────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### Container Runtime Components

•	**Container Runtime**: The core Quilt runtime (`quiltd`) that manages container lifecycle
•	**Image Management**: Support for rootfs tarballs and container images
•	**Operating System**: Linux with namespace and cgroup support
•	**Namespace Isolation**: Process, mount, network, UTS, and IPC isolation
•	**Resource Management**: CPU, memory, and process limits via cgroups

## Features

### Core Container Management
- **Container Lifecycle**: Create, start, stop, remove containers
- **Image Support**: Rootfs tarball extraction and management
- **Process Isolation**: Linux namespaces (PID, Mount, UTS, IPC, Network)
- **Resource Limits**: CPU, memory, and process count controls via cgroups

### Advanced Features
- **Dynamic Runtime Setup**: Automatic package installation (npm, pip, gem, etc.)
- **Log Management**: Real-time log capture and streaming
- **State Tracking**: Comprehensive container state monitoring
- **Security**: Namespace isolation and resource containment

### Developer Experience
- **gRPC API**: High-performance binary protocol
- **CLI Interface**: Intuitive command-line tools
- **Real-time Monitoring**: Live status and log streaming
- **Nix Integration**: Native Nix development environment support

## Usage Examples

### Basic Container Operations

```bash
# Create and start a container
quilt-cli create --image /path/to/rootfs.tar.gz --command "echo hello world"

# Check container status  
quilt-cli status <container-id>

# View container logs
quilt-cli logs <container-id>

# Stop a container
quilt-cli stop <container-id>

# Remove a container
quilt-cli remove <container-id>
```

### Advanced Container Creation

```bash
# Container with environment variables and setup commands
quilt-cli create \
  --image /path/to/node-rootfs.tar.gz \
  --env "NODE_ENV=production" \
  --env "PORT=3000" \
  --setup "npm: typescript @types/node" \
  --memory-limit 512 \
  --cpu-limit 50.0 \
  --enable-all-namespaces \
  -- node server.js
```

### Runtime Environment Setup

The runtime supports dynamic package installation:

```bash
# Node.js development environment
quilt-cli create \
  --image /path/to/base.tar.gz \
  --setup "npm: typescript ts-node @types/node" \
  -- ts-node app.ts

# Python data science environment  
quilt-cli create \
  --image /path/to/python.tar.gz \
  --setup "pip: pandas numpy matplotlib" \
  -- python analysis.py

# Multi-runtime environment
quilt-cli create \
  --image /path/to/base.tar.gz \
  --setup "npm: webpack" \
  --setup "pip: flask" \
  -- /bin/sh -c "npm run build && python app.py"
```

## Implementation Details

### Container Creation Process

1. **Initialization**: Parse container configuration and validate inputs
2. **Environment Setup**: The runtime assesses requirements and prepares the environment
3. **Namespace Creation**: Set up Linux namespaces for isolation
4. **Rootfs Setup**: Extract and prepare the container filesystem
5. **Process Execution**: Execute the specified command within the container
6. **Monitoring**: Track container state and capture logs

### Resource Management

Quilt uses Linux cgroups to enforce resource limits:

- **Memory Limits**: Configurable memory usage caps
- **CPU Limits**: CPU share allocation and quotas  
- **Process Limits**: Maximum number of processes/threads
- **Automatic Cleanup**: Resources freed when containers terminate

### Security Model

- **Namespace Isolation**: Each container runs in isolated namespaces
- **Resource Containment**: Cgroups prevent resource exhaustion
- **Minimal Attack Surface**: Lightweight runtime with focused functionality
- **Process Isolation**: Containers cannot interfere with each other or the host

## Development

### Building from Source

```bash
# Using Nix (recommended)
nix develop
cargo build --release

# Or with standard Rust toolchain
cargo build --release
```

### Running Tests

```bash
cargo test
```

### Development Environment

Quilt includes a comprehensive Nix flake for development:

```bash
# Enter development shell
nix develop

# Build the runtime
nix build .#quiltd

# Build the CLI
nix build .#quilt-cli
```

## License

Quilt is released under the MIT License. See LICENSE file for details.