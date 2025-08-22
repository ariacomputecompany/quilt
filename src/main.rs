mod daemon;
mod utils;
mod icc;
mod sync;
mod grpc;

use daemon::{ContainerConfig, CgroupLimits, NamespaceConfig};
use utils::console::ConsoleLogger;
use sync::{SyncEngine, ContainerState, MountType};
use grpc::start_container_process;

use std::sync::Arc;
use std::collections::HashMap;
use std::time::Duration;
use tonic::{transport::Server, Request, Response, Status};
use uuid::Uuid;
use sqlx::Row;

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
    network_manager: Arc<tokio::sync::Mutex<icc::network::NetworkManager>>,
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
        
        Ok(Self {
            sync_engine,
            network_manager: Arc::new(tokio::sync::Mutex::new(network_manager)),
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
                
                // Process mounts BEFORE starting container
                for mount in req.mounts {
                    let mount_type = match mount.r#type() {
                        quilt::MountType::Bind => MountType::Bind,
                        quilt::MountType::Volume => MountType::Volume,
                        quilt::MountType::Tmpfs => MountType::Tmpfs,
                    };
                    
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

                ConsoleLogger::debug(&format!("✅ [GRPC] Status for {}: {:?}", req.container_id, grpc_status));
                
                Ok(Response::new(GetContainerStatusResponse {
                    container_id: req.container_id,
                    status: grpc_status as i32,
                    exit_code: status.exit_code.unwrap_or(0) as i32,
                    error_message: if status.state == ContainerState::Error { "Container failed".to_string() } else { String::new() },
                    pid: status.pid.unwrap_or(0) as i32,
                    created_at: status.created_at as u64,
                    memory_usage_bytes: 0, // TODO: Implement memory monitoring in sync engine
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

        // TODO: Implement structured logging in sync engine
        // For now, return empty logs since we're focusing on the core sync functionality
        Ok(Response::new(GetContainerLogsResponse {
            container_id,
            logs: vec![],
        }))
    }

    async fn stop_container(
        &self,
        request: Request<StopContainerRequest>,
    ) -> Result<Response<StopContainerResponse>, Status> {
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

        // Get container status to get PID
        match self.sync_engine.get_container_status(&container_id).await {
            Ok(status) => {
                if let Some(pid) = status.pid {
                    // Send SIGTERM to the process
                    let timeout = req.timeout_seconds as u64;
                    let timeout = if timeout > 0 { timeout } else { 10 }; // Default 10s timeout
                    
                    // Use nix to send signal
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;
                    use nix::errno::Errno;
                    
                    match kill(Pid::from_raw(pid as i32), Some(Signal::SIGTERM)) {
                        Ok(()) => {
                            ConsoleLogger::debug(&format!("Sent SIGTERM to process {}", pid));
                            
                            // Wait for process to exit gracefully
                            let mut process_exists = true;
                            let start_time = std::time::Instant::now();
                            let timeout_duration = std::time::Duration::from_secs(timeout as u64);
                            
                            while process_exists && start_time.elapsed() < timeout_duration {
                                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                
                                // Check if process still exists (signal 0 = check only)
                                match kill(Pid::from_raw(pid as i32), None) {
                                    Ok(()) => {
                                        // Process still exists
                                        ConsoleLogger::debug(&format!("Process {} still running after {:.1}s", pid, start_time.elapsed().as_secs_f32()));
                                    }
                                    Err(nix::errno::Errno::ESRCH) => {
                                        // Process no longer exists
                                        process_exists = false;
                                        ConsoleLogger::debug(&format!("Process {} terminated after {:.1}s", pid, start_time.elapsed().as_secs_f32()));
                                    }
                                    Err(e) => {
                                        ConsoleLogger::warning(&format!("Error checking process {}: {}", pid, e));
                                        break;
                                    }
                                }
                            }
                            
                            // If process still exists after timeout, force kill
                            if process_exists {
                                ConsoleLogger::warning(&format!("Process {} didn't exit after {}s, sending SIGKILL", pid, timeout));
                                if let Err(e) = kill(Pid::from_raw(pid as i32), Some(Signal::SIGKILL)) {
                                    if e != nix::errno::Errno::ESRCH {
                                        ConsoleLogger::error(&format!("Failed to SIGKILL process {}: {}", pid, e));
                                    }
                                }
                                // Wait a bit more for SIGKILL to take effect
                                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                            }
                            
                            // Stop monitoring
                            let _ = self.sync_engine.stop_monitoring(&container_id).await;
                            
                            // Update container state
                            if let Err(e) = self.sync_engine.update_container_state(&container_id, ContainerState::Exited).await {
                                ConsoleLogger::warning(&format!("Failed to update container state: {}", e));
                            }
                            
                            ConsoleLogger::success(&format!("Container {} stopped", container_id));
                            
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
                            if e == nix::errno::Errno::ESRCH {
                                // Process already dead
                                let _ = self.sync_engine.stop_monitoring(&container_id).await;
                                let _ = self.sync_engine.update_container_state(&container_id, ContainerState::Exited).await;
                                
                                Ok(Response::new(StopContainerResponse {
                                    success: true,
                                    error_message: String::new(),
                                }))
                            } else {
                                ConsoleLogger::error(&format!("Failed to stop process {}: {}", pid, e));
                                Ok(Response::new(StopContainerResponse {
                                    success: false,
                                    error_message: format!("Failed to stop process: {}", e),
                                }))
                            }
                        }
                    }
                } else {
                    // No PID, just update state
                    let _ = self.sync_engine.stop_monitoring(&container_id).await;
                    let _ = self.sync_engine.update_container_state(&container_id, ContainerState::Exited).await;
                    
                    Ok(Response::new(StopContainerResponse {
                        success: true,
                        error_message: String::new(),
                    }))
                }
            }
            Err(e) => {
                ConsoleLogger::error(&format!("Failed to get container status: {}", e));
                Ok(Response::new(StopContainerResponse {
                    success: false,
                    error_message: format!("Container not found: {}", e),
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

        // ✅ NON-BLOCKING: Coordinated cleanup through sync engine
        match self.sync_engine.delete_container(&container_id).await {
            Ok(()) => {
                // Unregister from DNS
                {
                    let nm = self.network_manager.lock().await;
                    let _ = nm.unregister_container_dns(&container_id);
                }
                
                ConsoleLogger::success(&format!("Container {} removed", container_id));
                
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
                ConsoleLogger::error(&format!("Failed to remove container {}: {}", req.container_id, e));
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
            if std::path::Path::new(script_path).exists() {
                // Copy script to container
                match self.sync_engine.get_container_status(&container_id).await {
                    Ok(status) => {
                        if let Some(rootfs_path) = status.rootfs_path {
                            let dest_path = format!("{}/tmp/script.sh", rootfs_path);
                            if let Err(e) = utils::filesystem::FileSystemUtils::copy_file(script_path, &dest_path) {
                                return Ok(Response::new(ExecContainerResponse {
                                    success: false,
                                    exit_code: -1,
                                    stdout: String::new(),
                                    stderr: String::new(),
                                    error_message: format!("Failed to copy script: {}", e),
                                }));
                            }
                            // Make script executable
                            let _ = utils::filesystem::FileSystemUtils::make_executable(&dest_path);
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
                    match std::fs::read_to_string(script_path) {
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

                match utils::command::CommandExecutor::execute_shell(&exec_cmd) {
                    Ok(result) => {
                        ConsoleLogger::debug(&format!("✅ [GRPC] Exec completed with exit code: {}", result.exit_code.unwrap_or(-1)));
                        
                        // Clean up temporary script if we created one
                        if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                            let cleanup_cmd = format!(
                                "nsenter -t {} -p -m -n -u -- chroot {} rm -f {}",
                                pid, rootfs_path, command_to_execute
                            );
                            let _ = utils::command::CommandExecutor::execute_shell(&cleanup_cmd);
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
                        ConsoleLogger::error(&format!("❌ [GRPC] Exec failed: {}", e));
                        
                        // Clean up temporary script on error too
                        if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                            let cleanup_cmd = format!(
                                "nsenter -t {} -p -m -n -u -- chroot {} rm -f {}",
                                pid, rootfs_path, command_to_execute
                            );
                            let _ = utils::command::CommandExecutor::execute_shell(&cleanup_cmd);
                        }
                        
                        Ok(Response::new(ExecContainerResponse {
                            success: false,
                            exit_code: -1,
                            stdout: String::new(),
                            stderr: String::new(),
                            error_message: e,
                        }))
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
                    use nix::sys::signal::{kill, Signal};
                    use nix::unistd::Pid;
                    
                    match kill(Pid::from_raw(pid as i32), Some(Signal::SIGKILL)) {
                        Ok(()) => {
                            ConsoleLogger::debug(&format!("Sent SIGKILL to process {}", pid));
                            
                            // Wait briefly to ensure process is dead
                            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                            
                            // Verify process is dead
                            match kill(Pid::from_raw(pid as i32), None) {
                                Err(nix::errno::Errno::ESRCH) => {
                                    ConsoleLogger::debug(&format!("Process {} confirmed dead", pid));
                                }
                                Ok(()) => {
                                    ConsoleLogger::warning(&format!("Process {} still exists after SIGKILL", pid));
                                }
                                Err(e) => {
                                    ConsoleLogger::warning(&format!("Error checking process {}: {}", pid, e));
                                }
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
                            if e == nix::errno::Errno::ESRCH {
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
        use crate::utils::logger::Timer;
        
        let mut checks = Vec::new();
        let mut overall_healthy = true;
        
        // Check database connection
        let db_timer = Timer::new("database_check");
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
            duration_ms: db_timer.elapsed_ms(),
        });
        
        // Check cgroups availability
        let cgroup_timer = Timer::new("cgroup_check");
        let cgroup_healthy = std::path::Path::new("/sys/fs/cgroup").exists();
        if !cgroup_healthy {
            overall_healthy = false;
        }
        checks.push(HealthCheck {
            name: "cgroups".to_string(),
            healthy: cgroup_healthy,
            message: if cgroup_healthy { "Available".to_string() } else { "Not available".to_string() },
            duration_ms: cgroup_timer.elapsed_ms(),
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
        
        // Get container metrics
        if !req.container_id.is_empty() {
            // Get specific container metrics
            if let Ok(status) = self.sync_engine.get_container_status(&req.container_id).await {
                let collector = MetricsCollector::new();
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
        
        Ok(Response::new(GetSystemInfoResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            runtime: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            start_time: self.start_time.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64,
            features,
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger
    utils::logger::Logger::init();
    
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

