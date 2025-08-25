use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use crate::sync::{
    connection::ConnectionManager,
    schema::SchemaManager,
    containers::{ContainerManager, ContainerConfig, ContainerStatus, ContainerState},
    network::{NetworkManager, NetworkConfig, NetworkAllocation},
    monitor::ProcessMonitorService,
    cleanup::CleanupService,
    volumes::{VolumeManager, Volume, Mount, MountType},
    error::{SyncResult, SyncError},
};
use crate::utils::validation::InputValidator;

/// Main sync engine that coordinates all stateful resources
pub struct SyncEngine {
    connection_manager: Arc<ConnectionManager>,
    container_manager: Arc<ContainerManager>,
    network_manager: Arc<NetworkManager>,
    volume_manager: Arc<VolumeManager>,
    pub monitor_service: Arc<ProcessMonitorService>,
    pub cleanup_service: Arc<CleanupService>,
    
    // Background services control
    background_tasks: Arc<RwLock<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Clone for SyncEngine {
    fn clone(&self) -> Self {
        Self {
            connection_manager: Arc::clone(&self.connection_manager),
            container_manager: Arc::clone(&self.container_manager),
            network_manager: Arc::clone(&self.network_manager),
            volume_manager: Arc::clone(&self.volume_manager),
            monitor_service: Arc::clone(&self.monitor_service),
            cleanup_service: Arc::clone(&self.cleanup_service),
            background_tasks: Arc::clone(&self.background_tasks),
        }
    }
}

impl SyncEngine {
    /// Create a new sync engine with the given database path
    pub async fn new(database_path: &str) -> SyncResult<Self> {
        // Initialize connection
        let connection_manager = Arc::new(ConnectionManager::new(database_path).await?);
        
        // Initialize schema
        let schema_manager = SchemaManager::new(connection_manager.pool().clone());
        schema_manager.initialize_schema().await?;
        
        // Create component managers
        let container_manager = Arc::new(ContainerManager::new(connection_manager.pool().clone()));
        let network_manager = Arc::new(NetworkManager::new(connection_manager.pool().clone()));
        let volume_manager = Arc::new(VolumeManager::new(connection_manager.pool().clone()));
        let monitor_service = Arc::new(ProcessMonitorService::new(connection_manager.pool().clone()));
        let cleanup_service = Arc::new(CleanupService::new(connection_manager.pool().clone()));
        
        // Initialize volume manager
        volume_manager.initialize().await?;
        
        let engine = Self {
            connection_manager,
            container_manager,
            network_manager,
            volume_manager,
            monitor_service,
            cleanup_service,
            background_tasks: Arc::new(RwLock::new(Vec::new())),
        };
        
        tracing::info!("Sync engine initialized with database: {}", database_path);
        Ok(engine)
    }
    
    /// Create a new sync engine with custom network configuration
    pub async fn new_with_network_config(
        database_path: &str, 
        subnet_cidr: Option<String>,
        icc_network_manager: Option<std::sync::Arc<crate::icc::network::NetworkManager>>
    ) -> SyncResult<Self> {
        // If no special configuration, use the simpler new() constructor
        if subnet_cidr.is_none() && icc_network_manager.is_none() {
            return Self::new(database_path).await;
        }
        
        // Initialize connection
        let connection_manager = Arc::new(ConnectionManager::new(database_path).await?);
        
        // Initialize schema
        let schema_manager = SchemaManager::new(connection_manager.pool().clone());
        schema_manager.initialize_schema().await?;
        
        // Create component managers
        let container_manager = Arc::new(ContainerManager::new(connection_manager.pool().clone()));
        
        // Create NetworkManager with custom configuration
        let network_manager = if let Some(ref icc_manager) = icc_network_manager {
            tracing::info!("Initializing sync engine with ICC NetworkManager integration");
            Arc::new(NetworkManager::new_with_icc_manager(connection_manager.pool().clone(), icc_manager.clone()))
        } else if let Some(subnet) = subnet_cidr {
            tracing::info!("Initializing sync engine with custom subnet: {}", subnet);
            Arc::new(NetworkManager::new_with_subnet(connection_manager.pool().clone(), subnet))
        } else {
            tracing::info!("Initializing sync engine with default network configuration");
            Arc::new(NetworkManager::new(connection_manager.pool().clone()))
        };
        
        let volume_manager = Arc::new(VolumeManager::new(connection_manager.pool().clone()));
        let monitor_service = Arc::new(ProcessMonitorService::new(connection_manager.pool().clone()));
        
        // Create CleanupService with ICC integration if available
        let cleanup_service = if let Some(ref icc_manager) = icc_network_manager {
            tracing::info!("Initializing cleanup service with ICC NetworkManager integration");
            Arc::new(CleanupService::new_with_icc_manager(connection_manager.pool().clone(), icc_manager.clone()))
        } else {
            Arc::new(CleanupService::new(connection_manager.pool().clone()))
        };
        
        // Initialize volume manager
        volume_manager.initialize().await?;
        
        let engine = Self {
            connection_manager,
            container_manager,
            network_manager,
            volume_manager,
            monitor_service,
            cleanup_service,
            background_tasks: Arc::new(RwLock::new(Vec::new())),
        };
        
        tracing::info!("Sync engine initialized with custom network config and database: {}", database_path);
        Ok(engine)
    }

