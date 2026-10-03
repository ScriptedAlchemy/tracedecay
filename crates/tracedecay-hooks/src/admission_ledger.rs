//! Durable, bounded Hook V2 admission ledger and producer-work outbox.
//!
//! This is deliberately *not* an event store. For each already-authorized
//! envelope it persists the admission identity (`event_id`) plus a digest over
//! its canonical event material, so the daemon can answer three questions
//! across a restart:
//!
//! * has this exact native event already been admitted? (`ExactDuplicate`)
//! * has this identity already been admitted carrying *different* bytes?
//!   (`Conflict`)
//! * otherwise: this is a first admission.
//!
//! Admissions that owe producer work also carry the provider envelope until
//! that work completes, so a restart redrives it. It carries no session content
//! and no application state. The daemon is the sole writer; the ledger is
//! stored beside the transport spool in the same daemon-owned hook data root
//! and never touches a migrated database.
//!
//! Everything lives in one append-only log of checksummed frames. An admission
//! appends one frame holding its identity and, when it owes producer work, the
//! work, so a torn write can never leave one without the other; completing
//! that work appends a completion frame. Appends
//! carry no barrier of their own: the caller waits on the returned
//! [`HookAdmissionCommitV1`], and one sync of the log makes durable every frame
//! written before it, so concurrent admissions share a single commit. A torn
//! tail from a killed writer never had its commit acknowledged and is
//! truncated on open.
//!
//! Bounds (stated, not implied): at most
//! [`HookAdmissionLedgerLimitsV1::max_records`] live entries per host, nothing
//! older than [`HookAdmissionLedgerLimitsV1::max_age_micros`], and at most
//! [`MAX_SPOOL_BYTES_PER_HOST`] of pending work. Beyond the entry or age bound
//! the oldest entries are dropped with any work they still owed, so
//! idempotency converges within that window and no further, a replay older
//! than the window is admitted again rather than silently believed to be new
//! forever.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{UtcMicros, canonical_json_bytes, framed_log::checksum as frame_checksum};
use tracedecay_private_fs::FileLease;
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, atomic_write as shared_atomic_write, read_bounded as shared_read_bounded,
    sync_directory as shared_sync_directory, truncate_file as shared_truncate_file,
    validate_regular_or_missing as shared_validate_regular,
};

use crate::{
    HookEventEnvelopeV2, MAX_HOOK_PAYLOAD_BYTES, MAX_SPOOL_AGE_MICROS, MAX_SPOOL_BYTES_PER_HOST,
    MAX_SPOOL_RECORDS_PER_HOST,
};
use tracedecay_domain::NativeHostIdentityV1;

const LOG_MAGIC: &[u8; 4] = b"TDL2";
const LOG_FORMAT_VERSION: u16 = 2;
const HEADER_BYTES: usize = 6;
const IDENTITY_BYTES: usize = 16;
const DIGEST_BYTES: usize = 32;
const CHECKSUM_PREFIX_BYTES: usize = 8;
const FRAME_PREFIX_BYTES: usize = 4 + 1;
const FRAME_OVERHEAD_BYTES: usize = FRAME_PREFIX_BYTES + CHECKSUM_PREFIX_BYTES;
const ADMITTED_BODY_BYTES: usize = IDENTITY_BYTES + DIGEST_BYTES + 8;
const ADMITTED_FRAME_BYTES: usize = ADMITTED_BODY_BYTES + FRAME_OVERHEAD_BYTES;
const COMPLETED_FRAME_BYTES: usize = IDENTITY_BYTES + FRAME_OVERHEAD_BYTES;
const MAX_FRAME_BODY_BYTES: usize = ADMITTED_BODY_BYTES + MAX_HOOK_PAYLOAD_BYTES;
const FRAME_ADMITTED: u8 = 1;
const FRAME_WORK: u8 = 2;
const FRAME_COMPLETED: u8 = 3;
/// Superseded frames the log may carry before an append compacts it.
const COMPACTION_SLACK_BYTES: u64 = 256 * 1024;
const LOG_FILE: &str = "admissions.v2.log";
const LOCK_FILE: &str = "admissions.v1.lock";
/// The directory under a project's hook data root holding one admission
/// ledger per host.
pub const PROJECT_HOOK_ADMISSIONS_DIR: &str = "hook-v2-admissions";
/// The directory under a profile root holding one profile-scoped admission
/// ledger per host.
pub const PROFILE_HOOK_ADMISSIONS_DIR: &str = "hook-v2-profile-admissions";
/// Where producer work lived, per host, before the admission ledger owned it.
/// A project holding it is refused for reset with its ledgers.
pub const PRE_LEDGER_PENDING_WORK_DIR: &str = "hook-v2-pending-work";
/// Members of the pre-log ledger shape, which this binary refuses for reset.
const PRE_LOG_MEMBERS: [&str; 2] = ["admissions.v1.bin", "admission-work-completions.v1.json"];

/// Hook admission state under the data root `root` (a profile root or one
/// project shard) that this binary refuses for reset, read from what is on
/// disk: each host ledger that still holds the pre-log shape, and each host's
/// pre-ledger pending-work spool. A daemon that never opened a ledger still
/// reports it, so the reset census is right from the first status read.
pub fn hook_admission_reset_required_roots(root: &Path) -> Vec<PathBuf> {
    let hosts = |directory: &str| {
        fs::read_dir(root.join(directory))
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>()
    };
    let holds_pre_log = |ledger: &PathBuf| {
        PRE_LOG_MEMBERS
            .iter()
            .any(|member| fs::symlink_metadata(ledger.join(member)).is_ok())
    };
    let mut roots = [PROFILE_HOOK_ADMISSIONS_DIR, PROJECT_HOOK_ADMISSIONS_DIR]
        .into_iter()
        .flat_map(hosts)
        .filter(holds_pre_log)
        .chain(hosts(PRE_LEDGER_PENDING_WORK_DIR))
        .collect::<Vec<_>>();
    roots.sort();
    roots
}
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

    /// Live frames at their bound, twice over for superseded frames awaiting
    /// compaction, plus the compaction slack.
    fn max_log_bytes(self) -> usize {
        let live = (self.max_records as usize)
            .saturating_mul(ADMITTED_FRAME_BYTES + COMPLETED_FRAME_BYTES + FRAME_OVERHEAD_BYTES)
            .saturating_add(MAX_SPOOL_BYTES_PER_HOST as usize);
        HEADER_BYTES
            .saturating_add(live.saturating_mul(2))
            .saturating_add(COMPACTION_SLACK_BYTES as usize)
            .saturating_add(MAX_FRAME_BODY_BYTES + FRAME_OVERHEAD_BYTES)
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

/// Ledger decision plus the stable order assigned to its entry.
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
    #[error("hook admission ledger record is not canonically decodable")]
    RecordUndecodable,
    #[error("hook admission ledger identity is invalid")]
    InvalidIdentity,
    #[error("hook admission ledger is busy in another daemon")]
    Busy,
    #[error("hook admission ledger holds its bound of pending producer work")]
    WorkCapacityExceeded,
    #[error("hook admission ledger holds a pre-log shape this binary does not open")]
    ResetRequired,
}

