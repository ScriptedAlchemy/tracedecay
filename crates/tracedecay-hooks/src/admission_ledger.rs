//! Durable, bounded Hook V2 admission idempotency ledger.
//!
//! This is deliberately *not* an event store. It persists only the identity of
//! an already-authorized envelope (`event_id`) plus a digest over the exact
//! canonical envelope bytes, so the daemon can answer three questions across a
//! restart:
//!
//! * has this exact envelope already been admitted? (`ExactDuplicate`)
//! * has this identity already been admitted carrying *different* bytes?
//!   (`Conflict`)
//! * otherwise: this is a first admission.
//!
//! It carries no payload, no session content, and no application state. The
//! daemon is the sole writer; the ledger is stored beside the transport spool
//! in the same daemon-owned hook data root and never touches a migrated
//! database.
//!
//! Bounds (stated, not implied): at most
//! [`HookAdmissionLedgerLimitsV1::max_records`] live entries per host and
//! nothing older than [`HookAdmissionLedgerLimitsV1::max_age_micros`]. Beyond
//! either bound the oldest entries are dropped, so idempotency converges within
//! that window and no further — a replay older than the window is admitted
//! again rather than silently believed to be new forever.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{UtcMicros, canonical_json_bytes, framed_log::checksum as frame_checksum};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, append_durable, atomic_write as shared_atomic_write,
    read_bounded as shared_read_bounded, sync_directory as shared_sync_directory,
    truncate_file as shared_truncate_file, validate_regular_or_missing as shared_validate_regular,
};

use crate::{HookEventEnvelopeV2, HookHostV1, MAX_SPOOL_AGE_MICROS, MAX_SPOOL_RECORDS_PER_HOST};

const LEDGER_MAGIC: &[u8; 4] = b"TDL1";
const LEDGER_FORMAT_VERSION: u16 = 1;
const HEADER_BYTES: usize = 6;
const IDENTITY_BYTES: usize = 16;
const DIGEST_BYTES: usize = 32;
const CHECKSUM_PREFIX_BYTES: usize = 8;
const RECORD_BODY_BYTES: usize = IDENTITY_BYTES + DIGEST_BYTES + 8;
const RECORD_BYTES: usize = RECORD_BODY_BYTES + CHECKSUM_PREFIX_BYTES;
const RECORDS_FILE: &str = "admissions.v1.bin";
const COMPLETIONS_FILE: &str = "admission-work-completions.v1.json";
const LOCK_FILE: &str = "admissions.v1.lock";
const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::Strict;

/// Checked-in ledger bounds. Callers may narrow these but never widen them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookAdmissionLedgerLimitsV1 {
    pub max_records: u32,
    pub max_age_micros: i64,
}

impl HookAdmissionLedgerLimitsV1 {
    pub const fn stock() -> Self {
        Self {
            max_records: MAX_SPOOL_RECORDS_PER_HOST,
            max_age_micros: MAX_SPOOL_AGE_MICROS,
        }
    }

    fn validate(self) -> Result<(), HookAdmissionLedgerError> {
        if self.max_records == 0
            || self.max_records > MAX_SPOOL_RECORDS_PER_HOST
            || self.max_age_micros <= 0
            || self.max_age_micros > MAX_SPOOL_AGE_MICROS
        {
            return Err(HookAdmissionLedgerError::InvalidLimits);
        }
        Ok(())
    }

    fn max_file_bytes(self) -> usize {
        HEADER_BYTES.saturating_add(self.max_records as usize * RECORD_BYTES * 2)
    }
}

/// What the ledger decided about one admission attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAdmissionDecisionV1 {
    /// First durable admission for this identity inside the retained window.
    Admitted,
    /// The same identity already carries exactly these bytes.
    ExactDuplicate,
    /// The same identity already carries *different* bytes.
    Conflict,
}

