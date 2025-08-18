use std::collections::HashMap;
use regex::Regex;

/// Mount type enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum MountType {
    Bind,
    Volume,
    Tmpfs,
}

/// Volume mount configuration
#[derive(Debug, Clone)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub mount_type: MountType,
    pub readonly: bool,
    pub options: HashMap<String, String>,
}

pub struct InputValidator;

impl InputValidator {
    /// Parse key=value pairs from strings
    pub fn parse_key_val(s: &str) -> Result<(String, String), String> {
        let pos = s.find('=').ok_or_else(|| {
            format!("Invalid KEY=VALUE format: '{}'", s)
        })?;
        
        let key = s[..pos].trim().to_string();
        let value = s[pos + 1..].trim().to_string();
        
        if key.is_empty() {
            return Err("Empty key in KEY=VALUE pair".to_string());
        }
        
        Ok((key, value))
    }

    /// Validate container ID format
    pub fn validate_container_id(id: &str) -> Result<(), String> {
        if id.is_empty() {
            return Err("Container ID cannot be empty".to_string());
        }
        
        if id.len() > 64 {
            return Err("Container ID too long (max 64 characters)".to_string());
        }
        
        // Only allow alphanumeric, hyphens, and underscores
        let regex = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
        if !regex.is_match(id) {
            return Err("Container ID can only contain letters, numbers, hyphens, and underscores".to_string());
        }
        
        // Cannot start with hyphen or underscore
        if id.starts_with('-') || id.starts_with('_') {
            return Err("Container ID cannot start with hyphen or underscore".to_string());
        }
        
        Ok(())
    }

    /// Validate image path
    pub fn validate_image_path(path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Err("Image path cannot be empty".to_string());
        }
        
        if !path.ends_with(".tar.gz") && !path.ends_with(".tgz") {
            return Err("Image must be a .tar.gz or .tgz file".to_string());
        }
        
        // Basic path traversal protection
        if path.contains("..") {
            return Err("Image path cannot contain '..' for security reasons".to_string());
        }
        