/// Bounded recovery report for an opened ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookAdmissionLedgerOpenReportV1 {
    pub live_records: u32,
    pub pending_work: u32,
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

#[derive(Debug)]
struct PendingWork {
    envelope: HookEventEnvelopeV2,
    frame_bytes: u64,
}

/// The log file shared by every commit issued against one ledger generation.
#[derive(Debug)]
struct LedgerLogV1 {
    state: Mutex<LedgerLogStateV1>,
    /// Bytes written to the current generation's file; a sync that starts
    /// after this load makes all of them durable.
    written: AtomicU64,
    /// A failed append or sync leaves memory ahead of disk: the owner must
    /// reopen the ledger from its log.
    failed: AtomicBool,
}

#[derive(Debug)]
struct LedgerLogStateV1 {
    file: Arc<File>,
    generation: u64,
    synced_through: u64,
}

/// One admission's durability wait. Frames written before a sync are covered
/// by it, so the first waiter syncs for every frame staged ahead of it and the
/// rest return without a barrier of their own.
#[derive(Debug)]
#[must_use = "a staged ledger frame is durable only once its commit is awaited"]
pub struct HookAdmissionCommitV1 {
    log: Arc<LedgerLogV1>,
    generation: u64,
    end: u64,
}

impl HookAdmissionCommitV1 {
    #[tracing::instrument(name = "hooks.admission.commit", level = "trace", skip_all)]
    pub fn wait(self) -> Result<(), HookAdmissionLedgerError> {
        let mut state = self
            .log
            .state
            .lock()
            .map_err(|_| HookAdmissionLedgerError::Io)?;
        if self.log.failed.load(Ordering::Acquire) {
            return Err(HookAdmissionLedgerError::Io);
        }
        // A compaction republished every staged frame through a synced
        // replacement before it bumped the generation.
        if state.generation != self.generation || state.synced_through >= self.end {
            return Ok(());
        }
        let through = self.log.written.load(Ordering::Acquire);
        #[cfg(test)]
        tests::COMMIT_SYNCS.with(|syncs| syncs.set(syncs.get() + 1));
        let synced = {
            let _span = tracing::trace_span!("hooks.admission.fsync.commit").entered();
            state.file.sync_data()
        };
        if synced.is_err() {
            self.log.failed.store(true, Ordering::Release);
            return Err(HookAdmissionLedgerError::Io);
        }
        state.synced_through = through;
        Ok(())
    }
}

/// A staged admission: its decision now, its durability once `commit` is
/// awaited. Nothing may be acknowledged before that wait succeeds.
#[derive(Debug)]
pub struct HookAdmissionStagedV1 {
    pub receipt: HookAdmissionLedgerReceiptV1,
    pub commit: HookAdmissionCommitV1,
}

/// Digest over canonical native-event material. A provider retry observes the
/// same stable event at a later local instant, so `observed_at` is not part of
/// admission identity. Every authoritative scope, binding, ordering, and event
/// field remains covered; differences there are genuine producer conflicts.
pub fn hook_admission_digest(
    envelope: &HookEventEnvelopeV2,
) -> Result<[u8; DIGEST_BYTES], HookAdmissionLedgerError> {
    let mut identity = envelope.clone();
    identity.observed_at = UtcMicros(0);
    let bytes =
        canonical_json_bytes(&identity).map_err(|_| HookAdmissionLedgerError::RecordUnencodable)?;
    Ok(frame_checksum(&bytes))
}

/// The daemon-owned, per-host admission ledger.
#[derive(Debug)]
pub struct HookAdmissionLedgerV1 {
    root: PathBuf,
    _writer_lock: FileLease,
    host: NativeHostIdentityV1,
    limits: HookAdmissionLedgerLimitsV1,
    entries: BTreeMap<[u8; IDENTITY_BYTES], LedgerEntry>,
    completed_work: BTreeSet<[u8; IDENTITY_BYTES]>,
    pending_work: BTreeMap<[u8; IDENTITY_BYTES], PendingWork>,
    pending_work_bytes: u64,
    next_order: u64,
    log: Arc<LedgerLogV1>,
    file: Arc<File>,
    generation: u64,
    written: u64,
}