/// Durable ledger decision plus the stable order assigned to its entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookAdmissionLedgerReceiptV1 {
    pub decision: HookAdmissionDecisionV1,
    pub order: u64,
    pub work_completed: bool,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HookAdmissionLedgerError {
    #[error("hook admission ledger filesystem operation failed")]
    Io,
    #[error("hook admission ledger root or member path is unsafe")]
    UnsafePath,
    #[error("hook admission ledger limits are invalid")]
    InvalidLimits,
    #[error("hook admission ledger record is not canonically encodable")]
    RecordUnencodable,
    #[error("hook admission ledger identity is invalid")]
    InvalidIdentity,
    #[error("hook admission ledger is busy in another daemon")]
    Busy,
}

/// Bounded recovery report for an opened ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookAdmissionLedgerOpenReportV1 {
    pub live_records: u32,
    pub dropped_expired_records: u32,
    pub dropped_overflow_records: u32,
    pub truncated_tail_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LedgerEntry {
    digest: [u8; DIGEST_BYTES],
    admitted_at: UtcMicros,
    order: u64,
}

/// Digest over the exact canonical envelope bytes. Two envelopes with the same
/// `event_id` and different digests are a genuine producer conflict.
pub fn hook_admission_digest(
    envelope: &HookEventEnvelopeV2,
) -> Result<[u8; DIGEST_BYTES], HookAdmissionLedgerError> {
    let bytes =
        canonical_json_bytes(envelope).map_err(|_| HookAdmissionLedgerError::RecordUnencodable)?;
    Ok(frame_checksum(&bytes))
}

/// The daemon-owned, per-host admission ledger.
#[derive(Debug)]
pub struct HookAdmissionLedgerV1 {
    root: PathBuf,
    _writer_lock: fs::File,
    host: HookHostV1,
    limits: HookAdmissionLedgerLimitsV1,
    entries: BTreeMap<[u8; IDENTITY_BYTES], LedgerEntry>,
    completed_work: BTreeSet<[u8; IDENTITY_BYTES]>,
    next_order: u64,
}

impl Drop for HookAdmissionLedgerV1 {
    fn drop(&mut self) {
        let _ = self._writer_lock.unlock();
    }
}

impl HookAdmissionLedgerV1 {
    /// Open (and bounded-recover) the ledger for one host.
    pub fn open(
        root: impl Into<PathBuf>,
        host: HookHostV1,
        limits: HookAdmissionLedgerLimitsV1,
        now: UtcMicros,
    ) -> Result<(Self, HookAdmissionLedgerOpenReportV1), HookAdmissionLedgerError> {
        limits.validate()?;
        let root = root.into();
        ensure_root(&root)?;
        let writer_lock = acquire_writer_lock(&root)?;
        let path = records_path(&root);
        ensure_header(&path)?;
        let bytes = read_bounded(&path, limits.max_file_bytes())?.unwrap_or_default();
        let (scanned, truncated_tail_bytes) = scan_records(&bytes);
        let completions_existed = completions_path(&root).is_file();
        let mut ledger = Self {
            root,
            _writer_lock: writer_lock,
            host,
            limits,
            entries: BTreeMap::new(),
            completed_work: BTreeSet::new(),
            next_order: 0,
        };
        let mut dropped_expired_records = 0u32;
        for (identity, digest, admitted_at) in scanned {
            if is_expired(admitted_at, now, limits.max_age_micros) {
                dropped_expired_records = dropped_expired_records.saturating_add(1);
                // A later record for the same identity supersedes the earlier
                // one, so an expired duplicate must also evict the live entry.
                ledger.entries.remove(&identity);
                continue;
            }
            let order = ledger.next_order;
            ledger.next_order = ledger.next_order.saturating_add(1);
            ledger.entries.insert(
                identity,
                LedgerEntry {
                    digest,
                    admitted_at,
                    order,
                },
            );
        }
        ledger.completed_work = if completions_existed {
            read_work_completions(&ledger.root, limits.max_records)?
                .into_iter()
                .filter(|identity| ledger.entries.contains_key(identity))
                .collect()
        } else {
            // Records written before the durable producer-work outbox existed
            // were already treated as complete. Preserve that upgrade
            // invariant instead of redriving historical admissions.
            ledger.entries.keys().copied().collect()
        };
        let dropped_overflow_records = ledger.trim_to(limits.max_records as usize);
        if truncated_tail_bytes > 0 {
            shared_truncate_file(
                &path,
                (bytes.len() as u64).saturating_sub(truncated_tail_bytes),
                DIRECTORY_POLICY,
            )
            .map_err(|_| HookAdmissionLedgerError::Io)?;
        }
        if dropped_expired_records > 0
            || dropped_overflow_records > 0
            || ledger.entries.len() < ledger.next_order as usize
        {
            ledger.rewrite()?;
        } else if !completions_existed {
            ledger.write_work_completions()?;
        }
        let report = HookAdmissionLedgerOpenReportV1 {
            live_records: ledger.entries.len() as u32,
            dropped_expired_records,
            dropped_overflow_records,
            truncated_tail_bytes,
        };
        Ok((ledger, report))
    }

