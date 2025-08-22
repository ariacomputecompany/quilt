use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::Path;
use nix::unistd::Pid;
use crate::utils::{ConsoleLogger, CommandExecutor};
use crate::daemon::cgroup::CgroupManager;
use crate::icc::network::ContainerNetworkConfig;
use crate::utils::FileSystemUtils;

/// Thread-safe comprehensive resource manager for container lifecycle
pub struct ResourceManager {
    /// Track active mounts per container (thread-safe)
    active_mounts: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Track network interfaces per container (thread-safe)
    network_interfaces: Arc<Mutex<HashMap<String, ContainerNetworkConfig>>>,
}

impl ResourceManager {
    pub fn new() -> Self {
        ResourceManager {
            active_mounts: Arc::new(Mutex::new(HashMap::new())),
            network_interfaces: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register mounts for a container (thread-safe)
    pub fn register_mounts(&self, container_id: &str, mounts: Vec<String>) {
        ConsoleLogger::debug(&format!("[RESOURCE] Registering {} mounts for container {}", mounts.len(), container_id));
        if let Ok(mut active_mounts) = self.active_mounts.lock() {
            active_mounts.insert(container_id.to_string(), mounts);
        }
    }

    /// Register network configuration for a container (thread-safe)
    pub fn register_network(&self, container_id: &str, network_config: ContainerNetworkConfig) {
        ConsoleLogger::debug(&format!("[RESOURCE] Registering network config for container {}", container_id));
        if let Ok(mut network_interfaces) = self.network_interfaces.lock() {
            network_interfaces.insert(container_id.to_string(), network_config);
        }
    }

    /// Cleanup all resources for a container (thread-safe)
    pub fn cleanup_container_resources(&self, container_id: &str, container_pid: Option<Pid>) -> Result<(), String> {
        ConsoleLogger::progress(&format!("🧹 Cleaning up all resources for container: {}", container_id));

        let mut cleanup_errors = Vec::new();

        // 1. Cleanup network resources (thread-safe)
        let network_config = if let Ok(mut network_interfaces) = self.network_interfaces.lock() {
            network_interfaces.remove(container_id)
        } else {
            None
        };
        
        if let Some(network_config) = network_config {
            if let Err(e) = self.cleanup_network_resources(&network_config, container_pid) {
                cleanup_errors.push(format!("Network cleanup failed: {}", e));
            }
        }

        // 2. Cleanup mount namespaces (thread-safe)
        let mounts = if let Ok(mut active_mounts) = self.active_mounts.lock() {
            active_mounts.remove(container_id)
        } else {
            None
        };
        
        if let Some(mounts) = mounts {
            if let Err(e) = self.cleanup_mount_resources(container_id, &mounts, container_pid) {
                cleanup_errors.push(format!("Mount cleanup failed: {}", e));
            }
        }

        // 3. Cleanup cgroups
        if let Err(e) = self.cleanup_cgroup_resources(container_id) {
            cleanup_errors.push(format!("Cgroup cleanup failed: {}", e));
        }

        // 4. Final rootfs cleanup (retry with proper ordering)
        let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
        if let Err(e) = self.cleanup_rootfs_resources_safe(&rootfs_path) {
            cleanup_errors.push(format!("Rootfs cleanup failed: {}", e));
        }

        if cleanup_errors.is_empty() {
            ConsoleLogger::success(&format!("✅ All resources cleaned up for container {}", container_id));
            Ok(())
        } else {
            let error_msg = format!("Partial cleanup failures: {}", cleanup_errors.join("; "));
            ConsoleLogger::warning(&error_msg);
            Err(error_msg)
        }
    }

    /// Cleanup network resources (veth pairs, network namespaces)
    fn cleanup_network_resources(&self, network_config: &ContainerNetworkConfig, container_pid: Option<Pid>) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🌐 Cleaning up network resources: {}", network_config.veth_host_name));

        // Clean up veth pair - delete the host side, container side will be cleaned up automatically
        let cleanup_host_veth = format!("ip link delete {} 2>/dev/null || true", network_config.veth_host_name);
        if let Err(e) = CommandExecutor::execute_shell(&cleanup_host_veth) {
            ConsoleLogger::warning(&format!("Failed to delete host veth {}: {}", network_config.veth_host_name, e));
        } else {
            ConsoleLogger::debug(&format!("Deleted host veth interface: {}", network_config.veth_host_name));
        }

        // Clean up container side veth if container is still running
        if let Some(pid) = container_pid {
            // SECURITY NOTE: Safe cleanup operation - only deletes interface, with || true fallback
            let cleanup_container_veth = format!("nsenter -t {} -n ip link delete {} 2>/dev/null || true", pid.as_raw(), network_config.veth_container_name);
            if let Err(e) = CommandExecutor::execute_shell(&cleanup_container_veth) {
                ConsoleLogger::debug(&format!("Container veth cleanup attempt failed (expected if container exited): {}", e));
            }
        }

        // Clean up any custom interface names
        let interface_name = format!("qnet{}", &network_config.container_id[..8]);
        if let Some(pid) = container_pid {
            let cleanup_custom_interface = format!("nsenter -t {} -n ip link delete {} 2>/dev/null || true", pid.as_raw(), interface_name);
            if let Err(e) = CommandExecutor::execute_shell(&cleanup_custom_interface) {
                ConsoleLogger::debug(&format!("Custom interface cleanup attempt failed (expected if container exited): {}", e));
            }
        }

        ConsoleLogger::success("Network resources cleaned up");
        Ok(())
    }

    /// Cleanup mount namespaces
    fn cleanup_mount_resources(&self, container_id: &str, mounts: &[String], container_pid: Option<Pid>) -> Result<(), String> {
        ConsoleLogger::debug(&format!("📁 Cleaning up {} mount points for container {}", mounts.len(), container_id));

        // If container is still running, try to unmount from within the namespace
        if let Some(pid) = container_pid {
            for mount_point in mounts.iter().rev() { // Reverse order for proper unmounting
                let unmount_cmd = format!("nsenter -t {} -m umount -l {} 2>/dev/null || true", pid.as_raw(), mount_point);
                if let Err(e) = CommandExecutor::execute_shell(&unmount_cmd) {
                    ConsoleLogger::debug(&format!("Namespace unmount failed for {}: {} (may be expected)", mount_point, e));
                }
            }
        }

        // Force unmount from host side with lazy unmount
        let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
        let common_mounts = vec![
            format!("{}/proc", rootfs_path),
            format!("{}/sys", rootfs_path),
            format!("{}/dev/pts", rootfs_path),
            rootfs_path.clone(), // The rootfs bind mount itself
        ];

        for mount_point in common_mounts.iter().rev() {
            if Path::new(mount_point).exists() {
                // Try regular unmount first
                let unmount_cmd = format!("umount {} 2>/dev/null || true", mount_point);
                let _ = CommandExecutor::execute_shell(&unmount_cmd);

                // Force lazy unmount as fallback
                let lazy_unmount_cmd = format!("umount -l {} 2>/dev/null || true", mount_point);
                if let Err(e) = CommandExecutor::execute_shell(&lazy_unmount_cmd) {
                    ConsoleLogger::debug(&format!("Lazy unmount failed for {}: {}", mount_point, e));
                }
            }
        }

        ConsoleLogger::success("Mount resources cleaned up");
        Ok(())
    }

    /// Cleanup cgroup resources
    fn cleanup_cgroup_resources(&self, container_id: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("⚙️ Cleaning up cgroup resources for container {}", container_id));
        
        let cgroup_manager = CgroupManager::new(container_id.to_string());
        cgroup_manager.cleanup()
    }

