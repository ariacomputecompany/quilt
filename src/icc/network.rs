// src/icc/network.rs
// Optimized Inter-Container Communication using Linux Bridge

use crate::utils::{CommandExecutor, ConsoleLogger};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

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
            ConsoleLogger::debug(&format!("Bridge {} already initialized", self.config.bridge_name));
            return Ok(());
        }

        ConsoleLogger::progress(&format!("Initializing network bridge: {}", self.config.bridge_name));
        
        // Check if bridge already exists and is properly configured
        if self.bridge_exists() {
            ConsoleLogger::info(&format!("Bridge {} already exists, checking configuration...", self.config.bridge_name));
            
            // Check if bridge has correct IP
            let ip_check = CommandExecutor::execute_shell(&format!("ip addr show {} | grep {}", 
                self.config.bridge_name, self.config.bridge_ip));
            let bridge_has_ip = ip_check.map_or(false, |r| r.success);
            
            // Check if bridge is up
            let status_result = CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name))?;
            let bridge_is_up = status_result.success && status_result.stdout.to_uppercase().contains("UP");
            
            if bridge_has_ip && bridge_is_up {
                ConsoleLogger::success(&format!("Bridge {} already properly configured, reusing it", self.config.bridge_name));
                *initialized = true;
                return Ok(());
            } else {
                ConsoleLogger::warning(&format!("Bridge {} exists but not properly configured, recreating...", self.config.bridge_name));
                let _cleanup = CommandExecutor::execute_shell(&format!("ip link delete {}", self.config.bridge_name));
            }
        }
        
        // Create the bridge
        ConsoleLogger::debug(&format!("Creating bridge: {}", self.config.bridge_name));
        self.create_bridge()?;
        
        // Verify bridge was created
        if !self.bridge_exists() {
            return Err(format!("Bridge {} was not created successfully", self.config.bridge_name));
        }
        ConsoleLogger::debug(&format!("✅ Bridge {} created successfully", self.config.bridge_name));
        
        // Configure bridge IP
        ConsoleLogger::debug(&format!("Configuring bridge IP: {}", self.config.bridge_ip));
        self.configure_bridge_ip()?;
        
        // Bring bridge up
        ConsoleLogger::debug(&format!("Bringing bridge {} up", self.config.bridge_name));
        self.bring_bridge_up()?;
        
        // Final verification
        if !self.bridge_exists() {
            return Err(format!("Bridge {} disappeared after configuration", self.config.bridge_name));
        }
        
        // Check if bridge is actually up
        let status_result = CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name))?;
        if !status_result.success {
            return Err(format!("Failed to check bridge {} status", self.config.bridge_name));
        }
        
        // Check if the output contains "UP" (either "state UP" or just "UP")
        if !status_result.stdout.to_uppercase().contains("UP") {
            ConsoleLogger::warning(&format!("Bridge {} may not be fully UP yet, but proceeding anyway", self.config.bridge_name));
            ConsoleLogger::debug(&format!("Bridge status: {}", status_result.stdout.trim()));
        }
        
        *initialized = true;
        ConsoleLogger::success(&format!("Network bridge '{}' is ready", self.config.bridge_name));
        Ok(())
    }

    pub fn allocate_container_network(&self, container_id: &str) -> Result<ContainerNetworkConfig, String> {
        // Bridge should already be ready from startup - no need to call ensure_bridge_ready() again
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
        let check_cmd = format!("ip link show {}", self.config.bridge_name);
        ConsoleLogger::debug(&format!("Checking bridge existence: {}", check_cmd));
        
        // Add namespace debugging
        ConsoleLogger::debug(&format!("🔍 Current PID: {}", std::process::id()));
        
        // Check current namespace context
        let ns_debug = CommandExecutor::execute_shell("ls -la /proc/self/ns/");
        match ns_debug {
            Ok(result) => ConsoleLogger::debug(&format!("🔍 Current namespaces: {}", result.stdout.replace('\n', " | "))),
            Err(e) => ConsoleLogger::debug(&format!("🔍 Failed to check namespaces: {}", e)),
        }
        
        // Check if we can see other bridges
        let all_bridges = CommandExecutor::execute_shell("ip link show type bridge");
        match all_bridges {
            Ok(result) => ConsoleLogger::debug(&format!("🔍 All bridges visible: {}", result.stdout.replace('\n', " | "))),
            Err(e) => ConsoleLogger::debug(&format!("🔍 Failed to list bridges: {}", e)),
        }
        
        // Try multiple times with different approaches due to potential timing issues during container creation
        for attempt in 1..=3 {
            match CommandExecutor::execute_shell(&check_cmd) {
                Ok(result) => {
                    // Check both success and stdout content, but be more forgiving
                    let exists = result.stdout.contains(&self.config.bridge_name);
                    if exists {
                        ConsoleLogger::debug(&format!("Bridge {} found on attempt {}", self.config.bridge_name, attempt));
                        return true;
                    }
                    
                    // If not found, check if it's a real error or just timing
                    if !result.success {
                        ConsoleLogger::debug(&format!("Bridge check failed on attempt {}: stderr: '{}'", 
                                                     attempt, result.stderr.trim()));
                        
                        // If it's a "does not exist" error, that's definitive
                        if result.stderr.contains("does not exist") {
                            ConsoleLogger::debug(&format!("Bridge {} definitively does not exist", self.config.bridge_name));
                            return false;
                        }
                    }
                }
                Err(e) => {
                    ConsoleLogger::debug(&format!("Bridge check error on attempt {}: {}", attempt, e));
                }
            }
            
            // Wait a bit before retrying (only for first 2 attempts)
            if attempt < 3 {
                thread::sleep(Duration::from_millis(50));
            }
        }
        
        // Final fallback: check if bridge appears in general link list
        ConsoleLogger::debug(&format!("Falling back to general link list check for {}", self.config.bridge_name));
        match CommandExecutor::execute_shell("ip link show") {
            Ok(result) => {
                let exists = result.stdout.contains(&self.config.bridge_name);
                ConsoleLogger::debug(&format!("Bridge {} exists via fallback check: {}", self.config.bridge_name, exists));
                exists
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Failed fallback bridge check: {}", e));
                false
            }
        }
    }
    
    fn create_bridge(&self) -> Result<(), String> {
        let create_cmd = format!("ip link add name {} type bridge", self.config.bridge_name);
        ConsoleLogger::debug(&format!("Executing: {}", create_cmd));
        
        let result = CommandExecutor::execute_shell(&create_cmd)?;
        if !result.success {
            let error_msg = format!("Failed to create bridge {}: stderr: '{}', stdout: '{}'", 
                                   self.config.bridge_name, result.stderr.trim(), result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        // Give the system a moment to create the bridge
        thread::sleep(Duration::from_millis(100));
        
        ConsoleLogger::debug(&format!("Bridge creation command successful for {}", self.config.bridge_name));
        Ok(())
    }
    
    fn configure_bridge_ip(&self) -> Result<(), String> {
        let bridge_cidr = format!("{}/16", self.config.bridge_ip);
        let check_cmd = format!("ip addr show {} | grep {}", self.config.bridge_name, self.config.bridge_ip);
        
        ConsoleLogger::debug(&format!("Checking if bridge IP already assigned: {}", check_cmd));
        if CommandExecutor::execute_shell(&check_cmd).map_or(false, |r| r.success) {
            ConsoleLogger::debug(&format!("Bridge {} already has IP {}", self.config.bridge_name, self.config.bridge_ip));
            return Ok(());
        }
        
        let assign_cmd = format!("ip addr add {} dev {}", bridge_cidr, self.config.bridge_name);
        ConsoleLogger::debug(&format!("Executing: {}", assign_cmd));
        
        let result = CommandExecutor::execute_shell(&assign_cmd)?;
        if !result.success {
            let error_msg = format!("Failed to assign IP {} to bridge {}: stderr: '{}', stdout: '{}'", 
                                   bridge_cidr, self.config.bridge_name, result.stderr.trim(), result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        ConsoleLogger::debug(&format!("Successfully assigned IP {} to bridge {}", bridge_cidr, self.config.bridge_name));
        Ok(())
    }
    
    fn bring_bridge_up(&self) -> Result<(), String> {
        let up_cmd = format!("ip link set {} up", self.config.bridge_name);
        ConsoleLogger::debug(&format!("Executing: {}", up_cmd));
        
        let result = CommandExecutor::execute_shell(&up_cmd)?;
        if !result.success {
            let error_msg = format!("Failed to bring bridge {} up: stderr: '{}', stdout: '{}'", 
                                   self.config.bridge_name, result.stderr.trim(), result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        // Give the system a moment to bring the interface up
        thread::sleep(Duration::from_millis(100));
        
        ConsoleLogger::debug(&format!("Successfully brought bridge {} up", self.config.bridge_name));
        Ok(())
    }
    
    fn allocate_next_ip(&self) -> Result<String, String> {
        let mut next_ip_num = self.config.next_ip.lock().map_err(|e| e.to_string())?;
        let ip_num = *next_ip_num;
        *next_ip_num += 1;
        Ok(format!("10.42.0.{}", ip_num))
    }
    
    fn create_veth_pair(&self, host_name: &str, container_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Creating veth pair: {} <-> {}", host_name, container_name));
        
        // First, clean up any existing interfaces with the same names
        let _cleanup_host = CommandExecutor::execute_shell(&format!("ip link delete {} 2>/dev/null", host_name));
        let _cleanup_container = CommandExecutor::execute_shell(&format!("ip link delete {} 2>/dev/null", container_name));
        
        // Create the veth pair
        let create_cmd = format!("ip link add {} type veth peer name {}", host_name, container_name);
        ConsoleLogger::debug(&format!("Executing: {}", create_cmd));
        
        let result = CommandExecutor::execute_shell(&create_cmd)?;
        if !result.success {
            let error_msg = format!("Failed to create veth pair {}<->{}: stderr: '{}', stdout: '{}'", 
                                   host_name, container_name, result.stderr.trim(), result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        // Give the system a moment to create both interfaces
        thread::sleep(Duration::from_millis(100));
        
        // Verify both sides of the veth pair were created
        let verify_host = CommandExecutor::execute_shell(&format!("ip link show {}", host_name))?;
        if !verify_host.success {
            return Err(format!("Host side veth interface {} was not created successfully", host_name));
        }
        
        let verify_container = CommandExecutor::execute_shell(&format!("ip link show {}", container_name))?;
        if !verify_container.success {
            return Err(format!("Container side veth interface {} was not created successfully", container_name));
        }
        
        ConsoleLogger::debug(&format!("Successfully created and verified veth pair: {} <-> {}", host_name, container_name));
        Ok(())
    }
    
    fn connect_veth_to_bridge(&self, veth_name: &str) -> Result<(), String> {
        // Verify bridge exists before trying to connect
        if !self.bridge_exists() {
            return Err(format!("Bridge {} does not exist when trying to connect {}", self.config.bridge_name, veth_name));
        }
        
        let master_cmd = format!("ip link set {} master {}", veth_name, self.config.bridge_name);
        ConsoleLogger::debug(&format!("Executing: {}", master_cmd));
        
        let master_result = CommandExecutor::execute_shell(&master_cmd)?;
        if !master_result.success {
            let error_msg = format!("Failed to connect {} to bridge {}: stderr: '{}', stdout: '{}'", 
                                   veth_name, self.config.bridge_name, master_result.stderr.trim(), master_result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        let up_cmd = format!("ip link set {} up", veth_name);
        ConsoleLogger::debug(&format!("Executing: {}", up_cmd));
        
        let up_result = CommandExecutor::execute_shell(&up_cmd)?;
        if !up_result.success {
            let error_msg = format!("Failed to bring {} up: stderr: '{}', stdout: '{}'", 
                                   veth_name, up_result.stderr.trim(), up_result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        ConsoleLogger::debug(&format!("Successfully connected {} to bridge {}", veth_name, self.config.bridge_name));
        Ok(())
    }
    
    fn move_veth_to_container(&self, veth_name: &str, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Moving veth interface {} to container PID {}", veth_name, container_pid));
        
        // First verify the veth interface exists
        let verify_result = CommandExecutor::execute_shell(&format!("ip link show {}", veth_name))?;
        if !verify_result.success {
            return Err(format!("Veth interface {} does not exist before move operation", veth_name));
        }
        
        let move_cmd = format!("ip link set {} netns {}", veth_name, container_pid);
        ConsoleLogger::debug(&format!("Executing: {}", move_cmd));
        
        let result = CommandExecutor::execute_shell(&move_cmd)?;
        if !result.success {
            let error_msg = format!("Failed to move {} to container {}: stderr: '{}', stdout: '{}'", 
                                   veth_name, container_pid, result.stderr.trim(), result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        ConsoleLogger::debug(&format!("Successfully moved {} to container {}", veth_name, container_pid));
        Ok(())
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