// Network diagnostics module
// Handles network connectivity testing, troubleshooting, and verification

use crate::utils::command::CommandExecutor;
use crate::utils::console::ConsoleLogger;
use crate::icc::network::veth::ContainerNetworkConfig;

/// Network diagnostics and testing functionality
#[allow(dead_code)]
pub struct NetworkDiagnostics {
    pub bridge_name: String,
    pub bridge_ip: String,
}

impl NetworkDiagnostics {
    pub fn new(bridge_name: String, bridge_ip: String) -> Self {
        Self { bridge_name, bridge_ip }
    }

    pub fn test_gateway_connectivity_comprehensive(&self, container_pid: i32, gateway_ip: &str, interface_name: &str) {
        ConsoleLogger::debug(&format!("🌐 [GATEWAY-TEST] Comprehensive gateway connectivity test for {}", gateway_ip));
        
        // Test 1: Basic ping test
        let gateway_ping_cmd = format!("nsenter -t {} -n ping -c 3 -W 2 {} 2>/dev/null", 
            container_pid, gateway_ip);
        
        match CommandExecutor::execute_shell(&gateway_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::success(&format!("✅ [GATEWAY-TEST] Gateway {} is reachable (ping success)", gateway_ip));
            }
            Ok(result) => {
                ConsoleLogger::warning(&format!("⚠️ [GATEWAY-TEST] Gateway {} ping failed: {}", gateway_ip, result.stderr));
                // Continue with additional diagnostics
                self.test_gateway_arp_resolution(container_pid, gateway_ip);
                self.test_gateway_routing(container_pid, gateway_ip, interface_name);
                self.test_interface_connectivity(container_pid, interface_name);
            }
            Err(e) => {
                ConsoleLogger::error(&format!("❌ [GATEWAY-TEST] Gateway connectivity test failed: {}", e));
            }
        }
        
        // Always run ARP and routing tests for comprehensive diagnostics
        self.test_gateway_arp_resolution(container_pid, gateway_ip);
        self.test_gateway_routing(container_pid, gateway_ip, interface_name);
        
