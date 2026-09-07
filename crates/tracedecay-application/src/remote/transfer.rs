//! Authenticated transfer of an already encrypted offline frame.
//!
//! A reconnecting node transfers only its exact encrypted spool record. The
//! receiving authority validates the enrolled identity, writer fence, frame
//! digest, sequence predecessor, and canonical decrypted capture before it
//! admits the record to its own durable spool for ordinary replay.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    BrainNodeId, CurrentRemoteAuthorityStateV1, EntityId, ManifestDigest, UtcMicros,
};

use crate::ApplicationContractError;

use super::{
    capture::{RemoteCapturePersistenceErrorV1, RemoteCaptureSequenceV1, RemoteWriterAuthorityV1},
    protocol::RemoteProtocolBodyV1,
};

const MAX_TRANSFER_CIPHERTEXT_BYTES: usize = 1024 * 1024;

/// Opaque source record, never a caller-provided observation payload.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFrameTransferRequestV1 {
    pub event_id: String,
    pub enrollment_id: EntityId,
    pub enrollment_revision: u64,
    pub node_id: BrainNodeId,
    pub writer: RemoteWriterAuthorityV1,
    pub policy_revision: u64,
    pub sequence: RemoteCaptureSequenceV1,
    pub frame_digest: ManifestDigest,
    pub key_revision: u64,
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
    pub observed_authority_epoch: u64,
    pub expires_at_micros: i64,
}

impl RemoteFrameTransferRequestV1 {
    pub fn validate(&self, now_micros: i64) -> Result<(), ApplicationContractError> {
        if self.event_id.len() < 16
            || self.event_id.len() > 160
            || self.event_id.trim() != self.event_id
            || self.event_id.chars().any(char::is_control)
            || self.enrollment_revision == 0
            || self.policy_revision == 0
            || self.key_revision == 0
            || self.observed_authority_epoch == 0
            || self.ciphertext.is_empty()
            || self.ciphertext.len() > MAX_TRANSFER_CIPHERTEXT_BYTES
            || now_micros >= self.expires_at_micros
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote encrypted frame transfer",
            });
        }
        self.enrollment_id.validate()?;
        self.node_id.validate()?;
        self.writer
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "remote encrypted frame writer",
            })?;
        self.sequence
            .validate()
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "remote encrypted frame sequence",
            })?;
        self.frame_digest.validate()?;
        if self.writer.authority.fence.authority_epoch.0 != self.observed_authority_epoch {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote encrypted frame observed authority epoch",
            });
        }
        if self.key_revision != self.enrollment_revision {
            return Err(ApplicationContractError::Inconsistent {
                field: "remote encrypted frame key revision",
            });
        }
        Ok(())
    }
}

impl RemoteProtocolBodyV1 for RemoteFrameTransferRequestV1 {
    fn validate_remote_protocol_body(
        &self,
        sent_at: UtcMicros,
    ) -> Result<(), ApplicationContractError> {
        self.validate(sent_at.0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFrameTransferDispositionV1 {
    TransferredPending,
    AlreadyTransferred,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFrameTransferReceiptV1 {
    pub event_id: String,
    pub sequence: u64,
    pub disposition: RemoteFrameTransferDispositionV1,
}

impl RemoteFrameTransferReceiptV1 {
    pub fn validate_for(
        &self,
        request: &RemoteFrameTransferRequestV1,
    ) -> Result<(), RemoteFrameTransferErrorV1> {
        if self.event_id != request.event_id || self.sequence != request.sequence.sequence {
            return Err(RemoteFrameTransferErrorV1::InvalidReceipt);
        }
        Ok(())
    }
}

pub trait RemoteFrameTransferPortV1: Send + Sync {
    fn current_writer_authority(
        &self,
        writer: &RemoteWriterAuthorityV1,
    ) -> Result<CurrentRemoteAuthorityStateV1, RemoteCapturePersistenceErrorV1>;

    fn transfer_pending(
        &self,
        request: &RemoteFrameTransferRequestV1,
    ) -> Result<RemoteFrameTransferReceiptV1, RemoteFrameTransferErrorV1>;
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RemoteFrameTransferErrorV1 {
    #[error("remote frame transfer authority is stale")]
    StaleAuthority,
    #[error("remote frame transfer sequence has a gap")]
    SequenceGap,
    #[error("remote frame transfer payload is invalid")]
    InvalidFrame,
    #[error("remote frame transfer receipt is invalid")]
    InvalidReceipt,
    #[error("remote frame transfer spool has no remaining capacity")]
    Overflow,
    #[error("remote frame transfer store is unavailable")]
    Unavailable,
    #[error("remote frame transfer store is corrupt")]
    Corruption,
}

pub fn remote_frame_transfer_result_contract_v1()
-> Result<crate::ResultContractRef, ApplicationContractError> {
    let schema =
        tracedecay_tool_catalog::SchemaId::new("remote.frame-transfer.result").map_err(|_| {
            ApplicationContractError::InvalidIdentifier {
                field: "remote frame transfer result schema",
            }
        })?;
    crate::ResultContractRef::new(schema, 1)
}

pub const REMOTE_FRAME_TRANSFER_USE_CASE_ID_V1: &str = "use-case.remote.frame-transfer";
