// src/cli/icc.rs
// Inter-Container Communication CLI commands

use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::time::Duration;
use tonic::transport::Channel;

// Use protobuf definitions from parent
use crate::quilt::quilt_service_client::QuiltServiceClient;
use crate::quilt::{
    GetContainerStatusRequest,
    ExecContainerRequest,
    ContainerStatus,
};

#[derive(Debug, Clone)]
pub enum ConnectionType {
    Tcp { port: u16 },
    Udp { port: u16 },
    WebSocket { path: String },
    Database { pool_size: u32, db_type: DatabaseType },
    MessageQueue { queue_name: String },
    Http { method: String },
    Grpc { service: String },
}

#[derive(Debug, Clone)]
pub enum DatabaseType {
    PostgreSql,
    MySql,
    Redis,
    MongoDb,
}

#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub from_container: String,
    pub to_container: String,
    pub connection_type: ConnectionType,
    pub established_at: u64,
    pub status: ConnectionStatus,
    pub connection_id: String,
}

#[derive(Debug, Clone)]
pub enum ConnectionStatus {
    Establishing,
    Active,
    Failed(String),
    Disconnected,
}

#[derive(Subcommand, Debug)]
pub enum IccCommands {
    /// Test connectivity between containers
    Ping {
        #[clap(help = "Source container ID")]
        from_container: String,
        #[clap(help = "Target container ID or IP address")]
        target: String,
        #[clap(long, help = "Number of ping packets to send", default_value = "3")]
        count: u32,
        #[clap(long, help = "Timeout in seconds", default_value = "5")]
        timeout: u32,
    },

    /// Establish persistent connections between containers
    Connect {
        #[clap(help = "Source container ID")]
        from_container: String,
        #[clap(help = "Target container ID")]
        to_container: String,
        #[clap(long, help = "Connection type", default_value = "tcp")]
        connection_type: String,
        #[clap(long, help = "Target port for TCP/UDP connections")]
        port: Option<u16>,
        #[clap(long, help = "Connection pool size for database connections", default_value = "5")]
        pool_size: Option<u32>,
        #[clap(long, help = "WebSocket path for WebSocket connections")]
        path: Option<String>,
        #[clap(long, help = "Queue name for message queue connections")]
        queue: Option<String>,
        #[clap(long, help = "Keep connection alive", default_value = "true")]
        persistent: bool,
        #[clap(long, help = "Auto-reconnect on failure", default_value = "true")]
        auto_reconnect: bool,
    },

    /// Disconnect containers
    Disconnect {
        #[clap(help = "Source container ID")]
        from_container: String,
        #[clap(help = "Target container ID (optional - disconnects all if not specified)")]
        to_container: Option<String>,
        #[clap(long, help = "Specific connection ID to disconnect")]
        connection_id: Option<String>,
        #[clap(long, help = "Force disconnect even if connection is active")]
        force: bool,
        #[clap(long, help = "Disconnect all connections for this container")]
        all: bool,
    },

    /// Manage and view connections
    Connections {
        #[clap(subcommand)]
        action: ConnectionAction,
    },

    /// Execute commands inside containers for testing
    Exec {
        #[clap(help = "Container ID to execute command in")]
        container_id: String,
        #[clap(long, help = "Working directory inside container")]
        workdir: Option<String>,
        #[clap(long, help = "Environment variables", action = clap::ArgAction::Append)]
        env: Vec<String>,
        #[clap(help = "Command and arguments to execute", required = true, num_args = 1..)]
        command: Vec<String>,
    },

