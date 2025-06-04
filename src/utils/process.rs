use nix::unistd::Pid;
use nix::sys::signal::{self, Signal};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct ProcessUtils;

impl ProcessUtils {
    /// Convert nix::Pid to i32 for display/logging
    pub fn pid_to_i32(pid: Pid) -> i32 {
        pid.as_raw()
    }

    /// Convert i32 to nix::Pid
    pub fn i32_to_pid(pid: i32) -> Pid {
        Pid::from_raw(pid)
    }

    /// Check if a process is still running
    pub fn is_process_running(pid: Pid) -> bool {
        match signal::kill(pid, None) {
            Ok(()) => true,  // Process exists
            Err(_) => false, // Process doesn't exist or we don't have permission
        }
    }

    /// Gracefully terminate a process with SIGTERM, then SIGKILL if needed
    pub fn terminate_process(pid: Pid, timeout_seconds: u64) -> Result<(), String> {
        use std::thread;
        use std::time::Duration;

        // Check if process is still running
        if !Self::is_process_running(pid) {
            return Ok(()); // Process is already dead
        }

        // Send SIGTERM first
        if let Err(e) = signal::kill(pid, Signal::SIGTERM) {
            return Err(format!("Failed to send SIGTERM to process {}: {}", Self::pid_to_i32(pid), e));
        }

        // Wait for graceful shutdown
        for _ in 0..timeout_seconds {
            thread::sleep(Duration::from_secs(1));
            if !Self::is_process_running(pid) {
                return Ok(()); // Process terminated gracefully
            }
        }

        // Process still running, use SIGKILL
        if Self::is_process_running(pid) {
            if let Err(e) = signal::kill(pid, Signal::SIGKILL) {
                return Err(format!("Failed to send SIGKILL to process {}: {}", Self::pid_to_i32(pid), e));
            }

            // Give it a moment to die
            thread::sleep(Duration::from_millis(100));
            
            if Self::is_process_running(pid) {
                return Err(format!("Process {} refused to die even after SIGKILL", Self::pid_to_i32(pid)));
            }
        }

        Ok(())
    }

    /// Send a signal to a process
    pub fn send_signal(pid: Pid, signal: Signal) -> Result<(), String> {
        signal::kill(pid, signal)
            .map_err(|e| format!("Failed to send signal {:?} to process {}: {}", signal, Self::pid_to_i32(pid), e))
    }

    /// Get current timestamp in seconds since Unix epoch
    pub fn get_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Format timestamp as human-readable string
    pub fn format_timestamp(timestamp: u64) -> String {
        use std::time::{Duration, UNIX_EPOCH};
        
        let datetime = UNIX_EPOCH + Duration::from_secs(timestamp);
        
        // Simple formatting - in production you'd want to use chrono or similar
        match datetime.elapsed() {
            Ok(elapsed) => {
                let secs = elapsed.as_secs();
                if secs < 60 {
                    format!("{}s ago", secs)
                } else if secs < 3600 {
                    format!("{}m ago", secs / 60)
                } else if secs < 86400 {
                    format!("{}h ago", secs / 3600)
                } else {
                    format!("{}d ago", secs / 86400)
                }
            }
            Err(_) => format!("timestamp: {}", timestamp),
        }
    }

    /// Get process information (simple version)
    pub fn get_process_info(pid: Pid) -> Result<ProcessInfo, String> {
        if !Self::is_process_running(pid) {
            return Err(format!("Process {} is not running", Self::pid_to_i32(pid)));
        }

        // Read from /proc filesystem
        let pid_i32 = Self::pid_to_i32(pid);
        let stat_path = format!("/proc/{}/stat", pid_i32);
        let cmdline_path = format!("/proc/{}/cmdline", pid_i32);

        let stat_content = std::fs::read_to_string(&stat_path)
            .map_err(|e| format!("Failed to read process stat: {}", e))?;

        let cmdline_content = std::fs::read_to_string(&cmdline_path)
            .unwrap_or_else(|_| String::new());

        // Parse basic info from stat file
        let stat_parts: Vec<&str> = stat_content.split_whitespace().collect();
        if stat_parts.len() < 3 {
            return Err("Invalid stat format".to_string());
        }

        let state = stat_parts.get(2).unwrap_or(&"?").to_string();
        
        // Clean up command line (replace null bytes with spaces)
        let command = cmdline_content.replace('\0', " ").trim().to_string();
        let command = if command.is_empty() {
            format!("[process {}]", pid_i32)
        } else {
            command
        };

        Ok(ProcessInfo {
            pid: pid_i32,
            command,
            state,
        })
    }