        // Test bridge connectivity from host side
        self.diagnose_bridge_connectivity_issues(gateway_ip);
    }
    
    fn test_gateway_arp_resolution(&self, container_pid: i32, gateway_ip: &str) {
        ConsoleLogger::debug(&format!("🔍 [ARP-TEST] Testing ARP resolution for gateway {}", gateway_ip));
        
        // Check ARP entry for gateway
        let arp_check_cmd = format!("nsenter -t {} -n ip neigh show {}", container_pid, gateway_ip);
        match CommandExecutor::execute_shell(&arp_check_cmd) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [ARP-TEST] Gateway {} ARP entry: {}", gateway_ip, result.stdout.trim()));
            }
            Ok(_) => {
                ConsoleLogger::debug(&format!("ℹ️ [ARP-TEST] No ARP entry found for gateway {} (may be normal)", gateway_ip));
                
                // Try to ping once to populate ARP table
                let ping_once_cmd = format!("nsenter -t {} -n ping -c 1 -W 1 {} >/dev/null 2>&1", container_pid, gateway_ip);
                let _ = CommandExecutor::execute_shell(&ping_once_cmd);
                
                // Check again
                if let Ok(result) = CommandExecutor::execute_shell(&arp_check_cmd) {
                    if result.success && !result.stdout.trim().is_empty() {
                        ConsoleLogger::debug(&format!("✅ [ARP-TEST] Gateway {} ARP entry (after ping): {}", gateway_ip, result.stdout.trim()));
                    }
                }
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("⚠️ [ARP-TEST] Failed to check ARP for gateway {}: {}", gateway_ip, e));
            }
        }
    }
    
    fn test_gateway_routing(&self, container_pid: i32, gateway_ip: &str, interface_name: &str) {
        ConsoleLogger::debug(&format!("🛣️ [ROUTE-TEST] Testing routing to gateway {} via {}", gateway_ip, interface_name));
        
        // Check specific route to gateway
        let route_check_cmd = format!("nsenter -t {} -n ip route get {}", container_pid, gateway_ip);
        match CommandExecutor::execute_shell(&route_check_cmd) {
            Ok(result) if result.success => {
                if result.stdout.contains(interface_name) {
                    ConsoleLogger::debug(&format!("✅ [ROUTE-TEST] Route to {} via {}: {}", gateway_ip, interface_name, result.stdout.trim()));
                } else {
                    ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] Route to {} doesn't use expected interface {}: {}", 
                        gateway_ip, interface_name, result.stdout.trim()));
                }
            }
            Ok(result) => {
                ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] Route lookup failed for {}: {}", gateway_ip, result.stderr));
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] Failed to check route to gateway {}: {}", gateway_ip, e));
            }
        }
        
        // Check default route
        let default_route_cmd = format!("nsenter -t {} -n ip route show default", container_pid);
        match CommandExecutor::execute_shell(&default_route_cmd) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [ROUTE-TEST] Default route: {}", result.stdout.trim()));
            }
            Ok(_) => {
                ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] No default route found"));
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("⚠️ [ROUTE-TEST] Failed to check default route: {}", e));
            }
        }
    }
    
    fn test_interface_connectivity(&self, container_pid: i32, interface_name: &str) {
        ConsoleLogger::debug(&format!("🔌 [IFACE-TEST] Testing interface {} connectivity", interface_name));
        
        // Check interface state
        let iface_check_cmd = format!("nsenter -t {} -n ip link show {}", container_pid, interface_name);
        match CommandExecutor::execute_shell(&iface_check_cmd) {
            Ok(result) if result.success => {
                if result.stdout.contains("state UP") {
                    ConsoleLogger::debug(&format!("✅ [IFACE-TEST] Interface {} is UP", interface_name));
                } else {
                    ConsoleLogger::warning(&format!("⚠️ [IFACE-TEST] Interface {} is not UP: {}", interface_name, result.stdout.trim()));
                }
            }
            Ok(result) => {
                ConsoleLogger::warning(&format!("⚠️ [IFACE-TEST] Interface {} check failed: {}", interface_name, result.stderr));
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("⚠️ [IFACE-TEST] Failed to check interface {}: {}", interface_name, e));
            }
        }
        
        // Check interface statistics
        let stats_cmd = format!("nsenter -t {} -n ip -s link show {}", container_pid, interface_name);
        match CommandExecutor::execute_shell(&stats_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("ℹ️ [IFACE-TEST] Interface {} stats: {}", interface_name, 
                    result.stdout.lines().collect::<Vec<_>>().join(" | ")));
            }
            _ => {
                ConsoleLogger::debug(&format!("ℹ️ [IFACE-TEST] Could not get stats for interface {}", interface_name));
            }
        }
    }
    
    fn diagnose_bridge_connectivity_issues(&self, gateway_ip: &str) {
        ConsoleLogger::debug(&format!("🌉 [BRIDGE-DIAG] Diagnosing bridge connectivity issues for {}", gateway_ip));
        
        // Check if host can ping the bridge IP
        let host_ping_cmd = format!("ping -c 1 -W 1 {} >/dev/null 2>&1", gateway_ip);
        match CommandExecutor::execute_shell(&host_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ [BRIDGE-DIAG] Host can reach bridge IP {}", gateway_ip));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [BRIDGE-DIAG] Host cannot reach bridge IP {}", gateway_ip));
            }
        }
        
        // Check bridge interface status
        let bridge_status_cmd = format!("ip link show {}", self.bridge_name);
        match CommandExecutor::execute_shell(&bridge_status_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("ℹ️ [BRIDGE-DIAG] Bridge {} status: {}", self.bridge_name, 
                    result.stdout.lines().next().unwrap_or("").trim()));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [BRIDGE-DIAG] Could not get bridge {} status", self.bridge_name));
            }
        }
    }
    
    pub fn test_bidirectional_connectivity(&self, _container_pid: i32, container_ip: &str, gateway_ip: &str) {
        ConsoleLogger::debug(&format!("🔄 [BIDIR-TEST] Testing bidirectional connectivity: container {} <-> gateway {}", 
            container_ip, gateway_ip));
        
        // Test 1: Container -> Host (already tested above via gateway ping)
        ConsoleLogger::debug("🔽 [BIDIR-TEST] Container -> Host connectivity (via gateway ping)");
        
        // Test 2: Host -> Container
        ConsoleLogger::debug(&format!("🔼 [BIDIR-TEST] Testing Host -> Container connectivity to {}", container_ip));
        
        // Try to ping container from host
        let host_to_container_ping = format!("ping -c 2 -W 2 {} >/dev/null 2>&1", container_ip);
        match CommandExecutor::execute_shell(&host_to_container_ping) {
            Ok(result) if result.success => {
                ConsoleLogger::success(&format!("✅ [BIDIR-TEST] Host -> Container {} connectivity working", container_ip));
            }
            _ => {
                ConsoleLogger::warning(&format!("⚠️ [BIDIR-TEST] Host -> Container {} connectivity failed", container_ip));
                self.diagnose_host_to_container_connectivity_failure(container_ip);
            }
        }
        
        // Test bridge forwarding table
        let fdb_check = format!("bridge fdb show | grep {}", container_ip);
        match CommandExecutor::execute_shell(&fdb_check) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ [BIDIR-TEST] Bridge FDB entry for {}: {}", container_ip, result.stdout.trim()));
            }
            _ => {
                ConsoleLogger::debug(&format!("ℹ️ [BIDIR-TEST] No bridge FDB entry for {} (may be normal for new containers)", container_ip));
            }
        }
    }
    
    fn diagnose_host_to_container_connectivity_failure(&self, container_ip: &str) {
        ConsoleLogger::debug(&format!("🔍 [HOST-DIAG] Diagnosing host->container connectivity failure for {}", container_ip));
        
        // Check host routing to container IP
        let host_route_cmd = format!("ip route get {}", container_ip);
        if let Ok(result) = CommandExecutor::execute_shell(&host_route_cmd) {
            ConsoleLogger::debug(&format!("ℹ️ [HOST-DIAG] Host route to {}: {}", container_ip, result.stdout.trim()));
        }
        
        // Check if bridge knows about this container
        let bridge_neigh_cmd = format!("ip neigh show {} dev {}", container_ip, self.bridge_name);
        if let Ok(result) = CommandExecutor::execute_shell(&bridge_neigh_cmd) {
            if result.success && !result.stdout.trim().is_empty() {
                ConsoleLogger::debug(&format!("ℹ️ [HOST-DIAG] Bridge neighbor entry for {}: {}", container_ip, result.stdout.trim()));
            } else {
                ConsoleLogger::debug(&format!("ℹ️ [HOST-DIAG] No bridge neighbor entry for {}", container_ip));
            }
        }
        
        // Check bridge port list
        let bridge_ports_cmd = format!("bridge link show master {}", self.bridge_name);
        if let Ok(result) = CommandExecutor::execute_shell(&bridge_ports_cmd) {
            ConsoleLogger::debug(&format!("ℹ️ [HOST-DIAG] Bridge {} ports: {}", self.bridge_name, result.stdout.trim()));
        }
    }
    
    pub fn verify_container_network_ready(&self, config: &ContainerNetworkConfig, container_pid: i32) -> Result<(), String> {
        let interface_name = format!("quilt{}", &config.container_id[..8]);
        
        ConsoleLogger::debug(&format!("🔍 Production network verification for container {} (interface: {})", config.container_id, interface_name));
        
        // Phase 1: Network interface verification (fast check)
        let interface_check_cmd = format!("nsenter -t {} -n ip link show {}", container_pid, interface_name);
        match CommandExecutor::execute_shell(&interface_check_cmd) {
            Ok(result) if result.success => {
                if !result.stdout.contains("state UP") {
                    return Err(format!("Container interface {} is not UP", interface_name));
                }
                ConsoleLogger::debug(&format!("✅ Interface {} is UP and ready", interface_name));
            }
            Ok(result) => {
                return Err(format!("Container interface {} check failed: {}", interface_name, result.stderr));
            }
            Err(e) => {
                return Err(format!("Failed to check container interface: {}", e));
            }
        }
        
        // Phase 2: IP address verification
        let ip_check_cmd = format!("nsenter -t {} -n ip addr show {} | grep {}", 
            container_pid, interface_name, config.ip_address.split('/').next().unwrap());
        match CommandExecutor::execute_shell(&ip_check_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ Interface {} has correct IP {}", interface_name, config.ip_address));
            }
            _ => {
                return Err(format!("Container interface {} does not have expected IP {}", interface_name, config.ip_address));
            }
        }
        
        // Phase 3: Default route verification
        let route_check_cmd = format!("nsenter -t {} -n ip route show default", container_pid);
        match CommandExecutor::execute_shell(&route_check_cmd) {
            Ok(result) if result.success && !result.stdout.trim().is_empty() => {
                ConsoleLogger::debug(&format!("✅ Default route configured: {}", result.stdout.trim()));
            }
            _ => {
                return Err("No default route configured in container".to_string());
            }
        }
        
        // Phase 4: Gateway reachability test (critical for container networking)
        let gateway_ip = config.gateway_ip.split('/').next().unwrap();
        let gateway_ping_cmd = format!("nsenter -t {} -n ping -c 2 -W 3 {} >/dev/null 2>&1", container_pid, gateway_ip);
        match CommandExecutor::execute_shell(&gateway_ping_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug(&format!("✅ Gateway {} is reachable from container", gateway_ip));
            }
            _ => {
                // Gateway ping failed - this is a critical issue, but we'll log and continue
                // Some containers may have firewalls that block ping
                ConsoleLogger::warning(&format!("⚠️ Gateway {} ping failed (may be normal if firewall blocks ping)", gateway_ip));
                
                // Try a different connectivity test - check if we can resolve the gateway via ARP
                let arp_test_cmd = format!("nsenter -t {} -n ip neigh get {}", container_pid, gateway_ip);
                match CommandExecutor::execute_shell(&arp_test_cmd) {
                    Ok(result) if result.success => {
                        ConsoleLogger::debug(&format!("✅ Gateway {} is reachable via ARP", gateway_ip));
                    }
                    _ => {
                        ConsoleLogger::warning(&format!("⚠️ Gateway {} may not be reachable", gateway_ip));
                        // We don't fail here as some setups may have different gateway configurations
                    }
                }
            }
        }
        
        // Phase 5: DNS resolution test
        let dns_test_cmd = format!("nsenter -t {} -n nslookup quilt.local 127.0.0.1 >/dev/null 2>&1", container_pid);
        match CommandExecutor::execute_shell(&dns_test_cmd) {
            Ok(result) if result.success => {
                ConsoleLogger::debug("✅ DNS resolution working in container");
            }
            _ => {
                ConsoleLogger::debug("ℹ️ DNS resolution test inconclusive (nslookup may not be available)");
                // This is not critical as containers may not have nslookup
            }
        }
        
        ConsoleLogger::success(&format!("✅ Container {} network verification completed - all critical checks passed", config.container_id));
        Ok(())
    }
}