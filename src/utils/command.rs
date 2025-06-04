use std::process::{Command, Output};
use std::collections::HashMap;

#[derive(Debug)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: Option<i32>,
}

impl CommandResult {
    pub fn new(output: Output) -> Self {
        Self {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            success: output.status.success(),
            exit_code: output.status.code(),
        }
    }
}

pub struct CommandExecutor;

impl CommandExecutor {
    /// Execute a shell command and return structured result
    pub fn execute_shell(command: &str) -> Result<CommandResult, String> {
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|e| format!("Failed to execute command: {}", e))?;

        Ok(CommandResult::new(output))
    }

    /// Check if a command is available in PATH
    pub fn is_command_available(command: &str) -> bool {
        match Command::new("/bin/sh")
            .arg("-c")
            .arg(&format!("command -v {}", command))
            .output()
        {
            Ok(output) => output.status.success(),
            Err(_) => false,
        }
    }

    /// Execute a package manager command
    pub fn execute_package_manager(manager: &str, action: &str, packages: &[&str]) -> Result<CommandResult, String> {
        let mut cmd = Command::new(manager);
        
        match (manager, action) {
            ("apt", "update") => {
                cmd.arg("update").arg("-y");
            }
            ("apt", "install") => {
                cmd.arg("install").arg("-y");
                cmd.args(packages);
            }
            ("yum", "install") => {
                cmd.arg("install").arg("-y");
                cmd.args(packages);
            }
            ("dnf", "install") => {
                cmd.arg("install").arg("-y");
                cmd.args(packages);
            }
            _ => return Err(format!("Unsupported package manager action: {} {}", manager, action)),
        }

        let output = cmd.output()
            .map_err(|e| format!("Failed to execute {} command: {}", manager, e))?;

        Ok(CommandResult::new(output))
    }

    /// Check if a binary has dependencies in /nix/store (indicating it's Nix-linked)
    pub fn is_nix_linked_binary(binary_path: &str) -> bool {
        match Command::new("ldd").arg(binary_path).output() {
            Ok(output) => {
                let ldd_output = String::from_utf8_lossy(&output.stdout);
                ldd_output.contains("/nix/store")
            }
            Err(_) => false,
        }
    }

    /// Get library dependencies for a binary
    pub fn get_binary_dependencies(binary_path: &str) -> Vec<String> {
        match Command::new("ldd").arg(binary_path).output() {
            Ok(output) => {
                let ldd_output = String::from_utf8_lossy(&output.stdout);
                ldd_output
                    .lines()
                    .filter_map(|line| {
                        if let Some(start) = line.find(" => ") {
                            if let Some(end) = line[start + 4..].find(" (") {
                                let path = line[start + 4..start + 4 + end].trim();
                                if !path.is_empty() && path != "(0x" {
                                    return Some(path.to_string());
                                }
                            }
                        }
                        None
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Check if a package manager command would succeed
    pub fn validate_package_manager_command(manager: &str, packages: &[&str]) -> Result<(), String> {
        if !Self::is_command_available(manager) {
            return Err(format!("Package manager '{}' not available", manager));
        }

        if packages.is_empty() {
            return Err("No packages specified".to_string());
        }

        // For some package managers, we can do dry-run validation
        match manager {
            "apt" => {
                // Check if packages exist in apt cache
                for package in packages {
                    match Command::new("apt")
                        .arg("show")
                        .arg(package)
                        .output()
                    {
                        Ok(output) => {
                            if !output.status.success() {
                                return Err(format!("Package '{}' not found in apt", package));
                            }
                        }
                        Err(_) => return Err(format!("Failed to validate package '{}'", package)),
                    }
                }
            }
            _ => {
                // For other package managers, basic validation passed
            }
        }

        Ok(())
    }

    /// Execute command with environment variables
    pub fn execute_with_env(command: &str, env_vars: HashMap<String, String>) -> Result<CommandResult, String> {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        
        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let output = cmd.output()
            .map_err(|e| format!("Failed to execute command with environment: {}", e))?;

        Ok(CommandResult::new(output))
    }

    /// Execute command with timeout (basic implementation)
    pub fn execute_with_timeout(command: &str, timeout_seconds: u64) -> Result<CommandResult, String> {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        
        // This is a simplified timeout implementation
        // In production, you'd want to use async/await or more sophisticated timeout handling
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .map_err(|e| format!("Failed to execute command: {}", e))?;

        if start.elapsed() > Duration::from_secs(timeout_seconds) {
            return Err("Command execution timed out".to_string());
        }

        Ok(CommandResult::new(output))
    }

    /// Get command output as lines
    pub fn get_command_lines(command: &str) -> Result<Vec<String>, String> {
        let result = Self::execute_shell(command)?;
        
        if !result.success {
            return Err(format!("Command failed: {}", result.stderr));
        }

        Ok(result.stdout
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect())
    }
} 