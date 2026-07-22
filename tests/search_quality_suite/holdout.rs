//! The sealed-holdout boundary (Plan 15 `locked-judgments-v1.json`).
//!
//! Development runs validate only the opaque locator and seal digest metadata.
//! They never resolve the locator or open, hash, or parse holdout bytes. The
//! access gate is intentionally unavailable in this packet.

use crate::evaluation::{
    DecisionOwnerId, EvaluationContractError, HoldoutAccessReceiptV1, HoldoutSealV1, RunId,
};

/// Development-visible state of the sealed holdout locator metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldoutSealStatus {
    /// The metadata is valid and the payload remains external.
    AuthorizedStoreOnly,
    /// The opaque locator or seal metadata is malformed.
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

/// The holdout access gate. This harness revision grants no label access; the
/// locked-comparison packet opens sealed labels only after a frozen run and
/// records an unsigned access receipt in the run's evidence batch.
pub(crate) struct HoldoutAccessGate;

impl HoldoutAccessGate {
    pub(crate) fn request(
        seal: &HoldoutSealV1,
        run_id: &RunId,
        accessed_by: &DecisionOwnerId,
    ) -> Result<HoldoutAccessReceiptV1, EvaluationContractError> {
        let _ = (seal, run_id, accessed_by);
        Err(EvaluationContractError::HoldoutAccessViolation(
            "holdout access is not granted by this harness revision".to_string(),
        ))
    }
}
