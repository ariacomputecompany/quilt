use crate::namespace::{NamespaceManager, NamespaceConfig};
use crate::cgroup::{CgroupManager, CgroupLimits};
use crate::runtime_manager::RuntimeManager;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::process::Command;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use flate2::read::GzDecoder;
use tar::Archive;
use nix::unistd::{chroot, chdir, Pid};

#[derive(Debug, Clone)]
pub enum ContainerState {
    PENDING,
    RUNNING,
    EXITED(i32),
    FAILED(String),
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: u64,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ContainerConfig {
    pub image_path: String,
    pub command: Vec<String>,
    pub environment: HashMap<String, String>,
    pub setup_commands: Vec<String>,  // Setup commands specification
    pub resource_limits: Option<CgroupLimits>,
    pub namespace_config: Option<NamespaceConfig>,
    #[allow(dead_code)]
    pub working_directory: Option<String>,
}

impl Default for ContainerConfig {
    fn default() -> Self {
        ContainerConfig {
            image_path: String::new(),
            command: vec!["/bin/sh".to_string()],
            environment: HashMap::new(),
            setup_commands: vec![],
            resource_limits: Some(CgroupLimits::default()),
            namespace_config: Some(NamespaceConfig::default()),
            working_directory: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Container {
    #[allow(dead_code)]
    pub id: String,
    pub config: ContainerConfig,
    pub state: ContainerState,
    pub logs: Vec<LogEntry>,
    pub pid: Option<Pid>,
    pub rootfs_path: String,
    pub created_at: u64,
}

impl Container {
    pub fn new(id: String, config: ContainerConfig) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Container {
            id: id.clone(),
            config,
            state: ContainerState::PENDING,
            logs: Vec::new(),
            pid: None,
            rootfs_path: format!("/tmp/quilt-containers/{}", id),
            created_at: timestamp,
        }
    }

    pub fn add_log(&mut self, message: String) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        self.logs.push(LogEntry {
            timestamp,
            message,
        });
    }
}

pub struct ContainerRuntime {
    containers: Arc<Mutex<HashMap<String, Container>>>,
    namespace_manager: NamespaceManager,
    runtime_manager: RuntimeManager,
}

impl ContainerRuntime {
    pub fn new() -> Self {
        ContainerRuntime {
            containers: Arc::new(Mutex::new(HashMap::new())),
            namespace_manager: NamespaceManager::new(),
            runtime_manager: RuntimeManager::new(),
        }
    }

    pub fn create_container(&self, id: String, config: ContainerConfig) -> Result<(), String> {
        println!("Creating container: {}", id);

        // Create container instance
        let container = Container::new(id.clone(), config);

        // Add to containers map
        {
            let mut containers = self.containers.lock().unwrap();
            containers.insert(id.clone(), container);
        }

        // Setup rootfs
        self.setup_rootfs(&id)?;

        // Update state to PENDING
        self.update_container_state(&id, ContainerState::PENDING);

        println!("✅ Container {} created successfully", id);
        Ok(())
    }

