//! The sealed-holdout boundary (Plan 15 `locked-judgments-v1.json`).
//!
//! Development runs validate only the opaque locator and signed-envelope
//! metadata. They never resolve the locator or open, hash, or parse holdout
//! bytes. The reveal gate is intentionally unavailable in this packet.

use crate::evaluation::{
    DecisionOwnerId, EvaluationContractError, HoldoutAccessReceiptV1, HoldoutSealV1, RunId,
};

/// Development-visible state of the sealed holdout locator metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldoutSealStatus {
    /// The metadata is valid and the payload remains external.
    AuthorizedStoreOnly,
    /// The opaque locator or signed-envelope metadata is malformed.
    InvalidMetadata,
}

/// Validates metadata without resolving or accessing the authorized store.
pub(crate) fn seal_status(seal: &HoldoutSealV1) -> HoldoutSealStatus {
    if seal.validate().is_ok() {
        HoldoutSealStatus::AuthorizedStoreOnly
    } else {
        HoldoutSealStatus::InvalidMetadata
    }
}

/// The holdout reveal gate. This harness revision grants no reveal
/// capability; the locked-comparison packet will grant it to frozen locked
/// runs and record the returned receipt in the run's evidence batch.
pub(crate) struct HoldoutRevealGate;

impl HoldoutRevealGate {
    pub(crate) fn request(
        seal: &HoldoutSealV1,
        run_id: &RunId,
        revealed_by: &DecisionOwnerId,
    ) -> Result<HoldoutAccessReceiptV1, EvaluationContractError> {
        let _ = (seal, run_id, revealed_by);
        Err(EvaluationContractError::HoldoutAccessViolation(
            "the holdout reveal capability is not granted by this harness revision".to_string(),
        ))
    }
}
