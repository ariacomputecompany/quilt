use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::time::Duration;
use tonic::transport::Channel;

// Import protobuf definitions directly
pub mod quilt {
    tonic::include_proto!("quilt");
}

// Import CLI modules
#[path = "../cli/mod.rs"]
mod cli;
use cli::IccCommands;

use quilt::quilt_service_client::QuiltServiceClient;
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
    ContainerStatus, Mount, MountType,
};

// Use validation utilities from utils module
#[path = "../utils/mod.rs"]
mod utils;
use utils::validation::InputValidator;
use utils::console::ConsoleLogger;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
#[clap(propagate_version = true)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
    #[clap(short, long, value_parser, default_value = "http://127.0.0.1:50051")]
    server_addr: String,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a new container with advanced features
    Create {
        #[clap(short = 'n', long, help = "Container name (must be unique)")]
        name: Option<String>,
        
        #[clap(long, help = "Create as async/long-running container")]
        async_mode: bool,
        
        #[clap(long, help = "Path to the container image tarball")]
        image_path: String,
        
        #[arg(short, long, action = clap::ArgAction::Append, 
              help = "Environment variables in KEY=VALUE format",
              num_args = 0.., value_parser = InputValidator::parse_key_val)]
        env: Vec<(String, String)>,
        
        #[clap(long, help = "Setup commands for dynamic runtime installation (e.g., 'npm: typescript', 'pip: requests')", 
               num_args = 0..)]
        setup: Vec<String>,
        
        #[clap(long, help = "Working directory inside the container")]
        working_directory: Option<String>,
        
        // Resource limits
        #[clap(long, help = "Memory limit in megabytes (0 = default)", default_value = "0")]
        memory_limit: i32,
        
        #[clap(long, help = "CPU limit as percentage (0.0 = default)", default_value = "0.0")]
        cpu_limit: f32,
        
        // Namespace configuration
        #[clap(long, help = "Enable PID namespace isolation")]
        enable_pid_namespace: bool,
        
        #[clap(long, help = "Enable mount namespace isolation")]
        enable_mount_namespace: bool,
        
        #[clap(long, help = "Enable UTS namespace isolation (hostname)")]
        enable_uts_namespace: bool,
        
        #[clap(long, help = "Enable IPC namespace isolation")]
        enable_ipc_namespace: bool,
        
        #[clap(long, help = "Disable network namespace isolation")]
        no_network: bool,
        
        #[clap(long, help = "Enable all namespace isolation features")]
        enable_all_namespaces: bool,
        
        // Volume mounts
        #[clap(short = 'v', long = "volume", 
               help = "Mount volumes (format: [name:]source:dest[:options])",
               num_args = 0..,
               value_parser = InputValidator::parse_volume)]
        volumes: Vec<utils::validation::VolumeMount>,
        
        #[clap(long = "mount",
               help = "Advanced mount syntax (type=bind,source=/host,target=/container,readonly)",
               num_args = 0..,
               value_parser = InputValidator::parse_mount)]
        mounts: Vec<utils::validation::VolumeMount>,
        
        /// The command and its arguments to run in the container
        #[clap(required = false, num_args = 0.., 
               help = "Command and its arguments (use -- to separate from CLI options)")]
        command_and_args: Vec<String>,
    },
    
    /// Get the status of a container
    Status { 
        #[clap(help = "ID or name of the container to get status for")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
    },
    
    /// Get logs from a container
    Logs {
        #[clap(help = "ID or name of the container to get logs from")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
    },
    
    /// Stop a container gracefully
    Stop { 
        #[clap(help = "ID or name of the container to stop")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
        #[clap(short = 't', long, help = "Timeout in seconds before force kill", default_value = "10")]
        timeout: u32,
    },
    
    /// Remove a container
    Remove { 
        #[clap(help = "ID or name of the container to remove")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
        #[clap(long, short = 'f', help = "Force removal even if running")]
        force: bool,
    },
    
    /// Create a production-ready persistent container
    #[clap(name = "create-production")]
    CreateProduction {
        #[clap(help = "Container image tar.gz file")]
        image_path: String,
        #[clap(long, help = "Container name/identifier")]
        name: Option<String>,
        #[clap(long, help = "Setup commands (copy:src:dest, run:command, etc.)")]
        setup: Vec<String>,
        #[clap(long, help = "Environment variables in KEY=VALUE format")]
        env: Vec<String>,
        #[clap(long, help = "Memory limit in MB", default_value = "512")]
        memory: u64,
        #[clap(long, help = "CPU limit percentage", default_value = "50.0")]
        cpu: f64,
        #[clap(long, help = "Disable networking")]
        no_network: bool,
    },

    /// Start a stopped container
    Start {
        #[clap(help = "ID or name of the container to start")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
    },
    
    /// Kill a container immediately
    Kill {
        #[clap(help = "ID or name of the container to kill")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
    },
    
    /// Execute a command in a running container
    Exec {
        #[clap(help = "ID or name of the container")]
        container: String,
        #[clap(short = 'n', long, help = "Treat input as container name")]
        by_name: bool,
        #[clap(short = 'c', long, help = "Command to execute", required = true)]
        command: Vec<String>,
        #[clap(short = 'w', long, help = "Working directory")]
        working_directory: Option<String>,
        #[clap(long, help = "Capture output")]
        capture_output: bool,
    },

    /// Inter-Container Communication commands
    #[clap(subcommand)]
    Icc(IccCommands),
}

