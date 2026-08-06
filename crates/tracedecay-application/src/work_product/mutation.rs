use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AcceptanceCriterionId, ActorId, CatalogGenerationId, ConfigurationRevisionId,
    MAX_WORK_PRODUCT_EVENT_EVIDENCE, ManifestDigest, PolicyRevisionId, ProposalId,
    TaskEvidenceLinkId, TaskEvidenceLinkV1, TaskId, UtcMicros, WorkAttemptIdentityV1,
    WorkCommandId, WorkGraphChangeV1, WorkGraphVersionV1, WorkHandoffV1,
    WorkProductEventEvidenceV1, WorkProductEventId, WorkProductEventPayloadV1, WorkProductEventV1,
    WorkProductGraphV1, WorkProductProfileScopeV1, WorkProductSourceWatermarkV1,
    WorkProposalDispositionV1, WorkProposalV1, WorkRelationReplanProposalV1, canonical_sha256,
};

use crate::{RequestAdmission, RequestContext};

use super::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkProductApplicationErrorV1, WorkProductBindingV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductSelectionScopeV1,
};

const WORK_PRODUCT_MUTATION_DIGEST_DOMAIN: &str =
    "tracedecay.application.work-product-mutation.final-v2";

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
pub enum WorkProductEventAppendOutcomeV1 {
    Appended(WorkProductEventV1),
    Replayed(WorkProductEventV1),
}

impl WorkProductEventAppendOutcomeV1 {
    fn into_parts(self) -> (WorkProductEventV1, bool) {
        match self {
            Self::Appended(event) => (event, false),
            Self::Replayed(event) => (event, true),
        }
    }
}

