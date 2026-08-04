use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{ActorId, ManifestDigest, RetrievalAnchorId, UtcMicros};
use tracedecay_tool_catalog::{EffectClass, UseCaseId};

use crate::context::{Deadline, RequestId, ResolvedScope};
use crate::error::ApplicationContractError;
use crate::identity::application_identifier;

use super::AuthorityReceipt;

application_identifier!(
    PreviewId => ("preview id", 512),
    EffectId => ("effect id", 512),
    IdempotencyKey => ("idempotency key", 512),
);

/// Exact stage at which cancellation or deadline state was observed.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum CancellationStage {
    BeforeAdmission,
    BeforeRead,
    DuringRead,
    BeforeEffect,
    EffectInFlight,
    Reconciling,
    AfterCommit,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CancellationObservation {
    pub stage: CancellationStage,
    pub observed_at: UtcMicros,
}

/// Bounded work accounting supplied by an owning port or transaction.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationBudgetUsage {
    pub units_consumed: u64,
    pub bytes_consumed: u64,
    pub elapsed_micros: u64,
}

/// Terminal state after an operation has been admitted.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum OperationTermination {
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Unavailable,
    Partial,
    EffectUnknown,
}

/// Canonical operation evidence. An admitted failure remains represented here
/// rather than being replaced by a transport exception.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationReceipt {
    pub started_at: UtcMicros,
    pub ended_at: UtcMicros,
    pub effective_deadline: Deadline,
    pub cancellation: Option<CancellationObservation>,
    pub budget: OperationBudgetUsage,
    pub termination: OperationTermination,
}

impl OperationReceipt {
    pub fn completed(
        started_at: UtcMicros,
        ended_at: UtcMicros,
        effective_deadline: Deadline,
        budget: OperationBudgetUsage,
    ) -> Result<Self, ApplicationContractError> {
        let receipt = Self {
            started_at,
            ended_at,
            effective_deadline,
            cancellation: None,
            budget,
            termination: OperationTermination::Completed,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.ended_at < self.started_at {
            return Err(ApplicationContractError::InvalidRange {
                field: "operation receipt interval",
            });
        }
        match (self.termination, self.cancellation.as_ref()) {
            (OperationTermination::Cancelled | OperationTermination::TimedOut, None) => {
                Err(ApplicationContractError::Inconsistent {
                    field: "terminal cancellation observation",
                })
            }
            _ => Ok(()),
        }
    }
}

/// Durable effect receipt terminal state.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum EffectTermination {
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    EffectUnknown,
}

impl From<EffectTermination> for OperationTermination {
    fn from(value: EffectTermination) -> Self {
        match value {
            EffectTermination::Completed => Self::Completed,
            EffectTermination::Cancelled => Self::Cancelled,
            EffectTermination::TimedOut => Self::TimedOut,
            EffectTermination::Failed => Self::Failed,
            EffectTermination::Partial => Self::Partial,
            EffectTermination::EffectUnknown => Self::EffectUnknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OperationTermination;

    #[test]
    fn unavailable_read_receipt_has_a_distinct_wire_state() {
        let encoded =
            serde_json::to_string(&OperationTermination::Unavailable).expect("encode termination");

        assert_eq!(encoded, "\"unavailable\"");
        assert_eq!(
            serde_json::from_str::<OperationTermination>(&encoded).expect("decode termination"),
            OperationTermination::Unavailable
        );
    }
}

/// Reconciliation state retained after an admitted effect.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationState {
    Pending,
    Reconciled,
    Failed,
}

/// Read-only preview of a future typed effect.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreviewResult<T> {
    pub preview_id: PreviewId,
    pub preview_digest: ManifestDigest,
    pub effect_class: EffectClass,
    pub authority: AuthorityReceipt,
    pub expected_state: ManifestDigest,
    pub execution: OperationReceipt,
    pub payload: Option<T>,
}

impl<T> PreviewResult<T> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        preview_id: PreviewId,
        preview_digest: ManifestDigest,
        effect_class: EffectClass,
        authority: AuthorityReceipt,
        expected_state: ManifestDigest,
        execution: OperationReceipt,
        payload: Option<T>,
    ) -> Result<Self, ApplicationContractError> {
        if !effect_class.is_effect() {
            return Err(ApplicationContractError::Inconsistent {
                field: "preview effect class",
            });
        }
        execution.validate()?;
        preview_digest.validate()?;
        expected_state.validate()?;
        Ok(Self {
            preview_id,
            preview_digest,
            effect_class,
            authority,
            expected_state,
            execution,
            payload,
        })
    }
}

/// Durable effect proof. It records identities and receipts, never credentials
/// or arbitrary command text.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectReceipt {
    pub operation: UseCaseId,
    pub request_id: RequestId,
    pub actor: ActorId,
    pub scope: ResolvedScope,
    pub effect_class: EffectClass,
    pub idempotency_key: IdempotencyKey,
    pub input_digest: ManifestDigest,
    pub expected_state: ManifestDigest,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub catalog_digest: ManifestDigest,
    pub privacy_digest: ManifestDigest,
    pub outcome: EffectTermination,
    pub committed_state: Option<ManifestDigest>,
    pub external_proof: Option<RetrievalAnchorId>,
}

impl EffectReceipt {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if !self.effect_class.is_effect() {
            return Err(ApplicationContractError::Inconsistent {
                field: "effect receipt class",
            });
        }
        self.scope.validate()?;
        for digest in [
            &self.input_digest,
            &self.expected_state,
            &self.policy_digest,
            &self.configuration_digest,
            &self.catalog_digest,
            &self.privacy_digest,
        ] {
            digest.validate()?;
        }
        if let Some(state) = &self.committed_state {
            state.validate()?;
        }
        if let Some(proof) = &self.external_proof {
            proof.validate()?;
        }
        if self.outcome == EffectTermination::Completed
            && self.committed_state.is_none()
            && self.external_proof.is_none()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "completed effect receipt proof",
            });
        }
        Ok(())
    }
}

/// Result of an admitted effect. `EffectUnknown` remains a receipt state and
/// cannot be remapped into a pre-admission problem.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffectResult<T> {
    pub effect_id: EffectId,
    pub effect_class: EffectClass,
    pub idempotency_key: IdempotencyKey,
    pub authority: AuthorityReceipt,
    pub expected_state: ManifestDigest,
    pub execution: OperationReceipt,
    pub reconciliation: ReconciliationState,
    pub receipt: EffectReceipt,
    pub payload: Option<T>,
}

impl<T> EffectResult<T> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        effect_id: EffectId,
        effect_class: EffectClass,
        idempotency_key: IdempotencyKey,
        authority: AuthorityReceipt,
        expected_state: ManifestDigest,
        execution: OperationReceipt,
        reconciliation: ReconciliationState,
        receipt: EffectReceipt,
        payload: Option<T>,
    ) -> Result<Self, ApplicationContractError> {
        if !effect_class.is_effect()
            || receipt.effect_class != effect_class
            || receipt.idempotency_key != idempotency_key
            || receipt.expected_state != expected_state
            || execution.termination != receipt.outcome.into()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "effect result receipt binding",
            });
        }
        if receipt.outcome == EffectTermination::EffectUnknown
            && reconciliation != ReconciliationState::Pending
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "unknown effect reconciliation",
            });
        }
        expected_state.validate()?;
        execution.validate()?;
        receipt.validate()?;
        Ok(Self {
            effect_id,
            effect_class,
            idempotency_key,
            authority,
            expected_state,
            execution,
            reconciliation,
            receipt,
            payload,
        })
    }
}
