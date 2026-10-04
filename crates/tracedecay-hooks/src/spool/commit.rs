//! Group commit for appended frames.
//!
//! A writer appends its frame under the writer lease without a durability
//! barrier, releases the lease, and only then commits. Under the separate
//! commit lock one committer syncs the records file for every frame written
//! before that sync and publishes how far it reached; a later committer whose
//! frame ends inside the published extent returns without syncing. Concurrent
//! hooks therefore share one fsync per batch instead of each holding the
//! writer lease across its own barriers.
//!
//! The published extent names the records file it describes and is only a
//! skip hint: a missing, torn, or foreign one makes the next committer sync.
//! Every records file is created or replaced without a directory barrier, so
//! the first commit that finds no extent for a file also syncs its directory
//! entry. A rewrite in place (torn-tail truncation, reset) happens under the
//! commit lock and forgets the extent; a compaction forgets it before staging
//! its replacement, so no extent can outlive its file into a reused identity.

use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use tracedecay_domain::framed_log::checksum as frame_checksum;
use tracedecay_private_fs::FileLease;
use tracedecay_private_fs::framed_log::{sync_directory, sync_file_data};

use super::checkpoint::{read_transition, records_identity};
use super::lease::lock_member;
use super::{DIRECTORY_POLICY, HookSpoolError, records_path};

pub(super) const COMMIT_FILE: &str = "commit.v1.lock";
const IDENTITY_BYTES: usize = 32;
const EXTENT_BYTES: usize = IDENTITY_BYTES + 8;
const EXTENT_RECORD_BYTES: usize = EXTENT_BYTES + 32;

/// The held commit lock. Its file stores the synced extent, which is read
/// and written only while the lock is held.
pub(super) struct CommitLockV1 {
    file: FileLease,
}

impl CommitLockV1 {
    pub(super) fn acquire(root: &Path, wait_budget: Duration) -> Result<Self, HookSpoolError> {
        Ok(Self {
            file: lock_member(root, COMMIT_FILE, "hooks.spool.commit", Some(wait_budget))?,
        })
    }

    /// How far a completed sync of the records file `identity` reached.
    pub(super) fn synced_through(&self, identity: [u8; 32]) -> Result<Option<u64>, HookSpoolError> {
        let mut file = &*self.file;
        let mut bytes = Vec::with_capacity(EXTENT_RECORD_BYTES);
        file.seek(SeekFrom::Start(0))
            .and_then(|_| {
                file.take(EXTENT_RECORD_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|_| HookSpoolError::Io)?;
        if bytes.len() != EXTENT_RECORD_BYTES
            || frame_checksum(&bytes[..EXTENT_BYTES])[..] != bytes[EXTENT_BYTES..]
            || bytes[..IDENTITY_BYTES] != identity
        {
            return Ok(None);
        }
        let synced = bytes[IDENTITY_BYTES..EXTENT_BYTES]
            .try_into()
            .map_err(|_| HookSpoolError::MetadataCorrupted)?;
        Ok(Some(u64::from_le_bytes(synced)))
    }

    fn publish(&self, identity: [u8; 32], synced_through: u64) -> Result<(), HookSpoolError> {
        let mut bytes = Vec::with_capacity(EXTENT_RECORD_BYTES);
        bytes.extend_from_slice(&identity);
        bytes.extend_from_slice(&synced_through.to_le_bytes());
        let checksum = frame_checksum(&bytes);
        bytes.extend_from_slice(&checksum);
        let mut file = &*self.file;
        file.set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(&bytes))
            .map_err(|_| HookSpoolError::Io)
    }

    /// Forgets the synced extent; the records file was rewritten.
    pub(super) fn invalidate(&self) -> Result<(), HookSpoolError> {
        self.file.set_len(0).map_err(|_| HookSpoolError::Io)
    }
}

/// Forgets the synced extent before a compaction stages its replacement.
pub(super) fn forget_synced_extent(
    root: &Path,
    wait_budget: Duration,
) -> Result<(), HookSpoolError> {
    CommitLockV1::acquire(root, wait_budget)?.invalidate()
}

/// Makes the records file `identity` durable through `end`, sharing one sync
/// with every committer whose frame was written before it.
#[tracing::instrument(name = "hooks.spool.commit_records", level = "trace", skip_all)]
pub(super) fn commit_records(
    root: &Path,
    identity: [u8; 32],
    end: u64,
    wait_budget: Duration,
) -> Result<(), HookSpoolError> {
    let lock = CommitLockV1::acquire(root, wait_budget)?;
    let path = records_path(root);
    let records = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(records) => records,
        // A reset discarded the appended frames before they committed.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(HookSpoolError::RecoveryRequired);
        }
        Err(_) => return Err(HookSpoolError::Io),
    };
    let current = records_identity(&records)?;
    if current != identity {
        // A compaction republished every pending frame through a synced
        // replacement; only its directory entry may still be volatile.
        if lock.synced_through(current)?.is_none() {
            sync_records_directory(root)?;
            lock.publish(current, 0)?;
        }
        return Ok(());
    }
    let synced = lock.synced_through(identity)?;
    if synced.is_some_and(|synced| synced >= end) {
        metrics::gauge!("hooks.spool.commit.shared").increment(1);
        return Ok(());
    }
    let written = records.metadata().map_err(|_| HookSpoolError::Io)?.len();
    if written < end {
        return Err(HookSpoolError::MetadataCorrupted);
    }
    // The checkpoint transition is published after each complete frame, so
    // every byte it names was written before this sync starts.
    let claim = read_transition(root)?
        .filter(|transition| transition.current_revision.identity == identity)
        .map_or(end, |transition| {
            transition.current_revision.length.max(end)
        })
        .min(written);
    {
        let _span = tracing::trace_span!("hooks.spool.fsync.commit").entered();
        sync_file_data(&path, &records)
    }
    .map_err(|_| HookSpoolError::Io)?;
    if synced.is_none() {
        sync_records_directory(root)?;
    }
    lock.publish(identity, claim)
}

fn sync_records_directory(root: &Path) -> Result<(), HookSpoolError> {
    {
        let _span = tracing::trace_span!("hooks.spool.fsync.commit_directory").entered();
        sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookSpoolError::Io)
    }
}