impl HookAdmissionLedgerV1 {
    /// Open (and bounded-recover) the ledger for one host. A root still
    /// holding the pre-log shape is refused with
    /// [`HookAdmissionLedgerError::ResetRequired`] and left untouched.
    #[tracing::instrument(name = "hooks.admission.open", level = "trace", skip_all)]
    pub fn open(
        root: impl Into<PathBuf>,
        host: NativeHostIdentityV1,
        limits: HookAdmissionLedgerLimitsV1,
        now: UtcMicros,
    ) -> Result<(Self, HookAdmissionLedgerOpenReportV1), HookAdmissionLedgerError> {
        limits.validate()?;
        let root = root.into();
        ensure_root(&root)?;
        let writer_lock = acquire_writer_lock(&root)?;
        for member in PRE_LOG_MEMBERS {
            if validate_member(&root.join(member))? {
                return Err(HookAdmissionLedgerError::ResetRequired);
            }
        }
        let path = log_path(&root);
        let log_exists = validate_member(&path)?;
        let mut headerless = !log_exists;
        let (frames, truncated_tail_bytes) = if log_exists {
            let bytes = read_bounded(&path, limits.max_log_bytes())?.unwrap_or_default();
            let (frames, valid_end) = scan_log(&bytes)?;
            headerless = valid_end < HEADER_BYTES;
            let torn = (bytes.len() - valid_end) as u64;
            if torn > 0 {
                shared_truncate_file(&path, valid_end as u64)
                    .map_err(|_| HookAdmissionLedgerError::Io)?;
            }
            (frames, torn)
        } else {
            (Vec::new(), 0)
        };
        let file = Arc::new(open_log_file(&path, log_exists)?);
        let mut ledger = Self {
            root,
            _writer_lock: writer_lock,
            host,
            limits,
            entries: BTreeMap::new(),
            completed_work: BTreeSet::new(),
            pending_work: BTreeMap::new(),
            pending_work_bytes: 0,
            next_order: 0,
            log: Arc::new(LedgerLogV1 {
                state: Mutex::new(LedgerLogStateV1 {
                    file: Arc::clone(&file),
                    generation: 0,
                    synced_through: 0,
                }),
                written: AtomicU64::new(0),
                failed: AtomicBool::new(false),
            }),
            file,
            generation: 0,
            written: 0,
        };
        let (dropped_expired_records, admitted_frames) = ledger.replay(frames, now)?;
        let dropped_overflow_records = ledger.trim_to(limits.max_records as usize);
        if headerless
            || dropped_expired_records > 0
            || dropped_overflow_records > 0
            || (ledger.entries.len() as u64) < admitted_frames
        {
            ledger.rewrite()?;
        } else {
            ledger.adopt_log()?;
        }
        let report = HookAdmissionLedgerOpenReportV1 {
            live_records: ledger.entries.len() as u32,
            pending_work: ledger.pending_work.len() as u32,
            dropped_expired_records,
            dropped_overflow_records,
            truncated_tail_bytes,
        };

        Ok((ledger, report))
    }

    pub fn host(&self) -> NativeHostIdentityV1 {
        self.host
    }

    pub fn live_records(&self) -> u32 {
        self.entries.len() as u32
    }

    /// A failed append or sync left memory ahead of the log; drop this handle
    /// and reopen from disk.
    pub fn needs_reopen(&self) -> bool {
        self.log.failed.load(Ordering::Acquire)
    }

    /// Stage one admission attempt. `work` is the provider envelope to redrive
    /// until [`Self::stage_work_completion`]; it is recorded with a first
    /// admission, or with an exact duplicate whose work is neither pending nor
    /// complete. The decision is final now, but nothing may be acknowledged
    /// before the returned commit is awaited, and a duplicate of an admission
    /// still in flight waits for that admission's frames too.
    #[tracing::instrument(name = "hooks.admission.stage", level = "trace", skip_all)]
    pub fn stage_admission(
        &mut self,
        envelope: &HookEventEnvelopeV2,
        work: Option<&HookEventEnvelopeV2>,
        now: UtcMicros,
    ) -> Result<HookAdmissionStagedV1, HookAdmissionLedgerError> {
        self.ensure_writable()?;
        let identity = envelope.event_id;
        if identity == [0; IDENTITY_BYTES] {
            return Err(HookAdmissionLedgerError::InvalidIdentity);
        }
        let digest = hook_admission_digest(envelope)?;
        let work = work
            .map(|work| encode_work(work).map(|encoded| (work, encoded)))
            .transpose()?;
        if let Some(existing) = self.entries.get(&identity).copied() {
            if is_expired(existing.admitted_at, now, self.limits.max_age_micros) {
                self.forget(&identity);
            } else if existing.digest == digest {
                return self.stage_duplicate(identity, existing.order, work);
            } else {
                let work_completed = self.completed_work.contains(&identity);
                return Ok(self.staged(
                    HookAdmissionDecisionV1::Conflict,
                    existing.order,
                    work_completed,
                ));
            }
        }
        if let Some((_, encoded)) = work.as_ref() {
            self.ensure_work_capacity(encoded)?;
        }
        if self.entries.len() as u32 >= self.limits.max_records {
            self.trim_to((self.limits.max_records as usize).saturating_sub(1));
            self.rewrite()?;
        }
        let mut frames = Vec::new();
        encode_frame(
            FRAME_ADMITTED,
            &encode_admitted_body(
                identity,
                digest,
                now,
                work.as_ref().map_or(&[][..], |(_, encoded)| encoded),
            ),
            &mut frames,
        )?;

        self.append(&frames)?;
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
        if let Some((work, _)) = work {
            self.insert_pending_work(identity, work.clone())?;
        }

        self.compact_if_sparse()?;
        Ok(self.staged(HookAdmissionDecisionV1::Admitted, order, false))
    }

    /// Stage completion of an admission's producer work. `None` means it was
    /// already complete. Exact-duplicate redrives stay pending on disk until
    /// the returned commit is awaited.
    pub fn stage_work_completion(
        &mut self,
        envelope: &HookEventEnvelopeV2,
    ) -> Result<Option<HookAdmissionCommitV1>, HookAdmissionLedgerError> {
        self.ensure_writable()?;
        let identity = envelope.event_id;
        let Some(entry) = self.entries.get(&identity) else {
            return Err(HookAdmissionLedgerError::InvalidIdentity);
        };
        if entry.digest != hook_admission_digest(envelope)? {
            return Err(HookAdmissionLedgerError::InvalidIdentity);
        }
        if self.completed_work.contains(&identity) {
            return Ok(None);
        }
        let mut frames = Vec::with_capacity(COMPLETED_FRAME_BYTES);
        encode_frame(FRAME_COMPLETED, &identity, &mut frames)?;
        self.append(&frames)?;
        self.remove_pending_work(&identity);
        self.completed_work.insert(identity);
        self.compact_if_sparse()?;
        Ok(Some(self.commit_ticket()))
    }

    /// Provider envelopes whose producer work is still owed, oldest admission
    /// first. Work older than the age bound is dropped with its entry here:
    /// that is its terminal state, not another redrive.
    pub fn pending_work_envelopes(
        &mut self,
        now: UtcMicros,
    ) -> Result<Vec<HookEventEnvelopeV2>, HookAdmissionLedgerError> {
        self.expire(now)?;
        let mut pending = self
            .pending_work
            .iter()
            .filter_map(|(identity, work)| {
                self.entries
                    .get(identity)
                    .map(|entry| (entry.order, work.envelope.clone()))
            })
            .collect::<Vec<_>>();
        pending.sort_unstable_by_key(|(order, _)| *order);
        Ok(pending.into_iter().map(|(_, envelope)| envelope).collect())
    }

