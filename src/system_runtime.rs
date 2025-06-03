use std::process::Command;
use std::env;
use std::fs;
use std::path::Path;
use std::os::unix::fs::PermissionsExt;

pub struct SystemRuntime;

impl SystemRuntime {
    pub fn new() -> Self {
        SystemRuntime
    }

    /// Initialize the basic container environment
    pub fn initialize_container_environment(&self) -> Result<(), String> {
        println!("🔧 Initializing container system environment...");

        // Set up basic environment variables
        self.setup_environment_variables()?;
        
        // Verify basic system binaries
        self.verify_system_binaries()?;
        
        // Initialize basic directories
        self.initialize_basic_directories()?;

        println!("✅ Container system environment initialized");
        Ok(())
    }

    /// Set up essential environment variables
    fn setup_environment_variables(&self) -> Result<(), String> {
        // Set PATH to include both traditional and Nix store locations
        let path_dirs = vec![
            "/usr/local/sbin",
            "/usr/local/bin", 
            "/usr/sbin",
            "/usr/bin",
            "/sbin",
            "/bin",
            "/nix/store/*/bin",  // Include potential Nix store paths
        ];
        
        let path = path_dirs.join(":");
        env::set_var("PATH", &path);
        
        // Set other essential environment variables
        env::set_var("HOME", "/root");
        env::set_var("USER", "root");
        env::set_var("SHELL", "/bin/sh");
        env::set_var("TERM", "xterm");
        
        // Nix-specific environment variables
        env::set_var("NIX_SSL_CERT_FILE", "/etc/ssl/certs/ca-certificates.crt");
        
        println!("  ✓ Environment variables set (PATH, HOME, USER, SHELL, TERM)");
        Ok(())
    }

    /// Verify that basic system binaries are available
    fn verify_system_binaries(&self) -> Result<(), String> {
        // Check for basic shell first
        let shell_candidates = vec!["/bin/sh", "/bin/bash"];
        let mut working_shell = None;
        
        for shell in &shell_candidates {
            if Path::new(shell).exists() {
                // For containers where we've fixed binaries, just check if the file exists and is executable
                // Don't try to execute it yet since we're in a chrooted environment that may have missing libraries
                match std::fs::metadata(shell) {
                    Ok(metadata) => {
                        if metadata.permissions().mode() & 0o111 != 0 {
                            working_shell = Some(shell);
                            println!("  ✓ Found executable shell: {}", shell);
                            break;
                        }
                    }
                    Err(_) => continue,
                }
            }
        }

        if let Some(shell) = working_shell {
            println!("  ✓ Working shell found: {}", shell);
            env::set_var("SHELL", shell);
        } else {
            // More forgiving error - warn but don't fail
            println!("  ⚠ No shell found, but continuing anyway");
            println!("    Container execution will depend on command availability");
            env::set_var("SHELL", "/bin/sh"); // Set default
        }

        // Verify we can find basic commands (but don't execute them in chroot)
        let test_commands = vec!["echo", "ls", "cat"];

        for cmd in test_commands {
            let cmd_path = format!("/bin/{}", cmd);
            if Path::new(&cmd_path).exists() {
                if let Ok(metadata) = std::fs::metadata(&cmd_path) {
                    if metadata.permissions().mode() & 0o111 != 0 {
                        println!("  ✓ Command '{}' available and executable", cmd);
                    } else {
                        println!("  ⚠ Command '{}' exists but not executable", cmd);
                    }
                } else {
                    println!("  ⚠ Command '{}' not accessible", cmd);
                }
            } else {
                println!("  ⚠ Command '{}' not found", cmd);
            }
        }

        Ok(())
    }

    /// Initialize basic directories that should exist in containers
    fn initialize_basic_directories(&self) -> Result<(), String> {
        let basic_dirs = vec![
            "/tmp",
            "/var/log",
            "/var/tmp",
            "/root"
        ];

        for dir in &basic_dirs {
            if !Path::new(dir).exists() {
                if let Err(e) = fs::create_dir_all(dir) {
                    eprintln!("Warning: Failed to create directory {}: {}", dir, e);
                } else {
                    println!("  ✓ Created directory: {}", dir);
                }
            }
        }

        Ok(())
    }

    /// Check if a package manager is available and functional
    pub fn check_package_manager_availability(&self) -> Result<String, String> {
        // First check if we're in a Nix environment
        if self.check_nix_environment() {
            println!("  ✓ Nix environment detected");
            return Ok("nix".to_string());
        }

        // Check for Alpine's apk
        if self.test_command_availability("apk") {
            println!("  ✓ Package manager detected: apk (Alpine)");
            return Ok("apk".to_string());
        }

        // Check for Debian/Ubuntu apt
        if self.test_command_availability("apt") {
            println!("  ✓ Package manager detected: apt (Debian/Ubuntu)");
            return Ok("apt".to_string());
        }

        // Check for RedHat/CentOS yum
        if self.test_command_availability("yum") {
            println!("  ✓ Package manager detected: yum (RedHat/CentOS)");
            return Ok("yum".to_string());
        }

        // Check for newer dnf
        if self.test_command_availability("dnf") {
            println!("  ✓ Package manager detected: dnf (Fedora/newer RedHat)");
            return Ok("dnf".to_string());
        }

        // Fallback: assume we can work without a package manager
        println!("  ⚠ No traditional package manager found, using basic environment");
        Ok("none".to_string())
    }

    /// Check if we're running in a Nix-generated environment
    fn check_nix_environment(&self) -> bool {
        // Check for Nix store paths in filesystem
        if Path::new("/nix/store").exists() {
            return true;
        }

        // Check if binaries are from Nix store
        if let Ok(output) = Command::new("/bin/sh")
            .arg("-c")
            .arg("ls -la /bin/* 2>/dev/null | head -5")
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.contains("/nix/store") {
                return true;
            }
        }

