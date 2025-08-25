// Network management module
// Coordinates bridge, veth, DNS, diagnostics, and security for container networking

pub mod bridge;
pub mod veth;
pub mod dns_manager;
pub mod diagnostics;
pub mod security;

use crate::utils::console::ConsoleLogger;
use crate::utils::command::CommandExecutor;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

// Re-export commonly used types
pub use bridge::BridgeManager;
pub use veth::{VethManager, ContainerNetworkConfig};
pub use dns_manager::DnsManager;
pub use diagnostics::NetworkDiagnostics;
pub use security::NetworkSecurity;

/// Network configuration for the container networking system
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub bridge_name: String,
    pub subnet_cidr: String,
    pub bridge_ip: String,
    pub next_ip: Arc<AtomicU32>,
}

/// Main NetworkManager that orchestrates all networking components
pub struct NetworkManager {
    pub config: NetworkConfig,
    pub bridge_manager: BridgeManager,
    pub veth_manager: VethManager,
    pub dns_manager: DnsManager,
    pub diagnostics: NetworkDiagnostics,
    pub security: NetworkSecurity,
}

#[allow(dead_code)]
impl NetworkManager {
    pub fn new(bridge_name: &str, subnet_cidr: &str) -> Result<Self, String> {
        let config = NetworkConfig {
            bridge_name: bridge_name.to_string(),
            subnet_cidr: subnet_cidr.to_string(),
            bridge_ip: "10.42.0.1".to_string(),
            next_ip: Arc::new(AtomicU32::new(2)),
        };
        
        let bridge_manager = BridgeManager::new(config.bridge_name.clone(), config.bridge_ip.clone());
        let veth_manager = VethManager::new(config.bridge_name.clone());
        let dns_manager = DnsManager::new(config.bridge_name.clone(), config.bridge_ip.clone());
        let diagnostics = NetworkDiagnostics::new(config.bridge_name.clone(), config.bridge_ip.clone());
        let security = NetworkSecurity::new(config.bridge_ip.clone());
        
        Ok(Self { 
            config,
            bridge_manager,
            veth_manager,
            dns_manager,
            diagnostics,
            security,
        })
    }

    pub fn ensure_bridge_ready(&self) -> Result<(), String> {
        self.bridge_manager.ensure_bridge_ready()
    }

    pub fn setup_container_network(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        ConsoleLogger::progress(&format!("Setting up network for container {} (PID: {})", 
            config.container_id, container_pid));

        // Security validation first
        self.security.validate_container_id(&config.container_id)?;
        self.security.validate_container_pid(container_pid)?;
        self.security.validate_ip_address(&config.ip_address)?;

        // Step 1: Validate bridge is ready
        if !self.bridge_exists() {
            return Err(format!("Bridge {} does not exist - cannot setup container network", self.config.bridge_name));
        }
        
        self.bridge_manager.verify_bridge_up()
            .map_err(|e| format!("Bridge validation failed: {}", e))?;

        // Step 2: Create veth pair
        self.veth_manager.create_veth_pair(&config.veth_host_name, &config.veth_container_name)?;
        
        // Step 3: Verify veth pair creation
        self.veth_manager.verify_veth_pair_created(&config.veth_host_name, &config.veth_container_name)
            .map_err(|e| format!("Veth pair verification failed: {}", e))?;
        
        // Step 4: Security validation of container namespace  
        if !self.security.validate_container_namespace(container_pid) {
            return Err(format!("Container PID {} failed namespace security validation", container_pid));
        }
        
        // Step 4.1: Move container-side veth to container namespace
        self.veth_manager.move_veth_to_container(&config.veth_container_name, container_pid)?;
        
        // Step 5: Configure container interface (IP, routing, etc.)
        self.veth_manager.configure_container_interface(config, container_pid)?;
        
        // Step 6: Attach host-side veth to bridge
        self.veth_manager.attach_veth_to_bridge_with_retry(&config.veth_host_name)
            .map_err(|e| format!("Bridge attachment failed: {}", e))?;
        
        // Step 7: Configure DNS for container
        self.dns_manager.configure_container_dns(config, container_pid)?;
        
        // Step 7.1: Verify DNS container isolation
        let dns_content = format!("nameserver {}\nsearch quilt.local\n", self.config.bridge_ip);
        if !self.security.verify_dns_container_isolation(container_pid, &dns_content) {
            ConsoleLogger::warning(&format!("⚠️ DNS container isolation verification failed for {}", config.container_id));
        }
        
        // Step 8: Run comprehensive diagnostics
        let gateway_ip = config.gateway_ip.split('/').next().unwrap();
        let interface_name = format!("quilt{}", &config.container_id[..8]);
        self.diagnostics.test_gateway_connectivity_comprehensive(container_pid, gateway_ip, &interface_name);
        
        // Step 8.1: Test bidirectional connectivity
        let container_ip = config.ip_address.split('/').next().unwrap();
        self.diagnostics.test_bidirectional_connectivity(container_pid, container_ip, gateway_ip);
        
        // Step 9: Verify network readiness
        self.diagnostics.verify_container_network_ready(config, container_pid)?;
        
        // Step 10: Security audit
        self.security.audit_network_operation("SETUP_COMPLETE", &config.container_id, 
            &format!("IP: {}, Gateway: {}", config.ip_address, config.gateway_ip));
        
        ConsoleLogger::success(&format!("Network configured for container {} at {}", 
            config.container_id, config.ip_address));
        Ok(())
    }

