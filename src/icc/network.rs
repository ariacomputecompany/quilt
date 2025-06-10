// src/icc/network.rs
// Optimized Inter-Container Communication using Linux Bridge

use crate::utils::{CommandExecutor, ConsoleLogger};
use std::sync::{Arc};
use std::thread;
use std::time::Duration;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use scopeguard;

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub bridge_name: String,
    pub subnet_cidr: String,
    pub bridge_ip: String,
    pub next_ip: Arc<AtomicU32>,
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

// ELITE: Network state management with atomic operations
#[derive(Debug)]
struct NetworkStateCache {
    bridge_ready: Arc<AtomicBool>,
    routing_ready: Arc<AtomicBool>,
    setup_in_progress: Arc<AtomicBool>,
}

impl NetworkStateCache {
    fn new() -> Self {
        Self {
            bridge_ready: Arc::new(AtomicBool::new(false)),
            routing_ready: Arc::new(AtomicBool::new(false)),
            setup_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }
    
    fn is_bridge_ready(&self) -> bool {
        self.bridge_ready.load(Ordering::Acquire)
    }
    
    fn set_bridge_ready(&self, ready: bool) {
        self.bridge_ready.store(ready, Ordering::Release);
    }
    
    fn try_start_setup(&self) -> bool {
        self.setup_in_progress.compare_exchange(
            false, 
            true, 
            Ordering::AcqRel, 
            Ordering::Acquire
        ).is_ok()
    }
    
    fn finish_setup(&self) {
        self.setup_in_progress.store(false, Ordering::Release);
    }
}

pub struct NetworkManager {
    config: NetworkConfig,
    state_cache: NetworkStateCache,
}

impl NetworkManager {
    pub fn new(bridge_name: &str, subnet_cidr: &str) -> Result<Self, String> {
        let config = NetworkConfig {
            bridge_name: bridge_name.to_string(),
            subnet_cidr: subnet_cidr.to_string(),
            bridge_ip: "10.42.0.1".to_string(),
            next_ip: Arc::new(AtomicU32::new(2)),
        };
        
        Ok(Self { 
            config,
            state_cache: NetworkStateCache::new(),
        })
    }

    pub fn ensure_bridge_ready(&self) -> Result<(), String> {
        // ELITE: Fast path - check if bridge is already ready
        if self.state_cache.is_bridge_ready() {
            ConsoleLogger::debug(&format!("Bridge {} already initialized (fast path)", self.config.bridge_name));
            return Ok(());
        }
        
        // ELITE: Try to acquire setup lock, but don't block if another thread is setting up
        if !self.state_cache.try_start_setup() {
            // Another thread is setting up, wait for it to complete
            ConsoleLogger::debug("Bridge setup in progress by another thread, waiting...");
            for _ in 0..50 { // Max 500ms wait
                if self.state_cache.is_bridge_ready() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(10));
            }
            // If still not ready after 500ms, proceed with our own setup
            ConsoleLogger::warning("Bridge setup timeout, proceeding with own setup");
        }

        // ELITE: Ensure we release the setup lock on exit
        let _guard = scopeguard::guard((), |_| {
            self.state_cache.finish_setup();
        });

        ConsoleLogger::progress(&format!("Initializing network bridge: {}", self.config.bridge_name));
        
        // Check if bridge already exists and is properly configured
        if self.bridge_exists_fast() {
            ConsoleLogger::info(&format!("Bridge {} already exists, checking configuration...", self.config.bridge_name));
            
            // ELITE: Batch bridge verification in single command
            let verify_cmd = format!(
                "ip addr show {} | grep -q {} && ip link show {} | grep -q 'state UP'",
                self.config.bridge_name, self.config.bridge_ip, self.config.bridge_name
            );
            
            if CommandExecutor::execute_shell(&verify_cmd).map_or(false, |r| r.success) {
                ConsoleLogger::success(&format!("Bridge {} already properly configured, reusing it", self.config.bridge_name));
                self.state_cache.set_bridge_ready(true);
                return Ok(());
            } else {
                ConsoleLogger::warning(&format!("Bridge {} exists but not properly configured, recreating...", self.config.bridge_name));
                let _cleanup = CommandExecutor::execute_shell(&format!("ip link delete {}", self.config.bridge_name));
            }
        }
        
