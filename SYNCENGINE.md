# Quilt SQLite Sync Engine Implementation Plan

## 🎯 **Executive Summary**

**Problem**: Long-running containers cause server timeouts and blocking operations, making AI agents unusable for deep research tasks.

**Root Issue**: Process monitoring blocks the main server thread, preventing status checks and resource coordination.

**Solution**: Replace in-memory state with SQLite sync engine that coordinates **ALL stateful resources**: containers, networks, processes, and cleanup operations.

**Impact**: Transform Quilt from development tool to production-ready container engine with non-blocking operations.

---

## 🔍 **Current Architecture Issues - DETAILED ANALYSIS**

### **Critical Blocking Points Identified**

#### **1. Process Monitoring Bottleneck** (`src/daemon/runtime.rs:405-438`)
```rust
// ❌ BLOCKING: This blocks the ENTIRE server for hours
🔧 🕐 [TIMING] Started waiting for process 86395 at SystemTime { tv_sec: 1749571304, tv_nsec: 600971160 }
🔧 Waiting for process 86395 to complete  // ← 1 HOUR SLEEP BLOCKS EVERYTHING

// Later...
❌ Error getting container status: http2 error: keep-alive timed out
```

**Root Cause**: `tokio::spawn` with blocking `waitpid` system call accumulates and pressures the async runtime.

#### **2. Container Registry Locks** (`src/utils/locking.rs:38-60`)
```rust
// ❌ BLOCKING: Per-container locks during updates
🔧 [REGISTRY] Per-container update: 39d39d7e-bd33-4038-97e7-c5b011bb3723  // Locks during long operations
pub fn update<F, R>(&self, container_id: &str, updater: F) -> Option<R> {
    let operation_lock = self.operation_locks.entry(container_id.to_string()).or_insert_with(|| Arc::new(Mutex::new(())));
    let _lock = operation_lock.lock().unwrap(); // ← BLOCKING
}
```

#### **3. Network State Disconnect** (`src/main.rs:146-151` + `src/icc/network.rs`)
```rust
// ❌ INCONSISTENT: Network allocation not persisted or coordinated
🔧 Skipping network allocation for container 39d39d7e-bd33-4038-97e7-c5b011bb3723 (networking disabled)
// But network manager doesn't know this state across restarts
```

#### **4. gRPC Handler Blocking** (`src/main.rs:189-246`)
```rust
// ❌ BLOCKING: Status checks hit pressured registry
🔧 🔍 [GRPC] Received get_container_status request for: cd328333-7d43-48a1-bf60-60812877ef75
// Times out because container registry is locked by monitoring tasks
```

---

## 🏗️ **SQLite Sync Engine Architecture**

### **Core Design Principles**

1. **NEVER BLOCK**: All operations return immediately with database state
2. **ASYNC EVERYTHING**: Background tasks update database, server stays responsive  
3. **UNIFIED STATE**: Container + Network + Process state in one transactional system
4. **PERSISTENT**: Survive server restarts, maintain state across deployments

### **Database Schema Design**

