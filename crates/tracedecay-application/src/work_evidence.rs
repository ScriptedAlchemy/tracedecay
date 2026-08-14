//! TaskId-rooted composition over canonical Work relations and evidence owners.
//!
//! Work admits the task/version/attempt join; session retrieval retains that
//! identity through compact ranking and selected-anchor hydration.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CalibrationProfileId, ComponentRevision, ManifestDigest, ObservationSourceIdentityV1,
    RetrievalAnchorId, RetrieverKind, ScoreDomainId, SourceOccurrenceId, TaskEvidenceLinkId,
    TaskEvidenceLinkV1, TaskId, TemporalModeV1, UtcMicros, WorkArtifactRefV1,
    WorkAttemptIdentityV1, WorkAuthority, WorkItemV1, WorkProductRelationV1,
    WorkProposalDecisionV1, WorkRelationReplanDecisionV1,
};

use crate::work::work_authority;
use crate::{
    OpaqueCursor, RequestAdmission, RequestContext, VerifiedWorkGraphVersionV1,
    WorkAttemptEvidenceRecordV1, WorkProductApplicationErrorV1, WorkProductBindingV1,
    WorkProductOwnerAuthorizationErrorV1, WorkProductOwnerAuthorizationPortV1,
    WorkProductPortContextV1, WorkProductSelectionScopeV1,
};

