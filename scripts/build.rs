use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile protobuf files
    tonic_build::compile_protos("proto/quilt.proto")?;
    
    // Download and setup busybox for containers
    setup_busybox()?;
    
    Ok(())
}

fn setup_busybox() -> Result<(), Box<dyn std::error::Error>> {
    let busybox_dir = "src/daemon/resources";
    let busybox_path = format!("{}/busybox", busybox_dir);
    
    // Create resources directory if it doesn't exist
    fs::create_dir_all(busybox_dir)?;
    
    // Check if busybox already exists
    if Path::new(&busybox_path).exists() {
        println!("cargo:warning=Busybox already exists at {}", busybox_path);
        return Ok(());
    }
    
    // Download busybox static binary for x86_64
    println!("cargo:warning=Downloading busybox static binary...");
    
    let busybox_url = "https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox";
    
    // Use curl to download (available on most systems)
    let status = std::process::Command::new("curl")
        .args(&["-L", "-o", &busybox_path, busybox_url])
        .status()?;
    
    if !status.success() {
        // Try wget as fallback
        println!("cargo:warning=curl failed, trying wget...");
        let status = std::process::Command::new("wget")
            .args(&["-O", &busybox_path, busybox_url])
            .status()?;
        
        if !status.success() {
            return Err("Failed to download busybox with curl or wget".into());
        }
    }
    
    // Make busybox executable
    std::process::Command::new("chmod")
        .args(&["+x", &busybox_path])
        .status()?;
    
    println!("cargo:warning=Busybox downloaded successfully to {}", busybox_path);
    
    // Tell Cargo to re-run if busybox is deleted
    println!("cargo:rerun-if-changed={}", busybox_path);
    
    Ok(())
} 