/// Relational immutable event/idempotency/outbox authority.
pub trait WorkProductEventPortV1: Send + Sync {
    fn replay(
        &self,
        context: &WorkProductPortContextV1,
        command_id: &WorkCommandId,
        canonical_input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventV1>, WorkProductEventPortErrorV1>;

    fn append_with_outbox(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventAppendOutcomeV1, WorkProductEventPortErrorV1>;
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
    ) -> Result<Option<WorkProductEventV1>, WorkProductEventPortErrorV1> {
        (**self).replay(context, command_id, canonical_input_digest)
    }

    fn append_with_outbox(
        &self,
        context: &WorkProductPortContextV1,
        draft: &WorkProductEventDraftV1,
    ) -> Result<WorkProductEventAppendOutcomeV1, WorkProductEventPortErrorV1> {
        (**self).append_with_outbox(context, draft)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkGraphPublishPortErrorV1 {
    #[error("Work graph publication version changed")]
    VersionConflict,
    #[error("Work graph publication is unavailable")]
    Unavailable,
    #[error("Work graph publication durability is uncertain")]
    DurabilityUncertain,
    #[error("Work graph publication was cancelled")]
    Cancelled,
    #[error("Work graph publication timed out")]
    TimedOut,
}

/// Exact verified Grafeo topology publication authority.
pub trait WorkGraphPublishPortV1: Send + Sync {
    fn publish_event(
        &self,
        context: &WorkProductPortContextV1,
        event: &WorkProductEventV1,
    ) -> Result<VerifiedWorkGraphVersionV1, WorkGraphPublishPortErrorV1>;
}

impl<P> WorkGraphPublishPortV1 for &P
where
    P: WorkGraphPublishPortV1 + ?Sized,
{
    fn publish_event(
        &self,
        context: &WorkProductPortContextV1,
        event: &WorkProductEventV1,
    ) -> Result<VerifiedWorkGraphVersionV1, WorkGraphPublishPortErrorV1> {
        (**self).publish_event(context, event)
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductMutationReceiptV1 {
    event: WorkProductEventV1,
    verified_graph_version: VerifiedWorkGraphVersionV1,
    replayed: bool,
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
mutation_request!(LinkAcceptedWorkAttemptRequestV1 {
    task_id: TaskId,
    based_on_version: WorkGraphVersionV1,
    identity: WorkAttemptIdentityV1,
    evidence: TaskEvidenceLinkV1,
});
mutation_request!(RecordWorkHandoffRequestV1 {
    handoff: WorkHandoffV1
});

pub struct WorkProductMutationServiceV1<G, A, E, P> {
    graph: G,
    owner_authority: A,
    events: E,
    publisher: P,
}

impl<G, A, E, P> WorkProductMutationServiceV1<G, A, E, P>
where
    G: WorkGraphReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
    E: WorkProductEventPortV1,
    P: WorkGraphPublishPortV1,
{
    pub const fn new(graph: G, owner_authority: A, events: E, publisher: P) -> Self {
        Self {
            graph,
            owner_authority,
            events,
            publisher,
        }
    }

    pub fn create(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: CreateWorkProductRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        self.commit_create(
            context,
            binding,
            request.selection,
            request.mutation,
            request.initial_graph,
        )
    }

    pub fn decide_proposal(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: DecideWorkProposalRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let change = if request.disposition == WorkProposalDispositionV1::Accepted {
            WorkGraphChangeV1::ProposalAccepted {
                proposal: request.proposal,
                accepted_at: request.mutation.occurred_at,
            }
        } else {
            WorkGraphChangeV1::ProposalDecided {
                proposal: request.proposal,
                disposition: request.disposition,
                decided_at: request.mutation.occurred_at,
            }
        };
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            change,
        )
    }

    pub fn decide_relation_replan(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: DecideWorkRelationReplanRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let decided_at = request.mutation.occurred_at;
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::RelationReplanDecided {
                proposal: request.proposal,
                disposition: request.disposition,
                decided_at,
            },
        )
    }

    pub fn apply_relation_replan(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: ApplyWorkRelationReplanRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let change = WorkGraphChangeV1::TaskRelationsReplanned {
            proposal_id: request.proposal_id,
            applied_at: request.mutation.occurred_at,
        };
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            change,
        )
    }

    pub fn accept_task(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: AcceptWorkTaskRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let change = WorkGraphChangeV1::TaskAccepted {
            task_id: request.task_id,
            evidence_by_criterion: request.evidence_by_criterion,
            accepted_at: request.mutation.occurred_at,
        };
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            change,
        )
    }

    /// Links one exact accepted attempt identity to canonical task evidence.
    /// It does not dispatch work and cannot accept the task.
    pub fn link_accepted_attempt(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: LinkAcceptedWorkAttemptRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let linked_at = request.mutation.occurred_at;
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::AcceptedAttemptLinked {
                task_id: request.task_id,
                based_on_version: request.based_on_version,
                identity: request.identity,
                evidence: request.evidence,
                linked_at,
            },
        )
    }

    pub fn record_handoff(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: RecordWorkHandoffRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        self.commit_change(
            context,
            binding,
            request.selection,
            request.mutation,
            WorkGraphChangeV1::HandoffRecorded {
                handoff: request.handoff,
            },
        )
    }

    fn commit_create(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        selection: WorkProductSelectionScopeV1,
        mutation: WorkProductMutationIdentityV1,
        graph: WorkProductGraphV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let payload = WorkProductEventPayloadV1::Created { graph };
        let (port_context, mutation, digest) =
            self.prepare(context, binding, &selection, mutation, &payload)?;
        let WorkProductEventPayloadV1::Created { graph } = &payload else {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        };
        if !matches!(
            &mutation.expected_authority,
            WorkProductExpectedAuthorityV1::NoPriorGraph
        ) || !mutation.evidence.is_empty()
            || graph.version() != WorkGraphVersionV1::initial()
            || graph.validate().is_err()
        {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        if let Some(event) = self.replay(&port_context, &mutation, &payload, &digest)? {
            return self.publish(&port_context, event, true);
        }
        let draft = event_draft(
            context,
            port_context.authorized_scope(),
            &selection,
            &mutation,
            digest,
            WorkGraphVersionV1::initial(),
            payload,
        )?;
        self.append_and_publish(&port_context, draft)
    }

    fn commit_change(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        selection: WorkProductSelectionScopeV1,
        mutation: WorkProductMutationIdentityV1,
        change: WorkGraphChangeV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let payload = WorkProductEventPayloadV1::Changed {
            change: change.clone(),
        };
        let (port_context, mutation, digest) =
            self.prepare(context, binding, &selection, mutation, &payload)?;
        let WorkProductExpectedAuthorityV1::Verified {
            verified_version: expected_verified_version,
        } = &mutation.expected_authority
        else {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        };
        let expected_graph_version = expected_verified_version.graph_version();
        validate_change_request(&change, expected_graph_version, mutation.occurred_at)?;
        if let Some(event) = self.replay(&port_context, &mutation, &payload, &digest)? {
            return self.publish(&port_context, event, true);
        }

        let read_request = WorkGraphReadRequestV1::current(selection.clone(), mutation.occurred_at);
        let read = self.graph.read_graph(&port_context, &read_request)?;
        super::read::validate_result(&read_request, port_context.authorized_scope(), &read)?;
        let WorkGraphReadV1::Current { snapshot, .. } = read else {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        };
        if snapshot.verified_version() != expected_verified_version {
            return Err(WorkProductApplicationErrorV1::VersionConflict);
        }
        let result_graph = snapshot
            .graph()
            .clone()
            .apply(change.clone())
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let draft = event_draft(
            context,
            port_context.authorized_scope(),
            &selection,
            &mutation,
            digest,
            result_graph.version(),
            payload,
        )?;
        self.append_and_publish(&port_context, draft)
    }

    fn prepare(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        selection: &WorkProductSelectionScopeV1,
        mut mutation: WorkProductMutationIdentityV1,
        payload: &WorkProductEventPayloadV1,
    ) -> Result<
        (
            WorkProductPortContextV1,
            WorkProductMutationIdentityV1,
            ManifestDigest,
        ),
        WorkProductApplicationErrorV1,
    > {
        authorize_and_admit(context, binding, mutation.occurred_at)?;
        selection.validate()?;
        let authorized_scope = self
            .owner_authority
            .authorize_scope(context, selection, mutation.occurred_at)
            .map_err(map_owner_error)?;
        if authorized_scope.selection() != selection {
            return Err(WorkProductApplicationErrorV1::EventAuthorityUnavailable);
        }
        canonicalize_mutation_evidence(&mut mutation)?;
        let digest = canonical_work_product_mutation_digest(
            context.actor(),
            &authorized_scope,
            selection,
            &mutation,
            payload,
        )?;
        Ok((
            WorkProductPortContextV1::from_request(context, authorized_scope, mutation.occurred_at),
            mutation,
            digest,
        ))
    }

    fn replay(
        &self,
        port_context: &WorkProductPortContextV1,
        mutation: &WorkProductMutationIdentityV1,
        payload: &WorkProductEventPayloadV1,
        digest: &ManifestDigest,
    ) -> Result<Option<WorkProductEventV1>, WorkProductApplicationErrorV1> {
        let event = self
            .events
            .replay(port_context, &mutation.command_id, digest)
            .map_err(map_event_error)?;
        if let Some(event) = event {
            validate_replayed_event(
                &event,
                port_context,
                mutation,
                payload,
                digest,
                &selected_relations(port_context.authorized_scope().selection()),
            )?;
            return Ok(Some(event));
        }
        Ok(None)
    }

    fn append_and_publish(
        &self,
        port_context: &WorkProductPortContextV1,
        draft: WorkProductEventDraftV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let (event, replayed) = self
            .events
            .append_with_outbox(port_context, &draft)
            .map_err(map_event_error)?
            .into_parts();
        validate_appended_event(&event, &draft)?;
        self.publish(port_context, event, replayed)
    }

    fn publish(
        &self,
        context: &WorkProductPortContextV1,
        event: WorkProductEventV1,
        replayed: bool,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationErrorV1> {
        let verified_graph_version = self
            .publisher
            .publish_event(context, &event)
            .map_err(map_publish_error)?;
        if verified_graph_version.graph_version() != event.result_graph_version()
            || verified_graph_version.event_sequence() != event.sequence()
            || verified_graph_version.source_watermark() != event.source_watermark()
        {
            return Err(WorkProductApplicationErrorV1::ReconciliationRequired);
        }
        Ok(WorkProductMutationReceiptV1 {
            event,
            verified_graph_version,
            replayed,
        })
    }
}

fn canonical_work_product_mutation_digest(
    actor: &ActorId,
    authorized_scope: &AuthorizedWorkProductScopeV1,
    selection: &WorkProductSelectionScopeV1,
    mutation: &WorkProductMutationIdentityV1,
    payload: &WorkProductEventPayloadV1,
) -> Result<ManifestDigest, WorkProductApplicationErrorV1> {
    canonical_sha256(&(
        WORK_PRODUCT_MUTATION_DIGEST_DOMAIN,
        actor,
        authorized_scope,
        selection,
        &mutation.expected_authority,
        &mutation.command_id,
        &mutation.causation_event_id,
        &mutation.evidence,
        mutation.occurred_at,
        &mutation.revisions,
        payload,
    ))
    .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)
}

fn canonicalize_mutation_evidence(
    mutation: &mut WorkProductMutationIdentityV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if mutation.evidence.len() > MAX_WORK_PRODUCT_EVENT_EVIDENCE {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    mutation.evidence.sort();
    let source_watermark = mutation_source_watermark(&mutation.expected_authority)?;
    if mutation.evidence.windows(2).any(|pair| pair[0] == pair[1])
        || mutation.evidence.iter().any(|evidence| {
            !source_watermark
                .components()
                .contains_key(&evidence.source_store_id)
        })
    {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    Ok(())
}

fn mutation_source_watermark(
    authority: &WorkProductExpectedAuthorityV1,
) -> Result<WorkProductSourceWatermarkV1, WorkProductApplicationErrorV1> {
    match authority {
        WorkProductExpectedAuthorityV1::NoPriorGraph => {
            WorkProductSourceWatermarkV1::new(BTreeMap::new())
                .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)
        }
        WorkProductExpectedAuthorityV1::Verified { verified_version } => {
            Ok(verified_version.source_watermark().clone())
        }
    }
}

fn mutation_expected_graph_version(
    authority: &WorkProductExpectedAuthorityV1,
) -> Option<WorkGraphVersionV1> {
    match authority {
        WorkProductExpectedAuthorityV1::NoPriorGraph => None,
        WorkProductExpectedAuthorityV1::Verified { verified_version } => {
            Some(verified_version.graph_version())
        }
    }
}

fn validate_change_request(
    change: &WorkGraphChangeV1,
    expected_graph_version: WorkGraphVersionV1,
    occurred_at: UtcMicros,
) -> Result<(), WorkProductApplicationErrorV1> {
    if let WorkGraphChangeV1::AcceptedAttemptLinked {
        task_id,
        based_on_version,
        identity,
        evidence,
        linked_at,
    } = change
    {
        let canonical_evidence = TaskEvidenceLinkV1::new(
            evidence.link_id().clone(),
            evidence.revision(),
            evidence.task_id().clone(),
            evidence.anchor_id().clone(),
            evidence.evidence_digest().clone(),
            evidence.observed_at(),
        )
        .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        if identity.task_id() != task_id
            || evidence.task_id() != task_id
            || canonical_evidence != *evidence
            || evidence.observed_at() > *linked_at
            || *linked_at != occurred_at
            || *based_on_version != expected_graph_version
        {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
    }
    Ok(())
}

fn event_draft(
    context: &RequestContext,
    authorized_scope: &AuthorizedWorkProductScopeV1,
    selection: &WorkProductSelectionScopeV1,
    mutation: &WorkProductMutationIdentityV1,
    canonical_input_digest: ManifestDigest,
    result_graph_version: WorkGraphVersionV1,
    payload: WorkProductEventPayloadV1,
) -> Result<WorkProductEventDraftV1, WorkProductApplicationErrorV1> {
    Ok(WorkProductEventDraftV1 {
        actor_id: context.actor().clone(),
        owner_scope: WorkProductProfileScopeV1 {
            brain_id: authorized_scope.owner_brain_id().clone(),
            profile_id: authorized_scope.owner_profile_id().clone(),
        },
        authorized_relation_scopes: selected_relations(selection),
        expected_graph_version: mutation_expected_graph_version(&mutation.expected_authority),
        result_graph_version,
        command_id: mutation.command_id.clone(),
        canonical_input_digest,
        causation_event_id: mutation.causation_event_id.clone(),
        evidence: mutation.evidence.clone(),
        source_watermark: mutation_source_watermark(&mutation.expected_authority)?,
        occurred_at: mutation.occurred_at,
        policy_revision_id: mutation.revisions.policy_revision_id.clone(),
        configuration_revision_id: mutation.revisions.configuration_revision_id.clone(),
        catalog_generation_id: mutation.revisions.catalog_generation_id.clone(),
        payload,
    })
}

fn selected_relations(
    selection: &WorkProductSelectionScopeV1,
) -> Vec<tracedecay_domain::WorkProductAuthorizedRelationScopeV1> {
    selection
        .relation_scopes()
        .map_or_else(Vec::new, |relations| relations.iter().cloned().collect())
}

fn authorize_and_admit(
    context: &RequestContext,
    binding: &WorkProductBindingV1,
    observed_at: UtcMicros,
) -> Result<(), WorkProductApplicationErrorV1> {
    if !context.allows(binding.capability_id(), binding.use_case_id()) {
        return Err(WorkProductApplicationErrorV1::NotAuthorized);
    }
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(WorkProductApplicationErrorV1::Cancelled),
        RequestAdmission::TimedOut => Err(WorkProductApplicationErrorV1::TimedOut),
    }
}

fn map_owner_error(error: WorkProductOwnerAuthorizationErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkProductOwnerAuthorizationErrorV1::NotAuthorized => {
            WorkProductApplicationErrorV1::NotAuthorized
        }
        WorkProductOwnerAuthorizationErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EventAuthorityUnavailable
        }
    }
}

fn map_event_error(error: WorkProductEventPortErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkProductEventPortErrorV1::NotFoundOrNotAuthorized => {
            WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
        }
        WorkProductEventPortErrorV1::VersionConflict => {
            WorkProductApplicationErrorV1::VersionConflict
        }
        WorkProductEventPortErrorV1::IdempotencyConflict => {
            WorkProductApplicationErrorV1::IdempotencyConflict
        }
        WorkProductEventPortErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EventAuthorityUnavailable
        }
        WorkProductEventPortErrorV1::Cancelled => WorkProductApplicationErrorV1::Cancelled,
        WorkProductEventPortErrorV1::TimedOut => WorkProductApplicationErrorV1::TimedOut,
    }
}

