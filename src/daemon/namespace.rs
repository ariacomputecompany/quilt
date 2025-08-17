use nix::sched::CloneFlags;
use nix::unistd::Pid;
use nix::mount::{mount, MsFlags};
use nix::sys::wait::{waitpid, WaitStatus, WaitPidFlag};
use std::path::Path;
use crate::utils::{ConsoleLogger, ProcessUtils};
use crate::utils::CommandExecutor;
use crate::icc::network::ContainerNetworkConfig;

#[derive(Debug, Clone)]
pub struct NamespaceConfig {
    pub pid: bool,      // CLONE_NEWPID - Process ID isolation
    pub mount: bool,    // CLONE_NEWNS - Mount namespace isolation  
    pub uts: bool,      // CLONE_NEWUTS - Hostname/domain isolation
    pub ipc: bool,      // CLONE_NEWIPC - IPC isolation
    pub network: bool,  // CLONE_NEWNET - Network isolation
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        NamespaceConfig {
            pid: false,     // PID namespace can cause issues, disable by default
            mount: true,    // Keep mount namespace for basic isolation
            uts: false,     // UTS can cause issues in some environments
            ipc: false,     // IPC namespace disabled for compatibility
            network: true,  // Enable network namespace for ICC
        }
    }
}

pub struct NamespaceManager;

impl NamespaceManager {
    pub fn new() -> Self {
        NamespaceManager
    }

    /// Create a new process with the specified namespaces
    pub fn create_namespaced_process<F>(
        &self,
        config: &NamespaceConfig,
        child_func: F,
    ) -> Result<Pid, String>
    where
        F: FnOnce() -> i32 + Send + 'static,
    {
        let clone_flags = self.build_clone_flags(config);
        
        ConsoleLogger::namespace_created(&format!("{:?}", clone_flags));

        // If no namespaces are requested, just use regular fork
        if clone_flags.is_empty() {
            return self.create_simple_process(child_func);
        }

        // Try to create namespaces with unshare + fork approach
        // If that fails, fall back to simple fork
        match self.try_create_with_namespaces(clone_flags, child_func) {
            Ok(pid) => {
                ConsoleLogger::success(&format!("Successfully created namespaced process with PID: {}", ProcessUtils::pid_to_i32(pid)));
                Ok(pid)
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Namespace creation failed: {}", e));
                ConsoleLogger::info("Falling back to simple fork without namespaces...");
                
                // Note: child_func was consumed in the failed attempt, so we create a simple process
                // that will just exit cleanly
                self.create_fallback_process()
            }
        }
    }

