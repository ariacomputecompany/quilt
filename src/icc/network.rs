// src/icc/network.rs
// Optimized Inter-Container Communication using Linux Bridge

use crate::utils::{CommandExecutor, ConsoleLogger};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub bridge_name: String,
    pub subnet_cidr: String,
    pub bridge_ip: String,
    pub next_ip: Arc<Mutex<u32>>,
}

#[derive(Debug, Clone)]
pub struct ContainerNetworkConfig {
    pub ip_address: String,
    pub subnet_mask: String,
    pub gateway_ip: String,
    pub container_id: String,
    pub veth_host_name: String,
    pub veth_container_name: String,
}

pub struct NetworkManager {
    config: NetworkConfig,
    bridge_initialized: Arc<Mutex<bool>>,
}

impl NetworkManager {
    pub fn new(bridge_name: &str, subnet_cidr: &str) -> Result<Self, String> {
        let config = NetworkConfig {
            bridge_name: bridge_name.to_string(),
            subnet_cidr: subnet_cidr.to_string(),
            bridge_ip: "10.42.0.1".to_string(),
            next_ip: Arc::new(Mutex::new(2)), // Start from 10.42.0.2
        };
        
        Ok(Self { 
            config,
            bridge_initialized: Arc::new(Mutex::new(false)),
        })
    }

    /// Initialize the bridge if not already done - thread-safe singleton pattern
    pub fn ensure_bridge_ready(&self) -> Result<(), String> {
        let mut initialized = self.bridge_initialized.lock()
            .map_err(|_| "Failed to lock bridge initialization mutex")?;
        
        if *initialized {
            return Ok(());
        }

        ConsoleLogger::progress(&format!("Initializing network bridge: {}", self.config.bridge_name));
        
        // Check if bridge already exists
        if !self.bridge_exists() {
            self.create_bridge()?;
        }
        
        // Ensure bridge has correct IP
        self.configure_bridge_ip()?;
        
        // Bring bridge up
        self.bring_bridge_up()?;
        
        *initialized = true;
        ConsoleLogger::success(&format!("Network bridge '{}' is ready", self.config.bridge_name));
        Ok(())
    }

    /// Create a new network configuration for a container
    pub fn allocate_container_network(&self, container_id: &str) -> Result<ContainerNetworkConfig, String> {
        // Ensure bridge is ready
        self.ensure_bridge_ready()?;
        
        // Allocate unique IP
        let ip_address = self.allocate_next_ip()?;
        let veth_host_name = format!("veth-{}", &container_id[..8]);
        let veth_container_name = format!("vethc-{}", &container_id[..8]);
        
        ConsoleLogger::debug(&format!("Allocated IP {} for container {}", ip_address, container_id));
        
        Ok(ContainerNetworkConfig {
            ip_address,
            subnet_mask: "16".to_string(), // /16 for 10.42.0.0/16
            gateway_ip: self.config.bridge_ip.clone(),
            container_id: container_id.to_string(),
            veth_host_name,
            veth_container_name,
        })
    }

    /// Set up the network interface for a running container
    pub fn setup_container_network(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::progress(&format!("Setting up network for container {} (PID: {})", 
            config.container_id, container_pid));

        // 1. Create veth pair
        self.create_veth_pair(&config.veth_host_name, &config.veth_container_name)?;
        
        // 2. Connect host veth to bridge
        self.connect_veth_to_bridge(&config.veth_host_name)?;
        
        // 3. Move container veth to container's network namespace
        self.move_veth_to_container(&config.veth_container_name, container_pid)?;
        
        // 4. Configure container's network interface
        self.configure_container_interface(config, container_pid)?;
        
        ConsoleLogger::success(&format!("Network configured for container {} at {}", 
            config.container_id, config.ip_address));
        Ok(())
    }

    /// Clean up network resources for a container
    pub fn cleanup_container_network(&self, config: &ContainerNetworkConfig) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Cleaning up network for container {}", config.container_id));
        
