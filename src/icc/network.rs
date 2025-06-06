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
            next_ip: Arc::new(Mutex::new(2)),
        };
        
        Ok(Self { 
            config,
            bridge_initialized: Arc::new(Mutex::new(false)),
        })
    }

    pub fn ensure_bridge_ready(&self) -> Result<(), String> {
        let mut initialized = self.bridge_initialized.lock()
            .map_err(|_| "Failed to lock bridge initialization mutex")?;
        
        if *initialized {
            return Ok(());
        }

        ConsoleLogger::progress(&format!("Initializing network bridge: {}", self.config.bridge_name));
        
        if !self.bridge_exists() {
            self.create_bridge()?;
        }
        
        self.configure_bridge_ip()?;
        self.bring_bridge_up()?;
        
        *initialized = true;
        ConsoleLogger::success(&format!("Network bridge '{}' is ready", self.config.bridge_name));
        Ok(())
    }

    pub fn allocate_container_network(&self, container_id: &str) -> Result<ContainerNetworkConfig, String> {
        self.ensure_bridge_ready()?;
        
        let ip_address = self.allocate_next_ip()?;
        let veth_host_name = format!("veth-{}", &container_id[..8]);
        let veth_container_name = format!("vethc-{}", &container_id[..8]);
        
        ConsoleLogger::debug(&format!("Allocated IP {} for container {}", ip_address, container_id));
        
        Ok(ContainerNetworkConfig {
            ip_address,
            subnet_mask: "16".to_string(),
            gateway_ip: self.config.bridge_ip.clone(),
            container_id: container_id.to_string(),
            veth_host_name,
            veth_container_name,
        })
    }

    pub fn setup_container_network(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::progress(&format!("Setting up network for container {} (PID: {})", 
            config.container_id, container_pid));

        self.create_veth_pair(&config.veth_host_name, &config.veth_container_name)?;
        self.connect_veth_to_bridge(&config.veth_host_name)?;
        self.move_veth_to_container(&config.veth_container_name, container_pid)?;
        self.configure_container_interface(config, container_pid)?;
        
        ConsoleLogger::success(&format!("Network configured for container {} at {}", 
            config.container_id, config.ip_address));
        Ok(())
    }
    
    fn bridge_exists(&self) -> bool {
        CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name))
            .map_or(false, |r| r.success)
    }
    
    fn create_bridge(&self) -> Result<(), String> {
        let result = CommandExecutor::execute_shell(&format!("ip link add name {} type bridge", self.config.bridge_name))?;
        if !result.success {
            Err(format!("Failed to create bridge: {}", result.stderr))
        } else {
            Ok(())
        }
    }
    
    fn configure_bridge_ip(&self) -> Result<(), String> {
        let bridge_cidr = format!("{}/16", self.config.bridge_ip);
        let check_cmd = format!("ip addr show {} | grep {}", self.config.bridge_name, self.config.bridge_ip);
        if CommandExecutor::execute_shell(&check_cmd).map_or(false, |r| r.success) {
            return Ok(());
        }
        let result = CommandExecutor::execute_shell(&format!("ip addr add {} dev {}", bridge_cidr, self.config.bridge_name))?;
        if !result.success {
            Err(format!("Failed to assign IP to bridge: {}", result.stderr))
        } else {
            Ok(())
        }
    }
    
    fn bring_bridge_up(&self) -> Result<(), String> {
        let result = CommandExecutor::execute_shell(&format!("ip link set {} up", self.config.bridge_name))?;
        if !result.success {
            Err(format!("Failed to bring bridge up: {}", result.stderr))
        } else {
            Ok(())
        }
    }
    
    fn allocate_next_ip(&self) -> Result<String, String> {
        let mut next_ip_num = self.config.next_ip.lock().map_err(|e| e.to_string())?;
        let ip_num = *next_ip_num;
        *next_ip_num += 1;
        Ok(format!("10.42.0.{}", ip_num))
    }
    
    fn create_veth_pair(&self, host_name: &str, container_name: &str) -> Result<(), String> {
        let result = CommandExecutor::execute_shell(&format!("ip link add {} type veth peer name {}", host_name, container_name))?;
        if !result.success {
            Err(format!("Failed to create veth pair: {}", result.stderr))
        } else {
            Ok(())
        }
    }
    
    fn connect_veth_to_bridge(&self, veth_name: &str) -> Result<(), String> {
        let master_result = CommandExecutor::execute_shell(&format!("ip link set {} master {}", veth_name, self.config.bridge_name))?;
        if !master_result.success {
            return Err(format!("Failed to connect {} to bridge: {}", veth_name, master_result.stderr));
        }
        let up_result = CommandExecutor::execute_shell(&format!("ip link set {} up", veth_name))?;
        if !up_result.success {
            Err(format!("Failed to bring {} up: {}", veth_name, up_result.stderr))
        } else {
            Ok(())
        }
    }
    
    fn move_veth_to_container(&self, veth_name: &str, container_pid: i32) -> Result<(), String> {
        let result = CommandExecutor::execute_shell(&format!("ip link set {} netns {}", veth_name, container_pid))?;
        if !result.success {
            Err(format!("Failed to move {} to container: {}", veth_name, result.stderr))
        } else {
            Ok(())
        }
    }
    
    fn configure_container_interface(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        let ns_exec = format!("nsenter -t {} -n", container_pid);
        
        // Use consistent interface naming to avoid eth0 conflicts
        let interface_name = format!("quilt{}", &config.container_id[..8]);
        
        ConsoleLogger::debug(&format!("Configuring container interface for {}", config.container_id));
        
        // Rename the veth interface to our custom name
        let rename_result = CommandExecutor::execute_shell(&format!("{} ip link set {} name {}", ns_exec, config.veth_container_name, interface_name))?;
        if !rename_result.success {
            return Err(format!("Failed to rename veth to {}: {}", interface_name, rename_result.stderr));
        }

        // Assign IP address
        let ip_with_mask = format!("{}/{}", config.ip_address, config.subnet_mask);
        let ip_result = CommandExecutor::execute_shell(&format!("{} ip addr add {} dev {}", ns_exec, ip_with_mask, interface_name))?;
        if !ip_result.success {
            return Err(format!("Failed to assign IP: {}", ip_result.stderr));
        }

        // Bring interface up
        let up_result = CommandExecutor::execute_shell(&format!("{} ip link set {} up", ns_exec, interface_name))?;
        if !up_result.success {
            return Err(format!("Failed to bring {} up: {}", interface_name, up_result.stderr));
        }

        // Ensure loopback is up
        let lo_result = CommandExecutor::execute_shell(&format!("{} ip link set lo up", ns_exec))?;
        if !lo_result.success {
            ConsoleLogger::warning(&format!("Failed to bring loopback up: {}", lo_result.stderr));
        }

        // Add default route
        let route_result = CommandExecutor::execute_shell(&format!("{} ip route add default via {} dev {}", ns_exec, config.gateway_ip, interface_name))?;
        if !route_result.success {
            // Check if route already exists
            let route_check = CommandExecutor::execute_shell(&format!("{} ip route show default", ns_exec))?;
            if route_check.success && !route_check.stdout.trim().is_empty() {
                ConsoleLogger::debug("Default route already exists, skipping");
            } else {
                ConsoleLogger::warning(&format!("Failed to add default route: {}", route_result.stderr));
            }
        }
        
        ConsoleLogger::success(&format!("Container interface configured: {} = {}/{}", interface_name, config.ip_address, config.subnet_mask));
        Ok(())
    }
} 