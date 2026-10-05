//! Bounded post-flush hook delivery receipts.
//!
//! One private file is published per exact receipt only after the host output
//! writer has flushed successfully. The daemon settles files through the
//! project delivery authority and removes them only after that durable CAS.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DeliverySettlementOutcomeV1, DeliverySettlementV1, DeliverySurfaceFamilyV1,
    canonical_json_bytes, canonical_sha256, sha256_hex_suffix,
};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, is_owned_temporary_name, read_bounded, sync_directory, sync_file_at,
    tighten_existing_file, validate_regular_or_missing,
};
use tracedecay_private_fs::{FileLease, LockAdmissionError, lock_shared_until, lock_until};

const MAX_PENDING_RECEIPTS: usize = 1_024;
const MAX_RECEIPT_BYTES: usize = 4 * 1024;
const RECEIPT_SUFFIX: &str = ".delivery.v1.json";
const LOCK_FILE: &str = "writer.v1.lock";
/// Shared by writers while their staged receipts exist; exclusive only for
/// the owner's adoption of abandoned staging.
const STAGING_LOCK_FILE: &str = "staging.v1.lock";
const STAGED_MARKER: &str = ".staged.";
/// A staged receipt renamed after its sync: complete, so any owner adopts it
/// without waiting for live writers to leave the staging lease.
const READY_MARKER: &str = ".ready.";
const SPOOL_DIRECTORY: &str = "hook-delivery-spool";
const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::Strict;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookDeliverySourceReceiptV1 {
    pub receipt_id: [u8; 16],
    pub settlement: DeliverySettlementV1,
}

impl HookDeliverySourceReceiptV1 {
    pub fn new(settlement: DeliverySettlementV1) -> Result<Self, HookDeliverySpoolError> {
        validate_settlement(&settlement)?;
        // A source receipt identifies the logical host event and recipient,
        // not the wall-clock at which a retry happened.  Attempt/settlement
        // timestamps remain in the retained payload for truthful evidence,
        // but are deliberately absent from the durable file key so an exact
        // retry replays the first receipt instead of creating a second one.
        let receipt_id = receipt_id_for_settlement(&settlement)?;
        Ok(Self {
            receipt_id,
            settlement,
        })
    }

    pub fn validate(&self) -> Result<(), HookDeliverySpoolError> {
        if self.receipt_id == [0; 16] {
            return Err(HookDeliverySpoolError::InvalidReceipt);
        }
        validate_settlement(&self.settlement)?;
        let expected = receipt_id_for_settlement(&self.settlement)?;
        if expected != self.receipt_id {
            return Err(HookDeliverySpoolError::InvalidReceipt);
        }
        Ok(())
    }

    /// Compare the logical delivery and recipient, retaining the first
    /// attempt's timestamps as evidence when an identical output is retried.
    pub fn same_identity(&self, other: &Self) -> bool {
        self.receipt_id == other.receipt_id
            && StableReceiptIdentity::from_settlement(&self.settlement)
                == StableReceiptIdentity::from_settlement(&other.settlement)
    }
}

/// Stable source identity used for the spool filename and replay comparison.
/// Delivery timestamps are evidence attached to the first observed attempt,
/// never part of the retry key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StableReceiptIdentity<'a> {
    owner_event_id: &'a str,
    event_class: tracedecay_domain::DeliveryEventClassV1,
    channel: &'a tracedecay_domain::DeliveryChannelIdentityV1,
    work_attempt: &'a Option<tracedecay_domain::WorkAttemptIdentityV1>,
    eligible: u16,
    outcome: DeliverySettlementOutcomeV1,
    drop_reason: &'a Option<tracedecay_domain::DeliveryDropReasonV1>,
}

