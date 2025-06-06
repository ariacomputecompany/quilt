// src/cli/mod.rs
// CLI module organization

pub mod containers;
pub mod icc;

// Re-export main types
pub use containers::{ContainerCommands, handle_container_command};
pub use icc::{ICCCommands, handle_icc_command};

// Re-export the protobuf definitions for shared use
pub mod quilt {
    tonic::include_proto!("quilt");
} 