    pub fn host(&self) -> HookHostV1 {
        self.host
    }

    pub fn live_records(&self) -> u32 {
        self.entries.len() as u32
    }

    /// Record one admission attempt. `Admitted` is returned only after the
    /// identity is durably on disk, so a crash immediately afterwards still
    /// converges on replay.
    pub fn admit(
        &mut self,
        envelope: &HookEventEnvelopeV2,
        now: UtcMicros,
    ) -> Result<HookAdmissionDecisionV1, HookAdmissionLedgerError> {
        self.admit_with_receipt(envelope, now)
            .map(|receipt| receipt.decision)
    }

    /// Records one attempt and exposes the durable entry order. Exact
    /// duplicates reuse the original order, including after ledger reopen.
    pub fn admit_with_receipt(
        &mut self,
        envelope: &HookEventEnvelopeV2,
        now: UtcMicros,
    ) -> Result<HookAdmissionLedgerReceiptV1, HookAdmissionLedgerError> {
        let identity = envelope.event_id;
        if identity == [0; IDENTITY_BYTES] {
            return Err(HookAdmissionLedgerError::InvalidIdentity);
        }
        let digest = hook_admission_digest(envelope)?;
        if let Some(existing) = self.entries.get(&identity) {
            if is_expired(existing.admitted_at, now, self.limits.max_age_micros) {
                self.entries.remove(&identity);
            } else if existing.digest == digest {
                return Ok(HookAdmissionLedgerReceiptV1 {
                    decision: HookAdmissionDecisionV1::ExactDuplicate,
                    order: existing.order,
                    work_completed: self.completed_work.contains(&identity),
                });
            } else {
                return Ok(HookAdmissionLedgerReceiptV1 {
                    decision: HookAdmissionDecisionV1::Conflict,
                    order: existing.order,
                    work_completed: self.completed_work.contains(&identity),
                });
            }
        }
        if self.entries.len() as u32 >= self.limits.max_records {
            self.trim_to((self.limits.max_records as usize).saturating_sub(1));
            self.rewrite()?;
        }
        append_durable(
            &records_path(&self.root),
            &encode_record(identity, digest, now),
            DIRECTORY_POLICY,
        )
        .map_err(|_| HookAdmissionLedgerError::Io)?;
        let order = self.next_order;
        self.next_order = self.next_order.saturating_add(1);
        self.entries.insert(
            identity,
            LedgerEntry {
                digest,
                admitted_at: now,
                order,
            },
        );
        Ok(HookAdmissionLedgerReceiptV1 {
            decision: HookAdmissionDecisionV1::Admitted,
            order,
            work_completed: false,
        })
    }

