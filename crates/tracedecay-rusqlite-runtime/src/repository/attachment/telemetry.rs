use std::path::{Path, PathBuf};

use crate::{
    CheckpointPressure, CheckpointStatus, SqliteVmSnapshot, WalCheckpointSnapshot,
    WriterBatchTotals, WriterLockWorkSnapshot, WriterOperationCounters, WriterTransactionTotals,
};
use tracedecay_store::CommitSequenceV1;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryWriterRuntimeSnapshot {
    pub operations: WriterOperationCounters,
    pub batches: WriterBatchTotals,
    pub error_events: u64,
    pub health_lane_services: u64,
    pub commit_sequence: CommitSequenceV1,
    pub transactions: WriterTransactionTotals,
    pub sqlite_vm: SqliteVmSnapshot,
    pub wal: WalCheckpointSnapshot,
    pub lock_work: WriterLockWorkSnapshot,
    pub checkpoint_status: CheckpointStatus,
    pub checkpoint_pressure: CheckpointPressure,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryRuntimePhysicalSnapshot {
    pub healthy: bool,
    pub writer_present: bool,
    pub reader_handles: u32,
    pub general_reader_waiters: u16,
    pub health_reader_waiters: u16,
    pub queued_operations: u32,
    pub queued_bytes: u64,
    pub writer_busy_events: u64,
    pub writer: Option<RepositoryWriterRuntimeSnapshot>,
    pub wal_bytes: Option<u64>,
    pub snapshot_admissions: u64,
    pub active_readers: u16,
    pub reader_wait_micros: u64,
    pub reader_execution_micros: u64,
}

impl RepositoryRuntimePhysicalSnapshot {
    #[hotpath::skip]
    pub const fn is_drained(&self) -> bool {
        !self.writer_present
            && self.reader_handles == 0
            && self.queued_operations == 0
            && self.queued_bytes == 0
    }
}

pub(super) fn wal_bytes(database_path: &Path) -> Option<u64> {
    let mut name = database_path.as_os_str().to_os_string();
    name.push("-wal");
    match std::fs::metadata(PathBuf::from(name)) {
        Ok(metadata) => Some(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(0),
        Err(_) => None,
    }
}
