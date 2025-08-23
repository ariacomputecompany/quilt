// Warnings denied at workspace level via Cargo.toml

mod daemon;
mod utils;
mod icc;
mod sync;
mod grpc;

use utils::console::ConsoleLogger;
use utils::filesystem::FileSystemUtils;
use utils::command::CommandExecutor;
use sync::{SyncEngine, MountType, ContainerState};
use grpc::start_container_process;

use std::sync::Arc;
use std::collections::HashMap;
use std::time::Duration;
use tonic::{transport::Server, Request, Response, Status};
use uuid::Uuid;

// Include the generated protobuf code
pub mod quilt {
    tonic::include_proto!("quilt");
}

use quilt::quilt_service_server::{QuiltService, QuiltServiceServer};
use quilt::{
    CreateContainerRequest, CreateContainerResponse,
    GetContainerStatusRequest, GetContainerStatusResponse,
    GetContainerLogsRequest, GetContainerLogsResponse,
    StopContainerRequest, StopContainerResponse,
    RemoveContainerRequest, RemoveContainerResponse,
    ExecContainerRequest, ExecContainerResponse,
    StartContainerRequest, StartContainerResponse,
    KillContainerRequest, KillContainerResponse,
    GetContainerByNameRequest, GetContainerByNameResponse,
    CreateVolumeRequest, CreateVolumeResponse,
    RemoveVolumeRequest, RemoveVolumeResponse,
    ListVolumesRequest, ListVolumesResponse,
    InspectVolumeRequest, InspectVolumeResponse,
    GetHealthRequest, GetHealthResponse,
    GetMetricsRequest, GetMetricsResponse,
    GetSystemInfoRequest, GetSystemInfoResponse,
    StreamEventsRequest, ContainerEvent as ProtoContainerEvent,
    ContainerStatus, HealthCheck, ContainerMetric, SystemMetrics as ProtoSystemMetrics,
};

#[derive(Clone)]
pub struct QuiltServiceImpl {
    sync_engine: Arc<SyncEngine>,
    network_manager: Arc<icc::network::NetworkManager>,
    runtime: Arc<daemon::runtime::ContainerRuntime>,
    #[allow(dead_code)]  // Available for future inter-container messaging features
    message_broker: Arc<icc::messaging::MessageBroker>,
    start_time: std::time::SystemTime,
}

impl QuiltServiceImpl {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Initialize sync engine with database
        let sync_engine = Arc::new(SyncEngine::new("quilt.db").await?);
        
        // Start background services for monitoring and cleanup
        sync_engine.start_background_services().await?;
        
        ConsoleLogger::success("Sync engine initialized with background services");
        
        // Initialize ICC network manager
        let mut network_manager = icc::network::NetworkManager::new("quilt0", "10.42.0.0/16")
            .map_err(|e| format!("Failed to create network manager: {}", e))?;
        
        // CRITICAL: Ensure bridge is ready before any other network operations
        network_manager.ensure_bridge_ready()
            .map_err(|e| format!("Failed to setup network bridge: {}", e))?;
        
        ConsoleLogger::success("Bridge network initialized - containers can now communicate");
        
        // Start DNS server (non-critical - bridge networking works without DNS)
        match network_manager.start_dns_server().await {
            Ok(()) => {
                ConsoleLogger::success("DNS server started - containers can resolve names");
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("DNS server startup failed (non-critical): {}", e));
                ConsoleLogger::info("Bridge networking is fully functional - containers can communicate via IP addresses");
            }
        }
        
        ConsoleLogger::success("Network manager initialized with bridge networking");
        
        // Initialize MessageBroker for inter-container communication
        let message_broker = icc::messaging::MessageBroker::new();
        message_broker.start();
        
        // Initialize container runtime
        let runtime = daemon::runtime::ContainerRuntime::new();
        
        Ok(Self {
            sync_engine,
            network_manager: Arc::new(network_manager),
            runtime: Arc::new(runtime),
            message_broker: Arc::new(message_broker),
            start_time: std::time::SystemTime::now(),
        })
    }
}

