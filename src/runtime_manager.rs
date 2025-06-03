use std::process::Command;
use std::collections::HashMap;
use crate::system_runtime::SystemRuntime;

#[derive(Debug, Clone)]
pub enum Runtime {
    NodeJs,
    Python,
    Ruby,
    Go,
    Java,
    Php,
    Nix,
    Custom(String),
}

impl Runtime {
    pub fn from_string(runtime: &str) -> Result<Runtime, String> {
        match runtime.to_lowercase().as_str() {
            "node" | "nodejs" | "npm" => Ok(Runtime::NodeJs),
            "python" | "python3" | "pip" => Ok(Runtime::Python),
            "ruby" | "gem" => Ok(Runtime::Ruby),
            "go" | "golang" => Ok(Runtime::Go),
            "java" | "maven" | "gradle" => Ok(Runtime::Java),
            "php" | "composer" => Ok(Runtime::Php),
            "nix" => Ok(Runtime::Nix),
            custom => Ok(Runtime::Custom(custom.to_string())),
        }
    }

    pub fn get_name(&self) -> String {
        match self {
            Runtime::NodeJs => "NodeJs".to_string(),
            Runtime::Python => "Python".to_string(),
            Runtime::Ruby => "Ruby".to_string(),
            Runtime::Go => "Go".to_string(),
            Runtime::Java => "Java".to_string(),
            Runtime::Php => "PHP".to_string(),
            Runtime::Nix => "Nix".to_string(),
            Runtime::Custom(name) => name.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SetupCommand {
    pub runtime: Runtime,
    pub packages: Vec<String>,
}

pub struct RuntimeManager {
    system_runtime: SystemRuntime,
    installed_runtimes: HashMap<String, Runtime>,
    available_package_manager: Option<String>,
}

impl RuntimeManager {
    pub fn new() -> Self {
        RuntimeManager {
            system_runtime: SystemRuntime::new(),
            installed_runtimes: HashMap::new(),
            available_package_manager: None,
        }
    }

    /// Initialize the container environment and detect available package manager
    pub fn initialize_container(&mut self) -> Result<(), String> {
        println!("🚀 Initializing container runtime environment...");

        // First, initialize the basic system environment
        self.system_runtime.initialize_container_environment()?;

        // Detect and prepare package manager
        match self.system_runtime.check_package_manager_availability() {
            Ok(package_manager) => {
                self.available_package_manager = Some(package_manager.clone());
                self.system_runtime.prepare_for_package_installation(&package_manager)?;
                println!("✅ Container runtime environment ready with package manager: {}", package_manager);
            }
            Err(e) => {
                eprintln!("⚠️  Warning: {}", e);
                eprintln!("    Setup commands will be skipped.");
                self.available_package_manager = None;
            }
        }

        Ok(())
    }

    pub fn parse_setup_spec(&self, setup_spec: &str) -> Result<Vec<SetupCommand>, String> {
        let mut commands = Vec::new();
        
        for line in setup_spec.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            let command = self.parse_setup_line(line)?;
            commands.push(command);
        }
        
        Ok(commands)
    }

    fn parse_setup_line(&self, line: &str) -> Result<SetupCommand, String> {
        if let Some((runtime_str, packages_str)) = line.split_once(':') {
            let runtime = Runtime::from_string(runtime_str.trim())?;
            let packages: Vec<String> = packages_str
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            
            if packages.is_empty() {
                return Err(format!("No packages specified for runtime: {}", runtime_str));
            }
            
            Ok(SetupCommand { runtime, packages })
        } else {
            Err(format!("Invalid setup command format: '{}'. Expected 'runtime: package1 package2'", line))
        }
    }

    pub fn execute_setup_commands(&mut self, commands: &[SetupCommand]) -> Result<(), String> {
        if commands.is_empty() {
            return Ok(());
        }

        // Ensure container is initialized
        if self.available_package_manager.is_none() {
            self.initialize_container()?;
        }

        let package_manager = match &self.available_package_manager {
            Some(pm) => pm.clone(),
            None => "none".to_string(),
        };

        for command in commands {
            println!("Executing setup command: Install {} packages: {}", 
                    command.runtime.get_name(), 
                    command.packages.join(", "));
            
            if matches!(command.runtime, Runtime::Nix) {
                self.handle_nix_packages(&command.packages)?;
            } else {
                self.ensure_runtime_available(&command.runtime, &package_manager)?;
                self.install_packages(&command.runtime, &command.packages, &package_manager)?;
            }
        }
        
        Ok(())
    }

    /// Handle Nix package specifications
    fn handle_nix_packages(&self, packages: &[String]) -> Result<(), String> {
        println!("🔧 Processing Nix packages: {:?}", packages);
        
        for package in packages {
            if let Ok(output) = Command::new("/bin/sh")
                .arg("-c")
                .arg(&format!("command -v {} || which {} || ls /bin/{} || ls /usr/bin/{}", package, package, package, package))
                .output() 
            {
                if output.status.success() {
                    println!("  ✓ Nix package '{}' is available", package);
                } else {
                    println!("  ⚠ Nix package '{}' not found in standard locations", package);
                    println!("    (This is normal for Nix packages - they may be available when needed)");
                }
            }
        }
        
        println!("✅ Nix packages processed");
        Ok(())
    }

    fn ensure_runtime_available(&mut self, runtime: &Runtime, package_manager: &str) -> Result<(), String> {
        let runtime_name = runtime.get_name();
        
        // Check if runtime is already installed
        if self.installed_runtimes.contains_key(&runtime_name) {
            return Ok(());
        }
        
        if package_manager == "nix" || package_manager == "none" {
            println!("  ℹ Runtime {} should be pre-available in this environment", runtime_name);
            self.installed_runtimes.insert(runtime_name, runtime.clone());
            return Ok(());
        }
        
        match runtime {
            Runtime::NodeJs => {
                self.install_nodejs_runtime(package_manager)?;
            }
            Runtime::Python => {
                self.install_python_runtime(package_manager)?;
            }
            Runtime::Ruby => {
                self.install_ruby_runtime(package_manager)?;
            }
            Runtime::Go => {
                self.install_go_runtime(package_manager)?;
            }
            Runtime::Java => {
                self.install_java_runtime(package_manager)?;
            }
            Runtime::Php => {
                self.install_php_runtime(package_manager)?;
            }
            Runtime::Nix => {
                println!("  ℹ Nix runtime is environment-based, no installation needed");
            }
            Runtime::Custom(_) => {
                return Err("Custom runtime installation not implemented".to_string());
            }
        }
        
        self.installed_runtimes.insert(runtime_name, runtime.clone());
        Ok(())
    }

    fn install_nodejs_runtime(&self, package_manager: &str) -> Result<(), String> {
        println!("Installing runtime NodeJs");
        let packages = match package_manager {
            "apk" => vec!["nodejs", "npm"],
            "apt" => vec!["nodejs", "npm"],
            "yum" | "dnf" => vec!["nodejs", "npm"],
            _ => return Err(format!("NodeJs installation not supported for package manager: {}", package_manager)),
        };
        
        self.system_runtime.install_runtime(package_manager, "NodeJs", &packages)
    }

    fn install_python_runtime(&self, package_manager: &str) -> Result<(), String> {
        println!("Installing runtime Python");
        let packages = match package_manager {
            "apk" => vec!["python3", "py3-pip"],
            "apt" => vec!["python3", "python3-pip"],
            "yum" | "dnf" => vec!["python3", "python3-pip"],
            _ => return Err(format!("Python installation not supported for package manager: {}", package_manager)),
        };
        
        self.system_runtime.install_runtime(package_manager, "Python", &packages)
    }

    fn install_ruby_runtime(&self, package_manager: &str) -> Result<(), String> {
        println!("Installing runtime Ruby");
        let packages = match package_manager {
            "apk" => vec!["ruby", "ruby-dev", "ruby-bundler"],
            "apt" => vec!["ruby", "ruby-dev", "bundler"],
            "yum" | "dnf" => vec!["ruby", "ruby-devel", "rubygems"],
            _ => return Err(format!("Ruby installation not supported for package manager: {}", package_manager)),
        };
        
        self.system_runtime.install_runtime(package_manager, "Ruby", &packages)
    }

    fn install_go_runtime(&self, package_manager: &str) -> Result<(), String> {
        println!("Installing runtime Go");
        let packages = match package_manager {
            "apk" => vec!["go"],
            "apt" => vec!["golang-go"],
            "yum" | "dnf" => vec!["golang"],
            _ => return Err(format!("Go installation not supported for package manager: {}", package_manager)),
        };
        
        self.system_runtime.install_runtime(package_manager, "Go", &packages)
    }

    fn install_java_runtime(&self, package_manager: &str) -> Result<(), String> {
        println!("Installing runtime Java");
        let packages = match package_manager {
            "apk" => vec!["openjdk11", "maven"],
            "apt" => vec!["openjdk-11-jdk", "maven"],
            "yum" | "dnf" => vec!["java-11-openjdk-devel", "maven"],
            _ => return Err(format!("Java installation not supported for package manager: {}", package_manager)),
        };
        
        self.system_runtime.install_runtime(package_manager, "Java", &packages)
    }

    fn install_php_runtime(&self, package_manager: &str) -> Result<(), String> {
        println!("Installing runtime PHP");
        let packages = match package_manager {
            "apk" => vec!["php", "php-composer", "php-json"],
            "apt" => vec!["php", "composer", "php-json"],
            "yum" | "dnf" => vec!["php", "composer", "php-json"],
            _ => return Err(format!("PHP installation not supported for package manager: {}", package_manager)),
        };
        
        self.system_runtime.install_runtime(package_manager, "PHP", &packages)
    }

    fn install_packages(&self, runtime: &Runtime, packages: &[String], package_manager: &str) -> Result<(), String> {
        match runtime {
            Runtime::NodeJs => self.install_npm_packages(packages),
            Runtime::Python => self.install_pip_packages(packages),
            Runtime::Ruby => self.install_gem_packages(packages),
            Runtime::Go => self.install_go_packages(packages),
            Runtime::Java => self.install_maven_packages(packages),
            Runtime::Php => self.install_composer_packages(packages),
            Runtime::Nix => {
                println!("  ℹ Nix packages are pre-installed in environment");
                Ok(())
            }
            Runtime::Custom(_) => {
                if package_manager != "none" {
                    let packages_str: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
                    self.system_runtime.install_runtime(package_manager, "custom", &packages_str)
                } else {
                    println!("  ℹ Custom packages cannot be installed - no package manager available");
                    Ok(())
                }
            }
        }
    }

    fn install_npm_packages(&self, packages: &[String]) -> Result<(), String> {
        if packages.is_empty() {
            return Ok(());
        }

        println!("📦 Installing npm packages: {}", packages.join(", "));
        
        let mut cmd = Command::new("npm");
        cmd.arg("install").arg("-g");
        cmd.args(packages);

        match cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    println!("✅ Successfully installed npm packages");
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if !stdout.trim().is_empty() {
                        println!("   npm output: {}", stdout.trim());
                    }
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!("Failed to install npm packages: {}", stderr))
                }
            }
            Err(e) => Err(format!("Failed to execute npm command: {}", e)),
        }
    }

    fn install_pip_packages(&self, packages: &[String]) -> Result<(), String> {
        if packages.is_empty() {
            return Ok(());
        }

        println!("📦 Installing pip packages: {}", packages.join(", "));
        
        let mut cmd = Command::new("pip3");
        cmd.arg("install");
        cmd.args(packages);

        match cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    println!("✅ Successfully installed pip packages");
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!("Failed to install pip packages: {}", stderr))
                }
            }
            Err(e) => Err(format!("Failed to execute pip3 command: {}", e)),
        }
    }

    fn install_gem_packages(&self, packages: &[String]) -> Result<(), String> {
        if packages.is_empty() {
            return Ok(());
        }

        println!("📦 Installing gem packages: {}", packages.join(", "));
        
        for package in packages {
            let mut cmd = Command::new("gem");
            cmd.arg("install").arg(package);

            match cmd.output() {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(format!("Failed to install gem package {}: {}", package, stderr));
                    }
                }
                Err(e) => return Err(format!("Failed to execute gem command for {}: {}", package, e)),
            }
        }
        
        println!("✅ Successfully installed gem packages");
        Ok(())
    }

    fn install_go_packages(&self, packages: &[String]) -> Result<(), String> {
        if packages.is_empty() {
            return Ok(());
        }

        println!("📦 Installing Go packages: {}", packages.join(", "));
        
        for package in packages {
            let mut cmd = Command::new("go");
            cmd.arg("install").arg(package);

            match cmd.output() {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return Err(format!("Failed to install Go package {}: {}", package, stderr));
                    }
                }
                Err(e) => return Err(format!("Failed to execute go command for {}: {}", package, e)),
            }
        }
        
        println!("✅ Successfully installed Go packages");
        Ok(())
    }

    fn install_maven_packages(&self, packages: &[String]) -> Result<(), String> {
        println!("📦 Java/Maven packages requested: {}", packages.join(", "));
        println!("ℹ️  Java packages typically managed through project files (pom.xml, build.gradle)");
        Ok(())
    }

    fn install_composer_packages(&self, packages: &[String]) -> Result<(), String> {
        if packages.is_empty() {
            return Ok(());
        }

        println!("📦 Installing Composer packages: {}", packages.join(", "));
        
        let mut cmd = Command::new("composer");
        cmd.arg("global").arg("require");
        cmd.args(packages);

        match cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    println!("✅ Successfully installed Composer packages");
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!("Failed to install Composer packages: {}", stderr))
                }
            }
            Err(e) => Err(format!("Failed to execute composer command: {}", e)),
        }
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