```sql
-- Container lifecycle and state
CREATE TABLE containers (
    id TEXT PRIMARY KEY,
    name TEXT,
    image_path TEXT NOT NULL,
    command TEXT NOT NULL,
    environment TEXT, -- JSON blob
    state TEXT CHECK(state IN ('created', 'starting', 'running', 'exited', 'error')) NOT NULL,
    exit_code INTEGER,
    pid INTEGER,
    rootfs_path TEXT,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    exited_at INTEGER,
    memory_limit_mb INTEGER,
    cpu_limit_percent REAL,
    
    -- Resource configuration
    enable_network_namespace BOOLEAN NOT NULL DEFAULT 1,
    enable_pid_namespace BOOLEAN NOT NULL DEFAULT 1,
    enable_mount_namespace BOOLEAN NOT NULL DEFAULT 1,
    enable_uts_namespace BOOLEAN NOT NULL DEFAULT 1,
    enable_ipc_namespace BOOLEAN NOT NULL DEFAULT 1
);

-- Network allocations and coordination 
CREATE TABLE network_allocations (
    container_id TEXT PRIMARY KEY,
    ip_address TEXT NOT NULL,
    bridge_interface TEXT,
    veth_host TEXT,
    veth_container TEXT,
    allocation_time INTEGER NOT NULL,
    setup_completed BOOLEAN DEFAULT 0,
    status TEXT CHECK(status IN ('allocated', 'active', 'cleanup_pending', 'cleaned')) NOT NULL,
    FOREIGN KEY(container_id) REFERENCES containers(id) ON DELETE CASCADE
);

-- Global network state coordination
CREATE TABLE network_state (
    key TEXT PRIMARY KEY,
    value TEXT,
    updated_at INTEGER NOT NULL
);

-- Process monitoring (non-blocking)
CREATE TABLE process_monitors (
    container_id TEXT PRIMARY KEY,
    pid INTEGER NOT NULL,
    monitor_started_at INTEGER NOT NULL,
    last_check_at INTEGER,
    status TEXT CHECK(status IN ('monitoring', 'completed', 'failed', 'aborted')) NOT NULL,
    FOREIGN KEY(container_id) REFERENCES containers(id) ON DELETE CASCADE
);

-- Container logs (structured)
CREATE TABLE container_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    container_id TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    level TEXT CHECK(level IN ('debug', 'info', 'warn', 'error')) NOT NULL,
    message TEXT NOT NULL,
    FOREIGN KEY(container_id) REFERENCES containers(id) ON DELETE CASCADE
);

-- Resource cleanup tracking
CREATE TABLE cleanup_tasks (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    container_id TEXT NOT NULL,
    resource_type TEXT CHECK(resource_type IN ('rootfs', 'network', 'cgroup', 'mounts')) NOT NULL,
    resource_path TEXT NOT NULL,
    status TEXT CHECK(status IN ('pending', 'in_progress', 'completed', 'failed')) NOT NULL,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    error_message TEXT
);

-- Performance and monitoring
CREATE INDEX idx_containers_state ON containers(state);
CREATE INDEX idx_network_allocations_status ON network_allocations(status);
CREATE INDEX idx_process_monitors_status ON process_monitors(status);
CREATE INDEX idx_container_logs_container_time ON container_logs(container_id, timestamp);
```

---

## 🚀 **Integration Points - COMPREHENSIVE MAPPING**

### **1. Container Registry Replacement** (`src/utils/locking.rs` → `src/sync/engine.rs`)

**BEFORE** (Blocking):
```rust
// src/utils/locking.rs:38-60
pub fn update<F, R>(&self, container_id: &str, updater: F) -> Option<R> {
    let operation_lock = self.operation_locks.entry(container_id.to_string()).or_insert_with(|| Arc::new(Mutex::new(())));
    let _lock = operation_lock.lock().unwrap(); // ❌ BLOCKING
    // Modify in-memory state
}
```

**AFTER** (Non-blocking):
```rust
// src/sync/engine.rs
impl SyncEngine {
    pub async fn update_container_state(&self, container_id: &str, new_state: ContainerState) -> Result<(), SyncError> {
        let query = "UPDATE containers SET state = ?, updated_at = ? WHERE id = ?";
        self.conn.execute(query, (new_state.to_string(), SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(), container_id)).await?;
        Ok(()) // ✅ INSTANT RETURN
    }
    
    pub async fn get_container_status(&self, container_id: &str) -> Result<ContainerStatus, SyncError> {
        let query = "SELECT state, pid, ip_address FROM containers c LEFT JOIN network_allocations n ON c.id = n.container_id WHERE c.id = ?";
        // ✅ INSTANT DATABASE QUERY - NO BLOCKING
        let row = self.conn.query_row(query, [container_id]).await?;
        Ok(ContainerStatus::from_row(row))
    }
}
```

