use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    TaskEvidenceLinkId, TaskEvidenceLinkV1, TaskId, UtcMicros, WorkProductEventV1,
    WorkTaskEvidenceV1,
};

use crate::{OpaqueCursor, RequestAdmission, RequestContext};

use super::{
    AuthorizedWorkProductScopeV1, VerifiedWorkGraphVersionV1, WorkProductApplicationErrorV1,
    WorkProductBindingV1, WorkProductOwnerAuthorizationErrorV1,
    WorkProductOwnerAuthorizationPortV1, WorkProductPortContextV1, WorkProductSelectionScopeV1,
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

// Work proposal *generation* is mounted once, on the Work family:
// `WorkOperation::GenerateProposal` -> `WorkService::generate_proposal`
// (crates/tracedecay-application/src/work.rs) -> `WorkProposalEvaluatorV1`
// (crates/tracedecay-policy/src/work_loop.rs). A second, unmounted
// generator port and service used to live here. Plan 06 forbids both halves
// of that: "No slice lands a standalone schema, trait, registry, fixture
// framework, or policy phase without its production caller", and "Remove any
// route, score, fallback, or replan decision duplicated in a surface,
// provider adapter, dashboard, graph projector, or runtime handler." The
// shape/sizing/decomposition/route planner Plan 06 mandates is still owed;
// it lands with its production caller, not as a port ahead of one.
//
// This module keeps the read side only: evidence selection/expansion and
// history. Client-supplied proposals still enter through the mounted
// mutation path (`DecideWorkProposalRequestV1` in `mutation.rs`).

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
