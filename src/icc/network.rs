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
        match CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name)) {
            Ok(result) => result.success,
            Err(_) => false,
        }
    }
    
    fn create_bridge(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Creating bridge: {}", self.config.bridge_name));
        let result = CommandExecutor::execute_shell(&format!("ip link add name {} type bridge", self.config.bridge_name))?;
        if !result.success {
            return Err(format!("Failed to create bridge: {}", result.stderr));
        }
        ConsoleLogger::success(&format!("Bridge {} created successfully", self.config.bridge_name));
        Ok(())
    }
    
    fn configure_bridge_ip(&self) -> Result<(), String> {
        let bridge_cidr = format!("{}/{}", self.config.bridge_ip, 
            self.config.subnet_cidr.split('/').last().unwrap_or("16"));
        
        // Check if IP is already assigned
        let check_cmd = format!("ip addr show {} | grep {}", self.config.bridge_name, self.config.bridge_ip);
        match CommandExecutor::execute_shell(&check_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug("Bridge IP already configured");
                return Ok(());
            }
            _ => {} // Continue to assign IP
        }
        
        ConsoleLogger::debug(&format!("Assigning IP {} to bridge", bridge_cidr));
        let result = CommandExecutor::execute_shell(&format!("ip addr add {} dev {}", bridge_cidr, self.config.bridge_name))?;
        if !result.success {
            return Err(format!("Failed to assign IP to bridge: {}", result.stderr));
        }
        ConsoleLogger::success(&format!("Bridge IP {} assigned successfully", bridge_cidr));
        Ok(())
    }
    
    fn bring_bridge_up(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Bringing bridge {} up", self.config.bridge_name));
        let result = CommandExecutor::execute_shell(&format!("ip link set {} up", self.config.bridge_name))?;
        if !result.success {
            return Err(format!("Failed to bring bridge up: {}", result.stderr));
        }
        ConsoleLogger::success(&format!("Bridge {} is now up", self.config.bridge_name));
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
        let result = CommandExecutor::execute_shell(&format!("ip link add {} type veth peer name {}", 
            host_name, container_name))?;
        if !result.success {
            return Err(format!("Failed to create veth pair: {}", result.stderr));
        }
        ConsoleLogger::success(&format!("Veth pair created: {} <-> {}", host_name, container_name));
        Ok(())
    }
    
    fn connect_veth_to_bridge(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Connecting {} to bridge {}", veth_name, self.config.bridge_name));
        
        // Set veth as master to bridge
        let master_result = CommandExecutor::execute_shell(&format!("ip link set {} master {}", veth_name, self.config.bridge_name))?;
        if !master_result.success {
            return Err(format!("Failed to connect {} to bridge: {}", veth_name, master_result.stderr));
        }
        
        // Bring veth up
        let up_result = CommandExecutor::execute_shell(&format!("ip link set {} up", veth_name))?;
        if !up_result.success {
            return Err(format!("Failed to bring {} up: {}", veth_name, up_result.stderr));
        }
        
        ConsoleLogger::success(&format!("Veth {} connected to bridge and brought up", veth_name));
        Ok(())
    }
    
    fn move_veth_to_container(&self, veth_name: &str, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Moving {} to container PID {}", veth_name, container_pid));
        let result = CommandExecutor::execute_shell(&format!("ip link set {} netns {}", veth_name, container_pid))?;
        if !result.success {
            return Err(format!("Failed to move {} to container: {}", veth_name, result.stderr));
        }
        ConsoleLogger::success(&format!("Veth {} moved to container PID {}", veth_name, container_pid));
        Ok(())
    }
    
    fn configure_container_interface(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Configuring container interface for {}", config.container_id));
        
        // Execute commands in container's network namespace
        let ns_exec = format!("nsenter -t {} -n", container_pid);
        
        // Check what interfaces exist in the container
        let interfaces_result = CommandExecutor::execute_shell(&format!("{} ip link show", ns_exec))?;
        if !interfaces_result.success {
            return Err(format!("Failed to list interfaces: {}", interfaces_result.stderr));
        }
        
        // Use standard eth0 interface name (Docker/Podman convention)
        let container_interface = "eth0";
        
        // Check if eth0 already exists and remove it if necessary
        let check_eth0_result = CommandExecutor::execute_shell(&format!("{} ip link show {}", ns_exec, container_interface));
        if let Ok(result) = check_eth0_result {
            if result.success {
                ConsoleLogger::debug("eth0 interface already exists, removing it first");
                // Try to delete the existing interface (may fail if it's essential)
                let delete_result = CommandExecutor::execute_shell(&format!("{} ip link delete {}", ns_exec, container_interface));
                if let Ok(del_res) = delete_result {
                    if !del_res.success {
                        ConsoleLogger::warning(&format!("Could not delete existing eth0: {}", del_res.stderr));
                        // If we can't delete eth0, use a unique name as fallback
                        let fallback_interface = format!("qnet{}", &config.container_id[..8]);
                        ConsoleLogger::debug(&format!("Using fallback interface name: {}", fallback_interface));
                        return self.configure_with_interface_name(config, container_pid, &fallback_interface, &ns_exec);
                    } else {
                        ConsoleLogger::debug("Successfully removed existing eth0");
                    }
                }
            }
        }
        
        // Rename veth to eth0
        let rename_result = CommandExecutor::execute_shell(&format!("{} ip link set {} name {}", 
            ns_exec, config.veth_container_name, container_interface))?;
        if !rename_result.success {
            // If rename to eth0 fails, try with unique name
            let fallback_interface = format!("qnet{}", &config.container_id[..8]);
            ConsoleLogger::warning(&format!("Failed to rename to eth0: {}, trying {}", rename_result.stderr, fallback_interface));
            return self.configure_with_interface_name(config, container_pid, &fallback_interface, &ns_exec);
        }
        
        self.configure_interface_details(config, container_pid, container_interface, &ns_exec)
    }

    // Helper method to configure interface with a specific name
    fn configure_with_interface_name(&self, config: &ContainerNetworkConfig, container_pid: i32, interface_name: &str, ns_exec: &str) -> Result<(), String> {
        // Rename veth to the specified interface name
        let rename_result = CommandExecutor::execute_shell(&format!("{} ip link set {} name {}", 
            ns_exec, config.veth_container_name, interface_name))?;
        if !rename_result.success {
            return Err(format!("Failed to rename interface to {}: {}", interface_name, rename_result.stderr));
        }
        
        self.configure_interface_details(config, container_pid, interface_name, ns_exec)
    }

    // Helper method to configure interface IP, routes, etc.
    fn configure_interface_details(&self, config: &ContainerNetworkConfig, container_pid: i32, interface_name: &str, ns_exec: &str) -> Result<(), String> {
        let ip_with_mask = format!("{}/{}", config.ip_address, config.subnet_mask);
        
        // Assign IP address
        let ip_result = CommandExecutor::execute_shell(&format!("{} ip addr add {} dev {}", 
            ns_exec, ip_with_mask, interface_name))?;
        if !ip_result.success {
            return Err(format!("Failed to assign IP: {}", ip_result.stderr));
        }
        
        // Bring interface up
        let up_result = CommandExecutor::execute_shell(&format!("{} ip link set {} up", ns_exec, interface_name))?;
        if !up_result.success {
            return Err(format!("Failed to bring {} up: {}", interface_name, up_result.stderr));
        }
        
        // Set up loopback
        let lo_result = CommandExecutor::execute_shell(&format!("{} ip link set lo up", ns_exec))?;
        if !lo_result.success {
            ConsoleLogger::warning(&format!("Failed to bring loopback up: {}", lo_result.stderr));
        }
        
        // Add default route (check if it already exists first)
        let route_check_result = CommandExecutor::execute_shell(&format!("{} ip route show default", ns_exec))?;
        if !route_check_result.success || route_check_result.stdout.trim().is_empty() {
            // No default route exists, add one
            let route_result = CommandExecutor::execute_shell(&format!("{} ip route add default via {}", 
                ns_exec, config.gateway_ip))?;
            if !route_result.success {
                ConsoleLogger::warning(&format!("Failed to add default route: {}", route_result.stderr));
            } else {
                ConsoleLogger::debug("Default route added successfully");
            }
        } else {
            ConsoleLogger::debug("Default route already exists, skipping");
        }
        
        ConsoleLogger::success(&format!("Container interface configured: {} = {}", interface_name, ip_with_mask));
        Ok(())
    }
} 