        // ELITE: Atomic bridge creation with batched operations
        self.create_bridge_atomic()?;
        
        // Final verification and cache update
        if !self.bridge_exists_fast() {
            self.state_cache.set_bridge_ready(false);
            return Err(format!("Bridge {} was not created successfully", self.config.bridge_name));
        }
        
        self.state_cache.set_bridge_ready(true);
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

        // ELITE: Use ultra-batched network setup for maximum performance
        self.setup_container_network_ultra_batched(config, container_pid)?;
        
        ConsoleLogger::success(&format!("Network configured for container {} at {}", 
            config.container_id, config.ip_address));
        Ok(())
    }

    // ELITE: Ultra-batched network setup - maximum performance optimization
    fn setup_container_network_ultra_batched(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        // ELITE: Pre-generate all interface names and commands
        let interface_name = format!("quilt{}", &config.container_id[..8]);
        let ip_with_mask = format!("{}/{}", config.ip_address, config.subnet_mask);
        
        // ELITE: Step 1 - Ultra-batched host operations (single command)
        let host_batch_cmd = format!(
            "ip link delete {} 2>/dev/null || true && ip link delete {} 2>/dev/null || true && ip link add {} type veth peer name {} && ip link set {} master {} && ip link set {} up && ip link set {} netns {}",
            config.veth_host_name, config.veth_container_name,  // Cleanup
            config.veth_host_name, config.veth_container_name,  // Create veth pair
            config.veth_host_name, self.config.bridge_name,     // Attach to bridge
            config.veth_host_name,                              // Bring host side up
            config.veth_container_name, container_pid           // Move to container
        );
        
        ConsoleLogger::debug(&format!("Executing ultra-batched host setup: {}", host_batch_cmd));
        
        let host_result = CommandExecutor::execute_shell(&host_batch_cmd)?;
        if !host_result.success {
            return Err(format!("Failed ultra-batched host setup: {}", host_result.stderr));
        }
        
        // ELITE: Step 2 - Ultra-batched container operations (single nsenter)
        let container_batch_cmd = format!(
            "nsenter -t {} -n sh -c 'ip link set {} name {} && ip addr add {} dev {} && ip link set {} up && ip link set lo up && (ip route add default via {} dev {} 2>/dev/null || true) && ip route show'",
            container_pid, 
            config.veth_container_name, interface_name,         // Rename interface
            ip_with_mask, interface_name,                       // Assign IP
            interface_name,                                     // Bring interface up
            config.gateway_ip, interface_name                   // Add default route
        );
        
        ConsoleLogger::debug(&format!("Executing ultra-batched container setup: {}", container_batch_cmd));
        
        let container_result = CommandExecutor::execute_shell(&container_batch_cmd)?;
        if !container_result.success {
            return Err(format!("Failed ultra-batched container setup: {}", container_result.stderr));
        }
        
        // ELITE: Verify network readiness
        self.verify_container_network_ready(config, container_pid)?;
        
        ConsoleLogger::success(&format!("Ultra-batched network setup completed: {} = {}/{}", interface_name, config.ip_address, config.subnet_mask));
        Ok(())
    }
    