        // Remove host veth (this automatically removes the pair)
        if let Err(e) = CommandExecutor::execute_shell(&format!("ip link delete {}", config.veth_host_name)) {
            ConsoleLogger::warning(&format!("Failed to delete veth pair: {}", e));
        }
        
        ConsoleLogger::success(&format!("Network cleaned up for container {}", config.container_id));
        Ok(())
    }

    // Private implementation methods
    
    fn bridge_exists(&self) -> bool {
        CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name)).is_ok()
    }
    
    fn create_bridge(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Creating bridge: {}", self.config.bridge_name));
        CommandExecutor::execute_shell(&format!("ip link add name {} type bridge", self.config.bridge_name))?;
        Ok(())
    }
    
    fn configure_bridge_ip(&self) -> Result<(), String> {
        let bridge_cidr = format!("{}/{}", self.config.bridge_ip, 
            self.config.subnet_cidr.split('/').last().unwrap_or("16"));
        
        // Check if IP is already assigned
        let check_cmd = format!("ip addr show {} | grep {}", self.config.bridge_name, self.config.bridge_ip);
        if CommandExecutor::execute_shell(&check_cmd).is_ok() {
            return Ok(());
        }
        
        ConsoleLogger::debug(&format!("Assigning IP {} to bridge", bridge_cidr));
        CommandExecutor::execute_shell(&format!("ip addr add {} dev {}", bridge_cidr, self.config.bridge_name))?;
        Ok(())
    }
    
    fn bring_bridge_up(&self) -> Result<(), String> {
        CommandExecutor::execute_shell(&format!("ip link set {} up", self.config.bridge_name))?;
        Ok(())
    }
    
    fn allocate_next_ip(&self) -> Result<String, String> {
        let mut next_ip = self.config.next_ip.lock()
            .map_err(|_| "Failed to lock IP allocation mutex")?;
        
        let ip_num = *next_ip;
        *next_ip += 1;
        
        // Generate IP: 10.42.0.x
        let ip = format!("10.42.0.{}", ip_num);
        Ok(ip)
    }
    
    fn create_veth_pair(&self, host_name: &str, container_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Creating veth pair: {} <-> {}", host_name, container_name));
        CommandExecutor::execute_shell(&format!("ip link add {} type veth peer name {}", 
            host_name, container_name))?;
        Ok(())
    }
    
    fn connect_veth_to_bridge(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Connecting {} to bridge {}", veth_name, self.config.bridge_name));
        CommandExecutor::execute_shell(&format!("ip link set {} master {}", veth_name, self.config.bridge_name))?;
        CommandExecutor::execute_shell(&format!("ip link set {} up", veth_name))?;
        Ok(())
    }
    
    fn move_veth_to_container(&self, veth_name: &str, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Moving {} to container PID {}", veth_name, container_pid));
        CommandExecutor::execute_shell(&format!("ip link set {} netns {}", veth_name, container_pid))?;
        Ok(())
    }
    
    fn configure_container_interface(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        let ip_with_mask = format!("{}/{}", config.ip_address, config.subnet_mask);
        
        // Execute commands in container's network namespace
        let ns_exec = format!("nsenter -t {} -n", container_pid);
        
        // Rename veth to eth0
        CommandExecutor::execute_shell(&format!("{} ip link set {} name eth0", 
            ns_exec, config.veth_container_name))?;
        
        // Assign IP address
        CommandExecutor::execute_shell(&format!("{} ip addr add {} dev eth0", 
            ns_exec, ip_with_mask))?;
        
        // Bring interface up
        CommandExecutor::execute_shell(&format!("{} ip link set eth0 up", ns_exec))?;
        
        // Set up loopback
        CommandExecutor::execute_shell(&format!("{} ip link set lo up", ns_exec))?;
        
        // Add default route
        CommandExecutor::execute_shell(&format!("{} ip route add default via {}", 
            ns_exec, config.gateway_ip))?;
        
        Ok(())
    }
} 