impl<'a> StableReceiptIdentity<'a> {
    fn from_settlement(settlement: &'a DeliverySettlementV1) -> Self {
        Self {
            owner_event_id: &settlement.attempt.owner_event_id,
            event_class: settlement.attempt.event_class,
            channel: &settlement.attempt.channel,
            work_attempt: &settlement.attempt.work_attempt,
            eligible: settlement.attempt.eligible,
            outcome: settlement.outcome,
            drop_reason: &settlement.drop_reason,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HookDeliverySpoolError {
    #[error("hook delivery receipt is invalid")]
    InvalidReceipt,
    #[error("hook delivery receipt spool is full")]
    Full,
    #[error("hook delivery receipt spool is busy")]
    Busy,
    #[error("hook delivery writer admission deadline expired")]
    AdmissionTimedOut,
    #[error("hook delivery receipt spool path is unsafe")]
    UnsafePath,
    #[error("hook delivery receipt spool is corrupt")]
    Corrupt,
    #[error("hook delivery receipt spool I/O failed")]
    Io,
}

/// Sole reader and acknowledger of one host's delivery receipts.
///
/// It holds the writer lock exclusively, so no writer publishes while it
/// reads or releases receipts. Hook callbacks wait on that lock within their
/// synchronous budget, so it never holds the lock across a durability
/// barrier: every name it renames or removes is already durable or may
/// reappear harmlessly.
#[derive(Debug)]
pub struct HookDeliveryReceiptSpoolV1 {
    root: PathBuf,
    _lock: FileLease,
}

/// How a writer retained a receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookDeliveryRetentionV1 {
    /// The receipt is published for the daemon to settle.
    Published,
    /// The exact receipt was already retained; its first copy is returned.
    AlreadyRetained(HookDeliverySourceReceiptV1),
    /// The receipt is durably staged but the publish lock stayed held past the
    /// writer's budget; the next owner open adopts it.
    Staged,
}

/// What became of the delivery receipt of a hook output the host received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookDeliveryReceiptOutcomeV1 {
    /// The receipt is retained for the daemon to settle.
    Delivered(HookDeliveryRetentionV1),
    /// The receipt was refused, so the daemon never settles this delivery.
    Refused {
        reason: HookDeliveryReceiptRefusalV1,
    },
    /// Retaining the receipt failed.
    Failed { cause: HookDeliveryReceiptFailureV1 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookDeliveryReceiptRefusalV1 {
    /// The delivery's settlement identity could not be derived.
    IdentityUnavailable,
    /// The receipt failed validation.
    InvalidReceipt,
    /// The spool holds its bound of pending receipts.
    Full,
    /// The spool stayed locked past the hook's budget.
    Busy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookDeliveryReceiptFailureV1 {
    UnsafePath,
    Corrupt,
    Io,
}

impl HookDeliveryReceiptOutcomeV1 {
    /// Retains `receipt` in the spool at `root` within `wait_budget`.
    pub fn retain(
        root: impl Into<PathBuf>,
        wait_budget: Duration,
        receipt: &HookDeliverySourceReceiptV1,
    ) -> Self {
        HookDeliveryReceiptWriterV1::open_within(root, wait_budget)
            .and_then(|writer| writer.retain(receipt))
            .into()
    }

    /// The `hook_completed` reason code of a spooled event with this receipt.
    pub const fn spooled_reason_code(&self) -> &'static str {
        match self {
            Self::Delivered(_) => "hook_v2_spooled",
            Self::Refused { reason } => match reason {
                HookDeliveryReceiptRefusalV1::IdentityUnavailable => {
                    "hook_v2_spooled_delivery_receipt_refused_identity_unavailable"
                }
                HookDeliveryReceiptRefusalV1::InvalidReceipt => {
                    "hook_v2_spooled_delivery_receipt_refused_invalid"
                }
                HookDeliveryReceiptRefusalV1::Full => {
                    "hook_v2_spooled_delivery_receipt_refused_full"
                }
                HookDeliveryReceiptRefusalV1::Busy => {
                    "hook_v2_spooled_delivery_receipt_refused_busy"
                }
            },
            Self::Failed { cause } => match cause {
                HookDeliveryReceiptFailureV1::UnsafePath => {
                    "hook_v2_spooled_delivery_receipt_failed_unsafe_path"
                }
                HookDeliveryReceiptFailureV1::Corrupt => {
                    "hook_v2_spooled_delivery_receipt_failed_corrupt"
                }
                HookDeliveryReceiptFailureV1::Io => "hook_v2_spooled_delivery_receipt_failed_io",
            },
        }
    }
}

impl From<Result<HookDeliveryRetentionV1, HookDeliverySpoolError>>
    for HookDeliveryReceiptOutcomeV1
{
    fn from(retained: Result<HookDeliveryRetentionV1, HookDeliverySpoolError>) -> Self {
        let refused = |reason| Self::Refused { reason };
        let failed = |cause| Self::Failed { cause };
        match retained {
            Ok(retention) => Self::Delivered(retention),
            Err(HookDeliverySpoolError::InvalidReceipt) => {
                refused(HookDeliveryReceiptRefusalV1::InvalidReceipt)
            }
            Err(HookDeliverySpoolError::Full) => refused(HookDeliveryReceiptRefusalV1::Full),
            Err(HookDeliverySpoolError::Busy | HookDeliverySpoolError::AdmissionTimedOut) => {
                refused(HookDeliveryReceiptRefusalV1::Busy)
            }
            Err(HookDeliverySpoolError::UnsafePath) => {
                failed(HookDeliveryReceiptFailureV1::UnsafePath)
            }
            Err(HookDeliverySpoolError::Corrupt) => failed(HookDeliveryReceiptFailureV1::Corrupt),
            Err(HookDeliverySpoolError::Io) => failed(HookDeliveryReceiptFailureV1::Io),
        }
    }
}

/// One hook callback's writer for its host's delivery receipts.
///
/// Writers share the staging lease, so concurrent callbacks never wait on
/// each other's durability barriers: each stages and syncs its receipt
/// outside the writer lock and holds that lock only to publish by rename.
#[derive(Debug)]
pub struct HookDeliveryReceiptWriterV1 {
    root: PathBuf,
    writer_lock: File,
    wait_budget: Duration,
    _staging: FileLease,
}

impl HookDeliveryReceiptSpoolV1 {
    /// Opens the spool, waiting at most `wait_budget` for a held writer lock,
    /// then adopts receipts that writers staged but could not publish in
    /// their budget. A writer publishes its receipt while holding the lock,
    /// so a drain woken by that publication waits it out instead of missing
    /// the receipt. A lock still held after the budget is `Busy`.
    #[tracing::instrument(name = "hooks.delivery.open", level = "trace", skip_all)]
    pub fn open(
        root: impl Into<PathBuf>,
        wait_budget: Duration,
    ) -> Result<Self, HookDeliverySpoolError> {
        let root = root.into();
        ensure_root(&root)?;
        let lock = open_lock_file(&root, LOCK_FILE)?;
        let try_lock_result = {
            let _span = tracing::trace_span!("hooks.delivery.lock.try_lock").entered();
            lock.try_lock()
        };
        match try_lock_result {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                if wait_budget.is_zero() {
                    return Err(HookDeliverySpoolError::Busy);
                }
                lock_until(&lock, Instant::now() + wait_budget).map_err(|error| match error {
                    LockAdmissionError::TimedOut => HookDeliverySpoolError::Busy,
                    LockAdmissionError::Io(_) => HookDeliverySpoolError::Io,
                })?;
            }
            Err(std::fs::TryLockError::Error(_)) => return Err(HookDeliverySpoolError::Io),
        }
        let spool = Self {
            root,
            _lock: FileLease::held(lock, "hooks.delivery.writer"),
        };
        spool.adopt_staged()?;
        spool.receipt_paths()?;
        Ok(spool)
    }

    /// Publishes receipts writers readied after running out of budget, and
    /// staging a killed writer left behind. Readied receipts are complete, so
    /// they are adopted whenever the owner opens. Other staging is abandoned
    /// or complete only while no writer holds the staging lease, so it is
    /// adopted or removed only then. A writer syncs the directory before
    /// reporting a receipt staged, so a crash after an unsynced rename leaves
    /// one durable name to adopt again.
    fn adopt_staged(&self) -> Result<(), HookDeliverySpoolError> {
        let staging = open_lock_file(&self.root, STAGING_LOCK_FILE)?;
        let abandoned = match staging.try_lock() {
            Ok(()) => Some(FileLease::held(staging, "hooks.delivery.staging")),
            Err(std::fs::TryLockError::WouldBlock) => None,
            Err(std::fs::TryLockError::Error(_)) => return Err(HookDeliverySpoolError::Io),
        };
        for entry in fs::read_dir(&self.root).map_err(|_| HookDeliverySpoolError::Io)? {
            let entry = entry.map_err(|_| HookDeliverySpoolError::Io)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_owned_temporary_name(&name)
                || !entry
                    .file_type()
                    .map_err(|_| HookDeliverySpoolError::Io)?
                    .is_file()
            {
                continue;
            }
            let final_name = match (ready_receipt_name(&name), &abandoned) {
                (Some(final_name), _) => Some(final_name),
                (None, Some(_)) => staged_receipt_name(&name),
                (None, None) => continue,
            };
            let staged = entry.path();
            let adopted = final_name
                .zip(read_bounded(&staged, MAX_RECEIPT_BYTES).ok().flatten())
                .and_then(|(final_name, bytes)| {
                    decode_receipt(&bytes)
                        .ok()
                        .filter(|receipt| receipt_file_name(receipt.receipt_id) == final_name)
                });
            match adopted {
                Some(receipt) => {
                    publish_staged(&self.root, &staged, &receipt)?;
                }
                None => fs::remove_file(&staged).map_err(|_| HookDeliverySpoolError::Io)?,
            }
        }
        Ok(())
    }

    /// Whether `root` holds receipts, published or readied, for a drain.
    pub fn has_receipts(root: &Path) -> Result<bool, HookDeliverySpoolError> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err(HookDeliverySpoolError::Io),
        };
        for entry in entries {
            let entry = entry.map_err(|_| HookDeliverySpoolError::Io)?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(hook_delivery_publication_name)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[tracing::instrument(name = "hooks.delivery.pending", level = "trace", skip_all)]
    pub fn pending(
        &self,
        limit: usize,
    ) -> Result<Vec<HookDeliverySourceReceiptV1>, HookDeliverySpoolError> {
        let mut receipts = Vec::new();
        for path in self
            .receipt_paths()?
            .into_iter()
            .take(limit.min(MAX_PENDING_RECEIPTS))
        {
            let bytes = read_bounded(&path, MAX_RECEIPT_BYTES)
                .map_err(map_read_error)?
                .ok_or(HookDeliverySpoolError::Corrupt)?;
            let receipt = decode_receipt(&bytes)?;
            if self.receipt_path(receipt.receipt_id) != path {
                return Err(HookDeliverySpoolError::Corrupt);
            }
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    #[tracing::instrument(name = "hooks.delivery.acknowledge", level = "trace", skip_all)]
    pub fn acknowledge(self, receipt_id: [u8; 16]) -> Result<bool, HookDeliverySpoolError> {
        Ok(self.acknowledge_many(&[receipt_id])? > 0)
    }

    /// Releases settled receipts, then gives up the writer lock before the one
    /// directory sync that makes the removals durable. A receipt whose removal
    /// a crash undoes is settled again idempotently. Returns how many receipts
    /// were still present.
    #[tracing::instrument(name = "hooks.delivery.acknowledge_many", level = "trace", skip_all)]
    pub fn acknowledge_many(
        self,
        receipt_ids: &[[u8; 16]],
    ) -> Result<usize, HookDeliverySpoolError> {
        let mut removed = 0;
        for receipt_id in receipt_ids {
            let path = self.receipt_path(*receipt_id);
            if !validate_regular_or_missing(&path).map_err(map_read_error)? {
                continue;
            }
            fs::remove_file(path).map_err(|_| HookDeliverySpoolError::Io)?;
            removed += 1;
        }
        let Self { root, _lock: lock } = self;
        drop(lock);
        if removed > 0 {
            {
                let _span = tracing::trace_span!("hooks.delivery.fsync.ack").entered();
                sync_directory(&root, DIRECTORY_POLICY).map_err(|_| HookDeliverySpoolError::Io)
            }?;
        }
        Ok(removed)
    }

    fn receipt_paths(&self) -> Result<Vec<PathBuf>, HookDeliverySpoolError> {
        receipt_names(&self.root)
    }

    fn receipt_path(&self, receipt_id: [u8; 16]) -> PathBuf {
        self.root.join(receipt_file_name(receipt_id))
    }
}

/// Every published receipt, sorted; staging, locks, and temporaries excluded.
#[tracing::instrument(name = "hooks.delivery.receipt_paths", level = "trace", skip_all)]
fn receipt_names(root: &Path) -> Result<Vec<PathBuf>, HookDeliverySpoolError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(|_| HookDeliverySpoolError::Io)? {
        let entry = entry.map_err(|_| HookDeliverySpoolError::Io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| HookDeliverySpoolError::UnsafePath)?;
        if name == LOCK_FILE || name == STAGING_LOCK_FILE || is_owned_temporary_name(&name) {
            continue;
        }
        if !valid_receipt_name(&name)
            || !entry
                .file_type()
                .map_err(|_| HookDeliverySpoolError::Io)?
                .is_file()
        {
            return Err(HookDeliverySpoolError::UnsafePath);
        }
        paths.push(entry.path());
        if paths.len() > MAX_PENDING_RECEIPTS {
            return Err(HookDeliverySpoolError::Full);
        }
    }
    paths.sort();
    Ok(paths)
}

impl HookDeliveryReceiptWriterV1 {
    /// `wait_budget` bounds each lock wait, measured from its own attempt:
    /// the shared staging lease here, and the publish lock in [`Self::retain`].
    #[tracing::instrument(name = "hooks.delivery.open_writer", level = "trace", skip_all)]
    pub fn open_within(
        root: impl Into<PathBuf>,
        wait_budget: Duration,
    ) -> Result<Self, HookDeliverySpoolError> {
        let root = root.into();
        ensure_root(&root)?;
        let writer_lock = open_lock_file(&root, LOCK_FILE)?;
        let staging = open_lock_file(&root, STAGING_LOCK_FILE)?;
        lock_shared_until(&staging, Instant::now() + wait_budget).map_err(admission_error)?;
        Ok(Self {
            root,
            writer_lock,
            wait_budget,
            _staging: FileLease::held(staging, "hooks.delivery.staging"),
        })
    }

    /// Durably retains `receipt`: stages and syncs it outside the writer
    /// lock, then holds that lock only to publish by rename. A publish lock
    /// held past the budget leaves the receipt staged for the next owner open
    /// instead of failing.
    #[tracing::instrument(name = "hooks.delivery.retain", level = "trace", skip_all)]
    pub fn retain(
        &self,
        receipt: &HookDeliverySourceReceiptV1,
    ) -> Result<HookDeliveryRetentionV1, HookDeliverySpoolError> {
        let root = self.root.as_path();
        receipt.validate()?;
        if let Some(existing) = retained(root, receipt)? {
            return Ok(HookDeliveryRetentionV1::AlreadyRetained(existing));
        }
        let bytes =
            canonical_json_bytes(receipt).map_err(|_| HookDeliverySpoolError::InvalidReceipt)?;
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
            return Err(HookDeliverySpoolError::InvalidReceipt);
        }
        let staged = {
            let _span = tracing::trace_span!("hooks.delivery.fsync.stage").entered();
            stage(root, receipt.receipt_id, &bytes)
        }?;
        let publish = match self.writer_lock.try_clone() {
            Ok(publish) => publish,
            Err(_) => {
                let _ = fs::remove_file(&staged);
                return Err(HookDeliverySpoolError::Io);
            }
        };
        match lock_until(&publish, Instant::now() + self.wait_budget) {
            Ok(()) => {}
            Err(LockAdmissionError::TimedOut) => {
                let ready = staged
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| root.join(name.replacen(STAGED_MARKER, READY_MARKER, 1)))
                    .ok_or(HookDeliverySpoolError::Io)?;
                fs::rename(&staged, &ready).map_err(|_| HookDeliverySpoolError::Io)?;
                {
                    let _span = tracing::trace_span!("hooks.delivery.fsync.staged").entered();
                    {
                        sync_directory(root, DIRECTORY_POLICY)
                            .map_err(|_| HookDeliverySpoolError::Io)
                    }
                }?;
                return Ok(HookDeliveryRetentionV1::Staged);
            }
            Err(LockAdmissionError::Io(_)) => {
                let _ = fs::remove_file(&staged);
                return Err(HookDeliverySpoolError::Io);
            }
        }
        let lease = FileLease::held(publish, "hooks.delivery.writer");
        let published = publish_staged(root, &staged, receipt);
        drop(lease);
        let retention = published?;
        if retention == HookDeliveryRetentionV1::Published {
            {
                let _span = tracing::trace_span!("hooks.delivery.fsync.append").entered();
                sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookDeliverySpoolError::Io)
            }?;
        }
        Ok(retention)
    }
}