    /// Drop entries older than the age bound, with any work they still owed.
    /// Returns how many were removed.
    #[tracing::instrument(name = "hooks.admission.expire", level = "trace", skip_all)]
    pub fn expire(&mut self, now: UtcMicros) -> Result<u32, HookAdmissionLedgerError> {
        let max_age = self.limits.max_age_micros;
        let expired = self
            .entries
            .iter()
            .filter(|(_, entry)| is_expired(entry.admitted_at, now, max_age))
            .map(|(identity, _)| *identity)
            .collect::<Vec<_>>();
        for identity in &expired {
            self.forget(identity);
        }
        let removed = expired.len() as u32;
        if removed > 0 {
            self.rewrite()?;
        }

        Ok(removed)
    }

    /// Rebuild memory from the log's frames in order. Returns the expired
    /// admissions dropped and the live admission frames seen.
    fn replay(
        &mut self,
        frames: Vec<LogFrame>,
        now: UtcMicros,
    ) -> Result<(u32, u64), HookAdmissionLedgerError> {
        let mut dropped_expired_records = 0u32;
        let mut admitted_frames = 0u64;
        for frame in frames {
            match frame {
                LogFrame::Admitted {
                    identity,
                    digest,
                    admitted_at,
                    work,
                } => {
                    self.forget(&identity);
                    if is_expired(admitted_at, now, self.limits.max_age_micros) {
                        dropped_expired_records = dropped_expired_records.saturating_add(1);
                        continue;
                    }
                    admitted_frames = admitted_frames.saturating_add(1);
                    let order = self.next_order;
                    self.next_order = self.next_order.saturating_add(1);
                    self.entries.insert(
                        identity,
                        LedgerEntry {
                            digest,
                            admitted_at,
                            order,
                        },
                    );
                    if let Some(work) = work {
                        self.insert_pending_work(identity, work)?;
                    }
                }
                LogFrame::Work { identity, envelope } => {
                    if self.entries.contains_key(&identity)
                        && !self.completed_work.contains(&identity)
                    {
                        self.insert_pending_work(identity, envelope)?;
                    }
                }
                LogFrame::Completed { identity } => {
                    if self.entries.contains_key(&identity) {
                        self.remove_pending_work(&identity);
                        self.completed_work.insert(identity);
                    }
                }
            }
        }
        Ok((dropped_expired_records, admitted_frames))
    }

    /// Continue the existing log as it stands. A killed predecessor may have
    /// written frames it never synced; they are part of this ledger's state,
    /// so they are made durable before any duplicate is acknowledged against
    /// them.
    fn adopt_log(&mut self) -> Result<(), HookAdmissionLedgerError> {
        self.file
            .sync_data()
            .map_err(|_| HookAdmissionLedgerError::Io)?;
        let len = self
            .file
            .metadata()
            .map_err(|_| HookAdmissionLedgerError::Io)?
            .len();
        self.written = len;
        self.log.written.store(len, Ordering::Release);
        self.log
            .state
            .lock()
            .map_err(|_| HookAdmissionLedgerError::Io)?
            .synced_through = len;
        Ok(())
    }

    /// An exact duplicate adopts the work it carries when its identity owes
    /// none yet, so a redelivery converges on one pending record.
    fn stage_duplicate(
        &mut self,
        identity: [u8; IDENTITY_BYTES],
        order: u64,
        work: Option<(&HookEventEnvelopeV2, Vec<u8>)>,
    ) -> Result<HookAdmissionStagedV1, HookAdmissionLedgerError> {
        let work_completed = self.completed_work.contains(&identity);
        if let Some((work, encoded)) = work
            && !work_completed
            && !self.pending_work.contains_key(&identity)
        {
            self.ensure_work_capacity(&encoded)?;
            let mut frames = Vec::new();
            encode_frame(
                FRAME_WORK,
                &encode_work_body(identity, &encoded),
                &mut frames,
            )?;
            self.append(&frames)?;
            self.insert_pending_work(identity, work.clone())?;
        }
        Ok(self.staged(
            HookAdmissionDecisionV1::ExactDuplicate,
            order,
            work_completed,
        ))
    }

    fn staged(
        &self,
        decision: HookAdmissionDecisionV1,
        order: u64,
        work_completed: bool,
    ) -> HookAdmissionStagedV1 {
        HookAdmissionStagedV1 {
            receipt: HookAdmissionLedgerReceiptV1 {
                decision,
                order,
                work_completed,
            },
            commit: self.commit_ticket(),
        }
    }

    fn commit_ticket(&self) -> HookAdmissionCommitV1 {
        HookAdmissionCommitV1 {
            log: Arc::clone(&self.log),
            generation: self.generation,
            end: self.written,
        }
    }

    fn ensure_writable(&self) -> Result<(), HookAdmissionLedgerError> {
        if self.needs_reopen() {
            return Err(HookAdmissionLedgerError::Io);
        }
        Ok(())
    }

    fn ensure_work_capacity(&self, work: &[u8]) -> Result<(), HookAdmissionLedgerError> {
        if self
            .pending_work_bytes
            .saturating_add(work_frame_bytes(work))
            > MAX_SPOOL_BYTES_PER_HOST
        {
            return Err(HookAdmissionLedgerError::WorkCapacityExceeded);
        }
        Ok(())
    }

    /// Append frames without a barrier. A failed or short write poisons the
    /// handle: later frames must never land behind a torn one.
    fn append(&mut self, frames: &[u8]) -> Result<(), HookAdmissionLedgerError> {
        let written = (|| {
            let mut file = &*self.file;
            file.seek(SeekFrom::Start(self.written))?;
            file.write_all(frames)
        })();
        if written.is_err() {
            self.log.failed.store(true, Ordering::Release);
            return Err(HookAdmissionLedgerError::Io);
        }
        self.written = self.written.saturating_add(frames.len() as u64);
        self.log.written.store(self.written, Ordering::Release);
        Ok(())
    }