    pub fn bridge_exists(&self) -> bool {
        self.bridge_manager.bridge_exists()
    }

    pub async fn start_dns_server(&mut self) -> Result<(), String> {
        // Ensure bridge is ready first
        self.ensure_bridge_ready()?;
        self.dns_manager.start_dns_server().await
    }

    pub fn register_container_dns(&self, container_id: &str, container_name: &str, ip_address: &str) -> Result<(), String> {
        self.dns_manager.register_container_dns(container_id, container_name, ip_address)
    }

    pub fn unregister_container_dns(&self, container_id: &str) -> Result<(), String> {
        self.dns_manager.unregister_container_dns(container_id)
    }

    pub fn list_dns_entries(&self) -> Result<Vec<crate::icc::dns::DnsEntry>, String> {
        self.dns_manager.list_dns_entries()
    }

    pub fn allocate_next_ip(&self) -> Result<String, String> {
        // ELITE: Lock-free IP allocation using compare-and-swap
        let mut current_ip = self.config.next_ip.load(Ordering::Relaxed);
        loop {
            let next_ip = current_ip + 1;
            
            // Validate IP range (10.42.0.2 to 10.42.255.254)
            if next_ip > 65534 {  // 256 * 256 - 2 (avoid broadcast)
                return Err("IP address pool exhausted".to_string());
            }
            
            match self.config.next_ip.compare_exchange_weak(
                current_ip,
                next_ip,
                Ordering::Relaxed,
                Ordering::Relaxed
            ) {
                Ok(_) => {
                    // Successfully allocated IP
                    let subnet_a = 10;
                    let subnet_b = 42;
                    let subnet_c = (next_ip / 256) as u8;
                    let subnet_d = (next_ip % 256) as u8;
                    
                    let allocated_ip = format!("{}.{}.{}.{}", subnet_a, subnet_b, subnet_c, subnet_d);
                    ConsoleLogger::debug(&format!("Allocated IP: {} (index: {})", allocated_ip, next_ip));
                    return Ok(allocated_ip);
                }
                Err(actual) => {
                    // Another thread modified next_ip, retry with new value
                    current_ip = actual;
                }
            }
        }
    }

    pub fn cleanup_all_resources(&self) -> Result<(), String> {
        ConsoleLogger::info("🧹 [CLEANUP] Starting comprehensive network cleanup");
        
        // Cleanup DNS redirect rules
        self.dns_manager.cleanup_dns_rules()?;
        
        ConsoleLogger::success("✅ [CLEANUP] Network cleanup completed");
        Ok(())
    }

    // Convenience methods that delegate to sub-managers
    pub fn verify_bridge_attachment(&self, veth_name: &str) -> Result<(), String> {
        self.veth_manager.verify_bridge_attachment(veth_name)
    }

    pub fn get_interface_mac_address(&self, interface_name: &str) -> Result<String, String> {
        self.veth_manager.get_interface_mac_address(interface_name)
    }

    pub fn get_container_interface_mac_address(&self, container_pid: i32, interface_name: &str) -> Result<String, String> {
        self.veth_manager.get_container_interface_mac_address(container_pid, interface_name)
    }

    pub fn test_bidirectional_connectivity(&self, container_pid: i32, container_ip: &str, gateway_ip: &str) {
        self.diagnostics.test_bidirectional_connectivity(container_pid, container_ip, gateway_ip)
    }