    /// Cleanup rootfs resources with improved mount ordering
    fn cleanup_rootfs_resources_safe(&self, rootfs_path: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("📂 Cleaning up rootfs: {}", rootfs_path));

        if !Path::new(rootfs_path).exists() {
            ConsoleLogger::debug("Rootfs path doesn't exist, skipping cleanup");
            return Ok(());
        }

        // Step 1: Force unmount all nested mounts in proper order (most specific first)
        let nested_mounts = vec![
            format!("{}/proc", rootfs_path),
            format!("{}/sys", rootfs_path),
            format!("{}/dev/pts", rootfs_path),
            format!("{}/dev", rootfs_path),
        ];

        for mount_point in nested_mounts {
            if Path::new(&mount_point).exists() {
                let umount_cmd = format!("umount -l '{}' 2>/dev/null || true", mount_point);
                let _ = CommandExecutor::execute_shell(&umount_cmd);
                
                // Give kernel time to process unmount
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }

        // Step 2: Try graceful removal first
        match FileSystemUtils::remove_path(rootfs_path) {
            Ok(()) => {
                ConsoleLogger::success(&format!("✅ Rootfs cleanup successful: {}", rootfs_path));
                return Ok(());
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Normal rootfs removal failed, trying force removal: {}", e));
            }
        }

        // Step 3: Force cleanup with more aggressive unmounting
        let force_umount_cmd = format!("umount -f -l '{}' 2>/dev/null || true", rootfs_path);
        let _ = CommandExecutor::execute_shell(&force_umount_cmd);
        
        // Wait a bit longer for force unmount to complete
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Step 4: Force remove directory
        let force_remove_cmd = format!("rm -rf '{}'", rootfs_path);
        match CommandExecutor::execute_shell(&force_remove_cmd) {
            Ok(_) => {
                ConsoleLogger::success(&format!("✅ Force rootfs cleanup successful: {}", rootfs_path));
                Ok(())
            }
            Err(e) => {
                // Last resort - try with sudo if available
                let sudo_remove_cmd = format!("sudo rm -rf '{}' 2>/dev/null || rm -rf '{}'", rootfs_path, rootfs_path);
                match CommandExecutor::execute_shell(&sudo_remove_cmd) {
                    Ok(_) => {
                        ConsoleLogger::success(&format!("✅ Emergency rootfs cleanup successful: {}", rootfs_path));
                        Ok(())
                    }
                    Err(e2) => {
                        Err(format!("Failed to remove rootfs {}: {} (emergency attempt: {})", rootfs_path, e, e2))
                    }
                }
            }
        }
    }