/// The receipt already retained under `receipt`'s identity, if any.
fn retained(
    root: &Path,
    receipt: &HookDeliverySourceReceiptV1,
) -> Result<Option<HookDeliverySourceReceiptV1>, HookDeliverySpoolError> {
    let path = root.join(receipt_file_name(receipt.receipt_id));
    let Some(bytes) = read_bounded(&path, MAX_RECEIPT_BYTES).map_err(map_read_error)? else {
        return Ok(None);
    };
    let existing = decode_receipt(&bytes)?;
    if existing.same_identity(receipt) {
        Ok(Some(existing))
    } else {
        Err(HookDeliverySpoolError::Corrupt)
    }
}

/// Renames a synced staged receipt into place. The caller holds the writer
/// lock exclusively, so the existence and capacity checks cannot race another
/// publisher.
fn publish_staged(
    root: &Path,
    staged: &Path,
    receipt: &HookDeliverySourceReceiptV1,
) -> Result<HookDeliveryRetentionV1, HookDeliverySpoolError> {
    let outcome = match retained(root, receipt) {
        Ok(Some(existing)) => Ok(HookDeliveryRetentionV1::AlreadyRetained(existing)),
        Ok(None) if receipt_names(root)?.len() >= MAX_PENDING_RECEIPTS => {
            Err(HookDeliverySpoolError::Full)
        }
        Ok(None) => {
            return fs::rename(staged, root.join(receipt_file_name(receipt.receipt_id)))
                .map(|()| HookDeliveryRetentionV1::Published)
                .map_err(|_| HookDeliverySpoolError::Io);
        }
        Err(error) => Err(error),
    };
    fs::remove_file(staged).map_err(|_| HookDeliverySpoolError::Io)?;
    outcome
}