    pub fn verify_container_network_ready(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        self.diagnostics.verify_container_network_ready(config, container_pid)
    }

    pub fn validate_container_namespace(&self, container_pid: i32) -> bool {
        self.security.validate_container_namespace(container_pid)
    }

    pub fn verify_dns_container_isolation(&self, container_pid: i32, expected_content: &str) -> bool {
        self.security.verify_dns_container_isolation(container_pid, expected_content)
    }
    
    /// Comprehensive network health monitoring service
    pub fn run_network_health_monitoring(&self) -> Result<NetworkHealthReport, String> {
        ConsoleLogger::info("🔍 Starting comprehensive network health monitoring...");
        
        let mut report = NetworkHealthReport::new();
        let start_time = std::time::SystemTime::now();
        
        // 1. Bridge attachment verification for all veth interfaces
        ConsoleLogger::debug("Verifying bridge attachments...");
        let bridge_check_result = self.verify_all_bridge_attachments();
        report.bridge_attachments = bridge_check_result;
        
        // 2. Interface MAC address tracking for security
        ConsoleLogger::debug("Collecting interface MAC addresses for security tracking...");
        let mac_tracking_result = self.collect_interface_mac_addresses();
        report.mac_addresses = mac_tracking_result;
        
        // 3. Bidirectional connectivity testing for active containers  
        ConsoleLogger::debug("Testing bidirectional connectivity...");
        let connectivity_result = self.test_all_container_connectivity();
        report.connectivity_tests = connectivity_result;
        
        // 4. Network readiness validation
        ConsoleLogger::debug("Validating network readiness...");
        let readiness_result = self.validate_all_network_readiness();
        report.readiness_checks = readiness_result;
        
        // 5. Container namespace validation
        ConsoleLogger::debug("Validating container namespaces...");
        let namespace_result = self.validate_all_container_namespaces();
        report.namespace_validations = namespace_result;
        
        let duration = start_time.elapsed().unwrap_or_default();
        report.total_duration_ms = duration.as_millis() as u64;
        
        ConsoleLogger::success(&format!("✅ Network health monitoring completed in {}ms", report.total_duration_ms));
        
        Ok(report)
    }
    
    /// Verify bridge attachments for all veth interfaces
    fn verify_all_bridge_attachments(&self) -> Vec<BridgeAttachmentCheck> {
        let mut results = Vec::new();
        
        // Get list of veth interfaces from command output
        if let Ok(result) = CommandExecutor::execute_shell("ip link show | grep veth") {
            for line in result.stdout.lines() {
                if let Some(veth_name) = self.extract_veth_name(line) {
                    let check_result = match self.verify_bridge_attachment(&veth_name) {
                        Ok(()) => BridgeAttachmentCheck {
                            veth_name: veth_name.clone(),
                            attached: true,
                            error_message: None,
                        },
                        Err(e) => BridgeAttachmentCheck {
                            veth_name: veth_name.clone(),
                            attached: false,
                            error_message: Some(e),
                        },
                    };
                    results.push(check_result);
                }
            }
        }
        
        results
    }
    
    /// Collect MAC addresses for all interfaces for security tracking
    fn collect_interface_mac_addresses(&self) -> Vec<InterfaceMacInfo> {
        let mut results = Vec::new();
        
        // Get bridge interface MAC
        if let Ok(bridge_mac) = self.get_interface_mac_address(&self.config.bridge_name) {
            results.push(InterfaceMacInfo {
                interface_name: self.config.bridge_name.clone(),
                mac_address: bridge_mac,
                interface_type: "bridge".to_string(),
                container_pid: None,
            });
        }
        
        // Get veth interface MACs
        if let Ok(result) = CommandExecutor::execute_shell("ip link show | grep veth") {
            for line in result.stdout.lines() {
                if let Some(veth_name) = self.extract_veth_name(line) {
                    if let Ok(mac) = self.get_interface_mac_address(&veth_name) {
                        results.push(InterfaceMacInfo {
                            interface_name: veth_name,
                            mac_address: mac,
                            interface_type: "veth".to_string(),
                            container_pid: None,
                        });
                    }
                }
            }
        }
        
        results
    }
    
