//! Tiered storage: trait for data structures that can live in RAM or on disk.
//!
//! The [`TieredStore`] trait defines how a storage subsystem transitions
//! between memory tiers. The [`BufferManager`](super::BufferManager) decides
//! *when* to transition (based on memory pressure); this trait defines *how*.
//!
//! # Storage states
//!
//! ```text
//!       ┌──────────────┐    spill()     ┌──────────────┐
//!       │   InMemory   │ ────────────> │    OnDisk    │
//!       │ (RAM, fast)  │               │ (mmap, warm) │
//!       └──────────────┘               └──────┬───────┘
//!              ▲                               │
//!              └───────── reload() ────────────┘
//! ```
//!
//! All states expose the same read interface. Callers never need to know
//! which tier is active.

use std::path::Path;

use crate::utils::error::Result;

/// The current storage tier of a data structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StorageTier {
    /// Fully in RAM. Fastest access for both reads and writes.
    InMemory,
    /// On disk, accessed via mmap. The OS page cache provides warm reads.
    /// Mutations go through a WAL overlay.
    OnDisk,
    /// Not yet initialized (structure exists but has no data).
    Uninitialized,
}

impl StorageTier {
    /// Returns `true` if data is fully in RAM.
    #[must_use]
    pub fn is_in_memory(self) -> bool {
        self == Self::InMemory
    }

    /// Returns `true` if data is served from disk (mmap).
    #[must_use]
    pub fn is_on_disk(self) -> bool {
        self == Self::OnDisk
    }
}

/// A storage subsystem that can transition between RAM and disk tiers.
///
/// **Deprecated (Phase 8c, 0.5.42):** the [`crate::storage::section::Section`]
/// trait subsumed this trait's responsibilities — its `swap_to_mmap` and
/// `reload_to_ram` methods cover the same lifecycle, and every wired
/// section type (LPG, RDF, Vector, Ring, Compact) implements `Section`,
/// not `TieredStore`. This trait was never implemented anywhere; it is
/// kept for one release as a no-op to avoid breaking downstream callers
/// who may have prepared their own impls. Migrate to `Section` for new
/// code; the standalone trait will be removed in 0.6.0.
///
/// Implementors manage their own data layout for both tiers. The
/// [`BufferManager`](super::BufferManager) triggers transitions via the
/// [`MemoryConsumer`](super::MemoryConsumer) trait; this trait provides
/// the mechanics of persisting, mapping, and reloading data.
///
/// # Crate boundaries
///
/// This trait lives in `grafeo-common` so it can be referenced by both
/// `grafeo-core` (section serializers) and `grafeo-engine` (orchestration).
/// Implementations that need filesystem I/O (mmap, file creation) should
/// live in `grafeo-engine`, not `grafeo-core`.
#[deprecated(
    since = "0.5.42",
    note = "Use the `Section` trait (with `swap_to_mmap` / `reload_to_ram`) and `MemoryConsumer` instead. \
            This trait was never implemented anywhere; it will be removed in 0.6.0."
)]
pub trait TieredStore: Send + Sync {
    /// Estimated RAM footprint in bytes if built entirely in memory.
    ///
    /// Returns 0 when the structure is on disk or uninitialized.
    fn estimated_ram_bytes(&self) -> usize;

    /// Current storage tier.
    fn tier(&self) -> StorageTier;

    /// Serializes in-memory state to the given path.
    ///
    /// After this call, [`open_mmap`](Self::open_mmap) can serve reads
    /// from the file. This does NOT free RAM: the caller should drop the
    /// in-memory representation separately (typically via the
    /// [`MemoryConsumer::spill`](super::MemoryConsumer::spill) path).
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or I/O fails.
    fn persist(&self, path: &Path) -> Result<()>;

    /// Switches to mmap-backed reads from a previously persisted file.
    ///
    /// Drops the in-memory data and serves reads through the OS page cache.
    /// The tier transitions to [`StorageTier::OnDisk`].
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be memory-mapped.
    fn open_mmap(&self, path: &Path) -> Result<()>;

    /// Reloads data from disk back into RAM.
    ///
    /// The tier transitions to [`StorageTier::InMemory`]. Used when memory
    /// becomes available and faster access is desired.
    ///
    /// # Errors
    ///
    /// Returns an error if deserialization fails.
    fn reload_to_ram(&self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_tier_predicates() {
        assert!(StorageTier::InMemory.is_in_memory());
        assert!(!StorageTier::InMemory.is_on_disk());

        assert!(StorageTier::OnDisk.is_on_disk());
        assert!(!StorageTier::OnDisk.is_in_memory());

        assert!(!StorageTier::Uninitialized.is_in_memory());
        assert!(!StorageTier::Uninitialized.is_on_disk());
    }

    #[test]
    fn test_storage_tier_equality() {
        assert_eq!(StorageTier::InMemory, StorageTier::InMemory);
        assert_ne!(StorageTier::InMemory, StorageTier::OnDisk);
        assert_ne!(StorageTier::OnDisk, StorageTier::Uninitialized);
    }
}
