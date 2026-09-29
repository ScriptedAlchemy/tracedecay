//! Process-lifetime heap no worktree owns.
//!
//! The owner inventory charges what worktrees retain. The rest of a settled
//! daemon's anonymous memory lives as long as the process: heap sampled here
//! where a live measure exists, plus [`PROCESS_RUNTIME_ALLOWANCE_BYTES_V1`]
//! for what has none.

use tracedecay_domain::research::pooled_canonical_scratch_bytes;

use super::ResidentOwnerBytesV1;

/// Fixed process memory that has no live measure.
pub const PROCESS_RUNTIME_ALLOWANCE_BYTES_V1: u64 = 256 * 1024 * 1024;

/// Heap the process holds for its own lifetime, by what holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProcessRuntimeHeapKindV1 {
    /// `SQLite`'s own heap across every connection: page caches, schemas,
    /// prepared statements.
    SqliteHeap,
    /// Canonical serialization buffers threads keep for reuse until they idle.
    CanonicalScratch,
}

impl ProcessRuntimeHeapKindV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SqliteHeap => "sqlite_heap",
            Self::CanonicalScratch => "canonical_scratch",
        }
    }
}

/// One process-lifetime heap and what it holds now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessRuntimeHeapSampleV1 {
    pub kind: ProcessRuntimeHeapKindV1,
    pub bytes: ResidentOwnerBytesV1,
}

/// Every measured process-lifetime heap, in [`ProcessRuntimeHeapKindV1`] order.
#[must_use]
pub fn sample_process_runtime_heap_v1() -> [ProcessRuntimeHeapSampleV1; 2] {
    // SAFETY: `sqlite3_memory_used` takes no arguments and only reads the
    // library's memory statistics, which it guards itself.
    let sqlite = unsafe { rusqlite::ffi::sqlite3_memory_used() };
    [
        ProcessRuntimeHeapSampleV1 {
            kind: ProcessRuntimeHeapKindV1::SqliteHeap,
            bytes: u64::try_from(sqlite).map_or(
                ResidentOwnerBytesV1::Unmeasured,
                ResidentOwnerBytesV1::Measured,
            ),
        },
        ProcessRuntimeHeapSampleV1 {
            kind: ProcessRuntimeHeapKindV1::CanonicalScratch,
            bytes: ResidentOwnerBytesV1::Measured(pooled_canonical_scratch_bytes()),
        },
    ]
}