/// Writes and syncs `bytes` under a staging name only this writer owns.
fn stage(
    root: &Path,
    receipt_id: [u8; 16],
    bytes: &[u8],
) -> Result<PathBuf, HookDeliverySpoolError> {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let staged = root.join(format!(
        ".{}{STAGED_MARKER}{}.{}.tmp",
        receipt_file_name(receipt_id),
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed),
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(&staged)
        .and_then(|mut output| output.write_all(bytes))
        .and_then(|()| tighten_existing_file(&staged))
        .and_then(|()| sync_file_at(&staged));
    if written.is_err() {
        let _ = fs::remove_file(&staged);
        return Err(HookDeliverySpoolError::Io);
    }
    Ok(staged)
}

/// The receipt file a staging name was written for.
fn staged_receipt_name(name: &str) -> Option<&str> {
    marked_receipt_name(name, STAGED_MARKER)
}

/// The receipt file a readied staging name holds.
fn ready_receipt_name(name: &str) -> Option<&str> {
    marked_receipt_name(name, READY_MARKER)
}

fn marked_receipt_name<'a>(name: &'a str, marker: &str) -> Option<&'a str> {
    let (receipt, _) = name.strip_prefix('.')?.split_once(marker)?;
    valid_receipt_name(receipt).then_some(receipt)
}