### **2. Network Coordination** (`src/icc/network.rs` + `src/main.rs`)

**BEFORE** (Inconsistent state):
```rust
// src/main.rs:146-151 - Always tries network setup
if let Err(e) = runtime.setup_container_network_post_start(&container_id, &*network_manager) {
    // Fails even when networking is disabled
}

// src/icc/network.rs - No persistence
let network_manager = self.network_manager.lock().await; // In-memory only
```

**AFTER** (Coordinated persistence):
```rust
// src/main.rs - Coordinated decision making
impl QuiltService {
    pub async fn create_container(&self, req: CreateContainerRequest) -> Result<CreateContainerResponse> {
        // 1. Allocate network resources in database FIRST
        let network_config = if req.enable_network_namespace {
            Some(self.sync_engine.allocate_network(&container_id).await?)
        } else {
            self.sync_engine.mark_network_disabled(&container_id).await?;
            None
        };
        
        // 2. Create container with known network state
        let container = self.sync_engine.create_container(req, network_config).await?;
        
        // 3. Start async monitoring (NON-BLOCKING)
        self.start_background_monitoring(&container_id).await?;
        
        Ok(CreateContainerResponse { container_id, state: "starting" }) // ✅ INSTANT RETURN
    }
}

// src/sync/network.rs - Persistent network coordination
impl SyncEngine {
    pub async fn allocate_network(&self, container_id: &str) -> Result<NetworkConfig, SyncError> {
        let ip = self.find_available_ip().await?;
        let query = "INSERT INTO network_allocations (container_id, ip_address, status, allocation_time) VALUES (?, ?, 'allocated', ?)";
        self.conn.execute(query, (container_id, &ip, SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())).await?;
        Ok(NetworkConfig { ip, setup_required: true })
    }
    
    pub async fn should_setup_network(&self, container_id: &str) -> Result<bool, SyncError> {
        let query = "SELECT COUNT(*) FROM network_allocations WHERE container_id = ? AND status = 'allocated'";
        let count: i64 = self.conn.query_row(query, [container_id]).await?.get(0)?;
        Ok(count > 0) // ✅ INSTANT DATABASE CHECK
    }
}
```

### **3. Async Process Monitoring** (`src/daemon/runtime.rs:405-438`)

**BEFORE** (Blocks server):
```rust
// src/daemon/runtime.rs:405-438
let wait_task = tokio::spawn(async move {
    let exit_code = match NamespaceManager::new().wait_for_process(pid) { // ❌ BLOCKS FOR HOURS
        Ok(code) => code,
        Err(e) => { /* error handling */ }
    };
    // Update in-memory state while server is blocked
});
```

**AFTER** (Background service):
```rust
// src/sync/monitor.rs - Dedicated background service
pub struct ProcessMonitorService {
    sync_engine: Arc<SyncEngine>,
    active_monitors: Arc<Mutex<HashSet<String>>>,
}

impl ProcessMonitorService {
    pub async fn start_monitoring(&self, container_id: &str, pid: Pid) -> Result<(), SyncError> {
        // 1. Record monitoring start in database
        self.sync_engine.start_process_monitor(&container_id, pid).await?;
        
        // 2. Spawn DETACHED monitoring task
        let sync_engine = self.sync_engine.clone();
        let container_id = container_id.to_string();
        
        tokio::spawn(async move {
            // This runs independently and updates database
            loop {
                match Self::check_process_status(pid).await {
                    ProcessStatus::Running => {
                        sync_engine.update_monitor_heartbeat(&container_id).await.ok();
                        tokio::time::sleep(Duration::from_secs(10)).await; // Reasonable polling
                    },
                    ProcessStatus::Exited(code) => {
                        sync_engine.complete_process_monitor(&container_id, code).await.ok();
                        sync_engine.trigger_cleanup(&container_id).await.ok();
                        break;
                    },
                    ProcessStatus::Error => {
                        sync_engine.fail_process_monitor(&container_id, "Process check failed").await.ok();
                        break;
                    }
                }
            }
        });
        
        Ok(()) // ✅ INSTANT RETURN - Server not blocked
    }
}

// src/daemon/runtime.rs - Uses background service
impl ContainerRuntime {
    pub async fn start_container(&self, container_id: &str) -> Result<Pid, RuntimeError> {
        // Container startup logic...
        let pid = self.create_namespaced_process(&config).await?;
        
        // Start background monitoring (NON-BLOCKING)
        self.monitor_service.start_monitoring(container_id, pid).await?;
        
        Ok(pid) // ✅ INSTANT RETURN
    }
}
```

