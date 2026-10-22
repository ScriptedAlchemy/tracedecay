//! User-controlled source-edit invocation shapes admitted at the transport
//! boundary.
//!
//! These types name the edit the caller asked for. Authority, policy proof,
//! actor, and scope are minted by the daemon after project admission and are
//! never accepted here. Live cancellation stays on the invocation envelope.
//! [`CancellationSignal`] is process-local and is not part of this contract.

use serde::{Deserialize, Serialize};
use tracedecay_domain::ManifestDigest;

use crate::result::{EffectId, IdempotencyKey};
use crate::source_edit::effect_authorization::SourceEditReconciliationDispositionV1;
use crate::source_edit::{SourceEditKind, SourceEditRequest};

/// User-controlled fields for one source-edit apply or preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditInvocationV1 {
    pub edit: SourceEditRequest,
    pub idempotency_key: Option<IdempotencyKey>,
    pub expected_state: Option<ManifestDigest>,
}

/// User-controlled identity and inspection conclusion for one uncertain edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEditReconciliationInvocationV1 {
    pub kind: SourceEditKind,
    pub effect_id: EffectId,
    pub idempotency_key: IdempotencyKey,
    pub attempt_idempotency_key: IdempotencyKey,
    pub input_digest: ManifestDigest,
    pub disposition: SourceEditReconciliationDispositionV1,
}
