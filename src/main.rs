use tonic::{transport::Server, Request, Response, Status};
use uuid::Uuid;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use flate2::read::GzDecoder;
use tar::Archive;

mod runtime;
use runtime::{ContainerRuntime, ContainerState};

pub mod quilt_rpc {
    tonic::include_proto!("quilt");
}

use quilt_rpc::quilt_service_server::{QuiltService, QuiltServiceServer};
use quilt_rpc::{
    CreateContainerRequest, CreateContainerResponse, GetContainerStatusRequest,
    ContainerStatusResponse, GetContainerLogsRequest, LogStreamResponse,
    RemoveContainerRequest, RemoveContainerResponse, StopContainerRequest, StopContainerResponse,
};

#[derive(Debug)]
pub struct MyQuiltService {
    runtime: ContainerRuntime,
}

impl MyQuiltService {
    fn new() -> Self {
        MyQuiltService {
            runtime: ContainerRuntime::new(),
        }
    }
}

#[tonic::async_trait]
impl QuiltService for MyQuiltService {
    async fn create_container(
        &self,
        request: Request<CreateContainerRequest>,
    ) -> Result<Response<CreateContainerResponse>, Status> {
        println!("Got a create_container request: {:?}", request);
        let req_inner = request.into_inner();

        if req_inner.command.is_empty() {
            return Err(Status::invalid_argument("Command cannot be empty"));
        }

        let executable = req_inner.command[0].clone();
        let args = req_inner.command.iter().skip(1).cloned().collect::<Vec<String>>();

        let container_id = Uuid::new_v4().to_string();

        let base_runtime_path = PathBuf::from("./active_containers");
        let container_dir = base_runtime_path.join(&container_id);
        let rootfs_dir = container_dir.join("rootfs");

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

        let tarball_file = File::open(image_path).map_err(|e| {
            eprintln!("Failed to open image tarball {}: {}", image_path.display(), e);
            Status::internal(format!("Failed to open image tarball: {}", e))
        })?;

        if image_path.extension().map_or(false, |ext| ext == "gz") {
            let gz_decoder = GzDecoder::new(tarball_file);
            let mut archive = Archive::new(gz_decoder);
            archive.unpack(&rootfs_dir).map_err(|e| {
                eprintln!("Failed to unpack gzipped tarball to {}: {}", rootfs_dir.display(), e);
                Status::internal(format!("Failed to unpack gzipped tarball: {}", e))
            })?;
        } else {
            let mut archive = Archive::new(tarball_file);
            archive.unpack(&rootfs_dir).map_err(|e| {
                eprintln!("Failed to unpack tarball to {}: {}", rootfs_dir.display(), e);
                Status::internal(format!("Failed to unpack tarball: {}", e))
            })?;
        }

        println!("Successfully unpacked image {} to {}", image_path.display(), rootfs_dir.display());

        let container_state = ContainerState {
            id: container_id.clone(),
            image_tarball_path: req_inner.image_tarball_path.clone(),
            rootfs_path: rootfs_dir.to_str().unwrap_or_default().to_string(),
            command: executable,
            args: args,
            env_vars: req_inner.environment_variables.iter().map(|(k,v)| format!("{}={}",k,v)).collect(),
            status: "PENDING".to_string(),
            exit_code: None,
            pid: None,
            logs: Vec::new(),
        };

        // Store the container state
        {
            let mut containers_map = self.runtime.containers.lock().await;
            containers_map.insert(container_id.clone(), container_state);
        }

        // Start the container execution in the background
        println!("Starting container execution for: {}", container_id);
        self.runtime.start_container_execution(container_id.clone()).await;

        let reply = CreateContainerResponse {
            container_id,
            status: "PENDING".to_string(),
        };
        Ok(Response::new(reply))
    }

    async fn get_container_status(
        &self,
        request: Request<GetContainerStatusRequest>,
    ) -> Result<Response<ContainerStatusResponse>, Status> {
        println!("Got a get_container_status request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        let containers_map = self.runtime.containers.lock().await;
        match containers_map.get(&container_id) {
            Some(state) => {
                println!(
                    "Container Details (ID: {}):\n  Image Tarball: {}\n  RootFS Path: {}\n  Command: {}\n  Args: {:?}\n  Env: {:?}\n  Status: {}\n  Exit Code: {:?}\n  PID: {:?}",
                    state.id,
                    state.image_tarball_path,
                    state.rootfs_path,
                    state.command,
                    state.args,
                    state.env_vars,
                    state.status,
                    state.exit_code,
                    state.pid
                );

                let reply = ContainerStatusResponse {
                    container_id: state.id.clone(),
                    status: state.status.clone(),
                    exit_code: state.exit_code.unwrap_or(0),
                    error_message: if state.status == "FAILED" { "Container failed".to_string() } else { "".to_string() },
                };
                Ok(Response::new(reply))
            }
            None => Err(Status::not_found(format!(
                "Container {} not found",
                container_id
            ))),
        }
    }

    type GetContainerLogsStream = tokio_stream::wrappers::ReceiverStream<Result<LogStreamResponse, Status>>;

    async fn get_container_logs(
        &self,
        request: Request<GetContainerLogsRequest>,
    ) -> Result<Response<Self::GetContainerLogsStream>, Status> {
        println!("Got a get_container_logs request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        let logs = {
            let containers_map = self.runtime.containers.lock().await;
            match containers_map.get(&container_id) {
                Some(state) => state.logs.clone(),
                None => {
                    return Err(Status::not_found(format!(
                        "Container {} not found for logs",
                        container_id
                    )));
                }
            }
        };

        let (tx, rx) = tokio::sync::mpsc::channel(4);

        tokio::spawn(async move {
            for log_entry in logs {
                tx.send(Ok(LogStreamResponse {
                    source: log_entry.source.into(),
                    content: log_entry.content.into_bytes(),
                    timestamp_nanos: log_entry.timestamp_nanos,
                })).await.unwrap_or_else(|e| eprintln!("Failed to send log: {:?}",e));
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

        match self.runtime.stop_container(&container_id).await {
            Ok(status) => {
                let reply = StopContainerResponse {
                    container_id,
                    status,
                };
                Ok(Response::new(reply))
            }
            Err(error_msg) => Err(Status::not_found(error_msg)),
        }
    }

    async fn remove_container(
        &self,
        request: Request<RemoveContainerRequest>,
    ) -> Result<Response<RemoveContainerResponse>, Status> {
        println!("Got a remove_container request: {:?}", request);
        let req_inner = request.into_inner();
        let container_id = req_inner.container_id;

        match self.runtime.remove_container(&container_id).await {
            Ok(()) => {
                let reply = RemoveContainerResponse {
                    container_id,
                    message: "Container removed successfully".to_string(),
                };
                Ok(Response::new(reply))
            }
            Err(error_msg) => Err(Status::internal(error_msg)),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let quilt_service = MyQuiltService::new();

    println!("QuiltService listening on {}", addr);

    Server::builder()
        .add_service(QuiltServiceServer::new(quilt_service))
        .serve(addr)
        .await?;

    Ok(())
}