### **4. gRPC Service Optimization** (`src/main.rs:189-246`)

**BEFORE** (Timeout prone):
```rust
// src/main.rs:189-246
async fn get_container_status(&self, request: Request<GetContainerStatusRequest>) -> Result<Response<GetContainerStatusResponse>, Status> {
    let container_id = request.into_inner().container_id;
    
    // Hits blocked container registry
    match self.runtime.get_container_status(&container_id) { // ❌ CAN TIMEOUT
        Ok(status) => Ok(Response::new(GetContainerStatusResponse { /* ... */ })),
        Err(e) => Err(Status::internal(format!("Error getting container status: {}", e))),
    }
}
```

**AFTER** (Always responsive):
```rust
// src/main.rs - Optimized service
impl QuiltService {
    async fn get_container_status(&self, request: Request<GetContainerStatusRequest>) -> Result<Response<GetContainerStatusResponse>, Status> {
        let container_id = request.into_inner().container_id;
        
        // Direct database query - ALWAYS fast
        match self.sync_engine.get_container_status(&container_id).await {
            Ok(status) => {
                let response = GetContainerStatusResponse {
                    container_id: status.id,
                    state: status.state.to_string(),
                    pid: status.pid,
                    ip_address: status.ip_address.unwrap_or_default(),
                    exit_code: status.exit_code,
                    created_at: status.created_at,
                    // All data from database - NO blocking operations
                };
                Ok(Response::new(response))
            },
            Err(SyncError::NotFound) => Err(Status::not_found("Container not found")),
            Err(e) => Err(Status::internal(format!("Database error: {}", e))),
        }
        // ✅ ALWAYS RETURNS IN <1ms
    }
}
```

---

## 🔧 **Implementation Strategy**

### **Phase 1: Core Sync Engine** (Day 1-2)
1. **Database setup** (`src/sync/schema.rs`)
   - SQLite connection pool with WAL mode for performance
   - Schema migrations and version management
   - Connection management for async operations

2. **Basic CRUD operations** (`src/sync/engine.rs`)
   - Container state management
   - Network allocation tracking
   - Process monitor registration

### **Phase 2: Container Integration** (Day 2-3)
3. **Replace ConcurrentContainerRegistry** (`src/utils/locking.rs` → `src/sync/containers.rs`)
   - All container state operations go through sync engine
   - Remove all blocking mutex operations
   - Implement instant status queries

4. **Update ContainerRuntime** (`src/daemon/runtime.rs`)
   - Use sync engine for state management
   - Remove blocking wait operations
   - Implement background monitoring service

### **Phase 3: Network Coordination** (Day 3-4)
5. **Network state management** (`src/sync/network.rs`)
   - Persistent IP allocation tracking
   - Network setup coordination
   - Cross-restart state persistence

6. **Update NetworkManager** (`src/icc/network.rs`)
   - Coordinate with sync engine for allocations
   - Implement setup verification through database
   - Track interface state persistently

