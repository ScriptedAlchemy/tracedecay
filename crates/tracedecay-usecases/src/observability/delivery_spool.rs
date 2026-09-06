use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{DeliverySettlementV1, canonical_json_bytes, canonical_sha256};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, atomic_write, read_bounded, sync_directory, validate_regular_or_missing,
};

use super::ObservabilityProducerIdentityV1;

const MAX_PENDING_RECEIPTS: usize = 16_384;
const MAX_RECEIPT_BYTES: usize = 8 * 1024;
const RECEIPT_SUFFIX: &str = ".delivery.v1.json";
const LOCK_FILE: &str = "writer.v1.lock";
const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::Strict;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeliveryRecorderSourceReceiptV1 {
    pub receipt_id: [u8; 16],
    pub settlement: DeliverySettlementV1,
    /// Exact linked-root emission identity selected at admission. Receipts
    /// written before policy-specific frontends omit this and replay through
    /// the recorder's store-core identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emission_identity: Option<ObservabilityProducerIdentityV1>,
}

impl DeliveryRecorderSourceReceiptV1 {
    pub fn new(
        settlement: DeliverySettlementV1,
        emission_identity: ObservabilityProducerIdentityV1,
    ) -> Result<Self, DeliveryRecorderSpoolError> {
        settlement
            .validate()
            .map_err(|_| DeliveryRecorderSpoolError::InvalidReceipt)?;
        emission_identity
            .validate()
            .map_err(|_| DeliveryRecorderSpoolError::InvalidReceipt)?;
        let digest = canonical_sha256(&(
            "tracedecay.delivery-recorder-source-receipt.v2",
            &settlement,
            &emission_identity,
        ))
        .map_err(|_| DeliveryRecorderSpoolError::InvalidReceipt)?;
        let hex = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(DeliveryRecorderSpoolError::InvalidReceipt)?;
        let mut receipt_id = [0_u8; 16];
        decode_hex_prefix(hex, &mut receipt_id)?;
        Ok(Self {
            receipt_id,
            settlement,
            emission_identity: Some(emission_identity),
        })
    }

    fn validate(&self) -> Result<(), DeliveryRecorderSpoolError> {
        if self.receipt_id == [0; 16] {
            return Err(DeliveryRecorderSpoolError::InvalidReceipt);
        }
        self.settlement
            .validate()
            .map_err(|_| DeliveryRecorderSpoolError::InvalidReceipt)?;
        let expected_receipt_id = if let Some(emission_identity) = &self.emission_identity {
            Self::new(self.settlement.clone(), emission_identity.clone())?.receipt_id
        } else {
            legacy_receipt_id(&self.settlement)?
        };
        if expected_receipt_id != self.receipt_id {
            return Err(DeliveryRecorderSpoolError::InvalidReceipt);
        }
        Ok(())
    }
}

