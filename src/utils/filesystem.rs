use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;

pub struct FileSystemUtils;

impl FileSystemUtils {
    /// Create a directory and all parent directories if they don't exist
    pub fn create_dir_all_with_logging(path: &str, context: &str) -> Result<(), String> {
        match fs::create_dir_all(path) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("Failed to create {} directory '{}': {}", context, path, e)),
        }
    }

    /// Check if a path exists and is a file
    pub fn is_file(path: &str) -> bool {
        Path::new(path).is_file()
    }

    /// Check if a path exists and is a directory
    pub fn is_directory(path: &str) -> bool {
        Path::new(path).is_dir()
    }

    /// Check if a file exists and is executable
    pub fn is_executable(path: &str) -> bool {
        match fs::metadata(path) {
            Ok(metadata) => metadata.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }

    /// Check if a path is a broken symlink
    pub fn is_broken_symlink(path: &str) -> bool {
        let path_obj = Path::new(path);
        
        // Check if it's a symlink
        match fs::read_link(path) {
            Ok(target) => {
                // It's a symlink, check if the target exists
                let target_path = if target.is_absolute() {
                    target
                } else {
                    // Relative symlink - resolve relative to the symlink's directory
                    if let Some(parent) = path_obj.parent() {
                        parent.join(target)
                    } else {
                        target
                    }
                };
                
                !target_path.exists()
            }
            Err(_) => false, // Not a symlink
        }
    }

    /// Make a file executable
    pub fn make_executable(path: &str) -> Result<(), String> {
        let mut perms = fs::metadata(path)
            .map_err(|e| format!("Failed to get permissions for {}: {}", path, e))?
            .permissions();
        
        perms.set_mode(0o755);
        
        fs::set_permissions(path, perms)
            .map_err(|e| format!("Failed to set permissions for {}: {}", path, e))
    }

    /// Copy a file from source to destination, creating parent directories if needed
    pub fn copy_file(src: &str, dest: &str) -> Result<(), String> {
        // Create parent directory if it doesn't exist
        if let Some(parent) = Path::new(dest).parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }
        }

        fs::copy(src, dest)
            .map_err(|e| format!("Failed to copy {} to {}: {}", src, dest, e))?;

        Ok(())
    }

    /// Remove a file or directory (recursively if it's a directory)
    pub fn remove_path(path: &str) -> Result<(), String> {
        let path_obj = Path::new(path);
        
        if path_obj.is_dir() {
            fs::remove_dir_all(path)
                .map_err(|e| format!("Failed to remove directory {}: {}", path, e))
        } else if path_obj.exists() {
            fs::remove_file(path)
                .map_err(|e| format!("Failed to remove file {}: {}", path, e))
        } else {
            Ok(()) // Path doesn't exist, nothing to remove
        }
    }

    /// Get file size in bytes
    pub fn get_file_size(path: &str) -> Result<u64, String> {
        let metadata = fs::metadata(path)
            .map_err(|e| format!("Failed to get metadata for {}: {}", path, e))?;
        
        Ok(metadata.len())
    }

    /// Check if path exists (file or directory)
    pub fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }

    /// Read file contents as string
    pub fn read_to_string(path: &str) -> Result<String, String> {
        fs::read_to_string(path)
            .map_err(|e| format!("Failed to read file {}: {}", path, e))
    }

    /// Write string to file
    pub fn write_string(path: &str, content: &str) -> Result<(), String> {
        // Create parent directory if needed
        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }
        }

        fs::write(path, content)
            .map_err(|e| format!("Failed to write to file {}: {}", path, e))
    }

    /// Get the canonical path (resolving symlinks)
    pub fn canonicalize(path: &str) -> Result<PathBuf, String> {
        fs::canonicalize(path)
            .map_err(|e| format!("Failed to canonicalize path {}: {}", path, e))
    }

    /// Create a symlink from src to dest
    pub fn create_symlink(src: &str, dest: &str) -> Result<(), String> {
        // Create parent directory if needed
        if let Some(parent) = Path::new(dest).parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent directory: {}", e))?;
            }
        }

        std::os::unix::fs::symlink(src, dest)
            .map_err(|e| format!("Failed to create symlink from {} to {}: {}", src, dest, e))
    }

    /// List directory contents
    pub fn list_directory(path: &str) -> Result<Vec<String>, String> {
        let entries = fs::read_dir(path)
            .map_err(|e| format!("Failed to read directory {}: {}", path, e))?;

        let mut file_names = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    if let Some(name) = entry.file_name().to_str() {
                        file_names.push(name.to_string());
                    }
                }
                Err(e) => return Err(format!("Error reading directory entry: {}", e)),
            }
        }

        Ok(file_names)
    }

    /// Get file permissions as octal string
    pub fn get_permissions(path: &str) -> Result<String, String> {
        let metadata = fs::metadata(path)
            .map_err(|e| format!("Failed to get metadata for {}: {}", path, e))?;
        
        Ok(format!("{:o}", metadata.permissions().mode() & 0o777))
    }

    /// Set file permissions from octal mode
    pub fn set_permissions(path: &str, mode: u32) -> Result<(), String> {
        let mut perms = fs::metadata(path)
            .map_err(|e| format!("Failed to get metadata for {}: {}", path, e))?
            .permissions();
        
        perms.set_mode(mode);
        
        fs::set_permissions(path, perms)
            .map_err(|e| format!("Failed to set permissions for {}: {}", path, e))
    }
}

