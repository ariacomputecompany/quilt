use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use crate::quilt_rpc::log_stream_response::LogSource;

#[derive(Debug, Clone)]
pub struct ContainerState {
    pub id: String,
    pub image_tarball_path: String,
    pub rootfs_path: String,
    pub command: String,
    pub args: Vec<String>,
    pub env_vars: Vec<String>,
    pub status: String, // e.g., PENDING, RUNNING, EXITED, FAILED
    pub exit_code: Option<i32>,
    pub pid: Option<u32>,
    pub logs: Vec<LogEntry>,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub source: LogSource,
    pub content: String,
    pub timestamp_nanos: i64,
}

#[derive(Debug)]
pub struct ContainerRuntime {
    pub containers: Arc<Mutex<HashMap<String, ContainerState>>>,
}

impl ContainerRuntime {
    pub fn new() -> Self {
        ContainerRuntime {
            containers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // Execute container command with chroot isolation
    pub async fn execute_container_command(
        containers: Arc<Mutex<HashMap<String, ContainerState>>>,
        container_id: String,
    ) {
        println!("Starting execution for container: {}", container_id);

        let (rootfs_path, command, args, env_vars) = {
            let containers_map = containers.lock().await;
            if let Some(state) = containers_map.get(&container_id) {
                (
                    state.rootfs_path.clone(),
                    state.command.clone(),
                    state.args.clone(),
                    state.env_vars.clone(),
                )
            } else {
                eprintln!("Container {} not found for execution", container_id);
                return;
            }
        };

        println!("Executing container {} with command: {} {:?}", container_id, command, args);

        // Update status to RUNNING
        {
            let mut containers_map = containers.lock().await;
            if let Some(state) = containers_map.get_mut(&container_id) {
                state.status = "RUNNING".to_string();
                println!("Updated container {} status to RUNNING", container_id);
            }
        }

        // Create the execution script that will chroot and run the command
        let script_content = format!(
            r#"#!/bin/bash
set -e
cd "{}"
echo "Changing to rootfs directory: $(pwd)"
echo "Executing: chroot . {} {}"
exec chroot . {} {}
"#,
            rootfs_path,
            command,
            args.join(" "),
            command,
            args.join(" ")
        );

        let script_path = format!("/tmp/quilt_exec_{}.sh", container_id);
        
        // Write the script
        if let Err(e) = std::fs::write(&script_path, script_content) {
            eprintln!("Failed to write execution script for container {}: {}", container_id, e);
            Self::mark_container_failed(&containers, &container_id, -1).await;
            return;
        }

        // Make script executable
        if let Err(e) = std::fs::set_permissions(&script_path, std::os::unix::fs::PermissionsExt::from_mode(0o755)) {
            eprintln!("Failed to make script executable for container {}: {}", container_id, e);
            Self::mark_container_failed(&containers, &container_id, -1).await;
            std::fs::remove_file(&script_path).ok();
            return;
        }

        println!("Created execution script at: {}", script_path);

        // Execute the command using the script
        let mut child = match tokio::process::Command::new("bash")
            .arg(&script_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(env_vars.iter().filter_map(|env| {
                let parts: Vec<&str> = env.splitn(2, '=').collect();
                if parts.len() == 2 {
                    Some((parts[0], parts[1]))
                } else {
                    None
                }
            }))
            .spawn()
        {
            Ok(child) => {
                println!("Successfully spawned process for container {}", container_id);
                child
            },
            Err(e) => {
                eprintln!("Failed to spawn container process for {}: {}", container_id, e);
                Self::mark_container_failed(&containers, &container_id, -1).await;
                std::fs::remove_file(&script_path).ok();
                return;
            }
        };

        // Store the PID
        if let Some(pid) = child.id() {
            let mut containers_map = containers.lock().await;
            if let Some(state) = containers_map.get_mut(&container_id) {
                state.pid = Some(pid);
                println!("Container {} running with PID: {}", container_id, pid);
            }
        }

        // Capture stdout and stderr
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let containers_clone = containers.clone();
        let container_id_clone = container_id.clone();
        
        // Spawn task to capture stdout
        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let log_entry = LogEntry {
                    source: LogSource::Stdout,
                    content: line.clone(),
                    timestamp_nanos: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos() as i64,
                };
                
                println!("Container {} STDOUT: {}", container_id_clone, line);
                
                let mut containers_map = containers_clone.lock().await;
                if let Some(state) = containers_map.get_mut(&container_id_clone) {
                    state.logs.push(log_entry);
                }
            }
        });

        let containers_clone = containers.clone();
        let container_id_clone = container_id.clone();
        
        // Spawn task to capture stderr
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let log_entry = LogEntry {
                    source: LogSource::Stderr,
                    content: line.clone(),
                    timestamp_nanos: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos() as i64,
                };
                
                println!("Container {} STDERR: {}", container_id_clone, line);
                
                let mut containers_map = containers_clone.lock().await;
                if let Some(state) = containers_map.get_mut(&container_id_clone) {
                    state.logs.push(log_entry);
                }
            }
        });

        // Wait for the process to complete
        let exit_status = child.wait().await;
        
        println!("Container {} process finished, waiting for log capture", container_id);
        
        // Wait for log capture tasks to complete
        stdout_task.await.ok();
        stderr_task.await.ok();

        // Update container status based on exit
        {
            let mut containers_map = containers.lock().await;
            if let Some(state) = containers_map.get_mut(&container_id) {
                match exit_status {
                    Ok(status) => {
                        state.status = "EXITED".to_string();
                        let exit_code = status.code().unwrap_or(-1);
                        state.exit_code = Some(exit_code);
                        println!("Container {} exited with code: {}", container_id, exit_code);
                    }
                    Err(e) => {
                        state.status = "FAILED".to_string();
                        state.exit_code = Some(-1);
                        eprintln!("Container {} failed: {}", container_id, e);
                    }
                }
                state.pid = None;
            }
        }

        // Clean up the script file
        std::fs::remove_file(&script_path).ok();
        
        println!("Container {} execution completed", container_id);
    }

    async fn mark_container_failed(
        containers: &Arc<Mutex<HashMap<String, ContainerState>>>,
        container_id: &str,
        exit_code: i32,
    ) {
        let mut containers_map = containers.lock().await;
        if let Some(state) = containers_map.get_mut(container_id) {
            state.status = "FAILED".to_string();
            state.exit_code = Some(exit_code);
        }
    }

    pub async fn start_container_execution(&self, container_id: String) {
        let containers_clone = self.containers.clone();
        let container_id_clone = container_id.clone();
        
        println!("Spawning execution task for container: {}", container_id);
        
        tokio::spawn(async move {
            Self::execute_container_command(containers_clone, container_id_clone).await;
        });
    }

    pub async fn stop_container(&self, container_id: &str) -> Result<String, String> {
        let mut containers_map = self.containers.lock().await;
        match containers_map.get_mut(container_id) {
            Some(state) => {
                if let Some(pid) = state.pid {
                    // Try to terminate the process gracefully
                    println!("Stopping container {} (PID: {})", container_id, pid);
                    
                    // Send SIGTERM to the process
                    let kill_result = std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(pid.to_string())
                        .output();
                    
                    match kill_result {
                        Ok(output) => {
                            if output.status.success() {
                                state.status = "STOPPED".to_string();
                                state.exit_code = Some(0);
                                state.pid = None;
                                Ok("STOPPED".to_string())
                            } else {
                                let error_msg = format!("Failed to stop container {}: {}", container_id, String::from_utf8_lossy(&output.stderr));
                                eprintln!("{}", error_msg);
                                state.status = "FAILED".to_string();
                                Err(error_msg)
                            }
                        }
                        Err(e) => {
                            let error_msg = format!("Failed to send SIGTERM to container {}: {}", container_id, e);
                            eprintln!("{}", error_msg);
                            state.status = "FAILED".to_string();
                            Err(error_msg)
                        }
                    }
                } else {
                    // Container is not running, mark as stopped
                    state.status = "STOPPED".to_string();
                    state.exit_code = Some(0);
                    Ok("STOPPED".to_string())
                }
            }
            None => Err(format!("Container {} not found", container_id))
        }
    }

    pub async fn remove_container(&self, container_id: &str) -> Result<(), String> {
        let mut containers_map = self.containers.lock().await;
        
        if containers_map.contains_key(container_id) {
            // Clean up the container directory
            let container_dir = PathBuf::from("./active_containers").join(container_id);
            if container_dir.exists() {
                if let Err(e) = std::fs::remove_dir_all(&container_dir) {
                    let error_msg = format!("Failed to remove container directory {}: {}", container_dir.display(), e);
                    eprintln!("{}", error_msg);
                    return Err(error_msg);
                }
                println!("Removed container directory: {}", container_dir.display());
            }

            containers_map.remove(container_id);
            Ok(())
        } else {
            Err(format!("Container {} not found", container_id))
        }
    }
} 