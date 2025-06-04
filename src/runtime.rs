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
use nix::unistd::{chroot, chdir, Pid, execv};
use std::os::unix::fs::PermissionsExt;
use std::ffi::CString;

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
        let mut cgroup_manager = CgroupManager::new(id.to_string());
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
        
        // Reduce memory footprint - prepare everything needed outside the closure
        let id_for_logs = id.to_string();
        let command_for_logs = format!("{:?}", config.command);
        
        // Log start before entering child process to avoid memory allocation in child
        {
            let mut containers = self.containers.lock().unwrap();
            if let Some(container) = containers.get_mut(id) {
                container.add_log(format!("Starting container execution with command: {}", command_for_logs));
            }
        }
        
        // Prepare all data needed by child process (avoid heavy captures)
        let command_clone = config.command.clone();
        let environment_clone = config.environment.clone();
        let rootfs_path_clone = rootfs_path.clone();
        let setup_commands_clone = setup_commands.clone();
        
        // Create new lightweight runtime manager for child (not clone of existing)
        let child_func = move || -> i32 {
            // This runs in the child process with new namespaces
            // Keep memory allocation to minimum in child process
            
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
            if let Err(e) = namespace_manager.set_container_hostname(&id_for_logs) {
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
            let mut runtime_manager = RuntimeManager::new(); // Create fresh instance
            if let Err(e) = runtime_manager.initialize_container() {
                eprintln!("Failed to initialize container environment: {}", e);
                return 1;
            }

            // Execute setup commands inside the container
            if !setup_commands_clone.is_empty() {
                println!("Executing {} setup commands in container {}", setup_commands_clone.len(), id_for_logs);
                if let Err(e) = runtime_manager.execute_setup_commands(&setup_commands_clone) {
                    eprintln!("Setup commands failed: {}", e);
                    return 1;
                }
            }

            // Set environment variables
            for (key, value) in environment_clone {
                std::env::set_var(key, value);
            }

            // Execute the main command with reduced memory overhead
            println!("Executing main command in container: {:?}", command_clone);
            
            // Prepare the final command to execute
            let (final_program, final_args) = if command_clone.len() >= 3 
                && (command_clone[0].ends_with("/sh") || command_clone[0].ends_with("/bash"))
                && command_clone[1] == "-c" {
                // Command is already a shell command like ["/bin/sh", "-c", "actual command"]
                // Use it directly to avoid double-shell wrapping
                (command_clone[0].clone(), command_clone[1..].to_vec())
            } else if command_clone.len() == 1 {
                // Single command - execute it through shell
                ("/bin/sh".to_string(), vec!["-c".to_string(), command_clone[0].clone()])
            } else {
                // Multiple arguments - join them and execute through shell
                ("/bin/sh".to_string(), vec!["-c".to_string(), command_clone.join(" ")])
            };

            // Convert to CString for exec (do this once, outside any fork)
            let program_cstring = match CString::new(final_program.clone()) {
                Ok(cs) => cs,
                Err(e) => {
                    eprintln!("Failed to create program CString: {}", e);
                    return 1;
                }
            };

            // Prepare all arguments as CStrings with proper lifetime management
            let mut all_args = vec![final_program];
            all_args.extend(final_args);
            
            let args_cstrings: Vec<CString> = match all_args.iter()
                .map(|s| CString::new(s.clone()))
                .collect::<Result<Vec<CString>, _>>() {
                Ok(cstrings) => cstrings,
                Err(e) => {
                    eprintln!("Failed to prepare command arguments: {}", e);
                    return 1;
                }
            };

            // Create references with proper lifetime (after cstrings is owned)
            let arg_refs: Vec<&CString> = args_cstrings.iter().collect();

            // Direct exec without nested fork - this replaces the current process
            println!("Executing: {} {:?}", program_cstring.to_string_lossy(), 
                     arg_refs.iter().map(|cs| cs.to_string_lossy()).collect::<Vec<_>>());
            
            // This will replace the current process entirely
            match execv(&program_cstring, &arg_refs) {
                Ok(_) => {
                    // This should never be reached if exec succeeds
                    0
                }
                Err(e) => {
                    eprintln!("Failed to exec command: {}", e);
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

                // Finalize cgroup limits after process is started
                if let Some(limits) = &config.resource_limits {
                    if let Err(e) = cgroup_manager.finalize_limits(limits) {
                        eprintln!("Warning: Failed to finalize cgroup limits: {}", e);
                    }
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

        // Fix broken symlinks and ensure working binaries
        self.fix_container_binaries(rootfs_path)?;

        println!("✅ Rootfs setup completed for container {}", container_id);
        Ok(())
    }

    /// Fix broken symlinks in Nix-generated containers and ensure working binaries
    fn fix_container_binaries(&self, rootfs_path: &str) -> Result<(), String> {
        println!("🔧 Fixing container binaries and symlinks...");

        // Essential binaries that must work
        let essential_binaries = vec![
            ("sh", vec!["/bin/sh", "/bin/bash", "/usr/bin/sh"]),
            ("echo", vec!["/bin/echo", "/usr/bin/echo"]),
            ("ls", vec!["/bin/ls", "/usr/bin/ls"]),
            ("cat", vec!["/bin/cat", "/usr/bin/cat"]),
        ];

        // First, ensure we have essential library directories
        self.setup_library_directories(rootfs_path)?;

        for (binary_name, host_paths) in essential_binaries {
            let container_binary_path = format!("{}/bin/{}", rootfs_path, binary_name);
            
            // Check if the binary exists and works in the container
            if Path::new(&container_binary_path).exists() {
                // Check if it's a broken symlink
                if let Ok(target) = fs::read_link(&container_binary_path) {
                    // It's a symlink, check if target exists
                    let target_path = if target.is_absolute() {
                        format!("{}{}", rootfs_path, target.display())
                    } else {
                        format!("{}/bin/{}", rootfs_path, target.display())
                    };
                    
                    if !Path::new(&target_path).exists() {
                        println!("  ⚠ Broken symlink found for {}: {} -> {}", binary_name, container_binary_path, target.display());
                        self.fix_broken_binary(&container_binary_path, binary_name, &host_paths)?;
                    } else {
                        println!("  ✓ Symlink for {} is working", binary_name);
                    }
                } else {
                    // It's a regular file, check if it's executable
                    if let Ok(metadata) = fs::metadata(&container_binary_path) {
                        if metadata.permissions().mode() & 0o111 == 0 {
                            println!("  ⚠ Binary {} is not executable", binary_name);
                            self.fix_broken_binary(&container_binary_path, binary_name, &host_paths)?;
                        } else {
                            println!("  ✓ Binary {} exists and is executable", binary_name);
                        }
                    }
                }
            } else {
                println!("  ⚠ Missing binary: {}", binary_name);
                self.fix_broken_binary(&container_binary_path, binary_name, &host_paths)?;
            }
        }

        // Copy essential libraries
        self.copy_essential_libraries(rootfs_path)?;

        // Ensure basic shell works
        self.verify_container_shell(rootfs_path)?;

        println!("✅ Container binaries fixed and verified");
        Ok(())
    }

    /// Setup essential library directories
    fn setup_library_directories(&self, rootfs_path: &str) -> Result<(), String> {
        let lib_dirs = vec![
            format!("{}/lib", rootfs_path),
            format!("{}/lib64", rootfs_path),
            format!("{}/lib/x86_64-linux-gnu", rootfs_path),
        ];

        for dir in lib_dirs {
            if let Err(e) = fs::create_dir_all(&dir) {
                eprintln!("Warning: Failed to create library directory {}: {}", dir, e);
            }
        }

        Ok(())
    }

    /// Copy essential libraries needed by binaries
    fn copy_essential_libraries(&self, rootfs_path: &str) -> Result<(), String> {
        let essential_libs = vec![
            ("/lib/x86_64-linux-gnu/libc.so.6", "lib/x86_64-linux-gnu/libc.so.6"),
            ("/lib64/ld-linux-x86-64.so.2", "lib64/ld-linux-x86-64.so.2"),
            ("/lib/x86_64-linux-gnu/libtinfo.so.6", "lib/x86_64-linux-gnu/libtinfo.so.6"),
            ("/lib/x86_64-linux-gnu/libdl.so.2", "lib/x86_64-linux-gnu/libdl.so.2"),
        ];

        for (host_lib, container_lib) in essential_libs {
            if Path::new(host_lib).exists() {
                let container_lib_path = format!("{}/{}", rootfs_path, container_lib);
                if let Some(parent) = Path::new(&container_lib_path).parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        eprintln!("Warning: Failed to create lib directory: {}", e);
                        continue;
                    }
                }

                match fs::copy(host_lib, &container_lib_path) {
                    Ok(_) => {
                        println!("  ✓ Copied library: {}", container_lib);
                    }
                    Err(e) => {
                        eprintln!("  ⚠ Failed to copy library {}: {}", host_lib, e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Fix a broken or missing binary by copying from host
    fn fix_broken_binary(&self, container_binary_path: &str, binary_name: &str, host_paths: &[&str]) -> Result<(), String> {
        // Remove existing broken symlink or file
        if Path::new(container_binary_path).exists() {
            fs::remove_file(container_binary_path)
                .map_err(|e| format!("Failed to remove broken binary {}: {}", binary_name, e))?;
        }

        // For critical binaries, try to create a shell script wrapper that's more likely to work
        if binary_name == "sh" {
            return self.create_robust_shell(container_binary_path);
        }

        if binary_name == "echo" {
            return self.create_echo_script(container_binary_path);
        }

        // Try to find a working binary on the host
        for host_path in host_paths {
            if Path::new(host_path).exists() {
                // Check if it's executable
                if let Ok(metadata) = fs::metadata(host_path) {
                    if metadata.permissions().mode() & 0o111 != 0 {
                        // For simple utilities, try copying first
                        match fs::copy(host_path, container_binary_path) {
                            Ok(_) => {
                                // Make sure it's executable
                                let mut perms = fs::metadata(container_binary_path)
                                    .map_err(|e| format!("Failed to get permissions for copied binary: {}", e))?
                                    .permissions();
                                perms.set_mode(0o755);
                                fs::set_permissions(container_binary_path, perms)
                                    .map_err(|e| format!("Failed to set permissions: {}", e))?;
                                
                                println!("  ✅ Fixed {} by copying from {}", binary_name, host_path);
                                return Ok(());
                            }
                            Err(e) => {
                                eprintln!("  ⚠ Failed to copy {} from {}: {}", binary_name, host_path, e);
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // If copying failed, create a simple script implementation
        match binary_name {
            "ls" => self.create_ls_script(container_binary_path),
            "cat" => self.create_cat_script(container_binary_path),
            _ => {
                println!("  ⚠ Could not fix binary {}, container may not work properly", binary_name);
                Ok(())
            }
        }
    }

    /// Create a robust shell script that works without external dependencies
    fn create_robust_shell(&self, shell_path: &str) -> Result<(), String> {
        let container_root = Path::new(shell_path).parent().unwrap().parent().unwrap();
        
        // AVOID using Nix busybox - it has /nix/store dependencies that won't work after chroot
        // Check if there's a busybox but verify it's not Nix-linked
        let busybox_path = container_root.join("bin/busybox");
        if busybox_path.exists() {
            // Check if it's Nix-linked by looking for /nix/store in its dependencies
            if let Ok(output) = Command::new("ldd").arg(&busybox_path).output() {
                let ldd_output = String::from_utf8_lossy(&output.stdout);
                if ldd_output.contains("/nix/store") {
                    println!("  ⚠ Busybox is Nix-linked, avoiding it");
                } else {
                    // It's a good busybox, use it
                    match fs::copy(&busybox_path, shell_path) {
                Ok(_) => {
                    let mut perms = fs::metadata(shell_path)
                        .map_err(|e| format!("Failed to get shell permissions: {}", e))?
                        .permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(shell_path, perms)
                        .map_err(|e| format!("Failed to set shell permissions: {}", e))?;
                            println!("  ✅ Created shell using non-Nix busybox");
                            return Ok(());
                        }
                        Err(e) => {
                            println!("  ⚠ Failed to copy busybox: {}", e);
                        }
                    }
                }
            }
        }

        // Try to create a statically linked shell binary first
        if let Ok(()) = self.create_minimal_shell_binary(shell_path) {
                    return Ok(());
                }

        // Try copying minimal shells from host that are more likely to be portable
        let minimal_shells = ["/bin/dash", "/bin/ash", "/usr/bin/dash"];
        for shell in &minimal_shells {
            if Path::new(shell).exists() {
                // Check if it's statically linked or has reasonable dependencies
                if let Ok(output) = Command::new("ldd").arg(shell).output() {
                    let ldd_output = String::from_utf8_lossy(&output.stdout);
                    if ldd_output.contains("not a dynamic executable") || 
                       (!ldd_output.contains("/nix/store") && ldd_output.contains("libc.so.6")) {
                        // It's either static or has standard dependencies
                        match fs::copy(shell, shell_path) {
                            Ok(_) => {
                                // Copy standard system libraries
                                self.copy_shell_dependencies(shell, container_root.to_str().unwrap())?;
                                
                                let mut perms = fs::metadata(shell_path)
                                    .map_err(|e| format!("Failed to get shell permissions: {}", e))?
                                    .permissions();
                                perms.set_mode(0o755);
                                fs::set_permissions(shell_path, perms)
                                    .map_err(|e| format!("Failed to set shell permissions: {}", e))?;
                                
                                println!("  ✅ Created shell by copying {}", shell);
                                return Ok(());
                            }
                            Err(e) => {
                                println!("  ⚠ Failed to copy {}: {}", shell, e);
                                continue;
                            }
                        }
                    } else {
                        println!("  ⚠ Shell {} has complex dependencies, skipping", shell);
                    }
                }
            }
        }

        // Final fallback - create a shell script
        self.create_shell_script(shell_path)
    }

    /// Copy essential libraries for a shell binary
    fn copy_shell_dependencies(&self, shell_binary: &str, container_root: &str) -> Result<(), String> {
        // Use ldd to find dependencies
        let output = Command::new("ldd")
            .arg(shell_binary)
            .output()
            .map_err(|e| format!("Failed to run ldd: {}", e))?;

        let ldd_output = String::from_utf8_lossy(&output.stdout);
        
        for line in ldd_output.lines() {
            if let Some(lib_path) = self.extract_library_path(line) {
                if Path::new(&lib_path).exists() {
                    let lib_name = Path::new(&lib_path).file_name().unwrap().to_str().unwrap();
                    let container_lib_path = format!("{}/lib/{}", container_root, lib_name);
                    
                    if let Some(parent) = Path::new(&container_lib_path).parent() {
                        fs::create_dir_all(parent).ok();
                    }
                    
                    if fs::copy(&lib_path, &container_lib_path).is_ok() {
                        println!("    ✓ Copied library: {}", lib_name);
                    }
                }
            }
        }
        
        Ok(())
    }

    /// Extract library path from ldd output
    fn extract_library_path(&self, ldd_line: &str) -> Option<String> {
        // Parse lines like: "libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x...)"
        if let Some(arrow_pos) = ldd_line.find(" => ") {
            let after_arrow = &ldd_line[arrow_pos + 4..];
            if let Some(space_pos) = after_arrow.find(' ') {
                let path = after_arrow[..space_pos].trim();
                if path.starts_with('/') && Path::new(path).exists() {
                    return Some(path.to_string());
                }
            }
        }
        None
    }

    /// Create a minimal shell binary that can execute basic commands
    fn create_minimal_shell_binary(&self, shell_path: &str) -> Result<(), String> {
        // Create a more complete C program that can handle shell commands
        let c_program = r#"
#include <unistd.h>
#include <sys/wait.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>

// Simple built-in command implementations
int builtin_echo(char *args) {
    if (args && strlen(args) > 0) {
        printf("%s\n", args);
    } else {
        printf("\n");
    }
    return 0;
}

int builtin_pwd(void) {
    char cwd[1024];
    if (getcwd(cwd, sizeof(cwd)) != NULL) {
        printf("%s\n", cwd);
        return 0;
    }
    return 1;
}

int main(int argc, char *argv[]) {
    if (argc >= 3 && strcmp(argv[1], "-c") == 0) {
        char *command = argv[2];
        
        // Handle simple built-in commands to avoid dependency on other binaries
        if (strncmp(command, "echo ", 5) == 0) {
            return builtin_echo(command + 5);
        } else if (strcmp(command, "echo") == 0) {
            return builtin_echo("");
        } else if (strcmp(command, "pwd") == 0) {
            return builtin_pwd();
        } else if (strncmp(command, "echo '", 6) == 0 || strncmp(command, "echo \"", 6) == 0) {
            // Handle quoted echo
            char *start = command + 6;
            char *end = strchr(start, command[5]); // Find matching quote
            if (end) {
                *end = '\0';
                printf("%s\n", start);
                return 0;
            }
        }
        
        // For ALL commands (simple and complex), delegate to the system shell
        // This is the proper way to handle shell commands with operators
        pid_t pid = fork();
        if (pid == 0) {
            // Child process - use system shell to execute the command
            // Look for available shells in order of preference
            char *shells[] = {"/bin/bash", "/bin/dash", "/bin/ash", "/bin/sh", NULL};
            
            for (int i = 0; shells[i] != NULL; i++) {
                if (access(shells[i], X_OK) == 0) {
                    // Found an executable shell, use it
                    execl(shells[i], shells[i], "-c", command, (char *)NULL);
                    break;
                }
            }
            
            // If no standard shell found, try to parse and execute simple commands
            char *args[64];
            char cmd_copy[1024];
            int arg_count = 0;
            
            strncpy(cmd_copy, command, sizeof(cmd_copy)-1);
            cmd_copy[sizeof(cmd_copy)-1] = '\0';
            
            // Simple tokenization for basic commands
            char *token = strtok(cmd_copy, " ");
            while (token != NULL && arg_count < 63) {
                args[arg_count++] = token;
                token = strtok(NULL, " ");
            }
            args[arg_count] = NULL;
            
            if (arg_count > 0) {
                execvp(args[0], args);
            }
            
            // If all exec attempts fail
            fprintf(stderr, "Failed to execute command: %s\n", command);
            exit(127);
        } else if (pid > 0) {
            // Parent process - wait for child
            int status;
            waitpid(pid, &status, 0);
            return WEXITSTATUS(status);
        } else {
            // Fork failed
            fprintf(stderr, "Failed to fork process\n");
            return 1;
        }
    }
    
    // Interactive mode - just print a message and exit
    printf("Minimal shell ready (use -c for command execution)\n");
    return 0;
}
"#;

        // Try to compile a static shell
        let temp_c_file = "/tmp/minimal_shell.c";
        let temp_binary = "/tmp/minimal_shell";
        
        fs::write(temp_c_file, c_program)
            .map_err(|e| format!("Failed to write C file: {}", e))?;

        // First try with static linking
        let mut compile_result = Command::new("gcc")
            .args(&["-static", "-o", temp_binary, temp_c_file])
            .output();

        // If static compilation fails, try dynamic with musl if available
        if compile_result.is_err() || !compile_result.as_ref().unwrap().status.success() {
            compile_result = Command::new("musl-gcc")
                .args(&["-static", "-o", temp_binary, temp_c_file])
                .output();
        }

        // If still fails, try regular dynamic compilation
        if compile_result.is_err() || !compile_result.as_ref().unwrap().status.success() {
            compile_result = Command::new("gcc")
                .args(&["-o", temp_binary, temp_c_file])
                .output();
        }

        match compile_result {
            Ok(output) if output.status.success() => {
                // Check if the binary is usable
                if Path::new(temp_binary).exists() {
                    match fs::copy(temp_binary, shell_path) {
                        Ok(_) => {
                            let mut perms = fs::metadata(shell_path)
                                .map_err(|e| format!("Failed to get shell permissions: {}", e))?
                                .permissions();
                            perms.set_mode(0o755);
                            fs::set_permissions(shell_path, perms)
                                .map_err(|e| format!("Failed to set shell permissions: {}", e))?;
                            
                            // Cleanup
                            fs::remove_file(temp_c_file).ok();
                            fs::remove_file(temp_binary).ok();
                            
                            // Check if it's statically linked
                            if let Ok(ldd_output) = Command::new("ldd").arg(shell_path).output() {
                                let ldd_str = String::from_utf8_lossy(&ldd_output.stdout);
                                if ldd_str.contains("not a dynamic executable") {
                                    println!("  ✅ Created static shell binary");
                                } else {
                                    println!("  ✅ Created dynamic shell binary");
                                }
                            } else {
                                println!("  ✅ Created shell binary");
                            }
                            
                            return Ok(());
                        }
                        Err(e) => {
                            println!("  ⚠ Failed to copy compiled shell: {}", e);
                        }
                    }
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                println!("  ⚠ Compilation failed: {}", stderr);
            }
            Err(e) => {
                println!("  ⚠ Failed to run compiler: {}", e);
            }
        }

        // Cleanup
        fs::remove_file(temp_c_file).ok();
        fs::remove_file(temp_binary).ok();

        Err("Could not create minimal shell binary".to_string())
    }

    /// Create a shell script implementation
    fn create_shell_script(&self, shell_path: &str) -> Result<(), String> {
        // Create a simple shell script that uses exec to replace itself
        let shell_script = r#"#!/bin/sh
# Simple shell for Quilt containers

if [ "$1" = "-c" ]; then
    shift
    # Execute the command using exec to replace the current process
    # This avoids issues with nested shells and process management
    exec /bin/sh -c "$*"
fi

# Interactive mode - simplified
echo "Container shell ready"
        exit 0
"#;

        fs::write(shell_path, shell_script)
            .map_err(|e| format!("Failed to create shell script: {}", e))?;

        // Make it executable
        let mut perms = fs::metadata(shell_path)
            .map_err(|e| format!("Failed to get shell permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(shell_path, perms)
            .map_err(|e| format!("Failed to set shell permissions: {}", e))?;

        println!("  ✅ Created shell script at {}", shell_path);
        Ok(())
    }

    /// Create a simple echo script
    fn create_echo_script(&self, echo_path: &str) -> Result<(), String> {
        let echo_script = r#"#!/bin/sh
# Simple echo implementation
printf '%s\n' "$*"
"#;

        fs::write(echo_path, echo_script)
            .map_err(|e| format!("Failed to create echo script: {}", e))?;

        let mut perms = fs::metadata(echo_path)
            .map_err(|e| format!("Failed to get echo permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(echo_path, perms)
            .map_err(|e| format!("Failed to set echo permissions: {}", e))?;

        println!("  ✅ Created echo script at {}", echo_path);
        Ok(())
    }

    /// Create a simple ls script
    fn create_ls_script(&self, ls_path: &str) -> Result<(), String> {
        let ls_script = r#"#!/bin/sh
# Simple ls implementation
for arg in "$@"; do
    if [ -d "$arg" ]; then
        printf 'Contents of %s:\n' "$arg"
        for f in "$arg"/*; do
            [ -e "$f" ] && printf '%s\n' "${f##*/}"
        done
    elif [ -f "$arg" ]; then
        printf '%s\n' "$arg"
    else
        # Default to current directory
        for f in ./*; do
            [ -e "$f" ] && printf '%s\n' "${f##*/}"
        done
        break
    fi
done
"#;

        fs::write(ls_path, ls_script)
            .map_err(|e| format!("Failed to create ls script: {}", e))?;

        let mut perms = fs::metadata(ls_path)
            .map_err(|e| format!("Failed to get ls permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(ls_path, perms)
            .map_err(|e| format!("Failed to set ls permissions: {}", e))?;

        println!("  ✅ Created ls script at {}", ls_path);
        Ok(())
    }

    /// Create a simple cat script
    fn create_cat_script(&self, cat_path: &str) -> Result<(), String> {
        let cat_script = r#"#!/bin/sh
# Simple cat implementation
if [ $# -eq 0 ]; then
    # Read from stdin (not practical in this context, just exit)
    exit 0
fi

for file in "$@"; do
    if [ -f "$file" ]; then
        while IFS= read -r line; do
            printf '%s\n' "$line"
        done < "$file"
    else
        printf 'cat: %s: No such file or directory\n' "$file" >&2
    fi
done
"#;

        fs::write(cat_path, cat_script)
            .map_err(|e| format!("Failed to create cat script: {}", e))?;

        let mut perms = fs::metadata(cat_path)
            .map_err(|e| format!("Failed to get cat permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(cat_path, perms)
            .map_err(|e| format!("Failed to set cat permissions: {}", e))?;

        println!("  ✅ Created cat script at {}", cat_path);
        Ok(())
    }

    /// Verify that the container shell works
    fn verify_container_shell(&self, rootfs_path: &str) -> Result<(), String> {
        let shell_path = format!("{}/bin/sh", rootfs_path);
        
        if !Path::new(&shell_path).exists() {
            return Err("No shell binary found in container".to_string());
        }

        // Try to execute a simple command in the container environment
        // We can't easily test chroot here, but we can at least verify the binary exists and is executable
        match fs::metadata(&shell_path) {
            Ok(metadata) => {
                if metadata.permissions().mode() & 0o111 != 0 {
                    println!("  ✓ Shell binary is executable: {}", shell_path);
                    Ok(())
                } else {
                    Err(format!("Shell binary is not executable: {}", shell_path))
                }
            }
            Err(e) => Err(format!("Cannot access shell binary: {}", e))
        }
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