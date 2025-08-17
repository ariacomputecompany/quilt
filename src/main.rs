mod daemon;
mod utils;
mod icc;
mod sync;

use daemon::{ContainerConfig, CgroupLimits, NamespaceConfig};
use utils::console::ConsoleLogger;
use sync::{SyncEngine, containers::ContainerState, MountType};

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
    ContainerStatus,
};

#[derive(Clone)]
pub struct QuiltServiceImpl {
    sync_engine: Arc<SyncEngine>,
}

impl QuiltServiceImpl {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        // Initialize sync engine with database
        let sync_engine = Arc::new(SyncEngine::new("quilt.db").await?);
        
        // Start background services for monitoring and cleanup
        sync_engine.start_background_services().await?;
        
        ConsoleLogger::success("Sync engine initialized with background services");
        
        Ok(Self {
            sync_engine,
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
                        if let Ok(None) = self.sync_engine.get_volume(&mount.source).await {
                            ConsoleLogger::info(&format!("Auto-creating volume '{}'", mount.source));
                            if let Err(e) = self.sync_engine.create_volume(
                                &mount.source,
                                None,
                                HashMap::new(),
                                HashMap::new(),
                            ).await {
                                ConsoleLogger::warning(&format!("Failed to auto-create volume '{}': {}", mount.source, e));
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
                let container_id_clone = container_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = start_container_process(&sync_engine, &container_id_clone).await {
                        ConsoleLogger::error(&format!("Failed to start container process {}: {}", container_id_clone, e));
                        let _ = sync_engine.update_container_state(&container_id_clone, ContainerState::Error).await;
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
                ConsoleLogger::success(&format!("Container {} removed", container_id));
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
                            let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
                            let copy_cmd = format!(
                                "nsenter -t {} -p -m -n -u -i -- chroot {} /bin/sh -c 'cat > {} << 'EOF'\n{}\nEOF\nchmod +x {}'",
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
                // Get the rootfs path for the container
                let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
                
                let exec_cmd = if req.capture_output {
                    format!("nsenter -t {} -p -m -n -u -i -- chroot {} /bin/sh -c '{}'", pid, rootfs_path, command_to_execute)
                } else {
                    format!("nsenter -t {} -p -m -n -u -i -- chroot {} /bin/sh -c '{}' >/dev/null 2>&1", pid, rootfs_path, command_to_execute)
                };

                match utils::command::CommandExecutor::execute_shell(&exec_cmd) {
                    Ok(result) => {
                        ConsoleLogger::debug(&format!("✅ [GRPC] Exec completed with exit code: {}", result.exit_code.unwrap_or(-1)));
                        
                        // Clean up temporary script if we created one
                        if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                            let cleanup_cmd = format!(
                                "nsenter -t {} -p -m -n -u -i -- chroot {} rm -f {}",
                                pid, rootfs_path, command_to_execute
                            );
                            let _ = utils::command::CommandExecutor::execute_shell(&cleanup_cmd);
                        }
                        
                        Ok(Response::new(ExecContainerResponse {
                            success: result.success,
                            exit_code: result.exit_code.unwrap_or(-1),
                            stdout: result.stdout,
                            stderr: result.stderr,
                            error_message: String::new(),
                        }))
                    }
                    Err(e) => {
                        ConsoleLogger::error(&format!("❌ [GRPC] Exec failed: {}", e));
                        
                        // Clean up temporary script on error too
                        if req.copy_script && command_to_execute.starts_with("/tmp/quilt_exec_") {
                            let cleanup_cmd = format!(
                                "nsenter -t {} -p -m -n -u -i -- chroot {} rm -f {}",
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
        let container_id_clone = container_id.clone();
        tokio::spawn(async move {
            if let Err(e) = start_container_process(&sync_engine, &container_id_clone).await {
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
}

// ✅ BACKGROUND CONTAINER PROCESS STARTUP
async fn start_container_process(sync_engine: &SyncEngine, container_id: &str) -> Result<(), String> {
    use daemon::runtime::ContainerRuntime;
    use std::path::Path;
    
    // Get container configuration from sync engine
    let status = sync_engine.get_container_status(container_id).await
        .map_err(|e| format!("Failed to get container config: {}", e))?;

    // Get full container config from database to get image_path and command
    let container_record = sqlx::query("SELECT image_path, command, rootfs_path FROM containers WHERE id = ?")
        .bind(container_id)
        .fetch_one(sync_engine.pool())
        .await
        .map_err(|e| format!("Failed to get container details: {}", e))?;
    
    let image_path: String = container_record.get("image_path");
    let command: String = container_record.get("command");
    let rootfs_path: Option<String> = container_record.get("rootfs_path");

    // Get mounts for the container
    let sync_mounts = sync_engine.get_container_mounts(container_id).await
        .map_err(|e| format!("Failed to get mounts: {}", e))?;
    
    // Convert mounts from sync engine to daemon format
    let mut daemon_mounts: Vec<daemon::MountConfig> = Vec::new();
    for m in sync_mounts {
        let source = match m.mount_type {
            sync::MountType::Volume => {
                // For volumes, convert volume name to actual path
                sync_engine.get_volume_path(&m.source).to_string_lossy().to_string()
            }
            _ => m.source,
        };
        
        daemon_mounts.push(daemon::MountConfig {
            source,
            target: m.target,
            mount_type: match m.mount_type {
                sync::MountType::Bind => daemon::MountType::Bind,
                sync::MountType::Volume => daemon::MountType::Volume,
                sync::MountType::Tmpfs => daemon::MountType::Tmpfs,
            },
            readonly: m.readonly,
            options: m.options,
        });
    }
    
    // Convert sync engine config back to legacy format for actual container startup
    // TODO: Eventually replace this with native sync engine container startup
    let legacy_config = ContainerConfig {
        image_path,
        command: vec!["/bin/sh".to_string(), "-c".to_string(), command],
        environment: HashMap::new(), // TODO: Get from sync engine
        setup_commands: vec![],
        resource_limits: Some(CgroupLimits::default()),
        namespace_config: Some(NamespaceConfig::default()),
        working_directory: None,
        mounts: daemon_mounts,
    };

    // Create legacy runtime for actual process management (temporary)
    let runtime = ContainerRuntime::new();
    
    // Update state to Starting
    sync_engine.update_container_state(container_id, ContainerState::Starting).await
        .map_err(|e| format!("Failed to update state: {}", e))?;

    // Check if this is a restart (container already has rootfs)
    let needs_creation = if let Some(ref rootfs) = rootfs_path {
        !Path::new(rootfs).exists()
    } else {
        true
    };

    if needs_creation {
        // First time starting - create container in legacy runtime
        ConsoleLogger::debug(&format!("Creating new container runtime for {}", container_id));
        runtime.create_container(container_id.to_string(), legacy_config)
            .map_err(|e| format!("Failed to create legacy container: {}", e))?;
            
        // Save the rootfs path back to sync engine
        if let Some(container) = runtime.get_container_info(container_id) {
            sync_engine.set_rootfs_path(container_id, &container.rootfs_path).await
                .map_err(|e| format!("Failed to save rootfs path: {}", e))?;
        }
    } else {
        // Restarting existing container - just add to runtime registry without recreating rootfs
        ConsoleLogger::debug(&format!("Restarting existing container {}", container_id));
        
        // Add container to runtime's registry without creating rootfs
        // We'll implement a new method for this
        runtime.register_existing_container(container_id.to_string(), legacy_config, rootfs_path.unwrap())
            .map_err(|e| format!("Failed to register existing container: {}", e))?;
    }

    // Start the container
    match runtime.start_container(container_id, None) {
        Ok(()) => {
            // Get the PID from legacy runtime and store in sync engine
            if let Some(container) = runtime.get_container_info(container_id) {
                if let Some(pid) = container.pid {
                    sync_engine.set_container_pid(container_id, pid).await
                        .map_err(|e| format!("Failed to set PID: {}", e))?;
                }
                
                // Update state to Running
                sync_engine.update_container_state(container_id, ContainerState::Running).await
                    .map_err(|e| format!("Failed to update to running: {}", e))?;
            }
            
            ConsoleLogger::success(&format!("Container {} started successfully", container_id));
            Ok(())
        }
        Err(e) => {
            sync_engine.update_container_state(container_id, ContainerState::Error).await.ok();
            Err(format!("Failed to start container: {}", e))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ✅ SYNC ENGINE INITIALIZATION
    let service = QuiltServiceImpl::new().await
        .map_err(|e| format!("Failed to initialize sync engine: {}", e))?;
    
    let addr: std::net::SocketAddr = "127.0.0.1:50051".parse()?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex as TokioMutex;
    use std::sync::Arc;
    
    // Mock sync engine for testing
    struct MockSyncEngine {
        containers: Arc<TokioMutex<HashMap<String, ContainerState>>>,
        names: Arc<TokioMutex<HashMap<String, String>>>, // name -> id
    }
    
    impl MockSyncEngine {
        fn new() -> Self {
            Self {
                containers: Arc::new(TokioMutex::new(HashMap::new())),
                names: Arc::new(TokioMutex::new(HashMap::new())),
            }
        }
        
        async fn create_container(&self, config: sync::containers::ContainerConfig) -> Result<(), String> {
            let mut containers = self.containers.lock().await;
            let mut names = self.names.lock().await;
            
            // Check name uniqueness
            if let Some(ref name) = config.name {
                if !name.is_empty() && names.contains_key(name) {
                    return Err(format!("Container with name '{}' already exists", name));
                }
            }
            
            containers.insert(config.id.clone(), ContainerState::Created);
            
            if let Some(ref name) = config.name {
                if !name.is_empty() {
                    names.insert(name.clone(), config.id.clone());
                }
            }
            
            Ok(())
        }
        
        async fn get_container_by_name(&self, name: &str) -> Result<String, String> {
            let names = self.names.lock().await;
            names.get(name)
                .cloned()
                .ok_or_else(|| format!("Container with name '{}' not found", name))
        }
        
        async fn get_container_status(&self, id: &str) -> Result<ContainerState, String> {
            let containers = self.containers.lock().await;
            containers.get(id)
                .cloned()
                .ok_or_else(|| format!("Container {} not found", id))
        }
        
        async fn update_container_state(&self, id: &str, state: ContainerState) -> Result<(), String> {
            let mut containers = self.containers.lock().await;
            if let Some(container_state) = containers.get_mut(id) {
                *container_state = state;
                Ok(())
            } else {
                Err(format!("Container {} not found", id))
            }
        }
    }
    
    #[tokio::test]
    async fn test_create_container_with_name() {
        let sync_engine = Arc::new(SyncEngine::new(":memory:").await.unwrap());
        let service = QuiltServiceImpl { sync_engine };
        
        let request = tonic::Request::new(CreateContainerRequest {
            image_path: "test.tar.gz".to_string(),
            command: vec!["echo".to_string(), "test".to_string()],
            environment: HashMap::new(),
            working_directory: String::new(),
            setup_commands: vec![],
            memory_limit_mb: 0,
            cpu_limit_percent: 0.0,
            enable_pid_namespace: true,
            enable_mount_namespace: true,
            enable_uts_namespace: true,
            enable_ipc_namespace: true,
            enable_network_namespace: true,
            name: "test-container".to_string(),
            async_mode: false,
        });
        
        let response = service.create_container(request).await;
        assert!(response.is_ok());
        
        let res = response.unwrap().into_inner();
        assert!(res.success);
        assert!(!res.container_id.is_empty());
    }
    
    #[tokio::test]
    async fn test_async_container_without_command() {
        let sync_engine = Arc::new(SyncEngine::new(":memory:").await.unwrap());
        let service = QuiltServiceImpl { sync_engine };
        
        let request = tonic::Request::new(CreateContainerRequest {
            image_path: "test.tar.gz".to_string(),
            command: vec![], // Empty command
            environment: HashMap::new(),
            working_directory: String::new(),
            setup_commands: vec![],
            memory_limit_mb: 0,
            cpu_limit_percent: 0.0,
            enable_pid_namespace: true,
            enable_mount_namespace: true,
            enable_uts_namespace: true,
            enable_ipc_namespace: true,
            enable_network_namespace: true,
            name: "async-test".to_string(),
            async_mode: true, // Async mode
        });
        
        let response = service.create_container(request).await;
        assert!(response.is_ok());
        
        let res = response.unwrap().into_inner();
        assert!(res.success);
        // Verify it used default command
    }
    
    #[tokio::test]
    async fn test_non_async_without_command_fails() {
        let sync_engine = Arc::new(SyncEngine::new(":memory:").await.unwrap());
        let service = QuiltServiceImpl { sync_engine };
        
        let request = tonic::Request::new(CreateContainerRequest {
            image_path: "test.tar.gz".to_string(),
            command: vec![], // Empty command
            environment: HashMap::new(),
            working_directory: String::new(),
            setup_commands: vec![],
            memory_limit_mb: 0,
            cpu_limit_percent: 0.0,
            enable_pid_namespace: true,
            enable_mount_namespace: true,
            enable_uts_namespace: true,
            enable_ipc_namespace: true,
            enable_network_namespace: true,
            name: "fail-test".to_string(),
            async_mode: false, // Not async
        });
        
        let response = service.create_container(request).await;
        assert!(response.is_err());
        
        let err = response.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("Command required"));
    }
    
    #[tokio::test]
    async fn test_get_container_by_name_rpc() {
        let sync_engine = Arc::new(SyncEngine::new(":memory:").await.unwrap());
        let service = QuiltServiceImpl { sync_engine: sync_engine.clone() };
        
        // Create a container with name first
        let config = sync::containers::ContainerConfig {
            id: "test-id-123".to_string(),
            name: Some("lookup-test".to_string()),
            image_path: "test.tar.gz".to_string(),
            command: "echo test".to_string(),
            environment: HashMap::new(),
            memory_limit_mb: None,
            cpu_limit_percent: None,
            enable_network_namespace: true,
            enable_pid_namespace: true,
            enable_mount_namespace: true,
            enable_uts_namespace: true,
            enable_ipc_namespace: true,
        };
        
        sync_engine.create_container(config).await.unwrap();
        
        // Test the RPC
        let request = tonic::Request::new(GetContainerByNameRequest {
            name: "lookup-test".to_string(),
        });
        
        let response = service.get_container_by_name(request).await;
        assert!(response.is_ok());
        
        let res = response.unwrap().into_inner();
        assert!(res.found);
        assert_eq!(res.container_id, "test-id-123");
        assert!(res.error_message.is_empty());
    }
    
    #[tokio::test]
    async fn test_get_container_by_name_not_found() {
        let sync_engine = Arc::new(SyncEngine::new(":memory:").await.unwrap());
        let service = QuiltServiceImpl { sync_engine };
        
        let request = tonic::Request::new(GetContainerByNameRequest {
            name: "non-existent".to_string(),
        });
        
        let response = service.get_container_by_name(request).await;
        assert!(response.is_ok());
        
        let res = response.unwrap().into_inner();
        assert!(!res.found);
        assert!(res.container_id.is_empty());
        assert!(res.error_message.contains("not found"));
    }
}
