//! Daemon session orchestration runtime.
//!
//! Owns the session temporal refresh scheduler, the daemon session sync
//! service, daemon-owned session retrieval, and the mounted LCM authority.
//! The composition root (`tracedecay`) wires these against its daemon
//! engine; this crate never depends on the root aggregate.

use std::time::Duration;

pub mod lcm_authority;
pub mod lcm_effects;
mod lcm_summarization;
pub mod session_retrieval;
pub mod session_sync;
pub mod session_temporal_refresh_scheduler;
mod store_owner;

pub use store_owner::StoreOwnerKey;

/// Deadline for draining daemon client-facing work at shutdown.
///
/// Shared by the composition root's shutdown orchestration and the session
/// temporal refresh scheduler's registry drain, so it has exactly one home.
pub const DAEMON_CLIENT_DRAIN_DEADLINE: Duration = Duration::from_secs(2);
