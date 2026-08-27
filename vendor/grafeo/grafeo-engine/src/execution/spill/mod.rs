//! Async spill file management (moved from grafeo-core to keep grafeo-core free of tokio).
//!
//! The sync spill infrastructure (`SpillManager`, `SpillFile`, `ExternalSort`)
//! remains in `grafeo-core::execution::spill`. These async wrappers live here
//! because they require a tokio runtime.

#[cfg(feature = "async-storage")]
pub mod async_file;
#[cfg(feature = "async-storage")]
pub mod async_manager;

#[cfg(feature = "async-storage")]
pub use async_file::{AsyncSpillFile, AsyncSpillFileReader};
#[cfg(feature = "async-storage")]
pub use async_manager::AsyncSpillManager;
