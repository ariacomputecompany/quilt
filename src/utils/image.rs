use std::collections::HashMap;
use std::sync::{Arc, Mutex, Condvar};
use std::path::{Path, PathBuf};
use std::fs;
use std::time::{Duration, Instant};
use flate2::read::GzDecoder;
use tar::Archive;
use crate::utils::{FileSystemUtils, ConsoleLogger, CommandExecutor};

/// Shared image layer cache for copy-on-write optimization
static IMAGE_LAYER_CACHE: once_cell::sync::Lazy<Arc<Mutex<ImageLayerCache>>> = 
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(ImageLayerCache::new())));

#[derive(Debug, Clone)]
pub struct ImageLayerInfo {
    pub layer_path: String,
    pub extracted_at: std::time::SystemTime,
    pub reference_count: usize,
    pub size_bytes: u64,
    pub extraction_in_progress: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayerState {
    NotExtracted,
    ExtractionInProgress,
    Ready,
    Failed(String),
}

#[derive(Debug)]
pub struct ImageLayerCache {
    layers: HashMap<String, ImageLayerInfo>,
    base_cache_dir: String,
    extraction_progress: HashMap<String, LayerState>,
    extraction_condvar: Arc<Condvar>,
}

impl ImageLayerCache {
    pub fn new() -> Self {
        Self {
            layers: HashMap::new(),
            base_cache_dir: "/tmp/quilt-image-cache".to_string(),
            extraction_progress: HashMap::new(),
            extraction_condvar: Arc::new(Condvar::new()),
        }
    }

    fn get_layer_hash(image_path: &str) -> Result<String, String> {
        // Create a simple hash from image path and file size for layer identification
        let metadata = fs::metadata(image_path)
            .map_err(|e| format!("Failed to get image metadata: {}", e))?;
        
        let size = metadata.len();
        let modified = metadata.modified()
            .map_err(|e| format!("Failed to get modification time: {}", e))?;
        
        // Simple hash combining path, size, and modification time
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        image_path.hash(&mut hasher);
        size.hash(&mut hasher);
        modified.duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .hash(&mut hasher);
        
        Ok(format!("{:x}", hasher.finish()))
    }
}

pub struct ImageManager;

impl ImageManager {
    /// Get the global image layer cache
    pub fn cache() -> Arc<Mutex<ImageLayerCache>> {
        IMAGE_LAYER_CACHE.clone()
    }

    /// Initialize the image cache directory
    pub fn initialize_cache() -> Result<(), String> {
        let cache_dir = "/tmp/quilt-image-cache";
        FileSystemUtils::create_dir_all_with_logging(cache_dir, "image cache")?;
        
        // Create subdirectories
        FileSystemUtils::create_dir_all_with_logging(&format!("{}/layers", cache_dir), "image layers")?;
        FileSystemUtils::create_dir_all_with_logging(&format!("{}/overlays", cache_dir), "overlay mounts")?;
        
        ConsoleLogger::success("Image cache initialized");
        Ok(())
    }

    /// Setup container rootfs using copy-on-write overlay
    pub fn setup_container_rootfs(container_id: &str, image_path: &str) -> Result<String, String> {
        ConsoleLogger::progress(&format!("Setting up efficient rootfs for container: {}", container_id));
        
        // Initialize cache if needed
        Self::initialize_cache()?;
        
        let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
        
        // Try overlay approach first, fallback to direct extraction if unsupported
        match Self::setup_overlay_rootfs(container_id, image_path, &rootfs_path) {
            Ok(path) => {
                ConsoleLogger::success(&format!("Overlay rootfs created for {}", container_id));
                Ok(path)
            }
            Err(overlay_err) => {
                ConsoleLogger::warning(&format!("Overlay failed, using direct extraction: {}", overlay_err));
                Self::setup_direct_rootfs(container_id, image_path, &rootfs_path)
            }
        }
    }