        Ok(())
    }

    /// Validate command format
    pub fn validate_command(command: &[String]) -> Result<(), String> {
        if command.is_empty() {
            return Err("Command cannot be empty".to_string());
        }
        
        let program = &command[0];
        if program.is_empty() {
            return Err("Program name cannot be empty".to_string());
        }
        
        // Check for potentially dangerous commands
        let dangerous_commands = ["rm", "dd", "mkfs", "fdisk", "parted", "mount", "umount"];
        if dangerous_commands.contains(&program.as_str()) {
            return Err(format!("Command '{}' is not allowed for security reasons", program));
        }
        
        Ok(())
    }

    /// Validate environment variable name
    pub fn validate_env_name(name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("Environment variable name cannot be empty".to_string());
        }
        
        // Environment variable names should start with letter or underscore,
        // followed by letters, numbers, or underscores
        let regex = Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$").unwrap();
        if !regex.is_match(name) {
            return Err("Invalid environment variable name format".to_string());
        }
        
        Ok(())
    }

    /// Validate memory limit
    pub fn validate_memory_limit(limit_str: &str) -> Result<u64, String> {
        if limit_str.is_empty() {
            return Err("Memory limit cannot be empty".to_string());
        }
        
        let limit_str = limit_str.to_lowercase();
        
        // Parse size with unit
        let (number_str, multiplier) = if limit_str.ends_with("gb") {
            (&limit_str[..limit_str.len()-2], 1024 * 1024 * 1024)
        } else if limit_str.ends_with("mb") {
            (&limit_str[..limit_str.len()-2], 1024 * 1024)
        } else if limit_str.ends_with("kb") {
            (&limit_str[..limit_str.len()-2], 1024)
        } else if limit_str.ends_with("b") {
            (&limit_str[..limit_str.len()-1], 1)
        } else {
            // Assume bytes if no unit
            (limit_str.as_str(), 1)
        };
        
        let number: u64 = number_str.parse()
            .map_err(|_| format!("Invalid memory limit format: '{}'", limit_str))?;
        
        let bytes = number * multiplier;
        
        // Minimum 1MB
        if bytes < 1024 * 1024 {
            return Err("Memory limit must be at least 1MB".to_string());
        }
        
        // Maximum 32GB (reasonable for containers)
        if bytes > 32 * 1024 * 1024 * 1024 {
            return Err("Memory limit cannot exceed 32GB".to_string());
        }
        
        Ok(bytes)
    }

    /// Validate CPU limit (as a percentage)
    pub fn validate_cpu_limit(limit_str: &str) -> Result<f64, String> {
        if limit_str.is_empty() {
            return Err("CPU limit cannot be empty".to_string());
        }
        
        let limit_str = limit_str.trim_end_matches('%');
        let limit: f64 = limit_str.parse()
            .map_err(|_| format!("Invalid CPU limit format: '{}'", limit_str))?;
        
        if limit <= 0.0 {
            return Err("CPU limit must be greater than 0".to_string());
        }
        
        if limit > 100.0 {
            return Err("CPU limit cannot exceed 100%".to_string());
        }
        
        Ok(limit)
    }

    /// Validate port number
    pub fn validate_port(port_str: &str) -> Result<u16, String> {
        let port: u16 = port_str.parse()
            .map_err(|_| format!("Invalid port format: '{}'", port_str))?;
        
        if port < 1024 {
            return Err("Port number must be 1024 or higher".to_string());
        }
        
        Ok(port)
    }

    /// Parse volume mount specification (-v flag format)
    /// Formats supported:
    /// - /host/path:/container/path
    /// - /host/path:/container/path:ro
    /// - volume-name:/container/path
    /// - volume-name:/container/path:rw
    pub fn parse_volume(s: &str) -> Result<VolumeMount, String> {
        let parts: Vec<&str> = s.split(':').collect();
        
        if parts.len() < 2 || parts.len() > 3 {
            return Err("Volume format must be '[name:]source:dest[:options]'".to_string());
        }
        
        let source = parts[0].trim().to_string();
        let target = parts[1].trim().to_string();
        let options = if parts.len() == 3 { parts[2].trim() } else { "rw" };
        
        if source.is_empty() || target.is_empty() {
            return Err("Volume source and target cannot be empty".to_string());
        }
        
        if !target.starts_with('/') {
            return Err("Container path must be absolute".to_string());
        }
        
        // Determine mount type based on source
        let mount_type = if source.starts_with('/') {
            MountType::Bind
        } else if source.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
            MountType::Volume
        } else {
            // Assume bind mount for other patterns (security validation will catch issues later)
            MountType::Bind
        };
        
        // Parse options
        let readonly = options.contains("ro");
        
        Ok(VolumeMount {
            source,
            target,
            mount_type,
            readonly,
            options: HashMap::new(),
        })
    }
    
    /// Parse advanced mount specification (--mount flag format)
    /// Format: type=bind,source=/host/path,target=/container/path,readonly
    pub fn parse_mount(s: &str) -> Result<VolumeMount, String> {
        let mut mount = VolumeMount {
            source: String::new(),
            target: String::new(),
            mount_type: MountType::Bind,
            readonly: false,
            options: HashMap::new(),
        };
        
        for part in s.split(',') {
            let trimmed = part.trim();
            
            // Handle boolean flags (no value)
            if trimmed == "readonly" || trimmed == "ro" {
                mount.readonly = true;
                continue;
            }
            
            // Handle key=value pairs
            let kv: Vec<&str> = trimmed.split('=').collect();
            if kv.len() != 2 {
                return Err(format!("Invalid mount option: '{}'", part));
            }
            
            let key = kv[0].trim();
            let value = kv[1].trim();
            
            match key {
                "type" => {
                    mount.mount_type = match value {
                        "bind" => MountType::Bind,
                        "volume" => MountType::Volume,
                        "tmpfs" => MountType::Tmpfs,
                        _ => return Err(format!("Unknown mount type: '{}'", value)),
                    };
                }
                "source" | "src" => mount.source = value.to_string(),
                "target" | "dst" | "destination" => mount.target = value.to_string(),
                "readonly" | "ro" => mount.readonly = true,
                _ => {
                    mount.options.insert(key.to_string(), value.to_string());
                }
            }
        }
        
        // Validate required fields
        if mount.target.is_empty() {
            return Err("Mount target is required".to_string());
        }
        
        if mount.mount_type != MountType::Tmpfs && mount.source.is_empty() {
            return Err("Mount source is required for bind and volume mounts".to_string());
        }
        
        if !mount.target.starts_with('/') {
            return Err("Mount target must be an absolute path".to_string());
        }
        
        Ok(mount)
    }

    /// Validate mount point format (legacy method for compatibility)
    pub fn validate_mount_point(mount: &str) -> Result<(String, String), String> {
        let parts: Vec<&str> = mount.split(':').collect();
        if parts.len() != 2 {
            return Err("Mount format must be 'host_path:container_path'".to_string());
        }
        
        let host_path = parts[0].trim().to_string();
        let container_path = parts[1].trim().to_string();
        
        if host_path.is_empty() || container_path.is_empty() {
            return Err("Mount paths cannot be empty".to_string());
        }
        
        // Basic security checks
        if host_path.contains("..") || container_path.contains("..") {
            return Err("Mount paths cannot contain '..' for security reasons".to_string());
        }
        
        if !container_path.starts_with('/') {
            return Err("Container mount path must be absolute".to_string());
        }
        
        Ok((host_path, container_path))
    }

    /// Sanitize string for safe usage
    pub fn sanitize_string(input: &str, max_length: usize) -> String {
        let sanitized = input
            .chars()
            .filter(|c| c.is_alphanumeric() || " -_.,".contains(*c))
            .take(max_length)
            .collect();
        
        sanitized
    }

    /// Validate environment variables map
    pub fn validate_env_vars(env_vars: &HashMap<String, String>) -> Result<(), String> {
        for (key, value) in env_vars {
            Self::validate_env_name(key)?;
            
            if value.len() > 4096 {
                return Err(format!("Environment variable '{}' value too long (max 4096 chars)", key));
            }
        }
        
        Ok(())
    }
}

