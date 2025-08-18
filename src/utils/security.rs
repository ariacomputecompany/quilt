use std::path::{Path, PathBuf};
use super::validation::{VolumeMount, MountType};

pub struct SecurityValidator;

impl SecurityValidator {
    /// Validate mount source path for security issues
    pub fn validate_mount_source(path: &str, mount_type: MountType) -> Result<(), String> {
        match mount_type {
            MountType::Bind => {
                // Prevent path traversal
                if path.contains("..") {
                    return Err("Path traversal detected".to_string());
                }
                
                // Check if path exists
                let path_obj = Path::new(path);
                if !path_obj.exists() {
                    return Err(format!("Mount source path does not exist: {}", path));
                }
                
                // Deny sensitive system paths
                const DENIED_PATHS: &[&str] = &[
                    "/etc/passwd",
                    "/etc/shadow", 
                    "/etc/sudoers",
                    "/proc",
                    "/sys",
                    "/dev",
                    "/boot",
                    "/root/.ssh",
                ];
                
                let canonical = match path_obj.canonicalize() {
                    Ok(p) => p,
                    Err(_) => return Err(format!("Cannot resolve path: {}", path)),
                };
                
                let canonical_str = canonical.to_string_lossy();
                for denied in DENIED_PATHS {
                    if canonical_str.starts_with(denied) {
                        return Err(format!("Security: Mounting {} is not allowed", denied));
                    }
                }
                
                // Warn about risky paths
                const RISKY_PATHS: &[&str] = &["/home", "/var", "/opt"];
                for risky in RISKY_PATHS {
                    if canonical_str.starts_with(risky) {
                        eprintln!("Warning: Mounting {} may expose sensitive data", risky);
                    }
                }
            }
            MountType::Volume => {
                // Validate volume name format
                if !Self::is_valid_volume_name(path) {
                    return Err("Invalid volume name: must contain only alphanumeric, dash, or underscore".to_string());
                }
            }
            MountType::Tmpfs => {
                // No source validation needed for tmpfs
                if !path.is_empty() {
                    return Err("Tmpfs mount should not have a source path".to_string());
                }
            }
        }
        Ok(())
    }
    
    /// Validate mount target path for security issues
    pub fn validate_mount_target(path: &str) -> Result<(), String> {
        // Must be absolute path
        if !path.starts_with('/') {
            return Err("Mount target must be an absolute path".to_string());
        }
        
        // Prevent path traversal
        if path.contains("..") {
            return Err("Path traversal detected".to_string());
        }
        
        // Prevent mounting over critical container paths
        const PROTECTED_PATHS: &[&str] = &[
            "/",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/usr",
            "/proc",
            "/sys",
            "/dev",
            "/etc",
            "/.dockerenv",
        ];
        
        for protected in PROTECTED_PATHS {
            if path == *protected || (path.len() > 1 && path.trim_end_matches('/') == *protected) {
                return Err(format!("Cannot mount over protected path: {}", protected));
            }
        }
        
        Ok(())
    }
    
    /// Validate complete mount configuration
    pub fn validate_mount(mount: &VolumeMount) -> Result<(), String> {
        // Validate source
        Self::validate_mount_source(&mount.source, mount.mount_type.clone())?;
        
        // Validate target
        Self::validate_mount_target(&mount.target)?;
        
        // Additional validation for specific mount types
        match mount.mount_type {
            MountType::Tmpfs => {
                // Validate tmpfs options
                if let Some(size) = mount.options.get("size") {
                    Self::validate_tmpfs_size(size)?;
                }
            }
            _ => {}
        }
        
        Ok(())
    }
    
    /// Check if a volume name is valid
    fn is_valid_volume_name(name: &str) -> bool {
        !name.is_empty() && 
        name.len() <= 64 &&
        name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }
    
