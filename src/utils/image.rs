use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};
use std::fs;
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
}

#[derive(Debug)]
pub struct ImageLayerCache {
    layers: HashMap<String, ImageLayerInfo>,
    base_cache_dir: String,
}

impl ImageLayerCache {
    pub fn new() -> Self {
        Self {
            layers: HashMap::new(),
            base_cache_dir: "/tmp/quilt-image-cache".to_string(),
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

    /// Setup rootfs using overlay filesystem (efficient)
    fn setup_overlay_rootfs(container_id: &str, image_path: &str, rootfs_path: &str) -> Result<String, String> {
        let cache = Self::cache();
        let mut cache_guard = cache.lock()
            .map_err(|_| "Failed to lock image cache")?;
        
        // Get or create base layer
        let layer_hash = ImageLayerCache::get_layer_hash(image_path)?;
        let base_layer_path = format!("{}/layers/{}", cache_guard.base_cache_dir, layer_hash);
        
        // Check if base layer exists and is valid
        let needs_extraction = if let Some(layer_info) = cache_guard.layers.get(&layer_hash) {
            !FileSystemUtils::is_directory(&layer_info.layer_path)
        } else {
            true
        };
        
        if needs_extraction {
            ConsoleLogger::progress(&format!("Extracting base layer for image: {}", image_path));
            
            // Extract to base layer (this is the only time we extract)
            FileSystemUtils::create_dir_all_with_logging(&base_layer_path, "base layer")?;
            Self::extract_image_direct(image_path, &base_layer_path)?;
            
            // Record in cache
            let size = Self::calculate_directory_size(&base_layer_path)?;
            cache_guard.layers.insert(layer_hash.clone(), ImageLayerInfo {
                layer_path: base_layer_path.clone(),
                extracted_at: std::time::SystemTime::now(),
                reference_count: 1,
                size_bytes: size,
            });
            
            ConsoleLogger::success(&format!("Base layer cached: {} bytes", size));
        } else {
            // Increment reference count
            if let Some(layer_info) = cache_guard.layers.get_mut(&layer_hash) {
                layer_info.reference_count += 1;
                ConsoleLogger::debug(&format!("Reusing cached layer (refs: {})", layer_info.reference_count));
            }
        }
        
        drop(cache_guard); // Release lock early
        
        // Create overlay structure
        Self::create_overlay_mount(container_id, &base_layer_path, rootfs_path)
    }

    /// Create overlay mount for container
    fn create_overlay_mount(container_id: &str, base_layer: &str, rootfs_path: &str) -> Result<String, String> {
        let overlay_dir = format!("/tmp/quilt-image-cache/overlays/{}", container_id);
        
        // Create overlay directories
        let upper_dir = format!("{}/upper", overlay_dir);
        let work_dir = format!("{}/work", overlay_dir);
        
        FileSystemUtils::create_dir_all_with_logging(&upper_dir, "overlay upper")?;
        FileSystemUtils::create_dir_all_with_logging(&work_dir, "overlay work")?;
        FileSystemUtils::create_dir_all_with_logging(rootfs_path, "container rootfs")?;
        
        // Check if overlay is supported
        if !Self::is_overlay_supported()? {
            return Err("Overlay filesystem not supported".to_string());
        }
        
        // Create overlay mount
        let mount_cmd = format!(
            "mount -t overlay overlay -o lowerdir={},upperdir={},workdir={} {}",
            base_layer, upper_dir, work_dir, rootfs_path
        );
        
        ConsoleLogger::debug(&format!("Creating overlay mount: {}", mount_cmd));
        
        let result = CommandExecutor::execute_shell(&mount_cmd)?;
        if !result.success {
            return Err(format!("Failed to create overlay mount: {}", result.stderr));
        }
        
        ConsoleLogger::success(&format!("Overlay mounted for container {}", container_id));
        Ok(rootfs_path.to_string())
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
        let tar_file = std::fs::File::open(image_path)
            .map_err(|e| format!("Failed to open image file: {}", e))?;

        let tar = GzDecoder::new(tar_file);
        let mut archive = Archive::new(tar);

        archive.unpack(dest_path)
            .map_err(|e| format!("Failed to extract image: {}", e))?;

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

    /// Cleanup container overlay resources
    pub fn cleanup_container_image(container_id: &str) -> Result<(), String> {
        let rootfs_path = format!("/tmp/quilt-containers/{}", container_id);
        let overlay_dir = format!("/tmp/quilt-image-cache/overlays/{}", container_id);
        
        // Unmount overlay if it exists
        let unmount_cmd = format!("umount {}", rootfs_path);
        let _unmount_result = CommandExecutor::execute_shell(&unmount_cmd); // Don't fail if already unmounted
        
        // Remove overlay directories
        let _cleanup1 = FileSystemUtils::remove_path(&overlay_dir);
        let _cleanup2 = FileSystemUtils::remove_path(&rootfs_path);
        
        // Decrement reference count in cache
        let cache = Self::cache();
        if let Ok(mut cache_guard) = cache.lock() {
            // Find and decrement reference count
            let mut to_remove = Vec::new();
            for (hash, layer_info) in cache_guard.layers.iter_mut() {
                if layer_info.reference_count > 0 {
                    layer_info.reference_count -= 1;
                    if layer_info.reference_count == 0 {
                        to_remove.push(hash.clone());
                    }
                }
            }
            
            // Remove unused layers
            for hash in to_remove {
                if let Some(layer_info) = cache_guard.layers.remove(&hash) {
                    let _cleanup = FileSystemUtils::remove_path(&layer_info.layer_path);
                    ConsoleLogger::debug(&format!("Removed unused layer: {}", hash));
                }
            }
        }
        
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