async fn resolve_container_id(
    client: &mut QuiltServiceClient<Channel>,
    container: &str,
    by_name: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    if by_name {
        let request = tonic::Request::new(GetContainerByNameRequest {
            name: container.to_string(),
        });
        
        match client.get_container_by_name(request).await {
            Ok(response) => {
                let res = response.into_inner();
                if res.found {
                    Ok(res.container_id)
                } else {
                    Err(format!("Container with name '{}' not found", container).into())
                }
            }
            Err(e) => Err(format!("Failed to lookup container by name: {}", e).into()),
        }
    } else {
        Ok(container.to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logger
    utils::logger::Logger::init();
    
    let cli = Cli::parse();

    // Check for QUILT_SERVER environment variable (used by nested containers)
    let server_addr = if let Ok(env_server) = std::env::var("QUILT_SERVER") {
        format!("http://{}", env_server)
    } else {
        cli.server_addr.clone()
    };

    // Create a channel with extended timeout configuration for concurrent operations
    let channel = tonic::transport::Channel::from_shared(server_addr.clone())?
        .timeout(Duration::from_secs(60))  // Increased from 10s to handle concurrent load
        .connect_timeout(Duration::from_secs(10))  // Increased connection timeout
        .tcp_keepalive(Some(Duration::from_secs(60)))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_while_idle(true)
        .connect()
        .await
        .map_err(|e| {
            eprintln!("❌ Failed to connect to server at {}: {}", server_addr, e);
            eprintln!("   Make sure quiltd is running: ./dev.sh server-bg");
            e
        })?;

    let mut client = QuiltServiceClient::new(channel);

    match cli.command {
        Commands::Create { 
            name,
            async_mode,
            image_path, 
            env, 
            setup,
            working_directory,
            memory_limit,
            cpu_limit,
            enable_pid_namespace,
            enable_mount_namespace,
            enable_uts_namespace,
            enable_ipc_namespace,
            no_network,
            enable_all_namespaces,
            volumes,
            mounts,
            command_and_args 
        } => {
            println!("🚀 Creating container...");
            
            // For async containers, let server set the default command
            let final_command = if command_and_args.is_empty() && !async_mode {
                eprintln!("❌ Error: Command required for non-async containers.");
                std::process::exit(1);
            } else {
                command_and_args
            };

            let environment: HashMap<String, String> = env.into_iter().collect();
            
            // If enable_all_namespaces is true, enable all namespace options
            let (pid_ns, mount_ns, uts_ns, ipc_ns, net_ns) = if enable_all_namespaces {
                (true, true, true, true, true)
            } else {
                (
                    enable_pid_namespace,
                    enable_mount_namespace, 
                    enable_uts_namespace,
                    enable_ipc_namespace,
                    !no_network  // Fixed: Use no_network flag (default networking enabled)
                )
            };
            
            // Combine volumes and mounts, validate security
            let mut all_mounts: Vec<utils::validation::VolumeMount> = volumes;
            all_mounts.extend(mounts);
            
            // Convert to protobuf Mount format with security validation
            let mut proto_mounts: Vec<Mount> = Vec::new();
            for mount in all_mounts {
                // Security validation
                if let Err(e) = utils::security::SecurityValidator::validate_mount(&mount) {
                    eprintln!("❌ Error: Mount validation failed: {}", e);
                    std::process::exit(1);
                }
                
                // Convert mount type
                let proto_mount_type = match mount.mount_type {
                    utils::validation::MountType::Bind => MountType::Bind as i32,
                    utils::validation::MountType::Volume => MountType::Volume as i32,
                    utils::validation::MountType::Tmpfs => MountType::Tmpfs as i32,
                };
                
                proto_mounts.push(Mount {
                    source: mount.source,
                    target: mount.target,
                    r#type: proto_mount_type,
                    readonly: mount.readonly,
                    options: mount.options,
                });
            }

            let request = tonic::Request::new(CreateContainerRequest {
                image_path,
                command: final_command,
                environment,
                working_directory: working_directory.unwrap_or_default(),
                setup_commands: setup,
                memory_limit_mb: memory_limit,
                cpu_limit_percent: cpu_limit,
                enable_pid_namespace: pid_ns,
                enable_mount_namespace: mount_ns,
                enable_uts_namespace: uts_ns,
                enable_ipc_namespace: ipc_ns,
                enable_network_namespace: net_ns,
                name: name.unwrap_or_default(),
                async_mode,
                mounts: proto_mounts,
            });

            match client.create_container(request).await {
                Ok(response) => {
                    let res: CreateContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Container created successfully!");
                        println!("   Container ID: {}", res.container_id);
                    } else {
                        println!("❌ Failed to create container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error creating container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Status { container, by_name } => {
            // Resolve container name to ID if needed
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            
            println!("📊 Getting status for container {}...", container_id);
            let mut request = tonic::Request::new(GetContainerStatusRequest {
                container_id: container_id.clone(),
                container_name: String::new(), // We already resolved it
            });
            request.set_timeout(Duration::from_secs(60)); // ELITE: Extended timeout for network load
            
            match client.get_container_status(request).await {
                Ok(response) => {
                    let res: GetContainerStatusResponse = response.into_inner();
                    let status_enum = match res.status {
                        0 => ContainerStatus::Pending,
                        1 => ContainerStatus::Running,
                        2 => ContainerStatus::Exited,
                        3 => ContainerStatus::Failed,
                        _ => ContainerStatus::Failed,
                    };
                    let status_str = match status_enum {
                        ContainerStatus::Pending => "PENDING",
                        ContainerStatus::Running => "RUNNING",
                        ContainerStatus::Exited => "EXITED",
                        ContainerStatus::Failed => "FAILED",
                    };
                    
                    // Use ConsoleLogger for consistent formatting
                    let created_at_formatted = utils::process::ProcessUtils::format_timestamp(res.created_at);
                    ConsoleLogger::format_container_status(
                        &res.container_id,
                        status_str,
                        &created_at_formatted,
                        &res.rootfs_path,
                        if res.pid > 0 { Some(res.pid) } else { None },
                        if res.exit_code != 0 || status_enum == ContainerStatus::Exited { Some(res.exit_code) } else { None },
                        &res.error_message,
                        if res.memory_usage_bytes > 0 { Some(res.memory_usage_bytes) } else { None },
                        if !res.ip_address.is_empty() { Some(&res.ip_address) } else { None },
                    );
                }
                Err(e) => {
                    eprintln!("❌ Error getting container status: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Logs { container, by_name } => {
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            println!("📜 Getting logs for container {}...", container_id);
            let request = tonic::Request::new(GetContainerLogsRequest { 
                container_id: container_id.clone(),
                container_name: String::new(),
            });
            match client.get_container_logs(request).await {
                Ok(response) => {
                    let res: GetContainerLogsResponse = response.into_inner();
                    
                    if res.logs.is_empty() {
                        println!("📝 No logs available for container {}", container_id);
                    } else {
                        println!("📝 Logs for container {}:", container_id);
                        ConsoleLogger::separator();
                        
                        for log_entry in res.logs {
                            let timestamp = log_entry.timestamp;
                            let message = log_entry.message;
                            
                            // Convert timestamp to human readable format
                            let formatted_time = utils::process::ProcessUtils::format_timestamp(timestamp);
                            
                            println!("[{}] {}", formatted_time, message);
                        }
                        ConsoleLogger::separator();
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error getting container logs: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Stop { container, by_name, timeout } => {
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            println!("🛑 Stopping container {}...", container_id);
            let request = tonic::Request::new(StopContainerRequest { 
                container_id: container_id.clone(), 
                timeout_seconds: timeout as i32,
                container_name: String::new(),
            });
            match client.stop_container(request).await {
                Ok(response) => {
                    let res: StopContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Container {} stopped successfully", container_id);
                    } else {
                        println!("❌ Failed to stop container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error stopping container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Remove { container, by_name, force } => {
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            println!("🗑️  Removing container {}...", container_id);
            let request = tonic::Request::new(RemoveContainerRequest { 
                container_id: container_id.clone(), 
                force,
                container_name: String::new(),
            });
            match client.remove_container(request).await {
                Ok(response) => {
                    let res: RemoveContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Container {} removed successfully", container_id);
                    } else {
                        println!("❌ Failed to remove container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error removing container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::CreateProduction { image_path, name, setup, env, memory, cpu, no_network } => {
            let container_name = name.clone();
            println!("🚀 Creating production container using the new event-driven readiness system...");
            
            // Parse environment variables
            let mut environment = std::collections::HashMap::new();
            for env_var in env {
                if let Some((key, value)) = env_var.split_once('=') {
                    environment.insert(key.to_string(), value.to_string());
                }
            }
            
            // Create production container using enhanced daemon runtime with event-driven readiness
            let create_request = CreateContainerRequest {
                image_path,
                command: vec!["tail".to_string(), "-f".to_string(), "/dev/null".to_string()], // Default persistent command
                environment,
                working_directory: String::new(), // Empty string instead of None
                setup_commands: setup,
                memory_limit_mb: if memory > 0 { memory as i32 } else { 512 },
                cpu_limit_percent: if cpu > 0.0 { cpu as f32 } else { 50.0 },
                enable_network_namespace: !no_network,
                enable_pid_namespace: true,
                enable_mount_namespace: true,
                enable_uts_namespace: true,
                enable_ipc_namespace: true,
                name: name.unwrap_or_default(),
                async_mode: true, // Production containers are async by default
                mounts: vec![],
            };

            match client.create_container(tonic::Request::new(create_request)).await {
                Ok(response) => {
                    let res = response.into_inner();
                    if res.success {
                        println!("✅ Production container created and ready with ID: {}", res.container_id);
                        println!("   Memory: {}MB", memory);
                        println!("   CPU: {}%", cpu);
                        println!("   Networking: {}", if !no_network { "enabled" } else { "disabled" });
                        println!("   Event-driven readiness: enabled");
                        println!("   Container automatically started with PID verification");
                        
                        if let Some(ref name) = container_name {
                            println!("   Custom name: {}", name);
                        }
                    } else {
                        eprintln!("❌ Failed to create production container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error creating production container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }

        Commands::Start { container, by_name } => {
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            println!("▶️  Starting container {}...", container_id);
            
            let request = tonic::Request::new(StartContainerRequest {
                container_id: container_id.clone(),
                container_name: String::new(),
            });
            
            match client.start_container(request).await {
                Ok(response) => {
                    let res: StartContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Container {} started successfully", container_id);
                        if res.pid > 0 {
                            println!("   Process ID: {}", res.pid);
                        }
                    } else {
                        println!("❌ Failed to start container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error starting container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Kill { container, by_name } => {
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            println!("💀 Killing container {}...", container_id);
            
            let request = tonic::Request::new(KillContainerRequest {
                container_id: container_id.clone(),
                container_name: String::new(),
            });
            
            match client.kill_container(request).await {
                Ok(response) => {
                    let res: KillContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Container {} killed successfully", container_id);
                    } else {
                        println!("❌ Failed to kill container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error killing container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        Commands::Exec { container, by_name, command, working_directory, capture_output } => {
            let container_id = resolve_container_id(&mut client, &container, by_name).await?;
            println!("🔧 Executing command in container {}...", container_id);
            
            // Check if the command is a local script file
            let copy_script = command.len() == 1 && std::path::Path::new(&command[0]).exists();
            
            let request = tonic::Request::new(ExecContainerRequest {
                container_id: container_id.clone(),
                container_name: String::new(),
                command,
                working_directory: working_directory.unwrap_or_default(),
                environment: HashMap::new(),
                capture_output,
                copy_script,
            });
            
            match client.exec_container(request).await {
                Ok(response) => {
                    let res: ExecContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Command executed successfully (exit code: {})", res.exit_code);
                        if capture_output {
                            if !res.stdout.is_empty() {
                                println!("\n📤 Standard Output:");
                                println!("{}", res.stdout);
                            }
                            if !res.stderr.is_empty() {
                                println!("\n📤 Standard Error:");
                                println!("{}", res.stderr);
                            }
                        }
                    } else {
                        println!("❌ Command execution failed: {}", res.error_message);
                        if capture_output && !res.stderr.is_empty() {
                            println!("\n📤 Standard Error:");
                            println!("{}", res.stderr);
                        }
                        std::process::exit(res.exit_code);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error executing command: {}", e.message());
                    std::process::exit(1);
                }
            }
        }

        Commands::Icc(icc_cmd) => {
            cli::icc::handle_icc_command(icc_cmd, client).await?
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    
    #[test]
    fn test_create_command_parsing() {
        let args = vec![
            "cli",
            "create",
            "-n", "test-container",
            "--image-path", "test.tar.gz",
            "--", "echo", "hello"
        ];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Create { name, image_path, command_and_args, .. } => {
                assert_eq!(name, Some("test-container".to_string()));
                assert_eq!(image_path, "test.tar.gz");
                assert_eq!(command_and_args, vec!["echo", "hello"]);
            }
            _ => panic!("Expected Create command"),
        }
    }
    
    #[test]
    fn test_create_async_mode() {
        let args = vec![
            "cli",
            "create",
            "-n", "async-test",
            "--async-mode",
            "--image-path", "test.tar.gz"
        ];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Create { name, async_mode, command_and_args, .. } => {
                assert_eq!(name, Some("async-test".to_string()));
                assert!(async_mode);
                assert!(command_and_args.is_empty());
            }
            _ => panic!("Expected Create command"),
        }
    }
    
    #[test]
    fn test_status_by_name() {
        let args = vec!["cli", "status", "my-container", "-n"];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Status { container, by_name } => {
                assert_eq!(container, "my-container");
                assert!(by_name);
            }
            _ => panic!("Expected Status command"),
        }
    }
    
    #[test]
    fn test_exec_command_parsing() {
        let args = vec![
            "cli",
            "exec",
            "container-name",
            "-n",
            "-c", "echo hello world",
            "--capture-output"
        ];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Exec { container, by_name, command, capture_output, .. } => {
                assert_eq!(container, "container-name");
                assert!(by_name);
                assert_eq!(command, vec!["echo hello world"]);
                assert!(capture_output);
            }
            _ => panic!("Expected Exec command"),
        }
    }
    
    #[test]
    fn test_start_command() {
        let args = vec!["cli", "start", "stopped-container", "-n"];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Start { container, by_name } => {
                assert_eq!(container, "stopped-container");
                assert!(by_name);
            }
            _ => panic!("Expected Start command"),
        }
    }
    
    #[test]
    fn test_kill_command() {
        let args = vec!["cli", "kill", "running-container", "-n"];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Kill { container, by_name } => {
                assert_eq!(container, "running-container");
                assert!(by_name);
            }
            _ => panic!("Expected Kill command"),
        }
    }
    
    #[test]
    fn test_stop_with_timeout() {
        let args = vec!["cli", "stop", "container-id", "-t", "30"];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Stop { container, by_name, timeout } => {
                assert_eq!(container, "container-id");
                assert!(!by_name); // Not using name
                assert_eq!(timeout, 30);
            }
            _ => panic!("Expected Stop command"),
        }
    }
    
    #[test]
    fn test_remove_with_force() {
        let args = vec!["cli", "remove", "test-container", "-n", "--force"];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Remove { container, by_name, force } => {
                assert_eq!(container, "test-container");
                assert!(by_name);
                assert!(force);
            }
            _ => panic!("Expected Remove command"),
        }
    }
    
    #[test]
    fn test_logs_by_name() {
        let args = vec!["cli", "logs", "my-container", "-n"];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Logs { container, by_name } => {
                assert_eq!(container, "my-container");
                assert!(by_name);
            }
            _ => panic!("Expected Logs command"),
        }
    }
    
    #[test]
    fn test_env_var_parsing() {
        let args = vec![
            "cli",
            "create",
            "--image-path", "test.tar.gz",
            "-e", "KEY1=value1",
            "-e", "KEY2=value2",
            "--", "echo", "test"
        ];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Create { env, .. } => {
                assert_eq!(env.len(), 2);
                assert!(env.contains(&("KEY1".to_string(), "value1".to_string())));
                assert!(env.contains(&("KEY2".to_string(), "value2".to_string())));
            }
            _ => panic!("Expected Create command"),
        }
    }
    
    #[test]
    fn test_namespace_flags() {
        let args = vec![
            "cli",
            "create",
            "--image-path", "test.tar.gz",
            "--enable-all-namespaces",
            "--", "echo", "test"
        ];
        
        let cli = Cli::parse_from(args);
        
        match cli.command {
            Commands::Create { enable_all_namespaces, .. } => {
                assert!(enable_all_namespaces);
            }
            _ => panic!("Expected Create command"),
        }
    }
    
    #[test]
    fn test_resolve_container_id_logic() {
        // Test the helper function with mock client
        // This would require more setup to properly mock the gRPC client
        // For now, we're testing the CLI parsing which is the main concern
    }
} 