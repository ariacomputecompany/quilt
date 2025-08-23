// Utility modules for common functionality
pub mod console;
pub mod logger;
pub mod process;
pub mod validation;
pub mod security;
pub mod command;
pub mod filesystem;

// Re-export actually used utilities
// Note: Direct module access is preferred throughout the codebase 