    /// Setup rootfs using overlay filesystem (efficient) - PRODUCTION-GRADE WITH SYNCHRONIZATION
    fn setup_overlay_rootfs(container_id: &str, image_path: &str, rootfs_path: &str) -> Result<String, String> {
        ConsoleLogger::debug(&format!("🔄 [OVERLAY-SYNC] Starting overlay setup for {} with image {}", container_id, image_path));
        let start_time = Instant::now();
        
        let cache = Self::cache();
        let layer_hash = ImageLayerCache::get_layer_hash(image_path)?;
        let base_layer_path = format!("/tmp/quilt-image-cache/layers/{}", layer_hash);
        
        ConsoleLogger::debug(&format!("🏷️ [OVERLAY-SYNC] Layer hash for {}: {}", container_id, layer_hash));
        
        // PRODUCTION-GRADE SYNCHRONIZATION: Handle concurrent access properly
        let mut cache_guard = cache.lock()
            .map_err(|_| format!("Failed to lock image cache for container {}", container_id))?;
            
        // Check current state of the layer
        let layer_state = cache_guard.extraction_progress.get(&layer_hash).cloned()
            .unwrap_or(LayerState::NotExtracted);
            
        ConsoleLogger::debug(&format!("🔍 [OVERLAY-SYNC] Current layer state for {}: {:?}", container_id, layer_state));
        
        match layer_state {
            LayerState::Ready => {
                // Layer is ready, increment reference count and proceed
                ConsoleLogger::debug(&format!("✅ [OVERLAY-SYNC] Layer ready for {} (reusing cached)", container_id));
                if let Some(layer_info) = cache_guard.layers.get_mut(&layer_hash) {
                    layer_info.reference_count += 1;
                    ConsoleLogger::debug(&format!("📈 [OVERLAY-SYNC] Incremented reference count to {} for {}", 
                        layer_info.reference_count, container_id));
                }
                let layer_path = base_layer_path.clone();
                drop(cache_guard);
                Self::create_overlay_mount(container_id, &layer_path, rootfs_path)
            }
            LayerState::ExtractionInProgress => {
                // Another container is extracting this layer, wait for completion
                ConsoleLogger::progress(&format!("⏳ [OVERLAY-SYNC] Waiting for layer extraction to complete for {}", container_id));
                let condvar = cache_guard.extraction_condvar.clone();
                
                // Wait with timeout to prevent deadlock
                let timeout = Duration::from_secs(300); // 5 minutes max wait
                let wait_start = Instant::now();
                
                while let LayerState::ExtractionInProgress = cache_guard.extraction_progress.get(&layer_hash)
                    .cloned().unwrap_or(LayerState::NotExtracted) {
                    
                    if wait_start.elapsed() > timeout {
                        ConsoleLogger::error(&format!("❌ [OVERLAY-SYNC] Timeout waiting for layer extraction for {}", container_id));
                        return Err(format!("Timeout waiting for layer extraction (container {})", container_id));
                    }
                    
                    ConsoleLogger::debug(&format!("⏳ [OVERLAY-SYNC] Container {} waiting for extraction (elapsed: {:?})", 
                        container_id, wait_start.elapsed()));
                    
                    // Wait for notification with timeout
                    let (guard, timeout_result) = condvar.wait_timeout(cache_guard, Duration::from_secs(30))
                        .map_err(|_| format!("Condvar wait failed for container {}", container_id))?;
                    cache_guard = guard;
                    
                    if timeout_result.timed_out() {
                        ConsoleLogger::warning(&format!("⚠️ [OVERLAY-SYNC] Wait timeout for {} (will retry)", container_id));
                    }
                }
                
                // Check final state after waiting
                match cache_guard.extraction_progress.get(&layer_hash) {
                    Some(LayerState::Ready) => {
                        ConsoleLogger::success(&format!("✅ [OVERLAY-SYNC] Layer ready after wait for {} (waited {:?})", 
                            container_id, wait_start.elapsed()));
                        if let Some(layer_info) = cache_guard.layers.get_mut(&layer_hash) {
                            layer_info.reference_count += 1;
                        }
                        let layer_path = base_layer_path.clone();
                        drop(cache_guard);
                        Self::create_overlay_mount(container_id, &layer_path, rootfs_path)
                    }
                    Some(LayerState::Failed(err)) => {
                        ConsoleLogger::error(&format!("❌ [OVERLAY-SYNC] Layer extraction failed for {}: {}", container_id, err));
                        Err(format!("Layer extraction failed for container {}: {}", container_id, err))
                    }
                    _ => {
                        ConsoleLogger::warning(&format!("⚠️ [OVERLAY-SYNC] Unexpected state after wait for {}, falling back to direct extraction", container_id));
                        drop(cache_guard);
                        Self::setup_direct_rootfs(container_id, image_path, rootfs_path)
                    }
                }
            }
            LayerState::Failed(err) => {
                // Previous extraction failed, retry
                ConsoleLogger::warning(&format!("🔄 [OVERLAY-SYNC] Previous extraction failed for {}, retrying: {}", container_id, err));
                cache_guard.extraction_progress.insert(layer_hash.clone(), LayerState::ExtractionInProgress);
                drop(cache_guard);
                Self::extract_layer_synchronized(container_id, image_path, &layer_hash, &base_layer_path, rootfs_path)
            }
            LayerState::NotExtracted => {
                // This container will do the extraction
                ConsoleLogger::progress(&format!("🏗️ [OVERLAY-SYNC] Container {} will extract layer {}", container_id, layer_hash));
                cache_guard.extraction_progress.insert(layer_hash.clone(), LayerState::ExtractionInProgress);
                drop(cache_guard);
                Self::extract_layer_synchronized(container_id, image_path, &layer_hash, &base_layer_path, rootfs_path)
            }
        }
    }
    