/// Specialized utility for creating container directory structures
pub struct DirectoryCreator;

impl DirectoryCreator {
    /// Create all necessary directories for a container
    pub fn create_container_dirs(container_id: &str, base_path: &str) -> Result<(), String> {
        let container_root = format!("{}/{}", base_path, container_id);
        
        let directories = vec![
            container_root.clone(),
            format!("{}/bin", container_root),
            format!("{}/etc", container_root),
            format!("{}/lib", container_root),
            format!("{}/lib64", container_root),
            format!("{}/tmp", container_root),
            format!("{}/var", container_root),
            format!("{}/var/log", container_root),
            format!("{}/dev", container_root),
            format!("{}/proc", container_root),
            format!("{}/sys", container_root),
            format!("{}/root", container_root),
        ];

        for dir in directories {
            FileSystemUtils::create_dir_all_with_logging(&dir, "container directory")?;
        }

        Ok(())
    }

    /// Create essential runtime directories for container
    pub fn create_runtime_dirs(container_root: &str) -> Result<(), String> {
        let runtime_dirs = vec![
            format!("{}/run", container_root),
            format!("{}/run/lock", container_root),
            format!("{}/var/run", container_root),
            format!("{}/var/tmp", container_root),
            format!("{}/var/cache", container_root),
        ];

        for dir in runtime_dirs {
            FileSystemUtils::create_dir_all_with_logging(&dir, "runtime directory")?;
        }

        Ok(())
    }

    /// Create log directories for container
    pub fn create_log_dirs(container_root: &str) -> Result<(), String> {
        let log_dirs = vec![
            format!("{}/var/log", container_root),
            format!("{}/var/log/quilt", container_root),
            format!("{}/tmp/logs", container_root),
        ];

        for dir in log_dirs {
            FileSystemUtils::create_dir_all_with_logging(&dir, "log directory")?;
        }

        Ok(())
    }
}

/// Utility for managing library dependencies in containers
pub struct LibraryManager;

impl LibraryManager {
    /// Copy essential system libraries to container
    pub fn copy_essential_libraries(host_lib_dir: &str, container_lib_dir: &str) -> Result<(), String> {
        let essential_libs = vec![
            "libc.so.6",
            "ld-linux-x86-64.so.2", 
            "libdl.so.2",
            "libm.so.6",
            "libpthread.so.0",
            "libresolv.so.2",
            "libnss_dns.so.2",
            "libnss_files.so.2",
        ];

        FileSystemUtils::create_dir_all_with_logging(container_lib_dir, "container library directory")?;

        for lib in essential_libs {
            let host_path = format!("{}/{}", host_lib_dir, lib);
            let container_path = format!("{}/{}", container_lib_dir, lib);
            
            if FileSystemUtils::is_file(&host_path) {
                FileSystemUtils::copy_file(&host_path, &container_path)?;
            }
        }

        Ok(())
    }

    /// Create library search path configuration
    pub fn create_library_config(container_root: &str) -> Result<(), String> {
        let ld_config_path = format!("{}/etc/ld.so.conf", container_root);
        let lib_paths = "/lib\n/lib64\n/usr/lib\n/usr/lib64\n";
        
        FileSystemUtils::write_string(&ld_config_path, lib_paths)?;
        
        Ok(())
    }

    /// Fix library symlinks in container
    pub fn fix_library_symlinks(container_lib_dir: &str) -> Result<(), String> {
        let entries = FileSystemUtils::list_directory(container_lib_dir)?;
        
        for entry in entries {
            let lib_path = format!("{}/{}", container_lib_dir, entry);
            
            if FileSystemUtils::is_broken_symlink(&lib_path) {
                // Remove broken symlink and try to find a working version
                FileSystemUtils::remove_path(&lib_path)?;
                
                // Look for versioned libraries that might work
                if let Ok(all_entries) = FileSystemUtils::list_directory(container_lib_dir) {
                    for candidate in all_entries {
                        if candidate.starts_with(&entry) && candidate != entry {
                            let candidate_path = format!("{}/{}", container_lib_dir, candidate);
                            if FileSystemUtils::is_file(&candidate_path) {
                                FileSystemUtils::create_symlink(&candidate, &lib_path)?;
                                break;
                            }
                        }
                    }
                }
            }
        }
        
        Ok(())
    }
} 