    pub fn start_container(&self, id: &str) -> Result<(), String> {
        println!("Starting container: {}", id);

        // Get container config
        let (config, rootfs_path) = {
            let containers = self.containers.lock().unwrap();
            let container = containers.get(id)
                .ok_or_else(|| format!("Container {} not found", id))?;
            (container.config.clone(), container.rootfs_path.clone())
        };

        // Create cgroups
        let cgroup_manager = CgroupManager::new(id.to_string());
        if let Some(limits) = &config.resource_limits {
            if let Err(e) = cgroup_manager.create_cgroups(limits) {
                eprintln!("Warning: Failed to create cgroups: {}", e);
            }
        }

        // Parse and execute setup commands
        let setup_commands = if !config.setup_commands.is_empty() {
            let setup_spec = config.setup_commands.join("\n");
            self.runtime_manager.parse_setup_spec(&setup_spec)?
        } else {
            vec![]
        };

        // Create namespaced process for container execution
        let namespace_config = config.namespace_config.unwrap_or_default();
        let containers_clone = Arc::clone(&self.containers);
        let id_clone = id.to_string();
        let command_clone = config.command.clone();
        let environment_clone = config.environment.clone();
        let rootfs_path_clone = rootfs_path.clone();
        let setup_commands_clone = setup_commands.clone();
        let mut runtime_manager_clone = RuntimeManager::new(); // Create new instance for child process

        let child_func = move || -> i32 {
            // This runs in the child process with new namespaces
            
            // Setup mount namespace
            let namespace_manager = NamespaceManager::new();
            if let Err(e) = namespace_manager.setup_mount_namespace(&rootfs_path_clone) {
                eprintln!("Failed to setup mount namespace: {}", e);
                return 1;
            }

            // Setup network namespace (basic loopback)
            if let Err(e) = namespace_manager.setup_network_namespace() {
                eprintln!("Failed to setup network namespace: {}", e);
                // Non-fatal, continue
            }

            // Set container hostname
            if let Err(e) = namespace_manager.set_container_hostname(&id_clone) {
                eprintln!("Failed to set container hostname: {}", e);
                // Non-fatal, continue
            }

            // Change root to container filesystem
            if let Err(e) = chroot(rootfs_path_clone.as_str()) {
                eprintln!("Failed to chroot to {}: {}", rootfs_path_clone, e);
                return 1;
            }

            // Change to root directory inside container
            if let Err(e) = chdir("/") {
                eprintln!("Failed to chdir to /: {}", e);
                return 1;
            }

            // Initialize container system environment first
            if let Err(e) = runtime_manager_clone.initialize_container() {
                eprintln!("Failed to initialize container environment: {}", e);
                // Add error to container logs
                if let Ok(mut containers) = containers_clone.lock() {
                    if let Some(container) = containers.get_mut(&id_clone) {
                        container.add_log(format!("Environment initialization failed: {}", e));
                        container.state = ContainerState::FAILED(e.clone());
                    }
                }
                return 1;
            }

            // Execute setup commands inside the container
            if !setup_commands_clone.is_empty() {
                println!("Executing {} setup commands in container {}", setup_commands_clone.len(), id_clone);
                if let Err(e) = runtime_manager_clone.execute_setup_commands(&setup_commands_clone) {
                    eprintln!("Setup commands failed: {}", e);
                    // Add error to container logs
                    if let Ok(mut containers) = containers_clone.lock() {
                        if let Some(container) = containers.get_mut(&id_clone) {
                            container.add_log(format!("Setup failed: {}", e));
                            container.state = ContainerState::FAILED(e.clone());
                        }
                    }
                    return 1;
                }
            }

            // Set environment variables
            for (key, value) in environment_clone {
                std::env::set_var(key, value);
            }

            // Execute the main command
            println!("Executing main command in container: {:?}", command_clone);
            
            let mut cmd = Command::new(&command_clone[0]);
            if command_clone.len() > 1 {
                cmd.args(&command_clone[1..]);
            }

            // Capture output
            match cmd.output() {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    
                    // Add logs to container
                    if let Ok(mut containers) = containers_clone.lock() {
                        if let Some(container) = containers.get_mut(&id_clone) {
                            if !stdout.is_empty() {
                                container.add_log(format!("stdout: {}", stdout));
                            }
                            if !stderr.is_empty() {
                                container.add_log(format!("stderr: {}", stderr));
                            }
                        }
                    }

                    if output.status.success() {
                        println!("Container {} command completed successfully", id_clone);
                        0
                    } else {
                        println!("Container {} command failed with exit code: {:?}", 
                                id_clone, output.status.code());
                        output.status.code().unwrap_or(1)
                    }
                }
                Err(e) => {
                    eprintln!("Failed to execute command in container {}: {}", id_clone, e);
                    if let Ok(mut containers) = containers_clone.lock() {
                        if let Some(container) = containers.get_mut(&id_clone) {
                            container.add_log(format!("Execution error: {}", e));
                        }
                    }
                    1
                }
            }
        };

