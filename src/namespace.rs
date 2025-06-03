use nix::sched::CloneFlags;
use nix::unistd::Pid;
use nix::mount::{mount, MsFlags};
use nix::sys::wait::{waitpid, WaitStatus};
use std::path::Path;

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
            pid: true,
            mount: true,
            uts: true,
            ipc: true,
            network: true, // Start with basic network isolation
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
        
        println!("Creating namespaced process with flags: {:?}", clone_flags);

        // Use unshare to create namespaces, then fork
        if let Err(e) = nix::sched::unshare(clone_flags) {
            return Err(format!("Failed to unshare namespaces: {}", e));
        }

        // Now fork a child process
        match unsafe { nix::unistd::fork() } {
            Ok(nix::unistd::ForkResult::Parent { child }) => {
                println!("Successfully created namespaced process with PID: {}", child);
                Ok(child)
            }
            Ok(nix::unistd::ForkResult::Child) => {
                // This runs in the child process
                let exit_code = child_func();
                std::process::exit(exit_code);
            }
            Err(e) => {
                let error_msg = format!("Failed to fork process: {}", e);
                eprintln!("{}", error_msg);
                Err(error_msg)
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
        println!("Setting up mount namespace for rootfs: {}", rootfs_path);

        // Make the mount namespace private to prevent propagation to host
        if let Err(e) = mount(
            None::<&str>,
            "/",
            None::<&str>,
            MsFlags::MS_REC | MsFlags::MS_PRIVATE,
            None::<&str>,
        ) {
            return Err(format!("Failed to make mount namespace private: {}", e));
        }

        // Bind mount the rootfs to itself to make it a mount point
        if let Err(e) = mount(
            Some(rootfs_path),
            rootfs_path,
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        ) {
            return Err(format!("Failed to bind mount rootfs: {}", e));
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
                eprintln!("Warning: Failed to mount /proc in container: {}", e);
            } else {
                println!("Successfully mounted /proc in container");
            }
        }

        // Mount /sys inside the new namespace
        let sys_path = format!("{}/sys", rootfs_path);
        if Path::new(&sys_path).exists() {
            if let Err(e) = mount(
                Some("sysfs"),
                sys_path.as_str(),
                Some("sysfs"),
                MsFlags::MS_RDONLY,
                None::<&str>,
            ) {
                // Non-fatal error - log and continue
                eprintln!("Warning: Failed to mount /sys in container: {}", e);
            } else {
                println!("Successfully mounted /sys in container");
            }
        }

        // Mount /dev/pts for pseudo-terminals
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
                eprintln!("Warning: Failed to mount /dev/pts in container: {}", e);
            } else {
                println!("Successfully mounted /dev/pts in container");
            }
        }

        Ok(())
    }

    /// Setup basic loopback networking in the network namespace
    pub fn setup_network_namespace(&self) -> Result<(), String> {
        println!("Setting up basic loopback networking");
        
        // Bring up the loopback interface
        // This is a simplified implementation - in practice you'd use netlink
        // For now, we'll use the `ip` command if available
        match std::process::Command::new("ip")
            .args(["link", "set", "lo", "up"])
            .output()
        {
            Ok(output) => {
                if output.status.success() {
                    println!("Successfully brought up loopback interface");
                } else {
                    eprintln!("Warning: Failed to bring up loopback interface: {}", 
                             String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to execute ip command: {}", e);
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
                let error_msg = format!("Failed to set hostname: {}", e);
                eprintln!("{}", error_msg);
                Err(error_msg)
            }
        }
    }

    /// Wait for a namespaced process to complete
    pub fn wait_for_process(&self, pid: Pid) -> Result<i32, String> {
        match waitpid(pid, None) {
            Ok(WaitStatus::Exited(_, exit_code)) => {
                println!("Namespaced process {} exited with code: {}", pid, exit_code);
                Ok(exit_code)
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                let error_msg = format!("Namespaced process {} killed by signal: {:?}", pid, signal);
                eprintln!("{}", error_msg);
                Err(error_msg)
            }
            Ok(status) => {
                let error_msg = format!("Namespaced process {} stopped with status: {:?}", pid, status);
                eprintln!("{}", error_msg);
                Err(error_msg)
            }
            Err(e) => {
                let error_msg = format!("Failed to wait for namespaced process {}: {}", pid, e);
                eprintln!("{}", error_msg);
                Err(error_msg)
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
        assert!(config.pid);
        assert!(config.mount);
        assert!(config.uts);
        assert!(config.ipc);
        assert!(config.network);
    }

    #[test]
    fn test_build_clone_flags() {
        let manager = NamespaceManager::new();
        let config = NamespaceConfig::default();
        let flags = manager.build_clone_flags(&config);
        
        assert!(flags.contains(CloneFlags::CLONE_NEWPID));
        assert!(flags.contains(CloneFlags::CLONE_NEWNS));
        assert!(flags.contains(CloneFlags::CLONE_NEWUTS));
        assert!(flags.contains(CloneFlags::CLONE_NEWIPC));
        assert!(flags.contains(CloneFlags::CLONE_NEWNET));
    }
} 