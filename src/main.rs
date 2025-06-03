use tonic::{transport::Server, Request, Response, Status};
use uuid::Uuid;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use flate2::read::GzDecoder;
use tar::Archive;

pub mod quilt_rpc {
    tonic::include_proto!("quilt"); // The string specified here must match the proto package name
}

use quilt_rpc::quilt_service_server::{QuiltService, QuiltServiceServer};
use quilt_rpc::{ContainerResponse, CreateContainerRequest, ContainerStatusRequest, ContainerStatusResponse, LogRequest, LogResponse, RemoveContainerRequest, RemoveContainerResponse, StopContainerRequest, StopContainerResponse};

#[derive(Debug, Clone)]
pub struct ContainerState {
    id: String,
    image_tarball_path: String,
    rootfs_path: String,
    command: String,
    args: Vec<String>,
    env_vars: Vec<String>,
    status: String, // e.g., PENDING, RUNNING, EXITED, FAILED
    exit_code: Option<i32>,
}

#[derive(Debug)]
pub struct MyQuiltService {
    containers: Arc<Mutex<HashMap<String, ContainerState>>>,
}

impl MyQuiltService {
    fn new() -> Self {
        MyQuiltService {
            containers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[tonic::async_trait]
impl QuiltService for MyQuiltService {
    async fn create_container(
        &self,
        request: Request<CreateContainerRequest>,
    ) -> Result<Response<ContainerResponse>, Status> {
        println!("Got a create_container request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = Uuid::new_v4().to_string();

        // Define base path for container runtime data
        let base_runtime_path = PathBuf::from("./active_containers");
        let container_dir = base_runtime_path.join(&container_id);
        let rootfs_dir = container_dir.join("rootfs");

        // Create the directory for the container's rootfs
        fs::create_dir_all(&rootfs_dir).map_err(|e| {
            eprintln!("Failed to create rootfs directory {}: {}", rootfs_dir.display(), e);
            Status::internal(format!("Failed to create container directory: {}", e))
        })?;

        let image_path = Path::new(&req_inner.image_tarball_path);
        if !image_path.exists() {
            return Err(Status::invalid_argument(format!(
                "Image tarball not found at: {}",
                image_path.display()
            )));
        }

        // Open the tarball
        let tarball_file = File::open(image_path).map_err(|e| {
            eprintln!("Failed to open image tarball {}: {}", image_path.display(), e);
            Status::internal(format!("Failed to open image tarball: {}", e))
        })?;

        // Handle potential .gz compression
        if image_path.extension().map_or(false, |ext| ext == "gz") {
            let gz_decoder = GzDecoder::new(tarball_file);
            let mut archive = Archive::new(gz_decoder);
            archive.unpack(&rootfs_dir).map_err(|e| {
                eprintln!("Failed to unpack gzipped tarball to {}: {}", rootfs_dir.display(), e);
                Status::internal(format!("Failed to unpack gzipped tarball: {}", e))
            })?;
        } else {
            // Assume uncompressed tar if not .gz
            // Note: tar::Archive needs Read, so we might need BufReader if tarball_file isn't already buffered
            // For File, it should be fine.
            let mut archive = Archive::new(tarball_file);
            archive.unpack(&rootfs_dir).map_err(|e| {
                eprintln!("Failed to unpack tarball to {}: {}", rootfs_dir.display(), e);
                Status::internal(format!("Failed to unpack tarball: {}", e))
            })?;
        }

        println!("Successfully unpacked image {} to {}", image_path.display(), rootfs_dir.display());

        let container_state = ContainerState {
            id: container_id.clone(),
            image_tarball_path: req_inner.image_tarball_path,
            rootfs_path: rootfs_dir.to_str().unwrap_or_default().to_string(),
            command: req_inner.command,
            args: req_inner.args,
            env_vars: req_inner.env_vars,
            status: "PENDING".to_string(),
            exit_code: None,
        };

        let mut containers_map = self.containers.lock().await;
        containers_map.insert(container_id.clone(), container_state);

        let reply = ContainerResponse {
            container_id,
            status: "PENDING".to_string(),
            message: format!("Container creation initiated, rootfs at {}", rootfs_dir.display()),
        };
        Ok(Response::new(reply))
    }

    async fn get_container_status(
        &self,
        request: Request<ContainerStatusRequest>,
    ) -> Result<Response<ContainerStatusResponse>, Status> {
        println!("Got a get_container_status request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        let containers_map = self.containers.lock().await;
        match containers_map.get(&container_id) {
            Some(state) => {
                // Print details to server console to "use" the fields and for debugging
                println!(
                    "Container Details (ID: {}):\n  Image Tarball: {}\n  RootFS Path: {}\n  Command: {}\n  Args: {:?}\n  Env: {:?}\n  Status: {}\n  Exit Code: {:?}",
                    state.id,
                    state.image_tarball_path,
                    state.rootfs_path,
                    state.command,
                    state.args,
                    state.env_vars,
                    state.status,
                    state.exit_code
                );

                let reply = ContainerStatusResponse {
                    container_id: state.id.clone(),
                    status: state.status.clone(),
                    exit_code: state.exit_code.unwrap_or(0),
                    message: format!("Status for '{}': {}. Command: '{}'", state.id, state.status, state.command),
                };
                Ok(Response::new(reply))
            }
            None => Err(Status::not_found(format!(
                "Container {} not found",
                container_id
            ))),
        }
    }

    type GetContainerLogsStream = tokio_stream::wrappers::ReceiverStream<Result<LogResponse, Status>>;

    async fn get_container_logs(
        &self,
        request: Request<LogRequest>,
    ) -> Result<Response<Self::GetContainerLogsStream>, Status> {
        println!("Got a get_container_logs request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        {
            let containers_map = self.containers.lock().await;
            if !containers_map.contains_key(&container_id) {
                return Err(Status::not_found(format!(
                    "Container {} not found for logs",
                    container_id
                )));
            }
        } // Release lock early

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        // Clone container_id again for the tokio::spawn closure
        let log_stream_container_id = container_id.clone(); 

        tokio::spawn(async move {
            // Dummy log lines
            let logs = vec![
                "Log line 1 for container".to_string(),
                "Log line 2 for container".to_string(),
                "Log line 3 for container".to_string(),
            ];

            for log_line in logs {
                tx.send(Ok(LogResponse {
                    container_id: log_stream_container_id.clone(), // Use the cloned ID here
                    line: log_line,
                })).await.unwrap_or_else(|e| eprintln!("Failed to send log: {:?}",e));
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        });

        Ok(Response::new(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn stop_container(
        &self,
        request: Request<StopContainerRequest>,
    ) -> Result<Response<StopContainerResponse>, Status> {
        println!("Got a stop_container request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        let mut containers_map = self.containers.lock().await;
        match containers_map.get_mut(&container_id) {
            Some(state) => {
                state.status = "STOPPED".to_string();
                // In a real scenario, we might set an exit code or wait for the process to terminate
                state.exit_code = Some(0); // Dummy exit code
                let reply = StopContainerResponse {
                    container_id: state.id.clone(),
                    status: "STOPPED".to_string(),
                    message: format!("Container {} stop request processed", state.id),
                };
                Ok(Response::new(reply))
            }
            None => Err(Status::not_found(format!(
                "Container {} not found for stopping",
                container_id
            ))),
        }
    }

    async fn remove_container(
        &self,
        request: Request<RemoveContainerRequest>,
    ) -> Result<Response<RemoveContainerResponse>, Status> {
        println!("Got a remove_container request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        let mut containers_map = self.containers.lock().await;
        if containers_map.contains_key(&container_id) {
            containers_map.remove(&container_id);
            let reply = RemoveContainerResponse {
                container_id,
                status: "REMOVED".to_string(),
                message: "Container removed successfully".to_string(),
            };
            Ok(Response::new(reply))
        } else {
            Err(Status::not_found(format!(
                "Container {} not found for removal",
                container_id
            )))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // For now, let's listen on a TCP port. We can change to UDS later.
    let addr = "[::1]:50051".parse()?;
    let quilt_service = MyQuiltService::new();

    println!("QuiltService listening on {}", addr);

    Server::builder()
        .add_service(QuiltServiceServer::new(quilt_service))
        .serve(addr)
        .await?;

    Ok(())
}