    fn insert_pending_work(
        &mut self,
        identity: [u8; IDENTITY_BYTES],
        envelope: HookEventEnvelopeV2,
    ) -> Result<(), HookAdmissionLedgerError> {
        let frame_bytes = work_frame_bytes(&encode_work(&envelope)?);
        if let Some(previous) = self.pending_work.insert(
            identity,
            PendingWork {
                envelope,
                frame_bytes,
            },
        ) {
            self.pending_work_bytes = self.pending_work_bytes.saturating_sub(previous.frame_bytes);
        }
        self.pending_work_bytes = self.pending_work_bytes.saturating_add(frame_bytes);
        Ok(())
    }

    fn remove_pending_work(&mut self, identity: &[u8; IDENTITY_BYTES]) {
        if let Some(work) = self.pending_work.remove(identity) {
            self.pending_work_bytes = self.pending_work_bytes.saturating_sub(work.frame_bytes);
        }
    }

    fn forget(&mut self, identity: &[u8; IDENTITY_BYTES]) {
        self.entries.remove(identity);
        self.completed_work.remove(identity);
        if self.pending_work.contains_key(identity) {
            tracing::debug!(
                event = "hook_admission_pending_work_dropped",
                host = self.host.hook_key(),
                "hook admission pending producer work left the retained window"
            );
        }
        self.remove_pending_work(identity);
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
            self.forget(&identity);
        }
        dropped as u32
    }

    fn live_bytes(&self) -> u64 {
        HEADER_BYTES as u64
            + self.entries.len() as u64 * ADMITTED_FRAME_BYTES as u64
            + self.completed_work.len() as u64 * COMPLETED_FRAME_BYTES as u64
            + self.pending_work_bytes
    }

    fn compact_if_sparse(&mut self) -> Result<(), HookAdmissionLedgerError> {
        let live = self.live_bytes();
        if self.written
            > live
                .saturating_mul(2)
                .saturating_add(COMPACTION_SLACK_BYTES)
        {
            self.rewrite()?;
        }
        Ok(())
    }

    /// Durably replace the log with exactly the live state, including frames
    /// staged but not yet committed, then retire the previous generation so
    /// its outstanding commits return without syncing.
    #[tracing::instrument(name = "hooks.admission.rewrite", level = "trace", skip_all)]
    fn rewrite(&mut self) -> Result<(), HookAdmissionLedgerError> {
        let mut ordered = self
            .entries
            .iter()
            .map(|(identity, entry)| (entry.order, *identity, *entry))
            .collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|(order, _, _)| *order);
        let mut bytes = Vec::with_capacity(self.live_bytes() as usize);
        bytes.extend_from_slice(&log_header());
        for (_, identity, entry) in &ordered {
            let work = self
                .pending_work
                .get(identity)
                .map(|work| encode_work(&work.envelope))
                .transpose()?;
            encode_frame(
                FRAME_ADMITTED,
                &encode_admitted_body(
                    *identity,
                    entry.digest,
                    entry.admitted_at,
                    work.as_deref().unwrap_or_default(),
                ),
                &mut bytes,
            )?;
            if self.completed_work.contains(identity) {
                encode_frame(FRAME_COMPLETED, identity, &mut bytes)?;
            }
        }
        for (index, (_, identity, _)) in ordered.iter().enumerate() {
            if let Some(entry) = self.entries.get_mut(identity) {
                entry.order = index as u64;
            }
        }
        self.next_order = ordered.len() as u64;

        let path = log_path(&self.root);
        let mut state = self
            .log
            .state
            .lock()
            .map_err(|_| HookAdmissionLedgerError::Io)?;
        {
            let _span = tracing::trace_span!("hooks.admission.fsync.rewrite").entered();
            {
                shared_atomic_write(&path, "hook-admissions", &bytes, DIRECTORY_POLICY)
                    .map_err(|_| HookAdmissionLedgerError::Io)
            }
        }?;
        let file = Arc::new(open_log_file(&path, true)?);
        let len = bytes.len() as u64;
        self.generation = self.generation.saturating_add(1);
        self.file = Arc::clone(&file);
        self.written = len;
        self.log.written.store(len, Ordering::Release);
        state.file = file;
        state.generation = self.generation;
        state.synced_through = len;
        Ok(())
    }
}

#[derive(Debug)]
enum LogFrame {
    Admitted {
        identity: [u8; IDENTITY_BYTES],
        digest: [u8; DIGEST_BYTES],
        admitted_at: UtcMicros,
        work: Option<HookEventEnvelopeV2>,
    },
    Work {
        identity: [u8; IDENTITY_BYTES],
        envelope: HookEventEnvelopeV2,
    },
    Completed {
        identity: [u8; IDENTITY_BYTES],
    },
}

fn encode_frame(kind: u8, body: &[u8], out: &mut Vec<u8>) -> Result<(), HookAdmissionLedgerError> {
    let length =
        u32::try_from(body.len()).map_err(|_| HookAdmissionLedgerError::RecordUnencodable)?;
    let start = out.len();
    out.extend_from_slice(&length.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(body);
    let checksum = frame_checksum(&out[start..]);
    out.extend_from_slice(&checksum[..CHECKSUM_PREFIX_BYTES]);
    Ok(())
}

fn encode_admitted_body(
    identity: [u8; IDENTITY_BYTES],
    digest: [u8; DIGEST_BYTES],
    admitted_at: UtcMicros,
    work: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(ADMITTED_BODY_BYTES + work.len());
    body.extend_from_slice(&identity);
    body.extend_from_slice(&digest);
    body.extend_from_slice(&admitted_at.0.to_le_bytes());
    body.extend_from_slice(work);
    body
}

fn encode_work_body(identity: [u8; IDENTITY_BYTES], work: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(IDENTITY_BYTES + work.len());
    body.extend_from_slice(&identity);
    body.extend_from_slice(work);
    body
}

fn encode_work(envelope: &HookEventEnvelopeV2) -> Result<Vec<u8>, HookAdmissionLedgerError> {
    let encoded =
        canonical_json_bytes(envelope).map_err(|_| HookAdmissionLedgerError::RecordUnencodable)?;
    if encoded.is_empty() || encoded.len() > MAX_HOOK_PAYLOAD_BYTES {
        return Err(HookAdmissionLedgerError::RecordUnencodable);
    }
    Ok(encoded)
}

fn decode_work(bytes: &[u8]) -> Result<Option<HookEventEnvelopeV2>, HookAdmissionLedgerError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|_| HookAdmissionLedgerError::RecordUndecodable)
}