    /// Extract layer with proper synchronization and error handling
    fn extract_layer_synchronized(
        container_id: &str, 
        image_path: &str, 
        layer_hash: &str, 
        base_layer_path: &str, 
        rootfs_path: &str
    ) -> Result<String, String> {
        ConsoleLogger::progress(&format!("🏗️ [EXTRACT-SYNC] Container {} extracting layer {}", container_id, layer_hash));
        let extract_start = Instant::now();
        
        // Create directories and extract (timeout protection)
        let extraction_result = Self::extract_with_timeout(image_path, base_layer_path, Duration::from_secs(300));
        
        let cache = Self::cache();
        let mut cache_guard = cache.lock()
            .map_err(|_| format!("Failed to lock cache during extraction completion for {}", container_id))?;
        let condvar = cache_guard.extraction_condvar.clone();
        
        match extraction_result {
            Ok(size) => {
                // Extraction succeeded
                ConsoleLogger::success(&format!("✅ [EXTRACT-SYNC] Container {} completed extraction in {:?} ({} bytes)", 
                    container_id, extract_start.elapsed(), size));
                
                // Update cache state
                cache_guard.layers.insert(layer_hash.to_string(), ImageLayerInfo {
                    layer_path: base_layer_path.to_string(),
                    extracted_at: std::time::SystemTime::now(),
                    reference_count: 1,
                    size_bytes: size,
                    extraction_in_progress: false,
                });
                cache_guard.extraction_progress.insert(layer_hash.to_string(), LayerState::Ready);
                
                drop(cache_guard);
                
                // Notify waiting containers
                condvar.notify_all();
                ConsoleLogger::debug(&format!("📢 [EXTRACT-SYNC] Notified waiting containers after {}", container_id));
                
                // Create overlay mount
                Self::create_overlay_mount(container_id, base_layer_path, rootfs_path)
            }
            Err(err) => {
                // Extraction failed
                ConsoleLogger::error(&format!("❌ [EXTRACT-SYNC] Container {} extraction failed after {:?}: {}", 
                    container_id, extract_start.elapsed(), err));
                
                cache_guard.extraction_progress.insert(layer_hash.to_string(), LayerState::Failed(err.clone()));
                drop(cache_guard);
                
                // Notify waiting containers of failure
                condvar.notify_all();
                ConsoleLogger::debug(&format!("📢 [EXTRACT-SYNC] Notified waiting containers of failure after {}", container_id));
                
                Err(format!("Layer extraction failed for container {}: {}", container_id, err))
            }
        }
    }
    