        // Create the namespaced process
        match self.namespace_manager.create_namespaced_process(&namespace_config, child_func) {
            Ok(pid) => {
                println!("Container {} started with PID: {}", id, pid);
                
                // Add process to cgroups
                if let Err(e) = cgroup_manager.add_process(pid) {
                    eprintln!("Warning: Failed to add process to cgroups: {}", e);
                }

                // Update container state
                {
                    let mut containers = self.containers.lock().unwrap();
                    if let Some(container) = containers.get_mut(id) {
                        container.pid = Some(pid);
                        container.state = ContainerState::RUNNING;
                        container.add_log(format!("Container started with PID: {}", pid));
                    }
                }

                // Wait for process completion in a separate task
                let containers_clone = Arc::clone(&self.containers);
                let id_clone = id.to_string();
                let namespace_manager_clone = NamespaceManager::new();
                let cgroup_manager_clone = CgroupManager::new(id.to_string());
                
                tokio::spawn(async move {
                    match namespace_manager_clone.wait_for_process(pid) {
                        Ok(exit_code) => {
                            println!("Container {} exited with code: {}", id_clone, exit_code);
                            let mut containers = containers_clone.lock().unwrap();
                            if let Some(container) = containers.get_mut(&id_clone) {
                                container.state = ContainerState::EXITED(exit_code);
                                container.add_log(format!("Container exited with code: {}", exit_code));
                                container.pid = None;
                            }
                        }
                        Err(e) => {
                            eprintln!("Container {} failed: {}", id_clone, e);
                            let mut containers = containers_clone.lock().unwrap();
                            if let Some(container) = containers.get_mut(&id_clone) {
                                container.state = ContainerState::FAILED(e.clone());
                                container.add_log(format!("Container failed: {}", e));
                                container.pid = None;
                            }
                        }
                    }

                    // Cleanup cgroups
                    if let Err(e) = cgroup_manager_clone.cleanup() {
                        eprintln!("Warning: Failed to cleanup cgroups for {}: {}", id_clone, e);
                    }
                });

                Ok(())
            }
            Err(e) => {
                self.update_container_state(id, ContainerState::FAILED(e.clone()));
                Err(format!("Failed to start container {}: {}", id, e))
            }
        }
    }

    fn setup_rootfs(&self, container_id: &str) -> Result<(), String> {
        let containers = self.containers.lock().unwrap();
        let container = containers.get(container_id)
            .ok_or_else(|| format!("Container {} not found", container_id))?;

        let rootfs_path = &container.rootfs_path;
        let image_path = &container.config.image_path;

        println!("Setting up rootfs for container {} at {}", container_id, rootfs_path);

        // Create rootfs directory
        fs::create_dir_all(rootfs_path)
            .map_err(|e| format!("Failed to create rootfs directory: {}", e))?;

        // Extract image tarball to rootfs
        if Path::new(image_path).exists() {
            println!("Extracting image {} to {}", image_path, rootfs_path);
            self.extract_image(image_path, rootfs_path)?;
        } else {
            return Err(format!("Image file not found: {}", image_path));
        }

        println!("✅ Rootfs setup completed for container {}", container_id);
        Ok(())
    }

    fn extract_image(&self, image_path: &str, rootfs_path: &str) -> Result<(), String> {
        let file = fs::File::open(image_path)
            .map_err(|e| format!("Failed to open image file: {}", e))?;

        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        archive.unpack(rootfs_path)
            .map_err(|e| format!("Failed to extract image: {}", e))?;

        println!("✅ Successfully extracted image to {}", rootfs_path);
        Ok(())
    }

