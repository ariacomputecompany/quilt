// src/daemon/events.rs
// Event-driven container startup coordination system

use crate::utils::console::ConsoleLogger;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use std::time::SystemTime;

/// Container startup event types for deterministic coordination
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerEvent {
    /// Container record created in database
    ContainerCreated {
        container_id: String,
        timestamp: SystemTime,
    },
    
    /// Network IP allocated in database
    NetworkAllocated {
        container_id: String,
        ip_address: String,
        timestamp: SystemTime,
    },
    
    /// Container process started with PID
    ProcessStarted {
        container_id: String,
        pid: i32,
        timestamp: SystemTime,
    },
    
    /// Network setup process initiated
    NetworkSetupStarted {
        container_id: String,
        timestamp: SystemTime,
    },
    
    /// Veth pair created successfully
    VethPairCreated {
        container_id: String,
        host_veth: String,
        container_veth: String,
        timestamp: SystemTime,
    },
    
    /// Veth attached to bridge successfully
    BridgeAttached {
        container_id: String,
        bridge_name: String,
        timestamp: SystemTime,
    },
    
    /// Container network setup completed
    NetworkSetupCompleted {
        container_id: String,
        ip_address: String,
        timestamp: SystemTime,
    },
    
    /// Container fully ready for use
    ContainerReady {
        container_id: String,
        total_startup_time_ms: u64,
        timestamp: SystemTime,
    },
    
    /// Network setup failed - triggers rollback
    NetworkSetupFailed {
        container_id: String,
        error: String,
        timestamp: SystemTime,
    },
    
    /// Container startup failed
    ContainerStartupFailed {
        container_id: String,
        error: String,
        phase: String,
        timestamp: SystemTime,
    },
}

impl ContainerEvent {
    pub fn container_id(&self) -> &str {
        match self {
            ContainerEvent::ContainerCreated { container_id, .. } => container_id,
            ContainerEvent::NetworkAllocated { container_id, .. } => container_id,
            ContainerEvent::ProcessStarted { container_id, .. } => container_id,
            ContainerEvent::NetworkSetupStarted { container_id, .. } => container_id,
            ContainerEvent::VethPairCreated { container_id, .. } => container_id,
            ContainerEvent::BridgeAttached { container_id, .. } => container_id,
            ContainerEvent::NetworkSetupCompleted { container_id, .. } => container_id,
            ContainerEvent::ContainerReady { container_id, .. } => container_id,
            ContainerEvent::NetworkSetupFailed { container_id, .. } => container_id,
            ContainerEvent::ContainerStartupFailed { container_id, .. } => container_id,
        }
    }
    
    pub fn timestamp(&self) -> SystemTime {
        match self {
            ContainerEvent::ContainerCreated { timestamp, .. } => *timestamp,
            ContainerEvent::NetworkAllocated { timestamp, .. } => *timestamp,
            ContainerEvent::ProcessStarted { timestamp, .. } => *timestamp,
            ContainerEvent::NetworkSetupStarted { timestamp, .. } => *timestamp,
            ContainerEvent::VethPairCreated { timestamp, .. } => *timestamp,
            ContainerEvent::BridgeAttached { timestamp, .. } => *timestamp,
            ContainerEvent::NetworkSetupCompleted { timestamp, .. } => *timestamp,
            ContainerEvent::ContainerReady { timestamp, .. } => *timestamp,
            ContainerEvent::NetworkSetupFailed { timestamp, .. } => *timestamp,
            ContainerEvent::ContainerStartupFailed { timestamp, .. } => *timestamp,
        }
    }
    
    pub fn event_name(&self) -> &'static str {
        match self {
            ContainerEvent::ContainerCreated { .. } => "ContainerCreated",
            ContainerEvent::NetworkAllocated { .. } => "NetworkAllocated", 
            ContainerEvent::ProcessStarted { .. } => "ProcessStarted",
            ContainerEvent::NetworkSetupStarted { .. } => "NetworkSetupStarted",
            ContainerEvent::VethPairCreated { .. } => "VethPairCreated",
            ContainerEvent::BridgeAttached { .. } => "BridgeAttached",
            ContainerEvent::NetworkSetupCompleted { .. } => "NetworkSetupCompleted",
            ContainerEvent::ContainerReady { .. } => "ContainerReady",
            ContainerEvent::NetworkSetupFailed { .. } => "NetworkSetupFailed",
            ContainerEvent::ContainerStartupFailed { .. } => "ContainerStartupFailed",
        }
    }
}

/// Event subscriber for waiting on specific container events
pub type EventReceiver = mpsc::UnboundedReceiver<ContainerEvent>;
pub type EventSender = mpsc::UnboundedSender<ContainerEvent>;

/// Container startup event coordinator - replaces timeout-based coordination
pub struct ContainerEventCoordinator {
    /// Event subscribers indexed by container ID
    subscribers: Arc<RwLock<HashMap<String, Vec<EventSender>>>>,
    /// Global event log for debugging
    event_log: Arc<RwLock<Vec<ContainerEvent>>>,
}