fn legacy_receipt_id(
    settlement: &DeliverySettlementV1,
) -> Result<[u8; 16], DeliveryRecorderSpoolError> {
    let digest = canonical_sha256(&("tracedecay.delivery-recorder-source-receipt.v1", settlement))
        .map_err(|_| DeliveryRecorderSpoolError::InvalidReceipt)?;
    let hex = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(DeliveryRecorderSpoolError::InvalidReceipt)?;
    let mut receipt_id = [0_u8; 16];
    decode_hex_prefix(hex, &mut receipt_id)?;
    Ok(receipt_id)
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(super) enum DeliveryRecorderSpoolError {
    #[error("delivery recorder receipt is invalid")]
    InvalidReceipt,
    #[error("delivery recorder receipt spool is full")]
    Full,
    #[error("delivery recorder receipt spool is busy")]
    Busy,
    #[error("delivery recorder receipt spool path is unsafe")]
    UnsafePath,
    #[error("delivery recorder receipt spool is corrupt")]
    Corrupt,
    #[error("delivery recorder receipt spool I/O failed")]
    Io,
    #[error("delivery recorder receipt spool lock is poisoned")]
    LockPoisoned,
}

/// One process lease over the durable, bounded source receipts for a project.
pub(super) struct DeliveryRecorderSpoolV1 {
    root: PathBuf,
    _lease: File,
    state: Mutex<DeliveryRecorderSpoolState>,
}

struct DeliveryRecorderSpoolState {
    receipt_paths: BTreeSet<PathBuf>,
}

impl DeliveryRecorderSpoolV1 {
    pub fn open(root: PathBuf) -> Result<Self, DeliveryRecorderSpoolError> {
        ensure_root(&root)?;
        let lock_path = root.join(LOCK_FILE);
        validate_regular_or_missing(&lock_path)
            .map_err(|_| DeliveryRecorderSpoolError::UnsafePath)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let lease = options
            .open(&lock_path)
            .map_err(|_| DeliveryRecorderSpoolError::Io)?;
        if !validate_regular_or_missing(&lock_path)
            .map_err(|_| DeliveryRecorderSpoolError::UnsafePath)?
        {
            return Err(DeliveryRecorderSpoolError::UnsafePath);
        }
        lease.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => DeliveryRecorderSpoolError::Busy,
            std::fs::TryLockError::Error(_) => DeliveryRecorderSpoolError::Io,
        })?;
        let receipt_paths = scan_receipt_paths(&root)?;
        let spool = Self {
            root,
            _lease: lease,
            state: Mutex::new(DeliveryRecorderSpoolState {
                receipt_paths: receipt_paths.into_iter().collect(),
            }),
        };
        Ok(spool)
    }

    pub fn append(
        &self,
        receipt: &DeliveryRecorderSourceReceiptV1,
    ) -> Result<bool, DeliveryRecorderSpoolError> {
        receipt.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DeliveryRecorderSpoolError::LockPoisoned)?;
        let path = self.receipt_path(receipt.receipt_id);
        if let Some(bytes) = read_bounded(&path, MAX_RECEIPT_BYTES).map_err(map_read_error)? {
            if !state.receipt_paths.contains(&path) {
                return Err(DeliveryRecorderSpoolError::Corrupt);
            }
            return if decode_receipt(&bytes)? == *receipt {
                Ok(false)
            } else {
                Err(DeliveryRecorderSpoolError::Corrupt)
            };
        }
        if state.receipt_paths.len() >= MAX_PENDING_RECEIPTS {
            return Err(DeliveryRecorderSpoolError::Full);
        }
        let bytes = canonical_json_bytes(receipt)
            .map_err(|_| DeliveryRecorderSpoolError::InvalidReceipt)?;
        if bytes.is_empty() || bytes.len() > MAX_RECEIPT_BYTES {
            return Err(DeliveryRecorderSpoolError::InvalidReceipt);
        }
        atomic_write(&path, "delivery", &bytes, DIRECTORY_POLICY)
            .map_err(|_| DeliveryRecorderSpoolError::Io)?;
        state.receipt_paths.insert(path);
        Ok(true)
    }

    pub fn pending(
        &self,
        limit: usize,
    ) -> Result<Vec<DeliveryRecorderSourceReceiptV1>, DeliveryRecorderSpoolError> {
        let state = self
            .state
            .lock()
            .map_err(|_| DeliveryRecorderSpoolError::LockPoisoned)?;
        let mut receipts = Vec::new();
        for path in state
            .receipt_paths
            .iter()
            .take(limit.min(MAX_PENDING_RECEIPTS))
        {
            let bytes = read_bounded(path, MAX_RECEIPT_BYTES)
                .map_err(map_read_error)?
                .ok_or(DeliveryRecorderSpoolError::Corrupt)?;
            let receipt = decode_receipt(&bytes)?;
            if self.receipt_path(receipt.receipt_id) != *path {
                return Err(DeliveryRecorderSpoolError::Corrupt);
            }
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    pub fn len(&self) -> Result<usize, DeliveryRecorderSpoolError> {
        let state = self
            .state
            .lock()
            .map_err(|_| DeliveryRecorderSpoolError::LockPoisoned)?;
        Ok(state.receipt_paths.len())
    }

    pub fn acknowledge(&self, receipt_id: [u8; 16]) -> Result<bool, DeliveryRecorderSpoolError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| DeliveryRecorderSpoolError::LockPoisoned)?;
        let path = self.receipt_path(receipt_id);
        if !state.receipt_paths.contains(&path) {
            if validate_regular_or_missing(&path).map_err(map_read_error)? {
                return Err(DeliveryRecorderSpoolError::Corrupt);
            }
            return Ok(false);
        }
        if !validate_regular_or_missing(&path).map_err(map_read_error)? {
            return Err(DeliveryRecorderSpoolError::Corrupt);
        }
        fs::remove_file(&path).map_err(|_| DeliveryRecorderSpoolError::Io)?;
        state.receipt_paths.remove(&path);
        sync_directory(&self.root, DIRECTORY_POLICY).map_err(|_| DeliveryRecorderSpoolError::Io)?;
        Ok(true)
    }

    fn receipt_path(&self, receipt_id: [u8; 16]) -> PathBuf {
        self.root.join(format!(
            "{}{}",
            tracedecay_domain::canonical_text::encode_lowercase_hex(&receipt_id),
            RECEIPT_SUFFIX
        ))
    }
}