        // Check for Nix-style directory structure
        let nix_indicators = vec![
            "/nix",
            "/nix/store",
        ];

        for indicator in nix_indicators {
            if Path::new(indicator).exists() {
                return true;
            }
        }

        false
    }

    /// Test if a command is available and executable
    fn test_command_availability(&self, command: &str) -> bool {
        // Try to execute the command with --version or --help first
        match Command::new(command).arg("--version").output() {
            Ok(output) => output.status.success(),
            Err(_) => {
                // Fallback: try with --help 
                match Command::new(command).arg("--help").output() {
                    Ok(output) => output.status.success(),
                    Err(_) => {
                        // Final fallback: check if command exists using shell
                        match Command::new("/bin/sh").arg("-c").arg(&format!("command -v {}", command)).output() {
                            Ok(output) => output.status.success(),
                            Err(_) => false
                        }
                    }
                }
            }
        }
    }

    /// Prepare the container for package installation
    pub fn prepare_for_package_installation(&self, package_manager: &str) -> Result<(), String> {
        println!("🔧 Preparing container for package installation...");

        match package_manager {
            "nix" => self.prepare_nix_environment(),
            "apk" => self.prepare_apk_environment(),
            "apt" => self.prepare_apt_environment(), 
            "yum" | "dnf" => self.prepare_rpm_environment(),
            "none" => {
                println!("  ✓ No package manager preparation needed");
                Ok(())
            }
            _ => Err(format!("Unsupported package manager: {}", package_manager))
        }
    }

    /// Prepare Nix environment (mostly verification)
    fn prepare_nix_environment(&self) -> Result<(), String> {
        println!("  ✓ Nix environment detected - packages are pre-installed in rootfs");
        println!("  ℹ Nix setup commands will install packages directly without package manager");
        Ok(())
    }

    /// Prepare Alpine apk environment
    fn prepare_apk_environment(&self) -> Result<(), String> {
        // Update package index
        println!("  🔄 Updating apk package index...");
        match Command::new("apk").arg("update").output() {
            Ok(output) => {
                if output.status.success() {
                    println!("  ✓ APK package index updated");
                } else {
                    eprintln!("Warning: APK update failed: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                return Err(format!("Failed to update APK package index: {}", e));
            }
        }

        Ok(())
    }

    /// Prepare Debian/Ubuntu apt environment  
    fn prepare_apt_environment(&self) -> Result<(), String> {
        // Update package index
        println!("  🔄 Updating apt package index...");
        match Command::new("apt").args(["update", "-y"]).output() {
            Ok(output) => {
                if output.status.success() {
                    println!("  ✓ APT package index updated");
                } else {
                    eprintln!("Warning: APT update failed: {}", String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                return Err(format!("Failed to update APT package index: {}", e));
            }
        }

        Ok(())
    }

    /// Prepare RPM-based environment (yum/dnf)
    fn prepare_rpm_environment(&self) -> Result<(), String> {
        // RPM systems typically don't need explicit index updates
        println!("  ✓ RPM-based system ready for package installation");
        Ok(())
    }

    /// Install a runtime environment (e.g., python3, nodejs, etc.)
    pub fn install_runtime(&self, package_manager: &str, runtime_name: &str, packages: &[&str]) -> Result<(), String> {
        println!("🔧 Installing {} runtime...", runtime_name);
        
        match package_manager {
            "nix" => {
                println!("  ℹ Nix environment: {} runtime should already be available", runtime_name);
                println!("  📦 Requested packages: {:?}", packages);
                
                // For Nix, we assume packages are already available in the environment
                // but we can check if they're actually present
                for package in packages {
                    if let Ok(output) = Command::new("/bin/sh")
                        .arg("-c")
                        .arg(&format!("command -v {}", package))
                        .output() 
                    {
                        if output.status.success() {
                            println!("  ✓ Package '{}' available", package);
                        } else {
                            println!("  ⚠ Package '{}' not found in PATH", package);
                        }
                    }
                }
                
                Ok(())
            }
            "none" => {
                println!("  ℹ No package manager: {} runtime should be pre-installed", runtime_name);
                Ok(())
            }
            _ => {
                let mut install_command = match package_manager {
                    "apk" => {
                        let mut cmd = Command::new("apk");
                        cmd.arg("add").arg("--no-cache");
                        cmd.args(packages);
                        cmd
                    }
                    "apt" => {
                        let mut cmd = Command::new("apt");
                        cmd.arg("install").arg("-y");
                        cmd.args(packages);
                        cmd
                    }
                    "yum" => {
                        let mut cmd = Command::new("yum");
                        cmd.arg("install").arg("-y");
                        cmd.args(packages);
                        cmd
                    }
                    "dnf" => {
                        let mut cmd = Command::new("dnf");
                        cmd.arg("install").arg("-y");
                        cmd.args(packages);
                        cmd
                    }
                    _ => return Err(format!("Unsupported package manager: {}", package_manager))
                };

                println!("  🔄 Installing packages: {:?}", packages);
                match install_command.output() {
                    Ok(output) => {
                        if output.status.success() {
                            println!("  ✅ Successfully installed {} runtime", runtime_name);
                            
                            // Print installation output for debugging
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            if !stdout.trim().is_empty() {
                                println!("    Installation output: {}", stdout.trim());
                            }
                            
                            Ok(())
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            Err(format!("Failed to install {} runtime: {}", runtime_name, stderr))
                        }
                    }
                    Err(e) => {
                        Err(format!("Failed to execute package installation command: {}", e))
                    }
                }
            }
        }
    }
} 