/// Whether a delivery spool entry named `name` is a receipt for the drain to
/// settle: one published into place, or one readied for adoption. Creating
/// either wakes the drain.
pub fn hook_delivery_publication_name(name: &str) -> bool {
    valid_receipt_name(name) || ready_receipt_name(name).is_some()
}

fn receipt_file_name(receipt_id: [u8; 16]) -> String {
    format!(
        "{}{}",
        tracedecay_domain::canonical_text::encode_lowercase_hex(&receipt_id),
        RECEIPT_SUFFIX
    )
}

fn open_lock_file(root: &Path, name: &str) -> Result<File, HookDeliverySpoolError> {
    let lock_path = root.join(name);
    validate_regular_or_missing(&lock_path).map_err(|_| HookDeliverySpoolError::UnsafePath)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(&lock_path)
        .map_err(|_| HookDeliverySpoolError::Io)?;
    if !validate_regular_or_missing(&lock_path).map_err(|_| HookDeliverySpoolError::UnsafePath)? {
        return Err(HookDeliverySpoolError::UnsafePath);
    }
    Ok(lock)
}

fn admission_error(error: LockAdmissionError) -> HookDeliverySpoolError {
    match error {
        LockAdmissionError::TimedOut => HookDeliverySpoolError::AdmissionTimedOut,
        LockAdmissionError::Io(_) => HookDeliverySpoolError::Io,
    }
}

/// The directory holding every host's delivery receipt spool.
pub fn hook_delivery_spool_directory(data_root: &Path) -> PathBuf {
    data_root.join(SPOOL_DIRECTORY)
}

pub fn hook_delivery_receipt_spool_root(
    data_root: &Path,
    host: tracedecay_domain::NativeHostIdentityV1,
) -> PathBuf {
    hook_delivery_spool_directory(data_root).join(host.hook_key())
}

