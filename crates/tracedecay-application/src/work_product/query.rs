use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    ManifestDigest, TaskEvidenceLinkId, TaskEvidenceLinkV1, TaskId, UtcMicros, WorkProductEventV1,
    WorkProductSourceWatermarkV1, WorkProposalV1, WorkTaskEvidenceV1, canonical_sha256,
};

use crate::{OpaqueCursor, RequestAdmission, RequestContext};

use super::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkGraphReadPortV1,
    WorkGraphReadRequestV1, WorkGraphReadV1, WorkProductApplicationErrorV1, WorkProductBindingV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductRevisionPinsV1, WorkProductSelectionScopeV1,
};

pub const MAX_WORK_EVIDENCE_SELECTION_V1: u32 = 1_024;
pub const MAX_WORK_HISTORY_EVENTS_V1: u32 = 1_024;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceSelectRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub limit: u32,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceExpandRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub link_id: TaskEvidenceLinkId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SelectedWorkEvidenceV1 {
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub evidence: WorkTaskEvidenceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorkEvidenceExpansionV1 {
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub expansion: WorkEvidenceExpansionV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceExpansionV1 {
    link: TaskEvidenceLinkV1,
    content_handle: String,
    redacted: bool,
    observed_at: UtcMicros,
}

impl WorkEvidenceExpansionV1 {
    pub fn new(
        link: TaskEvidenceLinkV1,
        content_handle: String,
        redacted: bool,
        observed_at: UtcMicros,
    ) -> Result<Self, WorkProductApplicationErrorV1> {
        if !tracedecay_domain::canonical_text::is_canonical_text_within(&content_handle, 2_048) {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        Ok(Self {
            link,
            content_handle,
            redacted,
            observed_at,
        })
    }

    pub const fn link(&self) -> &TaskEvidenceLinkV1 {
        &self.link
    }

    pub fn content_handle(&self) -> &str {
        &self.content_handle
    }

    pub const fn is_redacted(&self) -> bool {
        self.redacted
    }

    pub const fn observed_at(&self) -> UtcMicros {
        self.observed_at
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkEvidenceReadPortErrorV1 {
    #[error("Work evidence was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work evidence graph version is stale")]
    Stale,
    #[error("Work evidence authority is unavailable")]
    Unavailable,
    #[error("Work evidence read was cancelled")]
    Cancelled,
    #[error("Work evidence read timed out")]
    TimedOut,
}

pub trait WorkEvidenceReadPortV1: Send + Sync {
    fn select_task_evidence(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkEvidenceSelectRequestV1,
    ) -> Result<SelectedWorkEvidenceV1, WorkEvidenceReadPortErrorV1>;

    fn expand_task_evidence(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkEvidenceExpandRequestV1,
    ) -> Result<VerifiedWorkEvidenceExpansionV1, WorkEvidenceReadPortErrorV1>;
}

impl<E> WorkEvidenceReadPortV1 for &E
where
    E: WorkEvidenceReadPortV1 + ?Sized,
{
    fn select_task_evidence(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkEvidenceSelectRequestV1,
    ) -> Result<SelectedWorkEvidenceV1, WorkEvidenceReadPortErrorV1> {
        (**self).select_task_evidence(context, request)
    }

    fn expand_task_evidence(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkEvidenceExpandRequestV1,
    ) -> Result<VerifiedWorkEvidenceExpansionV1, WorkEvidenceReadPortErrorV1> {
        (**self).expand_task_evidence(context, request)
    }
}

pub struct WorkProductEvidenceServiceV1<E, A> {
    evidence: E,
    owner_authority: A,
}

impl<E, A> WorkProductEvidenceServiceV1<E, A>
where
    E: WorkEvidenceReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
{
    pub const fn new(evidence: E, owner_authority: A) -> Self {
        Self {
            evidence,
            owner_authority,
        }
    }

    pub fn select(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: WorkEvidenceSelectRequestV1,
    ) -> Result<SelectedWorkEvidenceV1, WorkProductApplicationErrorV1> {
        let port_context = authorize_port_context(
            context,
            binding,
            &self.owner_authority,
            &request.selection,
            request.observed_at,
        )?;
        if request.limit == 0 || request.limit > MAX_WORK_EVIDENCE_SELECTION_V1 {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        let selected = self
            .evidence
            .select_task_evidence(&port_context, &request)
            .map_err(map_evidence_error)?;
        if selected.verified_version != request.verified_version
            || selected.evidence.task_id() != &request.task_id
            || selected.evidence.graph_version() != request.verified_version.graph_version()
            || !evidence_is_canonical_within_limit(&selected.evidence, request.limit)
        {
            return Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable);
        }
        Ok(selected)
    }

    pub fn expand(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: WorkEvidenceExpandRequestV1,
    ) -> Result<VerifiedWorkEvidenceExpansionV1, WorkProductApplicationErrorV1> {
        let port_context = authorize_port_context(
            context,
            binding,
            &self.owner_authority,
            &request.selection,
            request.observed_at,
        )?;
        let verified = self
            .evidence
            .expand_task_evidence(&port_context, &request)
            .map_err(map_evidence_error)?;
        let expansion = &verified.expansion;
        if verified.verified_version != request.verified_version
            || expansion.link().task_id() != &request.task_id
            || expansion.link().link_id() != &request.link_id
            || expansion.observed_at() != request.observed_at
            || !task_evidence_link_is_canonical(expansion.link())
            || !WorkEvidenceExpansionV1::new(
                expansion.link().clone(),
                expansion.content_handle().to_owned(),
                expansion.is_redacted(),
                expansion.observed_at(),
            )
            .is_ok_and(|canonical| canonical == *expansion)
        {
            return Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable);
        }
        Ok(verified)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHistoryRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub limit: u32,
    #[schemars(with = "Option<String>")]
    pub continuation: Option<OpaqueCursor>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum WorkHistoryCoverageV1 {
    Complete {
        returned: u32,
    },
    Partial {
        returned: u32,
        #[schemars(with = "String")]
        continuation: OpaqueCursor,
    },
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkHistoryV1 {
    pub authorized_scope: AuthorizedWorkProductScopeV1,
    pub events: Vec<WorkProductEventV1>,
    pub coverage: WorkHistoryCoverageV1,
}

pub trait WorkHistoryReadPortV1: Send + Sync {
    fn read_history(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkHistoryRequestV1,
    ) -> Result<WorkHistoryV1, WorkProductApplicationErrorV1>;
}

impl<H> WorkHistoryReadPortV1 for &H
where
    H: WorkHistoryReadPortV1 + ?Sized,
{
    fn read_history(
        &self,
        context: &WorkProductPortContextV1,
        request: &WorkHistoryRequestV1,
    ) -> Result<WorkHistoryV1, WorkProductApplicationErrorV1> {
        (**self).read_history(context, request)
    }
}

pub struct WorkHistoryServiceV1<H, A> {
    history: H,
    owner_authority: A,
}

impl<H, A> WorkHistoryServiceV1<H, A>
where
    H: WorkHistoryReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
{
    pub const fn new(history: H, owner_authority: A) -> Self {
        Self {
            history,
            owner_authority,
        }
    }

    pub fn read(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: WorkHistoryRequestV1,
    ) -> Result<WorkHistoryV1, WorkProductApplicationErrorV1> {
        let port_context = authorize_port_context(
            context,
            binding,
            &self.owner_authority,
            &request.selection,
            request.observed_at,
        )?;
        if request.limit == 0 || request.limit > MAX_WORK_HISTORY_EVENTS_V1 {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        let result = self.history.read_history(&port_context, &request)?;
        let returned = match &result.coverage {
            WorkHistoryCoverageV1::Complete { returned }
            | WorkHistoryCoverageV1::Partial { returned, .. } => *returned,
        };
        let authorized_relation_scopes = selected_relations(result.authorized_scope.selection());
        if &result.authorized_scope != port_context.authorized_scope()
            || result.events.len() > request.limit as usize
            || usize::try_from(returned).ok() != Some(result.events.len())
            || result
                .events
                .windows(2)
                .any(|pair| pair[0].sequence() >= pair[1].sequence())
            || result.events.iter().any(|event| {
                &event.owner_scope().brain_id != result.authorized_scope.owner_brain_id()
                    || &event.owner_scope().profile_id != result.authorized_scope.owner_profile_id()
                    || event.authorized_relation_scopes() != authorized_relation_scopes.as_slice()
                    || event.occurred_at() > request.observed_at
            })
        {
            return Err(WorkProductApplicationErrorV1::EventAuthorityUnavailable);
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerateWorkProposalRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub evidence_limit: u32,
    pub revisions: WorkProductRevisionPinsV1,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GeneratedWorkProposalV1 {
    pub proposal: WorkProposalV1,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub evidence_digest: ManifestDigest,
    pub source_watermark: WorkProductSourceWatermarkV1,
    pub revisions: WorkProductRevisionPinsV1,
}

pub trait WorkProposalGeneratorPortV1: Send + Sync {
    fn generate(
        &self,
        context: &WorkProductPortContextV1,
        request: &GenerateWorkProposalRequestV1,
        evidence: &WorkTaskEvidenceV1,
    ) -> Result<WorkProposalV1, WorkProductApplicationErrorV1>;
}

pub struct WorkProposalServiceV1<G, E, A, P> {
    graph: G,
    evidence: E,
    owner_authority: A,
    planner: P,
}

impl<G, E, A, P> WorkProposalServiceV1<G, E, A, P>
where
    G: WorkGraphReadPortV1,
    E: WorkEvidenceReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
    P: WorkProposalGeneratorPortV1,
{
    pub const fn new(graph: G, evidence: E, owner_authority: A, planner: P) -> Self {
        Self {
            graph,
            evidence,
            owner_authority,
            planner,
        }
    }

    pub fn generate(
        &self,
        context: &RequestContext,
        binding: &WorkProductBindingV1,
        request: GenerateWorkProposalRequestV1,
    ) -> Result<GeneratedWorkProposalV1, WorkProductApplicationErrorV1> {
        let port_context = authorize_port_context(
            context,
            binding,
            &self.owner_authority,
            &request.selection,
            request.observed_at,
        )?;
        if request.evidence_limit == 0 || request.evidence_limit > MAX_WORK_EVIDENCE_SELECTION_V1 {
            return Err(WorkProductApplicationErrorV1::InvalidRequest);
        }
        let graph_request =
            WorkGraphReadRequestV1::current(request.selection.clone(), request.observed_at);
        let graph = self.graph.read_graph(&port_context, &graph_request)?;
        super::read::validate_result(&graph_request, port_context.authorized_scope(), &graph)?;
        let WorkGraphReadV1::Current { snapshot, .. } = graph else {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        };
        if snapshot.verified_version() != &request.verified_version
            || snapshot.graph().item(&request.task_id).is_none()
        {
            return Err(WorkProductApplicationErrorV1::VersionConflict);
        }
        let evidence_request = WorkEvidenceSelectRequestV1 {
            selection: request.selection.clone(),
            task_id: request.task_id.clone(),
            verified_version: request.verified_version.clone(),
            limit: request.evidence_limit,
            observed_at: request.observed_at,
        };
        let selected = self
            .evidence
            .select_task_evidence(&port_context, &evidence_request)
            .map_err(map_evidence_error)?;
        if selected.verified_version != request.verified_version
            || selected.evidence.task_id() != &request.task_id
            || selected.evidence.graph_version() != request.verified_version.graph_version()
            || !evidence_is_canonical_within_limit(&selected.evidence, request.evidence_limit)
        {
            return Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable);
        }
        let evidence_digest = canonical_sha256(&selected.evidence)
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let proposal = self
            .planner
            .generate(&port_context, &request, &selected.evidence)?;
        let canonical_proposal = WorkProposalV1::new(
            proposal.proposal_id().clone(),
            proposal.task_id().clone(),
            proposal.based_on_version(),
            proposal.shape().clone(),
            proposal.sizing().clone(),
            proposal.children().to_vec(),
            proposal.route().clone(),
            proposal.explanation().to_owned(),
            proposal.evidence_digest().clone(),
        )
        .map_err(|_| WorkProductApplicationErrorV1::ProposalAuthorityUnavailable)?;
        if proposal.task_id() != &request.task_id
            || proposal.based_on_version() != request.verified_version.graph_version()
            || proposal.evidence_digest() != &evidence_digest
            || proposal != canonical_proposal
        {
            return Err(WorkProductApplicationErrorV1::ProposalAuthorityUnavailable);
        }
        Ok(GeneratedWorkProposalV1 {
            proposal,
            verified_version: request.verified_version,
            evidence_digest,
            source_watermark: selected.verified_version.source_watermark().clone(),
            revisions: request.revisions,
        })
    }
}

fn authorize_port_context<A: WorkProductOwnerAuthorizationPortV1>(
    context: &RequestContext,
    binding: &WorkProductBindingV1,
    owner_authority: &A,
    selection: &WorkProductSelectionScopeV1,
    observed_at: UtcMicros,
) -> Result<WorkProductPortContextV1, WorkProductApplicationErrorV1> {
    if !context.allows(binding.capability_id(), binding.use_case_id()) {
        return Err(WorkProductApplicationErrorV1::NotAuthorized);
    }
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => {}
        RequestAdmission::Cancelled => return Err(WorkProductApplicationErrorV1::Cancelled),
        RequestAdmission::TimedOut => return Err(WorkProductApplicationErrorV1::TimedOut),
    }
    selection.validate()?;
    let scope = owner_authority
        .authorize_scope(context, selection, observed_at)
        .map_err(|error| match error {
            WorkProductOwnerAuthorizationErrorV1::NotAuthorized => {
                WorkProductApplicationErrorV1::NotAuthorized
            }
            WorkProductOwnerAuthorizationErrorV1::Unavailable => {
                WorkProductApplicationErrorV1::EventAuthorityUnavailable
            }
        })?;
    if scope.selection() != selection {
        return Err(WorkProductApplicationErrorV1::EventAuthorityUnavailable);
    }
    Ok(WorkProductPortContextV1::from_request(
        context,
        scope,
        observed_at,
    ))
}

fn selected_relations(
    selection: &WorkProductSelectionScopeV1,
) -> Vec<tracedecay_domain::WorkProductAuthorizedRelationScopeV1> {
    selection
        .relation_scopes()
        .map_or_else(Vec::new, |relations| relations.iter().cloned().collect())
}

fn evidence_is_canonical_within_limit(evidence: &WorkTaskEvidenceV1, limit: u32) -> bool {
    if evidence.links().len() > limit as usize
        || evidence.validate().is_err()
        || evidence
            .links()
            .iter()
            .any(|link| !task_evidence_link_is_canonical(link))
    {
        return false;
    }
    WorkTaskEvidenceV1::new(
        evidence.task_id().clone(),
        evidence.graph_version(),
        evidence.links().to_vec(),
        evidence.coverage().clone(),
    )
    .is_ok_and(|canonical| canonical == *evidence)
}

fn task_evidence_link_is_canonical(link: &TaskEvidenceLinkV1) -> bool {
    TaskEvidenceLinkV1::new(
        link.link_id().clone(),
        link.revision(),
        link.task_id().clone(),
        link.anchor_id().clone(),
        link.evidence_digest().clone(),
        link.observed_at(),
    )
    .is_ok_and(|canonical| canonical == *link)
}

fn map_evidence_error(error: WorkEvidenceReadPortErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkEvidenceReadPortErrorV1::NotFoundOrNotAuthorized => {
            WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
        }
        WorkEvidenceReadPortErrorV1::Stale => WorkProductApplicationErrorV1::VersionConflict,
        WorkEvidenceReadPortErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
        }
        WorkEvidenceReadPortErrorV1::Cancelled => WorkProductApplicationErrorV1::Cancelled,
        WorkEvidenceReadPortErrorV1::TimedOut => WorkProductApplicationErrorV1::TimedOut,
    }
}
