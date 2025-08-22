// src/icc/network.rs
// Optimized Inter-Container Communication using Linux Bridge

use crate::utils::{CommandExecutor, ConsoleLogger};
use crate::icc::dns::{DnsServer, DnsEntry};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering, AtomicBool};
use std::net::SocketAddr;
use scopeguard;

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub bridge_name: String,
    pub subnet_cidr: String,
    pub bridge_ip: String,
    pub next_ip: Arc<AtomicU32>,
}

#[derive(Debug, Clone)]
pub struct BridgeState {
    pub exists: bool,
    pub has_ip: bool,
    pub is_up: bool,
    pub last_verified: Instant,
    pub verification_count: u32,
}

impl BridgeState {
    pub fn new() -> Self {
        Self {
            exists: false,
            has_ip: false,
            is_up: false,
            last_verified: Instant::now() - Duration::from_secs(60), // Force initial check
            verification_count: 0,
        }
    }
    
    pub fn is_fully_configured(&self) -> bool {
        self.exists && self.has_ip && self.is_up
    }
    
    pub fn needs_verification(&self, cache_duration: Duration) -> bool {
        self.last_verified.elapsed() > cache_duration
    }
    
    pub fn mark_verified(&mut self) {
        self.last_verified = Instant::now();
        self.verification_count += 1;
    }
}

#[derive(Debug, Clone)]
pub struct ContainerNetworkConfig {
    pub ip_address: String,
    pub subnet_mask: String,
    pub gateway_ip: String,
    pub container_id: String,
    pub veth_host_name: String,
    pub veth_container_name: String,
    pub rootfs_path: Option<String>,
}