### **Phase 4: Service Integration** (Day 4-5)
7. **gRPC service optimization** (`src/main.rs`)
   - All handlers use sync engine directly
   - Remove blocking operations from request handlers
   - Implement efficient database queries

8. **Background services** (`src/sync/monitor.rs`, `src/sync/cleanup.rs`)
   - Dedicated process monitoring service
   - Automated cleanup coordination
   - Resource leak prevention

---

## 📊 **Performance Transformation**

### **Current Performance Issues:**
| Operation | Current Time | Issue |
|-----------|-------------|--------|
| Status Check | 5-30s (timeout) | Registry locks, blocking monitoring |
| Container Creation | 2-5s | Network coordination delays |
| Server Response | Varies | Blocked by long-running operations |
| Cross-restart Recovery | Manual | No state persistence |

### **With Sync Engine:**
| Operation | New Time | Improvement |
|-----------|-----------|-------------|
| Status Check | <1ms | Direct database query |
| Container Creation | 200-500ms | Async monitoring, instant return |
| Server Response | Always <10ms | No blocking operations |
| Cross-restart Recovery | Automatic | Full state persistence |

### **AI Agent Benefits:**
- **Long-running research tasks**: No server blocking
- **Multi-agent coordination**: Shared state management
- **Resource accountability**: Track which agent uses what
- **Fault tolerance**: Persist state across crashes
- **Scalability**: Database-backed horizontal scaling

---

## 🎯 **Success Metrics**

### **Immediate (Week 1):**
- ✅ Container status checks always return in <1ms
- ✅ Long-running containers don't block server operations
- ✅ Network state persists across server restarts
- ✅ No more gRPC timeout errors

### **AI Agent Platform (Week 2-4):**
- ✅ 100+ concurrent AI agents running research tasks
- ✅ Cross-agent resource sharing and coordination
- ✅ Automatic cleanup and resource management
- ✅ Production-ready stability and monitoring

### **Technical Benchmarks:**
- **Database Operations**: <1ms average query time
- **Container Lifecycle**: <500ms from request to running
- **Memory Usage**: <50MB additional overhead for sync engine
- **Reliability**: 99.9% uptime with graceful failure recovery

---

## 💡 **Implementation Notes**

### **SQLite Optimizations:**
```rust
// src/sync/connection.rs
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

pub async fn create_optimized_pool() -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename("quilt.db")
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal) // Better concurrency
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal) // Performance vs safety balance
        .busy_timeout(Duration::from_secs(30))
        .pragma("cache_size", "10000") // 10MB cache
        .pragma("temp_store", "memory")
        .pragma("mmap_size", "268435456"); // 256MB memory mapping
    
    SqlitePool::connect_with(options).await
}
```

### **Rust Concurrency Patterns:**
- **Arc<SyncEngine>**: Shared across all services
- **tokio::spawn**: Background tasks that don't block main operations
- **Database connection pool**: Handle concurrent access efficiently
- **Atomic operations**: For simple state flags and counters

### **Error Handling Strategy:**
```rust
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    
    #[error("Container not found: {container_id}")]
    NotFound { container_id: String },
    
    #[error("Network allocation failed: {reason}")]
    NetworkAllocation { reason: String },
    
    #[error("State transition invalid: {from} -> {to}")]
    InvalidStateTransition { from: String, to: String },
}
```

---

## 🚀 **Critical Path to Success**

1. **✅ Network Issues Resolved**: Already complete - containers work with `--no-network`
2. **🔄 Sync Engine Implementation**: Replace blocking registry with database
3. **🔄 Background Monitoring**: Decouple process monitoring from server thread
4. **🔄 Network Coordination**: Persistent network state management
5. **✅ Production Ready**: AI agents can run long-running tasks without blocking

**Timeline**: 5 days to transform Quilt from development tool to production AI agent platform.

---

*Last Updated: December 2024*  
*Status: COMPREHENSIVE NETWORK + CONTAINER STATE COORDINATION REQUIRED* 