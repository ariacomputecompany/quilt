use std::process::{Command, Stdio};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RuntimeType {
    NodeJs,      // npm, yarn, node
    Python,      // pip, conda, python
    Rust,        // cargo, rustc
    Go,          // go mod, go get
    Java,        // maven, gradle
    System,      // apt, apk, yum (system package managers)
}

#[derive(Debug, Clone)]
pub struct SetupCommand {
    pub runtime_type: RuntimeType,
    pub command: String,
    pub args: Vec<String>,
    pub description: String,
}

impl SetupCommand {
    pub fn new(runtime_type: RuntimeType, command: String, args: Vec<String>, description: String) -> Self {
        SetupCommand {
            runtime_type,
            command,
            args,
            description,
        }
    }

    /// Create a Node.js npm install command
    pub fn npm_install(packages: Vec<String>) -> Self {
        let mut args = vec!["install".to_string(), "-g".to_string()];
        args.extend(packages.clone());
        
        SetupCommand {
            runtime_type: RuntimeType::NodeJs,
            command: "npm".to_string(),
            args,
            description: format!("Install npm packages: {}", packages.join(", ")),
        }
    }

    /// Create a Python pip install command
    pub fn pip_install(packages: Vec<String>) -> Self {
        let mut args = vec!["install".to_string()];
        args.extend(packages.clone());
        
        SetupCommand {
            runtime_type: RuntimeType::Python,
            command: "pip".to_string(),
            args,
            description: format!("Install pip packages: {}", packages.join(", ")),
        }
    }

    /// Create an Alpine apk add command
    pub fn apk_add(packages: Vec<String>) -> Self {
        let mut args = vec!["add".to_string(), "--no-cache".to_string()];
        args.extend(packages.clone());
        
        SetupCommand {
            runtime_type: RuntimeType::System,
            command: "apk".to_string(),
            args,
            description: format!("Install Alpine packages: {}", packages.join(", ")),
        }
    }

    /// Create an Ubuntu/Debian apt install command
    pub fn apt_install(packages: Vec<String>) -> Self {
        let mut args = vec!["install".to_string(), "-y".to_string()];
        args.extend(packages.clone());
        
        SetupCommand {
            runtime_type: RuntimeType::System,
            command: "apt".to_string(),
            args,
            description: format!("Install apt packages: {}", packages.join(", ")),
        }
    }

    /// Create a Rust cargo install command
    pub fn cargo_install(packages: Vec<String>) -> Self {
        let mut args = vec!["install".to_string()];
        args.extend(packages.clone());
        
        SetupCommand {
            runtime_type: RuntimeType::Rust,
            command: "cargo".to_string(),
            args,
            description: format!("Install cargo crates: {}", packages.join(", ")),
        }
    }

    /// Create a Go install command
    pub fn go_install(packages: Vec<String>) -> Self {
        let mut args = vec!["install".to_string()];
        args.extend(packages.clone());
        
        SetupCommand {
            runtime_type: RuntimeType::Go,
            command: "go".to_string(),
            args,
            description: format!("Install Go packages: {}", packages.join(", ")),
        }
    }
}

pub struct RuntimeManager {
    runtime_configs: HashMap<RuntimeType, RuntimeConfig>,
}

#[derive(Debug, Clone)]
struct RuntimeConfig {
    install_commands: Vec<SetupCommand>,  // Commands to install the runtime itself
    verify_command: Option<String>,       // Command to verify runtime is available
}

