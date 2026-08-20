//! Bounded post-flush hook delivery receipts.
//!
//! One private file is published per exact receipt only after the host output
//! writer has flushed successfully. The daemon settles files through the
//! project delivery authority and removes them only after that durable CAS.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    DeliverySettlementOutcomeV1, DeliverySettlementV1, DeliverySurfaceFamilyV1,
    canonical_json_bytes, canonical_sha256,
};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, atomic_write, read_bounded, sync_directory, validate_regular_or_missing,
};

const MAX_PENDING_RECEIPTS: usize = 1_024;
const MAX_RECEIPT_BYTES: usize = 4 * 1024;
const RECEIPT_SUFFIX: &str = ".delivery.v1.json";
const LOCK_FILE: &str = "writer.v1.lock";
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
        let digest = canonical_sha256(&(
            "tracedecay.hook-delivery-source-receipt.v1",
            StableReceiptIdentity::from_settlement(&settlement),
        ))
        .map_err(|_| HookDeliverySpoolError::InvalidReceipt)?;
        let hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(HookDeliverySpoolError::InvalidReceipt)?;
        let mut receipt_id = [0_u8; 16];
        decode_hex_prefix(hex, &mut receipt_id)?;
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
        let expected = Self::new(self.settlement.clone())?;
        if expected.receipt_id != self.receipt_id {
            return Err(HookDeliverySpoolError::InvalidReceipt);
        }
        Ok(())
    }

    fn same_identity(&self, other: &Self) -> bool {
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
    #[error("hook delivery receipt spool path is unsafe")]
    UnsafePath,
    #[error("hook delivery receipt spool is corrupt")]
    Corrupt,
    #[error("hook delivery receipt spool I/O failed")]
    Io,
}

/// Sole bounded writer/reader lease for one host's delivery receipts.
#[derive(Debug)]
pub struct HookDeliveryReceiptSpoolV1 {
    root: PathBuf,
    _lock: File,
}

impl HookDeliveryReceiptSpoolV1 {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, HookDeliverySpoolError> {
        let root = root.into();
        ensure_root(&root)?;
        let lock_path = root.join(LOCK_FILE);
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
        if !validate_regular_or_missing(&lock_path)
            .map_err(|_| HookDeliverySpoolError::UnsafePath)?
        {
            return Err(HookDeliverySpoolError::UnsafePath);
        }
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => HookDeliverySpoolError::Busy,
            std::fs::TryLockError::Error(_) => HookDeliverySpoolError::Io,
        })?;
        let spool = Self { root, _lock: lock };
        spool.receipt_paths()?;
        Ok(spool)
    }

    pub fn append(
        &self,
        receipt: &HookDeliverySourceReceiptV1,
    ) -> Result<bool, HookDeliverySpoolError> {
        receipt.validate()?;
        let path = self.receipt_path(receipt.receipt_id);
        if let Some(bytes) = read_bounded(&path, MAX_RECEIPT_BYTES).map_err(map_read_error)? {
            let existing = decode_receipt(&bytes)?;
            return if existing.same_identity(receipt) {
                Ok(false)
            } else {
                Err(HookDeliverySpoolError::Corrupt)
            };
        }
        if self.receipt_paths()?.len() >= MAX_PENDING_RECEIPTS {
            return Err(HookDeliverySpoolError::Full);
        }
        let bytes =
            canonical_json_bytes(receipt).map_err(|_| HookDeliverySpoolError::InvalidReceipt)?;
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
            return Err(HookDeliverySpoolError::InvalidReceipt);
        }
        atomic_write(&path, "delivery", &bytes, DIRECTORY_POLICY)
            .map_err(|_| HookDeliverySpoolError::Io)?;
        Ok(true)
    }

    /// Appends a source receipt or returns the exact durable receipt already
    /// retained for its stable identity.  Callers must forward the returned
    /// settlement to the daemon so a retry replays the original timestamps
    /// rather than reconstructing a conflicting delivery attempt.
    pub fn append_or_replay(
        &self,
        receipt: &HookDeliverySourceReceiptV1,
    ) -> Result<HookDeliverySourceReceiptV1, HookDeliverySpoolError> {
        receipt.validate()?;
        let path = self.receipt_path(receipt.receipt_id);
        if let Some(bytes) = read_bounded(&path, MAX_RECEIPT_BYTES).map_err(map_read_error)? {
            let existing = decode_receipt(&bytes)?;
            if existing.same_identity(receipt) {
                return Ok(existing);
            }
            return Err(HookDeliverySpoolError::Corrupt);
        }
        if self.receipt_paths()?.len() >= MAX_PENDING_RECEIPTS {
            return Err(HookDeliverySpoolError::Full);
        }
        let bytes =
            canonical_json_bytes(receipt).map_err(|_| HookDeliverySpoolError::InvalidReceipt)?;
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
            return Err(HookDeliverySpoolError::InvalidReceipt);
        }
        atomic_write(&path, "delivery", &bytes, DIRECTORY_POLICY)
            .map_err(|_| HookDeliverySpoolError::Io)?;
        Ok(receipt.clone())
    }

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

    pub fn acknowledge(&self, receipt_id: [u8; 16]) -> Result<bool, HookDeliverySpoolError> {
        let path = self.receipt_path(receipt_id);
        if !validate_regular_or_missing(&path).map_err(map_read_error)? {
            return Ok(false);
        }
        fs::remove_file(path).map_err(|_| HookDeliverySpoolError::Io)?;
        sync_directory(&self.root, DIRECTORY_POLICY).map_err(|_| HookDeliverySpoolError::Io)?;
        Ok(true)
    }

    fn receipt_paths(&self) -> Result<Vec<PathBuf>, HookDeliverySpoolError> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|_| HookDeliverySpoolError::Io)? {
            let entry = entry.map_err(|_| HookDeliverySpoolError::Io)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| HookDeliverySpoolError::UnsafePath)?;
            if name == LOCK_FILE {
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

    fn receipt_path(&self, receipt_id: [u8; 16]) -> PathBuf {
        self.root
            .join(format!("{}{}", encode_hex(receipt_id), RECEIPT_SUFFIX))
    }
}

