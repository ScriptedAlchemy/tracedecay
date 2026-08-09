use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, CatalogGenerationId, ConfigurationRevisionId, ManifestDigest,
    PolicyRevisionId, ProposalId, TaskEvidenceLinkId, TaskId, UtcMicros, WorkAttemptIdentityV1,
    WorkCommandId, WorkGraphVersionV1, WorkHandoffV1, WorkInitiativeV1, WorkItemV1,
    WorkMilestoneV1, WorkPlanV1, WorkProductEventEvidenceV1, WorkProductEventId,
    WorkProductEventPayloadV1, WorkProductEventV1, WorkProductGraphV1, WorkProductProfileScopeV1,
    WorkProductSourceWatermarkV1, WorkProposalDispositionV1, WorkProposalV1,
    WorkRelationReplanProposalV1,
};

use super::{VerifiedWorkGraphVersionV1, WorkProductPortContextV1, WorkProductSelectionScopeV1};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductRevisionPinsV1 {
    #[schemars(with = "String")]
    pub policy_revision_id: PolicyRevisionId,
    pub configuration_revision_id: ConfigurationRevisionId,
    pub catalog_generation_id: CatalogGenerationId,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[serde(tag = "authority", rename_all = "snake_case")]
pub enum WorkProductExpectedAuthorityV1 {
    NoPriorGraph,
    Verified {
        verified_version: VerifiedWorkGraphVersionV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductMutationIdentityV1 {
    pub expected_authority: WorkProductExpectedAuthorityV1,
    pub command_id: WorkCommandId,
    pub causation_event_id: Option<WorkProductEventId>,
    pub evidence: Vec<WorkProductEventEvidenceV1>,
    pub occurred_at: UtcMicros,
    pub revisions: WorkProductRevisionPinsV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductEventDraftV1 {
    pub actor_id: ActorId,
    pub owner_scope: WorkProductProfileScopeV1,
    pub authorized_relation_scopes: Vec<tracedecay_domain::WorkProductAuthorizedRelationScopeV1>,
    pub expected_graph_version: Option<WorkGraphVersionV1>,
    pub result_graph_version: WorkGraphVersionV1,
    pub command_id: WorkCommandId,
    pub canonical_input_digest: ManifestDigest,
    pub causation_event_id: Option<WorkProductEventId>,
    pub evidence: Vec<WorkProductEventEvidenceV1>,
    pub source_watermark: WorkProductSourceWatermarkV1,
    pub occurred_at: UtcMicros,
    pub policy_revision_id: PolicyRevisionId,
    pub configuration_revision_id: ConfigurationRevisionId,
    pub catalog_generation_id: CatalogGenerationId,
    pub payload: WorkProductEventPayloadV1,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductEventPortErrorV1 {
    #[error("Work event was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work event graph version changed")]
    VersionConflict,
    #[error("Work event idempotency key conflicts")]
    IdempotencyConflict,
    #[error("Work event authority is unavailable")]
    Unavailable,
    #[error("Work event append was cancelled")]
    Cancelled,
    #[error("Work event append timed out")]
    TimedOut,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkProductEventCommitOutcomeV1 {
    Appended(WorkProductEventCommitV1),
    Replayed(WorkProductEventCommitV1),
}

impl WorkProductEventCommitOutcomeV1 {
    pub(super) fn into_parts(self) -> (WorkProductEventCommitV1, bool) {
        match self {
            Self::Appended(commit) => (commit, false),
            Self::Replayed(commit) => (commit, true),
        }
    }
}

/// One atomic journal and verified-projection commit.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkProductEventCommitV1 {
    event: WorkProductEventV1,
    verified_graph_version: VerifiedWorkGraphVersionV1,
}

impl WorkProductEventCommitV1 {
    pub fn new(
        event: WorkProductEventV1,
        verified_graph_version: VerifiedWorkGraphVersionV1,
    ) -> Result<Self, WorkProductEventPortErrorV1> {
        let commit = Self {
            event,
            verified_graph_version,
        };
        commit.validate()?;
        Ok(commit)
    }

    pub const fn event(&self) -> &WorkProductEventV1 {
        &self.event
    }

    pub const fn verified_graph_version(&self) -> &VerifiedWorkGraphVersionV1 {
        &self.verified_graph_version
    }

    pub(super) fn validate(&self) -> Result<(), WorkProductEventPortErrorV1> {
        if self.verified_graph_version.graph_version() != self.event.result_graph_version()
            || self.verified_graph_version.event_sequence() != self.event.sequence()
            || self.verified_graph_version.source_watermark() != self.event.source_watermark()
        {
            return Err(WorkProductEventPortErrorV1::Unavailable);
        }
        Ok(())
    }

    pub(super) fn into_parts(self) -> (WorkProductEventV1, VerifiedWorkGraphVersionV1) {
        (self.event, self.verified_graph_version)
    }
}

/// Relational immutable event/idempotency and verified-projection authority.
///
/// A successful call commits both records in one transaction. There is no
/// intermediate appended-but-unpublished state for a restart to reconcile.
pub trait WorkProductEventPortV1: Send + Sync {
    fn replay(
        &self,
        context: &WorkProductPortContextV1,
        command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventCommitV1>, WorkProductEventPortErrorV1>;

    fn append_atomically(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventCommitOutcomeV1, WorkProductEventPortErrorV1>;
}

impl<E> WorkProductEventPortV1 for &E
where
    E: WorkProductEventPortV1 + ?Sized,
{
    fn replay(
        &self,
        context: &WorkProductPortContextV1,
        command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventCommitV1>, WorkProductEventPortErrorV1> {
        (**self).replay(context, command_id, canonical_input_digest)
    }

    fn append_atomically(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventCommitOutcomeV1, WorkProductEventPortErrorV1> {
        (**self).append_atomically(context, draft)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductMutationReceiptV1 {
    pub(super) event: WorkProductEventV1,
    pub(super) verified_graph_version: VerifiedWorkGraphVersionV1,
    pub(super) replayed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkProductMutationReceiptWireV1 {
    event: WorkProductEventV1,
    verified_graph_version: VerifiedWorkGraphVersionV1,
    replayed: bool,
}

impl<'de> Deserialize<'de> for WorkProductMutationReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WorkProductMutationReceiptWireV1::deserialize(deserializer)?;
        let commit = WorkProductEventCommitV1::new(wire.event, wire.verified_graph_version)
            .map_err(serde::de::Error::custom)?;
        let (event, verified_graph_version) = commit.into_parts();
        Ok(Self {
            event,
            verified_graph_version,
            replayed: wire.replayed,
        })
    }
}

impl WorkProductMutationReceiptV1 {
    pub const fn event(&self) -> &WorkProductEventV1 {
        &self.event
    }

    pub const fn verified_graph_version(&self) -> &VerifiedWorkGraphVersionV1 {
        &self.verified_graph_version
    }

    pub const fn replayed(&self) -> bool {
        self.replayed
    }
}

macro_rules! mutation_request {
    ($name:ident { $($field:ident : $ty:ty),+ $(,)? }) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub selection: WorkProductSelectionScopeV1,
            $(pub $field: $ty,)+
            pub mutation: WorkProductMutationIdentityV1,
        }
    };
}

mutation_request!(CreateWorkProductRequestV1 {
    initial_graph: WorkProductGraphV1
});
mutation_request!(AddWorkTaskRequestV1 { item: WorkItemV1 });
mutation_request!(CreateWorkTaskRequestV1 {
    initiative: WorkInitiativeV1,
    plan: WorkPlanV1,
    milestone: WorkMilestoneV1,
    item: WorkItemV1,
});
mutation_request!(DecideWorkProposalRequestV1 {
    proposal: WorkProposalV1,
    disposition: WorkProposalDispositionV1,
});
mutation_request!(DecideWorkRelationReplanRequestV1 {
    proposal: WorkRelationReplanProposalV1,
    disposition: WorkProposalDispositionV1,
});
mutation_request!(ApplyWorkRelationReplanRequestV1 {
    proposal_id: ProposalId,
});
mutation_request!(AcceptWorkTaskRequestV1 {
    task_id: TaskId,
    evidence_by_criterion: BTreeMap<AcceptanceCriterionId, TaskEvidenceLinkId>,
});
mutation_request!(AdmitWorkExecutionRequestV1 {
    task_id: TaskId,
    based_on_version: WorkGraphVersionV1,
});
mutation_request!(LinkAcceptedWorkAttemptRequestV1 {
    task_id: TaskId,
    based_on_version: WorkGraphVersionV1,
    identity: WorkAttemptIdentityV1,
});
mutation_request!(RecordWorkHandoffRequestV1 {
    handoff: WorkHandoffV1
});

/// Operator-selected graph change before the owning Work authority binds the
/// current verified head and revision authorities.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum WorkProductChangeDraftV1 {
    AddTask {
        item: WorkItemV1,
    },
    CreateTask {
        initiative: WorkInitiativeV1,
        plan: WorkPlanV1,
        milestone: WorkMilestoneV1,
        item: WorkItemV1,
    },
    DecideProposal {
        proposal: WorkProposalV1,
        disposition: WorkProposalDispositionV1,
    },
    DecideRelationReplan {
        proposal: WorkRelationReplanProposalV1,
        disposition: WorkProposalDispositionV1,
    },
    ApplyRelationReplan {
        proposal_id: ProposalId,
    },
    AcceptTask {
        task_id: TaskId,
        evidence_by_criterion: BTreeMap<AcceptanceCriterionId, TaskEvidenceLinkId>,
    },
    AdmitExecution {
        task_id: TaskId,
    },
    LinkAcceptedAttempt {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
    },
    RecordHandoff {
        handoff: WorkHandoffV1,
    },
}

/// Read-only preparation input. Authority identities, clocks, and revision
/// pins are deliberately absent because the backend owns them.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrepareWorkProductMutationRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub change: WorkProductChangeDraftV1,
    pub causation_event_id: Option<WorkProductEventId>,
    pub evidence: Vec<WorkProductEventEvidenceV1>,
}

/// Closed public mutation surface for the Work-product graph authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "mutation", content = "request", rename_all = "snake_case")]
#[schemars(extend("type" = "object"))]
pub enum WorkProductMutationRequestV1 {
    Create(CreateWorkProductRequestV1),
    AddTask(AddWorkTaskRequestV1),
    CreateTask(CreateWorkTaskRequestV1),
    DecideProposal(DecideWorkProposalRequestV1),
    DecideRelationReplan(DecideWorkRelationReplanRequestV1),
    ApplyRelationReplan(ApplyWorkRelationReplanRequestV1),
    AcceptTask(AcceptWorkTaskRequestV1),
    AdmitExecution(AdmitWorkExecutionRequestV1),
    LinkAcceptedAttempt(LinkAcceptedWorkAttemptRequestV1),
    RecordHandoff(RecordWorkHandoffRequestV1),
}

impl WorkProductMutationRequestV1 {
    pub const fn mutation_identity(&self) -> &WorkProductMutationIdentityV1 {
        match self {
            Self::Create(request) => &request.mutation,
            Self::AddTask(request) => &request.mutation,
            Self::CreateTask(request) => &request.mutation,
            Self::DecideProposal(request) => &request.mutation,
            Self::DecideRelationReplan(request) => &request.mutation,
            Self::ApplyRelationReplan(request) => &request.mutation,
            Self::AcceptTask(request) => &request.mutation,
            Self::AdmitExecution(request) => &request.mutation,
            Self::LinkAcceptedAttempt(request) => &request.mutation,
            Self::RecordHandoff(request) => &request.mutation,
        }
    }
}