    /// Mark producer work complete only after the admitted worker returns.
    /// Exact duplicate redrives remain pending until this fsync succeeds.
    pub fn mark_work_completed(
        &mut self,
        envelope: &HookEventEnvelopeV2,
    ) -> Result<bool, HookAdmissionLedgerError> {
        let identity = envelope.event_id;
        let Some(entry) = self.entries.get(&identity) else {
            return Err(HookAdmissionLedgerError::InvalidIdentity);
        };
        if entry.digest != hook_admission_digest(envelope)? {
            return Err(HookAdmissionLedgerError::InvalidIdentity);
        }
        if !self.completed_work.insert(identity) {
            return Ok(false);
        }
        match self.write_work_completions() {
            Ok(()) => Ok(true),
            Err(error) => {
                self.completed_work.remove(&identity);
                Err(error)
            }
        }
    }

    /// Drop entries older than the age bound. Returns how many were removed.
    pub fn expire(&mut self, now: UtcMicros) -> Result<u32, HookAdmissionLedgerError> {
        let before = self.entries.len();
        let max_age = self.limits.max_age_micros;
        self.entries
            .retain(|_, entry| !is_expired(entry.admitted_at, now, max_age));
        self.completed_work
            .retain(|identity| self.entries.contains_key(identity));
        let removed = before.saturating_sub(self.entries.len()) as u32;
        if removed > 0 {
            self.rewrite()?;
        }
        Ok(removed)
    }

    /// Drop the oldest entries until at most `allowed` remain. Compaction
    /// overshoots down to three quarters of the checked-in bound so the
    /// rewrite is amortized instead of firing on every later admission.
    fn trim_to(&mut self, allowed: usize) -> u32 {
        if self.entries.len() <= allowed {
            return 0;
        }
        let retained = allowed.min((self.limits.max_records as usize).saturating_mul(3) / 4);
        let mut ordered = self
            .entries
            .iter()
            .map(|(identity, entry)| (entry.order, *identity))
            .collect::<Vec<_>>();
        ordered.sort_unstable();
        let dropped = ordered.len().saturating_sub(retained);
        for (_, identity) in ordered.into_iter().take(dropped) {
            self.entries.remove(&identity);
            self.completed_work.remove(&identity);
        }
        dropped as u32
    }

    fn rewrite(&mut self) -> Result<(), HookAdmissionLedgerError> {
        let mut ordered = self
            .entries
            .iter()
            .map(|(identity, entry)| (entry.order, *identity, *entry))
            .collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|(order, _, _)| *order);
        let mut bytes = Vec::with_capacity(HEADER_BYTES + ordered.len() * RECORD_BYTES);
        bytes.extend_from_slice(LEDGER_MAGIC);
        bytes.extend_from_slice(&LEDGER_FORMAT_VERSION.to_le_bytes());
        self.next_order = 0;
        for (_, identity, entry) in &ordered {
            bytes.extend_from_slice(&encode_record(*identity, entry.digest, entry.admitted_at));
        }
        for (index, (_, identity, _)) in ordered.iter().enumerate() {
            if let Some(entry) = self.entries.get_mut(identity) {
                entry.order = index as u64;
            }
        }
        self.next_order = ordered.len() as u64;
        shared_atomic_write(
            &records_path(&self.root),
            "hook-admissions",
            &bytes,
            DIRECTORY_POLICY,
        )
        .map_err(|_| HookAdmissionLedgerError::Io)?;
        self.write_work_completions()
    }

    fn write_work_completions(&self) -> Result<(), HookAdmissionLedgerError> {
        let bytes = canonical_json_bytes(&self.completed_work.iter().copied().collect::<Vec<_>>())
            .map_err(|_| HookAdmissionLedgerError::RecordUnencodable)?;
        shared_atomic_write(
            &completions_path(&self.root),
            "hook-admission-work-completions",
            &bytes,
            DIRECTORY_POLICY,
        )
        .map_err(|_| HookAdmissionLedgerError::Io)
    }
}

fn completions_path(root: &Path) -> PathBuf {
    root.join(COMPLETIONS_FILE)
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(LOCK_FILE)
}