    /// Validate tmpfs size option
    fn validate_tmpfs_size(size: &str) -> Result<(), String> {
        // Parse size with units (e.g., "100m", "1g")
        let size_lower = size.to_lowercase();
        let (number_str, multiplier) = if size_lower.ends_with("g") {
            (&size_lower[..size_lower.len()-1], 1024 * 1024 * 1024)
        } else if size_lower.ends_with("m") {
            (&size_lower[..size_lower.len()-1], 1024 * 1024)
        } else if size_lower.ends_with("k") {
            (&size_lower[..size_lower.len()-1], 1024)
        } else {
            return Err("Tmpfs size must include unit (k, m, or g)".to_string());
        };
        
        let number: u64 = number_str.parse()
            .map_err(|_| format!("Invalid tmpfs size: {}", size))?;
        
        let bytes = number * multiplier;
        
        // Minimum 1MB
        if bytes < 1024 * 1024 {
            return Err("Tmpfs size must be at least 1m".to_string());
        }
        
        // Maximum 10GB for safety
        if bytes > 10 * 1024 * 1024 * 1024 {
            return Err("Tmpfs size cannot exceed 10g".to_string());
        }
        
        Ok(())
    }
    
    /// Check if a path would escape the container
    pub fn check_container_escape(container_root: &str, resolved_path: &str) -> Result<(), String> {
        let container_root = Path::new(container_root).canonicalize()
            .map_err(|e| format!("Cannot resolve container root: {}", e))?;
        
        let resolved = Path::new(resolved_path).canonicalize()
            .map_err(|e| format!("Cannot resolve path: {}", e))?;
        
        if !resolved.starts_with(&container_root) {
            return Err("Path would escape container root".to_string());
        }
        
        Ok(())
    }
    
    /// Validate that volume operations won't compromise security
    pub fn validate_volume_operation(operation: &str, volume_name: &str, user_id: Option<u32>) -> Result<(), String> {
        // Check volume name
        if !Self::is_valid_volume_name(volume_name) {
            return Err("Invalid volume name".to_string());
        }
        
        // In future, check user permissions here
        // For now, allow all operations
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_deny_sensitive_paths() {
        assert!(SecurityValidator::validate_mount_source("/etc/passwd", MountType::Bind).is_err());
        assert!(SecurityValidator::validate_mount_source("/proc", MountType::Bind).is_err());
        assert!(SecurityValidator::validate_mount_source("/sys", MountType::Bind).is_err());
    }
    
    #[test]
    fn test_allow_safe_paths() {
        // These tests would need actual directories to exist
        // assert!(SecurityValidator::validate_mount_source("/tmp", MountType::Bind).is_ok());
    }
    
    #[test]
    fn test_volume_name_validation() {
        assert!(SecurityValidator::is_valid_volume_name("my-data"));
        assert!(SecurityValidator::is_valid_volume_name("test_vol_123"));
        assert!(!SecurityValidator::is_valid_volume_name("my/data"));
        assert!(!SecurityValidator::is_valid_volume_name("my..data"));
        assert!(!SecurityValidator::is_valid_volume_name(""));
    }
    
    #[test]
    fn test_mount_target_validation() {
        assert!(SecurityValidator::validate_mount_target("/data").is_ok());
        assert!(SecurityValidator::validate_mount_target("/app/config").is_ok());
        assert!(SecurityValidator::validate_mount_target("/").is_err());
        assert!(SecurityValidator::validate_mount_target("/etc").is_err());
        assert!(SecurityValidator::validate_mount_target("/proc").is_err());
        assert!(SecurityValidator::validate_mount_target("../etc").is_err());
    }
    
    #[test]
    fn test_tmpfs_size_validation() {
        assert!(SecurityValidator::validate_tmpfs_size("100m").is_ok());
        assert!(SecurityValidator::validate_tmpfs_size("1g").is_ok());
        assert!(SecurityValidator::validate_tmpfs_size("512k").is_err()); // Too small
        assert!(SecurityValidator::validate_tmpfs_size("20g").is_err()); // Too large
        assert!(SecurityValidator::validate_tmpfs_size("100").is_err()); // No unit
    }
}