fn map_publish_error(error: WorkGraphPublishPortErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkGraphPublishPortErrorV1::VersionConflict
        | WorkGraphPublishPortErrorV1::Unavailable
        | WorkGraphPublishPortErrorV1::DurabilityUncertain
        | WorkGraphPublishPortErrorV1::Cancelled
        | WorkGraphPublishPortErrorV1::TimedOut => {
            WorkProductApplicationErrorV1::ReconciliationRequired
        }
    }
}

fn validate_replayed_event(
    event: &WorkProductEventV1,
    context: &WorkProductPortContextV1,
    mutation: &WorkProductMutationIdentityV1,
    payload: &WorkProductEventPayloadV1,
    canonical_input_digest: &ManifestDigest,
    authorized_relation_scopes: &[tracedecay_domain::WorkProductAuthorizedRelationScopeV1],
) -> Result<(), WorkProductApplicationErrorV1> {
    let expected_result_version = match payload {
        WorkProductEventPayloadV1::Created { .. } => WorkGraphVersionV1::initial(),
        WorkProductEventPayloadV1::Changed { .. } => {
            mutation_expected_graph_version(&mutation.expected_authority)
                .and_then(|version| version.next().ok())
                .ok_or(WorkProductApplicationErrorV1::IdempotencyConflict)?
        }
    };
    let expected_graph_version = mutation_expected_graph_version(&mutation.expected_authority);
    let source_watermark = mutation_source_watermark(&mutation.expected_authority)
        .map_err(|_| WorkProductApplicationErrorV1::IdempotencyConflict)?;
    if event.actor_id() != context.actor()
        || &event.owner_scope().brain_id != context.authorized_scope().owner_brain_id()
        || &event.owner_scope().profile_id != context.authorized_scope().owner_profile_id()
        || event.authorized_relation_scopes() != authorized_relation_scopes
        || event.expected_graph_version() != expected_graph_version
        || event.result_graph_version() != expected_result_version
        || event.command_id() != &mutation.command_id
        || event.canonical_input_digest() != canonical_input_digest
        || event.causation_event_id() != mutation.causation_event_id.as_ref()
        || event.evidence() != mutation.evidence
        || event.source_watermark() != &source_watermark
        || event.occurred_at() != mutation.occurred_at
        || event.policy_revision_id() != &mutation.revisions.policy_revision_id
        || event.configuration_revision_id() != &mutation.revisions.configuration_revision_id
        || event.catalog_generation_id() != &mutation.revisions.catalog_generation_id
        || event.payload() != payload
    {
        return Err(WorkProductApplicationErrorV1::IdempotencyConflict);
    }
    Ok(())
}

fn validate_appended_event(
    event: &WorkProductEventV1,
    draft: &WorkProductEventDraftV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if event.actor_id() != &draft.actor_id
        || event.owner_scope() != &draft.owner_scope
        || event.authorized_relation_scopes() != draft.authorized_relation_scopes
        || event.expected_graph_version() != draft.expected_graph_version
        || event.result_graph_version() != draft.result_graph_version
        || event.command_id() != &draft.command_id
        || event.canonical_input_digest() != &draft.canonical_input_digest
        || event.causation_event_id() != draft.causation_event_id.as_ref()
        || event.evidence() != draft.evidence
        || event.source_watermark() != &draft.source_watermark
        || event.occurred_at() != draft.occurred_at
        || event.policy_revision_id() != &draft.policy_revision_id
        || event.configuration_revision_id() != &draft.configuration_revision_id
        || event.catalog_generation_id() != &draft.catalog_generation_id
        || event.payload() != &draft.payload
    {
        return Err(WorkProductApplicationErrorV1::EventAuthorityUnavailable);
    }
    Ok(())
}