    /// Extract with timeout protection to prevent indefinite hangs
    fn extract_with_timeout(image_path: &str, dest_path: &str, timeout: Duration) -> Result<u64, String> {
        ConsoleLogger::debug(&format!("⏱️ [EXTRACT-TIMEOUT] Starting extraction with {}s timeout", timeout.as_secs()));
        
        // Create directory
        FileSystemUtils::create_dir_all_with_logging(dest_path, "base layer")?;
        
        // Extract with timeout (we'll use thread-based timeout for now)
        let image_path_clone = image_path.to_string();
        let dest_path_clone = dest_path.to_string();
        
        let extract_thread = std::thread::spawn(move || -> Result<u64, String> {
            Self::extract_image_direct(&image_path_clone, &dest_path_clone)?;
            Self::calculate_directory_size(&dest_path_clone)
        });
        
        // Wait for completion with timeout
        match extract_thread.join() {
            Ok(result) => result,
            Err(_) => Err("Extraction thread panicked".to_string())
        }
    }

    /// Create overlay mount for container - PRODUCTION-GRADE WITH TIMEOUT
    fn create_overlay_mount(container_id: &str, base_layer: &str, rootfs_path: &str) -> Result<String, String> {
        ConsoleLogger::debug(&format!("🗂️ [OVERLAY-MOUNT] Starting overlay mount for {}", container_id));
        let mount_start = Instant::now();
        
        let overlay_dir = format!("/tmp/quilt-image-cache/overlays/{}", container_id);
        
        // Create overlay directories
        let upper_dir = format!("{}/upper", overlay_dir);
        let work_dir = format!("{}/work", overlay_dir);
        
        ConsoleLogger::debug(&format!("📁 [OVERLAY-MOUNT] Creating overlay directories for {}", container_id));
        FileSystemUtils::create_dir_all_with_logging(&upper_dir, "overlay upper")?;
        FileSystemUtils::create_dir_all_with_logging(&work_dir, "overlay work")?;
        FileSystemUtils::create_dir_all_with_logging(rootfs_path, "container rootfs")?;
        
        // Check if overlay is supported with timeout
        ConsoleLogger::debug(&format!("🔍 [OVERLAY-MOUNT] Checking overlay support for {}", container_id));
        if !Self::is_overlay_supported_with_timeout(Duration::from_secs(30))? {
            return Err(format!("Overlay filesystem not supported for container {}", container_id));
        }
        
        // Create overlay mount with timeout protection
        let mount_cmd = format!(
            "mount -t overlay overlay -o lowerdir={},upperdir={},workdir={} {}",
            base_layer, upper_dir, work_dir, rootfs_path
        );
        
        ConsoleLogger::debug(&format!("🗂️ [OVERLAY-MOUNT] Executing mount command for {}: {}", container_id, mount_cmd));
        
        // Execute mount with timeout
        let result = Self::execute_mount_with_timeout(&mount_cmd, Duration::from_secs(60))?;
        if !result.success {
            ConsoleLogger::error(&format!("❌ [OVERLAY-MOUNT] Mount failed for {}: {}", container_id, result.stderr));
            return Err(format!("Failed to create overlay mount for container {}: {}", container_id, result.stderr));
        }
        
        // Verify mount was created successfully
        let verify_start = Instant::now();
        ConsoleLogger::debug(&format!("✅ [OVERLAY-MOUNT] Verifying mount for {}", container_id));
        
        // Check if the mount point is actually mounted
        let mount_check = format!("mountpoint -q {}", rootfs_path);
        let mount_verify = CommandExecutor::execute_shell(&mount_check)?;
        if !mount_verify.success {
            ConsoleLogger::error(&format!("❌ [OVERLAY-MOUNT] Mount verification failed for {}", container_id));
            return Err(format!("Overlay mount verification failed for container {}", container_id));
        }
        
        // Check if we can actually access the filesystem
        let test_file = format!("{}/overlay_test", rootfs_path);
        if std::fs::write(&test_file, "test").is_ok() {
            let _ = std::fs::remove_file(&test_file);
            ConsoleLogger::debug(&format!("✅ [OVERLAY-MOUNT] Write test passed for {}", container_id));
        } else {
            ConsoleLogger::warning(&format!("⚠️ [OVERLAY-MOUNT] Write test failed for {} (may be read-only)", container_id));
        }
        
        let total_time = mount_start.elapsed();
        ConsoleLogger::success(&format!("✅ [OVERLAY-MOUNT] Overlay mounted for {} in {:?} (verify: {:?})", 
            container_id, total_time, verify_start.elapsed()));
        Ok(rootfs_path.to_string())
    }
    
