mod daemon;
mod utils;
mod icc;

use daemon::{ContainerRuntime, ContainerConfig, ContainerState, CgroupLimits, NamespaceConfig};
use utils::console::ConsoleLogger;
use icc::network::NetworkManager;

use std::sync::Arc;
use tokio::sync::Mutex;
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
    LogEntry, ContainerStatus,
};

pub struct QuiltServiceImpl {
    runtime: Arc<ContainerRuntime>,
    network_manager: Arc<Mutex<NetworkManager>>,
}

impl QuiltServiceImpl {
    pub fn new() -> Self {
        let network_manager = match NetworkManager::new("quilt0", "10.42.0.0/16") {
            Ok(nm) => nm,
            Err(e) => {
                eprintln!("Failed to create network manager: {}", e);
                std::process::exit(1);
            }
        };
        
        // Initialize bridge in background - don't fail startup if this fails
        if let Err(e) = network_manager.ensure_bridge_ready() {
            eprintln!("Warning: Failed to initialize bridge during startup: {}", e);
            eprintln!("Bridge will be created when first container is started");
        }

        Self {
            runtime: Arc::new(ContainerRuntime::new()),
            network_manager: Arc::new(Mutex::new(network_manager)),
        }
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

        println!("Creating container {} with image: {}", container_id, req.image_path);

        // Parse resource limits
        let resource_limits = if req.memory_limit_mb > 0 || req.cpu_limit_percent > 0.0 {
            Some(CgroupLimits {
                memory_limit_bytes: if req.memory_limit_mb > 0 {
                    Some((req.memory_limit_mb as u64) * 1024 * 1024)
                } else {
                    None
                },
                cpu_quota: if req.cpu_limit_percent > 0.0 {
                    // Convert percentage to quota (100000 microseconds = 100% CPU)
                    Some((req.cpu_limit_percent * 1000.0) as i64)
                } else {
                    None
                },
                cpu_period: Some(100000), // 100ms period
                cpu_shares: Some(1024),   // Default
                pids_limit: Some(1024),   // Default
            })
        } else {
            Some(CgroupLimits::default())
        };

        // Parse namespace configuration
        let namespace_config = Some(NamespaceConfig {
            pid: req.enable_pid_namespace,
            mount: req.enable_mount_namespace,
            uts: req.enable_uts_namespace,
            ipc: req.enable_ipc_namespace,
            network: req.enable_network_namespace,
        });

        // Create container configuration
        let config = ContainerConfig {
            image_path: req.image_path,
            command: req.command,
            environment: req.environment,
            setup_commands: req.setup_commands,
            resource_limits,
            namespace_config,
            working_directory: if req.working_directory.is_empty() {
                None
            } else {
                Some(req.working_directory)
            },
        };

        let runtime = Arc::clone(&self.runtime);
        match runtime.create_container(container_id.clone(), config) {
            Ok(()) => {
                ConsoleLogger::container_created(&container_id);
                
                // 1. Allocate network configuration for the container
                let network_config = {
                    let network_manager = self.network_manager.lock().await;
                    match network_manager.allocate_container_network(&container_id) {
                        Ok(config) => config,
                        Err(e) => {
                            ConsoleLogger::error(&format!("Failed to allocate network for container: {}", e));
                            return Err(Status::internal(format!("Failed to allocate network: {}", e)));
                        }
                    }
                }; // Lock is released here

                // 2. Store network configuration in container
                if let Err(e) = runtime.set_container_network(&container_id, network_config.clone()) {
                    ConsoleLogger::error(&format!("Failed to store network config: {}", e));
                    return Err(Status::internal(format!("Failed to store network config: {}", e)));
                }

                // 3. Start container with network configuration passed for ResourceManager tracking
                match runtime.start_container(&container_id, Some(network_config.clone())) {
                    Ok(()) => {
                        // 4. Set up the container's network interface now that it's running
                        // Use a fresh lock for network setup to ensure proper context
                        let network_manager = self.network_manager.lock().await;
                        if let Err(e) = runtime.setup_container_network_post_start(&container_id, &*network_manager) {
                            ConsoleLogger::error(&format!("Failed to configure container network: {}", e));
                            return Err(Status::internal(format!("Failed to configure container network: {}", e)));
                        }

                        ConsoleLogger::container_started(&container_id, None);
                    }
                    Err(e) => {
                        ConsoleLogger::container_failed(&container_id, &e);
                        return Err(Status::internal(format!("Failed to start container: {}", e)));
                    }
                }

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
                    error_message: e,
                }))
            }
        }
    }

    async fn get_container_status(
        &self,
        request: Request<GetContainerStatusRequest>,
    ) -> Result<Response<GetContainerStatusResponse>, Status> {
        let req = request.into_inner();
        ConsoleLogger::debug(&format!("🔍 [GRPC] Received get_container_status request for: {}", req.container_id));
        
        // Use the deadlock-free combined method
        let (container, stats_result) = self.runtime.get_container_info_and_stats(&req.container_id);

        match container {
            Some(container) => {
                ConsoleLogger::debug(&format!("📋 [GRPC] Found container: {} with state: {:?}", req.container_id, container.state));
                
                let status = match container.state {
                    ContainerState::PENDING => ContainerStatus::Pending,
                    ContainerState::RUNNING => ContainerStatus::Running,
                    ContainerState::EXITED(_) => ContainerStatus::Exited,
                    ContainerState::FAILED(_) => ContainerStatus::Failed,
                };

                let exit_code = match container.state {
                    ContainerState::EXITED(code) => code,
                    _ => 0,
                };

                let error_message = match container.state {
                    ContainerState::FAILED(ref msg) => msg.clone(),
                    _ => String::new(),
                };

                // Use the stats we got from the combined call
                let stats = stats_result.unwrap_or_default();
                
                let ip_address = container.network_config
                    .as_ref()
                    .map(|nc| nc.ip_address.clone())
                    .unwrap_or_else(|| "No IP assigned".to_string());

                ConsoleLogger::debug(&format!("✅ [GRPC] Returning status for {}: {:?}, IP: {}", req.container_id, status, ip_address));
                
                Ok(Response::new(GetContainerStatusResponse {
                    container_id: req.container_id.clone(),
                    status: status as i32,
                    exit_code,
                    error_message,
                    pid: container.pid.map(|p| p.as_raw()).unwrap_or(0),
                    created_at: container.created_at,
                    memory_usage_bytes: stats.get("memory_usage_bytes")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    rootfs_path: container.rootfs_path,
                    ip_address,
                }))
            }
            None => {
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

        match self.runtime.get_container_logs(&req.container_id) {
            Some(logs) => {
                let log_entries: Vec<LogEntry> = logs
                    .into_iter()
                    .map(|entry| LogEntry {
                        timestamp: entry.timestamp,
                        message: entry.message,
                    })
                    .collect();

                Ok(Response::new(GetContainerLogsResponse {
                    container_id: req.container_id,
                    logs: log_entries,
                }))
            }
            None => Err(Status::not_found(format!("Container {} not found", req.container_id))),
        }
    }

    async fn stop_container(
        &self,
        request: Request<StopContainerRequest>,
    ) -> Result<Response<StopContainerResponse>, Status> {
        let req = request.into_inner();

        match self.runtime.stop_container(&req.container_id) {
            Ok(()) => {
                println!("✅ Container {} stopped successfully", req.container_id);
                Ok(Response::new(StopContainerResponse {
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                eprintln!("❌ Failed to stop container {}: {}", req.container_id, e);
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

        match self.runtime.remove_container(&req.container_id) {
            Ok(()) => {
                println!("✅ Container {} removed successfully", req.container_id);
                Ok(Response::new(RemoveContainerResponse {
                    success: true,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                eprintln!("❌ Failed to remove container {}: {}", req.container_id, e);
                Ok(Response::new(RemoveContainerResponse {
                    success: false,
                    error_message: e,
                }))
            }
        }
    }

    async fn exec_container(
        &self,
        request: Request<ExecContainerRequest>,
    ) -> Result<Response<ExecContainerResponse>, Status> {
        let req = request.into_inner();
        ConsoleLogger::debug(&format!("🔍 [GRPC] Received exec_container request for: {} with command: {:?}", req.container_id, req.command));
        
        match self.runtime.exec_container(
            &req.container_id,
            req.command.clone(),
            if req.working_directory.is_empty() { None } else { Some(req.working_directory.clone()) },
            req.environment.clone(),
            req.capture_output,
        ) {
            Ok((exit_code, stdout, stderr)) => {
                ConsoleLogger::debug(&format!("✅ [GRPC] Executed command in container {} with exit code: {}", req.container_id, exit_code));
                if !stdout.is_empty() {
                    ConsoleLogger::debug(&format!("📤 [GRPC] Command stdout: {}", stdout.trim()));
                }
                if !stderr.is_empty() {
                    ConsoleLogger::debug(&format!("📤 [GRPC] Command stderr: {}", stderr.trim()));
                }
                
                Ok(Response::new(ExecContainerResponse {
                    success: exit_code == 0,
                    exit_code,
                    stdout,
                    stderr,
                    error_message: String::new(),
                }))
            }
            Err(e) => {
                ConsoleLogger::error(&format!("❌ [GRPC] Failed to execute command in container {}: {}", req.container_id, e));
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = QuiltServiceImpl::new();
    let addr: std::net::SocketAddr = "127.0.0.1:50051".parse()?;

    ConsoleLogger::server_starting(&addr.to_string());

    Server::builder()
        .add_service(QuiltServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
