// src/cli/containers.rs
// Container management CLI commands

use clap::Subcommand;
use std::collections::HashMap;
use tonic::transport::Channel;

// Import the protobuf definitions
pub mod quilt {
    tonic::include_proto!("quilt");
}

use quilt::quilt_service_client::QuiltServiceClient;
use quilt::{
    CreateContainerRequest, CreateContainerResponse, 
    GetContainerStatusRequest, GetContainerStatusResponse,
    GetContainerLogsRequest, GetContainerLogsResponse,
    StopContainerRequest, StopContainerResponse,
    RemoveContainerRequest, RemoveContainerResponse,
    ExecContainerRequest, ExecContainerResponse,
    ContainerStatus,
};

// Use validation utilities from utils module
#[path = "../utils/mod.rs"]
mod utils;
use utils::validation::InputValidator;
use utils::console::ConsoleLogger;

#[derive(Subcommand, Debug)]
pub enum ContainerCommands {
    /// Create a new container with advanced features
    Create {
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
        
        #[clap(long, help = "Enable network namespace isolation")]
        enable_network_namespace: bool,
        
        #[clap(long, help = "Enable all namespace isolation features")]
        enable_all_namespaces: bool,
        
        /// The command and its arguments to run in the container
        #[clap(required = true, num_args = 1.., 
               help = "Command and its arguments (use -- to separate from CLI options)")]
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
    },
    
    /// Stop a container
    Stop { 
        #[clap(help = "ID of the container to stop")]
        container_id: String 
    },
    
    /// Remove a container
    Remove { 
        #[clap(help = "ID of the container to remove")]
        container_id: String,
        
        #[clap(long, short, help = "Force removal even if running")]
        force: bool,
    },
    
    /// Execute a command inside a running container
    Exec {
        #[clap(help = "ID of the container to execute command in")]
        container_id: String,
        
        #[clap(long, help = "Working directory inside container")]
        workdir: Option<String>,
        
        #[clap(long, help = "Environment variables in KEY=VALUE format", action = clap::ArgAction::Append)]
        env: Vec<String>,
        
        #[clap(long, help = "Capture and return command output")]
        capture_output: bool,
        
        #[clap(help = "Command and arguments to execute", required = true, num_args = 1..)]
        command: Vec<String>,
    },
    
    /// Create a production-ready persistent container
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
        #[clap(long, help = "Readiness timeout in milliseconds", default_value = "15000")]
        timeout: u64,
        #[clap(long, help = "Disable networking")]
        no_network: bool,
        #[clap(long, help = "Custom health check command")]
        health_check: Option<String>,
    },

    /// List all active production containers
    ListProduction,

    /// Check health of production containers
    HealthCheck {
        #[clap(help = "Container ID to check (optional - checks all if not specified)")]
        container_id: Option<String>,
    },

    /// Remove production container
    RemoveProduction {
        #[clap(help = "Container ID to remove")]
        container_id: String,
        #[clap(long, help = "Force removal even if running")]
        force: bool,
    },
}

pub async fn handle_container_command(
    cmd: ContainerCommands,
    mut client: QuiltServiceClient<Channel>
) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        ContainerCommands::Create { 
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
            enable_network_namespace,
            enable_all_namespaces,
            command_and_args 
        } => {
            println!("🚀 Creating container...");
            
            if command_and_args.is_empty() {
                eprintln!("❌ Error: Command cannot be empty.");
                std::process::exit(1);
            }

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
                    enable_network_namespace
                )
            };

            let request = tonic::Request::new(CreateContainerRequest {
                image_path,
                command: command_and_args,
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
                name: String::new(),
                async_mode: false,
                mounts: vec![],
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
        
        ContainerCommands::Status { container_id } => {
            println!("📊 Getting status for container {}...", container_id);
            let request = tonic::Request::new(GetContainerStatusRequest { 
                container_id,
                container_name: String::new(),
            }); 
            match client.get_container_status(request).await {
                Ok(response) => {
                    let res: GetContainerStatusResponse = response.into_inner();
                    let status_enum = match res.status {
                        1 => ContainerStatus::Pending,
                        2 => ContainerStatus::Running,
                        3 => ContainerStatus::Exited,
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
        
        ContainerCommands::Logs { container_id } => {
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
        
        ContainerCommands::Stop { container_id } => {
            println!("🛑 Stopping container {}...", container_id);
            let request = tonic::Request::new(StopContainerRequest { 
                container_id: container_id.clone(), 
                timeout_seconds: 10,
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
        
        ContainerCommands::Remove { container_id, force } => {
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
        
        ContainerCommands::Exec { container_id, workdir, env, capture_output, command } => {
            println!("🚀 Executing command inside container {}...", container_id);
            
            // Parse environment variables
            let mut environment = HashMap::new();
            for env_var in env {
                match InputValidator::parse_key_val(&env_var) {
                    Ok((key, value)) => {
                        environment.insert(key, value);
                    }
                    Err(e) => {
                        eprintln!("❌ Invalid environment variable format '{}': {}", env_var, e);
                        std::process::exit(1);
                    }
                }
            }
            
            let request = tonic::Request::new(ExecContainerRequest {
                container_id: container_id.clone(),
                command,
                working_directory: workdir.unwrap_or_default(),
                environment,
                capture_output,
                container_name: String::new(),
                copy_script: false,
            });

            match client.exec_container(request).await {
                Ok(response) => {
                    let res: ExecContainerResponse = response.into_inner();
                    println!("🔄 Command completed with exit code: {}", res.exit_code);
                    
                    if capture_output {
                        if !res.stdout.is_empty() {
                            println!("📤 Standard Output:");
                            ConsoleLogger::separator();
                            println!("{}", res.stdout);
                            ConsoleLogger::separator();
                        }
                        if !res.stderr.is_empty() {
                            println!("📤 Standard Error:");
                            ConsoleLogger::separator();
                            println!("{}", res.stderr);
                            ConsoleLogger::separator();
                        }
                    }
                    
                    if res.success {
                        println!("✅ Command executed successfully!");
                    } else {
                        println!("❌ Command failed: {}", res.error_message);
                        std::process::exit(res.exit_code);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error executing command: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
        
        ContainerCommands::CreateProduction { image_path, name, setup, env, memory, cpu, timeout: _, no_network, health_check: _ } => {
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
                async_mode: true,
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

        ContainerCommands::ListProduction => {
            // Implementation for listing all active production containers
            println!("🔍 Listing all active production containers...");
            // This command is not implemented in the provided code block
            std::process::exit(1);
        }

        ContainerCommands::HealthCheck { container_id: _ } => {
            // Implementation for checking health of production containers
            println!("🔍 Checking health of production containers...");
            // This command is not implemented in the provided code block
            std::process::exit(1);
        }

        ContainerCommands::RemoveProduction { container_id, force } => {
            println!("🗑️  Removing production container {}...", container_id);
            let request = tonic::Request::new(RemoveContainerRequest { 
                container_id: container_id.clone(), 
                force,
                container_name: String::new(),
            });
            match client.remove_container(request).await {
                Ok(response) => {
                    let res: RemoveContainerResponse = response.into_inner();
                    if res.success {
                        println!("✅ Production container {} removed successfully", container_id);
                    } else {
                        println!("❌ Failed to remove production container: {}", res.error_message);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("❌ Error removing production container: {}", e.message());
                    std::process::exit(1);
                }
            }
        }
    }

    Ok(())
} 