    /// Emergency cleanup for a container (thread-safe)
    pub fn emergency_cleanup(&self, container_id: &str) -> Result<(), String> {
        ConsoleLogger::warning(&format!("🚨 Emergency cleanup for container: {}", container_id));

        // Kill any remaining processes
        let kill_cmd = format!("pkill -9 -f 'quilt.*{}' 2>/dev/null || true", container_id);
        let _ = CommandExecutor::execute_shell(&kill_cmd);

        // Force cleanup all resources
        self.cleanup_container_resources(container_id, None)?;

        ConsoleLogger::success(&format!("Emergency cleanup completed for: {}", container_id));
        Ok(())
    }

    /// Cleanup all resources for all containers (system-wide cleanup, thread-safe)
    pub fn cleanup_all_resources(&self) -> Result<(), String> {
        ConsoleLogger::warning("🧹 Performing system-wide resource cleanup");

        // Get all container IDs (thread-safe)
        let container_ids: Vec<String> = if let Ok(active_mounts) = self.active_mounts.lock() {
            active_mounts.keys().cloned().collect()
        } else {
            vec![]
        };
        
        let mut cleanup_errors = Vec::new();
        for container_id in container_ids {
            if let Err(e) = self.emergency_cleanup(&container_id) {
                cleanup_errors.push(format!("Failed to cleanup {}: {}", container_id, e));
            }
        }

        // Clean up any leftover veth interfaces
        let cleanup_veth_cmd = "ip link show | grep 'veth-\\|qnet' | cut -d: -f2 | cut -d@ -f1 | xargs -I {} ip link delete {} 2>/dev/null || true";
        let _ = CommandExecutor::execute_shell(cleanup_veth_cmd);

        // Force unmount any remaining quilt mounts
        let cleanup_mounts_cmd = "umount -l /tmp/quilt-containers/*/proc /tmp/quilt-containers/*/sys 2>/dev/null || true";
        let _ = CommandExecutor::execute_shell(cleanup_mounts_cmd);

        // Remove all container directories
        let cleanup_dirs_cmd = "rm -rf /tmp/quilt-containers/* 2>/dev/null || true";
        let _ = CommandExecutor::execute_shell(cleanup_dirs_cmd);

        if cleanup_errors.is_empty() {
            ConsoleLogger::success("✅ System-wide resource cleanup completed");
            Ok(())
        } else {
            let error_msg = format!("Some cleanup operations failed: {}", cleanup_errors.join("; "));
            ConsoleLogger::warning(&error_msg);
            Err(error_msg)
        }
    }
}

/// Thread-safe singleton resource manager using proper synchronization
use std::sync::Once;
static mut RESOURCE_MANAGER: Option<Arc<ResourceManager>> = None;
static RESOURCE_MANAGER_INIT: Once = Once::new();

impl ResourceManager {
    /// Get the global resource manager instance (thread-safe)
    pub fn global() -> Arc<ResourceManager> {
        unsafe {
            RESOURCE_MANAGER_INIT.call_once(|| {
                RESOURCE_MANAGER = Some(Arc::new(ResourceManager::new()));
            });
            RESOURCE_MANAGER.as_ref().unwrap().clone()
        }
    }
} 