fn work_frame_bytes(work: &[u8]) -> u64 {
    (IDENTITY_BYTES + work.len() + FRAME_OVERHEAD_BYTES) as u64
}

fn identity_at(body: &[u8]) -> [u8; IDENTITY_BYTES] {
    let mut identity = [0u8; IDENTITY_BYTES];
    identity.copy_from_slice(&body[..IDENTITY_BYTES]);
    identity
}

/// Scan the log. Returns every intact frame in file order and where the
/// intact prefix ends; the rest is a torn or foreign tail. A checksummed frame
/// that does not decode is a format error, not a tail.
fn scan_log(bytes: &[u8]) -> Result<(Vec<LogFrame>, usize), HookAdmissionLedgerError> {
    if bytes.len() < HEADER_BYTES || bytes[..HEADER_BYTES] != log_header() {
        return Ok((Vec::new(), 0));
    }
    let mut frames = Vec::new();
    let mut offset = HEADER_BYTES;
    while let Some(prefix) = bytes.get(offset..offset + FRAME_PREFIX_BYTES) {
        let length = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
        let kind = prefix[4];
        if length > MAX_FRAME_BODY_BYTES {
            break;
        }
        let end = offset + FRAME_PREFIX_BYTES + length;
        let Some(checksum) = bytes.get(end..end + CHECKSUM_PREFIX_BYTES) else {
            break;
        };
        if frame_checksum(&bytes[offset..end])[..CHECKSUM_PREFIX_BYTES] != *checksum {
            break;
        }
        let body = &bytes[offset + FRAME_PREFIX_BYTES..end];
        frames.push(match (kind, body.len()) {
            (FRAME_ADMITTED, length) if length >= ADMITTED_BODY_BYTES => {
                let mut digest = [0u8; DIGEST_BYTES];
                digest.copy_from_slice(&body[IDENTITY_BYTES..IDENTITY_BYTES + DIGEST_BYTES]);
                let mut admitted = [0u8; 8];
                admitted.copy_from_slice(&body[IDENTITY_BYTES + DIGEST_BYTES..ADMITTED_BODY_BYTES]);
                LogFrame::Admitted {
                    identity: identity_at(body),
                    digest,
                    admitted_at: UtcMicros(i64::from_le_bytes(admitted)),
                    work: decode_work(&body[ADMITTED_BODY_BYTES..])?,
                }
            }
            (FRAME_WORK, length) if length > IDENTITY_BYTES => LogFrame::Work {
                identity: identity_at(body),
                envelope: decode_work(&body[IDENTITY_BYTES..])?
                    .ok_or(HookAdmissionLedgerError::RecordUndecodable)?,
            },
            (FRAME_COMPLETED, IDENTITY_BYTES) => LogFrame::Completed {
                identity: identity_at(body),
            },
            _ => return Err(HookAdmissionLedgerError::RecordUndecodable),
        });
        offset = end + CHECKSUM_PREFIX_BYTES;
    }
    Ok((frames, offset))
}

fn log_path(root: &Path) -> PathBuf {
    root.join(LOG_FILE)
}

fn log_header() -> [u8; HEADER_BYTES] {
    let mut header = [0u8; HEADER_BYTES];
    header[..4].copy_from_slice(LOG_MAGIC);
    header[4..].copy_from_slice(&LOG_FORMAT_VERSION.to_le_bytes());
    header
}

/// Opens the log for positioned appends and syncs. Read+write rather than
/// append-only: Windows flushes only through a handle with write access.
fn open_log_file(path: &Path, exists: bool) -> Result<File, HookAdmissionLedgerError> {
    if !exists {
        shared_atomic_write(path, "hook-admissions", &log_header(), DIRECTORY_POLICY)
            .map_err(|_| HookAdmissionLedgerError::Io)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| HookAdmissionLedgerError::Io)
}

fn acquire_writer_lock(root: &Path) -> Result<FileLease, HookAdmissionLedgerError> {
    let path = root.join(LOCK_FILE);
    shared_validate_regular(&path).map_err(|_| HookAdmissionLedgerError::UnsafePath)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|_| HookAdmissionLedgerError::Io)?;
    match file.try_lock() {
        Ok(()) => Ok(FileLease::held(file, "hooks.admission.writer")),
        Err(std::fs::TryLockError::WouldBlock) => Err(HookAdmissionLedgerError::Busy),
        Err(std::fs::TryLockError::Error(_)) => Err(HookAdmissionLedgerError::Io),
    }
}

fn validate_member(path: &Path) -> Result<bool, HookAdmissionLedgerError> {
    shared_validate_regular(path).map_err(|_| HookAdmissionLedgerError::UnsafePath)
}