    /// Get list of child processes
    pub fn get_child_processes(parent_pid: Pid) -> Vec<Pid> {
        let mut children = Vec::new();
        let parent_i32 = Self::pid_to_i32(parent_pid);

        // Read /proc to find child processes
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Ok(pid) = name.parse::<i32>() {
                        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
                            // Parse parent PID from stat file (4th field)
                            let parts: Vec<&str> = stat.split_whitespace().collect();
                            if parts.len() > 3 {
                                if let Ok(ppid) = parts[3].parse::<i32>() {
                                    if ppid == parent_i32 {
                                        children.push(Self::i32_to_pid(pid));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        children
    }

    /// Wait for any child process to exit
    pub fn wait_for_children() -> Result<Vec<(Pid, i32)>, String> {
        use nix::sys::wait::{waitpid, WaitStatus, WaitPidFlag};
        
        let mut exited_children = Vec::new();
        
        // Non-blocking wait for any child
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, exit_code)) => {
                    exited_children.push((pid, exit_code));
                }
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    // Convert signal to exit code (128 + signal number)
                    let exit_code = 128 + signal as i32;
                    exited_children.push((pid, exit_code));
                }
                Ok(WaitStatus::StillAlive) => {
                    // No more children to wait for
                    break;
                }
                Ok(_) => {
                    // Other status, continue waiting
                    continue;
                }
                Err(nix::errno::Errno::ECHILD) => {
                    // No children
                    break;
                }
                Err(e) => {
                    return Err(format!("Error waiting for children: {}", e));
                }
            }
        }
        
        Ok(exited_children)
    }
}

#[derive(Debug)]
pub struct ProcessInfo {
    pub pid: i32,
    pub command: String,
    pub state: String,
}

/// Simple PID manager for tracking container processes
pub struct PidManager {
    container_pids: std::collections::HashMap<String, Pid>,
}

impl PidManager {
    pub fn new() -> Self {
        Self {
            container_pids: std::collections::HashMap::new(),
        }
    }

    /// Register a PID for a container
    pub fn register_container_pid(&mut self, container_id: String, pid: Pid) {
        self.container_pids.insert(container_id, pid);
    }

    /// Get PID for a container
    pub fn get_container_pid(&self, container_id: &str) -> Option<Pid> {
        self.container_pids.get(container_id).copied()
    }

    /// Remove PID registration for a container
    pub fn unregister_container(&mut self, container_id: &str) -> Option<Pid> {
        self.container_pids.remove(container_id)
    }

    /// Get all registered container PIDs
    pub fn get_all_pids(&self) -> Vec<(String, Pid)> {
        self.container_pids
            .iter()
            .map(|(id, pid)| (id.clone(), *pid))
            .collect()
    }

    /// Clean up dead processes
    pub fn cleanup_dead_processes(&mut self) {
        self.container_pids.retain(|_id, pid| ProcessUtils::is_process_running(*pid));
    }

    /// Terminate all tracked processes
    pub fn terminate_all(&mut self, timeout_seconds: u64) -> Result<(), String> {
        let pids: Vec<Pid> = self.container_pids.values().copied().collect();
        
        for pid in pids {
            if let Err(e) = ProcessUtils::terminate_process(pid, timeout_seconds) {
                eprintln!("Warning: Failed to terminate process {}: {}", ProcessUtils::pid_to_i32(pid), e);
            }
        }
        
        self.container_pids.clear();
        Ok(())
    }
} 