    /// Test bidirectional connectivity for all active containers
    fn test_all_container_connectivity(&self) -> Vec<ConnectivityTestResult> {
        let results = Vec::new();
        
        // This would need to be integrated with the sync engine to get active containers
        // For now, we'll demonstrate the concept with placeholder logic
        
        ConsoleLogger::debug("Bidirectional connectivity testing requires container registry integration");
        
        results
    }
    
    /// Validate network readiness for all containers
    fn validate_all_network_readiness(&self) -> Vec<NetworkReadinessCheck> {
        let results = Vec::new();
        
        ConsoleLogger::debug("Network readiness validation requires container configuration integration");
        
        results
    }
    
    /// Validate container namespaces for all active containers
    fn validate_all_container_namespaces(&self) -> Vec<NamespaceValidationResult> {
        let mut results = Vec::new();
        
        // Get running container PIDs from system
        if let Ok(result) = CommandExecutor::execute_shell("pgrep -f quilt") {
            for line in result.stdout.lines() {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    let is_valid = self.validate_container_namespace(pid);
                    results.push(NamespaceValidationResult {
                        container_pid: pid,
                        namespace_valid: is_valid,
                        error_message: if is_valid { None } else { Some("Namespace validation failed".to_string()) },
                    });
                }
            }
        }
        
        results
    }
    
    /// Extract veth interface name from ip link output line
    fn extract_veth_name(&self, line: &str) -> Option<String> {
        // Parse lines like "123: veth-abc123@if124: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue master quilt0 state UP mode DEFAULT group default qlen 1000"
        if let Some(colon_pos) = line.find(':') {
            let after_colon = &line[colon_pos + 1..];
            if let Some(at_pos) = after_colon.find('@') {
                let veth_name = after_colon[..at_pos].trim();
                if veth_name.starts_with("veth") {
                    return Some(veth_name.to_string());
                }
            }
        }
        None
    }
}

/// Network health monitoring report
#[derive(Debug)]
#[allow(dead_code)]
pub struct NetworkHealthReport {
    pub bridge_attachments: Vec<BridgeAttachmentCheck>,
    pub mac_addresses: Vec<InterfaceMacInfo>,
    pub connectivity_tests: Vec<ConnectivityTestResult>,
    pub readiness_checks: Vec<NetworkReadinessCheck>,
    pub namespace_validations: Vec<NamespaceValidationResult>,
    pub total_duration_ms: u64,
}

#[allow(dead_code)]
impl NetworkHealthReport {
    pub fn new() -> Self {
        Self {
            bridge_attachments: Vec::new(),
            mac_addresses: Vec::new(),
            connectivity_tests: Vec::new(),
            readiness_checks: Vec::new(),
            namespace_validations: Vec::new(),
            total_duration_ms: 0,
        }
    }
    
    pub fn is_healthy(&self) -> bool {
        let bridge_healthy = self.bridge_attachments.iter().all(|check| check.attached);
        let namespaces_healthy = self.namespace_validations.iter().all(|check| check.namespace_valid);
        let connectivity_healthy = self.connectivity_tests.iter().all(|test| test.success);
        let readiness_healthy = self.readiness_checks.iter().all(|check| check.ready);
        
        bridge_healthy && namespaces_healthy && connectivity_healthy && readiness_healthy
    }
    
    pub fn get_issues_count(&self) -> usize {
        let bridge_issues = self.bridge_attachments.iter().filter(|check| !check.attached).count();
        let namespace_issues = self.namespace_validations.iter().filter(|check| !check.namespace_valid).count();
        let connectivity_issues = self.connectivity_tests.iter().filter(|test| !test.success).count();
        let readiness_issues = self.readiness_checks.iter().filter(|check| !check.ready).count();
        
        bridge_issues + namespace_issues + connectivity_issues + readiness_issues
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct BridgeAttachmentCheck {
    pub veth_name: String,
    pub attached: bool,
    pub error_message: Option<String>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct InterfaceMacInfo {
    pub interface_name: String,
    pub mac_address: String,
    pub interface_type: String,
    pub container_pid: Option<i32>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct ConnectivityTestResult {
    pub container_pid: i32,
    pub container_ip: String,
    pub gateway_ip: String,
    pub success: bool,
    pub response_time_ms: Option<u64>,
    pub error_message: Option<String>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct NetworkReadinessCheck {
    pub container_id: String,
    pub container_pid: i32,
    pub ready: bool,
    pub error_message: Option<String>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct NamespaceValidationResult {
    pub container_pid: i32,
    pub namespace_valid: bool,
    pub error_message: Option<String>,
}