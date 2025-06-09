use std::collections::HashMap;
use std::sync::{Arc, RwLock, Mutex};
use std::hash::Hash;
use dashmap::DashMap;
use parking_lot::{RwLock as ParkingRwLock, Mutex as ParkingMutex};
use crate::utils::ConsoleLogger;

/// High-performance concurrent container registry using DashMap
pub struct ConcurrentContainerRegistry<T> {
    containers: DashMap<String, T>,
    operation_locks: DashMap<String, Arc<ParkingMutex<()>>>,
}

impl<T> ConcurrentContainerRegistry<T> 
where 
    T: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            containers: DashMap::new(),
            operation_locks: DashMap::new(),
        }
    }

    /// Insert a container (lock-free for independent containers)
    pub fn insert(&self, container_id: String, container: T) {
        ConsoleLogger::debug(&format!("[REGISTRY] Lock-free insert: {}", container_id));
        self.containers.insert(container_id, container);
    }

    /// Get a container (lock-free read)
    pub fn get(&self, container_id: &str) -> Option<T> {
        ConsoleLogger::debug(&format!("[REGISTRY] Lock-free read: {}", container_id));
        self.containers.get(container_id).map(|entry| entry.value().clone())
    }

    /// Update a container with a closure (per-container lock)
    pub fn update<F, R>(&self, container_id: &str, updater: F) -> Option<R>
    where
        F: FnOnce(&mut T) -> R,
    {
        ConsoleLogger::debug(&format!("[REGISTRY] Per-container update: {}", container_id));
        
        // Get or create operation lock for this specific container
        let operation_lock = self.operation_locks
            .entry(container_id.to_string())
            .or_insert_with(|| Arc::new(ParkingMutex::new(())))
            .clone();
        
        // Lock only this container's operations
        let _guard = operation_lock.lock();
        
        // Update the container
        self.containers.get_mut(container_id).map(|mut entry| {
            updater(entry.value_mut())
        })
    }

    /// Remove a container (per-container lock)
    pub fn remove(&self, container_id: &str) -> Option<T> {
        ConsoleLogger::debug(&format!("[REGISTRY] Lock-free remove: {}", container_id));
        
        // Remove the container
        let result = self.containers.remove(container_id).map(|(_, container)| container);
        
        // Clean up the operation lock
        self.operation_locks.remove(container_id);
        
        result
    }

    /// Check if container exists (lock-free)
    pub fn contains(&self, container_id: &str) -> bool {
        self.containers.contains_key(container_id)
    }

    /// Get all container IDs (lock-free)
    pub fn keys(&self) -> Vec<String> {
        self.containers.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Execute a function with read access to container (lock-free)
    pub fn with_container<F, R>(&self, container_id: &str, accessor: F) -> Option<R>
    where
        F: FnOnce(&T) -> R,
    {
        self.containers.get(container_id).map(|entry| accessor(entry.value()))
    }

    /// Execute a function with exclusive access to multiple containers (ordered locking)
    pub fn with_multiple_containers<F, R>(&self, container_ids: &[String], accessor: F) -> Option<R>
    where
        F: FnOnce(&[&T]) -> R,
    {
        // Sort container IDs to prevent deadlocks
        let mut sorted_ids = container_ids.to_vec();
        sorted_ids.sort();
        
        ConsoleLogger::debug(&format!("[REGISTRY] Multi-container operation: {:?}", sorted_ids));
        
        // Acquire locks in sorted order - collect into owned vector
        let locks: Vec<_> = sorted_ids.iter()
            .map(|id| {
                self.operation_locks
                    .entry(id.clone())
                    .or_insert_with(|| Arc::new(ParkingMutex::new(())))
                    .clone()
            })
            .collect();
        
        let _guards: Vec<_> = locks.iter().map(|lock| lock.lock()).collect();
        
        // Get all containers
        let containers: Vec<_> = sorted_ids.iter()
            .filter_map(|id| self.containers.get(id))
            .collect();
        
        if containers.len() == sorted_ids.len() {
            let container_refs: Vec<_> = containers.iter().map(|entry| entry.value()).collect();
            Some(accessor(&container_refs))
        } else {
            None
        }
    }

    /// Get statistics about the registry
    pub fn stats(&self) -> RegistryStats {
        RegistryStats {
            container_count: self.containers.len(),
            operation_locks_count: self.operation_locks.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegistryStats {
    pub container_count: usize,
    pub operation_locks_count: usize,
}

/// Lock-free atomic operations utility
pub struct AtomicOperations;

impl AtomicOperations {
    /// Atomically increment a counter
    pub fn increment_counter(counter: &std::sync::atomic::AtomicUsize) -> usize {
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    }

    /// Atomically decrement a counter
    pub fn decrement_counter(counter: &std::sync::atomic::AtomicUsize) -> usize {
        counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed).saturating_sub(1)
    }

    /// Atomically compare and swap
    pub fn compare_and_swap<T>(
        atomic: &std::sync::atomic::AtomicUsize, 
        current: usize, 
        new: usize
    ) -> Result<usize, usize> {
        match atomic.compare_exchange_weak(
            current, 
            new, 
            std::sync::atomic::Ordering::Acquire,
            std::sync::atomic::Ordering::Relaxed
        ) {
            Ok(prev) => Ok(prev),
            Err(actual) => Err(actual),
        }
    }
}

/// Performance monitoring for locking operations
pub struct LockingMetrics {
    pub lock_acquisitions: std::sync::atomic::AtomicUsize,
    pub lock_contentions: std::sync::atomic::AtomicUsize,
    pub average_lock_time_ns: std::sync::atomic::AtomicU64,
}

impl LockingMetrics {
    pub fn new() -> Self {
        Self {
            lock_acquisitions: std::sync::atomic::AtomicUsize::new(0),
            lock_contentions: std::sync::atomic::AtomicUsize::new(0),
            average_lock_time_ns: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn record_lock_acquisition(&self, duration_ns: u64) {
        self.lock_acquisitions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        // Update rolling average
        let current_avg = self.average_lock_time_ns.load(std::sync::atomic::Ordering::Relaxed);
        let new_avg = (current_avg + duration_ns) / 2;
        self.average_lock_time_ns.store(new_avg, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn record_contention(&self) {
        self.lock_contentions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get_stats(&self) -> HashMap<String, String> {
        let mut stats = HashMap::new();
        stats.insert("lock_acquisitions".to_string(), 
                    self.lock_acquisitions.load(std::sync::atomic::Ordering::Relaxed).to_string());
        stats.insert("lock_contentions".to_string(), 
                    self.lock_contentions.load(std::sync::atomic::Ordering::Relaxed).to_string());
        stats.insert("average_lock_time_ns".to_string(), 
                    self.average_lock_time_ns.load(std::sync::atomic::Ordering::Relaxed).to_string());
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[derive(Debug, Clone)]
    struct TestContainer {
        id: String,
        value: i32,
    }

    #[test]
    fn test_concurrent_registry() {
        let registry = Arc::new(ConcurrentContainerRegistry::new());
        
        // Insert test container
        registry.insert("test1".to_string(), TestContainer {
            id: "test1".to_string(),
            value: 42,
        });
        
        // Test concurrent reads
        let handles: Vec<_> = (0..10).map(|_| {
            let registry = registry.clone();
            thread::spawn(move || {
                registry.get("test1").unwrap().value
            })
        }).collect();
        
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 42);
        }
    }

    #[test]
    fn test_per_container_locking() {
        let registry = Arc::new(ConcurrentContainerRegistry::new());
        
        // Insert test containers
        registry.insert("test1".to_string(), TestContainer {
            id: "test1".to_string(),
            value: 0,
        });
        registry.insert("test2".to_string(), TestContainer {
            id: "test2".to_string(),
            value: 0,
        });
        
        // Test concurrent updates to different containers (should not block each other)
        let handles: Vec<_> = (0..10).map(|i| {
            let registry = registry.clone();
            let container_id = if i % 2 == 0 { "test1" } else { "test2" };
            
            thread::spawn(move || {
                registry.update(container_id, |container| {
                    container.value += 1;
                });
            })
        }).collect();
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        // Both containers should have been updated independently
        assert_eq!(registry.get("test1").unwrap().value, 5);
        assert_eq!(registry.get("test2").unwrap().value, 5);
    }
} 