fn acquire_writer_lock(root: &Path) -> Result<fs::File, HookAdmissionLedgerError> {
    let path = lock_path(root);
    shared_validate_regular(&path).map_err(|_| HookAdmissionLedgerError::UnsafePath)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|_| HookAdmissionLedgerError::Io)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(HookAdmissionLedgerError::Busy),
        Err(std::fs::TryLockError::Error(_)) => Err(HookAdmissionLedgerError::Io),
    }
}

fn read_work_completions(
    root: &Path,
    max_records: u32,
) -> Result<Vec<[u8; IDENTITY_BYTES]>, HookAdmissionLedgerError> {
    let maximum = (max_records as usize)
        .saturating_mul(IDENTITY_BYTES.saturating_mul(4).saturating_add(8))
        .saturating_add(2);
    let Some(bytes) = read_bounded(&completions_path(root), maximum)? else {
        return Ok(Vec::new());
    };
    serde_json::from_slice(&bytes).map_err(|_| HookAdmissionLedgerError::Io)
}

fn is_expired(admitted_at: UtcMicros, now: UtcMicros, max_age_micros: i64) -> bool {
    now.0.saturating_sub(admitted_at.0) > max_age_micros
}

fn encode_record(
    identity: [u8; IDENTITY_BYTES],
    digest: [u8; DIGEST_BYTES],
    admitted_at: UtcMicros,
) -> [u8; RECORD_BYTES] {
    let mut record = [0u8; RECORD_BYTES];
    record[..IDENTITY_BYTES].copy_from_slice(&identity);
    record[IDENTITY_BYTES..IDENTITY_BYTES + DIGEST_BYTES].copy_from_slice(&digest);
    record[IDENTITY_BYTES + DIGEST_BYTES..RECORD_BODY_BYTES]
        .copy_from_slice(&admitted_at.0.to_le_bytes());
    let checksum = frame_checksum(&record[..RECORD_BODY_BYTES]);
    record[RECORD_BODY_BYTES..].copy_from_slice(&checksum[..CHECKSUM_PREFIX_BYTES]);
    record
}

type ScannedRecord = ([u8; IDENTITY_BYTES], [u8; DIGEST_BYTES], UtcMicros);

/// Scan the on-disk ledger. Returns every intact record in file order plus the
/// number of trailing bytes that are a partial or corrupt tail.
fn scan_records(bytes: &[u8]) -> (Vec<ScannedRecord>, u64) {
    if bytes.len() < HEADER_BYTES
        || &bytes[..4] != LEDGER_MAGIC
        || u16::from_le_bytes([bytes[4], bytes[5]]) != LEDGER_FORMAT_VERSION
    {
        return (Vec::new(), bytes.len() as u64);
    }
    let mut records = Vec::new();
    let mut offset = HEADER_BYTES;
    while offset + RECORD_BYTES <= bytes.len() {
        let record = &bytes[offset..offset + RECORD_BYTES];
        let checksum = frame_checksum(&record[..RECORD_BODY_BYTES]);
        if checksum[..CHECKSUM_PREFIX_BYTES] != record[RECORD_BODY_BYTES..] {
            break;
        }
        let mut identity = [0u8; IDENTITY_BYTES];
        identity.copy_from_slice(&record[..IDENTITY_BYTES]);
        let mut digest = [0u8; DIGEST_BYTES];
        digest.copy_from_slice(&record[IDENTITY_BYTES..IDENTITY_BYTES + DIGEST_BYTES]);
        let mut admitted = [0u8; 8];
        admitted.copy_from_slice(&record[IDENTITY_BYTES + DIGEST_BYTES..RECORD_BODY_BYTES]);
        records.push((identity, digest, UtcMicros(i64::from_le_bytes(admitted))));
        offset += RECORD_BYTES;
    }
    (records, (bytes.len() - offset) as u64)
}

fn records_path(root: &Path) -> PathBuf {
    root.join(RECORDS_FILE)
}

fn ledger_header() -> [u8; HEADER_BYTES] {
    let mut header = [0u8; HEADER_BYTES];
    header[..4].copy_from_slice(LEDGER_MAGIC);
    header[4..].copy_from_slice(&LEDGER_FORMAT_VERSION.to_le_bytes());
    header
}