    /// Show network topology and information
    Network {
        #[clap(subcommand)]
        action: NetworkAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConnectionAction {
    /// List all active connections
    List {
        #[clap(long, help = "Filter by container ID")]
        container: Option<String>,
        #[clap(long, help = "Filter by connection type")]
        connection_type: Option<String>,
        #[clap(long, help = "Show only active connections")]
        active_only: bool,
        #[clap(long, help = "Output format: table, json, yaml", default_value = "table")]
        format: String,
    },

    /// Show detailed information about a specific connection
    Show {
        #[clap(help = "Connection ID")]
        connection_id: String,
    },

    /// Monitor connection health and status
    Monitor {
        #[clap(help = "Container ID to monitor (optional - monitors all if not specified)")]
        container: Option<String>,
        #[clap(long, help = "Refresh interval in seconds", default_value = "5")]
        interval: u32,
        #[clap(long, help = "Show connection metrics")]
        metrics: bool,
    },

    /// Test connection health
    Health {
        #[clap(help = "Connection ID or container ID")]
        target: String,
        #[clap(long, help = "Detailed health check")]
        detailed: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum NetworkAction {
    /// Show network topology
    Topology {
        #[clap(long, help = "Output format: ascii, json, dot", default_value = "ascii")]
        format: String,
        #[clap(long, help = "Include connection details")]
        details: bool,
    },

    /// List all container IP addresses
    List {
        #[clap(long, help = "Show only running containers")]
        running_only: bool,
        #[clap(long, help = "Output format: table, json, yaml", default_value = "table")]
        format: String,
    },

    /// Show network information for a specific container
    Show {
        #[clap(help = "Container ID")]
        container_id: String,
    },

    /// Test network connectivity
    Test {
        #[clap(help = "Source container ID")]
        from_container: String,
        #[clap(help = "Target container ID or IP")]
        target: String,
        #[clap(long, help = "Test specific port")]
        port: Option<u16>,
        #[clap(long, help = "Test protocol: tcp, udp, icmp", default_value = "icmp")]
        protocol: String,
    },
}

// Implementation functions (to be implemented)
pub async fn handle_icc_command(cmd: IccCommands, mut client: QuiltServiceClient<Channel>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        IccCommands::Ping { from_container, target, count, timeout } => {
            handle_ping_command(from_container, target, count, timeout, &mut client).await
        },
        IccCommands::Connect { 
            from_container, 
            to_container, 
            connection_type, 
            port, 
            pool_size, 
            path, 
            queue, 
            persistent, 
            auto_reconnect 
        } => {
            handle_connect_command(
                from_container, 
                to_container, 
                connection_type, 
                port, 
                pool_size, 
                path, 
                queue, 
                persistent, 
                auto_reconnect,
                &mut client
            ).await
        },
        IccCommands::Disconnect { from_container, to_container, connection_id, force, all } => {
            handle_disconnect_command(from_container, to_container, connection_id, force, all, &mut client).await
        },
        IccCommands::Connections { action } => {
            handle_connections_command(action, &mut client).await
        },
        IccCommands::Exec { container_id, workdir, env, command } => {
            handle_exec_command(container_id, workdir, env, command, &mut client).await
        },
        IccCommands::Network { action } => {
            handle_network_command(action, &mut client).await
        },
    }
}

// Placeholder implementations - to be filled in
async fn handle_ping_command(
    from_container: String, 
    target: String, 
    count: u32, 
    timeout: u32,
    client: &mut QuiltServiceClient<Channel>
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🏓 Pinging from {} to {} ({} packets, {}s timeout)", from_container, target, count, timeout);
    
    // ELITE: Check source container status
    let mut from_request = tonic::Request::new(GetContainerStatusRequest { 
        container_id: from_container.clone(),
        container_name: String::new(),
    });
    from_request.set_timeout(Duration::from_secs(30));
    
    let from_status = match client.get_container_status(from_request).await {
        Ok(response) => {
            let status = response.into_inner();
            let container_status = match status.status {
                0 => ContainerStatus::Pending,
                1 => ContainerStatus::Running,
                2 => ContainerStatus::Exited,
                _ => ContainerStatus::Failed,
            };
            
            if !matches!(container_status, ContainerStatus::Running) {
                return Err(format!("Source container {} is not running (status: {:?})", from_container, container_status).into());
            }
            status
        }
        Err(e) => {
            return Err(format!("Failed to get status for container {}: {}", from_container, e).into());
        }
    };
    
    // ELITE: Determine target IP
    let final_target_ip = if target.contains('.') {
        // Already an IP address
        target
    } else {
        // Container ID - get its IP
        let mut target_request = tonic::Request::new(GetContainerStatusRequest {
            container_id: target.clone(),
            container_name: String::new(),
        });
        target_request.set_timeout(Duration::from_secs(30));
        
        match client.get_container_status(target_request).await {
            Ok(response) => {
                let status = response.into_inner();
                let container_status = match status.status {
                    0 => ContainerStatus::Pending,
                    1 => ContainerStatus::Running,
                    2 => ContainerStatus::Exited,
                    _ => ContainerStatus::Failed,
                };
                
                if !matches!(container_status, ContainerStatus::Running) {
                    return Err(format!("Target container {} is not running (status: {:?})", target, container_status).into());
                }
                
                if status.ip_address.is_empty() || status.ip_address == "No IP assigned" {
                    return Err(format!("Target container {} has no IP address assigned", target).into());
                }
                
                status.ip_address
            }
            Err(e) => {
                return Err(format!("Failed to get status for target container {}: {}", target, e).into());
            }
        }
    };
    
    // ELITE: Use optimized ping with adaptive timeout
    let adaptive_timeout = std::cmp::max(timeout, 10); // Minimum 10s for network load
    let ping_cmd = vec![
        "ping".to_string(),
        "-c".to_string(), count.to_string(),
        "-W".to_string(), adaptive_timeout.to_string(),
        "-i".to_string(), "0.5".to_string(),  // ELITE: Faster ping interval
        final_target_ip.clone()
    ];
    
    let mut exec_request = tonic::Request::new(ExecContainerRequest {
        container_id: from_container.clone(),
        command: ping_cmd,
        working_directory: String::new(),
        environment: HashMap::new(),
        capture_output: true,
        container_name: String::new(),
        copy_script: false,
    });
    // ELITE: Much more generous timeout for exec under load
    exec_request.set_timeout(Duration::from_secs(adaptive_timeout as u64 + 10)); 
    
    println!("📡 Executing ping with {:.1}s timeout...", adaptive_timeout);
    
    match client.exec_container(exec_request).await {
        Ok(response) => {
            let result = response.into_inner();
            
            if result.success {
                println!("✅ Ping successful!");
                if !result.stdout.is_empty() {
                    println!("📤 Output:");
                    println!("{}", result.stdout);
                }
            } else {
                println!("❌ Ping from {} to {} failed. Exit code: {}", from_container, final_target_ip, result.exit_code);
                if result.exit_code == 124 {
                    println!("⚠️  Exit code 124 indicates timeout - network may still be initializing");
                }
                if !result.stdout.is_empty() {
                    println!("📤 Output:");
                    println!("{}", result.stdout);
                }
                if !result.stderr.is_empty() {
                    println!("📤 Error:");
                    println!("{}", result.stderr);
                }
            }
            
            Ok(())
        }
        Err(e) => {
            Err(format!("Failed to execute ping command: {}", e).into())
        }
    }
}

async fn handle_connect_command(
    from_container: String,
    to_container: String,
    connection_type: String,
    port: Option<u16>,
    _pool_size: Option<u32>,
    _path: Option<String>,
    _queue: Option<String>,
    persistent: bool,
    auto_reconnect: bool,
    client: &mut QuiltServiceClient<Channel>
) -> Result<(), Box<dyn std::error::Error>> {
    println!("🔗 Establishing {} connection from {} to {}", connection_type, from_container, to_container);
    if let Some(port) = port {
        println!("   Port: {}", port);
    }
    if persistent {
        println!("   Mode: Persistent");
    }
    if auto_reconnect {
        println!("   Auto-reconnect: Enabled");
    }
    // TODO: Implement connection establishment
    Ok(())
}

async fn handle_disconnect_command(
    from_container: String,
    to_container: Option<String>,
    connection_id: Option<String>,
    force: bool,
    all: bool,
    client: &mut QuiltServiceClient<Channel>
) -> Result<(), Box<dyn std::error::Error>> {
    if all {
        println!("🔌 Disconnecting all connections for {}", from_container);
    } else if let Some(to_container) = to_container {
        println!("🔌 Disconnecting {} from {}", from_container, to_container);
    } else if let Some(connection_id) = connection_id {
        println!("🔌 Disconnecting connection {}", connection_id);
    }
    if force {
        println!("   Mode: Force disconnect");
    }
    // TODO: Implement disconnection
    Ok(())
}

async fn handle_connections_command(action: ConnectionAction, client: &mut QuiltServiceClient<Channel>) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ConnectionAction::List { container, connection_type, active_only, format } => {
            println!("📋 Listing connections (format: {})", format);
            if let Some(container) = container {
                println!("   Filter: Container {}", container);
            }
            if active_only {
                println!("   Filter: Active connections only");
            }
            // TODO: Implement connection listing
        },
        ConnectionAction::Show { connection_id } => {
            println!("🔍 Showing connection details for {}", connection_id);
            // TODO: Implement connection details
        },
        ConnectionAction::Monitor { container, interval, metrics } => {
            println!("📊 Monitoring connections ({}s interval)", interval);
            if let Some(container) = container {
                println!("   Monitoring: {}", container);
            }
            if metrics {
                println!("   Including: Connection metrics");
            }
            // TODO: Implement connection monitoring
        },
        ConnectionAction::Health { target, detailed } => {
            println!("🏥 Checking connection health for {}", target);
            if detailed {
                println!("   Mode: Detailed health check");
            }
            // TODO: Implement health checking
        },
    }
    Ok(())
}

async fn handle_exec_command(
    container_id: String,
    workdir: Option<String>,
    env: Vec<String>,
    command: Vec<String>,
    client: &mut QuiltServiceClient<Channel>
) -> Result<(), Box<dyn std::error::Error>> {
    println!("⚡ Executing command in container {}", container_id);
    println!("   Command: {:?}", command);
    if let Some(ref workdir) = workdir {
        println!("   Working directory: {}", workdir);
    }
    if !env.is_empty() {
        println!("   Environment variables: {:?}", env);
    }

    // Parse environment variables from "KEY=VALUE" format
    let mut environment = HashMap::new();
    for env_var in env {
        if let Some((key, value)) = env_var.split_once('=') {
            environment.insert(key.to_string(), value.to_string());
        } else {
            return Err(format!("Invalid environment variable format: {}. Use KEY=VALUE", env_var).into());
        }
    }

    // Execute the command
    let mut exec_request = tonic::Request::new(ExecContainerRequest {
        container_id: container_id.clone(),
        command,
        working_directory: workdir.unwrap_or_default(),
        environment,
        capture_output: true,
        container_name: String::new(),
        copy_script: false,
    });
    exec_request.set_timeout(Duration::from_secs(30)); // Generous timeout for exec commands

    match client.exec_container(exec_request).await {
        Ok(response) => {
            let result = response.into_inner();
            
            if result.success {
                println!("✅ Command executed successfully (exit code: {})", result.exit_code);
                if !result.stdout.is_empty() {
                    println!("📤 Output:");
                    println!("{}", result.stdout);
                }
                if !result.stderr.is_empty() {
                    println!("⚠️ Error output:");
                    println!("{}", result.stderr);
                }
            } else {
                println!("❌ Command failed with exit code: {}", result.exit_code);
                if !result.stderr.is_empty() {
                    println!("📤 Error:");
                    println!("{}", result.stderr);
                }
                if !result.stdout.is_empty() {
                    println!("📤 Output:");
                    println!("{}", result.stdout);
                }
            }
            
            Ok(())
        }
        Err(e) => {
            Err(format!("Failed to execute command: {}", e).into())
        }
    }
}

async fn handle_network_command(action: NetworkAction, client: &mut QuiltServiceClient<Channel>) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        NetworkAction::Topology { format, details } => {
            println!("🌐 Network topology (format: {})", format);
            if details {
                println!("   Including: Connection details");
            }
            // TODO: Implement topology display
        },
        NetworkAction::List { running_only, format } => {
            println!("📋 Container network information (format: {})", format);
            if running_only {
                println!("   Filter: Running containers only");
            }
            // TODO: Implement network listing
        },
        NetworkAction::Show { container_id } => {
            println!("🔍 Network information for container {}", container_id);
            // TODO: Implement container network info
        },
        NetworkAction::Test { from_container, target, port, protocol } => {
            println!("🧪 Testing {} connectivity from {} to {}", protocol, from_container, target);
            if let Some(port) = port {
                println!("   Port: {}", port);
            }
            // TODO: Implement network testing
        },
    }
    Ok(())
} 