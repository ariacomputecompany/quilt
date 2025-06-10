mod daemon;
mod utils;
mod icc;
mod sync;

use daemon::{ContainerConfig, CgroupLimits, NamespaceConfig};
use utils::console::ConsoleLogger;
use sync::{SyncEngine, containers::ContainerState};

use std::sync::Arc;
use std::collections::HashMap;
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
            name: None, // gRPC request doesn't have name field
            image_path: req.image_path,
            command: if req.command.is_empty() { 
                "sleep infinity".to_string() // Default for long-running agents
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
                
                // Start the actual container process in background
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
        ConsoleLogger::debug(&format!("🔍 [GRPC] Status request for: {}", req.container_id));
        
        // ✅ ALWAYS FAST: Direct database query, never blocks
        match self.sync_engine.get_container_status(&req.container_id).await {
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

        // TODO: Implement structured logging in sync engine
        // For now, return empty logs since we're focusing on the core sync functionality
        Ok(Response::new(GetContainerLogsResponse {
            container_id: req.container_id,
            logs: vec![],
        }))
    }

    async fn stop_container(
        &self,
        request: Request<StopContainerRequest>,
    ) -> Result<Response<StopContainerResponse>, Status> {
        let req = request.into_inner();

        // ✅ NON-BLOCKING: Stop monitoring and update state
        match self.sync_engine.stop_monitoring(&req.container_id).await {
            Ok(()) => {
                // Update container state to trigger cleanup
                if let Err(e) = self.sync_engine.update_container_state(&req.container_id, ContainerState::Exited).await {
                    ConsoleLogger::warning(&format!("Failed to update container state: {}", e));
                }
                
                ConsoleLogger::success(&format!("Container {} stopped", req.container_id));
                Ok(Response::new(StopContainerResponse {
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                ConsoleLogger::error(&format!("Failed to stop container {}: {}", req.container_id, e));
                Ok(Response::new(StopContainerResponse {
                    success: false,
                    error_message: e.to_string(),
                }))
            }
        }
    }

    async fn remove_container(
        &self,
        request: Request<RemoveContainerRequest>,
    ) -> Result<Response<RemoveContainerResponse>, Status> {
        let req = request.into_inner();

        // ✅ NON-BLOCKING: Coordinated cleanup through sync engine
        match self.sync_engine.delete_container(&req.container_id).await {
            Ok(()) => {
                ConsoleLogger::success(&format!("Container {} removed", req.container_id));
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
        ConsoleLogger::debug(&format!("🔍 [GRPC] Exec request for: {} with command: {:?}", req.container_id, req.command));
        
        // Get container status to check if it's running and get PID
        match self.sync_engine.get_container_status(&req.container_id).await {
            Ok(status) => {
                if status.state != ContainerState::Running {
                    return Ok(Response::new(ExecContainerResponse {
                        success: false,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: String::new(),
                        error_message: format!("Container {} is not running (state: {:?})", req.container_id, status.state),
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

                // Execute command using nsenter (direct execution, not through old runtime)
                let command_str = req.command.join(" ");
                let exec_cmd = if req.capture_output {
                    format!("nsenter -t {} -p -m -n -u -i -- /bin/sh -c '{}'", pid, command_str)
                } else {
                    format!("nsenter -t {} -p -m -n -u -i -- /bin/sh -c '{}' >/dev/null 2>&1", pid, command_str)
                };

                match utils::command::CommandExecutor::execute_shell(&exec_cmd) {
                    Ok(result) => {
                        ConsoleLogger::debug(&format!("✅ [GRPC] Exec completed with exit code: {}", result.exit_code.unwrap_or(-1)));
                        
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
}

// ✅ BACKGROUND CONTAINER PROCESS STARTUP
async fn start_container_process(sync_engine: &SyncEngine, container_id: &str) -> Result<(), String> {
    use daemon::runtime::ContainerRuntime;
    
    // Get container configuration from sync engine
    let _status = sync_engine.get_container_status(container_id).await
        .map_err(|e| format!("Failed to get container config: {}", e))?;

    // Get full container config from database to get image_path and command
    let container_record = sqlx::query("SELECT image_path, command FROM containers WHERE id = ?")
        .bind(container_id)
        .fetch_one(sync_engine.pool())
        .await
        .map_err(|e| format!("Failed to get container details: {}", e))?;
    
    let image_path: String = container_record.get("image_path");
    let command: String = container_record.get("command");

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
    };

    // Create legacy runtime for actual process management (temporary)
    let runtime = ContainerRuntime::new();
    
    // Update state to Starting
    sync_engine.update_container_state(container_id, ContainerState::Starting).await
        .map_err(|e| format!("Failed to update state: {}", e))?;

    // Create container in legacy runtime
    runtime.create_container(container_id.to_string(), legacy_config)
        .map_err(|e| format!("Failed to create legacy container: {}", e))?;

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
    let service = Arc::new(QuiltServiceImpl::new().await
        .map_err(|e| format!("Failed to initialize sync engine: {}", e))?);
    
    let addr: std::net::SocketAddr = "127.0.0.1:50051".parse()?;

    ConsoleLogger::server_starting(&addr.to_string());
    ConsoleLogger::success("🚀 Quilt server running with SQLite sync engine - non-blocking operations enabled");

    // ✅ GRACEFUL SHUTDOWN
    let service_clone = service.clone();
    tokio::select! {
        result = Server::builder()
            .add_service(QuiltServiceServer::new((*service).clone()))
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
