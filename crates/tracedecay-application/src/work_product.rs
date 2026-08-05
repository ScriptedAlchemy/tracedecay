//! Plan 24 product journey over an injected canonical topology handle.
//!
//! The port is deliberately storage-agnostic. The daemon adapter owns the
//! sole project/profile graph handle and may implement this boundary with
//! `Arc<GraphDb>`; this crate never opens a database or discovers a registry.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    AcceptanceCriterionId, ManifestDigest, ProposalId, TaskEvidenceLinkId, TaskEvidenceLinkV1,
    TaskId, UtcMicros, WorkAttemptIdentityV1, WorkCommandId, WorkGraphChangeV1, WorkGraphVersionV1,
    WorkHandoffV1, WorkProductContractError, WorkProductGraphV1, WorkProductProjectionBundleV1,
    WorkProposalDispositionV1, WorkProposalV1, WorkProposedChildV1, WorkProviderOutcomeV1,
    WorkProviderRouteV1, WorkRouteDecisionV1, WorkShapeAssessmentV1, WorkSizingV1,
    WorkTaskEvidenceV1, canonical_sha256,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::{RequestAdmission, RequestContext, work::work_authority};

const WORK_PRODUCT_INPUT_DIGEST_DOMAIN: &str = "tracedecay.application.work-product-command.v1";

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkProductOperationV1 {
    ProductSnapshot,
    ProductProjections,
    TaskEvidence,
    ExpandTaskEvidence,
    GenerateWorkProposal,
    ApplyWorkCommand,
}

