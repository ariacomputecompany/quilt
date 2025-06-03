use clap::{Parser, Subcommand};

pub mod quilt_rpc {
    // Assuming the `quilt` package name was used in the .proto file
    tonic::include_proto!("quilt"); 
}
use quilt_rpc::quilt_service_client::QuiltServiceClient;
use quilt_rpc::{
    CreateContainerRequest,
    ContainerStatusRequest,
    LogRequest,
    StopContainerRequest,
    RemoveContainerRequest
};

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
        #[clap(long)]
        image_tarball_path: String,
        #[clap(long, short, num_args = 0.., value_parser = parse_key_val::<String, String>)]
        env: Vec<(String, String)>,
        /// The command and its arguments to run in the container
        #[clap(required = true, num_args = 1..)]
        command_and_args: Vec<String>,
    },
    /// Get the status of a container
    Status { 
        container_id: String 
    },
    /// Get logs from a container
    Logs {
        container_id: String,
        #[clap(long, short)]
        follow: bool,
    },
    /// Stop a container
    Stop { 
        container_id: String 
    },
    /// Remove a container
    Rm { 
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
        Commands::Create { image_tarball_path, env, mut command_and_args } => {
            println!("CLI: Requesting container creation...");
            
            if command_and_args.is_empty() {
                eprintln!("Error: Command cannot be empty.");
                return Ok(()); // Or appropriate error handling
            }

            let command = command_and_args.remove(0); // First element is the command
            let args = command_and_args; // Remaining elements are the arguments

            let env_vars: Vec<String> = env.into_iter().map(|(k, v)| format!("{}={}", k, v)).collect();
            let request = tonic::Request::new(CreateContainerRequest {
                image_tarball_path,
                command,
                args,
                env_vars,
            });
            match client.create_container(request).await {
                Ok(response) => {
                    let res = response.into_inner();
                    println!("Container created successfully! ID: {}, Status: {}, Message: {}", res.container_id, res.status, res.message);
                }
                Err(e) => eprintln!("Error creating container: {}", e.message()),
            }
        }
        Commands::Status { container_id } => {
            println!("CLI: Requesting status for container {}...", container_id);
            let request = tonic::Request::new(ContainerStatusRequest { container_id });
            match client.get_container_status(request).await {
                Ok(response) => {
                    let res = response.into_inner();
                    println!("Container Status - ID: {}, Status: {}, Exit Code: {}, Message: {}", res.container_id, res.status, res.exit_code, res.message);
                }
                Err(e) => eprintln!("Error getting container status: {}", e.message()),
            }
        }
        Commands::Logs { container_id, follow } => {
            println!("CLI: Requesting logs for container {} (follow: {})...", container_id, follow);
            let request = tonic::Request::new(LogRequest { container_id: container_id.clone(), follow });
            match client.get_container_logs(request).await {
                Ok(response) => {
                    let mut stream = response.into_inner();
                    println!("--- Logs for {} ---", container_id);
                    while let Some(log_entry) = stream.message().await? {
                        println!("{}", log_entry.line);
                    }
                    println!("--- End of logs for {} ---", container_id);
                }
                Err(e) => eprintln!("Error getting container logs: {}", e.message()),
            }
        }
        Commands::Stop { container_id } => {
            println!("CLI: Requesting to stop container {}...", container_id);
            let request = tonic::Request::new(StopContainerRequest { container_id });
            match client.stop_container(request).await {
                Ok(response) => {
                    let res = response.into_inner();
                    println!("Stop Container - ID: {}, Status: {}, Message: {}", res.container_id, res.status, res.message);
                }
                Err(e) => eprintln!("Error stopping container: {}", e.message()),
            }
        }
        Commands::Rm { container_id } => {
            println!("CLI: Requesting to remove container {}...", container_id);
            let request = tonic::Request::new(RemoveContainerRequest { container_id });
            match client.remove_container(request).await {
                Ok(response) => {
                    let res = response.into_inner();
                    println!("Remove Container - ID: {}, Status: {}, Message: {}", res.container_id, res.status, res.message);
                }
                Err(e) => eprintln!("Error removing container: {}", e.message()),
            }
        }
    }

    Ok(())
} 