/// Appends only ever add fixed-width records, so the header must exist before
/// the first admission or the whole file would scan as foreign bytes.
fn ensure_header(path: &Path) -> Result<(), HookAdmissionLedgerError> {
    shared_validate_regular(path).map_err(|_| HookAdmissionLedgerError::UnsafePath)?;
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(_) => return Err(HookAdmissionLedgerError::Io),
    };
    if length >= HEADER_BYTES as u64 {
        return Ok(());
    }
    shared_atomic_write(path, "hook-admissions", &ledger_header(), DIRECTORY_POLICY)
        .map_err(|_| HookAdmissionLedgerError::Io)
}

fn ensure_root(root: &Path) -> Result<(), HookAdmissionLedgerError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HookAdmissionLedgerError::UnsafePath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(HookAdmissionLedgerError::Io),
    }
    fs::create_dir_all(root).map_err(|_| HookAdmissionLedgerError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|_| HookAdmissionLedgerError::Io)?;
    }
    shared_sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookAdmissionLedgerError::Io)
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, HookAdmissionLedgerError> {
    shared_validate_regular(path).map_err(|_| HookAdmissionLedgerError::UnsafePath)?;
    match shared_read_bounded(path, maximum) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            Err(HookAdmissionLedgerError::UnsafePath)
        }
        Err(_) => Err(HookAdmissionLedgerError::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HOOK_EVENT_SCHEMA_VERSION, HookBoundaryV1, HookEventV2, HookOrderingV1};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(1);
            let path = std::env::temp_dir().join(format!(
                "tracedecay-hook-admissions-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn envelope(event_id: u8, epoch: u64) -> HookEventEnvelopeV2 {
        HookEventEnvelopeV2 {
            schema_version: HOOK_EVENT_SCHEMA_VERSION,
            event_id: [event_id; 16],
            producer: HookHostV1::ClaudeCode,
            protected_session_id: [7; 32],
            project_id: [1; 16],
            repository_id: [2; 16],
            worktree_id: [3; 16],
            worktree_epoch: epoch,
            binding_token: [4; 32],
            ordering: HookOrderingV1::Unknown,
            observed_at: UtcMicros(11),
            event: HookEventV2::SessionBoundary {
                boundary: HookBoundaryV1::TurnComplete,
            },
        }
    }

    fn open(root: &Path, now: UtcMicros) -> HookAdmissionLedgerV1 {
        HookAdmissionLedgerV1::open(
            root,
            HookHostV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            now,
        )
        .unwrap()
        .0
    }

    #[test]
    fn identical_bytes_converge_on_exact_duplicate() {
        let root = TestDir::new("ledger");
        let mut ledger = open(root.path(), UtcMicros(1));

        assert_eq!(
            ledger.admit(&envelope(9, 5), UtcMicros(2)).unwrap(),
            HookAdmissionDecisionV1::Admitted
        );
        assert_eq!(
            ledger.admit(&envelope(9, 5), UtcMicros(3)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
        assert_eq!(ledger.live_records(), 1);
    }

    #[test]
    fn same_identity_with_different_bytes_is_a_conflict() {
        let root = TestDir::new("ledger");
        let mut ledger = open(root.path(), UtcMicros(1));

        assert_eq!(
            ledger.admit(&envelope(9, 5), UtcMicros(2)).unwrap(),
            HookAdmissionDecisionV1::Admitted
        );
        assert_eq!(
            ledger.admit(&envelope(9, 6), UtcMicros(3)).unwrap(),
            HookAdmissionDecisionV1::Conflict
        );
    }

    #[test]
    fn idempotency_survives_a_reopen() {
        let root = TestDir::new("ledger");
        {
            let mut ledger = open(root.path(), UtcMicros(1));
            assert_eq!(
                ledger.admit(&envelope(9, 5), UtcMicros(2)).unwrap(),
                HookAdmissionDecisionV1::Admitted
            );
        }
        let mut reopened = open(root.path(), UtcMicros(4));

        assert_eq!(reopened.live_records(), 1);
        assert_eq!(
            reopened.admit(&envelope(9, 5), UtcMicros(5)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
        assert_eq!(
            reopened.admit(&envelope(9, 6), UtcMicros(6)).unwrap(),
            HookAdmissionDecisionV1::Conflict
        );
    }

    #[test]
    fn writer_lock_contends_and_releases_across_processes() {
        const MODE_ENV: &str = "TRACEDECAY_HOOK_ADMISSION_LOCK_PROBE";
        const ROOT_ENV: &str = "TRACEDECAY_HOOK_ADMISSION_LOCK_ROOT";
        if let Ok(mode) = std::env::var(MODE_ENV) {
            let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("child lock root"));
            match mode.as_str() {
                "contended" => assert!(matches!(
                    HookAdmissionLedgerV1::open(
                        &root,
                        HookHostV1::ClaudeCode,
                        HookAdmissionLedgerLimitsV1::stock(),
                        UtcMicros(2),
                    ),
                    Err(HookAdmissionLedgerError::Busy)
                )),
                "released" => {
                    HookAdmissionLedgerV1::open(
                        &root,
                        HookHostV1::ClaudeCode,
                        HookAdmissionLedgerLimitsV1::stock(),
                        UtcMicros(3),
                    )
                    .expect("OS releases the ledger lock when its owner exits");
                }
                other => panic!("unknown child lock probe mode: {other}"),
            }
            return;
        }

        let root = TestDir::new("process-lock");
        let first = open(root.path(), UtcMicros(1));
        let test_name =
            "admission_ledger::tests::writer_lock_contends_and_releases_across_processes";
        let run_child = |mode: &str| {
            Command::new(std::env::current_exe().expect("current test binary"))
                .args(["--exact", test_name, "--nocapture"])
                .env(MODE_ENV, mode)
                .env(ROOT_ENV, root.path())
                .status()
                .expect("run admission lock probe child")
        };
        assert!(run_child("contended").success());
        drop(first);
        assert!(run_child("released").success());
    }

    #[test]
    fn durable_receipt_order_is_successive_and_survives_duplicate_reopen() {
        let root = TestDir::new("ledger-receipt-order");
        let first_order;
        {
            let mut ledger = open(root.path(), UtcMicros(1));
            let first = ledger
                .admit_with_receipt(&envelope(9, 5), UtcMicros(2))
                .unwrap();
            let second = ledger
                .admit_with_receipt(&envelope(10, 5), UtcMicros(3))
                .unwrap();
            assert_eq!(first.decision, HookAdmissionDecisionV1::Admitted);
            assert_eq!(second.decision, HookAdmissionDecisionV1::Admitted);
            assert_eq!(second.order, first.order + 1);
            first_order = first.order;
        }

        let mut reopened = open(root.path(), UtcMicros(4));
        let duplicate = reopened
            .admit_with_receipt(&envelope(9, 5), UtcMicros(5))
            .unwrap();
        assert_eq!(duplicate.decision, HookAdmissionDecisionV1::ExactDuplicate);
        assert_eq!(duplicate.order, first_order);
    }

    #[test]
    fn pending_producer_work_redrives_until_completion_survives_reopen() {
        let root = TestDir::new("ledger-work-completion");
        let admitted = envelope(9, 5);
        {
            let mut ledger = open(root.path(), UtcMicros(1));
            let first = ledger.admit_with_receipt(&admitted, UtcMicros(2)).unwrap();
            assert!(!first.work_completed);
        }

        {
            let mut restarted = open(root.path(), UtcMicros(3));
            let duplicate = restarted
                .admit_with_receipt(&admitted, UtcMicros(4))
                .unwrap();
            assert_eq!(duplicate.decision, HookAdmissionDecisionV1::ExactDuplicate);
            assert!(!duplicate.work_completed);
            assert!(restarted.mark_work_completed(&admitted).unwrap());
        }

        let mut completed = open(root.path(), UtcMicros(5));
        let duplicate = completed
            .admit_with_receipt(&admitted, UtcMicros(6))
            .unwrap();
        assert!(duplicate.work_completed);
        assert!(!completed.mark_work_completed(&admitted).unwrap());
    }

    #[test]
    fn failed_completion_persistence_keeps_work_pending_in_memory() {
        let root = TestDir::new("ledger-work-completion-failure");
        let admitted = envelope(9, 5);
        let mut ledger = open(root.path(), UtcMicros(1));
        ledger.admit_with_receipt(&admitted, UtcMicros(2)).unwrap();

        let completions = completions_path(root.path());
        fs::remove_file(&completions).unwrap();
        fs::create_dir(&completions).unwrap();
        assert_eq!(
            ledger.mark_work_completed(&admitted),
            Err(HookAdmissionLedgerError::Io)
        );
        assert!(
            !ledger
                .admit_with_receipt(&admitted, UtcMicros(3))
                .unwrap()
                .work_completed,
            "failed completion fsync must not suppress in-process redrive"
        );

        fs::remove_dir(&completions).unwrap();
        assert!(ledger.mark_work_completed(&admitted).unwrap());
    }

    #[test]
    fn entries_beyond_the_age_bound_stop_suppressing_admission() {
        let root = TestDir::new("ledger");
        let mut ledger = open(root.path(), UtcMicros(1));
        ledger.admit(&envelope(9, 5), UtcMicros(2)).unwrap();
        let beyond = UtcMicros(2 + MAX_SPOOL_AGE_MICROS + 1);

        assert_eq!(
            ledger.admit(&envelope(9, 5), beyond).unwrap(),
            HookAdmissionDecisionV1::Admitted
        );
        assert_eq!(ledger.expire(UtcMicros(beyond.0 * 2)).unwrap(), 1);
        assert_eq!(ledger.live_records(), 0);
    }

    #[test]
    fn record_bound_evicts_oldest_and_stays_durable() {
        let root = TestDir::new("ledger");
        let limits = HookAdmissionLedgerLimitsV1 {
            max_records: 8,
            max_age_micros: MAX_SPOOL_AGE_MICROS,
        };
        let mut ledger =
            HookAdmissionLedgerV1::open(root.path(), HookHostV1::ClaudeCode, limits, UtcMicros(1))
                .unwrap()
                .0;
        for index in 1..=9u8 {
            assert_eq!(
                ledger
                    .admit(&envelope(index, 5), UtcMicros(i64::from(index) + 1))
                    .unwrap(),
                HookAdmissionDecisionV1::Admitted
            );
        }

        assert!(ledger.live_records() <= 8);
        // The newest identity is still deduplicated after eviction + reopen.
        drop(ledger);
        let mut reopened =
            HookAdmissionLedgerV1::open(root.path(), HookHostV1::ClaudeCode, limits, UtcMicros(20))
                .unwrap()
                .0;
        assert_eq!(
            reopened.admit(&envelope(9, 5), UtcMicros(21)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
    }

    #[test]
    fn a_corrupt_tail_is_truncated_without_losing_the_valid_prefix() {
        let root = TestDir::new("ledger");
        {
            let mut ledger = open(root.path(), UtcMicros(1));
            ledger.admit(&envelope(9, 5), UtcMicros(2)).unwrap();
        }
        let path = records_path(root.path());
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&[0xAB; RECORD_BYTES]);
        fs::write(&path, &bytes).unwrap();

        let (mut ledger, report) = HookAdmissionLedgerV1::open(
            root.path(),
            HookHostV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            UtcMicros(3),
        )
        .unwrap();

        assert_eq!(report.truncated_tail_bytes, RECORD_BYTES as u64);
        assert_eq!(report.live_records, 1);
        assert_eq!(
            ledger.admit(&envelope(9, 5), UtcMicros(4)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
    }
}
