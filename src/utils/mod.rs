// Utility modules for common functionality
pub mod command;
pub mod console;
pub mod filesystem;
pub mod image;
pub mod locking;
pub mod process;
pub mod validation;

// Re-export only currently used utilities to avoid warnings
pub use console::ConsoleLogger;

// Future utilities ready for use (allow dead code for now)
#[allow(unused_imports)]
pub use command::{CommandExecutor, CommandResult};

#[allow(unused_imports)] 
pub use filesystem::{FileSystemUtils, DirectoryCreator};

#[allow(unused_imports)]
pub use process::{ProcessUtils, PidManager};

#[allow(unused_imports)]
pub use validation::{InputValidator, ConfigValidator};

#[allow(unused_imports)]
pub use image::{ImageManager, ImageLayerCache, ImageLayerInfo};

#[allow(unused_imports)]
pub use locking::{ConcurrentContainerRegistry, AtomicOperations, LockingMetrics};

// Container management is now implemented directly in the daemon layer 