fn receipt_id_for_settlement(
    settlement: &DeliverySettlementV1,
) -> Result<[u8; 16], HookDeliverySpoolError> {
    let digest = canonical_sha256(&(
        "tracedecay.hook-delivery-source-receipt.v1",
        StableReceiptIdentity::from_settlement(settlement),
    ))
    .map_err(|_| HookDeliverySpoolError::InvalidReceipt)?;
    let hex = sha256_hex_suffix(digest.as_str()).ok_or(HookDeliverySpoolError::InvalidReceipt)?;
    let mut receipt_id = [0_u8; 16];
    decode_hex_prefix(hex, &mut receipt_id)?;
    Ok(receipt_id)
}

fn validate_settlement(settlement: &DeliverySettlementV1) -> Result<(), HookDeliverySpoolError> {
    settlement
        .validate()
        .map_err(|_| HookDeliverySpoolError::InvalidReceipt)?;
    if settlement.attempt.channel.surface != DeliverySurfaceFamilyV1::Hook
        || settlement.attempt.eligible != 1
        || settlement.outcome != DeliverySettlementOutcomeV1::Delivered
        || settlement.drop_reason.is_some()
    {
        return Err(HookDeliverySpoolError::InvalidReceipt);
    }
    Ok(())
}

fn ensure_root(root: &Path) -> Result<(), HookDeliverySpoolError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(HookDeliverySpoolError::UnsafePath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(HookDeliverySpoolError::Io),
    }
    fs::create_dir_all(root).map_err(|_| HookDeliverySpoolError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|_| HookDeliverySpoolError::Io)?;
    }
    {
        let _span = tracing::trace_span!("hooks.delivery.fsync.directory").entered();
        sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookDeliverySpoolError::Io)
    }
}

fn decode_receipt(bytes: &[u8]) -> Result<HookDeliverySourceReceiptV1, HookDeliverySpoolError> {
    let receipt = serde_json::from_slice::<HookDeliverySourceReceiptV1>(bytes)
        .map_err(|_| HookDeliverySpoolError::Corrupt)?;
    receipt.validate()?;
    Ok(receipt)
}

fn map_read_error(error: std::io::Error) -> HookDeliverySpoolError {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        HookDeliverySpoolError::UnsafePath
    } else {
        HookDeliverySpoolError::Io
    }
}

