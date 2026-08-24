//! Durable source markers for Work-owned observability facts.
//!
//! Product writes retain retry, leak, and duplicate receipts as `Pending`. A project-owned
//! recovery worker may mark an exact receipt `Durable` only after the canonical
//! observability outbox has durably claimed its normalized owner fact.

use std::num::NonZeroU16;

use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, WorkAuthority, WorkCommandId, WorkDuplicateAdjudicationReceiptV1,
    canonical_sha256,
};

use crate::{WorkLeakAdjudicationReceiptV1, WorkRetryReceiptV1};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkOwnerObservationKindV1 {
    Retry,
    Leak,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkOwnerObservationMarkerV1 {
    pub kind: WorkOwnerObservationKindV1,
    pub authority: WorkAuthority,
    pub command_id: WorkCommandId,
    pub receipt_revision: u64,
    pub receipt_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkOwnerObservationReceiptV1 {
    Retry(WorkRetryReceiptV1),
    Leak(WorkLeakAdjudicationReceiptV1),
    Duplicate(WorkDuplicateAdjudicationReceiptV1),
}

impl Serialize for WorkOwnerObservationReceiptV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(tag = "kind", content = "receipt", rename_all = "snake_case")]
        enum Wire<'a> {
            Retry(&'a WorkRetryReceiptV1),
            Leak(&'a WorkLeakAdjudicationReceiptV1),
            Duplicate(&'a WorkDuplicateAdjudicationReceiptV1),
        }
        match self {
            Self::Retry(receipt) => Wire::Retry(receipt).serialize(serializer),
            Self::Leak(receipt) => Wire::Leak(receipt).serialize(serializer),
            Self::Duplicate(receipt) => Wire::Duplicate(receipt).serialize(serializer),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingWorkOwnerObservationV1 {
    pub scan_cursor: WorkOwnerObservationScanCursorV1,
    pub marker: WorkOwnerObservationMarkerV1,
    pub receipt: WorkOwnerObservationReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkOwnerObservationScanCursorV1 {
    pub ordered_at_micros: i64,
    pub kind: WorkOwnerObservationKindV1,
    pub command_id: WorkCommandId,
    pub authority: WorkAuthority,
}

impl PendingWorkOwnerObservationV1 {
    pub fn validate(&self) -> bool {
        if self.marker.receipt_revision == 0 || self.marker.receipt_digest.validate().is_err() {
            return false;
        }
        if self.scan_cursor.kind != self.marker.kind
            || self.scan_cursor.command_id != self.marker.command_id
            || self.scan_cursor.authority != self.marker.authority
        {
            return false;
        }
        canonical_sha256(&self.receipt).is_ok_and(|digest| digest == self.marker.receipt_digest)
            && match &self.receipt {
                WorkOwnerObservationReceiptV1::Retry(receipt) => {
                    self.marker.kind == WorkOwnerObservationKindV1::Retry
                        && self.marker.command_id == receipt.command.command_id
                        && self.marker.receipt_revision == 1
                        && self.scan_cursor.ordered_at_micros == receipt.restarted_at.0
                        && receipt.validate_for_observation()
                }
                WorkOwnerObservationReceiptV1::Leak(receipt) => {
                    self.marker.kind == WorkOwnerObservationKindV1::Leak
                        && self.marker.command_id == receipt.command.command_id
                        && self.marker.receipt_revision == receipt.revision
                        && self.scan_cursor.ordered_at_micros
                            == receipt.evidence.scan_completed_at.0
                        && receipt.validate_for_observation()
                }
                WorkOwnerObservationReceiptV1::Duplicate(receipt) => {
                    let canonical = WorkDuplicateAdjudicationReceiptV1::new(
                        &self.marker.authority,
                        receipt.command().clone(),
                        receipt.revision(),
                        receipt.canonical_input_digest().clone(),
                    );
                    self.marker.kind == WorkOwnerObservationKindV1::Duplicate
                        && self.marker.command_id == receipt.command().command_id
                        && self.marker.receipt_revision == receipt.revision().get()
                        && self.scan_cursor.ordered_at_micros == receipt.command().occurred_at.0
                        && receipt.actor_id() == self.marker.authority.actor_id()
                        && canonical.as_ref() == Ok(receipt)
                }
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkOwnerObservationMarkOutcomeV1 {
    Marked,
    Replayed,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkOwnerObservationStorageErrorV1 {
    #[error("the Work owner-observation marker changed")]
    Conflict,
    #[error("the Work owner-observation storage is unavailable")]
    Unavailable,
}

pub trait WorkOwnerObservationStoragePortV1: Send + Sync {
    fn pending_owner_observations(
        &self,
        after: Option<&WorkOwnerObservationScanCursorV1>,
        limit: NonZeroU16,
    ) -> Result<Vec<PendingWorkOwnerObservationV1>, WorkOwnerObservationStorageErrorV1>;

    fn mark_owner_observation_durable(
        &self,
        marker: &WorkOwnerObservationMarkerV1,
    ) -> Result<WorkOwnerObservationMarkOutcomeV1, WorkOwnerObservationStorageErrorV1>;
}
