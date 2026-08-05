//! Durable idempotency and reconciliation contracts for Workflow mutations.

use std::fmt;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{ActorId, ManifestDigest, UtcMicros, WorkflowDefinition, canonical_sha256};
use tracedecay_tool_catalog::UseCaseId;

use crate::{
    AuthorityReceipt, CancellationContext, Deadline, EffectId, IdempotencyKey, RequestId,
    ResolvedScope, TaskHandoffGrant, TaskHandoffRedeemed, TaskHandoffScope,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEffectOperationV1 {
    RegisterDefinition,
    HandoffIssue,
    HandoffRedeem,
}

impl WorkflowEffectOperationV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RegisterDefinition => "register_definition",
            Self::HandoffIssue => "handoff_issue",
            Self::HandoffRedeem => "handoff_redeem",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEffectIdentityV1 {
    operation: WorkflowEffectOperationV1,
    idempotency_key: IdempotencyKey,
    request_id: RequestId,
    actor: ActorId,
    scope: ResolvedScope,
    input_digest: ManifestDigest,
    started_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
    receipt_context: WorkflowEffectReceiptContextV1,
}

impl WorkflowEffectIdentityV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation: WorkflowEffectOperationV1,
        idempotency_key: IdempotencyKey,
        request_id: RequestId,
        actor: ActorId,
        scope: ResolvedScope,
        input_digest: ManifestDigest,
        started_at: UtcMicros,
        deadline: Deadline,
        cancellation: CancellationContext,
        receipt_context: WorkflowEffectReceiptContextV1,
    ) -> Result<Self, crate::ApplicationContractError> {
        let identity = Self {
            operation,
            idempotency_key,
            request_id,
            actor,
            scope,
            input_digest,
            started_at,
            deadline,
            cancellation,
            receipt_context,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub const fn operation(&self) -> WorkflowEffectOperationV1 {
        self.operation
    }

    pub fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }

    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    pub fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub const fn started_at(&self) -> UtcMicros {
        self.started_at
    }

    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    pub fn cancellation(&self) -> &CancellationContext {
        &self.cancellation
    }

    pub fn receipt_context(&self) -> &WorkflowEffectReceiptContextV1 {
        &self.receipt_context
    }

    pub fn validate(&self) -> Result<(), crate::ApplicationContractError> {
        self.actor.validate()?;
        self.scope.validate()?;
        self.input_digest.validate()?;
        self.receipt_context.authority.validate_for(&self.scope)?;
        self.receipt_context.expected_state.validate()?;
        self.receipt_context.configuration_digest.validate()?;
        self.receipt_context.catalog_digest.validate()?;
        self.receipt_context.privacy_digest.validate()?;
        if self.receipt_context.operation.as_str()
            != format!("use-case.workflow.{}", self.operation.as_str())
        {
            return Err(crate::ApplicationContractError::Inconsistent {
                field: "Workflow effect receipt operation",
            });
        }
        self.identity_digest()?;
        Ok(())
    }

    pub fn identity_digest(&self) -> Result<ManifestDigest, crate::ApplicationContractError> {
        canonical_sha256(&(
            "tracedecay.application.workflow-effect-identity.v1",
            self.operation,
            &self.idempotency_key,
            &self.actor,
            &self.scope,
            &self.input_digest,
        ))
        .map_err(Into::into)
    }

    pub fn payload_digest(&self) -> Result<ManifestDigest, crate::ApplicationContractError> {
        canonical_sha256(&("tracedecay.application.workflow-effect-payload.v1", self))
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEffectReceiptContextV1 {
    operation: UseCaseId,
    effect_id: EffectId,
    authority: AuthorityReceipt,
    expected_state: ManifestDigest,
    configuration_digest: ManifestDigest,
    catalog_digest: ManifestDigest,
    privacy_digest: ManifestDigest,
}

impl WorkflowEffectReceiptContextV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation: UseCaseId,
        effect_id: EffectId,
        authority: AuthorityReceipt,
        expected_state: ManifestDigest,
        configuration_digest: ManifestDigest,
        catalog_digest: ManifestDigest,
        privacy_digest: ManifestDigest,
    ) -> Self {
        Self {
            operation,
            effect_id,
            authority,
            expected_state,
            configuration_digest,
            catalog_digest,
            privacy_digest,
        }
    }

    pub fn operation(&self) -> &UseCaseId {
        &self.operation
    }

    pub fn effect_id(&self) -> &EffectId {
        &self.effect_id
    }

    pub fn authority(&self) -> &AuthorityReceipt {
        &self.authority
    }

    pub fn expected_state(&self) -> &ManifestDigest {
        &self.expected_state
    }

    pub fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }

    pub fn catalog_digest(&self) -> &ManifestDigest {
        &self.catalog_digest
    }

    pub fn privacy_digest(&self) -> &ManifestDigest {
        &self.privacy_digest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "operation", content = "input")]
pub enum WorkflowEffectPreparedV1 {
    RegisterDefinition(WorkflowDefinition),
    HandoffIssue(TaskHandoffGrant),
    HandoffRedeem {
        token_digest: ManifestDigest,
        expected_scope: TaskHandoffScope,
        consumed_at: UtcMicros,
    },
    Problem(WorkflowEffectProblemV1),
}