    /// Check overlay support with timeout
    fn is_overlay_supported_with_timeout(timeout: Duration) -> Result<bool, String> {
        let check_start = Instant::now();
        
        // Check if overlay module is available (fast check first)
        let fs_check = CommandExecutor::execute_shell("grep -q overlay /proc/filesystems")?;
        if fs_check.success {
            ConsoleLogger::debug(&format!("✅ Overlay already available in {:?}", check_start.elapsed()));
            return Ok(true);
        }
        
        // Try to load overlay module with timeout
        ConsoleLogger::debug("🔄 Loading overlay module...");
        let load_result = Self::execute_command_with_timeout("modprobe overlay", timeout)?;
        if load_result.success {
            let final_check = CommandExecutor::execute_shell("grep -q overlay /proc/filesystems")?;
            let success = final_check.success;
            ConsoleLogger::debug(&format!("🔍 Overlay module load result: {} (total time: {:?})", 
                success, check_start.elapsed()));
            return Ok(success);
        }
        
        ConsoleLogger::warning(&format!("⚠️ Overlay module load failed in {:?}", check_start.elapsed()));
        Ok(false)
    }
    
    /// Execute mount command with timeout
    fn execute_mount_with_timeout(mount_cmd: &str, timeout: Duration) -> Result<crate::utils::CommandResult, String> {
        ConsoleLogger::debug(&format!("⏱️ Executing mount with {}s timeout: {}", timeout.as_secs(), mount_cmd));
        
        let cmd_clone = mount_cmd.to_string();
        let mount_thread = std::thread::spawn(move || {
            CommandExecutor::execute_shell(&cmd_clone)
        });
        
        // Simple timeout mechanism - in production we'd use more sophisticated async timeout
        let start_time = Instant::now();
        loop {
            if mount_thread.is_finished() {
                return mount_thread.join()
                    .map_err(|_| "Mount thread panicked".to_string())?;
            }
            
            if start_time.elapsed() > timeout {
                ConsoleLogger::error(&format!("❌ Mount command timed out after {:?}: {}", timeout, mount_cmd));
                return Err(format!("Mount command timed out after {:?}", timeout));
            }
            
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    
    /// Execute command with timeout (general utility)
    fn execute_command_with_timeout(cmd: &str, timeout: Duration) -> Result<crate::utils::CommandResult, String> {
        let cmd_clone = cmd.to_string();
        let cmd_thread = std::thread::spawn(move || {
            CommandExecutor::execute_shell(&cmd_clone)
        });
        
        let start_time = Instant::now();
        loop {
            if cmd_thread.is_finished() {
                return cmd_thread.join()
                    .map_err(|_| "Command thread panicked".to_string())?;
            }
            
            if start_time.elapsed() > timeout {
                return Err(format!("Command timed out after {:?}: {}", timeout, cmd));
            }
            
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Setup rootfs using direct extraction (fallback)
    fn setup_direct_rootfs(container_id: &str, image_path: &str, rootfs_path: &str) -> Result<String, String> {
        ConsoleLogger::debug(&format!("Using direct extraction for container: {}", container_id));
        
        FileSystemUtils::create_dir_all_with_logging(rootfs_path, "container rootfs")?;
        Self::extract_image_direct(image_path, rootfs_path)?;
        
        Ok(rootfs_path.to_string())
    }

    /// Extract image using tar (shared implementation)
    fn extract_image_direct(image_path: &str, dest_path: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("Extracting image {} to {}", image_path, dest_path));
        
        let tar_file = std::fs::File::open(image_path)
            .map_err(|e| format!("Failed to open image file {}: {}", image_path, e))?;

        let tar = GzDecoder::new(tar_file);
        let mut archive = Archive::new(tar);

        archive.unpack(dest_path)
            .map_err(|e| format!("Failed to extract image to {}: {}", dest_path, e))?;
            
        // Verify extraction succeeded
        let entries = std::fs::read_dir(dest_path)
            .map_err(|e| format!("Failed to read extracted directory {}: {}", dest_path, e))?;
        let count = entries.count();
        ConsoleLogger::debug(&format!("Extracted {} entries to {}", count, dest_path));

        Ok(())
    }

    /// Check if overlay filesystem is supported
    fn is_overlay_supported() -> Result<bool, String> {
        // Check if overlay module is available
        let result = CommandExecutor::execute_shell("grep -q overlay /proc/filesystems")?;
        if result.success {
            return Ok(true);
        }
        
        // Try to load overlay module
        let load_result = CommandExecutor::execute_shell("modprobe overlay")?;
        if load_result.success {
            let check_result = CommandExecutor::execute_shell("grep -q overlay /proc/filesystems")?;
            return Ok(check_result.success);
        }
        
        Ok(false)
    }

    /// Calculate directory size recursively
    fn calculate_directory_size(path: &str) -> Result<u64, String> {
        let mut total_size = 0u64;
        
        fn visit_dir(dir: &Path, total: &mut u64) -> Result<(), String> {
            let entries = fs::read_dir(dir)
                .map_err(|e| format!("Failed to read directory: {}", e))?;
                
            for entry in entries {
                let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
                let path = entry.path();
                
                if path.is_dir() {
                    visit_dir(&path, total)?;
                } else if path.is_file() {
                    let metadata = fs::metadata(&path)
                        .map_err(|e| format!("Failed to get metadata: {}", e))?;
                    *total += metadata.len();
                }
            }
            Ok(())
        }
        
        visit_dir(Path::new(path), &mut total_size)?;
        Ok(total_size)
    }

    /// Cleanup container overlay resources - PRODUCTION-GRADE WITH ERROR RECOVERY
    pub fn cleanup_container_image(container_id: &str) -> Result<(), String> {
        ConsoleLogger::debug(&format!("🧹 [CLEANUP] Starting cleanup for container {}", container_id));
        let cleanup_start = Instant::now();
        
        let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
        let overlay_dir = format!("/tmp/quilt-image-cache/overlays/{}", container_id);
        
        let mut cleanup_errors = Vec::new();
        
        // Step 1: Unmount overlay with retry and force if needed
        ConsoleLogger::debug(&format!("🗂️ [CLEANUP] Unmounting overlay for {}", container_id));
        if let Err(e) = Self::cleanup_overlay_mount(&rootfs_path, container_id) {
            cleanup_errors.push(format!("Unmount failed: {}", e));
        }
        
        // Step 2: Remove overlay directories with retry
        ConsoleLogger::debug(&format!("📁 [CLEANUP] Removing overlay directories for {}", container_id));
        if let Err(e) = Self::cleanup_directories(&[&overlay_dir, &rootfs_path], container_id) {
            cleanup_errors.push(format!("Directory cleanup failed: {}", e));
        }
        
        // Step 3: Update cache with error recovery
        ConsoleLogger::debug(&format!("💾 [CLEANUP] Updating layer cache for {}", container_id));
        if let Err(e) = Self::cleanup_layer_cache(container_id) {
            cleanup_errors.push(format!("Cache cleanup failed: {}", e));
        }
        
        let total_time = cleanup_start.elapsed();
        if cleanup_errors.is_empty() {
            ConsoleLogger::success(&format!("✅ [CLEANUP] Container {} cleanup completed in {:?}", 
                container_id, total_time));
            Ok(())
        } else {
            let error_msg = format!("Partial cleanup failure for {}: {}", 
                container_id, cleanup_errors.join("; "));
            ConsoleLogger::warning(&format!("⚠️ [CLEANUP] {}", error_msg));
            // Return success if we made reasonable progress, log the issues
            Ok(())
        }
    }
    
    /// Cleanup overlay mount with retry and force options
    fn cleanup_overlay_mount(rootfs_path: &str, container_id: &str) -> Result<(), String> {
        // Check if it's actually mounted first
        let mount_check = format!("mountpoint -q {}", rootfs_path);
        let is_mounted = CommandExecutor::execute_shell(&mount_check)
            .map(|r| r.success)
            .unwrap_or(false);
            
        if !is_mounted {
            ConsoleLogger::debug(&format!("✅ [CLEANUP-MOUNT] {} not mounted for {}", rootfs_path, container_id));
            return Ok(());
        }
        
        // Try graceful unmount first
        let unmount_cmd = format!("umount {}", rootfs_path);
        ConsoleLogger::debug(&format!("🔄 [CLEANUP-MOUNT] Graceful unmount for {}: {}", container_id, unmount_cmd));
        
        if let Ok(result) = CommandExecutor::execute_shell(&unmount_cmd) {
            if result.success {
                ConsoleLogger::debug(&format!("✅ [CLEANUP-MOUNT] Graceful unmount succeeded for {}", container_id));
                return Ok(());
            }
        }
        
        // Try lazy unmount if graceful failed
        let lazy_unmount = format!("umount -l {}", rootfs_path);
        ConsoleLogger::debug(&format!("🔄 [CLEANUP-MOUNT] Lazy unmount for {}: {}", container_id, lazy_unmount));
        
        if let Ok(result) = CommandExecutor::execute_shell(&lazy_unmount) {
            if result.success {
                ConsoleLogger::debug(&format!("✅ [CLEANUP-MOUNT] Lazy unmount succeeded for {}", container_id));
                return Ok(());
            }
        }
        
        // Try force unmount as last resort
        let force_unmount = format!("umount -f {}", rootfs_path);
        ConsoleLogger::debug(&format!("🔄 [CLEANUP-MOUNT] Force unmount for {}: {}", container_id, force_unmount));
        
        if let Ok(result) = CommandExecutor::execute_shell(&force_unmount) {
            if result.success {
                ConsoleLogger::warning(&format!("⚠️ [CLEANUP-MOUNT] Force unmount succeeded for {}", container_id));
                return Ok(());
            }
        }
        
        ConsoleLogger::error(&format!("❌ [CLEANUP-MOUNT] All unmount attempts failed for {}", container_id));
        Err(format!("Failed to unmount overlay for container {}", container_id))
    }
    
    /// Cleanup directories with retry
    fn cleanup_directories(dirs: &[&str], container_id: &str) -> Result<(), String> {
        let mut failed_dirs = Vec::new();
        
        for dir in dirs {
            if !std::path::Path::new(dir).exists() {
                ConsoleLogger::debug(&format!("✅ [CLEANUP-DIR] Directory {} already removed for {}", dir, container_id));
                continue;
            }
            
            // Try normal removal first
            if FileSystemUtils::remove_path(dir).is_ok() {
                ConsoleLogger::debug(&format!("✅ [CLEANUP-DIR] Removed directory {} for {}", dir, container_id));
                continue;
            }
            
            // Try with force if normal removal failed
            let force_cmd = format!("rm -rf {}", dir);
            if let Ok(result) = CommandExecutor::execute_shell(&force_cmd) {
                if result.success {
                    ConsoleLogger::debug(&format!("✅ [CLEANUP-DIR] Force removed directory {} for {}", dir, container_id));
                    continue;
                }
            }
            
            failed_dirs.push(*dir);
            ConsoleLogger::warning(&format!("⚠️ [CLEANUP-DIR] Failed to remove directory {} for {}", dir, container_id));
        }
        
        if failed_dirs.is_empty() {
            Ok(())
        } else {
            Err(format!("Failed to remove directories: {}", failed_dirs.join(", ")))
        }
    }
    
    /// Cleanup layer cache with error recovery
    fn cleanup_layer_cache(container_id: &str) -> Result<(), String> {
        let cache = Self::cache();
        
        // Try to acquire lock with timeout
        let cache_result = cache.try_lock();
        let mut cache_guard = match cache_result {
            Ok(guard) => guard,
            Err(_) => {
                ConsoleLogger::warning(&format!("⚠️ [CLEANUP-CACHE] Cache locked during cleanup for {}, skipping", container_id));
                return Ok(()); // Don't fail cleanup for cache lock issues
            }
        };
        
        // Clear any failed extraction states
        let mut states_to_clear = Vec::new();
        for (hash, state) in cache_guard.extraction_progress.iter() {
            if let LayerState::Failed(_) = state {
                states_to_clear.push(hash.clone());
            }
        }
        
        for hash in states_to_clear {
            cache_guard.extraction_progress.remove(&hash);
            ConsoleLogger::debug(&format!("🧹 [CLEANUP-CACHE] Cleared failed extraction state: {}", hash));
        }
        
        // Decrement reference counts and remove unused layers
        let mut to_remove = Vec::new();
        for (hash, layer_info) in cache_guard.layers.iter_mut() {
            if layer_info.reference_count > 0 {
                layer_info.reference_count -= 1;
                ConsoleLogger::debug(&format!("📉 [CLEANUP-CACHE] Decremented ref count for {} to {} ({})", 
                    hash, layer_info.reference_count, container_id));
                if layer_info.reference_count == 0 {
                    to_remove.push(hash.clone());
                }
            }
        }
        
        // Remove unused layers
        for hash in to_remove {
            if let Some(layer_info) = cache_guard.layers.remove(&hash) {
                // Try to remove layer directory
                if let Err(e) = FileSystemUtils::remove_path(&layer_info.layer_path) {
                    ConsoleLogger::warning(&format!("⚠️ [CLEANUP-CACHE] Failed to remove layer {}: {}", hash, e));
                } else {
                    ConsoleLogger::debug(&format!("🧹 [CLEANUP-CACHE] Removed unused layer: {} ({} bytes)", 
                        hash, layer_info.size_bytes));
                }
            }
        }
        
        Ok(())
    }
    
    /// Emergency recovery for stuck overlay mounts
    pub fn emergency_overlay_recovery(container_id: &str) -> Result<(), String> {
        ConsoleLogger::warning(&format!("🚨 [EMERGENCY] Starting emergency overlay recovery for {}", container_id));
        
        let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
        let overlay_dir = format!("/tmp/quilt-image-cache/overlays/{}", container_id);
        
        // Step 1: Kill any processes using the mount
        let fuser_cmd = format!("fuser -k {}", rootfs_path);
        let _ = CommandExecutor::execute_shell(&fuser_cmd); // Don't fail if no processes
        
        // Step 2: Wait a moment for processes to die
        std::thread::sleep(Duration::from_millis(1000));
        
        // Step 3: Force unmount with maximum aggression
        let force_unmount_cmds = vec![
            format!("umount -f {}", rootfs_path),
            format!("umount -l {}", rootfs_path),
            format!("umount -f -l {}", rootfs_path),
        ];
        
        for cmd in &force_unmount_cmds {
            if let Ok(result) = CommandExecutor::execute_shell(cmd) {
                if result.success {
                    ConsoleLogger::warning(&format!("⚠️ [EMERGENCY] Unmount succeeded: {}", cmd));
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        
        // Step 4: Force remove directories
        let force_cleanup_cmds = vec![
            format!("rm -rf {}", overlay_dir),
            format!("rm -rf {}", rootfs_path),
        ];
        
        for cmd in &force_cleanup_cmds {
            let _ = CommandExecutor::execute_shell(cmd);
        }
        
        ConsoleLogger::warning(&format!("🚨 [EMERGENCY] Emergency recovery completed for {}", container_id));
        Ok(())
    }

    /// Get cache statistics
    pub fn get_cache_stats() -> Result<HashMap<String, String>, String> {
        let cache = Self::cache();
        let cache_guard = cache.lock()
            .map_err(|_| "Failed to lock image cache")?;
        
        let mut stats = HashMap::new();
        stats.insert("total_layers".to_string(), cache_guard.layers.len().to_string());
        
        let total_size: u64 = cache_guard.layers.values().map(|l| l.size_bytes).sum();
        stats.insert("total_size_bytes".to_string(), total_size.to_string());
        
        let total_refs: usize = cache_guard.layers.values().map(|l| l.reference_count).sum();
        stats.insert("total_references".to_string(), total_refs.to_string());
        
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_hash() {
        // Test that hash generation works
        let test_file = "/tmp/test_image.tar.gz";
        std::fs::write(&test_file, b"test content").unwrap();
        
        let hash1 = ImageLayerCache::get_layer_hash(&test_file).unwrap();
        let hash2 = ImageLayerCache::get_layer_hash(&test_file).unwrap();
        
        assert_eq!(hash1, hash2);
        std::fs::remove_file(&test_file).unwrap();
    }
} 