fn is_expired(admitted_at: UtcMicros, now: UtcMicros, max_age_micros: i64) -> bool {
    now.0.saturating_sub(admitted_at.0) > max_age_micros
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
    use std::cell::Cell;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};

    thread_local! {
        pub(super) static COMMIT_SYNCS: Cell<u32> = const { Cell::new(0) };
    }

    /// Stage-and-wait conveniences: the daemon stages under its ledger lock
    /// and waits outside it, but a single-threaded test does both at once.
    trait DurableAdmission {
        fn admit_with_receipt(
            &mut self,
            envelope: &HookEventEnvelopeV2,
            now: UtcMicros,
        ) -> Result<HookAdmissionLedgerReceiptV1, HookAdmissionLedgerError>;

        fn admit(
            &mut self,
            envelope: &HookEventEnvelopeV2,
            now: UtcMicros,
        ) -> Result<HookAdmissionDecisionV1, HookAdmissionLedgerError> {
            self.admit_with_receipt(envelope, now)
                .map(|receipt| receipt.decision)
        }

        fn mark_work_completed(
            &mut self,
            envelope: &HookEventEnvelopeV2,
        ) -> Result<bool, HookAdmissionLedgerError>;
    }

    impl DurableAdmission for HookAdmissionLedgerV1 {
        fn admit_with_receipt(
            &mut self,
            envelope: &HookEventEnvelopeV2,
            now: UtcMicros,
        ) -> Result<HookAdmissionLedgerReceiptV1, HookAdmissionLedgerError> {
            let staged = self.stage_admission(envelope, None, now)?;
            staged.commit.wait()?;
            Ok(staged.receipt)
        }

        fn mark_work_completed(
            &mut self,
            envelope: &HookEventEnvelopeV2,
        ) -> Result<bool, HookAdmissionLedgerError> {
            match self.stage_work_completion(envelope)? {
                Some(commit) => commit.wait().map(|()| true),
                None => Ok(false),
            }
        }
    }

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
            producer: NativeHostIdentityV1::ClaudeCode,
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
            NativeHostIdentityV1::ClaudeCode,
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
    fn same_native_event_observed_again_is_an_exact_duplicate() {
        let root = TestDir::new("ledger-native-redelivery");
        let mut ledger = open(root.path(), UtcMicros(1));
        let admitted = envelope(9, 5);
        let mut redelivered = admitted.clone();
        redelivered.observed_at = UtcMicros(12);

        assert_eq!(
            ledger.admit(&admitted, UtcMicros(2)).unwrap(),
            HookAdmissionDecisionV1::Admitted
        );
        assert_eq!(
            ledger.admit(&redelivered, UtcMicros(3)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
        assert!(ledger.mark_work_completed(&redelivered).unwrap());
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
                        NativeHostIdentityV1::ClaudeCode,
                        HookAdmissionLedgerLimitsV1::stock(),
                        UtcMicros(2),
                    ),
                    Err(HookAdmissionLedgerError::Busy)
                )),
                "released" => {
                    HookAdmissionLedgerV1::open(
                        &root,
                        NativeHostIdentityV1::ClaudeCode,
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
            let first = ledger
                .stage_admission(&admitted, Some(&admitted), UtcMicros(2))
                .unwrap();
            first.commit.wait().unwrap();
            assert!(!first.receipt.work_completed);
        }

        {
            let mut restarted = open(root.path(), UtcMicros(3));
            assert_eq!(
                restarted.pending_work_envelopes(UtcMicros(3)).unwrap(),
                vec![admitted.clone()]
            );
            let duplicate = restarted
                .admit_with_receipt(&admitted, UtcMicros(4))
                .unwrap();
            assert_eq!(duplicate.decision, HookAdmissionDecisionV1::ExactDuplicate);
            assert!(!duplicate.work_completed);
            assert!(restarted.mark_work_completed(&admitted).unwrap());
        }

        let mut completed = open(root.path(), UtcMicros(5));
        assert!(
            completed
                .pending_work_envelopes(UtcMicros(5))
                .unwrap()
                .is_empty()
        );
        let duplicate = completed
            .admit_with_receipt(&admitted, UtcMicros(6))
            .unwrap();
        assert!(duplicate.work_completed);
        assert!(!completed.mark_work_completed(&admitted).unwrap());
    }

    #[test]
    fn a_failed_log_refuses_further_writes_until_reopened() {
        let root = TestDir::new("ledger-failed-log");
        let admitted = envelope(9, 5);
        let mut ledger = open(root.path(), UtcMicros(1));
        ledger.admit(&admitted, UtcMicros(2)).unwrap();

        ledger.log.failed.store(true, Ordering::Release);
        assert!(ledger.needs_reopen());
        assert_eq!(
            ledger.admit(&envelope(10, 5), UtcMicros(3)),
            Err(HookAdmissionLedgerError::Io)
        );
        drop(ledger);

        let mut reopened = open(root.path(), UtcMicros(4));
        assert!(!reopened.needs_reopen());
        assert_eq!(
            reopened.admit(&admitted, UtcMicros(5)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
        assert_eq!(
            reopened.admit(&envelope(10, 5), UtcMicros(6)).unwrap(),
            HookAdmissionDecisionV1::Admitted
        );
    }

    #[test]
    fn expired_pending_work_ends_terminally_instead_of_redriving() {
        let root = TestDir::new("ledger-expired-work");
        let admitted = envelope(9, 5);
        let beyond = UtcMicros(2 + MAX_SPOOL_AGE_MICROS + 1);
        {
            let mut ledger = open(root.path(), UtcMicros(1));
            let staged = ledger
                .stage_admission(&admitted, Some(&admitted), UtcMicros(2))
                .unwrap();
            staged.commit.wait().unwrap();
            assert_eq!(
                ledger.pending_work_envelopes(UtcMicros(3)).unwrap(),
                vec![admitted.clone()]
            );
            assert!(ledger.pending_work_envelopes(beyond).unwrap().is_empty());
        }

        let (mut reopened, report) = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            beyond,
        )
        .unwrap();
        assert_eq!(report.live_records, 0);
        assert_eq!(report.pending_work, 0);
        assert!(reopened.pending_work_envelopes(beyond).unwrap().is_empty());
    }

    #[test]
    fn a_waiter_behind_an_earlier_sync_shares_it() {
        let root = TestDir::new("ledger-group-commit");
        let mut ledger = open(root.path(), UtcMicros(1));
        let first = ledger
            .stage_admission(&envelope(9, 5), None, UtcMicros(2))
            .unwrap();
        let second = ledger
            .stage_admission(&envelope(10, 5), None, UtcMicros(3))
            .unwrap();
        let before = COMMIT_SYNCS.with(Cell::get);

        first.commit.wait().unwrap();
        second.commit.wait().unwrap();

        assert_eq!(COMMIT_SYNCS.with(Cell::get) - before, 1);
        drop(ledger);
        let mut reopened = open(root.path(), UtcMicros(4));
        for id in [9, 10] {
            assert_eq!(
                reopened.admit(&envelope(id, 5), UtcMicros(5)).unwrap(),
                HookAdmissionDecisionV1::ExactDuplicate
            );
        }
    }

    #[test]
    fn concurrent_admissions_are_all_durable() {
        let root = TestDir::new("ledger-concurrent");
        let ledger = Arc::new(Mutex::new(open(root.path(), UtcMicros(1))));
        let workers = (0..8u8)
            .map(|worker| {
                let ledger = Arc::clone(&ledger);
                std::thread::spawn(move || {
                    for index in 0..8u8 {
                        let admitted = envelope(1 + worker * 8 + index, 5);
                        let staged = ledger
                            .lock()
                            .unwrap()
                            .stage_admission(&admitted, Some(&admitted), UtcMicros(2))
                            .unwrap();
                        assert_eq!(staged.receipt.decision, HookAdmissionDecisionV1::Admitted);
                        staged.commit.wait().unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        drop(ledger);

        let (mut reopened, report) = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            UtcMicros(3),
        )
        .unwrap();
        assert_eq!(report.live_records, 64);
        assert_eq!(report.pending_work, 64);
        assert_eq!(report.truncated_tail_bytes, 0);
        for id in 1..=64 {
            assert_eq!(
                reopened.admit(&envelope(id, 5), UtcMicros(4)).unwrap(),
                HookAdmissionDecisionV1::ExactDuplicate
            );
        }
    }

    #[test]
    fn killing_the_admitting_process_keeps_every_acknowledged_admission_whole() {
        const ROOT_ENV: &str = "TRACEDECAY_HOOK_ADMISSION_KILL_ROOT";
        const WORKERS: u8 = 4;
        const PER_WORKER: u8 = 60;
        const KILL_AFTER_ACKS: usize = 48;
        if let Some(root) = std::env::var_os(ROOT_ENV) {
            let ledger = Arc::new(Mutex::new(open(Path::new(&root), UtcMicros(1))));
            let workers = (0..WORKERS)
                .map(|worker| {
                    let ledger = Arc::clone(&ledger);
                    std::thread::spawn(move || {
                        for index in 0..PER_WORKER {
                            let id = 1 + worker * PER_WORKER + index;
                            let admitted = envelope(id, 5);
                            let staged = ledger
                                .lock()
                                .unwrap()
                                .stage_admission(&admitted, Some(&admitted), UtcMicros(2))
                                .unwrap();
                            staged.commit.wait().unwrap();
                            println!("acked {id}");
                        }
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().unwrap();
            }
            loop {
                std::thread::park();
            }
        }

        let root = TestDir::new("ledger-kill");
        let test_name = "admission_ledger::tests::killing_the_admitting_process_keeps_every_acknowledged_admission_whole";
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env(ROOT_ENV, root.path())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut acked = Vec::new();
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            if let Some(id) = line.unwrap().strip_prefix("acked ") {
                acked.push(id.parse::<u8>().unwrap());
                if acked.len() == KILL_AFTER_ACKS {
                    break;
                }
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(acked.len(), KILL_AFTER_ACKS);

        let (mut reopened, report) = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            UtcMicros(3),
        )
        .unwrap();
        assert!(report.live_records as usize >= KILL_AFTER_ACKS);
        assert_eq!(
            report.pending_work, report.live_records,
            "every surviving admission keeps the work it was admitted with"
        );
        let pending = reopened
            .pending_work_envelopes(UtcMicros(4))
            .unwrap()
            .into_iter()
            .map(|work| work.event_id[0])
            .collect::<BTreeSet<_>>();
        for id in acked {
            assert!(pending.contains(&id), "acknowledged work {id} was lost");
            assert_eq!(
                reopened.admit(&envelope(id, 5), UtcMicros(5)).unwrap(),
                HookAdmissionDecisionV1::ExactDuplicate,
                "acknowledged admission {id} was lost"
            );
        }
    }

    #[test]
    fn a_pre_log_ledger_is_refused_for_reset_not_imported() {
        let open = |root: &Path| {
            HookAdmissionLedgerV1::open(
                root,
                NativeHostIdentityV1::ClaudeCode,
                HookAdmissionLedgerLimitsV1::stock(),
                UtcMicros(3),
            )
            .map(|(_, report)| report)
        };
        for member in ["admissions.v1.bin", "admission-work-completions.v1.json"] {
            let root = TestDir::new("ledger-pre-log");
            // A TDL1 header followed by one record body.
            let mut pre_log = b"TDL1\x01\x00".to_vec();
            pre_log.extend_from_slice(&[9u8; 64]);
            fs::write(root.path().join(member), &pre_log).unwrap();

            assert_eq!(
                open(root.path()),
                Err(HookAdmissionLedgerError::ResetRequired)
            );
            assert_eq!(fs::read(root.path().join(member)).unwrap(), pre_log);
            assert!(!root.path().join(LOG_FILE).exists());
            // The refusal persists until the reset deletes the root.
            assert_eq!(
                open(root.path()),
                Err(HookAdmissionLedgerError::ResetRequired)
            );
            fs::remove_file(root.path().join(member)).unwrap();
            assert_eq!(open(root.path()).unwrap().live_records, 0);
        }
    }

    #[test]
    fn a_foreign_log_is_replaced_by_an_empty_one() {
        let root = TestDir::new("ledger-foreign");
        fs::write(root.path().join(LOG_FILE), [0xAB; 32]).unwrap();

        let (mut ledger, report) = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            UtcMicros(1),
        )
        .unwrap();
        assert_eq!(report.truncated_tail_bytes, 32);
        assert_eq!(
            ledger.admit(&envelope(9, 5), UtcMicros(2)).unwrap(),
            HookAdmissionDecisionV1::Admitted
        );
        drop(ledger);
        let mut reopened = open(root.path(), UtcMicros(3));
        assert_eq!(
            reopened.admit(&envelope(9, 5), UtcMicros(4)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
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
            ..HookAdmissionLedgerLimitsV1::stock()
        };
        let mut ledger = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            limits,
            UtcMicros(1),
        )
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
        let mut reopened = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            limits,
            UtcMicros(20),
        )
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
        let path = root.path().join(LOG_FILE);
        let mut bytes = fs::read(&path).unwrap();
        bytes.extend_from_slice(&[0xAB; 20]);
        fs::write(&path, &bytes).unwrap();

        let (mut ledger, report) = HookAdmissionLedgerV1::open(
            root.path(),
            NativeHostIdentityV1::ClaudeCode,
            HookAdmissionLedgerLimitsV1::stock(),
            UtcMicros(3),
        )
        .unwrap();

        assert_eq!(report.truncated_tail_bytes, 20);
        assert_eq!(report.live_records, 1);
        assert_eq!(
            ledger.admit(&envelope(9, 5), UtcMicros(4)).unwrap(),
            HookAdmissionDecisionV1::ExactDuplicate
        );
    }
}