#[tonic::async_trait]
impl QuiltService for QuiltServiceImpl {
    async fn create_container(
        &self,
        request: Request<CreateContainerRequest>,
    ) -> Result<Response<CreateContainerResponse>, Status> {
        let req = request.into_inner();
        let container_id = Uuid::new_v4().to_string();

        ConsoleLogger::container_created(&container_id);
        
        // Emit container created event
        sync::events::global_event_buffer().emit(
            sync::events::EventType::Created,
            &container_id,
            None,
        );

        // Convert gRPC request to sync engine container config
        let config = sync::containers::ContainerConfig {
            id: container_id.clone(),
            name: if req.name.is_empty() { None } else { Some(req.name) },
            image_path: req.image_path,
            command: if req.command.is_empty() { 
                if req.async_mode {
                    // Use tail -f /dev/null as primary, with fallback to while loop
                    "tail -f /dev/null || while true; do sleep 3600; done".to_string()
                } else {
                    return Err(Status::invalid_argument("Command required for non-async containers"));
                }
            } else { 
                req.command.join(" ")
            },
            environment: req.environment,
            memory_limit_mb: if req.memory_limit_mb > 0 { Some(req.memory_limit_mb as i64) } else { None },
            cpu_limit_percent: if req.cpu_limit_percent > 0.0 { Some(req.cpu_limit_percent as f64) } else { None },
            enable_network_namespace: req.enable_network_namespace,
            enable_pid_namespace: req.enable_pid_namespace,
            enable_mount_namespace: req.enable_mount_namespace,
            enable_uts_namespace: req.enable_uts_namespace,
            enable_ipc_namespace: req.enable_ipc_namespace,
        };

        // ✅ NON-BLOCKING: Create container with coordinated network allocation
        match self.sync_engine.create_container(config).await {
            Ok(_network_config) => {
                // ✅ INSTANT RETURN: Container creation is coordinated but non-blocking
                ConsoleLogger::success(&format!("Container {} created with network config", container_id));
                
                // Store creation log
                let _ = self.sync_engine.store_container_log(&container_id, "info", "Container created and configured").await;
                
                // Process mounts BEFORE starting container with security validation
                for mount in req.mounts {
                    let mount_type = match mount.r#type() {
                        quilt::MountType::Bind => MountType::Bind,
                        quilt::MountType::Volume => MountType::Volume,
                        quilt::MountType::Tmpfs => MountType::Tmpfs,
                    };
                    
                    // Convert to validation format for security check
                    use crate::utils::security::SecurityValidator;
                    use crate::utils::validation::{VolumeMount, MountType as ValidationMountType};
                    
                    let validation_mount = VolumeMount {
                        source: mount.source.clone(),
                        target: mount.target.clone(),
                        mount_type: match mount_type {
                            MountType::Bind => ValidationMountType::Bind,
                            MountType::Volume => ValidationMountType::Volume, 
                            MountType::Tmpfs => ValidationMountType::Tmpfs,
                        },
                        readonly: mount.readonly,
                        options: mount.options.clone(),
                    };
                    
                    // Validate mount for security issues
                    if let Err(e) = SecurityValidator::validate_mount(&validation_mount) {
                        ConsoleLogger::error(&format!("Mount security validation failed for container {}: {}", container_id, e));
                        return Ok(Response::new(CreateContainerResponse {
                            container_id: String::new(),
                            success: false,
                            error_message: format!("Mount security validation failed: {}", e),
                        }));
                    }
                    
                    ConsoleLogger::debug(&format!("Mount security validation passed for {}: {} -> {}", 
                        container_id, mount.source, mount.target));
                    
                    // For named volumes, auto-create if needed
                    if mount_type == MountType::Volume {
                        match self.sync_engine.get_volume(&mount.source).await {
                            Ok(None) => {
                                // Volume doesn't exist, create it
                                ConsoleLogger::info(&format!("Auto-creating volume '{}'", mount.source));
                                if let Err(e) = self.sync_engine.create_volume(&mount.source, None, HashMap::new(), HashMap::new()).await {
                                    ConsoleLogger::warning(&format!("Failed to auto-create volume '{}': {}", mount.source, e));
                                }
                            }
                            Ok(Some(_)) => {
                                // Volume exists, nothing to do
                            }
                            Err(e) => {
                                ConsoleLogger::warning(&format!("Error checking volume '{}': {}", mount.source, e));
                            }
                        }
                    }
                    
                    if let Err(e) = self.sync_engine.add_container_mount(
                        &container_id,
                        &mount.source,
                        &mount.target,
                        mount_type,
                        mount.readonly,
                        mount.options,
                    ).await {
                        ConsoleLogger::error(&format!("Failed to add mount for container {}: {}", container_id, e));
                        // Mount failure should be fatal
                        return Ok(Response::new(CreateContainerResponse {
                            container_id: String::new(),
                            success: false,
                            error_message: format!("Failed to configure mount: {}", e),
                        }));
                    }
                    
                    ConsoleLogger::success(&format!("Mount successfully added for {}: {} -> {} (readonly: {})", 
                        container_id, mount.source, mount.target, mount.readonly));
                }
                
                // Now start the container with mounts already configured
                let sync_engine = self.sync_engine.clone();
                let network_manager = self.network_manager.clone();
                let container_id_clone = container_id.clone();
                tokio::spawn(async move {
                    // Add timeout to prevent hanging containers
                    let startup_timeout = std::time::Duration::from_secs(120); // 2 minute timeout
                    let task_start = std::time::Instant::now();
                    
                    ConsoleLogger::info(&format!("⏰ [TASK-SPAWN] Starting container {} with {:?} timeout", 
                        container_id_clone, startup_timeout));
                    
                    let startup_result = tokio::time::timeout(
                        startup_timeout,
                        start_container_process(&sync_engine, &container_id_clone, network_manager)
                    ).await;
                    
                    match startup_result {
                        Ok(Ok(())) => {
                            ConsoleLogger::success(&format!("🎯 [TASK-COMPLETE] Container {} startup completed successfully in {:?}", 
                                container_id_clone, task_start.elapsed()));
                        }
                        Ok(Err(e)) => {
                            ConsoleLogger::error(&format!("💥 [TASK-ERROR] Failed to start container process {} after {:?}: {}", 
                                container_id_clone, task_start.elapsed(), e));
                            let _ = sync_engine.update_container_state(&container_id_clone, ContainerState::Error).await;
                        }
                        Err(_) => {
                            ConsoleLogger::error(&format!("⏰ [TASK-TIMEOUT] Container {} startup timed out after {:?} (limit: {:?})", 
                                container_id_clone, task_start.elapsed(), startup_timeout));
                            let _ = sync_engine.update_container_state(&container_id_clone, ContainerState::Error).await;
                        }
                    }
                });
                
                Ok(Response::new(CreateContainerResponse {
                    container_id,
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                ConsoleLogger::error(&format!("Failed to create container: {}", e));
                Ok(Response::new(CreateContainerResponse {
                    container_id: String::new(),
                    success: false,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    async fn get_container_status(
        &self,
        request: Request<GetContainerStatusRequest>,
    ) -> Result<Response<GetContainerStatusResponse>, Status> {
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Err(Status::not_found(format!("Container with name '{}' not found", req.container_name))),
            }
        } else {
            req.container_id.clone()
        };
        
        ConsoleLogger::debug(&format!("🔍 [GRPC] Status request for: {}", container_id));
        
        // ✅ ALWAYS FAST: Direct database query, never blocks
        match self.sync_engine.get_container_status(&container_id).await {
            Ok(status) => {
                let grpc_status = match status.state {
                    ContainerState::Created => ContainerStatus::Pending,
                    ContainerState::Starting => ContainerStatus::Pending,
                    ContainerState::Running => ContainerStatus::Running,
                    ContainerState::Exited => ContainerStatus::Exited,
                    ContainerState::Error => ContainerStatus::Failed,
                };

                // Get enhanced runtime statistics if container is running
                let mut memory_usage_bytes = 0i64;
                if status.state == ContainerState::Running && status.pid.is_some() {
                    if let Ok(runtime_stats) = self.runtime.get_container_stats(&container_id) {
                        if let Some(memory_str) = runtime_stats.get("memory_usage_bytes") {
                            memory_usage_bytes = memory_str.parse().unwrap_or(0);
                        }
                        ConsoleLogger::debug(&format!("Runtime stats for {}: {} entries", container_id, runtime_stats.len()));
                    }
                }

                ConsoleLogger::debug(&format!("✅ [GRPC] Status for {}: {:?}", req.container_id, grpc_status));
                
                Ok(Response::new(GetContainerStatusResponse {
                    container_id: req.container_id,
                    status: grpc_status as i32,
                    exit_code: status.exit_code.unwrap_or(0) as i32,
                    error_message: if status.state == ContainerState::Error { "Container failed".to_string() } else { String::new() },
                    pid: status.pid.unwrap_or(0) as i32,
                    created_at: status.created_at as u64,
                    memory_usage_bytes: memory_usage_bytes as u64,
                    rootfs_path: status.rootfs_path.unwrap_or_default(),
                    ip_address: status.ip_address.unwrap_or_default(),
                }))
            }
            Err(_) => {
                ConsoleLogger::debug(&format!("❌ [GRPC] Container not found: {}", req.container_id));
                Err(Status::not_found(format!("Container {} not found", req.container_id)))
            }
        }
    }

    async fn get_container_logs(
        &self,
        request: Request<GetContainerLogsRequest>,
    ) -> Result<Response<GetContainerLogsResponse>, Status> {
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Err(Status::not_found(format!("Container with name '{}' not found", req.container_name))),
            }
        } else {
            req.container_id.clone()
        };

        // Get logs from both sync engine and runtime for comprehensive logging
        let mut all_logs = Vec::new();
        
        // Get logs from sync engine (structured logs)
        match self.sync_engine.get_container_logs(&container_id, None).await {
            Ok(logs) => {
                for log in logs {
                    all_logs.push(quilt::LogEntry {
                        timestamp: log.timestamp as u64,
                        message: format!("[{}] {}", log.level.to_uppercase(), log.message),
                    });
                }
            }
            Err(e) => {
                tracing::warn!("Failed to get sync engine logs for container {}: {}", container_id, e);
            }
        }
        
        // Also get logs from runtime (container output logs)
        use crate::daemon::runtime::ContainerRuntime;
        let runtime = ContainerRuntime::new();
        if let Some(runtime_logs) = runtime.get_container_logs(&container_id) {
            for (i, log_line) in runtime_logs.iter().enumerate() {
                all_logs.push(quilt::LogEntry {
                    timestamp: (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - runtime_logs.len() as u64 + i as u64) as u64,
                    message: format!("[RUNTIME] {}", log_line),
                });
            }
        }
        
        // Add comprehensive filesystem inspection for debugging
        use crate::utils::filesystem::FileSystemUtils;
        let current_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        
        // Inspect container rootfs if accessible
        if let Ok(status) = self.sync_engine.get_container_status(&container_id).await {
            if let Some(rootfs) = status.rootfs_path {
                if FileSystemUtils::exists(&rootfs) {
                    if FileSystemUtils::is_directory(&rootfs) {
                        all_logs.push(quilt::LogEntry {
                            timestamp: current_time,
                            message: format!("[INSPECT] Container rootfs exists at: {}", rootfs),
                        });
                        
                        // Check key directories
                        let key_dirs = vec!["/bin", "/usr/bin", "/tmp", "/var", "/etc"];
                        for dir in key_dirs {
                            let full_path = FileSystemUtils::join(&rootfs, dir);
                            if FileSystemUtils::exists(&full_path) {
                                let info = if FileSystemUtils::is_directory(&full_path) {
                                    "directory exists"
                                } else if FileSystemUtils::is_file(&full_path) {
                                    "exists as file"
                                } else {
                                    "exists (unknown type)"
                                };
                                all_logs.push(quilt::LogEntry {
                                    timestamp: current_time + 1,
                                    message: format!("[INSPECT] {} - {}", dir, info),
                                });
                            }
                        }
                        
                        // Check important files
                        let important_files = vec!["/bin/sh", "/bin/busybox", "/usr/local/bin/quilt_ready"];
                        for file in important_files {
                            let full_path = FileSystemUtils::join(&rootfs, file);
                            if FileSystemUtils::exists(&full_path) {
                                if FileSystemUtils::is_file(&full_path) {
                                    let executable = if FileSystemUtils::is_executable(&full_path) {
                                        "executable"
                                    } else {
                                        "not executable"
                                    };
                                    if let Ok(size) = FileSystemUtils::get_file_size(&full_path) {
                                        all_logs.push(quilt::LogEntry {
                                            timestamp: current_time + 2,
                                            message: format!("[INSPECT] {} - {} ({} bytes, {})", file, "exists", size, executable),
                                        });
                                    }
                                } else if FileSystemUtils::is_broken_symlink(&full_path) {
                                    all_logs.push(quilt::LogEntry {
                                        timestamp: current_time + 2,
                                        message: format!("[INSPECT] {} - broken symlink detected", file),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // Sort logs by timestamp for chronological order
        all_logs.sort_by_key(|log| log.timestamp);
        
        Ok(Response::new(GetContainerLogsResponse {
            container_id,
            logs: all_logs,
        }))
    }

    async fn stop_container(
        &self,
        request: Request<StopContainerRequest>,
    ) -> Result<Response<StopContainerResponse>, Status> {
        use crate::daemon::runtime::ContainerRuntime;
        
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Ok(Response::new(StopContainerResponse {
                    success: false,
                    error_message: format!("Container with name '{}' not found", req.container_name),
                })),
            }
        } else {
            req.container_id.clone()
        };

        // Use the comprehensive runtime stop_container method
        let runtime = ContainerRuntime::new();
        match runtime.stop_container(&container_id) {
            Ok(()) => {
                // Update sync engine state
                if let Err(e) = self.sync_engine.update_container_state(&container_id, ContainerState::Exited).await {
                    ConsoleLogger::warning(&format!("Failed to update container state in sync engine: {}", e));
                }
                
                // Stop monitoring in sync engine
                let _ = self.sync_engine.stop_monitoring(&container_id).await;
                
                // Store stop log
                let _ = self.sync_engine.store_container_log(&container_id, "info", "Container stopped successfully").await;
                
                // Emit container stopped event
                sync::events::global_event_buffer().emit(
                    sync::events::EventType::Stopped,
                    &container_id,
                    None,
                );

                Ok(Response::new(StopContainerResponse {
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                // Store error log
                let _ = self.sync_engine.store_container_log(&container_id, "error", &format!("Failed to stop container: {}", e)).await;
                
                ConsoleLogger::error(&format!("Failed to stop container {}: {}", container_id, e));
                Ok(Response::new(StopContainerResponse {
                    success: false,
                    error_message: e,
                }))
            }
        }
    }

    async fn remove_container(
        &self,
        request: Request<RemoveContainerRequest>,
    ) -> Result<Response<RemoveContainerResponse>, Status> {
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Ok(Response::new(RemoveContainerResponse {
                    success: false,
                    error_message: format!("Container with name '{}' not found", req.container_name),
                })),
            }
        } else {
            req.container_id.clone()
        };

        // Use both runtime cleanup and sync engine cleanup for comprehensive removal
        use crate::daemon::runtime::ContainerRuntime;
        let runtime = ContainerRuntime::new();
        
        // First, attempt runtime removal (handles process stopping and resource cleanup)
        let runtime_result = runtime.remove_container(&container_id);
        
        // Then, remove from sync engine (handles database cleanup)
        match self.sync_engine.delete_container(&container_id).await {
            Ok(()) => {
                // Comprehensive cleanup using all sync engine methods
                
                // Remove container mounts
                if let Err(e) = self.sync_engine.remove_container_mounts(&container_id).await {
                    ConsoleLogger::warning(&format!("Failed to remove mounts for {}: {}", container_id, e));
                }
                
                // Cleanup container logs (keep last 10 for debugging)
                if let Ok(cleaned_count) = self.sync_engine.cleanup_container_logs(&container_id, 10).await {
                    ConsoleLogger::debug(&format!("Cleaned up {} log entries for {}", cleaned_count, container_id));
                }
                
                // Unregister from DNS
                let _ = self.network_manager.unregister_container_dns(&container_id);
                
                // Log runtime result for debugging
                if let Err(e) = runtime_result {
                    ConsoleLogger::warning(&format!("Runtime cleanup issues for {}: {}", container_id, e));
                }
                
                ConsoleLogger::success(&format!("Container {} removed with comprehensive cleanup", container_id));
                
                // Store removal log
                let _ = self.sync_engine.store_container_log(&container_id, "info", "Container removed successfully").await;
                
                // Emit container removed event
                sync::events::global_event_buffer().emit(
                    sync::events::EventType::Removed,
                    &container_id,
                    None,
                );
                
                Ok(Response::new(RemoveContainerResponse {
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                ConsoleLogger::error(&format!("Failed to remove container {}: {}", container_id, e));
                Ok(Response::new(RemoveContainerResponse {
                    success: false,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    async fn exec_container(
        &self,
        request: Request<ExecContainerRequest>,
    ) -> Result<Response<ExecContainerResponse>, Status> {
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Ok(Response::new(ExecContainerResponse {
                    success: false,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    error_message: format!("Container with name '{}' not found", req.container_name),
                })),
            }
        } else {
            req.container_id.clone()
        };
        
        ConsoleLogger::debug(&format!("🔍 [GRPC] Exec request for: {} with command: {:?}", container_id, req.command));
        
        // Handle script copying if needed
        if req.copy_script && req.command.len() == 1 {
            let script_path = &req.command[0];
            if FileSystemUtils::exists(script_path) {
                // Copy script to container
                match self.sync_engine.get_container_status(&container_id).await {
                    Ok(status) => {
                        if let Some(rootfs_path) = status.rootfs_path {
                            let dest_path = format!("{}/tmp/script.sh", rootfs_path);
                            if let Err(e) = FileSystemUtils::copy_file(script_path, &dest_path) {
                                return Ok(Response::new(ExecContainerResponse {
                                    success: false,
                                    exit_code: -1,
                                    stdout: String::new(),
                                    stderr: String::new(),
                                    error_message: format!("Failed to copy script: {}", e),
                                }));
                            }
                            // Make script executable
                            let _ = FileSystemUtils::make_executable(&dest_path);
                        }
                    }
                    Err(e) => {
                        return Ok(Response::new(ExecContainerResponse {
                            success: false,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: String::new(),
                            error_message: format!("Failed to get container info: {}", e),
                        }));
                    }
                }
            }
        }
        
        // Get container status to check if it's running and get PID
        match self.sync_engine.get_container_status(&container_id).await {
            Ok(status) => {
                if status.state != ContainerState::Running {
                    return Ok(Response::new(ExecContainerResponse {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        error_message: format!("Container {} is not running (state: {:?})", container_id, status.state),
                    }));
                }

                let pid = match status.pid {
                    Some(pid) => pid,
                    None => {
                        return Ok(Response::new(ExecContainerResponse {
                            success: false,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: String::new(),
                            error_message: "Container has no PID".to_string(),
                        }));
                    }
                };

                // Handle script copying if requested
                let command_to_execute = if req.copy_script && req.command.len() == 1 {
                    let script_path = &req.command[0];
                    
                    // Read the local script file
                    match FileSystemUtils::read_file(script_path) {
                        Ok(script_content) => {
                            // Generate unique script name
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs();
                            let temp_script = format!("/tmp/quilt_exec_{}", timestamp);
                            
                            // Copy script to container using nsenter with chroot
                            // SECURITY NOTE: This nsenter command is validated - PID checked before execution
                            let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
                            let copy_cmd = format!(
                                "nsenter -t {} -p -m -n -u -- chroot {} /bin/sh -c 'cat > {} << 'EOF'\n{}\nEOF\nchmod +x {}'",
                                pid, rootfs_path, temp_script, script_content, temp_script
                            );
                            
                            match utils::command::CommandExecutor::execute_shell(&copy_cmd) {
                                Ok(_) => {
                                    ConsoleLogger::debug(&format!("✅ Copied script to container: {}", temp_script));
                                    // Return the temporary script path to execute
                                    temp_script
                                }
                                Err(e) => {
                                    return Ok(Response::new(ExecContainerResponse {
                                        success: false,
                                        exit_code: -1,
                                        stdout: String::new(),
                                        stderr: String::new(),
                                        error_message: format!("Failed to copy script to container: {}", e),
                                    }));
                                }
                            }
                        }
                        Err(e) => {
                            return Ok(Response::new(ExecContainerResponse {
                                success: false,
                                exit_code: -1,
                                stdout: String::new(),
                                stderr: String::new(),
                                error_message: format!("Failed to read script file: {}", e),
                            }));
                        }
                    }
                } else {
                    req.command.join(" ")
                };

                // Execute command using nsenter with chroot to match container's view
                // SECURITY NOTE: Container PID validated before reaching this point
                // Get the rootfs path for the container
                let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
                
                // Escape the command for shell execution
                // Using double quotes to allow shell expansion (redirects, pipes, etc.)
                let escaped_command = command_to_execute.replace("\\", "\\\\")
                    .replace("\"", "\\\"")
                    .replace("$", "\\$")
                    .replace("`", "\\`");
                
                // Set PATH to include busybox binaries
                let path_prefix = "export PATH=/bin:/usr/bin:/sbin:/usr/sbin:$PATH; ";
                // Note: We're not using IPC namespace (-i) by default as it's disabled in NamespaceConfig::default()
                let exec_cmd = if req.capture_output {
                    format!("nsenter -t {} -p -m -n -u -- chroot {} /bin/sh -c \"{}{}\"", pid, rootfs_path, path_prefix, escaped_command)
                } else {
                    format!("nsenter -t {} -p -m -n -u -- chroot {} /bin/sh -c \"{}{}\" >/dev/null 2>&1", pid, rootfs_path, path_prefix, escaped_command)
                };

                // Primary execution using CommandExecutor with fallback to runtime method
                match CommandExecutor::execute_shell(&exec_cmd) {
                    Ok(result) => {
                        ConsoleLogger::debug(&format!("✅ [GRPC] Exec completed with exit code: {}", result.exit_code.unwrap_or(-1)));
                        
                        // Clean up temporary script if we created one
                        if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                            let cleanup_cmd = format!(
                                "nsenter -t {} -p -m -n -u -- chroot {} rm -f {}",
                                pid, rootfs_path, command_to_execute
                            );
                            let _ = CommandExecutor::execute_shell(&cleanup_cmd);
                        }
                        
                        // Check if command failed due to "command not found" or similar
                        let command_not_found = result.stderr.contains("not found") || 
                                              result.stderr.contains("No such file") ||
                                              result.stderr.contains("can't execute");
                        
                        // Set success based on exit code AND command existence
                        let success = result.success && !command_not_found;
                        let error_message = if command_not_found {
                            format!("Command not found: {}", req.command.join(" "))
                        } else if !result.success {
                            format!("Command failed with exit code {}", result.exit_code.unwrap_or(-1))
                        } else {
                            String::new()
                        };
                        
                        Ok(Response::new(ExecContainerResponse {
                            success,
                            exit_code: result.exit_code.unwrap_or(-1),
                            stdout: result.stdout,
                            stderr: result.stderr,
                            error_message,
                        }))
                    }
                    Err(e) => {
                        ConsoleLogger::warning(&format!("⚠️ [GRPC] CommandExecutor failed, trying runtime exec: {}", e));
                        
                        // Fallback to runtime exec_container method for enhanced reliability
                        use crate::daemon::runtime::ContainerRuntime;
                        let runtime = ContainerRuntime::new();
                        
                        match runtime.exec_container(&container_id, req.command.clone(), Some(req.working_directory), req.environment, true) {
                            Ok((exit_code, stdout, stderr)) => {
                                ConsoleLogger::debug(&format!("✅ [GRPC] Runtime exec completed with exit code: {}", exit_code));
                                
                                // Clean up temporary script on success
                                if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                                    let cleanup_cmd = format!(
                                        "nsenter -t {} -p -m -n -u -- chroot {} rm -f {}",
                                        pid, rootfs_path, command_to_execute
                                    );
                                    let _ = CommandExecutor::execute_shell(&cleanup_cmd);
                                }
                                
                                Ok(Response::new(ExecContainerResponse {
                                    success: exit_code == 0,
                                    exit_code,
                                    stdout,
                                    stderr,
                                    error_message: if exit_code != 0 { format!("Command failed with exit code {}", exit_code) } else { String::new() },
                                }))
                            }
                            Err(runtime_error) => {
                                ConsoleLogger::error(&format!("❌ [GRPC] Both exec methods failed. CommandExecutor: {}, Runtime: {}", e, runtime_error));
                                
                                // Clean up temporary script on error
                                if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                                    let cleanup_cmd = format!(
                                        "nsenter -t {} -p -m -n -u -- chroot {} rm -f {}",
                                        pid, rootfs_path, command_to_execute
                                    );
                                    let _ = CommandExecutor::execute_shell(&cleanup_cmd);
                                }
                                
                                Ok(Response::new(ExecContainerResponse {
                                    success: false,
                                    exit_code: -1,
                                    stdout: String::new(),
                                    stderr: String::new(),
                                    error_message: format!("Exec failed: {} (Runtime fallback: {})", e, runtime_error),
                                }))
                            }
                        }
                    }
                }
            }
            Err(_) => {
                Err(Status::not_found(format!("Container {} not found", req.container_id)))
            }
        }
    }
    
    async fn start_container(
        &self,
        request: Request<StartContainerRequest>,
    ) -> Result<Response<StartContainerResponse>, Status> {
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Ok(Response::new(StartContainerResponse {
                    success: false,
                    error_message: format!("Container with name '{}' not found", req.container_name),
                    pid: 0,
                })),
            }
        } else {
            req.container_id.clone()
        };
        
        ConsoleLogger::info(&format!("Starting container {}", container_id));
        
        // Check current state
        match self.sync_engine.get_container_status(&container_id).await {
            Ok(status) => {
                if status.state == ContainerState::Running {
                    return Ok(Response::new(StartContainerResponse {
                        success: false,
                        error_message: "Container is already running".to_string(),
                        pid: status.pid.unwrap_or(0) as i32,
                    }));
                }
                
                if status.state != ContainerState::Created && status.state != ContainerState::Exited {
                    return Ok(Response::new(StartContainerResponse {
                        success: false,
                        error_message: format!("Cannot start container in state: {:?}", status.state),
                        pid: 0,
                    }));
                }
            }
            Err(e) => {
                return Ok(Response::new(StartContainerResponse {
                    success: false,
                    error_message: format!("Container not found: {}", e),
                    pid: 0,
                }));
            }
        }
        
        // Start the container process in background
        let sync_engine = self.sync_engine.clone();
        let network_manager = self.network_manager.clone();
        let container_id_clone = container_id.clone();
        tokio::spawn(async move {
            if let Err(e) = start_container_process(&sync_engine, &container_id_clone, network_manager).await {
                ConsoleLogger::error(&format!("Failed to start container process {}: {}", container_id_clone, e));
                let _ = sync_engine.update_container_state(&container_id_clone, ContainerState::Error).await;
            }
        });
        
        Ok(Response::new(StartContainerResponse {
            success: true,
            error_message: String::new(),
            pid: 0, // Will be set once container starts
        }))
    }
    
    async fn kill_container(
        &self,
        request: Request<KillContainerRequest>,
    ) -> Result<Response<KillContainerResponse>, Status> {
        let req = request.into_inner();
        
        // Resolve container name to ID if needed
        let container_id = if !req.container_name.is_empty() {
            match self.sync_engine.get_container_by_name(&req.container_name).await {
                Ok(id) => id,
                Err(_) => return Ok(Response::new(KillContainerResponse {
                    success: false,
                    error_message: format!("Container with name '{}' not found", req.container_name),
                })),
            }
        } else {
            req.container_id.clone()
        };
        
        ConsoleLogger::warning(&format!("Killing container {}", container_id));
        
        // Get container PID
        match self.sync_engine.get_container_status(&container_id).await {
            Ok(status) => {
                if status.state != ContainerState::Running {
                    return Ok(Response::new(KillContainerResponse {
                        success: false,
                        error_message: format!("Container is not running (state: {:?})", status.state),
                    }));
                }
                
                if let Some(pid) = status.pid {
                    // Kill the process immediately with SIGKILL
                    use nix::sys::signal::Signal;
                    use crate::utils::process::ProcessUtils;
                    
                    let nix_pid = ProcessUtils::i32_to_pid(pid as i32);
                    match ProcessUtils::send_signal(nix_pid, Signal::SIGKILL) {
                        Ok(()) => {
                            ConsoleLogger::debug(&format!("Sent SIGKILL to process {}", pid));
                            
                            // Wait briefly to ensure process is dead
                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                            
                            // Verify process is dead
                            if !ProcessUtils::is_process_running(nix_pid) {
                                ConsoleLogger::debug(&format!("Process {} confirmed dead", pid));
                            } else {
                                ConsoleLogger::warning(&format!("Process {} still exists after SIGKILL", pid));
                            }
                            
                            // Update state to exited
                            let _ = self.sync_engine.update_container_state(&container_id, ContainerState::Exited).await;
                            let _ = self.sync_engine.set_container_exit_code(&container_id, -9).await;
                            
                            // Stop monitoring
                            let _ = self.sync_engine.stop_monitoring(&container_id).await;
                            
                            // Trigger cleanup
                            let _ = self.sync_engine.trigger_cleanup(&container_id).await;
                            
                            Ok(Response::new(KillContainerResponse {
                                success: true,
                                error_message: String::new(),
                            }))
                        }
                        Err(e) => {
                            if e.contains("ESRCH") || e.contains("does not exist") {
                                // Process already dead
                                let _ = self.sync_engine.update_container_state(&container_id, ContainerState::Exited).await;
                                let _ = self.sync_engine.stop_monitoring(&container_id).await;
                                
                                Ok(Response::new(KillContainerResponse {
                                    success: true,
                                    error_message: String::new(),
                                }))
                            } else {
                                Ok(Response::new(KillContainerResponse {
                                    success: false,
                                    error_message: format!("Failed to kill process: {}", e),
                                }))
                            }
                        }
                    }
                } else {
                    Ok(Response::new(KillContainerResponse {
                        success: false,
                        error_message: "Container has no PID".to_string(),
                    }))
                }
            }
            Err(e) => Ok(Response::new(KillContainerResponse {
                success: false,
                error_message: format!("Failed to get container status: {}", e),
            }))
        }
    }
    
    async fn get_container_by_name(
        &self,
        request: Request<GetContainerByNameRequest>,
    ) -> Result<Response<GetContainerByNameResponse>, Status> {
        let req = request.into_inner();
        
        match self.sync_engine.get_container_by_name(&req.name).await {
            Ok(container_id) => Ok(Response::new(GetContainerByNameResponse {
                container_id,
                found: true,
                error_message: String::new(),
            })),
            Err(_) => Ok(Response::new(GetContainerByNameResponse {
                container_id: String::new(),
                found: false,
                error_message: format!("Container with name '{}' not found", req.name),
            }))
        }
    }

    async fn create_volume(
        &self,
        request: Request<CreateVolumeRequest>,
    ) -> Result<Response<CreateVolumeResponse>, Status> {
        let req = request.into_inner();
        
        match self.sync_engine.create_volume(
            &req.name,
            if req.driver.is_empty() { None } else { Some(&req.driver) },
            req.labels,
            req.options,
        ).await {
            Ok(volume) => {
                Ok(Response::new(CreateVolumeResponse {
                    success: true,
                    error_message: String::new(),
                    volume: Some(quilt::Volume {
                        name: volume.name,
                        driver: volume.driver,
                        mount_point: volume.mount_point,
                        labels: volume.labels,
                        options: volume.options,
                        created_at: volume.created_at,
                    }),
                }))
            }
            Err(e) => {
                Ok(Response::new(CreateVolumeResponse {
                    success: false,
                    error_message: e.to_string(),
                    volume: None,
                }))
            }
        }
    }

    async fn remove_volume(
        &self,
        request: Request<RemoveVolumeRequest>,
    ) -> Result<Response<RemoveVolumeResponse>, Status> {
        let req = request.into_inner();
        
        match self.sync_engine.remove_volume(&req.name, req.force).await {
            Ok(()) => {
                Ok(Response::new(RemoveVolumeResponse {
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                Ok(Response::new(RemoveVolumeResponse {
                    success: false,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    async fn list_volumes(
        &self,
        request: Request<ListVolumesRequest>,
    ) -> Result<Response<ListVolumesResponse>, Status> {
        let req = request.into_inner();
        
        match self.sync_engine.list_volumes(
            if req.filters.is_empty() { None } else { Some(req.filters) }
        ).await {
            Ok(volumes) => {
                let proto_volumes: Vec<quilt::Volume> = volumes.into_iter().map(|v| {
                    quilt::Volume {
                        name: v.name,
                        driver: v.driver,
                        mount_point: v.mount_point,
                        labels: v.labels,
                        options: v.options,
                        created_at: v.created_at,
                    }
                }).collect();
                
                Ok(Response::new(ListVolumesResponse {
                    volumes: proto_volumes,
                }))
            }
            Err(e) => {
                ConsoleLogger::error(&format!("Failed to list volumes: {}", e));
                Ok(Response::new(ListVolumesResponse {
                    volumes: vec![],
                }))
            }
        }
    }

    async fn inspect_volume(
        &self,
        request: Request<InspectVolumeRequest>,
    ) -> Result<Response<InspectVolumeResponse>, Status> {
        let req = request.into_inner();
        
        match self.sync_engine.get_volume(&req.name).await {
            Ok(Some(volume)) => {
                Ok(Response::new(InspectVolumeResponse {
                    found: true,
                    volume: Some(quilt::Volume {
                        name: volume.name,
                        driver: volume.driver,
                        mount_point: volume.mount_point,
                        labels: volume.labels,
                        options: volume.options,
                        created_at: volume.created_at,
                    }),
                    error_message: String::new(),
                }))
            }
            Ok(None) => {
                Ok(Response::new(InspectVolumeResponse {
                    found: false,
                    volume: None,
                    error_message: format!("Volume '{}' not found", req.name),
                }))
            }
            Err(e) => {
                Ok(Response::new(InspectVolumeResponse {
                    found: false,
                    volume: None,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    async fn get_health(
        &self,
        _request: Request<GetHealthRequest>,
    ) -> Result<Response<GetHealthResponse>, Status> {
        use std::time::Instant;
        
        let mut checks = Vec::new();
        let mut overall_healthy = true;
        
        // Check database connection
        let db_start = Instant::now();
        let db_healthy = match sqlx::query("SELECT 1").fetch_one(self.sync_engine.pool()).await {
            Ok(_) => true,
            Err(_) => {
                overall_healthy = false;
                false
            }
        };
        checks.push(HealthCheck {
            name: "database".to_string(),
            healthy: db_healthy,
            message: if db_healthy { "Connected".to_string() } else { "Connection failed".to_string() },
            duration_ms: db_start.elapsed().as_millis() as u64,
        });
        
        // Check cgroups availability
        let cgroup_start = Instant::now();
        let cgroup_healthy = FileSystemUtils::exists("/sys/fs/cgroup");
        if !cgroup_healthy {
            overall_healthy = false;
        }
        checks.push(HealthCheck {
            name: "cgroups".to_string(),
            healthy: cgroup_healthy,
            message: if cgroup_healthy { "Available".to_string() } else { "Not available".to_string() },
            duration_ms: cgroup_start.elapsed().as_millis() as u64,
        });
        
        // Get container counts
        let (containers_total, containers_running) = match self.sync_engine.get_container_counts().await {
            Ok((total, running)) => (total as u32, running as u32),
            Err(_) => (0, 0),
        };
        
        // Calculate uptime
        let uptime_seconds = self.start_time.elapsed().unwrap_or_default().as_secs();
        
        Ok(Response::new(GetHealthResponse {
            healthy: overall_healthy,
            status: if overall_healthy { "healthy".to_string() } else { "degraded".to_string() },
            uptime_seconds,
            containers_running,
            containers_total,
            checks,
        }))
    }

    async fn get_metrics(
        &self,
        request: Request<GetMetricsRequest>,
    ) -> Result<Response<GetMetricsResponse>, Status> {
        let req = request.into_inner();
        use crate::daemon::metrics::{MetricsCollector, SystemMetrics};
        
        let mut container_metrics = Vec::new();
        
        // Get container metrics - use historical data if time range specified
        if !req.container_id.is_empty() {
            if req.start_time > 0 && req.end_time > 0 {
                // Historical metrics requested
                if let Ok(historical_metrics) = self.sync_engine.get_metrics_history(
                    &req.container_id, 
                    req.start_time, 
                    req.end_time, 
                    Some(1000)
                ).await {
                    for metrics in historical_metrics {
                        container_metrics.push(ContainerMetric {
                            container_id: metrics.container_id.clone(),
                            timestamp: metrics.timestamp,
                            cpu_usage_usec: metrics.cpu.usage_usec,
                            cpu_user_usec: metrics.cpu.user_usec,
                            cpu_system_usec: metrics.cpu.system_usec,
                            cpu_throttled_usec: metrics.cpu.throttled_usec,
                            memory_current_bytes: metrics.memory.current_bytes,
                            memory_peak_bytes: metrics.memory.peak_bytes,
                            memory_limit_bytes: metrics.memory.limit_bytes,
                            memory_cache_bytes: metrics.memory.cache_bytes,
                            memory_rss_bytes: metrics.memory.rss_bytes,
                            network_rx_bytes: metrics.network.rx_bytes,
                            network_tx_bytes: metrics.network.tx_bytes,
                            network_rx_packets: metrics.network.rx_packets,
                            network_tx_packets: metrics.network.tx_packets,
                            disk_read_bytes: metrics.disk.read_bytes,
                            disk_write_bytes: metrics.disk.write_bytes,
                        });
                    }
                }
            } else {
                // Real-time metrics requested - try cached first, then fresh collection
                if let Ok(status) = self.sync_engine.get_container_status(&req.container_id).await {
                    // First try to get cached metrics (within last 30 seconds)
                    let use_cached = if let Ok(Some(latest_metrics)) = self.sync_engine.get_latest_metrics(&req.container_id).await {
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
                        let metrics_age = now - latest_metrics.timestamp;
                        if metrics_age <= 30 {  // Use cached if less than 30 seconds old
                            // Use cached metrics
                            container_metrics.push(ContainerMetric {
                                container_id: latest_metrics.container_id.clone(),
                                timestamp: latest_metrics.timestamp,
                                cpu_usage_usec: latest_metrics.cpu.usage_usec,
                                cpu_user_usec: latest_metrics.cpu.user_usec,
                                cpu_system_usec: latest_metrics.cpu.system_usec,
                                cpu_throttled_usec: latest_metrics.cpu.throttled_usec,
                                memory_current_bytes: latest_metrics.memory.current_bytes,
                                memory_peak_bytes: latest_metrics.memory.peak_bytes,
                                memory_limit_bytes: latest_metrics.memory.limit_bytes,
                                memory_cache_bytes: latest_metrics.memory.cache_bytes,
                                memory_rss_bytes: latest_metrics.memory.rss_bytes,
                                network_rx_bytes: latest_metrics.network.rx_bytes,
                                network_tx_bytes: latest_metrics.network.tx_bytes,
                                network_rx_packets: latest_metrics.network.rx_packets,
                                network_tx_packets: latest_metrics.network.tx_packets,
                                disk_read_bytes: latest_metrics.disk.read_bytes,
                                disk_write_bytes: latest_metrics.disk.write_bytes,
                            });
                            true
                        } else {
                            false  // Cached metrics too old
                        }
                    } else {
                        false  // No cached metrics
                    };
                    
                    // If no cached metrics or they're stale, collect fresh metrics
                    if !use_cached {
                        let collector = MetricsCollector::new();
                        
                        // Also get stats from runtime for additional details
                        use crate::daemon::runtime::ContainerRuntime;
                        let runtime = ContainerRuntime::new();
                        let _runtime_stats = runtime.get_container_stats(&req.container_id).unwrap_or_default();
                        
                        if let Ok(metrics) = collector.collect_container_metrics(&req.container_id, status.pid.map(|p| p as i32)) {
                        container_metrics.push(ContainerMetric {
                                container_id: metrics.container_id.clone(),
                                timestamp: metrics.timestamp,
                                cpu_usage_usec: metrics.cpu.usage_usec,
                                cpu_user_usec: metrics.cpu.user_usec,
                                cpu_system_usec: metrics.cpu.system_usec,
                                cpu_throttled_usec: metrics.cpu.throttled_usec,
                                memory_current_bytes: metrics.memory.current_bytes,
                                memory_peak_bytes: metrics.memory.peak_bytes,
                                memory_limit_bytes: metrics.memory.limit_bytes,
                                memory_cache_bytes: metrics.memory.cache_bytes,
                                memory_rss_bytes: metrics.memory.rss_bytes,
                                network_rx_bytes: metrics.network.rx_bytes,
                                network_tx_bytes: metrics.network.tx_bytes,
                                network_rx_packets: metrics.network.rx_packets,
                                network_tx_packets: metrics.network.tx_packets,
                                disk_read_bytes: metrics.disk.read_bytes,
                                disk_write_bytes: metrics.disk.write_bytes,
                            });
                        
                            // Store metrics in database for history
                            let _ = self.sync_engine.store_metrics(&metrics).await;
                        }
                    }
                }
            }
        } else {
            // Get metrics for all running containers
            if let Ok(containers) = self.sync_engine.list_containers(Some(ContainerState::Running)).await {
                let collector = MetricsCollector::new();
                for container in containers {
                    if let Ok(metrics) = collector.collect_container_metrics(&container.id, container.pid.map(|p| p as i32)) {
                        container_metrics.push(ContainerMetric {
                            container_id: metrics.container_id.clone(),
                            timestamp: metrics.timestamp,
                            cpu_usage_usec: metrics.cpu.usage_usec,
                            cpu_user_usec: metrics.cpu.user_usec,
                            cpu_system_usec: metrics.cpu.system_usec,
                            cpu_throttled_usec: metrics.cpu.throttled_usec,
                            memory_current_bytes: metrics.memory.current_bytes,
                            memory_peak_bytes: metrics.memory.peak_bytes,
                            memory_limit_bytes: metrics.memory.limit_bytes,
                            memory_cache_bytes: metrics.memory.cache_bytes,
                            memory_rss_bytes: metrics.memory.rss_bytes,
                            network_rx_bytes: metrics.network.rx_bytes,
                            network_tx_bytes: metrics.network.tx_bytes,
                            network_rx_packets: metrics.network.rx_packets,
                            network_tx_packets: metrics.network.tx_packets,
                            disk_read_bytes: metrics.disk.read_bytes,
                            disk_write_bytes: metrics.disk.write_bytes,
                        });
                    
                    // Store metrics in database for history
                    let _ = self.sync_engine.store_metrics(&metrics).await;
                    }
                }
            }
        }
        
        // Get system metrics if requested
        let system_metrics = if req.include_system {
            if let Ok(mut sys_metrics) = SystemMetrics::collect() {
                // Update container counts
                if let Ok((total, running)) = self.sync_engine.get_container_counts().await {
                    sys_metrics.containers_total = total as u64;
                    sys_metrics.containers_running = running as u64;
                    sys_metrics.containers_stopped = (total - running) as u64;
                }
                
                Some(ProtoSystemMetrics {
                    timestamp: sys_metrics.timestamp,
                    memory_used_mb: sys_metrics.memory_used_mb,
                    memory_total_mb: sys_metrics.memory_total_mb,
                    cpu_count: sys_metrics.cpu_count as u32,
                    load_average: sys_metrics.load_average.to_vec(),
                    containers_total: sys_metrics.containers_total,
                    containers_running: sys_metrics.containers_running,
                    containers_stopped: sys_metrics.containers_stopped,
                })
            } else {
                None
            }
        } else {
            None
        };
        
        Ok(Response::new(GetMetricsResponse {
            container_metrics,
            system_metrics,
        }))
    }

    async fn get_system_info(
        &self,
        _request: Request<GetSystemInfoRequest>,
    ) -> Result<Response<GetSystemInfoResponse>, Status> {
        let mut features = HashMap::new();
        features.insert("namespaces".to_string(), "pid,mount,uts,ipc,network".to_string());
        features.insert("cgroups".to_string(), "v1,v2".to_string());
        features.insert("storage".to_string(), "sqlite".to_string());
        features.insert("networking".to_string(), "bridge,veth".to_string());
        features.insert("volumes".to_string(), "bind,volume,tmpfs".to_string());
        
        let mut limits = HashMap::new();
        limits.insert("max_containers".to_string(), "1000".to_string());
        limits.insert("max_memory_per_container".to_string(), "unlimited".to_string());
        limits.insert("max_cpus_per_container".to_string(), "unlimited".to_string());
        
        // Add SyncEngineStats for comprehensive system information
        let mut stats_features = features.clone();
        if let Ok(engine_stats) = self.sync_engine.get_stats().await {
            stats_features.insert("active_containers".to_string(), engine_stats.total_containers.to_string());
            stats_features.insert("running_containers".to_string(), engine_stats.running_containers.to_string());
            stats_features.insert("active_networks".to_string(), engine_stats.active_networks.to_string());
            stats_features.insert("active_monitors".to_string(), engine_stats.active_monitors.to_string());
            
            // Get detailed runtime stats for running containers
            if let Ok(containers) = self.sync_engine.list_containers(Some(crate::sync::ContainerState::Running)).await {
                let mut total_memory = 0i64;
                let mut containers_with_stats = 0;
                
                for container in containers {
                    // Use the combined method to get both container info and stats
                    let (container_info, stats_result) = self.runtime.get_container_info_and_stats(&container.id);
                    
                    if let Ok(stats) = stats_result {
                        if let Some(memory_str) = stats.get("memory_usage_bytes") {
                            total_memory += memory_str.parse::<i64>().unwrap_or(0);
                            containers_with_stats += 1;
                        }
                        
                        // Add additional info if container details are available
                        if let Some(info) = container_info {
                            if let Some(pid) = info.pid {
                                // Could add PID-based stats here if needed
                                use crate::utils::process::ProcessUtils;
                                let pid_i32 = ProcessUtils::pid_to_i32(pid);
                                stats_features.insert(format!("container_{}_pid", container.id), pid_i32.to_string());
                            }
                        }
                    }
                }
                
                if containers_with_stats > 0 {
                    stats_features.insert("total_memory_usage_bytes".to_string(), total_memory.to_string());
                    stats_features.insert("containers_with_runtime_stats".to_string(), containers_with_stats.to_string());
                }
            }
        }
        
        Ok(Response::new(GetSystemInfoResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            runtime: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            start_time: self.start_time.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
            features: stats_features,
            limits,
        }))
    }

    async fn stream_events(
        &self,
        request: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        use tokio_stream::wrappers::IntervalStream;
        use futures::stream::StreamExt;
        
        let req = request.into_inner();
        let event_buffer = sync::events::global_event_buffer();
        
        // Parse event type filters
        let event_types: Option<Vec<sync::events::EventType>> = if req.event_types.is_empty() {
            None
        } else {
            let types: Vec<_> = req.event_types.iter()
                .filter_map(|s| sync::events::EventType::from_str(s))
                .collect();
            if types.is_empty() {
                None
            } else {
                Some(types)
            }
        };
        
        // Create a stream that polls for new events every 100ms
        let stream = IntervalStream::new(tokio::time::interval(Duration::from_millis(100)))
            .map(move |_| {
                let events = event_buffer.get_filtered(
                    if req.container_ids.is_empty() { None } else { Some(&req.container_ids) },
                    event_types.as_deref(),
                    None,
                );
                
                // Convert to proto events
                let proto_events: Vec<ProtoContainerEvent> = events.into_iter()
                    .map(|e| ProtoContainerEvent {
                        event_type: e.event_type.as_str().to_string(),
                        container_id: e.container_id,
                        timestamp: e.timestamp,
                        attributes: e.attributes,
                    })
                    .collect();
                
                futures::stream::iter(proto_events.into_iter().map(Ok))
            })
            .flatten();
        
        Ok(Response::new(Box::pin(stream)))
    }
    
    type StreamEventsStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<ProtoContainerEvent, Status>> + Send>>;

    // Container monitoring endpoints
    async fn list_active_monitors(
        &self,
        _request: Request<quilt::ListActiveMonitorsRequest>,
    ) -> Result<Response<quilt::ListActiveMonitorsResponse>, Status> {
        match self.sync_engine.list_active_monitors().await {
            Ok(monitors) => {
                let proto_monitors = monitors.into_iter().map(|m| quilt::ProcessMonitor {
                    container_id: m.container_id,
                    pid: m.pid,
                    status: m.status.to_string(),
                    started_at: m.monitor_started_at as u64,
                    last_check: m.last_check_at.unwrap_or(0) as u64,
                    check_count: 0, // TODO: Add check count to database schema
                    error_message: String::new(),
                }).collect();

                Ok(Response::new(quilt::ListActiveMonitorsResponse {
                    monitors: proto_monitors,
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => Ok(Response::new(quilt::ListActiveMonitorsResponse {
                monitors: vec![],
                success: false,
                error_message: e.to_string(),
            }))
        }
    }

    async fn get_monitor_status(
        &self,
        request: Request<quilt::GetMonitorStatusRequest>,
    ) -> Result<Response<quilt::GetMonitorStatusResponse>, Status> {
        let req = request.into_inner();
        
        match self.sync_engine.get_monitor_status(&req.container_id).await {
            Ok(monitor) => {
                let proto_monitor = quilt::ProcessMonitor {
                    container_id: monitor.container_id,
                    pid: monitor.pid,
                    status: monitor.status.to_string(),
                    started_at: monitor.monitor_started_at as u64,
                    last_check: monitor.last_check_at.unwrap_or(0) as u64,
                    check_count: 0, // TODO: Add check count to database schema
                    error_message: String::new(),
                };

                Ok(Response::new(quilt::GetMonitorStatusResponse {
                    monitor: Some(proto_monitor),
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => Ok(Response::new(quilt::GetMonitorStatusResponse {
                monitor: None,
                success: false,
                error_message: e.to_string(),
            }))
        }
    }

    async fn list_monitoring_processes(
        &self,
        _request: Request<quilt::ListMonitoringProcessesRequest>,
    ) -> Result<Response<quilt::ListMonitoringProcessesResponse>, Status> {
        // Same as list_active_monitors for now - could be different in the future
        match self.sync_engine.list_active_monitors().await {
            Ok(monitors) => {
                let proto_monitors = monitors.into_iter().map(|m| quilt::ProcessMonitor {
                    container_id: m.container_id,
                    pid: m.pid,
                    status: m.status.to_string(),
                    started_at: m.monitor_started_at as u64,
                    last_check: m.last_check_at.unwrap_or(0) as u64,
                    check_count: 0, // TODO: Add check count to database schema
                    error_message: String::new(),
                }).collect();

                Ok(Response::new(quilt::ListMonitoringProcessesResponse {
                    processes: proto_monitors,
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => Ok(Response::new(quilt::ListMonitoringProcessesResponse {
                processes: vec![],
                success: false,
                error_message: e.to_string(),
            }))
        }
    }

    // Cleanup operation endpoints
    async fn get_cleanup_status(
        &self,
        request: Request<quilt::GetCleanupStatusRequest>,
    ) -> Result<Response<quilt::GetCleanupStatusResponse>, Status> {
        let req = request.into_inner();
        let container_filter = if req.container_id.is_empty() { None } else { Some(req.container_id.as_str()) };

        match self.sync_engine.cleanup_service.get_cleanup_tasks(container_filter).await {
            Ok(tasks) => {
                let proto_tasks = tasks.into_iter().map(|t| quilt::CleanupTask {
                    task_id: t.id,
                    container_id: t.container_id,
                    resource_type: t.resource_type.to_string(),
                    resource_path: t.resource_path,
                    status: t.status.to_string(),
                    created_at: t.created_at as u64,
                    completed_at: t.completed_at.unwrap_or(0) as u64,
                    error_message: t.error_message.unwrap_or_default(),
                }).collect();

                Ok(Response::new(quilt::GetCleanupStatusResponse {
                    tasks: proto_tasks,
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => Ok(Response::new(quilt::GetCleanupStatusResponse {
                tasks: vec![],
                success: false,
                error_message: e.to_string(),
            }))
        }
    }

    async fn list_cleanup_tasks(
        &self,
        request: Request<quilt::ListCleanupTasksRequest>,
    ) -> Result<Response<quilt::ListCleanupTasksResponse>, Status> {
        let req = request.into_inner();
        
        // List cleanup tasks always gets all tasks - no container_id filtering
        let tasks_result = self.sync_engine.cleanup_service.get_cleanup_tasks(None).await;
        
        match tasks_result {
            Ok(tasks) => {
                let proto_tasks = tasks.into_iter().map(|t| quilt::CleanupTask {
                    task_id: t.id,
                    container_id: t.container_id,
                    resource_type: t.resource_type.to_string(),
                    resource_path: t.resource_path,
                    status: t.status.to_string(),
                    created_at: t.created_at as u64,
                    completed_at: t.completed_at.unwrap_or(0) as u64,
                    error_message: t.error_message.unwrap_or_default(),
                }).collect();

                Ok(Response::new(quilt::ListCleanupTasksResponse {
                    tasks: proto_tasks,
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => Ok(Response::new(quilt::ListCleanupTasksResponse {
                tasks: vec![],
                success: false,
                error_message: e.to_string(),
            }))
        }
    }

    async fn list_network_allocations(
        &self,
        _request: Request<quilt::ListNetworkAllocationsRequest>,
    ) -> Result<Response<quilt::ListNetworkAllocationsResponse>, Status> {
        match self.sync_engine.list_network_allocations().await {
            Ok(allocations) => {
                let proto_allocations = allocations.into_iter().map(|a| quilt::NetworkAllocation {
                    container_id: a.container_id,
                    ip_address: a.ip_address,
                    bridge_interface: a.bridge_interface.unwrap_or_default(),
                    veth_host: a.veth_host.unwrap_or_default(),
                    veth_container: a.veth_container.unwrap_or_default(),
                    setup_completed: a.setup_completed,
                    allocation_time: a.allocation_time,
                    status: match a.status {
                        crate::sync::network::NetworkStatus::Allocated => "allocated".to_string(),
                        crate::sync::network::NetworkStatus::Active => "active".to_string(),
                        crate::sync::network::NetworkStatus::CleanupPending => "cleanup_pending".to_string(),
                        crate::sync::network::NetworkStatus::Cleaned => "cleaned".to_string(),
                    },
                }).collect();

                Ok(Response::new(quilt::ListNetworkAllocationsResponse {
                    allocations: proto_allocations,
                }))
            }
            Err(_e) => Ok(Response::new(quilt::ListNetworkAllocationsResponse {
                allocations: vec![],
            }))
        }
    }

    async fn force_cleanup(
        &self,
        request: Request<quilt::ForceCleanupRequest>,
    ) -> Result<Response<quilt::ForceCleanupResponse>, Status> {
        let req = request.into_inner();
        let mut all_cleaned_resources = Vec::new();
        
        // Force cleanup specific container if provided
        if !req.container_id.is_empty() {
            match self.sync_engine.cleanup_service.force_cleanup(&req.container_id).await {
                Ok(cleaned_resources) => {
                    all_cleaned_resources.extend(cleaned_resources);
                }
                Err(e) => return Ok(Response::new(quilt::ForceCleanupResponse {
                    success: false,
                    error_message: format!("Container cleanup failed: {}", e),
                    cleaned_resources: vec![],
                }))
            }
        }
        
        // Run general cleanup tasks
        let mut cleanup_messages = Vec::new();
        
        // Clean up orphaned volumes
        match self.sync_engine.cleanup_orphaned_volumes().await {
            Ok(cleaned_volumes) => {
                if cleaned_volumes > 0 {
                    cleanup_messages.push(format!("Cleaned {} orphaned volumes", cleaned_volumes));
                }
            }
            Err(e) => cleanup_messages.push(format!("Volume cleanup failed: {}", e)),
        }
        
        // Clean up old metrics
        match self.sync_engine.cleanup_old_metrics(7).await {
            Ok(cleaned_metrics) => {
                if cleaned_metrics > 0 {
                    cleanup_messages.push(format!("Cleaned {} old metric records", cleaned_metrics));
                }
            }
            Err(e) => cleanup_messages.push(format!("Metrics cleanup failed: {}", e)),
        }
        
        all_cleaned_resources.extend(cleanup_messages);
        
        Ok(Response::new(quilt::ForceCleanupResponse {
            success: true,
            error_message: String::new(),
            cleaned_resources: all_cleaned_resources,
        }))
    }

    async fn get_container_network(
        &self,
        request: Request<quilt::GetContainerNetworkRequest>,
    ) -> Result<Response<quilt::GetContainerNetworkResponse>, Status> {
        let req = request.into_inner();
        
        // Use the unused runtime method to get container network config
        match self.runtime.get_container_network(&req.container_id) {
            Some(network_config) => {
                let proto_config = quilt::ContainerNetworkConfig {
                    ip_address: network_config.ip_address,
                    bridge_interface: network_config.gateway_ip,
                    veth_host: network_config.veth_host_name,
                    veth_container: network_config.veth_container_name,
                    setup_completed: true,
                    status: format!("Network configured for container {}", req.container_id),
                };
                
                Ok(Response::new(quilt::GetContainerNetworkResponse {
                    success: true,
                    error_message: String::new(),
                    network_config: Some(proto_config),
                }))
            }
            None => Ok(Response::new(quilt::GetContainerNetworkResponse {
                success: false,
                error_message: format!("No network configuration found for container {}", req.container_id),
                network_config: None,
            }))
        }
    }

    async fn set_container_network(
        &self,
        request: Request<quilt::SetContainerNetworkRequest>,
    ) -> Result<Response<quilt::SetContainerNetworkResponse>, Status> {
        let req = request.into_inner();
        
        if let Some(proto_config) = req.network_config {
            // Convert protobuf config to ContainerNetworkConfig
            let network_config = crate::icc::network::ContainerNetworkConfig {
                ip_address: proto_config.ip_address,
                subnet_mask: "255.255.0.0".to_string(),
                gateway_ip: proto_config.bridge_interface,
                container_id: req.container_id.clone(),
                veth_host_name: proto_config.veth_host,
                veth_container_name: proto_config.veth_container,
                rootfs_path: None,
            };
            
            // Use the unused runtime method to set container network config
            match self.runtime.set_container_network(&req.container_id, network_config) {
                Ok(_) => Ok(Response::new(quilt::SetContainerNetworkResponse {
                    success: true,
                    error_message: String::new(),
                })),
                Err(e) => Ok(Response::new(quilt::SetContainerNetworkResponse {
                    success: false,
                    error_message: e,
                }))
            }
        } else {
            Ok(Response::new(quilt::SetContainerNetworkResponse {
                success: false,
                error_message: "Network configuration is required".to_string(),
            }))
        }
    }

    async fn setup_container_network_post_start(
        &self,
        request: Request<quilt::SetupContainerNetworkPostStartRequest>,
    ) -> Result<Response<quilt::SetupContainerNetworkPostStartResponse>, Status> {
        let req = request.into_inner();
        
        // Use the unused runtime method to setup container network post-start
        match self.runtime.setup_container_network_post_start(&req.container_id, &self.network_manager) {
            Ok(_) => Ok(Response::new(quilt::SetupContainerNetworkPostStartResponse {
                success: true,
                error_message: String::new(),
            })),
            Err(e) => Ok(Response::new(quilt::SetupContainerNetworkPostStartResponse {
                success: false,
                error_message: e,
            }))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger
    // Logger initialization not needed - ConsoleLogger is used directly
    
    // ✅ SYNC ENGINE INITIALIZATION
    let service = QuiltServiceImpl::new().await
        .map_err(|e| format!("Failed to initialize sync engine: {}", e))?;
    
    // Bind to all interfaces so containers can access the gRPC server
    let addr: std::net::SocketAddr = "0.0.0.0:50051".parse()?;

    ConsoleLogger::server_starting(&addr.to_string());
    ConsoleLogger::success("🚀 Quilt server running with SQLite sync engine - non-blocking operations enabled");

    // ✅ GRACEFUL SHUTDOWN
    let service_clone = service.clone();
    tokio::select! {
        result = Server::builder()
            .http2_keepalive_interval(Some(Duration::from_secs(30)))
            .http2_keepalive_timeout(Some(Duration::from_secs(60)))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .add_service(QuiltServiceServer::new(service.clone()))
            .serve(addr) => {
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            ConsoleLogger::info("Received shutdown signal, cleaning up...");
            service_clone.sync_engine.close().await;
            ConsoleLogger::success("Sync engine closed gracefully");
        }
    }

    Ok(())
}