    fn update_container_state(&self, container_id: &str, new_state: ContainerState) {
        let mut containers = self.containers.lock().unwrap();
        if let Some(container) = containers.get_mut(container_id) {
            container.state = new_state;
        }
    }

    #[allow(dead_code)]
    pub fn get_container_state(&self, container_id: &str) -> Option<ContainerState> {
        let containers = self.containers.lock().unwrap();
        containers.get(container_id).map(|c| c.state.clone())
    }

    pub fn get_container_logs(&self, container_id: &str) -> Option<Vec<LogEntry>> {
        let containers = self.containers.lock().unwrap();
        containers.get(container_id).map(|c| c.logs.clone())
    }

    pub fn get_container_info(&self, container_id: &str) -> Option<Container> {
        let containers = self.containers.lock().unwrap();
        containers.get(container_id).cloned()
    }

    pub fn stop_container(&self, container_id: &str) -> Result<(), String> {
        println!("Stopping container: {}", container_id);

        let pid = {
            let containers = self.containers.lock().unwrap();
            let container = containers.get(container_id)
                .ok_or_else(|| format!("Container {} not found", container_id))?;
            
            match container.pid {
                Some(pid) => pid,
                None => return Err(format!("Container {} is not running", container_id)),
            }
        };

        // Send SIGTERM to the container process
        match nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM) {
            Ok(()) => {
                println!("Sent SIGTERM to container {} (PID: {})", container_id, pid);
                
                // Update container state
                self.update_container_state(container_id, ContainerState::EXITED(143)); // 128 + 15 (SIGTERM)
                
                // Cleanup cgroups
                let cgroup_manager = CgroupManager::new(container_id.to_string());
                if let Err(e) = cgroup_manager.cleanup() {
                    eprintln!("Warning: Failed to cleanup cgroups: {}", e);
                }
                
                Ok(())
            }
            Err(e) => Err(format!("Failed to stop container {}: {}", container_id, e)),
        }
    }

    pub fn remove_container(&self, container_id: &str) -> Result<(), String> {
        println!("Removing container: {}", container_id);

        // First stop the container if it's running
        if let Ok(()) = self.stop_container(container_id) {
            // Give it a moment to stop
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let rootfs_path = {
            let mut containers = self.containers.lock().unwrap();
            let container = containers.remove(container_id)
                .ok_or_else(|| format!("Container {} not found", container_id))?;
            container.rootfs_path
        };

        // Remove rootfs directory
        if Path::new(&rootfs_path).exists() {
            fs::remove_dir_all(&rootfs_path)
                .map_err(|e| format!("Failed to remove rootfs directory: {}", e))?;
        }

        // Cleanup cgroups (just in case)
        let cgroup_manager = CgroupManager::new(container_id.to_string());
        if let Err(e) = cgroup_manager.cleanup() {
            eprintln!("Warning: Failed to cleanup cgroups during removal: {}", e);
        }

        println!("✅ Container {} removed successfully", container_id);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn list_containers(&self) -> Vec<String> {
        let containers = self.containers.lock().unwrap();
        containers.keys().cloned().collect()
    }

    pub fn get_container_stats(&self, container_id: &str) -> Result<HashMap<String, String>, String> {
        let mut stats = HashMap::new();
        
        // Get memory usage from cgroups
        let cgroup_manager = CgroupManager::new(container_id.to_string());
        if let Ok(memory_usage) = cgroup_manager.get_memory_usage() {
            stats.insert("memory_usage_bytes".to_string(), memory_usage.to_string());
        }

        // Get container info
        if let Some(container) = self.get_container_info(container_id) {
            stats.insert("state".to_string(), format!("{:?}", container.state));
            stats.insert("created_at".to_string(), container.created_at.to_string());
            if let Some(pid) = container.pid {
                stats.insert("pid".to_string(), pid.to_string());
            }
            stats.insert("rootfs_path".to_string(), container.rootfs_path);
        }

        Ok(stats)
    }
} 