impl ContainerEventCoordinator {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            event_log: Arc::new(RwLock::new(Vec::new())),
        }
    }
    
    /// Emit a container event - triggers all waiting subscribers
    pub async fn emit_event(&self, event: ContainerEvent) {
        let container_id = event.container_id().to_string();
        
        // Log event for debugging
        {
            let mut log = self.event_log.write().await;
            log.push(event.clone());
            
            // Keep only recent events (last 1000)
            if log.len() > 1000 {
                log.drain(0..500);
            }
        }
        
        // Emit to console for visibility
        ConsoleLogger::info(&format!("📡 [EVENT] {} -> {}", 
            event.event_name(), container_id));
        
        // Notify all subscribers for this container
        let subscribers = self.subscribers.read().await;
        if let Some(senders) = subscribers.get(&container_id) {
            for sender in senders {
                if sender.send(event.clone()).is_err() {
                    // Subscriber disconnected, will be cleaned up later
                }
            }
        }
    }
    
    /// Subscribe to events for a specific container
    pub async fn subscribe_to_container(&self, container_id: &str) -> EventReceiver {
        let (sender, receiver) = mpsc::unbounded_channel();
        
        let mut subscribers = self.subscribers.write().await;
        subscribers.entry(container_id.to_string())
            .or_insert_with(Vec::new)
            .push(sender);
            
        receiver
    }
    
    /// Wait for a specific event type for a container with no timeout
    pub async fn wait_for_event<F>(&self, container_id: &str, predicate: F) -> Result<ContainerEvent, String>
    where
        F: Fn(&ContainerEvent) -> bool,
    {
        let mut receiver = self.subscribe_to_container(container_id).await;
        
        ConsoleLogger::debug(&format!("🔍 [EVENT-WAIT] Waiting for event matching predicate for {}", container_id));
        
        while let Some(event) = receiver.recv().await {
            if predicate(&event) {
                ConsoleLogger::debug(&format!("✅ [EVENT-WAIT] Found matching event {} for {}", 
                    event.event_name(), container_id));
                return Ok(event);
            }
        }
        
        Err(format!("Event stream closed for container {}", container_id))
    }
    
    /// Wait for network setup completion - completely event-driven
    pub async fn wait_for_network_ready(&self, container_id: &str) -> Result<ContainerEvent, String> {
        self.wait_for_event(container_id, |event| {
            matches!(event, ContainerEvent::NetworkSetupCompleted { .. }) ||
            matches!(event, ContainerEvent::NetworkSetupFailed { .. })
        }).await
    }
    
    /// Wait for container to be fully ready - completely event-driven
    pub async fn wait_for_container_ready(&self, container_id: &str) -> Result<ContainerEvent, String> {
        self.wait_for_event(container_id, |event| {
            matches!(event, ContainerEvent::ContainerReady { .. }) ||
            matches!(event, ContainerEvent::ContainerStartupFailed { .. })
        }).await
    }
    
    /// Get event history for debugging
    pub async fn get_event_history(&self, container_id: Option<&str>) -> Vec<ContainerEvent> {
        let log = self.event_log.read().await;
        match container_id {
            Some(id) => log.iter()
                .filter(|event| event.container_id() == id)
                .cloned()
                .collect(),
            None => log.clone(),
        }
    }
    
    /// Clean up subscribers for completed containers
    pub async fn cleanup_container_subscribers(&self, container_id: &str) {
        let mut subscribers = self.subscribers.write().await;
        subscribers.remove(container_id);
        ConsoleLogger::debug(&format!("🧹 [EVENT] Cleaned up subscribers for {}", container_id));
    }
    
    /// Get current subscriber count (for debugging)
    pub async fn get_subscriber_count(&self) -> usize {
        let subscribers = self.subscribers.read().await;
        subscribers.values().map(|v| v.len()).sum()
    }
}

/// Global static coordinator instance
static EVENT_COORDINATOR: std::sync::OnceLock<ContainerEventCoordinator> = std::sync::OnceLock::new();

/// Get the global event coordinator instance
pub fn get_event_coordinator() -> &'static ContainerEventCoordinator {
    EVENT_COORDINATOR.get_or_init(|| {
        ConsoleLogger::info("📡 [EVENT] Initializing global container event coordinator");
        ContainerEventCoordinator::new()
    })
}

/// Helper macros for easy event emission - async block for proper async context
#[macro_export]
macro_rules! emit_container_event {
    ($event:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let _ = tokio::spawn(async move {
                coordinator.emit_event($event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_container_created {
    ($container_id:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::ContainerCreated {
                container_id: $container_id.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_network_allocated {
    ($container_id:expr, $ip:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::NetworkAllocated {
                container_id: $container_id.to_string(),
                ip_address: $ip.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_process_started {
    ($container_id:expr, $pid:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::ProcessStarted {
                container_id: $container_id.to_string(),
                pid: $pid,
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_network_setup_started {
    ($container_id:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::NetworkSetupStarted {
                container_id: $container_id.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_veth_pair_created {
    ($container_id:expr, $host_veth:expr, $container_veth:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::VethPairCreated {
                container_id: $container_id.to_string(),
                host_veth: $host_veth.to_string(),
                container_veth: $container_veth.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_bridge_attached {
    ($container_id:expr, $bridge_name:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::BridgeAttached {
                container_id: $container_id.to_string(),
                bridge_name: $bridge_name.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_network_setup_completed {
    ($container_id:expr, $ip:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::NetworkSetupCompleted {
                container_id: $container_id.to_string(),
                ip_address: $ip.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_container_ready {
    ($container_id:expr, $startup_time_ms:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::ContainerReady {
                container_id: $container_id.to_string(),
                total_startup_time_ms: $startup_time_ms,
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_network_setup_failed {
    ($container_id:expr, $error:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::NetworkSetupFailed {
                container_id: $container_id.to_string(),
                error: $error.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}

#[macro_export]
macro_rules! emit_container_startup_failed {
    ($container_id:expr, $error:expr, $phase:expr) => {
        {
            let coordinator = crate::daemon::events::get_event_coordinator();
            let event = crate::daemon::events::ContainerEvent::ContainerStartupFailed {
                container_id: $container_id.to_string(),
                error: $error.to_string(),
                phase: $phase.to_string(),
                timestamp: std::time::SystemTime::now(),
            };
            let _ = tokio::spawn(async move {
                coordinator.emit_event(event).await;
            });
        }
    };
}