impl WorkProductOperationV1 {
    pub const ALL: [Self; 6] = [
        Self::ProductSnapshot,
        Self::ProductProjections,
        Self::TaskEvidence,
        Self::ExpandTaskEvidence,
        Self::GenerateWorkProposal,
        Self::ApplyWorkCommand,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            Self::ProductSnapshot => "product_snapshot",
            Self::ProductProjections => "product_projections",
            Self::TaskEvidence => "task_evidence",
            Self::ExpandTaskEvidence => "expand_task_evidence",
            Self::GenerateWorkProposal => "generate_work_proposal",
            Self::ApplyWorkCommand => "apply_work_command",
        }
    }

    pub fn capability_id(self) -> Result<CapabilityId, WorkProductApplicationError> {
        CapabilityId::new(format!("capability.work.{}", self.key()))
            .map_err(|_| WorkProductApplicationError::InvalidRequest)
    }

    pub fn use_case_id(self) -> Result<UseCaseId, WorkProductApplicationError> {
        UseCaseId::new(format!("use-case.work.{}", self.key()))
            .map_err(|_| WorkProductApplicationError::InvalidRequest)
    }

    pub const fn is_read_only(self) -> bool {
        !matches!(self, Self::ApplyWorkCommand)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkProductApplicationError {
    #[error("Work product operation is not authorized")]
    NotAuthorized,
    #[error("Work product operation was cancelled before admission")]
    Cancelled,
    #[error("Work product operation timed out before admission")]
    TimedOut,
    #[error("Work product was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work product graph changed after the command was prepared")]
    VersionConflict,
    #[error("Work product command identity was reused with different input")]
    IdempotencyConflict,
    #[error("Work product command is invalid")]
    InvalidRequest,
    #[error("Work product topology is unavailable")]
    TopologyUnavailable,
    #[error("Work product evidence is unavailable")]
    EvidenceUnavailable,
}

impl From<WorkProductContractError> for WorkProductApplicationError {
    fn from(_error: WorkProductContractError) -> Self {
        Self::InvalidRequest
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum WorkTopologyPortError {
    #[error("Work product was not found or is not authorized")]
    NotFoundOrNotAuthorized,
    #[error("Work product graph version changed")]
    VersionConflict,
    #[error("Work product command identity conflicts")]
    IdempotencyConflict,
    #[error("Work product topology is unavailable")]
    Unavailable,
}

impl From<WorkTopologyPortError> for WorkProductApplicationError {
    fn from(error: WorkTopologyPortError) -> Self {
        match error {
            WorkTopologyPortError::NotFoundOrNotAuthorized => Self::NotFoundOrNotAuthorized,
            WorkTopologyPortError::VersionConflict => Self::VersionConflict,
            WorkTopologyPortError::IdempotencyConflict => Self::IdempotencyConflict,
            WorkTopologyPortError::Unavailable => Self::TopologyUnavailable,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkTopologyReadV1 {
    Current(WorkProductGraphV1),
    Stale {
        graph: WorkProductGraphV1,
        observed_at: UtcMicros,
    },
    Partial {
        graph: WorkProductGraphV1,
        unknowns: Vec<String>,
    },
}

impl WorkTopologyReadV1 {
    pub const fn graph(&self) -> &WorkProductGraphV1 {
        match self {
            Self::Current(graph) | Self::Stale { graph, .. } | Self::Partial { graph, .. } => graph,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WorkProductProjectionReadV1 {
    Current(WorkProductProjectionBundleV1),
    Stale {
        projections: WorkProductProjectionBundleV1,
        observed_at: UtcMicros,
    },
    Partial {
        projections: WorkProductProjectionBundleV1,
        unknowns: Vec<String>,
    },
}

impl WorkProductProjectionReadV1 {
    pub const fn projections(&self) -> &WorkProductProjectionBundleV1 {
        match self {
            Self::Current(projections)
            | Self::Stale { projections, .. }
            | Self::Partial { projections, .. } => projections,
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTopologyCasRequestV1 {
    authority: tracedecay_domain::WorkAuthority,
    expected_version: Option<WorkGraphVersionV1>,
    command_id: WorkCommandId,
    input_digest: ManifestDigest,
    replacement: WorkProductGraphV1,
}

impl WorkTopologyCasRequestV1 {
    fn new(
        authority: tracedecay_domain::WorkAuthority,
        expected_version: Option<WorkGraphVersionV1>,
        command_id: WorkCommandId,
        input_digest: ManifestDigest,
        replacement: WorkProductGraphV1,
    ) -> Result<Self, WorkProductApplicationError> {
        replacement.validate()?;
        let valid_replacement = match expected_version {
            Some(expected) => expected
                .next()
                .is_ok_and(|next| next == replacement.version()),
            None => replacement.version() == WorkGraphVersionV1::initial(),
        };
        if !valid_replacement {
            return Err(WorkProductApplicationError::InvalidRequest);
        }
        Ok(Self {
            authority,
            expected_version,
            command_id,
            input_digest,
            replacement,
        })
    }

    pub fn authority(&self) -> &tracedecay_domain::WorkAuthority {
        &self.authority
    }

    pub const fn expected_version(&self) -> Option<WorkGraphVersionV1> {
        self.expected_version
    }

    pub fn command_id(&self) -> &WorkCommandId {
        &self.command_id
    }

    pub fn input_digest(&self) -> &ManifestDigest {
        &self.input_digest
    }

    pub fn replacement(&self) -> &WorkProductGraphV1 {
        &self.replacement
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductMutationReceiptV1 {
    graph: WorkProductGraphV1,
    replayed: bool,
    command_id: WorkCommandId,
    projections: WorkProductProjectionBundleV1,
}

impl WorkProductMutationReceiptV1 {
    pub fn new(
        graph: WorkProductGraphV1,
        replayed: bool,
        command_id: WorkCommandId,
    ) -> Result<Self, WorkProductContractError> {
        graph.validate()?;
        let projections = WorkProductProjectionBundleV1::from_graph(&graph)?;
        Ok(Self {
            graph,
            replayed,
            command_id,
            projections,
        })
    }

    fn into_replayed(mut self) -> Self {
        self.replayed = true;
        self
    }

    pub const fn graph(&self) -> &WorkProductGraphV1 {
        &self.graph
    }

    pub const fn replayed(&self) -> bool {
        self.replayed
    }

    pub const fn projections(&self) -> &WorkProductProjectionBundleV1 {
        &self.projections
    }

    pub fn command_id(&self) -> &WorkCommandId {
        &self.command_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkTopologyCommitV1 {
    Committed(WorkProductMutationReceiptV1),
    Replayed(WorkProductMutationReceiptV1),
}

impl WorkTopologyCommitV1 {
    fn into_receipt(self) -> WorkProductMutationReceiptV1 {
        match self {
            Self::Committed(receipt) => receipt,
            Self::Replayed(receipt) => receipt.into_replayed(),
        }
    }
}

pub trait WorkTopologyPort: Send + Sync {
    fn read(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
    ) -> Result<WorkTopologyReadV1, WorkTopologyPortError>;

    fn compare_and_swap(
        &self,
        request: &WorkTopologyCasRequestV1,
    ) -> Result<WorkTopologyCommitV1, WorkTopologyPortError>;

    /// Returns the original receipt for same-command/same-input replay.
    ///
    /// This check runs before current-version validation so a response lost
    /// after commit can be recovered even after later commands have advanced
    /// the graph. Changed input under the same command identity conflicts.
    fn replay(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
        command_id: &WorkCommandId,
        input_digest: &ManifestDigest,
    ) -> Result<Option<WorkProductMutationReceiptV1>, WorkTopologyPortError>;
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkEvidenceExpansionV1 {
    link: TaskEvidenceLinkV1,
    content_handle: String,
    redacted: bool,
}

impl WorkEvidenceExpansionV1 {
    pub fn new(
        link: TaskEvidenceLinkV1,
        content_handle: String,
        redacted: bool,
    ) -> Result<Self, WorkProductApplicationError> {
        if !tracedecay_domain::canonical_text::is_canonical_text_within(&content_handle, 2_048) {
            return Err(WorkProductApplicationError::InvalidRequest);
        }
        Ok(Self {
            link,
            content_handle,
            redacted,
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
}

pub trait WorkEvidencePort: Send + Sync {
    fn task_evidence(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
        task_id: &TaskId,
        graph_version: WorkGraphVersionV1,
        limit: u32,
    ) -> Result<WorkTaskEvidenceV1, WorkProductApplicationError>;

    fn expand(
        &self,
        authority: &tracedecay_domain::WorkAuthority,
        task_id: &TaskId,
        link_id: &TaskEvidenceLinkId,
    ) -> Result<WorkEvidenceExpansionV1, WorkProductApplicationError>;
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductSnapshotRequestV1 {}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkProductProjectionsRequestV1 {}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTaskEvidenceRequestV1 {
    pub task_id: TaskId,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExpandWorkEvidenceRequestV1 {
    pub task_id: TaskId,
    pub link_id: TaskEvidenceLinkId,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkProductCommandV1 {
    pub graph: WorkProductGraphV1,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerateWorkProposalRequestV1 {
    pub proposal_id: ProposalId,
    pub task_id: TaskId,
    pub shape: WorkShapeAssessmentV1,
    pub sizing: WorkSizingV1,
    pub children: Vec<WorkProposedChildV1>,
    pub route: WorkRouteDecisionV1,
    pub explanation: String,
    pub evidence_limit: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkProductCommandV1 {
    LinkEvidence {
        task_id: TaskId,
        evidence: TaskEvidenceLinkV1,
    },
    DecideProposal {
        proposal: WorkProposalV1,
        disposition: WorkProposalDispositionV1,
    },
    AdmitProvider {
        task_id: TaskId,
        proposal_id: ProposalId,
        identity: WorkAttemptIdentityV1,
        route: WorkProviderRouteV1,
    },
    RecordProviderOutcome {
        task_id: TaskId,
        outcome: WorkProviderOutcomeV1,
    },
    AcceptTask {
        task_id: TaskId,
        evidence_by_criterion: BTreeMap<AcceptanceCriterionId, TaskEvidenceLinkId>,
    },
    RecordHandoff {
        handoff: WorkHandoffV1,
    },
    CancelAttempt {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
    },
    RetryAttempt {
        task_id: TaskId,
        prior_identity: WorkAttemptIdentityV1,
        identity: WorkAttemptIdentityV1,
        route: WorkProviderRouteV1,
    },
    RollbackAdmission {
        task_id: TaskId,
        identity: WorkAttemptIdentityV1,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplyWorkProductCommandV1 {
    pub expected_version: WorkGraphVersionV1,
    pub command_id: WorkCommandId,
    pub occurred_at: UtcMicros,
    pub command: WorkProductCommandV1,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkProductMutationRequestV1 {
    Create(CreateWorkProductCommandV1),
    Apply(ApplyWorkProductCommandV1),
}

pub struct WorkProductService<T, E> {
    topology: T,
    evidence: E,
}

impl<T, E> WorkProductService<T, E>
where
    T: WorkTopologyPort,
    E: WorkEvidencePort,
{
    pub const fn new(topology: T, evidence: E) -> Self {
        Self { topology, evidence }
    }

    pub fn create(
        &self,
        context: &RequestContext,
        command: CreateWorkProductCommandV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::ApplyWorkCommand)?;
        admit(context, command.occurred_at)?;
        if command.graph.version() != WorkGraphVersionV1::initial() {
            return Err(WorkProductApplicationError::InvalidRequest);
        }
        command.graph.validate()?;
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        let input_digest = input_digest(&(
            WORK_PRODUCT_INPUT_DIGEST_DOMAIN,
            "create",
            &command.graph,
            command.occurred_at,
        ))?;
        self.topology
            .compare_and_swap(&WorkTopologyCasRequestV1::new(
                authority,
                None,
                command.command_id,
                input_digest,
                command.graph,
            )?)
            .map(WorkTopologyCommitV1::into_receipt)
            .map_err(Into::into)
    }

    pub fn execute_mutation(
        &self,
        context: &RequestContext,
        request: WorkProductMutationRequestV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationError> {
        match request {
            WorkProductMutationRequestV1::Create(command) => self.create(context, command),
            WorkProductMutationRequestV1::Apply(command) => self.apply(context, command),
        }
    }

    pub fn snapshot(
        &self,
        context: &RequestContext,
    ) -> Result<WorkTopologyReadV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::ProductSnapshot)?;
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        self.topology.read(&authority).map_err(Into::into)
    }

    pub fn projections(
        &self,
        context: &RequestContext,
    ) -> Result<WorkProductProjectionReadV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::ProductProjections)?;
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        match self.topology.read(&authority)? {
            WorkTopologyReadV1::Current(graph) => WorkProductProjectionBundleV1::from_graph(&graph)
                .map(WorkProductProjectionReadV1::Current)
                .map_err(Into::into),
            WorkTopologyReadV1::Stale { graph, observed_at } => {
                WorkProductProjectionBundleV1::from_graph(&graph)
                    .map(|projections| WorkProductProjectionReadV1::Stale {
                        projections,
                        observed_at,
                    })
                    .map_err(Into::into)
            }
            WorkTopologyReadV1::Partial { graph, unknowns } => {
                WorkProductProjectionBundleV1::from_graph(&graph)
                    .map(|projections| WorkProductProjectionReadV1::Partial {
                        projections,
                        unknowns,
                    })
                    .map_err(Into::into)
            }
        }
    }

    pub fn task_evidence(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
        limit: u32,
    ) -> Result<WorkTaskEvidenceV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::TaskEvidence)?;
        if limit == 0 || limit > 1_024 {
            return Err(WorkProductApplicationError::InvalidRequest);
        }
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        let graph = current_graph(self.topology.read(&authority)?)?;
        if graph.item(task_id).is_none() {
            return Err(WorkProductApplicationError::NotFoundOrNotAuthorized);
        }
        self.evidence
            .task_evidence(&authority, task_id, graph.version(), limit)
    }

    pub fn expand_evidence(
        &self,
        context: &RequestContext,
        task_id: &TaskId,
        link_id: &TaskEvidenceLinkId,
    ) -> Result<WorkEvidenceExpansionV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::ExpandTaskEvidence)?;
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        let graph = current_graph(self.topology.read(&authority)?)?;
        if graph.item(task_id).is_none() {
            return Err(WorkProductApplicationError::NotFoundOrNotAuthorized);
        }
        self.evidence.expand(&authority, task_id, link_id)
    }

    pub fn generate_proposal(
        &self,
        context: &RequestContext,
        request: GenerateWorkProposalRequestV1,
    ) -> Result<WorkProposalV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::GenerateWorkProposal)?;
        if request.evidence_limit == 0 || request.evidence_limit > 1_024 {
            return Err(WorkProductApplicationError::InvalidRequest);
        }
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        let graph = current_graph(self.topology.read(&authority)?)?;
        if graph.item(&request.task_id).is_none() {
            return Err(WorkProductApplicationError::NotFoundOrNotAuthorized);
        }
        let evidence = self.evidence.task_evidence(
            &authority,
            &request.task_id,
            graph.version(),
            request.evidence_limit,
        )?;
        let evidence_digest =
            canonical_sha256(&evidence).map_err(|_| WorkProductApplicationError::InvalidRequest)?;
        WorkProposalV1::new(
            request.proposal_id,
            request.task_id,
            graph.version(),
            request.shape,
            request.sizing,
            request.children,
            request.route,
            request.explanation,
            evidence_digest,
        )
        .map_err(Into::into)
    }

    pub fn apply(
        &self,
        context: &RequestContext,
        request: ApplyWorkProductCommandV1,
    ) -> Result<WorkProductMutationReceiptV1, WorkProductApplicationError> {
        authorize(context, WorkProductOperationV1::ApplyWorkCommand)?;
        admit(context, request.occurred_at)?;
        let authority =
            work_authority(context).map_err(|_| WorkProductApplicationError::NotAuthorized)?;
        let input_digest = input_digest(&(
            WORK_PRODUCT_INPUT_DIGEST_DOMAIN,
            "apply",
            request.expected_version,
            &request.command,
            request.occurred_at,
        ))?;
        if let Some(receipt) =
            self.topology
                .replay(&authority, &request.command_id, &input_digest)?
        {
            return Ok(receipt.into_replayed());
        }
        let graph = current_graph(self.topology.read(&authority)?)?;
        if graph.version() != request.expected_version {
            return Err(WorkProductApplicationError::VersionConflict);
        }
        ensure_command_legal(&graph, &request.command)?;
        let change = graph_change(request.command.clone(), request.occurred_at);
        let replacement = graph.apply(change)?;
        self.topology
            .compare_and_swap(&WorkTopologyCasRequestV1::new(
                authority,
                Some(request.expected_version),
                request.command_id,
                input_digest,
                replacement,
            )?)
            .map(WorkTopologyCommitV1::into_receipt)
            .map_err(Into::into)
    }
}

fn current_graph(
    read: WorkTopologyReadV1,
) -> Result<WorkProductGraphV1, WorkProductApplicationError> {
    match read {
        WorkTopologyReadV1::Current(graph) => {
            graph.validate()?;
            Ok(graph)
        }
        WorkTopologyReadV1::Stale { .. } | WorkTopologyReadV1::Partial { .. } => {
            Err(WorkProductApplicationError::TopologyUnavailable)
        }
    }
}

fn graph_change(command: WorkProductCommandV1, occurred_at: UtcMicros) -> WorkGraphChangeV1 {
    match command {
        WorkProductCommandV1::LinkEvidence { task_id, evidence } => {
            WorkGraphChangeV1::EvidenceLinked { task_id, evidence }
        }
        WorkProductCommandV1::DecideProposal {
            proposal,
            disposition: WorkProposalDispositionV1::Accepted,
        } => WorkGraphChangeV1::ProposalAccepted {
            proposal,
            accepted_at: occurred_at,
        },
        WorkProductCommandV1::DecideProposal {
            proposal,
            disposition,
        } => WorkGraphChangeV1::ProposalDecided {
            proposal,
            disposition,
            decided_at: occurred_at,
        },
        WorkProductCommandV1::AdmitProvider {
            task_id,
            proposal_id,
            identity,
            route,
        } => WorkGraphChangeV1::ProviderAdmitted {
            task_id,
            proposal_id,
            identity,
            route,
            admitted_at: occurred_at,
        },
        WorkProductCommandV1::RecordProviderOutcome { task_id, outcome } => {
            WorkGraphChangeV1::ProviderOutcomeRecorded { task_id, outcome }
        }
        WorkProductCommandV1::AcceptTask {
            task_id,
            evidence_by_criterion,
        } => WorkGraphChangeV1::TaskAccepted {
            task_id,
            evidence_by_criterion,
            accepted_at: occurred_at,
        },
        WorkProductCommandV1::RecordHandoff { handoff } => {
            WorkGraphChangeV1::HandoffRecorded { handoff }
        }
        WorkProductCommandV1::CancelAttempt { task_id, identity } => {
            WorkGraphChangeV1::AttemptCancelled {
                task_id,
                identity,
                cancelled_at: occurred_at,
            }
        }
        WorkProductCommandV1::RetryAttempt {
            task_id,
            prior_identity,
            identity,
            route,
        } => WorkGraphChangeV1::AttemptRetried {
            task_id,
            prior_identity,
            identity,
            route,
            admitted_at: occurred_at,
        },
        WorkProductCommandV1::RollbackAdmission { task_id, identity } => {
            WorkGraphChangeV1::AdmissionRolledBack {
                task_id,
                identity,
                rolled_back_at: occurred_at,
            }
        }
    }
}

fn ensure_command_legal(
    graph: &WorkProductGraphV1,
    command: &WorkProductCommandV1,
) -> Result<(), WorkProductApplicationError> {
    if let WorkProductCommandV1::AdmitProvider { task_id, .. } = command {
        let item = graph
            .item(task_id)
            .ok_or(WorkProductApplicationError::NotFoundOrNotAuthorized)?;
        if item.dependencies().iter().any(|dependency| {
            graph
                .item(dependency)
                .is_none_or(|dependency| !dependency.is_accepted())
        }) {
            return Err(WorkProductApplicationError::InvalidRequest);
        }
    }
    Ok(())
}

fn authorize(
    context: &RequestContext,
    operation: WorkProductOperationV1,
) -> Result<(), WorkProductApplicationError> {
    if context.allows(&operation.capability_id()?, &operation.use_case_id()?) {
        Ok(())
    } else {
        Err(WorkProductApplicationError::NotAuthorized)
    }
}

fn admit(
    context: &RequestContext,
    occurred_at: UtcMicros,
) -> Result<(), WorkProductApplicationError> {
    match context.admission_at(occurred_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(WorkProductApplicationError::Cancelled),
        RequestAdmission::TimedOut => Err(WorkProductApplicationError::TimedOut),
    }
}

fn input_digest<T: Serialize>(value: &T) -> Result<ManifestDigest, WorkProductApplicationError> {
    canonical_sha256(value).map_err(|_| WorkProductApplicationError::InvalidRequest)
}
