# PLAN.md: Building Quilt

This document outlines the development plan for Quilt, a lightweight container runtime for the agentic firmware. Development will occur on a Linux environment (e.g., a DigitalOcean droplet running a suitable Linux distribution like Alpine or Ubuntu).

## Phase 0: Environment Setup & Project Initialization

1.  ✅ **Provision Linux VM**: Set up the DigitalOcean droplet.
    *   Choose a Linux distribution (Alpine recommended for target environment similarity, but Ubuntu is also fine for development ease).
    *   Ensure SSH access is configured.
2.  ✅ **Install Rust**: Install Rust and Cargo on the VM using `rustup`.
    *   `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3.  ✅ **Install Essential Build Tools**: Install `build-essential` (or equivalent for your chosen distro) for C compilers, linker, etc., which might be needed by some Rust crates or for `musl` compilation if targeting Alpine directly.
    *   Ubuntu: `sudo apt update && sudo apt install build-essential protobuf-compiler`
    *   Alpine: `sudo apk add build-base protobuf-dev` (Note: `protobuf-compiler` is `protoc`, `protobuf-dev` for libs)
4.  ✅ **Install Protocol Buffer Compiler (`protoc`)**: Required for gRPC.
    *   Ensure it's installed and accessible in your `PATH`.
5.  ✅ **Initialize Rust Project (`quilt`)**: Create a new Rust binary project for `quiltd`.
    *   `cargo new quilt --bin`
    *   `cd quilt`
6.  ✅ **Initialize `quilt-cli` Project**: This could be a separate crate within a Cargo workspace, or a separate project for now.
    *   (Optional, can be done later) `cargo new quilt-cli --bin`
7.  ✅ **Version Control**: Initialize a Git repository.
    *   `git init`

## Phase 1: API Definition & Basic gRPC Service (quiltd)

1.  ✅ **Define gRPC Service (`quilt.proto`)**:
    *   Create a `proto/quilt.proto` file.
    *   Define services (e.g., `QuiltService`).
    *   Define RPC methods (e.g., `CreateContainer`, `GetContainerStatus`, `GetContainerLogs`, `StopContainer`, `RemoveContainer`).
    *   Define message types for requests and responses (e.g., `CreateContainerRequest`, `ContainerStatusResponse`, `LogStreamResponse`). Include fields for image source (path to tarball), command, environment variables, resource limits (basic), container ID, status, logs, etc.
2.  ✅ **Add Dependencies to `quiltd/Cargo.toml`**:
    *   `tonic` (for gRPC server).
    *   `prost` (for Protocol Buffer implementation).
    *   `tokio` (for asynchronous runtime).
    *   `tonic-build` (for compiling `.proto` files in `build.rs`).
    *   `nix` (for Linux syscalls - will be used heavily later).
    *   `uuid` (for generating container IDs).
    *   `serde`, `serde_json` (for any internal JSON parsing/serialization if needed, and for the optional HTTP API later).
3.  ✅ **Set up `build.rs` for `quiltd`**: To compile the `.proto` file into Rust code using `tonic-build`.
4.  ✅ **Implement Basic gRPC Server (`quiltd/src/main.rs`)**:
    *   Create stubs for the gRPC service methods.
    *   Initialize the Tokio runtime and start the gRPC server listening on a Unix Domain Socket (UDS).
    *   For now, methods can just log the request and return dummy responses (e.g., generate a UUID for container ID, return "PENDING" status).
5.  ✅ **Initial State Management (In-Memory for `quiltd`)**:
    *   Use `std::collections::HashMap` wrapped in `Arc<Mutex<...>>` (or `tokio::sync::Mutex`) to store container configurations and their states (e.g., `HashMap<String, ContainerState>`).

## Phase 2: Core Containerization Logic (quiltd)

This is the most complex phase and will be iterative.

1.  ✅ **Container ID Generation**: Implement robust unique ID generation (e.g., UUIDs).
2.  ✅ **Root Filesystem Setup (Initial)**:
    *   Mechanism to use a base rootfs tarball (e.g., Alpine mini rootfs). The `CreateContainerRequest` will specify the path to this tarball.
    *   Logic to create a temporary directory for the container and unpack the tarball into it.
    *   Use `pivot_root` (or `chroot` as a simpler, less isolated starting point, then move to `pivot_root`) to change the container's root filesystem. This will require `nix` crate for syscalls.

**✅ \[Checkpoint: Transition to Alpine Linux Focus]**

*   ✅ **Target Environment Alignment**: Before proceeding with deep syscall integrations (namespaces, `pivot_root`, cgroups), prioritize setting up an Alpine Linux test environment (VM or container).
*   ✅ **Musl Compilation**: Configure the Rust toolchain for `x86_64-unknown-linux-musl` cross-compilation (or native compilation if developing directly on Alpine). Ensure `quiltd` can be built as a statically linked `musl` binary if possible, or that all dependencies are compatible.
*   ✅ **Iterative Testing on Alpine**: All subsequent features in Phase 2 should be regularly tested on the Alpine environment to catch compatibility issues early. The goal is to ensure `quiltd` behaves as expected on the target platform.

3.  **Namespace Creation (`nix` crate)**:
    *   **PID Namespace (`CLONE_NEWPID`)**: Isolate process IDs.
    *   **Mount Namespace (`CLONE_NEWNS`)**: Isolate filesystem mount points. Ensure `/proc` is remounted correctly within the new namespace.
    *   **UTS Namespace (`CLONE_NEWUTS`)**: Isolate hostname and domain name.
    *   **IPC Namespace (`CLONE_NEWIPC`)**: Isolate inter-process communication resources.
    *   **Network Namespace (`CLONE_NEWNET`)**: (Start with no networking or just loopback, then add basic bridge/veth pair later if needed).
    *   Use the `clone` syscall via the `nix` crate with appropriate flags.
4.  ✅ **Process Execution**: Fork a child process (`fork` from `nix`) after setting up namespaces.
    *   The child process will perform `pivot_root`/`chroot` and then `execvp` to run the user-specified command from `CreateContainerRequest`.
    *   The parent process (in `quiltd`) will monitor the child.
5.  **Cgroup Management (Initial - Manual or via `cgroups-rs` crate)**:
    *   Create cgroups for CPU, memory, and PIDs for each container.
    *   Set basic limits based on `CreateContainerRequest`.
    *   Add the container's main process to these cgroups.
    *   Explore the `cgroups-rs` crate or interact with the cgroup filesystem directly.
6.  ✅ **Log Collection**: Capture `stdout` and `stderr` from the containerized process.
    *   Use pipes to redirect output from the child process back to `quiltd`.
    *   Implement the `GetContainerLogs` gRPC method (initially simple buffering, later streaming).
7.  ✅ **Container Lifecycle & State Updates**: Update the in-memory state of containers (e.g., `RUNNING`, `EXITED (code)`, `FAILED`).
    *   Handle process termination and cleanup.
8.  **Setup Commands**: Implement logic to run setup commands (e.g., `apk add ...`) inside the container *before* the main tool command. This means `exec`-ing these commands sequentially within the container's environment.

## Phase 3: `quilt-cli` Development

1.  ✅ **Add Dependencies to `quilt-cli/Cargo.toml`**:
    *   `tonic` (for gRPC client).
    *   `prost`.
    *   `tokio`.
    *   `clap` (for command-line argument parsing).
2.  ✅ **Set up `build.rs` for `quilt-cli`**: To compile the same `.proto` file.
3.  ✅ **Implement CLI Commands**:
    *   `quilt-cli create --image <path_to_tarball> --command "/bin/sh -c 'echo hello'"`
    *   `quilt-cli status <container_id>`
    *   `quilt-cli logs <container_id>`
    *   `quilt-cli stop <container_id>`
    *   `quilt-cli rm <container_id>`
    *   These commands will use the generated gRPC client to communicate with `quiltd` over the UDS.

## Phase 4: Testing & Refinement

1.  **Unit Tests**: For individual functions and modules (e.g., parsing, state updates).
2.  **Integration Tests**: Test the CLI against the `quiltd` daemon for full workflows.
    *   Create a container, check its status, get logs, stop it, remove it.
    *   Test with various commands and base images.
    *   Test resource limiting (if basic cgroups are implemented).
    *   Test behavior with failing commands or invalid setup.
3.  **Error Handling**: Robust error handling and reporting through gRPC status codes and messages.
4.  **Security Considerations (Initial Review)**:
    *   Permissions of `quiltd` (needs to run as root or have `CAP_SYS_ADMIN`, etc., for namespaces/cgroups).
    *   Review potential vulnerabilities related to input parsing (image paths, commands).
    *   (Later phases: `seccomp` filtering, AppArmor/SELinux profiles, user namespaces for rootless containers).
5.  **Performance Profiling**: Basic profiling once core features are working to identify bottlenecks.

## Phase 5: Advanced Features & Long-Term Maintenance (Future)

1.  **Optional HTTP API**: Implement the secondary HTTP REST API for `quiltd` for broader accessibility, possibly using `axum` or `actix-web` and mapping to the gRPC service logic.
2.  **Advanced Image Management**: Support for OCI image formats, pulling from registries (e.g., using `oci-distribution` and `skopeo` concepts or libraries).
3.  **Advanced Networking**: More sophisticated container networking (e.g., virtual Ethernet pairs, bridging).
4.  **Volume Mounting**: Allow mounting host directories or persistent volumes into containers.
5.  **Resource Monitoring**: Expose more detailed resource usage metrics.
6.  **User Namespaces**: For running containers as non-root users on the host, enhancing security.
7.  **Daemonization & Service Management**: Proper daemonization of `quiltd` and integration with systemd or OpenRC for production-like deployment on the firmware.
8.  **Documentation**: Comprehensive documentation for users and developers.

## Development Principles

*   **Iterative Development**: Build and test features incrementally.
*   **Focus on Core Functionality First**: Get basic container creation, execution, and isolation working before adding complex features.
*   **Security-Minded**: Keep security implications in mind throughout development.
*   **Test Thoroughly**: Especially for system-level code involving namespaces and cgroups.
*   **Keep it Lightweight**: Adhere to the primary goal of a minimal and fast runtime.

This plan provides a roadmap. Details within each phase will be refined as development progresses. 