# Quilt OSS

`quilt-core` is [Quilt](https://quilt.sh)'s open-source runtime: container ops, icc networking, oci image workflow, sync/state engine, and local operational tooling.


## Scope

Included in OSS:
- gRPC daemon (`quilt`) and CLI (`cli`)
- Linux container runtime primitives (namespaces, cgroups, process supervision)
- SQLite-backed sync/state engine
- ICC networking stack (bridge, veth, DNS manager, firewall/netlink path)
- Image/registry workflows and runtime support modules
- Volume and metrics paths
- Fozzy scenarios and verification tooling

## Requirements

- Linux host (cgroups + namespaces enabled)
- Rust toolchain (edition 2021; recent stable recommended)
- `protoc` (for gRPC/protobuf codegen)
- common build tools (`gcc`, `pkg-config`)

## Build

```bash
cargo build --release
```

Binaries:
- `target/release/quilt`
- `target/release/cli`
- `target/release/minit`

## Run

Start daemon:

```bash
./target/release/quilt
```

Basic CLI flow:

```bash
./target/release/cli --help
./target/release/cli list
./target/release/cli images
```

Optional local stack:

```bash
docker compose up --build
```

## Cluster Management with `quiltc`

`quilt-core` focuses on single-runtime and local container operations. For Kubernetes-style cluster workflows, use [`quiltc`](https://github.com/ariacomputecompany/quiltc).

Use `quiltc` when you need:
- cluster/node/workload lifecycle management
- replica orchestration across multiple nodes
- placement and rescheduling behavior
- distributed agent registration, heartbeat, and status reporting

High-level mapping:
- Workload (replicas) ~= Deployment/ReplicaSet
- Placement (`replica_index -> node`) ~= Pod scheduled to a Node
- Agent register/heartbeat/report ~= node lifecycle and status flow

## Optional GUI Workloads (`qgui`)

If you need browser-accessible desktop apps inside a container, use `qgui` in a GUI-capable image (for example `prod-gui`).

Typical flow:

```bash
# 1) Start GUI services in the container
./quilt.sh exec <container_id> "qgui up"

# 2) Launch a GUI app on the container display
./quilt.sh exec <container_id> "apk add --no-cache xeyes xclock && DISPLAY=:1 xeyes & DISPLAY=:1 xclock &"

# 3) Request a signed URL and open it immediately in browser
curl -sS -H "Authorization: Bearer $QUILT_API_KEY" \
  "$QUILT_API_URL/api/containers/<container_id>/gui-url"
```

Notes:
- `qgui status` reports xvfb/vnc/websockify health.
- `/gui/<id>/` may return `401` in direct API-key flows; use the signed `gui_url` response.

## Testing

[Fozzy](https://github.com/ariacomputecompany/fozzy) is the primary verification path in this repo.

Recommended full gate:

```bash
fozzy full \
  --scenario-root tests \
  --seed 1337 \
  --doctor-runs 5 \
  --fuzz-time 2s \
  --explore-steps 200 \
  --explore-nodes 3 \
  --allow-expected-failures \
  --require-topology-coverage . \
  --topology-min-risk 60 \
  --topology-profile pedantic \
  --json
```

Additional script tests are in `tests/` (container, ICC, sync, volume, stress, diagnostics).

## Project Layout

```text
src/
  main.rs            # gRPC daemon
  cli/               # CLI client
  daemon/            # runtime + system integration
  sync/              # SQLite state/scheduling/orchestration
  icc/               # networking + messaging
  image/             # OCI image handling
  registry/          # registry client/auth flows
  usage/             # usage event tracking
  utils/             # shared runtime utilities

proto/               # gRPC proto definitions
scripts/             # install/migration/dev helpers
tests/               # shell + fozzy test scenarios
```

## Contributing

- Keep changes scoped and production-safe for self-hosting.
- Validate with Fozzy full gate before PR.
- Avoid introducing cloud-only HTTP/auth/subscription/serverless surface into `quilt-core`.

## License

MIT OR Apache-2.0
