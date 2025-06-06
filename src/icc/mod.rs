// src/icc/mod.rs
// Declares the modules for Inter-Container Communication

pub mod network;
pub mod dns;
pub mod messaging;

// Re-export key components for easier access
pub use network::{NetworkManager, NetworkConfig, ContainerNetworkConfig};
pub use dns::DnsServer;
pub use messaging::MessageBroker; 