impl RuntimeManager {
    pub fn new() -> Self {
        let mut runtime_configs = HashMap::new();
        
        // Node.js runtime configuration
        runtime_configs.insert(RuntimeType::NodeJs, RuntimeConfig {
            install_commands: vec![
                SetupCommand::apk_add(vec!["nodejs".to_string(), "npm".to_string()]),
            ],
            verify_command: Some("node --version".to_string()),
        });

        // Python runtime configuration
        runtime_configs.insert(RuntimeType::Python, RuntimeConfig {
            install_commands: vec![
                SetupCommand::apk_add(vec!["python3".to_string(), "py3-pip".to_string()]),
            ],
            verify_command: Some("python3 --version".to_string()),
        });

        // Rust runtime configuration
        runtime_configs.insert(RuntimeType::Rust, RuntimeConfig {
            install_commands: vec![
                SetupCommand::apk_add(vec!["cargo".to_string(), "rust".to_string()]),
            ],
            verify_command: Some("cargo --version".to_string()),
        });

        // Go runtime configuration
        runtime_configs.insert(RuntimeType::Go, RuntimeConfig {
            install_commands: vec![
                SetupCommand::apk_add(vec!["go".to_string()]),
            ],
            verify_command: Some("go version".to_string()),
        });

        // Java runtime configuration
        runtime_configs.insert(RuntimeType::Java, RuntimeConfig {
            install_commands: vec![
                SetupCommand::apk_add(vec!["openjdk11".to_string(), "maven".to_string()]),
            ],
            verify_command: Some("java -version".to_string()),
        });

        // System runtime (package managers are usually pre-installed)
        runtime_configs.insert(RuntimeType::System, RuntimeConfig {
            install_commands: vec![],
            verify_command: None,
        });

        RuntimeManager {
            runtime_configs,
        }
    }

    /// Execute a series of setup commands in sequence
    pub fn execute_setup_commands(&self, commands: &[SetupCommand]) -> Result<Vec<String>, String> {
        let mut results = Vec::new();
        
        for command in commands {
            println!("Executing setup command: {}", command.description);
            
            // Ensure runtime is available before executing command
            if let Err(e) = self.ensure_runtime_available(&command.runtime_type) {
                return Err(format!("Failed to ensure runtime {:?} is available: {}", command.runtime_type, e));
            }
            
            // Execute the command
            match self.execute_command(&command.command, &command.args) {
                Ok(output) => {
                    println!("✅ Successfully executed: {}", command.description);
                    results.push(output);
                }
                Err(e) => {
                    let error_msg = format!("❌ Failed to execute '{}': {}", command.description, e);
                    eprintln!("{}", error_msg);
                    return Err(error_msg);
                }
            }
        }
        
        Ok(results)
    }

    /// Ensure a runtime is available, installing it if necessary
    fn ensure_runtime_available(&self, runtime_type: &RuntimeType) -> Result<(), String> {
        if let Some(config) = self.runtime_configs.get(runtime_type) {
            // Check if runtime is already available
            if let Some(verify_cmd) = &config.verify_command {
                if self.check_command_available(verify_cmd) {
                    println!("Runtime {:?} is already available", runtime_type);
                    return Ok(());
                }
            }

            // Install runtime if not available
            if !config.install_commands.is_empty() {
                println!("Installing runtime {:?}", runtime_type);
                for install_cmd in &config.install_commands {
                    if let Err(e) = self.execute_command(&install_cmd.command, &install_cmd.args) {
                        return Err(format!("Failed to install runtime {:?}: {}", runtime_type, e));
                    }
                }

                // Verify installation
                if let Some(verify_cmd) = &config.verify_command {
                    if !self.check_command_available(verify_cmd) {
                        return Err(format!("Runtime {:?} installation verification failed", runtime_type));
                    }
                }
            }
        }

        Ok(())
    }