fn scan_receipt_paths(root: &Path) -> Result<Vec<PathBuf>, DeliveryRecorderSpoolError> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(|_| DeliveryRecorderSpoolError::Io)? {
        let entry = entry.map_err(|_| DeliveryRecorderSpoolError::Io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| DeliveryRecorderSpoolError::UnsafePath)?;
        if name == LOCK_FILE {
            continue;
        }
        if !valid_receipt_name(&name)
            || !entry
                .file_type()
                .map_err(|_| DeliveryRecorderSpoolError::Io)?
                .is_file()
        {
            return Err(DeliveryRecorderSpoolError::UnsafePath);
        }
        paths.push(entry.path());
        if paths.len() > MAX_PENDING_RECEIPTS {
            return Err(DeliveryRecorderSpoolError::Full);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(super) fn recorder_spool_root(db_path: &Path) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(".delivery-settlement-spool-v1");
    PathBuf::from(path)
}

fn ensure_root(root: &Path) -> Result<(), DeliveryRecorderSpoolError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(DeliveryRecorderSpoolError::UnsafePath);
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(DeliveryRecorderSpoolError::Io),
    }
    fs::create_dir_all(root).map_err(|_| DeliveryRecorderSpoolError::Io)?;
    #[cfg(unix)]
    {
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|_| DeliveryRecorderSpoolError::Io)?;
    }
    sync_directory(root, DIRECTORY_POLICY).map_err(|_| DeliveryRecorderSpoolError::Io)
}

fn decode_receipt(
    bytes: &[u8],
) -> Result<DeliveryRecorderSourceReceiptV1, DeliveryRecorderSpoolError> {
    let receipt = serde_json::from_slice::<DeliveryRecorderSourceReceiptV1>(bytes)
        .map_err(|_| DeliveryRecorderSpoolError::Corrupt)?;
    receipt.validate()?;
    Ok(receipt)
}

fn map_read_error(error: std::io::Error) -> DeliveryRecorderSpoolError {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        DeliveryRecorderSpoolError::UnsafePath
    } else {
        DeliveryRecorderSpoolError::Io
    }
}

fn valid_receipt_name(name: &str) -> bool {
    name.len() == 32 + RECEIPT_SUFFIX.len()
        && name.ends_with(RECEIPT_SUFFIX)
        && name[..32]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_hex_prefix(hex: &str, output: &mut [u8; 16]) -> Result<(), DeliveryRecorderSpoolError> {
    if hex.len() < 32 {
        return Err(DeliveryRecorderSpoolError::InvalidReceipt);
    }
    for (index, slot) in output.iter_mut().enumerate() {
        let offset = index * 2;
        let high = decode_nibble(hex.as_bytes()[offset])?;
        let low = decode_nibble(hex.as_bytes()[offset + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(())
}

fn decode_nibble(byte: u8) -> Result<u8, DeliveryRecorderSpoolError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(DeliveryRecorderSpoolError::InvalidReceipt),
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        DeliveryChannelIdentityV1, DeliveryEventClassV1, DeliverySettlementAttemptV1,
        DeliverySettlementOutcomeV1, DeliverySurfaceFamilyV1, UtcMicros,
    };

    use super::*;

    fn settlement() -> DeliverySettlementV1 {
        DeliverySettlementV1 {
            attempt: DeliverySettlementAttemptV1 {
                owner_event_id: "work:delivery-spool:test".to_owned(),
                event_class: DeliveryEventClassV1::OperationTerminal,
                channel: DeliveryChannelIdentityV1 {
                    surface: DeliverySurfaceFamilyV1::Mcp,
                    channel_ref: "mcp:delivery-spool:test".to_owned(),
                },
                work_attempt: None,
                eligible: 1,
                valid_at: UtcMicros(100),
                attempted_at: UtcMicros(110),
            },
            outcome: DeliverySettlementOutcomeV1::Delivered,
            settled_at: UtcMicros(120),
            drop_reason: None,
        }
    }

    fn identity() -> ObservabilityProducerIdentityV1 {
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: "project.delivery-spool".to_owned(),
            process_boot_id: "boot:delivery-spool".to_owned(),
            producer_revision: "delivery-spool-producer.v1".to_owned(),
            configuration_revision: "delivery-spool-config.v1".to_owned(),
            policy_revision: "delivery-spool-policy.v1".to_owned(),
        }
    }

    #[test]
    fn v2_receipt_refuses_tampered_emission_identity() {
        let mut receipt =
            DeliveryRecorderSourceReceiptV1::new(settlement(), identity()).expect("v2 receipt");
        receipt
            .emission_identity
            .as_mut()
            .expect("v2 emission identity")
            .policy_revision = "delivery-spool-tampered-policy.v1".to_owned();

        assert_eq!(
            receipt.validate(),
            Err(DeliveryRecorderSpoolError::InvalidReceipt)
        );
    }
}