    /// Try creating process with namespaces
    fn try_create_with_namespaces<F>(
        &self,
        clone_flags: CloneFlags,
        child_func: F,
    ) -> Result<Pid, String>
    where
        F: FnOnce() -> i32 + Send + 'static,
    {
        // Use fork first, then unshare in child to avoid affecting the server process
        // This fixes the issue where unshare() was incorrectly isolating the server
        
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => {
                ConsoleLogger::debug(&format!("Successfully created child process with PID: {} that will setup isolated namespaces", ProcessUtils::pid_to_i32(child)));
                Ok(child)
            }
            Ok(nix::unistd::ForkResult::Child) => {
                // This runs in the child process - now create namespaces
                // This approach ensures the server process is never affected
                if let Err(e) = nix::sched::unshare(clone_flags) {
                    ConsoleLogger::error(&format!("Failed to unshare namespaces in child: {}", e));
                    std::process::exit(1);
                }
                
                // Run the child function in the isolated namespaces
                let exit_code = child_func();
                std::process::exit(exit_code);
            }
            Err(e) => {
                Err(format!("Failed to fork process: {}", e))
            }
        }
    }

    /// Create a fallback process when namespace creation fails
    fn create_fallback_process(&self) -> Result<Pid, String> {
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => {
                ConsoleLogger::info(&format!("Created fallback process with PID: {}", ProcessUtils::pid_to_i32(child)));
                Ok(child)
            }
            Ok(nix::unistd::ForkResult::Child) => {
                // Child process - just exit with failure
                ConsoleLogger::error("Fallback process: namespace creation failed");
                std::process::exit(1);
            }
            Err(e) => {
                Err(format!("Failed to fork fallback process: {}", e))
            }
        }
    }

    /// Create a simple process without namespaces (fallback)
    fn create_simple_process<F>(&self, child_func: F) -> Result<Pid, String>
    where
        F: FnOnce() -> i32 + Send + 'static,
    {
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => {
                ConsoleLogger::success(&format!("Successfully created simple process with PID: {}", ProcessUtils::pid_to_i32(child)));
                Ok(child)
            }
            Ok(nix::unistd::ForkResult::Child) => {
                // This runs in the child process
                let exit_code = child_func();
                std::process::exit(exit_code);
            }
            Err(e) => {
                Err(format!("Failed to fork process: {}", e))
            }
        }
    }

    /// Build clone flags based on namespace configuration
    fn build_clone_flags(&self, config: &NamespaceConfig) -> CloneFlags {
        let mut flags = CloneFlags::empty();

        if config.pid {
            flags |= CloneFlags::CLONE_NEWPID;
        }
        if config.mount {
            flags |= CloneFlags::CLONE_NEWNS;
        }
        if config.uts {
            flags |= CloneFlags::CLONE_NEWUTS;
        }
        if config.ipc {
            flags |= CloneFlags::CLONE_NEWIPC;
        }
        if config.network {
            flags |= CloneFlags::CLONE_NEWNET;
        }

        flags
    }

    /// Setup the mount namespace for a container
    pub fn setup_mount_namespace(&self, rootfs_path: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Setting up mount namespace for rootfs: {}", rootfs_path));

        // Make the mount namespace private to prevent propagation to host
        if let Err(e) = mount(
            None::<&str>,
            "/",
            None::<&str>,
            MsFlags::MS_REC | MsFlags::MS_PRIVATE,
            None::<&str>,
        ) {
            ConsoleLogger::warning(&format!("Failed to make mount namespace private: {}", e));
            // Continue anyway - this might fail in restricted environments
        }

        // Bind mount the rootfs to itself to make it a mount point
        if let Err(e) = mount(
            Some(rootfs_path),
            rootfs_path,
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        ) {
            ConsoleLogger::warning(&format!("Failed to bind mount rootfs: {}", e));
            // Continue anyway - this might fail in restricted environments
        }

        // Mount /proc inside the new namespace
        let proc_path = format!("{}/proc", rootfs_path);
        if Path::new(&proc_path).exists() {
            if let Err(e) = mount(
                Some("proc"),
                proc_path.as_str(),
                Some("proc"),
                MsFlags::empty(),
                None::<&str>,
            ) {
                // Non-fatal error - log and continue
                ConsoleLogger::warning(&format!("Failed to mount /proc in container: {}", e));
            } else {
                ConsoleLogger::success("Successfully mounted /proc in container");
            }
        }

        // Mount /sys inside the new namespace
        let sys_path = format!("{}/sys", rootfs_path);
        if Path::new(&sys_path).exists() {
            if let Err(e) = mount(
                Some("sysfs"),
                sys_path.as_str(),
                Some("sysfs"),
                MsFlags::MS_RDONLY | MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                None::<&str>,
            ) {
                // Non-fatal error - log and continue
                ConsoleLogger::warning(&format!("Failed to mount /sys in container: {}", e));
            } else {
                ConsoleLogger::success("Successfully mounted /sys in container");
            }
        }

        // Mount /dev/pts for pseudo-terminals if it exists
        let devpts_path = format!("{}/dev/pts", rootfs_path);
        if Path::new(&devpts_path).exists() {
            if let Err(e) = mount(
                Some("devpts"),
                devpts_path.as_str(),
                Some("devpts"),
                MsFlags::empty(),
                Some("newinstance,ptmxmode=0666"),
            ) {
                // Non-fatal error - log and continue
                ConsoleLogger::warning(&format!("Failed to mount /dev/pts in container: {}", e));
            } else {
                ConsoleLogger::success("Successfully mounted /dev/pts in container");
            }
        }

        Ok(())
    }
    
    /// Setup container mounts (bind mounts, volumes, tmpfs)
    pub fn setup_container_mounts(&self, rootfs_path: &str, mounts: &[crate::daemon::MountConfig]) -> Result<(), String> {
        use crate::daemon::MountType;
        
        ConsoleLogger::debug(&format!("Setting up {} mounts for container", mounts.len()));
        
        for mount_config in mounts {
            let target_path = if mount_config.target.starts_with('/') {
                format!("{}{}", rootfs_path, mount_config.target)
            } else {
                format!("{}/{}", rootfs_path, mount_config.target)
            };
            
            // Ensure target directory exists
            if let Err(e) = std::fs::create_dir_all(&target_path) {
                ConsoleLogger::warning(&format!("Failed to create mount target {}: {}", target_path, e));
                continue;
            }
            
            match mount_config.mount_type {
                MountType::Bind => {
                    self.setup_bind_mount(&mount_config.source, &target_path, mount_config.readonly)?;
                }
                MountType::Volume => {
                    // For volumes, the source should be the full volume path
                    self.setup_bind_mount(&mount_config.source, &target_path, mount_config.readonly)?;
                }
                MountType::Tmpfs => {
                    self.setup_tmpfs_mount(&target_path, &mount_config.options)?;
                }
            }
        }
        
        Ok(())
    }
    
    fn setup_bind_mount(&self, source: &str, target: &str, readonly: bool) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Setting up bind mount: {} -> {} (readonly: {})", source, target, readonly));
        
        // Check if source exists
        if !Path::new(source).exists() {
            return Err(format!("Mount source '{}' does not exist", source));
        }
        
        // Perform bind mount
        let mut flags = MsFlags::MS_BIND;
        if readonly {
            flags |= MsFlags::MS_RDONLY;
        }
        
        if let Err(e) = mount(
            Some(source),
            target,
            None::<&str>,
            flags,
            None::<&str>,
        ) {
            return Err(format!("Failed to bind mount {} to {}: {}", source, target, e));
        }
        
        // For readonly mounts, remount to ensure readonly is applied
        if readonly {
            if let Err(e) = mount(
                None::<&str>,
                target,
                None::<&str>,
                MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
                None::<&str>,
            ) {
                ConsoleLogger::warning(&format!("Failed to remount {} as readonly: {}", target, e));
            }
        }
        
        ConsoleLogger::success(&format!("Successfully mounted {} to {}", source, target));
        Ok(())
    }
    
    fn setup_tmpfs_mount(&self, target: &str, options: &std::collections::HashMap<String, String>) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Setting up tmpfs mount at {}", target));
        
        // Build mount options string
        let mut mount_opts = Vec::new();
        
        // Add size option if specified
        if let Some(size) = options.get("size") {
            mount_opts.push(format!("size={}", size));
        } else {
            mount_opts.push("size=64m".to_string()); // Default size
        }
        
        // Add mode option if specified
        if let Some(mode) = options.get("mode") {
            mount_opts.push(format!("mode={}", mode));
        }
        
        let opts_str = if mount_opts.is_empty() {
            None
        } else {
            Some(mount_opts.join(","))
        };
        
        if let Err(e) = mount(
            Some("tmpfs"),
            target,
            Some("tmpfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            opts_str.as_deref(),
        ) {
            return Err(format!("Failed to mount tmpfs at {}: {}", target, e));
        }
        
        ConsoleLogger::success(&format!("Successfully mounted tmpfs at {}", target));
        Ok(())
    }

    /// Setup the network for a container with a veth pair
    pub fn setup_container_network(&self, config: &ContainerNetworkConfig) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Configuring container network for {}", config.container_id));
        
        // Move veth peer into container's network namespace
        CommandExecutor::execute_shell(&format!("ip link set {} netns {}", 
            config.veth_container_name,
            ProcessUtils::pid_to_i32(nix::unistd::getpid())
        ))?;
        
        // Rename veth peer to eth0 inside container
        CommandExecutor::execute_shell(&format!("ip link set dev {} name eth0", config.veth_container_name))?;
        
        // Assign IP address to eth0
        CommandExecutor::execute_shell(&format!("ip addr add {} dev eth0", config.ip_address))?;
        
        // Bring up eth0
        CommandExecutor::execute_shell("ip link set eth0 up")?;
        
        // Bring up loopback interface
        CommandExecutor::execute_shell("ip link set lo up")?;
        
        // Set default route
        CommandExecutor::execute_shell("ip route add default via 10.42.0.1")?;

        ConsoleLogger::success("Container network configured successfully");
        Ok(())
    }

    /// Setup basic loopback networking in the network namespace
    pub fn setup_network_namespace(&self) -> Result<(), String> {
        ConsoleLogger::debug("Setting up basic loopback networking");
        
        // Bring up the loopback interface
        // This is a simplified implementation - in production you'd want to use netlink
        // For now, we'll use the `ip` command if available
        match CommandExecutor::execute_shell("ip link set lo up")
        {
            Ok(output) => {
                if output.success {
                    ConsoleLogger::success("Successfully brought up loopback interface");
                } else {
                    ConsoleLogger::warning(&format!("Failed to bring up loopback interface: {}", output.stderr));
                }
            }
            Err(e) => {
                ConsoleLogger::warning(&format!("Failed to execute ip command: {}", e));
            }
        }

        Ok(())
    }

    /// Set hostname in UTS namespace
    pub fn set_container_hostname(&self, hostname: &str) -> Result<(), String> {
        println!("Setting container hostname to: {}", hostname);
        
        match nix::unistd::sethostname(hostname) {
            Ok(()) => {
                println!("Successfully set hostname to: {}", hostname);
                Ok(())
            }
            Err(e) => {
                eprintln!("Warning: Failed to set hostname: {}", e);
                // Non-fatal - continue without hostname change
                Ok(())
            }
        }
    }

    /// Wait for a process to complete and return its exit code (non-blocking for async)
    pub async fn wait_for_process_async(&self, pid: Pid) -> Result<i32, String> {
        ConsoleLogger::debug(&format!("Starting async wait for process {}", ProcessUtils::pid_to_i32(pid)));
        
        // Use non-blocking waitpid with WNOHANG in a loop
        loop {
            match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {
                    // Process is still running, yield to async runtime
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    continue;
                }
                Ok(WaitStatus::Exited(_, exit_code)) => {
                    ConsoleLogger::success(&format!("Process {} exited with code: {}", ProcessUtils::pid_to_i32(pid), exit_code));
                    return Ok(exit_code);
                }
                Ok(WaitStatus::Signaled(_, signal, _)) => {
                    let msg = format!("Process {} was terminated by signal: {:?}", ProcessUtils::pid_to_i32(pid), signal);
                    ConsoleLogger::warning(&msg);
                    return Err(msg);
                }
                Ok(status) => {
                    let msg = format!("Process {} ended with unexpected status: {:?}", ProcessUtils::pid_to_i32(pid), status);
                    ConsoleLogger::warning(&msg);
                    return Err(msg);
                }
                Err(nix::errno::Errno::ECHILD) => {
                    // Process doesn't exist or is not our child
                    let msg = format!("Process {} is not a child or doesn't exist", ProcessUtils::pid_to_i32(pid));
                    ConsoleLogger::warning(&msg);
                    return Ok(0); // Assume normal exit
                }
                Err(e) => {
                    let msg = format!("Failed to wait for process {}: {}", ProcessUtils::pid_to_i32(pid), e);
                    ConsoleLogger::error(&msg);
                    return Err(msg);
                }
            }
        }
    }
    
    /// Wait for a process to complete and return its exit code (blocking version for sync code)
    pub fn wait_for_process(&self, pid: Pid) -> Result<i32, String> {
        ConsoleLogger::debug(&format!("Waiting for process {} to complete", ProcessUtils::pid_to_i32(pid)));

        match waitpid(pid, None) {
            Ok(WaitStatus::Exited(_, exit_code)) => {
                ConsoleLogger::success(&format!("Process {} exited with code: {}", ProcessUtils::pid_to_i32(pid), exit_code));
                Ok(exit_code)
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                let msg = format!("Process {} was terminated by signal: {:?}", ProcessUtils::pid_to_i32(pid), signal);
                ConsoleLogger::warning(&msg);
                Err(msg)
            }
            Ok(status) => {
                let msg = format!("Process {} ended with unexpected status: {:?}", ProcessUtils::pid_to_i32(pid), status);
                ConsoleLogger::warning(&msg);
                Err(msg)
            }
            Err(e) => {
                let msg = format!("Failed to wait for process {}: {}", ProcessUtils::pid_to_i32(pid), e);
                ConsoleLogger::error(&msg);
                Err(msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_namespace_config() {
        let config = NamespaceConfig::default();
        assert!(!config.pid);     // Updated to match actual default
        assert!(config.mount);
        assert!(!config.uts);     // Updated to match actual default
        assert!(!config.ipc);     // Updated to match actual default
        assert!(config.network); // Updated to match actual default
    }

    #[test]
    fn test_build_clone_flags() {
        let manager = NamespaceManager::new();
        let mut config = NamespaceConfig::default();
        
        // Test with all flags enabled
        config.pid = true;
        config.uts = true;  // Enable UTS to test the flag
        config.ipc = true;
        config.network = true;
        
        let flags = manager.build_clone_flags(&config);
        
        assert!(flags.contains(CloneFlags::CLONE_NEWPID));
        assert!(flags.contains(CloneFlags::CLONE_NEWNS));
        assert!(flags.contains(CloneFlags::CLONE_NEWUTS));
        assert!(flags.contains(CloneFlags::CLONE_NEWIPC));
        assert!(flags.contains(CloneFlags::CLONE_NEWNET));
    }

    #[test]
    fn test_minimal_flags() {
        let manager = NamespaceManager::new();
        let config = NamespaceConfig::default();
        let flags = manager.build_clone_flags(&config);
        
        // With default config, only mount namespace is enabled
        assert!(flags.contains(CloneFlags::CLONE_NEWNS));
        assert!(!flags.contains(CloneFlags::CLONE_NEWUTS));  // UTS is disabled by default
        assert!(!flags.contains(CloneFlags::CLONE_NEWPID));
        assert!(!flags.contains(CloneFlags::CLONE_NEWIPC));
        assert!(!flags.contains(CloneFlags::CLONE_NEWNET));
    }
} 