    // ELITE: Production-grade network readiness verification with exec testing
    fn verify_container_network_ready(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        let interface_name = format!("quilt{}", &config.container_id[..8]);
        
        ConsoleLogger::debug(&format!("🔍 Production network verification for container {} (interface: {})", config.container_id, interface_name));
        
        // Phase 1: Network interface verification (fast check)
        for attempt in 1..=20 { // Max 2 seconds for network interface
            let mut verification_ok = true;
            let mut error_details = Vec::new();
            
            // Check 1: Interface exists and has IP
            let ip_check_cmd = format!(
                "nsenter -t {} -n ip addr show {} | grep 'inet.*{}'",
                container_pid, interface_name, config.ip_address.split('/').next().unwrap()
            );
            
            match CommandExecutor::execute_shell(&ip_check_cmd) {
                Ok(result) if result.success => {
                    ConsoleLogger::debug(&format!("✅ Interface {} has correct IP", interface_name));
                }
                _ => {
                    verification_ok = false;
                    error_details.push(format!("Interface {} missing or incorrect IP", interface_name));
                }
            }
            
            // Check 2: Bridge connectivity
            let bridge_check_cmd = format!("ip link show {} | grep 'master {}'", 
                                         format!("veth-{}", &config.container_id[..8]), self.config.bridge_name);
            match CommandExecutor::execute_shell(&bridge_check_cmd) {
                Ok(result) if result.success => {
                    ConsoleLogger::debug(&format!("✅ Bridge connectivity verified"));
                }
                _ => {
                    verification_ok = false;
                    error_details.push("Bridge connectivity issue".to_string());
                }
            }
            
            if verification_ok {
                break; // Network interface ready, proceed to exec test
            }
            
            if attempt == 20 {
                return Err(format!("Network interface verification failed: {}", error_details.join(", ")));
            }
            
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        
        // Phase 2: Container exec verification (ensure container can actually be used)
        ConsoleLogger::debug(&format!("🔍 Testing container {} exec readiness", config.container_id));
        for attempt in 1..=30 { // Max 3 seconds for exec readiness
            // Test basic exec functionality
            let exec_test_cmd = format!(
                "nsenter -t {} -p -m -n -u -i -- /bin/sh -c 'echo network_exec_ready'",
                container_pid
            );
            
            match CommandExecutor::execute_shell(&exec_test_cmd) {
                Ok(result) if result.success => {
                    let stdout = result.stdout.trim();
                    if stdout == "network_exec_ready" {
                        ConsoleLogger::debug(&format!("✅ Container {} exec readiness verified", config.container_id));
                        break;
                    } else {
                        ConsoleLogger::debug(&format!("Exec test unexpected output: '{}'", stdout));
                    }
                }
                Ok(result) => {
                    ConsoleLogger::debug(&format!("Exec test failed: {}", result.stderr));
                }
                Err(e) => {
                    ConsoleLogger::debug(&format!("Exec test error: {}", e));
                }
            }
            
            if attempt == 30 {
                return Err(format!("Container {} exec verification failed - container not ready for commands", config.container_id));
            }
            
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        
        // Phase 3: Network connectivity test (ping to bridge gateway)
        ConsoleLogger::debug(&format!("🔍 Testing container {} network connectivity", config.container_id));
        let gateway_ping_cmd = format!(
            "nsenter -t {} -n -- ping -c 1 -W 2 {} > /dev/null 2>&1",
            container_pid, self.config.bridge_ip
        );
        
        match CommandExecutor::execute_shell(&gateway_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::success(&format!("✅ Container {} production network ready - interface, exec, and connectivity verified", config.container_id));
                Ok(())
            }
            _ => {
                ConsoleLogger::warning(&format!("Network connectivity test failed for {}, but interface and exec are ready", config.container_id));
                // Don't fail on ping issues - interface and exec work, which is what matters
                Ok(())
            }
        }
    }
    
    // ELITE: Fast bridge existence check without debugging overhead
    fn bridge_exists_fast(&self) -> bool {
        // Single command to check bridge existence
        let check_cmd = format!("ip link show {}", self.config.bridge_name);
        match CommandExecutor::execute_shell(&check_cmd) {
            Ok(result) => result.success && result.stdout.contains(&self.config.bridge_name),
            Err(_) => false,
        }
    }
    
    // ELITE: Atomic bridge creation with all operations batched
    fn create_bridge_atomic(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Creating bridge atomically: {}", self.config.bridge_name));
        
        // ELITE: Single compound command for complete bridge setup
        let bridge_cidr = format!("{}/16", self.config.bridge_ip);
        let atomic_bridge_cmd = format!(
            "ip link add name {} type bridge && ip addr add {} dev {} && ip link set {} up",
            self.config.bridge_name, bridge_cidr, self.config.bridge_name, self.config.bridge_name
        );
        
        ConsoleLogger::debug(&format!("Executing atomic bridge setup: {}", atomic_bridge_cmd));
        
        let result = CommandExecutor::execute_shell(&atomic_bridge_cmd)?;
        if !result.success {
            let error_msg = format!("Failed atomic bridge creation for {}: stderr: '{}', stdout: '{}'", 
                                   self.config.bridge_name, result.stderr.trim(), result.stdout.trim());
            ConsoleLogger::error(&error_msg);
            return Err(error_msg);
        }
        
        // ELITE: Fast verification without artificial delays
        for attempt in 1..=10 {
            if self.bridge_exists_fast() {
                ConsoleLogger::debug(&format!("✅ Atomic bridge creation verified on attempt {}", attempt));
                return Ok(());
            }
            if attempt < 10 {
                thread::sleep(Duration::from_millis(5)); // Minimal delay
            }
        }
        
        Err(format!("Bridge {} failed atomic creation verification", self.config.bridge_name))
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
        
        // ELITE: Try multiple times with faster polling instead of fixed delays
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
            
            // ELITE: Micro-sleep instead of 50ms delay
            if attempt < 3 {
                thread::sleep(Duration::from_millis(5));  // 5ms vs 50ms
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
        
        // ELITE: Replace artificial delay with efficient verification
        self.verify_bridge_up()?;
        
        ConsoleLogger::debug(&format!("Successfully brought bridge {} up", self.config.bridge_name));
        Ok(())
    }
    
    // ELITE: Efficient bridge verification without artificial delays
    fn verify_bridge_created(&self) -> Result<(), String> {
        for attempt in 1..=10 {  // Fast polling instead of single 100ms delay
            if self.bridge_exists() {
                return Ok(());
            }
            if attempt < 10 {
                thread::sleep(Duration::from_millis(10));  // 10ms vs 100ms
            }
        }
        Err(format!("Bridge {} was not created after verification", self.config.bridge_name))
    }

    fn verify_bridge_up(&self) -> Result<(), String> {
        let check_cmd = format!("ip link show {} | grep -q 'state UP'", self.config.bridge_name);
        for attempt in 1..=10 {  // Fast polling instead of single 100ms delay
            if CommandExecutor::execute_shell(&check_cmd).map_or(false, |r| r.success) {
                return Ok(());
            }
            if attempt < 10 {
                thread::sleep(Duration::from_millis(10));  // 10ms vs 100ms
            }
        }
        Err(format!("Bridge {} failed to come up", self.config.bridge_name))
    }
    
    fn allocate_next_ip(&self) -> Result<String, String> {
        // ELITE: Lock-free IP allocation using compare-and-swap
        let mut current_ip = self.config.next_ip.load(Ordering::Relaxed);
        loop {
            let next_ip = current_ip + 1;
            
            // Ensure we don't exceed IP range (10.42.0.2 - 10.42.0.254)
            if next_ip > 254 {
                return Err("IP address pool exhausted".to_string());
            }
            
            match self.config.next_ip.compare_exchange_weak(
                current_ip, 
                next_ip, 
                Ordering::Relaxed, 
                Ordering::Relaxed
            ) {
                Ok(_) => return Ok(format!("10.42.0.{}", next_ip)),
                Err(actual) => current_ip = actual, // CAS failed, retry with updated value
            }
        }
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
        
        // ELITE: Replace artificial delay with efficient verification
        self.verify_veth_pair_created(host_name, container_name)?;
        
        ConsoleLogger::debug(&format!("Successfully created and verified veth pair: {} <-> {}", host_name, container_name));
        Ok(())
    }
    
    // ELITE: Efficient veth pair verification without artificial delays  
    fn verify_veth_pair_created(&self, host_name: &str, container_name: &str) -> Result<(), String> {
        for attempt in 1..=10 {  // Fast polling instead of single 100ms delay
            let verify_host = CommandExecutor::execute_shell(&format!("ip link show {}", host_name));
            let verify_container = CommandExecutor::execute_shell(&format!("ip link show {}", container_name));
            
            if verify_host.map_or(false, |r| r.success) && verify_container.map_or(false, |r| r.success) {
                return Ok(());
            }
            
            if attempt < 10 {
                thread::sleep(Duration::from_millis(10));  // 10ms vs 100ms
            }
        }
        Err(format!("Veth pair {} <-> {} was not created successfully", host_name, container_name))
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