    /// Create a new sync engine for testing with IP range
    pub async fn new_for_testing(database_path: &str, start_ip: std::net::Ipv4Addr, end_ip: std::net::Ipv4Addr) -> SyncResult<Self> {
        // Initialize connection
        let connection_manager = Arc::new(ConnectionManager::new(database_path).await?);
        
        // Initialize schema
        let schema_manager = SchemaManager::new(connection_manager.pool().clone());
        schema_manager.initialize_schema().await?;
        
        // Create component managers
        let container_manager = Arc::new(ContainerManager::new(connection_manager.pool().clone()));
        let network_manager = Arc::new(NetworkManager::with_ip_range(connection_manager.pool().clone(), start_ip, end_ip));
        let volume_manager = Arc::new(VolumeManager::new(connection_manager.pool().clone()));
        let monitor_service = Arc::new(ProcessMonitorService::new(connection_manager.pool().clone()));
        let cleanup_service = Arc::new(CleanupService::new(connection_manager.pool().clone()));
        
        // Initialize volume manager
        volume_manager.initialize().await?;
        
        let engine = Self {
            connection_manager,
            container_manager,
            network_manager,
            volume_manager,
            monitor_service,
            cleanup_service,
            background_tasks: Arc::new(RwLock::new(Vec::new())),
        };
        
        tracing::info!("Sync engine initialized for testing with IP range {}..{} and database: {}", 
            start_ip, end_ip, database_path);
        Ok(engine)
    }
    
    /// Start background services for monitoring and cleanup
    pub async fn start_background_services(&self) -> SyncResult<()> {
        let mut tasks = self.background_tasks.write().await;
        
        // Start cleanup worker
        let cleanup_service = self.cleanup_service.clone();
        let cleanup_task = tokio::spawn(async move {
            if let Err(e) = cleanup_service.run_cleanup_worker(5).await {
                tracing::error!("Cleanup worker failed: {}", e);
            }
        });
        tasks.push(cleanup_task);
        
        // Start monitor cleanup task (runs every 5 minutes)
        let monitor_service = self.monitor_service.clone();
        let monitor_cleanup_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 minutes
            loop {
                interval.tick().await;
                if let Err(e) = monitor_service.cleanup_stale_monitors(Duration::from_secs(600)).await {
                    tracing::warn!("Failed to cleanup stale monitors: {}", e);
                }
            }
        });
        tasks.push(monitor_cleanup_task);
        