pub fn hook_delivery_receipt_spool_root(data_root: &Path, host: crate::HookHostV1) -> PathBuf {
    data_root.join("hook-delivery-spool").join(host.hook_key())
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
    sync_directory(root, DIRECTORY_POLICY).map_err(|_| HookDeliverySpoolError::Io)
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

fn encode_hex(bytes: [u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(32);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
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
        {
            let spool = HookDeliveryReceiptSpoolV1::open(&root.0).expect("open");
            assert!(spool.append(&receipt).expect("append"));
            assert!(!spool.append(&receipt).expect("exact replay"));
            assert_eq!(spool.pending(64).expect("pending"), vec![receipt.clone()]);
            assert_eq!(
                HookDeliveryReceiptSpoolV1::open(&root.0).unwrap_err(),
                HookDeliverySpoolError::Busy
            );
        }
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0).expect("reopen");
        assert_eq!(spool.pending(64).expect("replayed"), vec![receipt.clone()]);
        assert!(spool.acknowledge(receipt.receipt_id).expect("ack"));
        assert!(!spool.acknowledge(receipt.receipt_id).expect("ack replay"));
        assert!(spool.pending(64).expect("empty").is_empty());
    }

    #[test]
    fn exact_retry_identity_ignores_delivery_timestamps_and_replays_first_receipt() {
        let root = TestDir::new();
        let first = receipt_with_times(100, 110, 110);
        let retry = receipt_with_times(200, 220, 220);
        assert_eq!(first.receipt_id, retry.receipt_id);
        {
            let spool = HookDeliveryReceiptSpoolV1::open(&root.0).expect("open");
            assert!(spool.append(&first).expect("first append"));
            assert!(!spool.append(&retry).expect("retry dedupe"));
            assert_eq!(spool.pending(64).expect("pending"), vec![first.clone()]);
        }
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0).expect("restart open");
        assert_eq!(
            spool.append_or_replay(&retry).expect("replay"),
            first,
            "the retained settlement, including its first timestamps, is authoritative"
        );
    }

    #[test]
    fn open_rejects_a_non_directory_without_silently_dropping_receipts() {
        let root = TestDir::new();
        fs::write(&root.0, b"not a spool directory").expect("fixture file");
        assert_eq!(
            HookDeliveryReceiptSpoolV1::open(&root.0).expect_err("open must fail"),
            HookDeliverySpoolError::UnsafePath
        );
    }

    #[test]
    fn append_propagates_full_spool_without_overwriting_existing_receipts() {
        let root = TestDir::new();
        let spool = HookDeliveryReceiptSpoolV1::open(&root.0).expect("open");
        for index in 0..MAX_PENDING_RECEIPTS {
            let name = format!("{index:032x}{RECEIPT_SUFFIX}");
            fs::write(root.0.join(name), b"placeholder").expect("full fixture");
        }
        let before = spool.receipt_paths().expect("receipt census").len();
        assert_eq!(before, MAX_PENDING_RECEIPTS);
        assert_eq!(
            spool.append(&receipt()).expect_err("full spool must fail"),
            HookDeliverySpoolError::Full
        );
        assert_eq!(spool.receipt_paths().expect("receipt census").len(), before);
    }
}