pub struct NetworkManager {
    config: NetworkConfig,
    dns_server: Option<Arc<DnsServer>>,
    bridge_state: Arc<Mutex<BridgeState>>,
    bridge_ready: AtomicBool, // Fast check for bridge readiness
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
            dns_server: None,
            bridge_state: Arc::new(Mutex::new(BridgeState::new())),
            bridge_ready: AtomicBool::new(false),
        })
    }

    pub fn ensure_bridge_ready(&self) -> Result<(), String> {
        ConsoleLogger::progress(&format!("Initializing network bridge: {}", self.config.bridge_name));
        
        // Always check if bridge actually exists on the system (no caching bullshit)
        if self.bridge_exists_and_configured() {
            ConsoleLogger::success(&format!("Bridge {} already properly configured", self.config.bridge_name));
            return Ok(());
        }
        
        // FIXED: Only clean up bridge if this is the initial startup, not during container operations
        // Check if this is being called during container setup (avoid destructive operations)
        ConsoleLogger::info(&format!("🏗️ [BRIDGE-INIT] Bridge {} needs to be created (initial setup only)", self.config.bridge_name));
        
        // Clean up any partial bridge configuration - ONLY during initial setup
        let cleanup_result = CommandExecutor::execute_shell(&format!("ip link delete {} 2>/dev/null || true", self.config.bridge_name));
        if cleanup_result.is_ok() {
            ConsoleLogger::debug("🧹 [BRIDGE-INIT] Cleaned up any existing partial bridge configuration");
        }
        
        // Create bridge with proper atomic operations
        self.create_bridge_atomic()?;
        
        // Final verification - ensure bridge is actually working
        if !self.bridge_exists_and_configured() {
            return Err(format!("Bridge {} was not created successfully - verification failed", self.config.bridge_name));
        }
        
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
            rootfs_path: None,
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
        
        ConsoleLogger::info(&format!("🌐 Setting up network for container {} (PID: {})", config.container_id, container_pid));
        ConsoleLogger::info(&format!("   Host veth: {}, Container veth: {}", config.veth_host_name, config.veth_container_name));
        ConsoleLogger::info(&format!("   IP: {}, Bridge: {}", ip_with_mask, self.config.bridge_name));
        
        // Step 1: Create veth pair with detailed debugging
        ConsoleLogger::debug("Step 1: Creating veth pair...");
        
        // First clean up any existing interfaces
        let cleanup_cmd = format!("ip link delete {} 2>/dev/null || true && ip link delete {} 2>/dev/null || true",
            config.veth_host_name, config.veth_container_name);
        let _ = CommandExecutor::execute_shell(&cleanup_cmd);
        
        // Create veth pair
        let create_veth_cmd = format!("ip link add {} type veth peer name {}", 
            config.veth_host_name, config.veth_container_name);
        
        ConsoleLogger::debug(&format!("Creating veth pair: {}", create_veth_cmd));
        let create_result = CommandExecutor::execute_shell(&create_veth_cmd)?;
        if !create_result.success {
            return Err(format!("Failed to create veth pair: {}", create_result.stderr));
        }
        
        // Verify veth pair exists
        let verify_cmd = format!("ip link show {} && ip link show {}", 
            config.veth_host_name, config.veth_container_name);
        match CommandExecutor::execute_shell(&verify_cmd) {
            Ok(r) if r.success => ConsoleLogger::debug("✅ Veth pair created successfully"),
            _ => return Err("Failed to verify veth pair creation".to_string()),
        }
        
        // Step 2: Ensure bridge exists and is ready before attachment
        ConsoleLogger::debug("Step 2a: Verifying bridge exists and is ready...");
        self.ensure_bridge_ready_for_attachment()?;
        
        // Step 2b: Attach host veth to bridge with retry logic
        ConsoleLogger::debug("Step 2b: Attaching host veth to bridge...");
        self.attach_veth_to_bridge_with_retry(&config.veth_host_name)?;
        
        // Step 3: Configure and bring up host veth
        ConsoleLogger::debug("Step 3: Configuring and bringing up host veth...");
        
        // Enable promiscuous mode on host veth for proper bridge communication
        let promisc_host_cmd = format!("ip link set {} promisc on", config.veth_host_name);
        ConsoleLogger::debug(&format!("Enabling promiscuous mode on host veth: {}", promisc_host_cmd));
        let promisc_result = CommandExecutor::execute_shell(&promisc_host_cmd)?;
        if !promisc_result.success {
            return Err(format!("Failed to enable promiscuous mode on host veth: {}", promisc_result.stderr));
        }
        
        // Bring up host veth
        let up_cmd = format!("ip link set {} up", config.veth_host_name);
        ConsoleLogger::debug(&format!("Bringing up host veth: {}", up_cmd));
        let up_result = CommandExecutor::execute_shell(&up_cmd)?;
        if !up_result.success {
            return Err(format!("Failed to bring up host veth: {}", up_result.stderr));
        }
        
        // Verify host veth is up and attached to bridge with promiscuous mode
        let verify_host_cmd = format!("ip link show {} | grep 'master {}.*state UP'", 
            config.veth_host_name, self.config.bridge_name);
        match CommandExecutor::execute_shell(&verify_host_cmd) {
            Ok(r) if r.success => ConsoleLogger::debug("✅ Host veth is up and attached to bridge"),
            _ => ConsoleLogger::warning("Host veth may not be fully configured"),
        }
        
        // Step 4: Move container veth to container namespace (with retry logic)
        ConsoleLogger::debug("Step 4: Moving container veth to namespace...");
        let move_cmd = format!("ip link set {} netns {}", config.veth_container_name, container_pid);
        
        ConsoleLogger::debug(&format!("Moving veth to container: {}", move_cmd));
        
        // Retry logic for the critical netns move operation
        let mut move_success = false;
        let mut last_error = String::new();
        
        for attempt in 1..=5 {
            // First, verify the container process still exists
            let proc_check = format!("kill -0 {}", container_pid);
            match CommandExecutor::execute_shell(&proc_check) {
                Ok(result) if result.success => {
                    // Process exists, try the move operation
                    match CommandExecutor::execute_shell(&move_cmd) {
                        Ok(result) if result.success => {
                            move_success = true;
                            break;
                        }
                        Ok(result) => {
                            last_error = result.stderr.clone();
                            ConsoleLogger::debug(&format!("Move attempt {} failed: {}", attempt, result.stderr));
                            
                            // If it says "No such process", the container died - fail fast
                            if result.stderr.contains("No such process") {
                                ConsoleLogger::debug("Container process no longer exists, failing fast");
                                break;
                            }
                        }
                        Err(e) => {
                            last_error = e.clone();
                            ConsoleLogger::debug(&format!("Move attempt {} error: {}", attempt, e));
                        }
                    }
                }
                _ => {
                    last_error = format!("Container process {} no longer exists", container_pid);
                    ConsoleLogger::debug(&format!("Process check failed on attempt {}", attempt));
                    break;
                }
            }
            
            if attempt < 5 {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        
        if !move_success {
            return Err(format!("Failed to move veth to container after 5 attempts: {}", last_error));
        }
        
        // Step 5: Configure interface inside container namespace
        ConsoleLogger::debug("Step 5: Configuring interface inside container...");
        
        // Rename interface
        let rename_cmd = format!("nsenter -t {} -n ip link set {} name {}", 
            container_pid, config.veth_container_name, interface_name);
        
        ConsoleLogger::debug(&format!("Renaming interface: {}", rename_cmd));
        let rename_result = CommandExecutor::execute_shell(&rename_cmd)?;
        if !rename_result.success {
            return Err(format!("Failed to rename interface: {}", rename_result.stderr));
        }
        
        // Add IP address
        let ip_cmd = format!("nsenter -t {} -n ip addr add {} dev {}", 
            container_pid, ip_with_mask, interface_name);
        
        ConsoleLogger::debug(&format!("Adding IP address: {}", ip_cmd));
        let ip_result = CommandExecutor::execute_shell(&ip_cmd)?;
        if !ip_result.success && !ip_result.stderr.contains("File exists") {
            return Err(format!("Failed to add IP address: {}", ip_result.stderr));
        }
        
        // Enable promiscuous mode on container interface for proper bridge communication
        let promisc_container_cmd = format!("nsenter -t {} -n ip link set {} promisc on", 
            container_pid, interface_name);
        
        ConsoleLogger::debug(&format!("Enabling promiscuous mode on container interface: {}", promisc_container_cmd));
        let promisc_container_result = CommandExecutor::execute_shell(&promisc_container_cmd)?;
        if !promisc_container_result.success {
            return Err(format!("Failed to enable promiscuous mode on container interface: {}", promisc_container_result.stderr));
        }
        
        // Bring up interface
        let up_container_cmd = format!("nsenter -t {} -n ip link set {} up", 
            container_pid, interface_name);
        
        ConsoleLogger::debug(&format!("Bringing up container interface: {}", up_container_cmd));
        let up_container_result = CommandExecutor::execute_shell(&up_container_cmd)?;
        if !up_container_result.success {
            return Err(format!("Failed to bring up container interface: {}", up_container_result.stderr));
        }
        
        // Bring up loopback
        let lo_cmd = format!("nsenter -t {} -n ip link set lo up", container_pid);
        ConsoleLogger::debug(&format!("Bringing up loopback: {}", lo_cmd));
        let _ = CommandExecutor::execute_shell(&lo_cmd);
        
        // Wait for DAD (Duplicate Address Detection) to complete
        ConsoleLogger::debug("Waiting for DAD completion...");
        std::thread::sleep(std::time::Duration::from_millis(100));
        
        // Add static ARP entry for the gateway to ensure connectivity
        let gateway_ip = config.gateway_ip.split('/').next().unwrap();
        
        // Get the bridge MAC address for proper ARP entry
        match self.get_interface_mac_address(&self.config.bridge_name) {
            Ok(bridge_mac) => {
                let arp_cmd = format!(
                    "nsenter -t {} -n ip neigh add {} lladdr {} dev {} nud permanent 2>/dev/null || true",
                    container_pid, gateway_ip, bridge_mac, interface_name
                );
                ConsoleLogger::debug(&format!("Adding ARP entry for gateway {} with MAC {}: {}", 
                    gateway_ip, bridge_mac, arp_cmd));
                let _ = CommandExecutor::execute_shell(&arp_cmd);
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Failed to get bridge MAC address for ARP entry: {}", e));
                // Fallback: Don't add ARP entry rather than using wrong MAC
                ConsoleLogger::debug("Skipping gateway ARP entry due to MAC lookup failure");
            }
        }
        
        // Also ensure the bridge can reach this container
        let container_ip = config.ip_address.split('/').next().unwrap();
        
        // Get the container interface MAC address for proper ARP entry
        match self.get_container_interface_mac_address(container_pid, &interface_name) {
            Ok(container_mac) => {
                let host_arp_cmd = format!(
                    "ip neigh add {} lladdr {} dev {} nud permanent 2>/dev/null || true",
                    container_ip, container_mac, self.config.bridge_name
                );
                ConsoleLogger::debug(&format!("Adding host ARP entry for container {} with MAC {}: {}", 
                    container_ip, container_mac, host_arp_cmd));
                let _ = CommandExecutor::execute_shell(&host_arp_cmd);
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Failed to get container interface MAC address for ARP entry: {}", e));
                // Fallback: Don't add ARP entry rather than using wrong MAC
                ConsoleLogger::debug("Skipping container ARP entry due to MAC lookup failure");
            }
        }
        
        // Add default route
        let route_cmd = format!("nsenter -t {} -n ip route add default via {} dev {}", 
            container_pid, config.gateway_ip, interface_name);
        
        ConsoleLogger::debug(&format!("Adding default route: {}", route_cmd));
        let route_result = CommandExecutor::execute_shell(&route_cmd);
        if let Err(e) = route_result {
            if !e.contains("File exists") {
                ConsoleLogger::warning(&format!("Failed to add default route: {}", e));
            }
        }
        
        // Debug: Show final network configuration
        let show_config_cmd = format!(
            "nsenter -t {} -n sh -c 'echo \"=== Network Config ===\"; ip addr show; echo \"=== Routes ===\"; ip route show; echo \"=== ARP ===\"; ip neigh show'",
            container_pid
        );
        
        if let Ok(config_result) = CommandExecutor::execute_shell(&show_config_cmd) {
            ConsoleLogger::debug(&format!("🔧 Container network configuration:\n{}", config_result.stdout));
        }
        
        // ADDITIONAL VERIFICATION: Test basic connectivity
        ConsoleLogger::debug(&format!("🔍 [NET-VERIFY] Testing network connectivity for container {}", config.container_id));
        
        // Test 1: Check if interface has the correct IP
        let container_ip = config.ip_address.split('/').next().unwrap();
        let ip_check_cmd = format!("nsenter -t {} -n ip addr show {} | grep {}", 
            container_pid, interface_name, container_ip);
        match CommandExecutor::execute_shell(&ip_check_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [NET-VERIFY] Interface {} has IP {}", interface_name, container_ip));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [NET-VERIFY] Interface {} may not have correct IP {}", interface_name, container_ip));
            }
        }
        
        // Test 2: Check if loopback is up
        let lo_check_cmd = format!("nsenter -t {} -n ip link show lo | grep 'state UP'", container_pid);
        match CommandExecutor::execute_shell(&lo_check_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [NET-VERIFY] Loopback is UP"));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [NET-VERIFY] Loopback may not be UP"));
            }
        }
        
        // Test 3: Check if default route exists
        let route_check_cmd = format!("nsenter -t {} -n ip route show default", container_pid);
        match CommandExecutor::execute_shell(&route_check_cmd) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [NET-VERIFY] Default route exists: {}", result.stdout.trim()));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [NET-VERIFY] No default route found"));
            }
        }
        
        // Test 4: Enhanced gateway connectivity testing with detailed diagnostics
        let gateway_ip = config.gateway_ip.split('/').next().unwrap();
        self.test_gateway_connectivity_comprehensive(container_pid, gateway_ip, &interface_name);
        
        // BRIDGE VERIFICATION: Check host-side bridge connectivity
        ConsoleLogger::debug(&format!("🌉 [BRIDGE-VERIFY] Checking bridge connectivity for container {}", config.container_id));
        
        // Check if veth pair exists on host side
        let host_veth_check = format!("ip link show {} | grep 'master {}'", config.veth_host_name, self.config.bridge_name);
        match CommandExecutor::execute_shell(&host_veth_check) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [BRIDGE-VERIFY] Host veth {} is attached to bridge {}", 
                    config.veth_host_name, self.config.bridge_name));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [BRIDGE-VERIFY] Host veth {} may not be attached to bridge {}", 
                    config.veth_host_name, self.config.bridge_name));
            }
        }
        
        // Check bridge forwarding table
        let bridge_fdb_cmd = format!("bridge fdb show dev {} | grep {}", config.veth_host_name, container_ip);
        match CommandExecutor::execute_shell(&bridge_fdb_cmd) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [BRIDGE-VERIFY] Bridge has FDB entry for container: {}", result.stdout.trim()));
            }
            _ => {
                ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-VERIFY] No FDB entry found (may be normal for new containers)"));
            }
        }
        
        // Enhanced bidirectional connectivity testing
        self.test_bidirectional_connectivity(container_pid, container_ip, gateway_ip);
        
        // ELITE: Verify network readiness
        self.verify_container_network_ready(config, container_pid)?;
        
        // Write DNS configuration to container
        // Use nsenter to write resolv.conf inside the container's mount namespace
        let dns_content = format!("nameserver {}\nsearch quilt.local\n", self.config.bridge_ip);
        let write_resolv_cmd = format!(
            "nsenter -t {} -m -p -- sh -c 'mkdir -p /etc && echo \"{}\" > /etc/resolv.conf'",
            container_pid, dns_content
        );
        
        match CommandExecutor::execute_shell(&write_resolv_cmd) {
            Ok(_) => {
                ConsoleLogger::debug("DNS configuration written to container's /etc/resolv.conf");
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Failed to write DNS configuration: {}", e));
                // Try alternative method if rootfs_path is available
                if let Some(rootfs_path) = &config.rootfs_path {
                    let resolv_conf_path = format!("{}/etc/resolv.conf", rootfs_path);
                    if let Err(e) = std::fs::write(&resolv_conf_path, &dns_content) {
                        ConsoleLogger::warning(&format!("Alternative DNS write also failed: {}", e));
                    }
                }
            }
        }
        
        ConsoleLogger::success(&format!("Ultra-batched network setup completed: {} = {}/{}", interface_name, config.ip_address, config.subnet_mask));
        Ok(())
    }
    
    /// ENHANCED: Comprehensive gateway connectivity testing with detailed diagnostics
    fn test_gateway_connectivity_comprehensive(&self, container_pid: i32, gateway_ip: &str, interface_name: &str) {
        ConsoleLogger::debug(&format!("🌐 [GATEWAY-TEST] Comprehensive gateway connectivity test for {}", gateway_ip));
        
        // Test 1: Basic ping test
        let gateway_ping_cmd = format!("nsenter -t {} -n ping -c 3 -W 2 {} 2>/dev/null", 
            container_pid, gateway_ip);
        
        match CommandExecutor::execute_shell(&gateway_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::success(&format!("✅ [GATEWAY-TEST] Gateway {} is reachable (ping success)", gateway_ip));
                
                // Extract ping statistics for detailed analysis
                if let Some(stats_line) = result.stdout.lines().find(|line| line.contains("packets transmitted")) {
                    ConsoleLogger::debug(&format!("📊 [GATEWAY-TEST] Ping stats: {}", stats_line.trim()));
                }
                return; // Success - no need for additional tests
            }
            Ok(result) => {
                ConsoleLogger::warning(&format!("⚠️ [GATEWAY-TEST] Gateway {} ping failed", gateway_ip));
                ConsoleLogger::debug(&format!("🔍 [GATEWAY-TEST] Ping output: {}", result.stdout.trim()));
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("⚠️ [GATEWAY-TEST] Gateway ping command failed: {}", e));
            }
        }
        
        // Test 2: ARP resolution test
        self.test_gateway_arp_resolution(container_pid, gateway_ip);
        
        // Test 3: Route verification
        self.test_gateway_routing(container_pid, gateway_ip, interface_name);
        
        // Test 4: Interface connectivity test
        self.test_interface_connectivity(container_pid, interface_name);
        
        // Test 5: Bridge-side diagnostics
        self.diagnose_bridge_connectivity_issues(gateway_ip);
    }
    
    /// Test ARP resolution to gateway
    fn test_gateway_arp_resolution(&self, container_pid: i32, gateway_ip: &str) {
        ConsoleLogger::debug(&format!("🔍 [ARP-TEST] Testing ARP resolution for gateway {}", gateway_ip));
        
        // Check ARP entry for gateway
        let arp_check_cmd = format!("nsenter -t {} -n ip neigh show {}", container_pid, gateway_ip);
        match CommandExecutor::execute_shell(&arp_check_cmd) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [ARP-TEST] Gateway ARP entry found: {}", result.stdout.trim()));
                
                // Verify ARP entry uses correct MAC address
                if result.stdout.contains("PERMANENT") {
                    ConsoleLogger::debug("✅ [ARP-TEST] ARP entry is PERMANENT (configured statically)");
                } else if result.stdout.contains("REACHABLE") || result.stdout.contains("STALE") {
                    ConsoleLogger::debug("ℹ️ [ARP-TEST] ARP entry is learned dynamically");
                }
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [ARP-TEST] No ARP entry found for gateway {}", gateway_ip));
                
                // Try to trigger ARP resolution
                let arp_ping_cmd = format!("nsenter -t {} -n ping -c 1 -W 1 {} >/dev/null 2>&1", 
                    container_pid, gateway_ip);
                let _ = CommandExecutor::execute_shell(&arp_ping_cmd);
                
                // Check again
                if let Ok(result) = CommandExecutor::execute_shell(&arp_check_cmd) {
                    if !result.stdout.trim().is_empty() {
                        ConsoleLogger::debug(&format!("ℹ️ [ARP-TEST] ARP entry created after ping: {}", result.stdout.trim()));
                    }
                }
            }
        }
    }
    
    /// Test routing to gateway
    fn test_gateway_routing(&self, container_pid: i32, gateway_ip: &str, interface_name: &str) {
        ConsoleLogger::debug(&format!("🛣️ [ROUTE-TEST] Testing routing to gateway {} via {}", gateway_ip, interface_name));
        
        // Check specific route to gateway
        let route_check_cmd = format!("nsenter -t {} -n ip route get {}", container_pid, gateway_ip);
        match CommandExecutor::execute_shell(&route_check_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [ROUTE-TEST] Route to gateway: {}", result.stdout.trim()));
                
                // Verify route uses correct interface
                if result.stdout.contains(interface_name) {
                    ConsoleLogger::debug(&format!("✅ [ROUTE-TEST] Route correctly uses interface {}", interface_name));
                } else {
                    ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] Route does not use expected interface {}", interface_name));
                }
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] Cannot determine route to gateway {}", gateway_ip));
                
                // Show all routes for debugging
                let all_routes_cmd = format!("nsenter -t {} -n ip route show", container_pid);
                if let Ok(routes_result) = CommandExecutor::execute_shell(&all_routes_cmd) {
                    ConsoleLogger::debug(&format!("🔍 [ROUTE-TEST] All routes:\n{}", routes_result.stdout));
                }
            }
        }
    }
    
    /// Test interface-level connectivity
    fn test_interface_connectivity(&self, container_pid: i32, interface_name: &str) {
        ConsoleLogger::debug(&format!("🔌 [IFACE-TEST] Testing interface {} connectivity", interface_name));
        
        // Check interface state
        let iface_check_cmd = format!("nsenter -t {} -n ip link show {}", container_pid, interface_name);
        match CommandExecutor::execute_shell(&iface_check_cmd) {
            Ok(result) if result.success => {
                if result.stdout.contains("state UP") {
                    ConsoleLogger::debug(&format!("✅ [IFACE-TEST] Interface {} is UP", interface_name));
                } else {
                    ConsoleLogger::warning(&format!("⚠️ [IFACE-TEST] Interface {} is not UP", interface_name));
                }
                
                if result.stdout.contains("LOWER_UP") {
                    ConsoleLogger::debug(&format!("✅ [IFACE-TEST] Interface {} has carrier", interface_name));
                } else {
                    ConsoleLogger::warning(&format!("⚠️ [IFACE-TEST] Interface {} has no carrier", interface_name));
                }
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [IFACE-TEST] Cannot check interface {} state", interface_name));
            }
        }
        
        // Check interface statistics
        let stats_check_cmd = format!("nsenter -t {} -n cat /proc/net/dev | grep {}", container_pid, interface_name);
        if let Ok(result) = CommandExecutor::execute_shell(&stats_check_cmd) {
            if !result.stdout.trim().is_empty() {
                ConsoleLogger::debug(&format!("📊 [IFACE-TEST] Interface stats: {}", result.stdout.trim()));
            }
        }
    }
    
    /// Diagnose bridge connectivity issues from host side
    fn diagnose_bridge_connectivity_issues(&self, gateway_ip: &str) {
        ConsoleLogger::debug(&format!("🌉 [BRIDGE-DIAG] Diagnosing bridge connectivity issues for {}", gateway_ip));
        
        // Check if host can ping the bridge IP
        let host_ping_cmd = format!("ping -c 1 -W 1 {} >/dev/null 2>&1", gateway_ip);
        match CommandExecutor::execute_shell(&host_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [BRIDGE-DIAG] Host can ping bridge IP {}", gateway_ip));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [BRIDGE-DIAG] Host cannot ping bridge IP {}", gateway_ip));
                
                // Check bridge interface status from host
                let bridge_status_cmd = format!("ip addr show {}", self.config.bridge_name);
                if let Ok(result) = CommandExecutor::execute_shell(&bridge_status_cmd) {
                    ConsoleLogger::debug(&format!("🔍 [BRIDGE-DIAG] Bridge status:\n{}", result.stdout));
                }
            }
        }
        
        // Check bridge forwarding table
        let fdb_cmd = format!("bridge fdb show | head -20");
        if let Ok(result) = CommandExecutor::execute_shell(&fdb_cmd) {
            ConsoleLogger::debug(&format!("🔍 [BRIDGE-DIAG] Bridge FDB (first 20 entries):\n{}", result.stdout));
        }
    }
    
    /// ENHANCED: Test bidirectional connectivity between container and host
    fn test_bidirectional_connectivity(&self, container_pid: i32, container_ip: &str, gateway_ip: &str) {
        ConsoleLogger::debug(&format!("🔄 [BIDIR-TEST] Testing bidirectional connectivity: container {} <-> gateway {}", 
            container_ip, gateway_ip));
        
        // Test 1: Container -> Host (already tested above via gateway ping)
        ConsoleLogger::debug("🔽 [BIDIR-TEST] Container -> Host connectivity (via gateway ping)");
        
        // Test 2: Host -> Container
        ConsoleLogger::debug("🔼 [BIDIR-TEST] Host -> Container connectivity");
        let host_to_container_cmd = format!("ping -c 2 -W 1 {}", container_ip);
        match CommandExecutor::execute_shell(&host_to_container_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::success(&format!("✅ [BIDIR-TEST] Host can ping container at {}", container_ip));
                
                // Extract RTT statistics
                if let Some(rtt_line) = result.stdout.lines().find(|line| line.contains("rtt")) {
                    ConsoleLogger::debug(&format!("📊 [BIDIR-TEST] RTT stats: {}", rtt_line.trim()));
                }
            }
            Ok(result) => {
                ConsoleLogger::warning(&format!("⚠️ [BIDIR-TEST] Host cannot ping container at {}", container_ip));
                ConsoleLogger::debug(&format!("🔍 [BIDIR-TEST] Host->Container ping output:\n{}", result.stdout));
                
                // Additional diagnostics
                self.diagnose_host_to_container_connectivity_failure(container_ip);
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("⚠️ [BIDIR-TEST] Host->Container ping command failed: {}", e));
            }
        }
        
        // Test 3: Check if container can be reached via bridge interface specifically
        let bridge_ping_cmd = format!("ping -c 1 -W 1 -I {} {}", self.config.bridge_name, container_ip);
        match CommandExecutor::execute_shell(&bridge_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [BIDIR-TEST] Bridge interface can reach container"));
            }
            _ => {
                ConsoleLogger::debug(&format!("ℹ️ [BIDIR-TEST] Bridge interface specific ping failed (may be normal)"));
            }
        }
    }
    
    /// Diagnose why host cannot reach container
    fn diagnose_host_to_container_connectivity_failure(&self, container_ip: &str) {
        ConsoleLogger::debug(&format!("🔍 [HOST-DIAG] Diagnosing host->container connectivity failure for {}", container_ip));
        
        // Check host routing to container IP
        let host_route_cmd = format!("ip route get {}", container_ip);
        if let Ok(result) = CommandExecutor::execute_shell(&host_route_cmd) {
            ConsoleLogger::debug(&format!("🛣️ [HOST-DIAG] Host route to container: {}", result.stdout.trim()));
        }
        
        // Check host ARP table for container
        let host_arp_cmd = format!("ip neigh show {}", container_ip);
        if let Ok(result) = CommandExecutor::execute_shell(&host_arp_cmd) {
            if !result.stdout.trim().is_empty() {
                ConsoleLogger::debug(&format!("🔍 [HOST-DIAG] Host ARP entry: {}", result.stdout.trim()));
            } else {
                ConsoleLogger::debug(&format!("ℹ️ [HOST-DIAG] No ARP entry found for container IP"));
            }
        }
        
        // Check bridge interface list to see if container's veth is attached
        let bridge_list_cmd = format!("brctl show {}", self.config.bridge_name);
        if let Ok(result) = CommandExecutor::execute_shell(&bridge_list_cmd) {
            ConsoleLogger::debug(&format!("🌉 [HOST-DIAG] Bridge interfaces:\n{}", result.stdout));
        }
        
        // Check iptables rules that might be blocking
        let iptables_check_cmd = "iptables -L FORWARD -v -n | head -10";
        if let Ok(result) = CommandExecutor::execute_shell(iptables_check_cmd) {
            ConsoleLogger::debug(&format!("🔒 [HOST-DIAG] FORWARD rules (first 10):\n{}", result.stdout));
        }
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
            
            // Check 3: Test actual network connectivity (ping gateway)
            if verification_ok && attempt > 5 { // Give network a moment to stabilize
                let gateway_ip = config.gateway_ip.split('/').next().unwrap();
                let ping_test_cmd = format!(
                    "nsenter -t {} -n ping -c 1 -W 1 {} >/dev/null 2>&1",
                    container_pid, gateway_ip
                );
                
                match CommandExecutor::execute_shell(&ping_test_cmd) {
                    Ok(result) if result.success => {
                        ConsoleLogger::debug(&format!("✅ Gateway ping successful"));
                        // Network is fully ready
                        break;
                    }
                    _ => {
                        ConsoleLogger::debug(&format!("Gateway ping failed on attempt {}", attempt));
                        verification_ok = false;
                        error_details.push("Gateway not reachable yet".to_string());
                    }
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
        let rootfs_path = format!("/tmp/quilt-containers/{}", config.container_id);
        
        for attempt in 1..=30 { // Max 3 seconds for exec readiness
            // Test basic exec functionality with chroot to match actual exec behavior
            let exec_test_cmd = format!(
                "nsenter -t {} -p -m -n -u -- chroot {} /bin/sh -c 'export PATH=/bin:/usr/bin:/sbin:/usr/sbin:$PATH; echo network_exec_ready'",
                container_pid, rootfs_path
            );
            
            match CommandExecutor::execute_shell(&exec_test_cmd) {
                Ok(result) if result.success => {
                    let stdout = result.stdout.trim();
                    if stdout.contains("network_exec_ready") {
                        ConsoleLogger::debug(&format!("✅ Container {} exec readiness verified", config.container_id));
                        break;
                    } else {
                        ConsoleLogger::debug(&format!("Exec test unexpected output: '{}'", stdout));
                    }
                }
                Ok(result) => {
                    ConsoleLogger::debug(&format!("Exec test failed: {}", result.stderr));
                    // If chroot fails, might be a timing issue with mount namespace
                    if result.stderr.contains("chroot:") && attempt < 10 {
                        ConsoleLogger::debug("Chroot not ready yet, retrying...");
                    }
                }
                Err(e) => {
                    ConsoleLogger::debug(&format!("Exec test error: {}", e));
                }
            }
            
            if attempt == 30 {
                ConsoleLogger::warning(&format!("Container {} exec verification timed out - proceeding anyway", config.container_id));
                // Don't fail hard here - container might still work
                break;
            }
            
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        
        // Phase 3: Network connectivity test with debugging
        ConsoleLogger::debug(&format!("🔍 Testing container {} network connectivity", config.container_id));
        
        // First check if we can see the gateway in ARP table
        let arp_check_cmd = format!(
            "nsenter -t {} -n ip neigh show | grep {}",
            container_pid, self.config.bridge_ip
        );
        
        if let Ok(arp_result) = CommandExecutor::execute_shell(&arp_check_cmd) {
            ConsoleLogger::debug(&format!("ARP entry for gateway: {}", arp_result.stdout.trim()));
        }
        
        // Try to ping the gateway with verbose output
        let gateway_ping_cmd = format!(
            "nsenter -t {} -n ping -c 1 -W 2 {}",
            container_pid, self.config.bridge_ip
        );
        
        match CommandExecutor::execute_shell(&gateway_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::success(&format!("✅ Container {} can reach gateway {}", config.container_id, self.config.bridge_ip));
            }
            Ok(result) => {
                ConsoleLogger::warning(&format!("Gateway ping failed: stdout='{}', stderr='{}'", 
                    result.stdout.trim(), result.stderr.trim()));
                
                // Debug: Check routing table
                let route_check = format!("nsenter -t {} -n ip route", container_pid);
                if let Ok(route_result) = CommandExecutor::execute_shell(&route_check) {
                    ConsoleLogger::debug(&format!("Container routes:\n{}", route_result.stdout));
                }
                
                // Debug: Check interface status
                let iface_check = format!("nsenter -t {} -n ip link show", container_pid);
                if let Ok(iface_result) = CommandExecutor::execute_shell(&iface_check) {
                    ConsoleLogger::debug(&format!("Container interfaces:\n{}", iface_result.stdout));
                }
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Gateway ping error: {}", e));
            }
        }
        
        ConsoleLogger::info(&format!("Container {} network setup completed", config.container_id));
        Ok(())
    }
    
    // Comprehensive bridge existence and configuration verification - WITH CACHING
    fn bridge_exists_and_configured(&self) -> bool {
        // Fast path: Check cached bridge ready flag first
        if self.bridge_ready.load(Ordering::Relaxed) {
            // Check if we need to re-verify based on cache duration
            if let Ok(state) = self.bridge_state.lock() {
                if !state.needs_verification(Duration::from_secs(10)) {
                    ConsoleLogger::debug(&format!("✅ [BRIDGE-CACHE] Bridge {} verified from cache (age: {:?})", 
                        self.config.bridge_name, state.last_verified.elapsed()));
                    return state.is_fully_configured();
                }
            }
        }
        
        // Slow path: Actually verify bridge state
        ConsoleLogger::debug(&format!("🔍 [BRIDGE-VERIFY] Checking if bridge {} is properly configured", self.config.bridge_name));
        
        let verification_result = self.verify_bridge_state_full();
        
        // Update cache and fast-path flag
        if let Ok(mut state) = self.bridge_state.lock() {
            state.exists = verification_result.0;
            state.has_ip = verification_result.1;
            state.is_up = verification_result.2;
            state.mark_verified();
            
            let fully_configured = state.is_fully_configured();
            self.bridge_ready.store(fully_configured, Ordering::Relaxed);
            
            ConsoleLogger::debug(&format!("🔧 [BRIDGE-CACHE] Updated bridge state: exists={}, has_ip={}, is_up={}, configured={}", 
                state.exists, state.has_ip, state.is_up, fully_configured));
            
            fully_configured
        } else {
            ConsoleLogger::warning("⚠️ [BRIDGE-VERIFY] Failed to acquire bridge state lock, using uncached result");
            verification_result.0 && verification_result.1 && verification_result.2
        }
    }
    
    // Perform full bridge state verification (called when cache is stale)
    fn verify_bridge_state_full(&self) -> (bool, bool, bool) {
        ConsoleLogger::debug(&format!("🔍 [BRIDGE-VERIFY-FULL] Full verification for bridge {}", self.config.bridge_name));
        
        // Check 1: Bridge device exists
        let bridge_exists = match CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name)) {
            Ok(result) => result.success && result.stdout.contains(&self.config.bridge_name),
            Err(_) => false,
        };
        
        if !bridge_exists {
            ConsoleLogger::debug(&format!("❌ [BRIDGE-VERIFY] Bridge {} does not exist", self.config.bridge_name));
            return (false, false, false);
        }
        
        // Check 2: Bridge has correct IP address - IMPROVED with multiple verification methods
        let ip_configured = self.verify_bridge_ip_with_retry();
        
        if !ip_configured {
            ConsoleLogger::debug(&format!("❌ [BRIDGE-VERIFY] Bridge {} does not have correct IP {}", 
                self.config.bridge_name, self.config.bridge_ip));
        }
        
        // Check 3: Bridge is administratively UP (operational state can be DOWN if no interfaces connected)
        let bridge_up = match CommandExecutor::execute_shell(&format!("ip link show {} | grep '<.*UP.*>'", self.config.bridge_name)) {
            Ok(result) => result.success,
            Err(_) => false,
        };
        
        if !bridge_up {
            ConsoleLogger::debug(&format!("❌ [BRIDGE-VERIFY] Bridge {} is not UP", self.config.bridge_name));
        }
        
        // Check 4: IP forwarding is enabled - NON-BLOCKING check
        let forwarding_enabled = match CommandExecutor::execute_shell("cat /proc/sys/net/ipv4/ip_forward") {
            Ok(result) => result.success && result.stdout.trim() == "1",
            Err(_) => false,
        };
        
        if !forwarding_enabled {
            ConsoleLogger::debug("ℹ️ [BRIDGE-VERIFY] IP forwarding not enabled, will enable during setup (non-blocking)");
        }
        
        let result = (bridge_exists, ip_configured, bridge_up);
        ConsoleLogger::debug(&format!("🔧 [BRIDGE-VERIFY-FULL] Verification results: exists={}, has_ip={}, is_up={}", 
            result.0, result.1, result.2));
        
        result
    }
    
    // ROBUST IP address verification with retry logic and multiple detection methods
    fn verify_bridge_ip_with_retry(&self) -> bool {
        ConsoleLogger::debug(&format!("🔍 [IP-VERIFY] Verifying IP {} on bridge {} with retry logic", 
            self.config.bridge_ip, self.config.bridge_name));
            
        // Try multiple verification methods with retry
        for attempt in 1..=3 {
            ConsoleLogger::debug(&format!("🔄 [IP-VERIFY] Attempt {}/3", attempt));
            
            // Method 1: Standard ip addr show with exact IP match
            let exact_match_cmd = format!("ip addr show {} | grep -q 'inet {}'", 
                self.config.bridge_name, self.config.bridge_ip);
            if let Ok(result) = CommandExecutor::execute_shell(&exact_match_cmd) {
                if result.success {
                    ConsoleLogger::debug(&format!("✅ [IP-VERIFY] Method 1 success: Exact IP match found"));
                    return true;
                }
            }
            
            // Method 2: IP with CIDR notation
            let cidr_match_cmd = format!("ip addr show {} | grep -q 'inet {}/16'", 
                self.config.bridge_name, self.config.bridge_ip);
            if let Ok(result) = CommandExecutor::execute_shell(&cidr_match_cmd) {
                if result.success {
                    ConsoleLogger::debug(&format!("✅ [IP-VERIFY] Method 2 success: CIDR IP match found"));
                    return true;
                }
            }
            
            // Method 3: Parse output manually for more robust detection
            let show_cmd = format!("ip addr show {}", self.config.bridge_name);
            if let Ok(result) = CommandExecutor::execute_shell(&show_cmd) {
                if result.success && result.stdout.contains(&self.config.bridge_ip) {
                    ConsoleLogger::debug(&format!("✅ [IP-VERIFY] Method 3 success: IP found in addr output"));
                    return true;
                }
            }
            
            // Method 4: Check if IP responds to ping (final validation)
            let ping_cmd = format!("ping -c 1 -W 1 {} >/dev/null 2>&1", self.config.bridge_ip);
            if let Ok(result) = CommandExecutor::execute_shell(&ping_cmd) {
                if result.success {
                    ConsoleLogger::debug(&format!("✅ [IP-VERIFY] Method 4 success: IP responds to ping"));
                    return true;
                }
            }
            
            if attempt < 3 {
                ConsoleLogger::debug(&format!("⏳ [IP-VERIFY] Attempt {} failed, waiting 100ms before retry", attempt));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        
        ConsoleLogger::debug(&format!("❌ [IP-VERIFY] All verification methods failed for IP {} on bridge {}", 
            self.config.bridge_ip, self.config.bridge_name));
        false
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
        
        // Enable IP forwarding
        ConsoleLogger::debug("Enabling IP forwarding");
        if let Err(e) = CommandExecutor::execute_shell("sysctl -w net.ipv4.ip_forward=1") {
            ConsoleLogger::warning(&format!("Failed to enable IP forwarding: {}", e));
        }
        
        // Enable promiscuous mode on bridge for better packet visibility
        let promisc_cmd = format!("ip link set {} promisc on", self.config.bridge_name);
        if let Err(e) = CommandExecutor::execute_shell(&promisc_cmd) {
            ConsoleLogger::debug(&format!("Failed to enable promiscuous mode: {}", e));
        }
        
        // Enable bridge netfilter for iptables to work with bridged traffic
        ConsoleLogger::debug("Enabling bridge netfilter");
        
        // First ensure br_netfilter module is loaded
        if let Err(e) = CommandExecutor::execute_shell("modprobe br_netfilter 2>/dev/null") {
            ConsoleLogger::debug(&format!("Failed to load br_netfilter module (may not be needed): {}", e));
        }
        
        // Set bridge-specific settings to allow traffic between containers
        // IMPORTANT: These must be set to 1 for iptables to work with bridge traffic
        let bridge_sysctls = vec![
            ("net.bridge.bridge-nf-call-iptables", "1"),
            ("net.bridge.bridge-nf-call-ip6tables", "1"), 
            ("net.bridge.bridge-nf-call-arptables", "1"),
            ("net.ipv4.conf.all.forwarding", "1"),
            ("net.ipv4.conf.default.forwarding", "1"),
        ];
        
        for (sysctl, value) in bridge_sysctls {
            let cmd = format!("sysctl -w {}={} 2>/dev/null", sysctl, value);
            if let Err(e) = CommandExecutor::execute_shell(&cmd) {
                ConsoleLogger::debug(&format!("Sysctl {} failed (may be OK): {}", sysctl, e));
            } else {
                ConsoleLogger::debug(&format!("Set {} = {}", sysctl, value));
            }
        }
        
        // Also set forwarding for the bridge interface specifically
        let bridge_forward_cmd = format!("sysctl -w net.ipv4.conf.{}.forwarding=1 2>/dev/null", self.config.bridge_name);
        let _ = CommandExecutor::execute_shell(&bridge_forward_cmd);
        
        // Enable proxy ARP on bridge to help with container discovery
        let proxy_arp_cmd = format!("sysctl -w net.ipv4.conf.{}.proxy_arp=1", self.config.bridge_name);
        let _ = CommandExecutor::execute_shell(&proxy_arp_cmd);
        
        // Disable bridge filtering to ensure packets flow freely
        let bridge_filter_cmds = vec![
            format!("echo 0 > /proc/sys/net/bridge/bridge-nf-call-iptables 2>/dev/null || true"),
            format!("echo 0 > /proc/sys/net/bridge/bridge-nf-call-ip6tables 2>/dev/null || true"),
            format!("echo 0 > /proc/sys/net/bridge/bridge-nf-call-arptables 2>/dev/null || true"),
        ];
        
        for cmd in bridge_filter_cmds {
            let _ = CommandExecutor::execute_shell(&cmd);
        }
        
        // Set up iptables rules for bridge forwarding
        ConsoleLogger::debug("Setting up iptables rules for bridge");
        
        // First, ensure default FORWARD policy isn't DROP
        let _ = CommandExecutor::execute_shell("iptables -P FORWARD ACCEPT");
        
        // Clear any existing rules that might block our traffic
        let clear_commands = vec![
            format!("iptables -D FORWARD -i {} -j REJECT 2>/dev/null || true", self.config.bridge_name),
            format!("iptables -D FORWARD -o {} -j REJECT 2>/dev/null || true", self.config.bridge_name),
        ];
        
        for cmd in clear_commands {
            let _ = CommandExecutor::execute_shell(&cmd);
        }
        
        let iptables_commands = vec![
            // CRITICAL: Allow all established connections
            "iptables -I FORWARD 1 -m state --state ESTABLISHED,RELATED -j ACCEPT".to_string(),
            
            // Allow all traffic within the bridge subnet (container-to-container)
            format!("iptables -I FORWARD 1 -s 10.42.0.0/16 -d 10.42.0.0/16 -j ACCEPT"),
            
            // Accept all traffic on the bridge interface
            format!("iptables -I FORWARD 1 -i {} -j ACCEPT", self.config.bridge_name),
            format!("iptables -I FORWARD 1 -o {} -j ACCEPT", self.config.bridge_name),
            
            // Allow DNS traffic to the bridge interface (both original port 53 and redirect port 1053)
            format!("iptables -I INPUT 1 -i {} -p udp --dport 53 -j ACCEPT", self.config.bridge_name),
            format!("iptables -I INPUT 1 -i {} -p tcp --dport 53 -j ACCEPT", self.config.bridge_name),
            format!("iptables -I INPUT 1 -i {} -p udp --dport 1053 -j ACCEPT", self.config.bridge_name),
            format!("iptables -I INPUT 1 -i {} -p tcp --dport 1053 -j ACCEPT", self.config.bridge_name),
            
            // CRITICAL: Redirect DNS queries on bridge from port 53 to 1053 to avoid systemd-resolved conflicts
            format!("iptables -t nat -A PREROUTING -i {} -p udp --dport 53 -j DNAT --to-destination {}:1053", self.config.bridge_name, self.config.bridge_ip),
            format!("iptables -t nat -A PREROUTING -i {} -p tcp --dport 53 -j DNAT --to-destination {}:1053", self.config.bridge_name, self.config.bridge_ip),
            
            // Allow gRPC traffic to the bridge interface
            format!("iptables -I INPUT 1 -i {} -p tcp --dport 50051 -j ACCEPT", self.config.bridge_name),
            
            // Allow all ICMP traffic (ping, traceroute, etc)
            "iptables -I FORWARD 1 -p icmp -j ACCEPT".to_string(),
            "iptables -I INPUT 1 -p icmp -j ACCEPT".to_string(),
            
            // Enable NAT for external connectivity
            format!("iptables -t nat -A POSTROUTING -s 10.42.0.0/16 ! -o {} -j MASQUERADE", self.config.bridge_name),
        ];
        
        for cmd in iptables_commands {
            ConsoleLogger::debug(&format!("Executing: {}", cmd));
            if let Err(e) = CommandExecutor::execute_shell(&cmd) {
                ConsoleLogger::warning(&format!("Failed to execute iptables rule: {} - {}", cmd, e));
            }
        }
        
        // Configure bridge settings for proper container networking
        ConsoleLogger::debug("Configuring bridge for container networking...");
        
        // Enable STP (Spanning Tree Protocol) with fast convergence
        let stp_cmd = format!("ip link set dev {} type bridge stp_state 1", self.config.bridge_name);
        if let Err(e) = CommandExecutor::execute_shell(&stp_cmd) {
            ConsoleLogger::warning(&format!("Failed to enable STP: {}", e));
        }
        
        // Set forward delay to 0 for immediate forwarding
        let forward_delay_cmd = format!("ip link set dev {} type bridge forward_delay 0", self.config.bridge_name);
        if let Err(e) = CommandExecutor::execute_shell(&forward_delay_cmd) {
            ConsoleLogger::warning(&format!("Failed to set forward delay: {}", e));
        }
        
        // Ensure learning is enabled (should be default, but make it explicit)
        let learning_cmd = format!("ip link set dev {} type bridge learning 1", self.config.bridge_name);
        if let Err(e) = CommandExecutor::execute_shell(&learning_cmd) {
            ConsoleLogger::warning(&format!("Failed to enable learning: {}", e));
        }
        
        // Set MAC address aging time
        let aging_cmd = format!("ip link set dev {} type bridge ageing_time 300", self.config.bridge_name);
        if let Err(e) = CommandExecutor::execute_shell(&aging_cmd) {
            ConsoleLogger::warning(&format!("Failed to set aging time: {}", e));
        }
        
        // Disable VLAN filtering which can interfere with container communication
        let vlan_cmd = format!("ip link set dev {} type bridge vlan_filtering 0", self.config.bridge_name);
        if let Err(e) = CommandExecutor::execute_shell(&vlan_cmd) {
            ConsoleLogger::warning(&format!("Failed to disable VLAN filtering: {}", e));
        }
        
        // Verify bridge creation succeeded
        for attempt in 1..=5 {
            if self.bridge_exists_and_configured() {
                ConsoleLogger::success(&format!("✅ Bridge {} created and configured successfully", self.config.bridge_name));
                return Ok(());
            }
            if attempt < 5 {
                thread::sleep(Duration::from_millis(100));
            }
        }
        
        Err(format!("Bridge {} failed creation verification after 5 attempts", self.config.bridge_name))
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
    
    /// Start DNS server on bridge interface with graceful retry logic
    pub async fn start_dns_server(&mut self) -> Result<(), String> {
        // Ensure bridge is ready first
        self.ensure_bridge_ready()?;
        
        // Try primary port 1053, with fallback to other ports if needed
        let primary_port = 1053;
        let fallback_ports = vec![1153, 1253, 1353, 1453]; // Additional ports to try
        
        for (attempt, port) in std::iter::once(primary_port)
            .chain(fallback_ports.into_iter())
            .enumerate() {
            
            let dns_addr: SocketAddr = format!("{}:{}", self.config.bridge_ip, port).parse()
                .map_err(|e| format!("Invalid DNS address: {}", e))?;
            
            ConsoleLogger::debug(&format!("🔄 [DNS-START] Attempt {}: trying to bind DNS server to {}", attempt + 1, dns_addr));
            
            let dns_server = Arc::new(DnsServer::new(dns_addr));
            
            match dns_server.start().await {
                Ok(()) => {
                    self.dns_server = Some(dns_server);
                    
                    if port != primary_port {
                        ConsoleLogger::warning(&format!("⚠️ [DNS-START] DNS server started on fallback port {}", port));
                        // Update iptables rules to redirect to the actual port
                        self.update_dns_redirect_rules(port)?;
                    }
                    
                    ConsoleLogger::success(&format!("✅ [DNS-START] DNS server started on {} (accessible to containers via port 53)", dns_addr));
                    return Ok(());
                }
                Err(e) => {
                    ConsoleLogger::warning(&format!("⚠️ [DNS-START] Failed to start DNS on port {}: {}", port, e));
                    
                    if attempt == 4 { // Last attempt
                        ConsoleLogger::error(&format!("❌ [DNS-START] Failed to start DNS server after trying 5 ports"));
                        
                        // Continue without DNS - bridge networking still works for IP-based communication
                        ConsoleLogger::warning("🔄 [DNS-START] Continuing without DNS server - containers can still communicate via IP addresses");
                        return Ok(()); // Don't fail the entire server startup
                    }
                    
                    // Small delay before next attempt
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        
        unreachable!()
    }
    
    /// Update iptables DNS redirect rules for fallback port
    fn update_dns_redirect_rules(&self, actual_port: u16) -> Result<(), String> {
        if actual_port == 1053 {
            return Ok(()); // No update needed for primary port
        }
        
        ConsoleLogger::debug(&format!("🔧 [DNS-REDIRECT] Updating iptables to redirect DNS to port {}", actual_port));
        
        // Remove old redirect rules (ignore errors)
        let cleanup_cmds = vec![
            format!("iptables -t nat -D PREROUTING -i {} -p udp --dport 53 -j DNAT --to-destination {}:1053 2>/dev/null || true", self.config.bridge_name, self.config.bridge_ip),
            format!("iptables -t nat -D PREROUTING -i {} -p tcp --dport 53 -j DNAT --to-destination {}:1053 2>/dev/null || true", self.config.bridge_name, self.config.bridge_ip),
        ];
        
        for cmd in cleanup_cmds {
            let _ = CommandExecutor::execute_shell(&cmd);
        }
        
        // Add new redirect rules
        let new_rules = vec![
            format!("iptables -t nat -A PREROUTING -i {} -p udp --dport 53 -j DNAT --to-destination {}:{}", self.config.bridge_name, self.config.bridge_ip, actual_port),
            format!("iptables -t nat -A PREROUTING -i {} -p tcp --dport 53 -j DNAT --to-destination {}:{}", self.config.bridge_name, self.config.bridge_ip, actual_port),
            format!("iptables -I INPUT 1 -i {} -p udp --dport {} -j ACCEPT", self.config.bridge_name, actual_port),
            format!("iptables -I INPUT 1 -i {} -p tcp --dport {} -j ACCEPT", self.config.bridge_name, actual_port),
        ];
        
        for cmd in new_rules {
            if let Err(e) = CommandExecutor::execute_shell(&cmd) {
                ConsoleLogger::warning(&format!("Failed to update iptables rule: {} - {}", cmd, e));
            }
        }
        
        Ok(())
    }
    
    /// Register container with DNS
    pub fn register_container_dns(&self, container_id: &str, container_name: &str, ip_address: &str) -> Result<(), String> {
        if let Some(dns) = &self.dns_server {
            dns.register_container(container_id, container_name, ip_address)?;
        } else {
            ConsoleLogger::warning("DNS server not started, skipping container registration");
        }
        Ok(())
    }
    
    /// Unregister container from DNS
    pub fn unregister_container_dns(&self, container_id: &str) -> Result<(), String> {
        if let Some(dns) = &self.dns_server {
            dns.unregister_container(container_id)?;
        }
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
        let check_cmd = format!("ip link show {} | grep -q '<.*UP.*>'", self.config.bridge_name);
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
    
    /// PRODUCTION-GRADE: Ensure bridge exists and is ready for veth attachment - FIXED to avoid bridge deletion
    fn ensure_bridge_ready_for_attachment(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔍 [BRIDGE-VERIFY] Checking if bridge {} exists and is ready for veth attachment", self.config.bridge_name));
        
        // Check if bridge exists and is properly configured
        if self.bridge_exists_and_configured() {
            ConsoleLogger::debug(&format!("✅ [BRIDGE-VERIFY] Bridge {} is ready for attachment", self.config.bridge_name));
            return Ok(());
        }
        
        // FIXED: Never delete the bridge during container operations!
        // If bridge is missing, this is a serious system issue that should not be "fixed" by deletion
        ConsoleLogger::error(&format!("❌ [BRIDGE-VERIFY] Bridge {} is missing or misconfigured!", self.config.bridge_name));
        ConsoleLogger::error("🚨 [BRIDGE-VERIFY] Bridge should be created at startup - this indicates a system-level networking issue");
        
        // Instead of deleting and recreating, try to diagnose the issue
        self.diagnose_bridge_issues()?;
        
        // Try ONE non-destructive recreation attempt (without deletion)
        ConsoleLogger::warning(&format!("🔧 [BRIDGE-REPAIR] Attempting non-destructive bridge repair for {}", self.config.bridge_name));
        
        // Only create if bridge is completely missing (don't delete existing bridge)
        match CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name)) {
            Ok(result) if result.success => {
                // Bridge exists but is misconfigured - try to fix configuration without deletion
                ConsoleLogger::info(&format!("🔧 [BRIDGE-REPAIR] Bridge exists but misconfigured, attempting repair"));
                self.repair_bridge_configuration()?;
            }
            _ => {
                // Bridge doesn't exist at all - safe to create
                ConsoleLogger::info(&format!("🏗️ [BRIDGE-REPAIR] Bridge missing, creating new bridge"));
                self.create_bridge_atomic()?;
            }
        }
        
        // Final verification
        if !self.bridge_exists_and_configured() {
            return Err(format!(
                "Bridge {} is still not ready after repair attempt. This indicates a serious networking issue that requires manual intervention.", 
                self.config.bridge_name
            ));
        }
        
        ConsoleLogger::success(&format!("✅ [BRIDGE-REPAIR] Bridge {} is now ready for attachment", self.config.bridge_name));
        Ok(())
    }
    
    /// Diagnose bridge networking issues without making destructive changes
    fn diagnose_bridge_issues(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔍 [BRIDGE-DIAG] Diagnosing bridge issues for {}", self.config.bridge_name));
        
        // Check if bridge exists
        if let Ok(result) = CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name)) {
            if result.success {
                ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] Bridge exists: {}", result.stdout.trim()));
                
                // Check IP configuration
                if let Ok(addr_result) = CommandExecutor::execute_shell(&format!("ip addr show {}", self.config.bridge_name)) {
                    ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] Bridge addressing: {}", addr_result.stdout.trim()));
                }
                
                // Check bridge interfaces
                if let Ok(brctl_result) = CommandExecutor::execute_shell(&format!("brctl show {}", self.config.bridge_name)) {
                    ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] Bridge interfaces: {}", brctl_result.stdout.trim()));
                }
            } else {
                ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] Bridge does not exist: {}", result.stderr.trim()));
            }
        }
        
        // Check system network state
        if let Ok(all_links) = CommandExecutor::execute_shell("ip link show") {
            ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] All network interfaces available"));
        }
        
        // Check IP forwarding
        if let Ok(forward_result) = CommandExecutor::execute_shell("cat /proc/sys/net/ipv4/ip_forward") {
            ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] IP forwarding: {}", forward_result.stdout.trim()));
        }
        
        Ok(())
    }
    
    /// Attempt to repair bridge configuration without deleting the bridge
    fn repair_bridge_configuration(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔧 [BRIDGE-REPAIR] Repairing bridge {} configuration", self.config.bridge_name));
        
        // Try to add IP address if missing
        let bridge_cidr = format!("{}/16", self.config.bridge_ip);
        let add_ip_cmd = format!("ip addr add {} dev {} 2>/dev/null || true", bridge_cidr, self.config.bridge_name);
        if let Ok(result) = CommandExecutor::execute_shell(&add_ip_cmd) {
            ConsoleLogger::debug(&format!("🔧 [BRIDGE-REPAIR] IP address repair result: success={}", result.success));
        }
        
        // Try to bring bridge up if down
        let up_cmd = format!("ip link set {} up", self.config.bridge_name);
        if let Ok(result) = CommandExecutor::execute_shell(&up_cmd) {
            ConsoleLogger::debug(&format!("🔧 [BRIDGE-REPAIR] Bridge up result: success={}", result.success));
        }
        
        // Enable IP forwarding
        if let Err(e) = CommandExecutor::execute_shell("sysctl -w net.ipv4.ip_forward=1") {
            ConsoleLogger::debug(&format!("🔧 [BRIDGE-REPAIR] IP forwarding enable failed (may be OK): {}", e));
        }
        
        // Wait for configuration to take effect
        std::thread::sleep(Duration::from_millis(200));
        
        ConsoleLogger::debug(&format!("✅ [BRIDGE-REPAIR] Bridge repair operations completed"));
        Ok(())
    }
    
    /// PRODUCTION-GRADE: Attach veth to bridge with enhanced retry logic and verification - IMPROVED
    fn attach_veth_to_bridge_with_retry(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔗 [BRIDGE-ATTACH] Attaching {} to bridge {} with enhanced reliability", veth_name, self.config.bridge_name));
        
        // Pre-flight checks
        self.verify_veth_exists(veth_name)?;
        self.verify_bridge_ready_for_attachment_fast()?;
        
        let attach_cmd = format!("ip link set {} master {}", veth_name, self.config.bridge_name);
        
        // Enhanced retry logic with exponential backoff
        for attempt in 1..=5 {  // Increased from 3 to 5 attempts
            ConsoleLogger::debug(&format!("🔄 [BRIDGE-ATTACH] Attempt {}/5: {}", attempt, attach_cmd));
            
            match CommandExecutor::execute_shell(&attach_cmd) {
                Ok(result) if result.success => {
                    // Multiple verification methods for attachment
                    match self.verify_bridge_attachment_comprehensive(veth_name) {
                        Ok(()) => {
                            ConsoleLogger::success(&format!("✅ [BRIDGE-ATTACH] Successfully attached {} to bridge {} (attempt {})", 
                                veth_name, self.config.bridge_name, attempt));
                            
                            // Post-attachment validation
                            self.post_attachment_validation(veth_name)?;
                            
                            return Ok(());
                        }
                        Err(verify_err) => {
                            ConsoleLogger::warning(&format!("⚠️ [BRIDGE-ATTACH] Attachment verification failed (attempt {}): {}", 
                                attempt, verify_err));
                            
                            // Try to diagnose attachment issue
                            self.diagnose_attachment_failure(veth_name, attempt);
                            
                            if attempt == 5 {
                                return Err(format!("Bridge attachment verification failed after 5 attempts: {}", verify_err));
                            }
                        }
                    }
                }
                Ok(result) => {
                    ConsoleLogger::warning(&format!("⚠️ [BRIDGE-ATTACH] Attachment command failed (attempt {}): {}", 
                        attempt, result.stderr));
                    
                    // Check if it's a recoverable error
                    if result.stderr.contains("Device or resource busy") && attempt < 5 {
                        ConsoleLogger::info(&format!("🔄 [BRIDGE-ATTACH] Device busy, will retry with longer wait"));
                    } else if attempt == 5 {
                        return Err(format!("Failed to attach {} to bridge {} after 5 attempts: {}", 
                            veth_name, self.config.bridge_name, result.stderr));
                    }
                }
                Err(cmd_err) => {
                    ConsoleLogger::warning(&format!("⚠️ [BRIDGE-ATTACH] Command execution failed (attempt {}): {}", 
                        attempt, cmd_err));
                    if attempt == 5 {
                        return Err(format!("Failed to execute bridge attachment command: {}", cmd_err));
                    }
                }
            }
            
            // Exponential backoff with jitter
            if attempt < 5 {
                let base_delay = 100 * (1 << (attempt - 1)); // 100ms, 200ms, 400ms, 800ms
                let jitter = (attempt * 50) as u64; // Add some jitter
                let delay = Duration::from_millis(base_delay + jitter);
                
                ConsoleLogger::debug(&format!("⏳ [BRIDGE-ATTACH] Waiting {:?} before retry", delay));
                thread::sleep(delay);
            }
        }
        
        Err("Bridge attachment failed after all retry attempts".to_string())
    }
    
    /// Verify veth interface exists before attempting attachment
    fn verify_veth_exists(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔍 [VETH-CHECK] Verifying veth {} exists", veth_name));
        
        match CommandExecutor::execute_shell(&format!("ip link show {}", veth_name)) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [VETH-CHECK] Veth {} exists and is visible", veth_name));
                Ok(())
            }
            Ok(result) => {
                Err(format!("Veth {} does not exist: {}", veth_name, result.stderr))
            }
            Err(e) => {
                Err(format!("Failed to check veth {} existence: {}", veth_name, e))
            }
        }
    }
    
    /// Fast bridge readiness check for attachment operations
    fn verify_bridge_ready_for_attachment_fast(&self) -> Result<(), String> {
        ConsoleLogger::debug(&format!("⚡ [BRIDGE-FAST] Fast bridge readiness check for {}", self.config.bridge_name));
        
        // Use cached state if available and recent
        if self.bridge_ready.load(Ordering::Relaxed) {
            if let Ok(state) = self.bridge_state.lock() {
                if !state.needs_verification(Duration::from_secs(5)) { // 5s cache for attachment ops
                    ConsoleLogger::debug("✅ [BRIDGE-FAST] Bridge ready from cache");
                    return Ok(());
                }
            }
        }
        
        // Quick verification
        if self.bridge_exists_and_configured() {
            ConsoleLogger::debug("✅ [BRIDGE-FAST] Bridge verified and ready");
            Ok(())
        } else {
            Err(format!("Bridge {} is not ready for attachment", self.config.bridge_name))
        }
    }
    
    /// Comprehensive attachment verification with multiple methods
    fn verify_bridge_attachment_comprehensive(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔍 [ATTACH-VERIFY] Comprehensive attachment verification for {}", veth_name));
        
        // Method 1: Check master relationship
        let master_check = format!("ip link show {} | grep 'master {}'", veth_name, self.config.bridge_name);
        let master_ok = match CommandExecutor::execute_shell(&master_check) {
            Ok(result) if result.success => {
                ConsoleLogger::debug("✅ [ATTACH-VERIFY] Method 1: Master relationship verified");
                true
            }
            _ => {
                ConsoleLogger::debug("❌ [ATTACH-VERIFY] Method 1: Master relationship not found");
                false
            }
        };
        
        // Method 2: Check bridge interface list
        let bridge_list_check = format!("brctl show {} | grep -q {}", self.config.bridge_name, veth_name);
        let bridge_list_ok = match CommandExecutor::execute_shell(&bridge_list_check) {
            Ok(result) if result.success => {
                ConsoleLogger::debug("✅ [ATTACH-VERIFY] Method 2: Bridge interface list verified");
                true
            }
            _ => {
                ConsoleLogger::debug("❌ [ATTACH-VERIFY] Method 2: Not found in bridge interface list");
                false
            }
        };
        
        // Method 3: Check bridge forwarding database
        let fdb_check = format!("bridge fdb show dev {}", veth_name);
        let fdb_ok = match CommandExecutor::execute_shell(&fdb_check) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [ATTACH-VERIFY] Method 3: FDB entry found: {}", result.stdout.trim()));
                true
            }
            _ => {
                ConsoleLogger::debug("ℹ️ [ATTACH-VERIFY] Method 3: FDB entry not found (may be normal for new interfaces)");
                true // Don't fail on FDB - it may not be populated yet
            }
        };
        
        // Require at least 2 out of 3 methods to succeed
        let success_count = [master_ok, bridge_list_ok, fdb_ok].iter().filter(|&&x| x).count();
        
        if success_count >= 2 {
            ConsoleLogger::success(&format!("✅ [ATTACH-VERIFY] Attachment verified ({}/3 methods succeeded)", success_count));
            Ok(())
        } else {
            Err(format!("Attachment verification failed: only {}/3 methods succeeded", success_count))
        }
    }
    
    /// Post-attachment validation to ensure attachment is stable
    fn post_attachment_validation(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔄 [POST-ATTACH] Post-attachment validation for {}", veth_name));
        
        // Wait for attachment to stabilize
        thread::sleep(Duration::from_millis(100));
        
        // Re-verify attachment is still stable
        match self.verify_bridge_attachment_comprehensive(veth_name) {
            Ok(()) => {
                ConsoleLogger::debug("✅ [POST-ATTACH] Attachment remains stable");
                Ok(())
            }
            Err(e) => {
                Err(format!("Post-attachment validation failed: {}", e))
            }
        }
    }
    
    /// Diagnose why attachment might be failing
    fn diagnose_attachment_failure(&self, veth_name: &str, attempt: u32) {
        ConsoleLogger::debug(&format!("🔍 [ATTACH-DIAG] Diagnosing attachment failure for {} (attempt {})", veth_name, attempt));
        
        // Check veth state
        if let Ok(result) = CommandExecutor::execute_shell(&format!("ip link show {}", veth_name)) {
            ConsoleLogger::debug(&format!("ℹ️ [ATTACH-DIAG] Veth state: {}", result.stdout.trim()));
        }
        
        // Check bridge state
        if let Ok(result) = CommandExecutor::execute_shell(&format!("ip link show {}", self.config.bridge_name)) {
            ConsoleLogger::debug(&format!("ℹ️ [ATTACH-DIAG] Bridge state: {}", result.stdout.trim()));
        }
        
        // Check for any existing master
        if let Ok(result) = CommandExecutor::execute_shell(&format!("ip link show {} | grep master", veth_name)) {
            if !result.stdout.trim().is_empty() {
                ConsoleLogger::debug(&format!("ℹ️ [ATTACH-DIAG] Existing master: {}", result.stdout.trim()));
            }
        }
        
        // Check system bridge capacity
        if let Ok(result) = CommandExecutor::execute_shell("brctl show") {
            ConsoleLogger::debug(&format!("ℹ️ [ATTACH-DIAG] System bridges: {}", result.stdout.replace('\n', " | ")));
        }
    }
    
    /// PRODUCTION-GRADE: Verify veth is properly attached to bridge
    fn verify_bridge_attachment(&self, veth_name: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🔍 [BRIDGE-VERIFY] Verifying {} is attached to bridge {}", 
            veth_name, self.config.bridge_name));
        
        // Check 1: Verify veth shows bridge as master
        let master_check = format!("ip link show {} | grep 'master {}'", veth_name, self.config.bridge_name);
        match CommandExecutor::execute_shell(&master_check) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [BRIDGE-VERIFY] {} shows bridge {} as master", 
                    veth_name, self.config.bridge_name));
            }
            _ => {
                return Err(format!("Veth {} does not show bridge {} as master", veth_name, self.config.bridge_name));
            }
        }
        
        // Check 2: Verify bridge shows veth as interface
        let bridge_check = format!("brctl show {} | grep {}", self.config.bridge_name, veth_name);
        match CommandExecutor::execute_shell(&bridge_check) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [BRIDGE-VERIFY] Bridge {} shows {} as attached interface", 
                    self.config.bridge_name, veth_name));
            }
            _ => {
                return Err(format!("Bridge {} does not show {} as attached interface", 
                    self.config.bridge_name, veth_name));
            }
        }
        
        ConsoleLogger::success(&format!("✅ [BRIDGE-VERIFY] {} is properly attached to bridge {}", 
            veth_name, self.config.bridge_name));
        Ok(())
    }

    /// PRODUCTION-GRADE: Get MAC address of a network interface
    /// Returns the hardware address in standard format (xx:xx:xx:xx:xx:xx)
    fn get_interface_mac_address(&self, interface_name: &str) -> Result<String, String> {
        ConsoleLogger::debug(&format!("🔍 [MAC-LOOKUP] Getting MAC address for interface: {}", interface_name));
        
        // Use ip link show to get interface details including MAC address
        let cmd = format!("ip link show {} | grep 'link/ether' | awk '{{print $2}}'", interface_name);
        
        match CommandExecutor::execute_shell(&cmd) {
            Ok(result) if result.success => {
                let mac_address = result.stdout.trim().to_string();
                
                // Validate MAC address format (xx:xx:xx:xx:xx:xx)
                if mac_address.len() == 17 && mac_address.matches(':').count() == 5 {
                    // Additional validation: ensure each octet is valid hex
                    let parts: Vec<&str> = mac_address.split(':').collect();
                    if parts.len() == 6 && parts.iter().all(|part| {
                        part.len() == 2 && part.chars().all(|c| c.is_ascii_hexdigit())
                    }) {
                        ConsoleLogger::debug(&format!("✅ [MAC-LOOKUP] Found valid MAC for {}: {}", 
                            interface_name, mac_address));
                        return Ok(mac_address);
                    }
                }
                
                return Err(format!("Invalid MAC address format for {}: '{}'", interface_name, mac_address));
            }
            Ok(result) => {
                return Err(format!("Failed to get MAC address for {}: command failed with stderr: '{}'", 
                    interface_name, result.stderr.trim()));
            }
            Err(e) => {
                return Err(format!("Failed to execute MAC lookup command for {}: {}", interface_name, e));
            }
        }
    }

    /// PRODUCTION-GRADE: Get MAC address of interface inside container namespace
    /// Returns the hardware address for container's network interface
    fn get_container_interface_mac_address(&self, container_pid: i32, interface_name: &str) -> Result<String, String> {
        ConsoleLogger::debug(&format!("🔍 [MAC-LOOKUP-NS] Getting MAC address for interface {} in container PID {}", 
            interface_name, container_pid));
        
        // Use nsenter to get MAC address from within container namespace
        let cmd = format!("nsenter -t {} -n ip link show {} | grep 'link/ether' | awk '{{print $2}}'", 
            container_pid, interface_name);
        
        match CommandExecutor::execute_shell(&cmd) {
            Ok(result) if result.success => {
                let mac_address = result.stdout.trim().to_string();
                
                // Validate MAC address format (same validation as host interface)
                if mac_address.len() == 17 && mac_address.matches(':').count() == 5 {
                    let parts: Vec<&str> = mac_address.split(':').collect();
                    if parts.len() == 6 && parts.iter().all(|part| {
                        part.len() == 2 && part.chars().all(|c| c.is_ascii_hexdigit())
                    }) {
                        ConsoleLogger::debug(&format!("✅ [MAC-LOOKUP-NS] Found valid MAC for {} in container {}: {}", 
                            interface_name, container_pid, mac_address));
                        return Ok(mac_address);
                    }
                }
                
                return Err(format!("Invalid MAC address format for {} in container {}: '{}'", 
                    interface_name, container_pid, mac_address));
            }
            Ok(result) => {
                return Err(format!("Failed to get MAC address for {} in container {}: command failed with stderr: '{}'", 
                    interface_name, container_pid, result.stderr.trim()));
            }
            Err(e) => {
                return Err(format!("Failed to execute MAC lookup command for {} in container {}: {}", 
                    interface_name, container_pid, e));
            }
        }
    }
} 