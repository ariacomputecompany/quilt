use clap::{Parser, Subcommand};
use std::collections::HashMap;

pub mod quilt_rpc {
    // Assuming the `quilt` package name was used in the .proto file
    tonic::include_proto!("quilt"); 
}
use quilt_rpc::quilt_service_client::QuiltServiceClient;
use quilt_rpc::{
    CreateContainerRequest, CreateContainerResponse, GetContainerStatusRequest, ContainerStatusResponse,
    GetContainerLogsRequest, LogStreamResponse, StopContainerRequest, StopContainerResponse,
    RemoveContainerRequest, RemoveContainerResponse,
    // ResourceLimits is nested, imported via its full path if needed or used directly
};
// Correct import for ResourceLimits if used directly by type, or use quilt_rpc::create_container_request::ResourceLimits
use quilt_rpc::create_container_request::ResourceLimits; 
use quilt_rpc::log_stream_response::LogSource;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
#[clap(propagate_version = true)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
    #[clap(short, long, value_parser, default_value = "http://[::1]:50051")]
    server_addr: String,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a new container.
    /// 
    /// Usage: create [OPTIONS] --image-tarball-path <PATH> -- <COMMAND> [ARG...]
    /// 
    /// Use '--' to separate quilt-cli options from the command and its arguments
    /// if the command or any of its arguments start with a hyphen.
    /// Example: quilt-cli create --image path/to.tar -- /bin/sh -c "echo hello"
    Create {
        #[clap(long, help = "Path to the rootfs image tarball (e.g., alpine.tar.gz)")]
        image_tarball_path: String,
        #[clap(long, short, help = "Environment variables in KEY=VALUE format", num_args = 0.., value_parser = parse_key_val::<String, String>)]
        env: Vec<(String, String)>,
        /// The command and its arguments to run in the container
        #[clap(required = true, num_args = 1.., help = "Command and its arguments (e.g., /bin/echo hello world)")]
        command_and_args: Vec<String>,
    },
    /// Get the status of a container
    Status { 
        #[clap(help = "ID of the container to get status for")]
        container_id: String 
    },
    /// Get logs from a container
    Logs {
        #[clap(help = "ID of the container to get logs from")]
        container_id: String,
        #[clap(long, short, help = "Follow the log output")]
        follow: bool,
    },
    /// Stop a container
    Stop { 
        #[clap(help = "ID of the container to stop")]
        container_id: String 
    },
    /// Remove a container
    Rm { 
        #[clap(help = "ID of the container to remove")]
        container_id: String 
    },
}

/// Parses a KEY=VALUE string into a (String, String) tuple
fn parse_key_val<T, U>(s: &str) -> Result<(T, U), Box<dyn std::error::Error + Send + Sync + 'static>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
    U: std::str::FromStr,
    U::Err: std::error::Error + Send + Sync + 'static,
{
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{}`", s))?;
    Ok((s[..pos].parse()?, s[pos + 1..].parse()?))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let mut client = QuiltServiceClient::connect(cli.server_addr.clone()).await
        .map_err(|e| {
            eprintln!("Failed to connect to server at {}: {}. Ensure quiltd is running.", cli.server_addr, e);
            e
        })?;

    match cli.command {
        Commands::Create { image_tarball_path, env, command_and_args } => {
            println!("CLI: Requesting container creation...");
            
            if command_and_args.is_empty() {
                eprintln!("Error: Command cannot be empty.");
                std::process::exit(1);
            }

            let environment_variables: HashMap<String, String> = env.into_iter().collect();
            
            let resource_limits = Some(ResourceLimits { 
                cpu_cores: 0, 
                memory_mb: 0, 
            });

            let request = tonic::Request::new(CreateContainerRequest {
                image_tarball_path,
                command: command_and_args, 
                environment_variables,      
                resource_limits,            
            });

            match client.create_container(request).await {
                Ok(response) => {
                    let res: CreateContainerResponse = response.into_inner();
                    println!("Container created successfully! ID: {}, Status: {}", res.container_id, res.status);
                }
                Err(e) => eprintln!("Error creating container: {}", e.message()),
            }
        }
        Commands::Status { container_id } => {
            println!("CLI: Requesting status for container {}...", container_id);
            let request = tonic::Request::new(GetContainerStatusRequest { container_id }); 
            match client.get_container_status(request).await {
                Ok(response) => {
                    let res: ContainerStatusResponse = response.into_inner();
                    println!("Container Status - ID: {}, Status: {}, Exit Code: {}, Error: {}", 
                             res.container_id, res.status, res.exit_code, res.error_message);
                }
                Err(e) => eprintln!("Error getting container status: {}", e.message()),
            }
        }
        Commands::Logs { container_id, follow } => {
            println!("CLI: Requesting logs for container {} (follow: {})...", container_id, follow);
            let request = tonic::Request::new(GetContainerLogsRequest { container_id: container_id.clone(), follow });
            match client.get_container_logs(request).await {
                Ok(response) => {
                    let mut stream: tonic::Streaming<LogStreamResponse> = response.into_inner();
                    println!("--- Logs for {} ---", container_id);
                    while let Some(log_entry) = stream.message().await? {
                        let source_str = match LogSource::try_from(log_entry.source) {
                            Ok(LogSource::Stdout) => "STDOUT",
                            Ok(LogSource::Stderr) => "STDERR",
                            _ => "UNKNOWN",
                        };
                        let content_str = String::from_utf8_lossy(&log_entry.content);
                        println!("[{}] ({}ns): {}", source_str, log_entry.timestamp_nanos, content_str.trim_end());
                    }
                    println!("--- End of logs for {} ---", container_id);
                }
                Err(e) => eprintln!("Error getting container logs: {}", e.message()),
            }
        }
        Commands::Stop { container_id } => {
            println!("CLI: Requesting to stop container {}...", container_id);
            let request = tonic::Request::new(StopContainerRequest { 
                container_id: container_id.clone(), 
                timeout_seconds: 0 
            });
            match client.stop_container(request).await {
                Ok(response) => {
                    let res: StopContainerResponse = response.into_inner();
                    println!("Stop Container - ID: {}, Status: {}", res.container_id, res.status);
                }
                Err(e) => eprintln!("Error stopping container: {}", e.message()),
            }
        }
        Commands::Rm { container_id } => {
            println!("CLI: Requesting to remove container {}...", container_id);
            let request = tonic::Request::new(RemoveContainerRequest { 
                container_id: container_id.clone(), 
                force: false 
            });
            match client.remove_container(request).await {
                Ok(response) => {
                    let res: RemoveContainerResponse = response.into_inner();
                    println!("Remove Container - ID: {}, Message: {}", res.container_id, res.message);
                }
                Err(e) => eprintln!("Error removing container: {}", e.message()),
            }
        }
    }

    Ok(())
} 