fn valid_receipt_name(name: &str) -> bool {
    name.len() == 32 + RECEIPT_SUFFIX.len()
        && name.ends_with(RECEIPT_SUFFIX)
        && name[..32]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_hex_prefix(hex: &str, output: &mut [u8; 16]) -> Result<(), HookDeliverySpoolError> {
    if hex.len() < 32 {
        return Err(HookDeliverySpoolError::InvalidReceipt);
    }
    for (index, slot) in output.iter_mut().enumerate() {
        let offset = index * 2;
        let high = decode_nibble(hex.as_bytes()[offset])?;
        let low = decode_nibble(hex.as_bytes()[offset + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(())
}

fn decode_nibble(byte: u8) -> Result<u8, HookDeliverySpoolError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(HookDeliverySpoolError::InvalidReceipt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracedecay_domain::{
        DeliveryChannelIdentityV1, DeliveryEventClassV1, DeliverySettlementAttemptV1, UtcMicros,
    };

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "tracedecay-hook-delivery-{}-{}-{}",
                std::process::id(),
                crate::spool::hook_spool_checksum(b"delivery-spool-test")[0],
                sequence,
            ));
            let _ = fs::remove_dir_all(&root);
            Self(root)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if self.0.is_dir() {
                let _ = fs::remove_dir_all(&self.0);
            } else {
                let _ = fs::remove_file(&self.0);
            }
        }
    }

    #[test]
    fn a_writer_behind_a_held_owner_stages_its_receipt_for_adoption() {
        let root = TestDir::new();
        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        assert_eq!(
            HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap_err(),
            HookDeliverySpoolError::Busy
        );
        // An exhausted budget never admits, even when the lease is free.
        assert_eq!(
            HookDeliveryReceiptWriterV1::open_within(&root.0, Duration::ZERO).unwrap_err(),
            HookDeliverySpoolError::AdmissionTimedOut
        );
        let writer =
            HookDeliveryReceiptWriterV1::open_within(&root.0, Duration::from_millis(20)).unwrap();
        assert_eq!(
            writer.retain(&receipt()).unwrap(),
            HookDeliveryRetentionV1::Staged
        );
        drop(writer);
        assert_eq!(owner.pending(64).unwrap(), Vec::new());
        drop(owner);

        // The next owner open publishes the staged receipt exactly once.
        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        assert_eq!(owner.pending(64).unwrap(), vec![receipt()]);
        drop(owner);
        let writer =
            HookDeliveryReceiptWriterV1::open_within(&root.0, crate::HOOK_SYNCHRONOUS_BUDGET)
                .unwrap();
        assert_eq!(
            writer.retain(&receipt()).unwrap(),
            HookDeliveryRetentionV1::AlreadyRetained(receipt())
        );
    }

    #[test]
    fn a_readied_receipt_is_adopted_while_writers_hold_the_staging_lease() {
        let root = TestDir::new();
        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        let staged_by =
            HookDeliveryReceiptWriterV1::open_within(&root.0, Duration::from_millis(20)).unwrap();
        assert_eq!(
            staged_by.retain(&receipt()),
            Ok(HookDeliveryRetentionV1::Staged)
        );
        let later =
            HookDeliveryReceiptWriterV1::open_within(&root.0, Duration::from_millis(20)).unwrap();
        drop(owner);
        assert_eq!(HookDeliveryReceiptSpoolV1::has_receipts(&root.0), Ok(true));

        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        assert_eq!(owner.pending(64).unwrap(), vec![receipt()]);
        drop((owner, staged_by, later));
    }

    #[test]
    fn an_owner_open_waits_out_a_publishing_writer_within_its_budget() {
        let root = TestDir::new();
        assert_eq!(HookDeliveryReceiptSpoolV1::has_receipts(&root.0), Ok(false));
        drop(HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap());
        let publishing = open_lock_file(&root.0, LOCK_FILE).unwrap();
        publishing.lock().unwrap();
        assert_eq!(
            HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap_err(),
            HookDeliverySpoolError::Busy
        );
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                publishing.unlock().unwrap();
            });
            assert!(
                HookDeliveryReceiptSpoolV1::open(&root.0, crate::HOOK_SYNCHRONOUS_BUDGET).is_ok()
            );
        });
    }

    #[test]
    fn a_live_writers_staging_survives_an_owner_open_and_torn_staging_does_not() {
        let root = TestDir::new();
        let writer =
            HookDeliveryReceiptWriterV1::open_within(&root.0, crate::HOOK_SYNCHRONOUS_BUDGET)
                .unwrap();
        let staged = stage(&root.0, receipt().receipt_id, b"{\"torn").unwrap();
        drop(HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap());
        assert!(staged.is_file(), "a live writer's staging is never adopted");
        drop(writer);

        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        assert!(
            !staged.exists(),
            "a killed writer's torn staging is removed"
        );
        assert_eq!(owner.pending(64).unwrap(), Vec::new());
    }

    #[test]
    fn concurrent_writers_on_a_slow_disk_publish_every_receipt_within_budget() {
        const WRITERS: usize = 8;
        let root = TestDir::new();
        drop(HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap());
        let _slow_disk = tracedecay_private_fs::framed_log::sync_latency::inject(
            &root.0,
            Duration::from_millis(20),
        );
        let receipts = (0..WRITERS).map(indexed_receipt).collect::<Vec<_>>();
        let retained = std::thread::scope(|scope| {
            let writers = receipts
                .iter()
                .map(|receipt| {
                    let root = &root.0;
                    scope.spawn(move || {
                        HookDeliveryReceiptWriterV1::open_within(
                            root,
                            crate::HOOK_SYNCHRONOUS_BUDGET,
                        )
                        .and_then(|writer| writer.retain(receipt))
                    })
                })
                .collect::<Vec<_>>();
            writers
                .into_iter()
                .map(|writer| writer.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            retained,
            (0..WRITERS)
                .map(|_| Ok(HookDeliveryRetentionV1::Published))
                .collect::<Vec<_>>()
        );
        let mut pending = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO)
            .unwrap()
            .pending(64)
            .unwrap();
        let mut expected = receipts;
        pending.sort_by_key(|receipt| receipt.receipt_id);
        expected.sort_by_key(|receipt| receipt.receipt_id);
        assert_eq!(pending, expected);
    }

    #[test]
    fn the_owner_holds_the_writer_lock_across_no_durability_barrier() {
        let root = TestDir::new();
        let staged = indexed_receipt(1);
        let published = indexed_receipt(2);
        assert_eq!(
            retain(&root.0, &published),
            Ok(HookDeliveryRetentionV1::Published)
        );
        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        let writer =
            HookDeliveryReceiptWriterV1::open_within(&root.0, Duration::from_millis(20)).unwrap();
        assert_eq!(writer.retain(&staged), Ok(HookDeliveryRetentionV1::Staged));
        drop((writer, owner));

        // Each barrier outlasts a callback's whole lock-wait budget.
        let slow_disk = tracedecay_private_fs::framed_log::sync_latency::inject(
            &root.0,
            2 * crate::HOOK_SYNCHRONOUS_BUDGET,
        );
        let owner = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap();
        let mut pending = owner.pending(64).unwrap();
        pending.sort_by_key(|receipt| receipt.receipt_id);
        let mut expected = vec![staged.clone(), published.clone()];
        expected.sort_by_key(|receipt| receipt.receipt_id);
        assert_eq!(pending, expected);
        assert_eq!(slow_disk.syncs(), 0, "adoption syncs under the lock");

        let callback = std::thread::scope(|scope| {
            let callback = scope.spawn(|| {
                let lock = open_lock_file(&root.0, LOCK_FILE).unwrap();
                lock_until(&lock, Instant::now() + crate::HOOK_SYNCHRONOUS_BUDGET)
            });
            assert_eq!(
                owner.acknowledge_many(&[staged.receipt_id, published.receipt_id]),
                Ok(2)
            );
            callback.join().unwrap()
        });
        assert!(
            callback.is_ok(),
            "a callback waited out an acknowledgement barrier"
        );
        // Only unix fsyncs a directory; Windows has no directory barrier.
        assert_eq!(slow_disk.syncs(), u64::from(cfg!(unix)));
    }

    fn retain(
        root: &Path,
        receipt: &HookDeliverySourceReceiptV1,
    ) -> Result<HookDeliveryRetentionV1, HookDeliverySpoolError> {
        HookDeliveryReceiptWriterV1::open_within(root, crate::HOOK_SYNCHRONOUS_BUDGET)?
            .retain(receipt)
    }

    fn indexed_receipt(index: usize) -> HookDeliverySourceReceiptV1 {
        let mut settlement = receipt().settlement;
        settlement.attempt.owner_event_id = format!("hook:native:fixture-{index}");
        HookDeliverySourceReceiptV1::new(settlement).expect("receipt")
    }

    fn receipt() -> HookDeliverySourceReceiptV1 {
        HookDeliverySourceReceiptV1::new(DeliverySettlementV1 {
            attempt: DeliverySettlementAttemptV1 {
                owner_event_id: "hook:native:fixture".to_owned(),
                event_class: DeliveryEventClassV1::Activity,
                channel: DeliveryChannelIdentityV1 {
                    surface: DeliverySurfaceFamilyV1::Hook,
                    channel_ref: "hook:claude:session-fixture".to_owned(),
                },
                work_attempt: None,
                eligible: 1,
                valid_at: UtcMicros(100),
                attempted_at: UtcMicros(110),
            },
            outcome: DeliverySettlementOutcomeV1::Delivered,
            settled_at: UtcMicros(110),
            drop_reason: None,
        })
        .expect("receipt")
    }

    fn receipt_with_times(
        valid_at: i64,
        attempted_at: i64,
        settled_at: i64,
    ) -> HookDeliverySourceReceiptV1 {
        HookDeliverySourceReceiptV1::new(DeliverySettlementV1 {
            attempt: DeliverySettlementAttemptV1 {
                owner_event_id: "hook:native:fixture".to_owned(),
                event_class: DeliveryEventClassV1::Activity,
                channel: DeliveryChannelIdentityV1 {
                    surface: DeliverySurfaceFamilyV1::Hook,
                    channel_ref: "hook:claude:session-fixture".to_owned(),
                },
                work_attempt: None,
                eligible: 1,
                valid_at: UtcMicros(valid_at),
                attempted_at: UtcMicros(attempted_at),
            },
            outcome: DeliverySettlementOutcomeV1::Delivered,
            settled_at: UtcMicros(settled_at),
            drop_reason: None,
        })
        .expect("receipt")
    }

    #[test]
    fn post_flush_receipt_reopens_replays_and_acks_exactly_once() {
        let root = TestDir::new();
        let receipt = receipt();
        assert_eq!(
            retain(&root.0, &receipt),
            Ok(HookDeliveryRetentionV1::Published)
        );
        assert_eq!(
            retain(&root.0, &receipt),
            Ok(HookDeliveryRetentionV1::AlreadyRetained(receipt.clone()))
        );
        {
            let spool = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect("open");
            assert_eq!(spool.pending(64).expect("pending"), vec![receipt.clone()]);
            assert_eq!(
                HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).unwrap_err(),
                HookDeliverySpoolError::Busy
            );
        }
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect("reopen");
        assert_eq!(spool.pending(64).expect("replayed"), vec![receipt.clone()]);
        assert!(spool.acknowledge(receipt.receipt_id).expect("ack"));
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect("reopen");
        assert!(!spool.acknowledge(receipt.receipt_id).expect("ack replay"));
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect("reopen");
        assert!(spool.pending(64).expect("empty").is_empty());
    }

    #[test]
    fn exact_retry_identity_ignores_delivery_timestamps_and_replays_first_receipt() {
        let root = TestDir::new();
        let first = receipt_with_times(100, 110, 110);
        let retry = receipt_with_times(200, 220, 220);
        assert_eq!(first.receipt_id, retry.receipt_id);
        assert_eq!(
            retain(&root.0, &first),
            Ok(HookDeliveryRetentionV1::Published)
        );
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect("open");
        assert_eq!(spool.pending(64).expect("pending"), vec![first.clone()]);
        drop(spool);
        assert_eq!(
            retain(&root.0, &retry),
            Ok(HookDeliveryRetentionV1::AlreadyRetained(first)),
            "the retained settlement, including its first timestamps, is authoritative"
        );
    }

    #[test]
    fn open_rejects_a_non_directory_without_silently_dropping_receipts() {
        let root = TestDir::new();
        fs::write(&root.0, b"not a spool directory").expect("fixture file");
        assert_eq!(
            HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect_err("open must fail"),
            HookDeliverySpoolError::UnsafePath
        );
    }

    #[test]
    fn retain_propagates_full_spool_without_overwriting_existing_receipts() {
        let root = TestDir::new();
        drop(HookDeliveryReceiptSpoolV1::open(&root.0, Duration::ZERO).expect("open"));
        for index in 0..MAX_PENDING_RECEIPTS {
            let name = format!("{index:032x}{RECEIPT_SUFFIX}");
            fs::write(root.0.join(name), b"placeholder").expect("full fixture");
        }
        let before = receipt_names(&root.0).expect("receipt census");
        assert_eq!(before.len(), MAX_PENDING_RECEIPTS);
        assert_eq!(
            retain(&root.0, &receipt()),
            Err(HookDeliverySpoolError::Full)
        );
        assert_eq!(receipt_names(&root.0).expect("receipt census"), before);
    }
}