        // Start volume cleanup task (runs every 30 minutes)
        let volume_manager = self.volume_manager.clone();
        let volume_cleanup_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1800)); // 30 minutes
            loop {
                interval.tick().await;
                if let Err(e) = volume_manager.cleanup_orphaned_volumes().await {
                    tracing::warn!("Failed to cleanup orphaned volumes: {}", e);
                }
            }
        });
        tasks.push(volume_cleanup_task);
        
        // Start network cleanup task (runs every 15 minutes)
        let network_manager = self.network_manager.clone();
        let network_cleanup_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(900)); // 15 minutes
            loop {
                interval.tick().await;
                // Get networks needing cleanup and process them
                if let Ok(networks_to_cleanup) = network_manager.get_networks_needing_cleanup().await {
                    for network_alloc in networks_to_cleanup {
                        tracing::info!("Cleaning up network for container {}", network_alloc.container_id);
                        // Mark as cleaned after successful cleanup
                        if let Err(e) = network_manager.mark_network_cleaned(&network_alloc.container_id).await {
                            tracing::warn!("Failed to mark network cleaned for {}: {}", network_alloc.container_id, e);
                        }
                    }
                }
            }
        });
        tasks.push(network_cleanup_task);
        
        // Start metrics cleanup task (runs daily)
        let pool = self.connection_manager.pool().clone();
        let metrics_cleanup_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(86400)); // 24 hours
            loop {
                interval.tick().await;
                let metrics_store = crate::sync::metrics::MetricsStore::new(pool.clone());
                if let Err(e) = metrics_store.cleanup_old_metrics(7).await { // Keep 7 days
                    tracing::warn!("Failed to cleanup old metrics: {}", e);
                }
            }
        });
        tasks.push(metrics_cleanup_task);
        
        // Start log cleanup task (runs every 6 hours)
        let container_manager = self.container_manager.clone();
        let log_cleanup_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(21600)); // 6 hours
            loop {
                interval.tick().await;
                // Get all containers and cleanup logs (keep last 1000 entries per container)
                if let Ok(containers) = container_manager.list_containers(None).await {
                    for container in containers {
                        if let Err(e) = container_manager.cleanup_container_logs(&container.id, 1000).await {
                            tracing::warn!("Failed to cleanup logs for container {}: {}", container.id, e);
                        }
                    }
                }
            }
        });
        tasks.push(log_cleanup_task);
        
        tracing::info!("Started {} background services", tasks.len());
        Ok(())
    }
    
    /// Stop all background services
    pub async fn stop_background_services(&self) {
        let mut tasks = self.background_tasks.write().await;
        
        for task in tasks.drain(..) {
            task.abort();
        }
        
        tracing::info!("Stopped all background services");
    }
    
    /// Close the sync engine and all connections
    pub async fn close(&self) {
        self.stop_background_services().await;
        self.connection_manager.close().await;
        tracing::info!("Sync engine closed");
    }
    
    // === Container Management ===
    
    /// PRODUCTION-GRADE: Atomic container + network creation
    /// Eliminates database lock contention by using single transaction for both operations
    pub async fn create_container(&self, config: ContainerConfig) -> SyncResult<NetworkConfig> {
        // Store container ID and network namespace flag before moving config
        let container_id = config.id.clone();
        let enable_network = config.enable_network_namespace;
        
        // Import ConsoleLogger
        use crate::utils::console::ConsoleLogger;
        use std::time::{SystemTime, UNIX_EPOCH};
        
        println!("🔧 [SYNC-CREATE] Creating container {} with networking: {} (atomic)", container_id, enable_network);
        ConsoleLogger::info(&format!("🔧 [SYNC-CREATE] Creating container {} with networking: {} (atomic)", container_id, enable_network));
        
        // ATOMIC TRANSACTION: Container + Network creation in single database operation
        let mut transaction = self.connection_manager.pool().begin().await?;
        
        // Step 1: Insert container record within transaction
        let environment_json = serde_json::to_string(&config.environment)?;
        let created_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        
        sqlx::query(r#"
            INSERT INTO containers (
                id, name, image_path, command, environment, state,
                memory_limit_mb, cpu_limit_percent,
                enable_network_namespace, enable_pid_namespace, enable_mount_namespace,
                enable_uts_namespace, enable_ipc_namespace,
                created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#)
        .bind(&config.id)
        .bind(&config.name)
        .bind(&config.image_path)
        .bind(&config.command)
        .bind(&environment_json)
        .bind(crate::sync::containers::ContainerState::Created.to_string())
        .bind(config.memory_limit_mb)
        .bind(config.cpu_limit_percent)
        .bind(config.enable_network_namespace)
        .bind(config.enable_pid_namespace)
        .bind(config.enable_mount_namespace)
        .bind(config.enable_uts_namespace)
        .bind(config.enable_ipc_namespace)
        .bind(created_at)
        .bind(created_at)
        .execute(&mut *transaction)
        .await?;
        
        ConsoleLogger::debug(&format!("✅ [ATOMIC] Container record inserted for {}", container_id));
        
        // Step 2: Commit container creation first
        transaction.commit().await?;
        ConsoleLogger::debug(&format!("✅ [ATOMIC] Container record committed for {}", container_id));
        
        // Step 3: Network allocation using proper NetworkManager (separate transaction, if enabled)
        let network_config = if enable_network {
            match self.network_manager.allocate_network(&container_id).await {
                Ok(config) => {
                    ConsoleLogger::debug(&format!("✅ [NETWORK] IP allocated via NetworkManager for {}: {}", container_id, config.ip_address));
                    Some(config)
                },
                Err(e) => {
                    // Network allocation failed - clean up the container
                    ConsoleLogger::error(&format!("❌ [NETWORK] Failed to allocate IP for {}: {}", container_id, e));
                    if let Err(cleanup_err) = self.container_manager.delete_container(&container_id).await {
                        ConsoleLogger::error(&format!("❌ [CLEANUP] Failed to cleanup container {} after network failure: {}", container_id, cleanup_err));
                    }
                    return Err(e);
                }
            }
        } else {
            None
        };
        
        ConsoleLogger::info(&format!("🔒 [COMPLETE] Container {} created with network config", container_id));
        
        // Step 4: Emit events after successful database commit
        
        if let Some(ref net_cfg) = network_config {
            // Validate that the network config container ID matches
            if net_cfg.container_id != container_id {
                return Err(SyncError::ValidationFailed {
                    message: format!("Network config container ID mismatch: expected {}, got {}", container_id, net_cfg.container_id),
                });
            }
            
            ConsoleLogger::info(&format!("✅ [ATOMIC] Network allocated for {}: IP={}, Container={}, Setup Required={}", 
                container_id, net_cfg.ip_address, net_cfg.container_id, net_cfg.setup_required));
            
            // Log additional network details if available
            if let Some(ref bridge) = net_cfg.bridge_interface {
                ConsoleLogger::debug(&format!("🌉 [NETWORK] Bridge interface for {}: {}", container_id, bridge));
            }
            if let Some(ref veth_host) = net_cfg.veth_host {
                ConsoleLogger::debug(&format!("🔗 [NETWORK] Host veth for {}: {}", container_id, veth_host));
            }
            if let Some(ref veth_container) = net_cfg.veth_container {
                ConsoleLogger::debug(&format!("🔗 [NETWORK] Container veth for {}: {}", container_id, veth_container));
            }
        } else {
            ConsoleLogger::debug(&format!("🚫 [ATOMIC] Network disabled for {}", container_id));
        }
        
        // Return network configuration
        Ok(network_config.unwrap_or(NetworkConfig {
            container_id,
            ip_address: String::new(),
            bridge_interface: None,
            veth_host: None,
            veth_container: None,
            setup_required: false,
        }))
    }
    
    /// Update container state with validation
    pub async fn update_container_state(&self, container_id: &str, new_state: ContainerState) -> SyncResult<()> {
        // Clone the state to use it after the move
        let state_for_check = new_state.clone();
        self.container_manager.update_container_state(container_id, new_state).await?;
        
        // Trigger cleanup if container is finished
        if matches!(state_for_check, ContainerState::Exited | ContainerState::Error) {
            self.trigger_cleanup(container_id).await?;
        }
        
        Ok(())
    }
    
    /// Set container PID and start monitoring
    pub async fn set_container_pid(&self, container_id: &str, pid: nix::unistd::Pid) -> SyncResult<()> {
        // Update container record
        self.container_manager.set_container_pid(container_id, pid.as_raw() as i64).await?;
        
        // Start background monitoring (non-blocking)
        self.monitor_service.start_monitoring(container_id, pid).await?;
        
        Ok(())
    }
    
    /// Set container exit code
    pub async fn set_container_exit_code(&self, container_id: &str, exit_code: i64) -> SyncResult<()> {
        self.container_manager.set_container_exit_code(container_id, exit_code).await
    }
    
    /// Set rootfs path
    pub async fn set_rootfs_path(&self, container_id: &str, rootfs_path: &str) -> SyncResult<()> {
        self.container_manager.set_rootfs_path(container_id, rootfs_path).await
    }
    
    /// Get container status (always fast - direct database query)
    pub async fn get_container_status(&self, container_id: &str) -> SyncResult<ContainerStatus> {
        self.container_manager.get_container_status(container_id).await
    }
    
    /// List containers with optional state filter
    pub async fn list_containers(&self, state_filter: Option<ContainerState>) -> SyncResult<Vec<ContainerStatus>> {
        self.container_manager.list_containers(state_filter).await
    }
    
    /// Delete container and all associated resources
    pub async fn delete_container(&self, container_id: &str) -> SyncResult<()> {
        // Stop monitoring if active
        let _ = self.monitor_service.stop_monitoring(container_id).await;
        
        // Get container info for cleanup
        let status = self.container_manager.get_container_status(container_id).await?;
        
        // Schedule cleanup tasks
        self.cleanup_service.schedule_container_cleanup(
            container_id,
            status.rootfs_path.as_deref()
        ).await?;
        
        // Mark network for cleanup
        if let Ok(_) = self.network_manager.get_network_allocation(container_id).await {
            self.network_manager.mark_network_cleanup_pending(container_id).await?;
        }
        
        // Delete container record
        self.container_manager.delete_container(container_id).await?;
        
        tracing::info!("Scheduled full cleanup for container {}", container_id);
        Ok(())
    }
    
    // === Network Management ===
    
    /// Check if container should have network setup
    pub async fn should_setup_network(&self, container_id: &str) -> SyncResult<bool> {
        self.network_manager.should_setup_network(container_id).await
    }
    
    /// Mark network setup as complete
    pub async fn mark_network_setup_complete(&self, container_id: &str, bridge_interface: &str, veth_host: &str, veth_container: &str) -> SyncResult<()> {
        self.network_manager.mark_network_setup_complete(container_id, bridge_interface, veth_host, veth_container).await
    }
    
    /// Get network allocation for container
    pub async fn get_network_allocation(&self, container_id: &str) -> SyncResult<NetworkAllocation> {
        self.network_manager.get_network_allocation(container_id).await
    }
    
    /// List all network allocations
    pub async fn list_network_allocations(&self) -> SyncResult<Vec<NetworkAllocation>> {
        self.network_manager.list_allocations(None).await
    }
    
    // === Process Monitoring ===
    
    /// Get process monitor status
    pub async fn get_monitor_status(&self, container_id: &str) -> SyncResult<crate::sync::monitor::ProcessMonitor> {
        self.monitor_service.get_monitor_status(container_id).await
    }
    
    /// List all active monitors
    pub async fn list_active_monitors(&self) -> SyncResult<Vec<crate::sync::monitor::ProcessMonitor>> {
        self.monitor_service.list_active_monitors().await
    }
    
    /// Stop monitoring a container
    pub async fn stop_monitoring(&self, container_id: &str) -> SyncResult<()> {
        self.monitor_service.stop_monitoring(container_id).await
    }
    
    // === Cleanup Management ===
    
    /// Trigger cleanup for a container
    pub async fn trigger_cleanup(&self, container_id: &str) -> SyncResult<()> {
        // Get container info
        let status = self.container_manager.get_container_status(container_id).await?;
        
        // Schedule cleanup tasks
        self.cleanup_service.schedule_container_cleanup(
            container_id,
            status.rootfs_path.as_deref()
        ).await?;
        
        // Mark network for cleanup if allocated
        if let Ok(_) = self.network_manager.get_network_allocation(container_id).await {
            self.network_manager.mark_network_cleanup_pending(container_id).await?;
        }
        
        Ok(())
    }
    
    
    // === Utility Methods ===
    
    
    /// Get container ID by name
    pub async fn get_container_by_name(&self, name: &str) -> SyncResult<String> {
        self.container_manager.get_container_by_name(name).await
    }
    
    /// Get database connection pool for advanced operations
    pub fn pool(&self) -> &sqlx::SqlitePool {
        self.connection_manager.pool()
    }
    
    /// Get container counts (total and running)
    pub async fn get_container_counts(&self) -> SyncResult<(usize, usize)> {
        let containers = self.list_containers(None).await?;
        let total = containers.len();
        let running = containers.iter()
            .filter(|c| matches!(c.state, ContainerState::Running))
            .count();
        Ok((total, running))
    }
    
    /// Store container metrics
    pub async fn store_metrics(&self, metrics: &crate::daemon::metrics::ContainerMetrics) -> SyncResult<()> {
        use crate::sync::metrics::MetricsStore;
        let store = MetricsStore::new(self.connection_manager.pool().clone());
        store.store_metrics(metrics).await
    }
    
    /// Get latest metrics for a container
    pub async fn get_latest_metrics(&self, container_id: &str) -> SyncResult<Option<crate::daemon::metrics::ContainerMetrics>> {
        use crate::sync::metrics::MetricsStore;
        let store = MetricsStore::new(self.connection_manager.pool().clone());
        store.get_latest_metrics(container_id).await
    }
    
    /// Get metrics history for a container within time range
    /// Example function showing how to create specialized engines for testing/development
    /// This ensures constructors like new_for_testing are properly integrated
    #[allow(dead_code)]
    pub async fn create_test_engine_example() -> SyncResult<()> {
        // Example usage of new_for_testing with specific IP range
        let _engine = Self::new_for_testing(
            ":memory:", 
            std::net::Ipv4Addr::new(192, 168, 1, 2),
            std::net::Ipv4Addr::new(192, 168, 1, 254)
        ).await?;
        tracing::debug!("Created test engine with custom IP range");
        Ok(())
    }
    
    pub async fn get_metrics_history(
        &self, 
        container_id: &str, 
        start_time: u64, 
        end_time: u64,
        limit: Option<u32>
    ) -> SyncResult<Vec<crate::daemon::metrics::ContainerMetrics>> {
        use crate::sync::metrics::MetricsStore;
        let store = MetricsStore::new(self.connection_manager.pool().clone());
        store.get_metrics_history(container_id, start_time, end_time, limit).await
    }
    
    /// Clean up old metrics
    pub async fn cleanup_old_metrics(&self, retention_days: u32) -> SyncResult<u64> {
        use crate::sync::metrics::MetricsStore;
        let store = MetricsStore::new(self.connection_manager.pool().clone());
        store.cleanup_old_metrics(retention_days).await
    }
    
    
    /// Get sync engine statistics
    pub async fn get_stats(&self) -> SyncResult<SyncEngineStats> {
        let containers = self.container_manager.list_containers(None).await?;
        let active_monitors = self.monitor_service.list_active_monitors().await?;
        let network_allocations = self.network_manager.list_allocations(None).await?;
        
        let running_containers = containers.iter().filter(|c| c.state == ContainerState::Running).count();
        let total_containers = containers.len();
        let active_networks = network_allocations.iter().filter(|n| n.setup_completed).count();
        let active_monitors_count = active_monitors.len();
        
        Ok(SyncEngineStats {
            total_containers,
            running_containers,
            active_networks,
            active_monitors: active_monitors_count,
        })
    }
    
    // Volume management methods
    
    /// Create a new volume
    pub async fn create_volume(
        &self,
        name: &str,
        driver: Option<&str>,
        labels: std::collections::HashMap<String, String>,
        options: std::collections::HashMap<String, String>,
    ) -> SyncResult<Volume> {
        self.volume_manager.create_volume(name, driver, labels, options).await
    }
    
    /// Get volume by name
    pub async fn get_volume(&self, name: &str) -> SyncResult<Option<Volume>> {
        self.volume_manager.get_volume(name).await
    }
    
    /// List all volumes
    pub async fn list_volumes(&self, filters: Option<std::collections::HashMap<String, String>>) -> SyncResult<Vec<Volume>> {
        self.volume_manager.list_volumes(filters).await
    }
    
    /// Remove a volume
    pub async fn remove_volume(&self, name: &str, force: bool) -> SyncResult<()> {
        self.volume_manager.remove_volume(name, force).await
    }
    
    /// Clean up orphaned volumes
    pub async fn cleanup_orphaned_volumes(&self) -> SyncResult<u32> {
        self.volume_manager.cleanup_orphaned_volumes().await
    }

    /// Perform comprehensive network cleanup using ICC NetworkManager integration
    pub async fn comprehensive_network_cleanup(&self) -> SyncResult<Vec<String>> {
        self.cleanup_service.perform_comprehensive_network_cleanup().await
    }
    
    /// Add mount to container
    pub async fn add_container_mount(
        &self,
        container_id: &str,
        source: &str,
        target: &str,
        mount_type: MountType,
        readonly: bool,
        options: std::collections::HashMap<String, String>,
    ) -> SyncResult<Mount> {
        // Validate mount configuration using InputValidator
        let mount_string = format!("{}:{}", source, target);
        match InputValidator::parse_volume(&mount_string) {
            Ok(parsed_mount) => {
                tracing::debug!("Mount validation passed for container {}: {} -> {} (readonly: {})", 
                    container_id, parsed_mount.source, parsed_mount.target, parsed_mount.readonly);
                
                // Use parsed readonly flag if it differs from input
                let final_readonly = if parsed_mount.readonly != readonly {
                    tracing::info!("Using parsed readonly flag {} instead of {} for container {}", 
                        parsed_mount.readonly, readonly, container_id);
                    parsed_mount.readonly
                } else {
                    readonly
                };
                
                self.volume_manager.add_mount(container_id, source, target, mount_type, final_readonly, options).await
            }
            Err(e) => {
                tracing::warn!("Mount parsing validation failed for container {}: {}, proceeding with original config", 
                    container_id, e);
                self.volume_manager.add_mount(container_id, source, target, mount_type, readonly, options).await
            }
        }
    }
    
    /// Get mounts for a container
    pub async fn get_container_mounts(&self, container_id: &str) -> SyncResult<Vec<Mount>> {
        self.volume_manager.get_container_mounts(container_id).await
    }
    
    /// Remove all mounts for a container
    pub async fn remove_container_mounts(&self, container_id: &str) -> SyncResult<()> {
        self.volume_manager.remove_container_mounts(container_id).await
    }
    
    /// Get volume path for mounting
    pub fn get_volume_path(&self, volume_name: &str) -> std::path::PathBuf {
        self.volume_manager.get_volume_path(volume_name)
    }
    
    // === Container Logging ===
    
    /// Store a log entry for a container
    pub async fn store_container_log(&self, container_id: &str, level: &str, message: &str) -> SyncResult<()> {
        self.container_manager.store_log(container_id, level, message).await
    }
    
    /// Get logs for a container
    pub async fn get_container_logs(&self, container_id: &str, limit: Option<u32>) -> SyncResult<Vec<crate::sync::containers::LogEntry>> {
        self.container_manager.get_container_logs(container_id, limit).await
    }
    
    /// Clean up old logs for a container
    pub async fn cleanup_container_logs(&self, container_id: &str, keep_count: u32) -> SyncResult<u64> {
        self.container_manager.cleanup_container_logs(container_id, keep_count).await
    }
}

#[derive(Debug, Clone)]
pub struct SyncEngineStats {
    pub total_containers: usize,
    pub running_containers: usize,
    pub active_networks: usize,
    pub active_monitors: usize,
}

impl Drop for SyncEngine {
    fn drop(&mut self) {
        // Note: Can't call async methods in Drop, so background services
        // should be explicitly stopped before dropping
        tracing::debug!("SyncEngine dropped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::collections::HashMap;
    
    async fn setup_test_engine() -> SyncEngine {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();
        
        SyncEngine::new(db_path).await.unwrap()
    }
    
    #[tokio::test]
    async fn test_sync_engine_creation() {
        let engine = setup_test_engine().await;
        
        let stats = engine.get_stats().await.unwrap();
        assert_eq!(stats.total_containers, 0);
        assert_eq!(stats.running_containers, 0);
        assert_eq!(stats.active_networks, 0);
        assert_eq!(stats.active_monitors, 0);
        
        engine.close().await;
    }
    
    #[tokio::test]
    async fn test_container_lifecycle_integration() {
        let engine = setup_test_engine().await;
        
        let config = ContainerConfig {
            id: "test-container".to_string(),
            name: Some("test".to_string()),
            image_path: "/path/to/image".to_string(),
            command: "echo hello".to_string(),
            environment: HashMap::new(),
            memory_limit_mb: Some(1024),
            cpu_limit_percent: Some(50.0),
            enable_network_namespace: true,
            enable_pid_namespace: true,
            enable_mount_namespace: true,
            enable_uts_namespace: true,
            enable_ipc_namespace: true,
        };
        
        // Create container
        let network_config = engine.create_container(config).await.unwrap();
        assert!(!network_config.ip_address.is_empty());
        assert!(network_config.setup_required);
        
        // Check initial status
        let status = engine.get_container_status("test-container").await.unwrap();
        assert_eq!(status.state, ContainerState::Created);
        assert_eq!(status.ip_address, Some(network_config.ip_address.clone()));
        
        // Transition through states
        engine.update_container_state("test-container", ContainerState::Starting).await.unwrap();
        
        // Set PID (would normally come from actual process creation)
        let test_pid = nix::unistd::Pid::from_raw(12345);
        engine.set_container_pid("test-container", test_pid).await.unwrap();
        
        engine.update_container_state("test-container", ContainerState::Running).await.unwrap();
        
        // Complete network setup
        engine.mark_network_setup_complete("test-container", "br0", "veth123", "eth0").await.unwrap();
        
        // Verify final state
        let final_status = engine.get_container_status("test-container").await.unwrap();
        assert_eq!(final_status.state, ContainerState::Running);
        assert_eq!(final_status.pid, Some(12345));
        
        let network_allocation = engine.get_network_allocation("test-container").await.unwrap();
        assert!(network_allocation.setup_completed);
        assert_eq!(network_allocation.bridge_interface, Some("br0".to_string()));
        
        // Clean up
        engine.delete_container("test-container").await.unwrap();
        engine.close().await;
    }
    
    #[tokio::test]
    async fn test_network_disabled_container() {
        let engine = setup_test_engine().await;
        
        let config = ContainerConfig {
            id: "no-network-container".to_string(),
            name: None,
            image_path: "/path/to/image".to_string(),
            command: "echo hello".to_string(),
            environment: HashMap::new(),
            memory_limit_mb: None,
            cpu_limit_percent: None,
            enable_network_namespace: false, // Networking disabled
            enable_pid_namespace: true,
            enable_mount_namespace: true,
            enable_uts_namespace: true,
            enable_ipc_namespace: true,
        };
        
        // Create container
        let network_config = engine.create_container(config).await.unwrap();
        assert_eq!(network_config.ip_address, "");
        assert!(!network_config.setup_required);
        
        // Should not have network allocation
        assert!(!engine.should_setup_network("no-network-container").await.unwrap());
        
        let status = engine.get_container_status("no-network-container").await.unwrap();
        assert_eq!(status.ip_address, None);
        
        engine.close().await;
    }
    
    #[tokio::test]
    async fn test_stats_collection() {
        let engine = setup_test_engine().await;
        
        // Create some test containers
        for i in 0..3 {
            let config = ContainerConfig {
                id: format!("container-{}", i),
                name: Some(format!("test-{}", i)),
                image_path: "/path/to/image".to_string(),
                command: "echo hello".to_string(),
                environment: HashMap::new(),
                memory_limit_mb: None,
                cpu_limit_percent: None,
                enable_network_namespace: i % 2 == 0, // Half with networking
                enable_pid_namespace: true,
                enable_mount_namespace: true,
                enable_uts_namespace: true,
                enable_ipc_namespace: true,
            };
            
            engine.create_container(config).await.unwrap();
            
            // Start one container
            if i == 0 {
                engine.update_container_state(&format!("container-{}", i), ContainerState::Starting).await.unwrap();
                engine.update_container_state(&format!("container-{}", i), ContainerState::Running).await.unwrap();
            }
        }
        
        let stats = engine.get_stats().await.unwrap();
        assert_eq!(stats.total_containers, 3);
        assert_eq!(stats.running_containers, 1);
        assert_eq!(stats.active_networks, 0); // None completed setup
        
        engine.close().await;
    }
} 