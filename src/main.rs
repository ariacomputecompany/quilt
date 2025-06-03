mod runtime;
mod namespace;
mod cgroup;
mod runtime_manager;
mod system_runtime;

use runtime::{ContainerRuntime, ContainerConfig, ContainerState};
use crate::cgroup::CgroupLimits;
use crate::namespace::NamespaceConfig;

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
    LogEntry, ContainerStatus,
};

pub struct QuiltServiceImpl {
    runtime: Arc<Mutex<ContainerRuntime>>,
}

impl QuiltServiceImpl {
    pub fn new() -> Self {
        Self {
            runtime: Arc::new(Mutex::new(ContainerRuntime::new())),
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

        let runtime = self.runtime.lock().await;
        match runtime.create_container(container_id.clone(), config) {
            Ok(()) => {
                println!("✅ Container {} created successfully", container_id);
                
                // Automatically start the container
                match runtime.start_container(&container_id) {
                    Ok(()) => {
                        println!("✅ Container {} started successfully", container_id);
                    }
                    Err(e) => {
                        eprintln!("❌ Failed to start container {}: {}", container_id, e);
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
                eprintln!("❌ Failed to create container: {}", e);
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
        let runtime = self.runtime.lock().await;

        match runtime.get_container_info(&req.container_id) {
            Some(container) => {
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

                // Get container stats
                let stats = runtime.get_container_stats(&req.container_id)
                    .unwrap_or_default();

                Ok(Response::new(GetContainerStatusResponse {
                    container_id: req.container_id,
                    status: status as i32,
                    exit_code,
                    error_message,
                    pid: container.pid.map(|p| p.as_raw()).unwrap_or(0),
                    created_at: container.created_at,
                    memory_usage_bytes: stats.get("memory_usage_bytes")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                    rootfs_path: container.rootfs_path,
                }))
            }
            None => Err(Status::not_found(format!("Container {} not found", req.container_id))),
        }
    }

    async fn get_container_logs(
        &self,
        request: Request<GetContainerLogsRequest>,
    ) -> Result<Response<GetContainerLogsResponse>, Status> {
        let req = request.into_inner();
        let runtime = self.runtime.lock().await;

        match runtime.get_container_logs(&req.container_id) {
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
        let runtime = self.runtime.lock().await;

        match runtime.stop_container(&req.container_id) {
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
        let runtime = self.runtime.lock().await;

        match runtime.remove_container(&req.container_id) {
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Starting Quilt Container Runtime Server");
    println!("Features enabled:");
    println!("  ✅ Linux Namespaces (PID, Mount, UTS, IPC, Network)");
    println!("  ✅ Cgroup Resource Management (Memory, CPU, PIDs)");
    println!("  ✅ Dynamic Runtime Setup Commands (npm, pip, apk, etc.)");
    println!("  ✅ Container Isolation and Security");

    let service = QuiltServiceImpl::new();
    let addr = "127.0.0.1:50051".parse()?;

    println!("🌐 Quilt gRPC server listening on {}", addr);
    println!("📋 Ready to accept container creation requests...");

    Server::builder()
        .add_service(QuiltServiceServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