/// Configuration validator for complete container setups
pub struct ConfigValidator;

impl ConfigValidator {
    /// Validate complete container configuration
    pub fn validate_container_config(
        container_id: &str,
        image_path: &str,
        command: &[String],
        env_vars: &HashMap<String, String>,
        memory_limit: Option<&str>,
        cpu_limit: Option<&str>,
    ) -> Result<(), String> {
        
        InputValidator::validate_container_id(container_id)?;
        InputValidator::validate_image_path(image_path)?;
        InputValidator::validate_command(command)?;
        InputValidator::validate_env_vars(env_vars)?;
        
        if let Some(memory) = memory_limit {
            InputValidator::validate_memory_limit(memory)?;
        }
        
        if let Some(cpu) = cpu_limit {
            InputValidator::validate_cpu_limit(cpu)?;
        }
        
        Ok(())
    }

    /// Validate resource limits are reasonable
    pub fn validate_resource_limits(
        memory_bytes: Option<u64>,
        cpu_shares: Option<u64>,
        pids_limit: Option<u64>,
    ) -> Result<(), String> {
        
        if let Some(memory) = memory_bytes {
            if memory < 1024 * 1024 {  // 1MB minimum
                return Err("Memory limit too low (minimum 1MB)".to_string());
            }
            if memory > 64 * 1024 * 1024 * 1024 {  // 64GB maximum
                return Err("Memory limit too high (maximum 64GB)".to_string());
            }
        }
        
        if let Some(shares) = cpu_shares {
            if shares < 10 {
                return Err("CPU shares too low (minimum 10)".to_string());
            }
            if shares > 10240 {
                return Err("CPU shares too high (maximum 10240)".to_string());
            }
        }
        
        if let Some(pids) = pids_limit {
            if pids < 10 {
                return Err("PIDs limit too low (minimum 10)".to_string());
            }
            if pids > 32768 {
                return Err("PIDs limit too high (maximum 32768)".to_string());
            }
        }
        
        Ok(())
    }