pub const MAX_WORK_ROOTED_EVIDENCE_SOURCES_V1: u32 = 100;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkEvidenceExpansionSelectorV1 {
    Anchor { link_id: TaskEvidenceLinkId },
    TaskSession { attempt: WorkAttemptIdentityV1 },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionContinuationV1 {
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub attempt: WorkAttemptIdentityV1,
    pub source: ObservationSourceIdentityV1,
    pub participant_epoch: ManifestDigest,
    #[schemars(with = "Option<String>")]
    pub temporal_cursor: Option<OpaqueCursor>,
    #[schemars(with = "Option<String>")]
    pub ranking_cursor: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkEvidenceContinuationV1 {
    Anchor {
        link_id: TaskEvidenceLinkId,
        #[schemars(with = "String")]
        cursor: OpaqueCursor,
    },
    TaskSession {
        continuation: WorkTaskSessionContinuationV1,
    },
}

/// One TaskId-rooted read. The exact Work graph identity remains mandatory on
/// continuation and expansion requests, so neither an anchor nor a cursor is
/// authority by possession.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceRetrieveRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub temporal: TemporalModeV1,
    pub page_size: u32,
    #[serde(default)]
    pub expansion: Option<WorkEvidenceExpansionSelectorV1>,
    #[serde(default)]
    pub continuation: Option<WorkEvidenceContinuationV1>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedWorkEvidenceRootV1 {
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub item: WorkItemV1,
    pub relations: Vec<WorkProductRelationV1>,
    pub proposal_decisions: Vec<WorkProposalDecisionV1>,
    pub relation_replan_decisions: Vec<WorkRelationReplanDecisionV1>,
    pub links: Vec<TaskEvidenceLinkV1>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkEvidenceRootReadErrorV1 {
    #[error("Work evidence root was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work evidence graph version is stale")]
    Stale,
    #[error("Work evidence root authority is unavailable")]
    Unavailable,
    #[error("Work evidence root read was cancelled")]
    Cancelled,
    #[error("Work evidence root read timed out")]
    TimedOut,
}

/// Read authority over the exact immutable Work version named by the caller.
pub trait WorkEvidenceRootReadPortV1: Send + Sync {
    fn read_evidence_root(
        &self,
        context: &WorkProductPortContextV1,
        task_id: &TaskId,
        verified_version: &VerifiedWorkGraphVersionV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkEvidenceRootReadErrorV1>;
}

impl<P> WorkEvidenceRootReadPortV1 for &P
where
    P: WorkEvidenceRootReadPortV1 + ?Sized,
{
    fn read_evidence_root(
        &self,
        context: &WorkProductPortContextV1,
        task_id: &TaskId,
        verified_version: &VerifiedWorkGraphVersionV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkEvidenceRootReadErrorV1> {
        (**self).read_evidence_root(context, task_id, verified_version)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkAttemptReceiptReadErrorV1 {
    #[error("Work attempt receipt was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work attempt receipt authority is unavailable")]
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAttemptReceiptV1 {
    pub identity: WorkAttemptIdentityV1,
    pub artifacts: Vec<WorkArtifactRefV1>,
    pub evidence: Option<WorkAttemptEvidenceRecordV1>,
}

/// Exact lookup on the owning attempt store. The Work graph determines which
/// identities may be requested; adapters cannot broaden this lookup.
pub trait WorkAttemptReceiptReadPortV1: Send + Sync {
    fn attempt_receipt(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptReceiptV1, WorkAttemptReceiptReadErrorV1>;
}

impl<P> WorkAttemptReceiptReadPortV1 for &P
where
    P: WorkAttemptReceiptReadPortV1 + ?Sized,
{
    fn attempt_receipt(
        &self,
        authority: &WorkAuthority,
        identity: &WorkAttemptIdentityV1,
    ) -> Result<WorkAttemptReceiptV1, WorkAttemptReceiptReadErrorV1> {
        (**self).attempt_receipt(authority, identity)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkEvidenceFreshnessV1 {
    Current,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkEvidenceCoverageStateV1 {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceCoverageV1 {
    pub state: WorkEvidenceCoverageStateV1,
    pub selected: u32,
    pub hydrated: u32,
    pub omitted: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkEvidenceOmissionReasonV1 {
    LimitReached,
    Pending,
    NotFoundOrNotAuthorized,
    Unavailable,
    ResetRequired,
    Stale,
    Cancelled,
    TimedOut,
    Redacted,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceOmissionV1 {
    pub relation: String,
    pub reason: WorkEvidenceOmissionReasonV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionRequestV1 {
    pub selection: WorkProductSelectionScopeV1,
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub accepted_attempts: BTreeSet<WorkAttemptIdentityV1>,
    pub attempt: WorkAttemptIdentityV1,
    pub source: ObservationSourceIdentityV1,
    pub temporal: TemporalModeV1,
    pub page_size: u32,
    pub continuation: Option<WorkTaskSessionContinuationV1>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkTaskSessionHydrationStateV1 {
    Available,
    RetainedButUnavailable,
    Redacted,
    Deleted,
    RetentionExpired,
    Unauthorized,
    Locked,
    UnverifiableLegacy,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionRankContributionV1 {
    #[schemars(with = "String")]
    pub retriever: RetrieverKind,
    #[schemars(with = "String")]
    pub retriever_revision: ComponentRevision,
    #[schemars(with = "String")]
    pub source_occurrence: SourceOccurrenceId,
    pub ordinal_rank: u32,
    pub raw_score_micros: i64,
    #[schemars(with = "String")]
    pub score_domain: ScoreDomainId,
    #[schemars(with = "String")]
    pub calibration_profile: CalibrationProfileId,
    pub calibrated_feature_micros: u32,
    pub weight_micros: u32,
    pub weighted_contribution_micros: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionRankedAnchorV1 {
    pub anchor_id: RetrievalAnchorId,
    pub final_ordinal: u32,
    pub utility_micros: u64,
    pub contributions: Vec<WorkTaskSessionRankContributionV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionHydrationV1 {
    pub rank: u32,
    pub anchor_id: RetrievalAnchorId,
    pub state: WorkTaskSessionHydrationStateV1,
    pub content: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionCoverageV1 {
    pub visible: u64,
    pub hidden: u64,
    pub unknown: u64,
    pub redacted: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskSessionEvidenceV1 {
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub attempt: WorkAttemptIdentityV1,
    pub source: ObservationSourceIdentityV1,
    pub participant_epoch: ManifestDigest,
    pub ranked_anchors: Vec<WorkTaskSessionRankedAnchorV1>,
    pub hydrated: Vec<WorkTaskSessionHydrationV1>,
    pub coverage: WorkEvidenceCoverageStateV1,
    pub coverage_counts: WorkTaskSessionCoverageV1,
    pub freshness: WorkEvidenceFreshnessV1,
    pub redacted: bool,
    pub continuation: Option<WorkTaskSessionContinuationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAnchorHydrationRequestV1 {
    pub anchor_id: RetrievalAnchorId,
    pub temporal: TemporalModeV1,
    pub page_size: u32,
    #[schemars(with = "Option<String>")]
    pub continuation: Option<OpaqueCursor>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkAnchorHydrationV1 {
    pub anchor_id: RetrievalAnchorId,
    pub exact_anchors: Vec<RetrievalAnchorId>,
    pub content: Vec<String>,
    pub coverage: WorkEvidenceCoverageStateV1,
    pub freshness: WorkEvidenceFreshnessV1,
    pub redacted: bool,
    #[schemars(with = "Option<String>")]
    pub continuation: Option<OpaqueCursor>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkEvidenceHydrationErrorV1 {
    #[error("evidence was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("evidence is unavailable")]
    Unavailable,
    #[error("evidence persisted state requires an explicit reset")]
    ResetRequired,
    #[error("evidence is stale")]
    Stale,
    #[error("evidence hydration was cancelled")]
    Cancelled,
    #[error("evidence hydration timed out")]
    TimedOut,
}

pub type WorkTaskSessionFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<WorkTaskSessionEvidenceV1, WorkEvidenceHydrationErrorV1>>
            + Send
            + 'a,
    >,
>;

pub type WorkAnchorHydrationFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<WorkAnchorHydrationV1, WorkEvidenceHydrationErrorV1>>
            + Send
            + 'a,
    >,
>;

/// Admitted Work-to-Plan-23 TaskSession adapter. Work identity authorizes the
/// join while provider-qualified session identity scopes the temporal read.
pub trait WorkTaskSessionPortV1: Send + Sync {
    fn retrieve_task_session<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkTaskSessionRequestV1,
        reauthorization: &'a dyn WorkTaskSessionReauthorizationPortV1,
    ) -> WorkTaskSessionFuture<'a>;
}

impl<P> WorkTaskSessionPortV1 for &P
where
    P: WorkTaskSessionPortV1 + ?Sized,
{
    fn retrieve_task_session<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkTaskSessionRequestV1,
        reauthorization: &'a dyn WorkTaskSessionReauthorizationPortV1,
    ) -> WorkTaskSessionFuture<'a> {
        (**self).retrieve_task_session(context, request, reauthorization)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum WorkTaskSessionReauthorizationErrorV1 {
    #[error("the exact task/session binding was denied")]
    Denied,
    #[error("the verified Work graph binding is stale")]
    Stale,
    #[error("the Work task/session authority is unavailable")]
    Unavailable,
}

/// Callback retained by the temporal kernel across compact selection and
/// rank-final hydration. Each invocation reopens the exact Work authority;
/// neither the task/session binding nor either cursor grants authority.
pub trait WorkTaskSessionReauthorizationPortV1: Send + Sync {
    fn reauthorize_task_session(
        &self,
        context: &RequestContext,
        request: &WorkTaskSessionRequestV1,
    ) -> Result<(), WorkTaskSessionReauthorizationErrorV1>;
}

/// Plan 13/owning-store exact expansion adapter for non-session anchors.
pub trait WorkAnchorHydrationPortV1: Send + Sync {
    fn hydrate_anchor<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkAnchorHydrationRequestV1,
    ) -> WorkAnchorHydrationFuture<'a>;
}

impl<P> WorkAnchorHydrationPortV1 for &P
where
    P: WorkAnchorHydrationPortV1 + ?Sized,
{
    fn hydrate_anchor<'a>(
        &'a self,
        context: &'a RequestContext,
        request: WorkAnchorHydrationRequestV1,
    ) -> WorkAnchorHydrationFuture<'a> {
        (**self).hydrate_anchor(context, request)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkEvidenceSourceV1 {
    AttemptReceipt {
        receipt: WorkAttemptReceiptV1,
    },
    TaskSession {
        attempt: WorkAttemptIdentityV1,
        evidence: WorkTaskSessionEvidenceV1,
    },
    Anchor {
        link: TaskEvidenceLinkV1,
        hydration: WorkAnchorHydrationV1,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceRetrievalV1 {
    pub task_id: TaskId,
    pub verified_version: VerifiedWorkGraphVersionV1,
    pub item: WorkItemV1,
    pub relations: Vec<WorkProductRelationV1>,
    pub proposal_decisions: Vec<WorkProposalDecisionV1>,
    pub relation_replan_decisions: Vec<WorkRelationReplanDecisionV1>,
    pub sources: Vec<WorkEvidenceSourceV1>,
    pub coverage: WorkEvidenceCoverageV1,
    pub omissions: Vec<WorkEvidenceOmissionV1>,
    pub freshness: WorkEvidenceFreshnessV1,
    pub redacted: bool,
    pub continuations: Vec<WorkEvidenceContinuationV1>,
}

pub struct WorkEvidenceRetrievalServiceV1<R, A, T, S, H> {
    roots: R,
    owner_authority: A,
    attempts: T,
    sessions: S,
    anchors: H,
    binding: WorkProductBindingV1,
}

impl<R, A, T, S, H> WorkEvidenceRetrievalServiceV1<R, A, T, S, H>
where
    R: WorkEvidenceRootReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
    T: WorkAttemptReceiptReadPortV1,
    S: WorkTaskSessionPortV1,
    H: WorkAnchorHydrationPortV1,
{
    pub const fn new(
        roots: R,
        owner_authority: A,
        attempts: T,
        sessions: S,
        anchors: H,
        binding: WorkProductBindingV1,
    ) -> Self {
        Self {
            roots,
            owner_authority,
            attempts,
            sessions,
            anchors,
            binding,
        }
    }

    pub async fn retrieve(
        &self,
        context: &RequestContext,
        request: WorkEvidenceRetrieveRequestV1,
    ) -> Result<WorkEvidenceRetrievalV1, WorkProductApplicationErrorV1> {
        validate_request(&request)?;
        let root = self.authorize_root(context, &request)?;
        let selected = select_sources(&root, &request)?;
        let authority = work_authority(context)
            .map_err(|_| WorkProductApplicationErrorV1::NotFoundOrNotAuthorized)?;
        let mut sources = Vec::new();
        let mut omissions = selected.omissions;
        let mut continuations = Vec::new();
        let mut hydrated = 0_u32;
        let mut source_partial = false;
        let mut freshness = WorkEvidenceFreshnessV1::Current;
        let mut redacted = false;

        for source in selected.sources {
            // Reauthorize the exact root before every owning-store read.
            self.authorize_root(context, &request)?;
            match source {
                SelectedSource::Attempt(identity) => {
                    match self.attempts.attempt_receipt(&authority, &identity) {
                        Ok(receipt) => {
                            let provider_session = receipt
                                .evidence
                                .as_ref()
                                .and_then(|evidence| evidence.provider_session.clone());
                            sources.push(WorkEvidenceSourceV1::AttemptReceipt {
                                receipt: receipt.clone(),
                            });
                            hydrated = hydrated.saturating_add(1);
                            if let Some(source) = provider_session {
                                self.authorize_root(context, &request)?;
                                let continuation = task_session_continuation(&request, &identity);
                                let accepted_attempts =
                                    root.item.accepted_attempts().iter().cloned().collect();
                                match self
                                    .sessions
                                    .retrieve_task_session(
                                        context,
                                        WorkTaskSessionRequestV1 {
                                            selection: request.selection.clone(),
                                            task_id: request.task_id.clone(),
                                            verified_version: request.verified_version.clone(),
                                            accepted_attempts,
                                            attempt: identity.clone(),
                                            source,
                                            temporal: request.temporal,
                                            page_size: request.page_size,
                                            continuation,
                                            observed_at: request.observed_at,
                                        },
                                        self,
                                    )
                                    .await
                                {
                                    Ok(evidence) => {
                                        validate_task_session(&request, &receipt, &evidence)?;
                                        source_partial |= evidence.coverage
                                            != WorkEvidenceCoverageStateV1::Complete;
                                        freshness = merge_freshness(freshness, evidence.freshness);
                                        redacted |= evidence.redacted;
                                        if let Some(continuation) = evidence.continuation.clone() {
                                            continuations.push(
                                                WorkEvidenceContinuationV1::TaskSession {
                                                    continuation,
                                                },
                                            );
                                        }
                                        sources.push(WorkEvidenceSourceV1::TaskSession {
                                            attempt: identity,
                                            evidence,
                                        });
                                    }
                                    Err(error) => {
                                        omissions.push(hydration_omission("task_session", error))
                                    }
                                }
                            } else if receipt.evidence.is_none() {
                                omissions.push(WorkEvidenceOmissionV1 {
                                    relation: "attempt_receipt".to_owned(),
                                    reason: WorkEvidenceOmissionReasonV1::Pending,
                                });
                            }
                        }
                        Err(WorkAttemptReceiptReadErrorV1::NotFoundOrNotAuthorized) => {
                            omissions.push(WorkEvidenceOmissionV1 {
                                relation: "attempt_receipt".to_owned(),
                                reason: WorkEvidenceOmissionReasonV1::NotFoundOrNotAuthorized,
                            });
                        }
                        Err(WorkAttemptReceiptReadErrorV1::Unavailable) => {
                            omissions.push(WorkEvidenceOmissionV1 {
                                relation: "attempt_receipt".to_owned(),
                                reason: WorkEvidenceOmissionReasonV1::Unavailable,
                            });
                        }
                    }
                }
                SelectedSource::Anchor(link) => {
                    let cursor = anchor_cursor(&request, link.link_id());
                    match self
                        .anchors
                        .hydrate_anchor(
                            context,
                            WorkAnchorHydrationRequestV1 {
                                anchor_id: link.anchor_id().clone(),
                                temporal: request.temporal,
                                page_size: request.page_size,
                                continuation: cursor,
                                observed_at: request.observed_at,
                            },
                        )
                        .await
                    {
                        Ok(hydration) => {
                            validate_anchor(&link, &hydration)?;
                            source_partial |=
                                hydration.coverage != WorkEvidenceCoverageStateV1::Complete;
                            freshness = merge_freshness(freshness, hydration.freshness);
                            redacted |= hydration.redacted;
                            if let Some(cursor) = hydration.continuation.clone() {
                                continuations.push(WorkEvidenceContinuationV1::Anchor {
                                    link_id: link.link_id().clone(),
                                    cursor,
                                });
                            }
                            hydrated = hydrated.saturating_add(1);
                            sources.push(WorkEvidenceSourceV1::Anchor { link, hydration });
                        }
                        Err(error) => omissions.push(hydration_omission("evidence_anchor", error)),
                    }
                }
            }
        }

        let selected_count = selected.selected_count;
        let omitted = u32::try_from(omissions.len())
            .map_err(|_| WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable)?;
        for omission in &omissions {
            match omission.reason {
                WorkEvidenceOmissionReasonV1::Stale => {
                    freshness = merge_freshness(freshness, WorkEvidenceFreshnessV1::Stale)
                }
                WorkEvidenceOmissionReasonV1::Redacted => redacted = true,
                WorkEvidenceOmissionReasonV1::LimitReached
                | WorkEvidenceOmissionReasonV1::Pending
                | WorkEvidenceOmissionReasonV1::NotFoundOrNotAuthorized
                | WorkEvidenceOmissionReasonV1::Unavailable
                | WorkEvidenceOmissionReasonV1::ResetRequired
                | WorkEvidenceOmissionReasonV1::Cancelled
                | WorkEvidenceOmissionReasonV1::TimedOut => {
                    freshness = merge_freshness(freshness, WorkEvidenceFreshnessV1::Unknown)
                }
            }
        }
        let coverage = WorkEvidenceCoverageV1 {
            state: overall_coverage_state(&omissions, &continuations, source_partial),
            selected: selected_count,
            hydrated,
            omitted,
        };
        Ok(WorkEvidenceRetrievalV1 {
            task_id: request.task_id,
            verified_version: root.verified_version,
            item: root.item,
            relations: root.relations,
            proposal_decisions: root.proposal_decisions,
            relation_replan_decisions: root.relation_replan_decisions,
            sources,
            coverage,
            omissions,
            freshness,
            redacted,
            continuations,
        })
    }

    fn authorize_root(
        &self,
        context: &RequestContext,
        request: &WorkEvidenceRetrieveRequestV1,
    ) -> Result<VerifiedWorkEvidenceRootV1, WorkProductApplicationErrorV1> {
        if !context.allows(self.binding.capability_id(), self.binding.use_case_id()) {
            return Err(WorkProductApplicationErrorV1::NotAuthorized);
        }
        match context.admission_at(request.observed_at) {
            RequestAdmission::Admitted => {}
            RequestAdmission::Cancelled => return Err(WorkProductApplicationErrorV1::Cancelled),
            RequestAdmission::TimedOut => return Err(WorkProductApplicationErrorV1::TimedOut),
        }
        request
            .selection
            .validate()
            .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
        let scope = self
            .owner_authority
            .authorize_scope(context, &request.selection, request.observed_at)
            .map_err(|error| match error {
                WorkProductOwnerAuthorizationErrorV1::NotAuthorized => {
                    WorkProductApplicationErrorV1::NotAuthorized
                }
                WorkProductOwnerAuthorizationErrorV1::Unavailable => {
                    WorkProductApplicationErrorV1::GraphAuthorityUnavailable
                }
            })?;
        if scope.selection() != &request.selection {
            return Err(WorkProductApplicationErrorV1::GraphAuthorityUnavailable);
        }
        let port_context =
            WorkProductPortContextV1::from_request(context, scope, request.observed_at);
        let root = self
            .roots
            .read_evidence_root(&port_context, &request.task_id, &request.verified_version)
            .map_err(root_error)?;
        validate_root(request, &root)?;
        Ok(root)
    }
}

impl<R, A, T, S, H> WorkTaskSessionReauthorizationPortV1
    for WorkEvidenceRetrievalServiceV1<R, A, T, S, H>
where
    R: WorkEvidenceRootReadPortV1,
    A: WorkProductOwnerAuthorizationPortV1,
    T: WorkAttemptReceiptReadPortV1,
    S: WorkTaskSessionPortV1,
    H: WorkAnchorHydrationPortV1,
{
    fn reauthorize_task_session(
        &self,
        context: &RequestContext,
        request: &WorkTaskSessionRequestV1,
    ) -> Result<(), WorkTaskSessionReauthorizationErrorV1> {
        let root_request = WorkEvidenceRetrieveRequestV1 {
            selection: request.selection.clone(),
            task_id: request.task_id.clone(),
            verified_version: request.verified_version.clone(),
            temporal: request.temporal,
            page_size: request.page_size,
            expansion: Some(WorkEvidenceExpansionSelectorV1::TaskSession {
                attempt: request.attempt.clone(),
            }),
            continuation: request
                .continuation
                .clone()
                .map(|continuation| WorkEvidenceContinuationV1::TaskSession { continuation }),
            observed_at: request.observed_at,
        };
        let root = self
            .authorize_root(context, &root_request)
            .map_err(task_session_reauthorization_error)?;
        if root.item.accepted_attempts() != &request.accepted_attempts
            || !root.item.accepted_attempts().contains(&request.attempt)
        {
            return Err(WorkTaskSessionReauthorizationErrorV1::Stale);
        }
        let authority =
            work_authority(context).map_err(|_| WorkTaskSessionReauthorizationErrorV1::Denied)?;
        let receipt = self
            .attempts
            .attempt_receipt(&authority, &request.attempt)
            .map_err(|error| match error {
                WorkAttemptReceiptReadErrorV1::NotFoundOrNotAuthorized => {
                    WorkTaskSessionReauthorizationErrorV1::Denied
                }
                WorkAttemptReceiptReadErrorV1::Unavailable => {
                    WorkTaskSessionReauthorizationErrorV1::Unavailable
                }
            })?;
        if receipt
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.provider_session.as_ref())
            != Some(&request.source)
        {
            return Err(WorkTaskSessionReauthorizationErrorV1::Stale);
        }
        Ok(())
    }
}

fn overall_coverage_state(
    omissions: &[WorkEvidenceOmissionV1],
    continuations: &[WorkEvidenceContinuationV1],
    source_partial: bool,
) -> WorkEvidenceCoverageStateV1 {
    if omissions.is_empty() && continuations.is_empty() && !source_partial {
        WorkEvidenceCoverageStateV1::Complete
    } else {
        WorkEvidenceCoverageStateV1::Partial
    }
}

enum SelectedSource {
    Attempt(WorkAttemptIdentityV1),
    Anchor(TaskEvidenceLinkV1),
}

struct SelectedSources {
    sources: Vec<SelectedSource>,
    selected_count: u32,
    omissions: Vec<WorkEvidenceOmissionV1>,
}

fn select_sources(
    root: &VerifiedWorkEvidenceRootV1,
    request: &WorkEvidenceRetrieveRequestV1,
) -> Result<SelectedSources, WorkProductApplicationErrorV1> {
    let mut all = Vec::new();
    if let Some(expansion) = &request.expansion {
        match expansion {
            WorkEvidenceExpansionSelectorV1::Anchor { link_id } => {
                let link = root
                    .links
                    .iter()
                    .find(|link| link.link_id() == link_id)
                    .cloned()
                    .ok_or(WorkProductApplicationErrorV1::NotFoundOrNotAuthorized)?;
                all.push(SelectedSource::Anchor(link));
            }
            WorkEvidenceExpansionSelectorV1::TaskSession { attempt } => {
                if !root.item.accepted_attempts().contains(attempt) {
                    return Err(WorkProductApplicationErrorV1::NotFoundOrNotAuthorized);
                }
                all.push(SelectedSource::Attempt(attempt.clone()));
            }
        }
    } else {
        all.extend(
            root.item
                .accepted_attempts()
                .iter()
                .cloned()
                .map(SelectedSource::Attempt),
        );
        all.extend(root.links.iter().cloned().map(SelectedSource::Anchor));
    }
    let selected_count = u32::try_from(all.len())
        .map_err(|_| WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable)?;
    let limit = usize::try_from(request.page_size)
        .map_err(|_| WorkProductApplicationErrorV1::InvalidRequest)?;
    let omitted_count = all.len().saturating_sub(limit);
    all.truncate(limit);
    let omissions = (0..omitted_count)
        .map(|_| WorkEvidenceOmissionV1 {
            relation: "task_evidence".to_owned(),
            reason: WorkEvidenceOmissionReasonV1::LimitReached,
        })
        .collect();
    Ok(SelectedSources {
        sources: all,
        selected_count,
        omissions,
    })
}

fn validate_request(
    request: &WorkEvidenceRetrieveRequestV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if request.page_size == 0 || request.page_size > MAX_WORK_ROOTED_EVIDENCE_SOURCES_V1 {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    let continuation_matches = match (&request.expansion, &request.continuation) {
        (_, None) => true,
        (
            Some(WorkEvidenceExpansionSelectorV1::Anchor { link_id }),
            Some(WorkEvidenceContinuationV1::Anchor {
                link_id: cursor_link,
                ..
            }),
        ) => link_id == cursor_link,
        (
            Some(WorkEvidenceExpansionSelectorV1::TaskSession { attempt }),
            Some(WorkEvidenceContinuationV1::TaskSession { continuation }),
        ) => {
            attempt == &continuation.attempt
                && request.task_id == *continuation.attempt.task_id()
                && request.verified_version == continuation.verified_version
        }
        _ => false,
    };
    if !continuation_matches {
        return Err(WorkProductApplicationErrorV1::InvalidRequest);
    }
    Ok(())
}

fn validate_root(
    request: &WorkEvidenceRetrieveRequestV1,
    root: &VerifiedWorkEvidenceRootV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if root.verified_version != request.verified_version
        || root.item.task_id() != &request.task_id
        || root
            .links
            .iter()
            .any(|link| link.task_id() != &request.task_id)
        || root
            .relations
            .iter()
            .any(|relation| !relation_touches_task(relation, &request.task_id))
        || root
            .proposal_decisions
            .iter()
            .any(|decision| decision.proposal().task_id() != &request.task_id)
        || root
            .relation_replan_decisions
            .iter()
            .any(|decision| decision.proposal.task_id.as_str() != request.task_id.as_str())
        || root
            .links
            .windows(2)
            .any(|pair| pair[0].link_id() >= pair[1].link_id())
    {
        return Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable);
    }
    Ok(())
}

fn relation_touches_task(relation: &WorkProductRelationV1, task_id: &TaskId) -> bool {
    match relation {
        WorkProductRelationV1::MilestoneContainsTask { task_id: task, .. }
        | WorkProductRelationV1::Evidence { task_id: task, .. }
        | WorkProductRelationV1::AcceptedAttempt { task_id: task, .. }
        | WorkProductRelationV1::Handoff { task_id: task, .. }
        | WorkProductRelationV1::ProposalDecision { task_id: task, .. } => task == task_id,
        WorkProductRelationV1::Gates {
            dependency,
            dependent,
        } => dependency == task_id || dependent == task_id,
        WorkProductRelationV1::Informational { source, target } => {
            source == task_id || target == task_id
        }
        WorkProductRelationV1::CausalCandidate { cause, effect } => {
            cause == task_id || effect == task_id
        }
        WorkProductRelationV1::InitiativeContainsPlan { .. }
        | WorkProductRelationV1::PlanContainsMilestone { .. } => false,
    }
}

fn validate_task_session(
    request: &WorkEvidenceRetrieveRequestV1,
    receipt: &WorkAttemptReceiptV1,
    evidence: &WorkTaskSessionEvidenceV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if receipt
        .evidence
        .as_ref()
        .and_then(|evidence| evidence.provider_session.as_ref())
        != Some(&evidence.source)
        || evidence.task_id != request.task_id
        || evidence.verified_version != request.verified_version
        || evidence.attempt != receipt.identity
        || evidence
            .ranked_anchors
            .windows(2)
            .any(|pair| pair[0].final_ordinal >= pair[1].final_ordinal)
        || evidence.hydrated.iter().any(|hydrated| {
            !evidence
                .ranked_anchors
                .iter()
                .any(|ranked| ranked.anchor_id == hydrated.anchor_id)
        })
        || evidence.continuation.as_ref().is_some_and(|continuation| {
            continuation.verified_version != evidence.verified_version
                || continuation.attempt != evidence.attempt
                || continuation.source != evidence.source
                || continuation.participant_epoch != evidence.participant_epoch
        })
    {
        return Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable);
    }
    Ok(())
}

fn task_session_reauthorization_error(
    error: WorkProductApplicationErrorV1,
) -> WorkTaskSessionReauthorizationErrorV1 {
    match error {
        WorkProductApplicationErrorV1::NotAuthorized
        | WorkProductApplicationErrorV1::NotFoundOrNotAuthorized => {
            WorkTaskSessionReauthorizationErrorV1::Denied
        }
        WorkProductApplicationErrorV1::VersionConflict => {
            WorkTaskSessionReauthorizationErrorV1::Stale
        }
        WorkProductApplicationErrorV1::InvalidRequest
        // Reauthorization has no vocabulary for "widen your selection", and a
        // slice-bounded reading cannot reauthorize a session it may not have
        // observed, so it is reported as the typed unavailability rather than
        // as a denial that would read as a revocation.
        | WorkProductApplicationErrorV1::SelectionCoverageIncomplete
        | WorkProductApplicationErrorV1::RevisionConflict
        | WorkProductApplicationErrorV1::IdempotencyConflict
        | WorkProductApplicationErrorV1::EventAuthorityUnavailable
        | WorkProductApplicationErrorV1::GraphAuthorityUnavailable
        | WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
        | WorkProductApplicationErrorV1::ProposalAuthorityUnavailable
        | WorkProductApplicationErrorV1::Cancelled
        | WorkProductApplicationErrorV1::TimedOut => {
            WorkTaskSessionReauthorizationErrorV1::Unavailable
        }
    }
}

fn validate_anchor(
    link: &TaskEvidenceLinkV1,
    hydration: &WorkAnchorHydrationV1,
) -> Result<(), WorkProductApplicationErrorV1> {
    if hydration.anchor_id != *link.anchor_id()
        || !hydration
            .exact_anchors
            .iter()
            .any(|anchor| anchor == link.anchor_id())
    {
        return Err(WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable);
    }
    Ok(())
}

fn task_session_continuation(
    request: &WorkEvidenceRetrieveRequestV1,
    identity: &WorkAttemptIdentityV1,
) -> Option<WorkTaskSessionContinuationV1> {
    match &request.continuation {
        Some(WorkEvidenceContinuationV1::TaskSession { continuation })
            if &continuation.attempt == identity =>
        {
            Some(continuation.clone())
        }
        _ => None,
    }
}

fn anchor_cursor(
    request: &WorkEvidenceRetrieveRequestV1,
    link_id: &TaskEvidenceLinkId,
) -> Option<OpaqueCursor> {
    match &request.continuation {
        Some(WorkEvidenceContinuationV1::Anchor {
            link_id: cursor_link,
            cursor,
        }) if cursor_link == link_id => Some(cursor.clone()),
        _ => None,
    }
}

fn merge_freshness(
    left: WorkEvidenceFreshnessV1,
    right: WorkEvidenceFreshnessV1,
) -> WorkEvidenceFreshnessV1 {
    match (left, right) {
        (WorkEvidenceFreshnessV1::Unknown, _) | (_, WorkEvidenceFreshnessV1::Unknown) => {
            WorkEvidenceFreshnessV1::Unknown
        }
        (WorkEvidenceFreshnessV1::Stale, _) | (_, WorkEvidenceFreshnessV1::Stale) => {
            WorkEvidenceFreshnessV1::Stale
        }
        _ => WorkEvidenceFreshnessV1::Current,
    }
}

fn hydration_omission(
    relation: &str,
    error: WorkEvidenceHydrationErrorV1,
) -> WorkEvidenceOmissionV1 {
    let reason = match error {
        WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized => {
            WorkEvidenceOmissionReasonV1::NotFoundOrNotAuthorized
        }
        WorkEvidenceHydrationErrorV1::Unavailable => WorkEvidenceOmissionReasonV1::Unavailable,
        WorkEvidenceHydrationErrorV1::ResetRequired => WorkEvidenceOmissionReasonV1::ResetRequired,
        WorkEvidenceHydrationErrorV1::Stale => WorkEvidenceOmissionReasonV1::Stale,
        WorkEvidenceHydrationErrorV1::Cancelled => WorkEvidenceOmissionReasonV1::Cancelled,
        WorkEvidenceHydrationErrorV1::TimedOut => WorkEvidenceOmissionReasonV1::TimedOut,
    };
    WorkEvidenceOmissionV1 {
        relation: relation.to_owned(),
        reason,
    }
}

fn root_error(error: WorkEvidenceRootReadErrorV1) -> WorkProductApplicationErrorV1 {
    match error {
        WorkEvidenceRootReadErrorV1::NotFoundOrNotAuthorized => {
            WorkProductApplicationErrorV1::NotFoundOrNotAuthorized
        }
        WorkEvidenceRootReadErrorV1::Stale => WorkProductApplicationErrorV1::VersionConflict,
        WorkEvidenceRootReadErrorV1::Unavailable => {
            WorkProductApplicationErrorV1::EvidenceAuthorityUnavailable
        }
        WorkEvidenceRootReadErrorV1::Cancelled => WorkProductApplicationErrorV1::Cancelled,
        WorkEvidenceRootReadErrorV1::TimedOut => WorkProductApplicationErrorV1::TimedOut,
    }
}

#[cfg(test)]
mod tests;