    /// Check if a command is available
    fn check_command_available(&self, command: &str) -> bool {
        let parts: Vec<&str> = command.split_whitespace().collect();
        if parts.is_empty() {
            return false;
        }

        Command::new(parts[0])
            .args(&parts[1..])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// Execute a command and return its output
    fn execute_command(&self, command: &str, args: &[String]) -> Result<String, String> {
        println!("Executing: {} {}", command, args.join(" "));
        
        let output = Command::new(command)
            .args(args)
            .output()
            .map_err(|e| format!("Failed to execute command '{}': {}", command, e))?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            
            // Return combined output
            let result = if stderr.is_empty() {
                stdout.to_string()
            } else {
                format!("stdout: {}\nstderr: {}", stdout, stderr)
            };
            
            Ok(result)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("Command '{}' failed with exit code {}: {}", 
                       command, 
                       output.status.code().unwrap_or(-1), 
                       stderr))
        }
    }

    /// Parse setup commands from a string specification
    pub fn parse_setup_spec(&self, spec: &str) -> Result<Vec<SetupCommand>, String> {
        let mut commands = Vec::new();
        
        for line in spec.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse different command formats
            if line.starts_with("npm:") {
                let packages: Vec<String> = line[4..].split_whitespace()
                    .map(|s| s.to_string()).collect();
                commands.push(SetupCommand::npm_install(packages));
            } else if line.starts_with("pip:") {
                let packages: Vec<String> = line[4..].split_whitespace()
                    .map(|s| s.to_string()).collect();
                commands.push(SetupCommand::pip_install(packages));
            } else if line.starts_with("apk:") {
                let packages: Vec<String> = line[4..].split_whitespace()
                    .map(|s| s.to_string()).collect();
                commands.push(SetupCommand::apk_add(packages));
            } else if line.starts_with("apt:") {
                let packages: Vec<String> = line[4..].split_whitespace()
                    .map(|s| s.to_string()).collect();
                commands.push(SetupCommand::apt_install(packages));
            } else if line.starts_with("cargo:") {
                let packages: Vec<String> = line[6..].split_whitespace()
                    .map(|s| s.to_string()).collect();
                commands.push(SetupCommand::cargo_install(packages));
            } else if line.starts_with("go:") {
                let packages: Vec<String> = line[3..].split_whitespace()
                    .map(|s| s.to_string()).collect();
                commands.push(SetupCommand::go_install(packages));
            } else {
                // Generic command format: "command arg1 arg2 ..."
                let parts: Vec<String> = line.split_whitespace()
                    .map(|s| s.to_string()).collect();
                if !parts.is_empty() {
                    let command = parts[0].clone();
                    let args = parts[1..].to_vec();
                    commands.push(SetupCommand::new(
                        RuntimeType::System,
                        command.clone(),
                        args,
                        format!("Execute: {}", line),
                    ));
                }
            }
        }

        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_npm_install_command() {
        let cmd = SetupCommand::npm_install(vec!["typescript".to_string(), "ts-node".to_string()]);
        assert_eq!(cmd.command, "npm");
        assert_eq!(cmd.args, vec!["install", "-g", "typescript", "ts-node"]);
    }

    #[test]
    fn test_apk_add_command() {
        let cmd = SetupCommand::apk_add(vec!["python3".to_string(), "pip".to_string()]);
        assert_eq!(cmd.command, "apk");
        assert_eq!(cmd.args, vec!["add", "--no-cache", "python3", "pip"]);
    }

    #[test]
    fn test_parse_setup_spec() {
        let manager = RuntimeManager::new();
        let spec = r#"
            # Install Node.js packages
            npm: typescript ts-node
            
            # Install Python packages  
            pip: requests beautifulsoup4
            
            # Install system packages
            apk: curl wget
        "#;

        let commands = manager.parse_setup_spec(spec).unwrap();
        assert_eq!(commands.len(), 3);
        
        assert_eq!(commands[0].command, "npm");
        assert_eq!(commands[1].command, "pip");
        assert_eq!(commands[2].command, "apk");
    }

    #[test]
    fn test_runtime_manager_creation() {
        let manager = RuntimeManager::new();
        assert!(manager.runtime_configs.contains_key(&RuntimeType::NodeJs));
        assert!(manager.runtime_configs.contains_key(&RuntimeType::Python));
        assert!(manager.runtime_configs.contains_key(&RuntimeType::Rust));
    }
} 