    /// Validate network configuration
    pub fn validate_network_config(
        ports: &[String],
        hostname: Option<&str>,
    ) -> Result<(), String> {
        
        for port_mapping in ports {
            // Parse port mapping like "8080:80" or just "80"
            if port_mapping.contains(':') {
                let parts: Vec<&str> = port_mapping.split(':').collect();
                if parts.len() != 2 {
                    return Err(format!("Invalid port mapping format: '{}'", port_mapping));
                }
                InputValidator::validate_port(parts[0])?;
                InputValidator::validate_port(parts[1])?;
            } else {
                InputValidator::validate_port(port_mapping)?;
            }
        }
        
        if let Some(host) = hostname {
            if host.is_empty() {
                return Err("Hostname cannot be empty".to_string());
            }
            if host.len() > 63 {
                return Err("Hostname too long (max 63 characters)".to_string());
            }
            
            let hostname_regex = Regex::new(r"^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?$").unwrap();
            if !hostname_regex.is_match(host) {
                return Err("Invalid hostname format".to_string());
            }
        }
        
        Ok(())
    }

    /// Validate setup commands configuration
    pub fn validate_setup_commands(commands: &[String]) -> Result<(), String> {
        if commands.len() > 20 {
            return Err("Too many setup commands (maximum 20)".to_string());
        }
        
        for command in commands {
            if command.trim().is_empty() {
                return Err("Setup command cannot be empty".to_string());
            }
            
            if command.len() > 1000 {
                return Err("Setup command too long (max 1000 characters)".to_string());
            }
            
            // Basic security check - no shell metacharacters that could be dangerous
            if command.contains("&&") || command.contains("||") || command.contains(";") {
                return Err("Setup commands cannot contain shell operators for security".to_string());
            }
        }
        
        Ok(())
    }

    /// Validate volume mount configuration
    pub fn validate_volume_mounts(mounts: &[String]) -> Result<(), String> {
        if mounts.len() > 10 {
            return Err("Too many volume mounts (maximum 10)".to_string());
        }
        
        let mut container_paths = std::collections::HashSet::new();
        
        for mount in mounts {
            let (_host_path, container_path) = InputValidator::validate_mount_point(mount)?;
            
            // Check for duplicate container paths
            if container_paths.contains(&container_path) {
                return Err(format!("Duplicate container mount path: '{}'", container_path));
            }
            container_paths.insert(container_path.clone());
            
            // Additional security checks
            let restricted_paths = ["/", "/bin", "/sbin", "/usr", "/lib", "/lib64", "/boot", "/proc", "/sys"];
            if restricted_paths.iter().any(|&path| container_path.starts_with(path)) {
                return Err(format!("Cannot mount to restricted path: '{}'", container_path));
            }
        }
        
        Ok(())
    }

    /// Validate complete runtime configuration
    pub fn validate_runtime_config(
        runtime_type: &str,
        packages: &[String],
    ) -> Result<(), String> {
        
        let valid_runtimes = ["nodejs", "python", "python3", "ruby", "go", "java", "php", "rust", "nix"];
        if !valid_runtimes.contains(&runtime_type) {
            return Err(format!("Unsupported runtime type: '{}'", runtime_type));
        }
        
        if packages.len() > 50 {
            return Err("Too many packages (maximum 50)".to_string());
        }
        
        for package in packages {
            if package.trim().is_empty() {
                return Err("Package name cannot be empty".to_string());
            }
            
            if package.len() > 100 {
                return Err(format!("Package name too long: '{}'", package));
            }
            
            // Basic validation - package names should be reasonable
            let package_regex = Regex::new(r"^[a-zA-Z0-9._@/-]+$").unwrap();
            if !package_regex.is_match(package) {
                return Err(format!("Invalid package name format: '{}'", package));
            }
        }
        
        Ok(())
    }
} 