impl WorkflowEffectPreparedV1 {
    pub const fn operation(&self) -> Option<WorkflowEffectOperationV1> {
        match self {
            Self::RegisterDefinition(_) => Some(WorkflowEffectOperationV1::RegisterDefinition),
            Self::HandoffIssue(_) => Some(WorkflowEffectOperationV1::HandoffIssue),
            Self::HandoffRedeem { .. } => Some(WorkflowEffectOperationV1::HandoffRedeem),
            Self::Problem(_) => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "payload")]
pub enum WorkflowEffectSuccessV1 {
    DefinitionRegistered(WorkflowDefinition),
    HandoffIssued(TaskHandoffGrant),
    HandoffRedeemed(TaskHandoffRedeemed),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEffectProblemV1 {
    InvalidRequest,
    NotFoundOrNotAuthorized,
    Conflict,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "status", content = "payload")]
pub enum WorkflowEffectOutcomeV1 {
    Success(WorkflowEffectSuccessV1),
    Problem(WorkflowEffectProblemV1),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEffectTerminalV1 {
    identity: WorkflowEffectIdentityV1,
    ended_at: UtcMicros,
    outcome: WorkflowEffectOutcomeV1,
}

impl WorkflowEffectTerminalV1 {
    pub fn new(
        identity: WorkflowEffectIdentityV1,
        ended_at: UtcMicros,
        outcome: WorkflowEffectOutcomeV1,
    ) -> Result<Self, WorkflowEffectAuthorityErrorV1> {
        let terminal = Self {
            identity,
            ended_at,
            outcome,
        };
        terminal.validate()?;
        Ok(terminal)
    }

    pub fn identity(&self) -> &WorkflowEffectIdentityV1 {
        &self.identity
    }

    pub const fn ended_at(&self) -> UtcMicros {
        self.ended_at
    }

    pub fn outcome(&self) -> &WorkflowEffectOutcomeV1 {
        &self.outcome
    }

    pub fn validate(&self) -> Result<(), WorkflowEffectAuthorityErrorV1> {
        self.identity
            .validate()
            .map_err(|_| WorkflowEffectAuthorityErrorV1::InvalidTransition)?;
        if self.ended_at < self.identity.started_at {
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        let success_operation = match &self.outcome {
            WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::DefinitionRegistered(_)) => {
                Some(WorkflowEffectOperationV1::RegisterDefinition)
            }
            WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffIssued(_)) => {
                Some(WorkflowEffectOperationV1::HandoffIssue)
            }
            WorkflowEffectOutcomeV1::Success(WorkflowEffectSuccessV1::HandoffRedeemed(_)) => {
                Some(WorkflowEffectOperationV1::HandoffRedeem)
            }
            WorkflowEffectOutcomeV1::Problem(_) => None,
        };
        if success_operation.is_some_and(|operation| operation != self.identity.operation) {
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowEffectJournalStateV1 {
    BeforeEffect,
    InFlight,
    Committed,
    Reconciled,
}

impl WorkflowEffectJournalStateV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeEffect => "before_effect",
            Self::InFlight => "in_flight",
            Self::Committed => "committed",
            Self::Reconciled => "reconciled",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEffectJournalRecordV1 {
    state: WorkflowEffectJournalStateV1,
    terminal: Option<WorkflowEffectTerminalV1>,
}

impl WorkflowEffectJournalRecordV1 {
    pub fn before_effect() -> Self {
        Self {
            state: WorkflowEffectJournalStateV1::BeforeEffect,
            terminal: None,
        }
    }

    pub fn terminal(
        state: WorkflowEffectJournalStateV1,
        terminal: WorkflowEffectTerminalV1,
    ) -> Result<Self, WorkflowEffectAuthorityErrorV1> {
        if !matches!(
            state,
            WorkflowEffectJournalStateV1::Committed | WorkflowEffectJournalStateV1::Reconciled
        ) {
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        Ok(Self {
            state,
            terminal: Some(terminal),
        })
    }

    pub fn pending(
        state: WorkflowEffectJournalStateV1,
    ) -> Result<Self, WorkflowEffectAuthorityErrorV1> {
        if !matches!(
            state,
            WorkflowEffectJournalStateV1::BeforeEffect | WorkflowEffectJournalStateV1::InFlight
        ) {
            return Err(WorkflowEffectAuthorityErrorV1::InvalidTransition);
        }
        Ok(Self {
            state,
            terminal: None,
        })
    }

    pub const fn state(&self) -> WorkflowEffectJournalStateV1 {
        self.state
    }

    pub fn terminal(&self) -> Option<&WorkflowEffectTerminalV1> {
        self.terminal.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowEffectAuthorityErrorV1 {
    IdentityConflict,
    InvalidTransition,
    Unavailable(String),
}

impl fmt::Display for WorkflowEffectAuthorityErrorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdentityConflict => formatter.write_str("workflow effect identity conflicts"),
            Self::InvalidTransition => {
                formatter.write_str("workflow effect journal transition is invalid")
            }
            Self::Unavailable(message) => {
                write!(formatter, "workflow effect journal unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for WorkflowEffectAuthorityErrorV1 {}

pub trait WorkflowEffectAuthorityPortV1: Send + Sync {
    fn reserve_effect(
        &self,
        identity: &WorkflowEffectIdentityV1,
    ) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1>;

    fn execute_effect(
        &self,
        identity: &WorkflowEffectIdentityV1,
        prepared: &WorkflowEffectPreparedV1,
        ended_at: UtcMicros,
    ) -> Result<WorkflowEffectJournalRecordV1, WorkflowEffectAuthorityErrorV1>;
}
