//! Exact native-Git integration identities, previews, approvals, and receipts.
//!
//! These values contain no filesystem paths, generic Git arguments, remote
//! operations, or mutable provider state. Native Git remains authoritative;
//! persisted values are immutable evidence used for compare-and-set.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    ActorId, BranchStackRevisionV1, CapabilityId, DomainError, ManifestDigest,
    NativeIntegrationApprovalId, NativeIntegrationPreviewId, NativeIntegrationTransactionId,
    ProjectId, RefId, RepositoryId, StackNodeId, UtcMicros, WorktreeId, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId, canonical_sha256,
};
use crate::{
    CodeGenerationId, ContentDigest, GitHeadStateV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, SourceSpan,
};

const STACK_SELECTION_DIGEST_DOMAIN: &str = "tracedecay.native-integration.stack-selection.v1";
const INDEPENDENT_SELECTION_DIGEST_DOMAIN: &str =
    "tracedecay.native-integration.independent-selection.v1";
const REPOSITORY_SNAPSHOT_DIGEST_DOMAIN: &str =
    "tracedecay.native-integration.repository-snapshot.v1";
const PREVIEW_DIGEST_DOMAIN: &str = "tracedecay.native-integration.preview.v1";
const APPROVAL_DIGEST_DOMAIN: &str = "tracedecay.native-integration.approval.v1";
const RECEIPT_DIGEST_DOMAIN: &str = "tracedecay.native-integration.receipt.v1";
const ANALYSIS_REPORT_DIGEST_DOMAIN: &str = "tracedecay.native-integration.analysis-report.v1";
const ANALYSIS_CONFLICT_DIGEST_DOMAIN: &str = "tracedecay.native-integration.analysis-conflict.v1";

/// Explicit direction of one integration. Stack meaning is never inferred.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationDirectionV1 {
    PropagateDependencyToDependent,
    LandDependentIntoDependency,
    IntegrateIndependentBranch,
}

/// The only Git histories this product operation can create.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum MechanicalIntegrationModeV1 {
    FastForward,
    TwoParentMerge,
    CherryPickExactCommits,
}

/// Exact visible stack revision and declared edge selected for preflight.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenBranchStackSnapshotV1 {
    pub revision: BranchStackRevisionV1,
    pub source_node_id: StackNodeId,
    pub destination_node_id: StackNodeId,
    pub direction: NativeIntegrationDirectionV1,
    pub captured_at: UtcMicros,
    pub digest: ManifestDigest,
}

impl FrozenBranchStackSnapshotV1 {
    pub fn new(
        revision: BranchStackRevisionV1,
        source_node_id: StackNodeId,
        destination_node_id: StackNodeId,
        direction: NativeIntegrationDirectionV1,
        captured_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let mut value = Self {
            revision,
            source_node_id,
            destination_node_id,
            direction,
            captured_at,
            digest: pending_digest()?,
        };
        value.validate_selection()?;
        value.digest = value.compute_digest()?;
        Ok(value)
    }

    pub fn source(&self) -> Result<&super::BranchStackNodeV1, DomainError> {
        self.revision
            .nodes
            .iter()
            .find(|node| node.node_id == self.source_node_id)
            .ok_or(DomainError::UnknownReference {
                field: "native integration source node",
            })
    }

    pub fn destination(&self) -> Result<&super::BranchStackNodeV1, DomainError> {
        self.revision
            .nodes
            .iter()
            .find(|node| node.node_id == self.destination_node_id)
            .ok_or(DomainError::UnknownReference {
                field: "native integration destination node",
            })
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            STACK_SELECTION_DIGEST_DOMAIN,
            &self.revision.digest,
            &self.source_node_id,
            &self.destination_node_id,
            self.direction,
            self.captured_at,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_selection()?;
        if self.compute_digest()? != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_selection(&self) -> Result<(), DomainError> {
        self.revision.validate()?;
        self.source_node_id.validate()?;
        self.destination_node_id.validate()?;
        if self.source_node_id == self.destination_node_id
            || self.direction == NativeIntegrationDirectionV1::IntegrateIndependentBranch
        {
            return Err(DomainError::NonCanonical {
                field: "native integration stack selection",
            });
        }
        self.source()?;
        self.destination()?;
        let declared = match self.direction {
            NativeIntegrationDirectionV1::PropagateDependencyToDependent => {
                self.revision.edges.iter().any(|edge| {
                    edge.dependency == self.source_node_id
                        && edge.dependent == self.destination_node_id
                })
            }
            NativeIntegrationDirectionV1::LandDependentIntoDependency => {
                self.revision.edges.iter().any(|edge| {
                    edge.dependency == self.destination_node_id
                        && edge.dependent == self.source_node_id
                })
            }
            NativeIntegrationDirectionV1::IntegrateIndependentBranch => false,
        };
        if !declared {
            return Err(DomainError::UnknownReference {
                field: "native integration declared stack edge",
            });
        }
        Ok(())
    }
}

/// Exact same-repository branch pair selected by a separately authorized
/// independent-branch proposal.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenIndependentBranchSelectionV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub inventory_snapshot_id: WorktreeInventorySnapshotId,
    pub inventory_epoch: WorktreeInventoryEpoch,
    pub source_worktree_id: Option<WorktreeId>,
    pub destination_worktree_id: Option<WorktreeId>,
    pub source_ref: RefId,
    pub destination_ref: RefId,
    pub source_tip: GitOidV1,
    pub destination_tip: GitOidV1,
    pub proposal_digest: ManifestDigest,
    pub captured_at: UtcMicros,
    pub digest: ManifestDigest,
}

impl FrozenIndependentBranchSelectionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: ProjectId,
        repository_id: RepositoryId,
        inventory_snapshot_id: WorktreeInventorySnapshotId,
        inventory_epoch: WorktreeInventoryEpoch,
        source_worktree_id: Option<WorktreeId>,
        destination_worktree_id: Option<WorktreeId>,
        source_ref: RefId,
        destination_ref: RefId,
        source_tip: GitOidV1,
        destination_tip: GitOidV1,
        proposal_digest: ManifestDigest,
        captured_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let mut value = Self {
            project_id,
            repository_id,
            inventory_snapshot_id,
            inventory_epoch,
            source_worktree_id,
            destination_worktree_id,
            source_ref,
            destination_ref,
            source_tip,
            destination_tip,
            proposal_digest,
            captured_at,
            digest: pending_digest()?,
        };
        value.validate_fields()?;
        value.digest = value.compute_digest()?;
        Ok(value)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            INDEPENDENT_SELECTION_DIGEST_DOMAIN,
            &self.project_id,
            &self.repository_id,
            &self.inventory_snapshot_id,
            self.inventory_epoch,
            &self.source_worktree_id,
            &self.destination_worktree_id,
            &self.source_ref,
            &self.destination_ref,
            &self.source_tip,
            &self.destination_tip,
            &self.proposal_digest,
            self.captured_at,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.compute_digest()? != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.inventory_snapshot_id.validate()?;
        self.inventory_epoch.validate()?;
        self.source_worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)?;
        self.destination_worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)?;
        self.source_ref.validate()?;
        self.destination_ref.validate()?;
        self.source_tip.validate()?;
        self.destination_tip.validate()?;
        self.proposal_digest.validate()?;
        if self.source_ref == self.destination_ref
            || self.source_tip.format() != self.destination_tip.format()
            || self.source_worktree_id == self.destination_worktree_id
                && self.source_worktree_id.is_some()
        {
            return Err(DomainError::NonCanonical {
                field: "native integration independent selection",
            });
        }
        Ok(())
    }
}

/// One frozen, path-free selection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "selection", rename_all = "snake_case")]
pub enum NativeIntegrationSelectionV1 {
    DeclaredStackEdge(FrozenBranchStackSnapshotV1),
    IndependentBranch(FrozenIndependentBranchSelectionV1),
}

impl NativeIntegrationSelectionV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => value.validate(),
            Self::IndependentBranch(value) => value.validate(),
        }
    }

    pub fn project_id(&self) -> Result<&ProjectId, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => Ok(&value.source()?.project_id),
            Self::IndependentBranch(value) => Ok(&value.project_id),
        }
    }

    pub fn repository_id(&self) -> Result<&RepositoryId, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => Ok(&value.source()?.repository_id),
            Self::IndependentBranch(value) => Ok(&value.repository_id),
        }
    }

    pub fn source_ref(&self) -> Result<&RefId, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => Ok(&value.source()?.reference),
            Self::IndependentBranch(value) => Ok(&value.source_ref),
        }
    }

    pub fn destination_ref(&self) -> Result<&RefId, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => Ok(&value.destination()?.reference),
            Self::IndependentBranch(value) => Ok(&value.destination_ref),
        }
    }

    pub fn source_worktree_id(&self) -> Result<Option<&WorktreeId>, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => Ok(value.source()?.worktree_id.as_ref()),
            Self::IndependentBranch(value) => Ok(value.source_worktree_id.as_ref()),
        }
    }

    pub fn destination_worktree_id(&self) -> Result<Option<&WorktreeId>, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => Ok(value.destination()?.worktree_id.as_ref()),
            Self::IndependentBranch(value) => Ok(value.destination_worktree_id.as_ref()),
        }
    }

    pub fn source_tip(&self) -> Result<GitOidV1, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => {
                GitOidV1::new(value.source()?.tip.as_str().to_owned())
            }
            Self::IndependentBranch(value) => Ok(value.source_tip.clone()),
        }
    }

    pub fn destination_tip(&self) -> Result<GitOidV1, DomainError> {
        match self {
            Self::DeclaredStackEdge(value) => {
                GitOidV1::new(value.destination()?.tip.as_str().to_owned())
            }
            Self::IndependentBranch(value) => Ok(value.destination_tip.clone()),
        }
    }

    pub fn digest(&self) -> &ManifestDigest {
        match self {
            Self::DeclaredStackEdge(value) => &value.digest,
            Self::IndependentBranch(value) => &value.digest,
        }
    }
}

/// Exact native repository and worktree state used for CAS.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationRepositorySnapshotV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub source_worktree_id: Option<WorktreeId>,
    pub destination_worktree_id: Option<WorktreeId>,
    pub source_ref: RefId,
    pub destination_ref: RefId,
    pub source_tip: GitOidV1,
    pub destination_tip: GitOidV1,
    pub source_tree: GitOidV1,
    pub destination_tree: GitOidV1,
    pub merge_base: GitOidV1,
    pub dependency_commits: Vec<GitOidV1>,
    pub destination_head: GitHeadStateV1,
    pub refs_digest: ManifestDigest,
    pub index_digest: ManifestDigest,
    pub worktree_digest: ManifestDigest,
    pub attributes_digest: ManifestDigest,
    pub operation_state: GitOperationStateV1,
    pub clean: bool,
    pub object_format: GitObjectFormatV1,
    pub adapter_revision: String,
    pub captured_at: UtcMicros,
    pub digest: ManifestDigest,
}

impl NativeIntegrationRepositorySnapshotV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.validate_fields()?;
        self.digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            REPOSITORY_SNAPSHOT_DIGEST_DOMAIN,
            (
                &self.project_id,
                &self.repository_id,
                &self.source_worktree_id,
                &self.destination_worktree_id,
                &self.source_ref,
                &self.destination_ref,
                &self.source_tip,
                &self.destination_tip,
            ),
            (
                &self.source_tree,
                &self.destination_tree,
                &self.merge_base,
                &self.dependency_commits,
                &self.destination_head,
                &self.refs_digest,
                &self.index_digest,
                &self.worktree_digest,
            ),
            (
                &self.attributes_digest,
                self.operation_state,
                self.clean,
                self.object_format,
                &self.adapter_revision,
                self.captured_at,
            ),
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.compute_digest()? != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.source_ref.validate()?;
        self.destination_ref.validate()?;
        self.destination_head.validate()?;
        self.refs_digest.validate()?;
        self.index_digest.validate()?;
        self.worktree_digest.validate()?;
        self.attributes_digest.validate()?;
        if self.adapter_revision.is_empty() || self.source_ref == self.destination_ref {
            return Err(DomainError::NonCanonical {
                field: "native integration repository snapshot",
            });
        }
        let format = self.object_format;
        for object in [
            &self.source_tip,
            &self.destination_tip,
            &self.source_tree,
            &self.destination_tree,
            &self.merge_base,
        ]
        .into_iter()
        .chain(self.dependency_commits.iter())
        {
            object.validate()?;
            if object.format() != format {
                return Err(DomainError::SnapshotMismatch {
                    field: "native integration object format",
                });
            }
        }
        Ok(())
    }
}

/// Why a preflight cannot authorize apply.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationUnavailabilityV1 {
    PartialEvidence,
    StaleScope,
    Denied,
    NativeStateUnavailable,
    ResetRequired,
    DurabilityUncertain,
    UnsupportedHooks,
    SigningRequired,
    DestinationOccupied,
}

/// One immutable code-generation binding used by native integration analysis.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationGenerationBindingV1 {
    pub generation_id: CodeGenerationId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub snapshot_digest: ManifestDigest,
    pub content_identity: ContentDigest,
    pub source_revision: Option<GitOidV1>,
    pub source_tree: GitOidV1,
    pub seal_digest: ManifestDigest,
}

impl NativeIntegrationGenerationBindingV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.generation_id.validate()?;
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)?;
        self.reference.as_ref().map_or(Ok(()), RefId::validate)?;
        self.snapshot_digest.validate()?;
        self.content_identity.validate()?;
        self.source_revision
            .as_ref()
            .map_or(Ok(()), GitOidV1::validate)?;
        self.source_tree.validate()?;
        self.seal_digest.validate()
    }
}

/// Completeness of one independently evaluated semantic evidence lane.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationAnalysisCoverageV1 {
    Complete,
    Partial,
    Unsupported,
}

/// Why one semantic evidence lane cannot authorize integration.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationAnalysisGapV1 {
    ParserFailure,
    UnresolvedRequiredEdge,
    WithheldSource,
    StaleEvidence,
    UnsupportedLanguage,
    DynamicSchema,
    UnboundMigrationOrder,
    CapacityUnavailable,
    AuthorityUnavailable,
}

/// Coverage and exact gaps for one independently evaluated analysis lane.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationAnalysisLaneV1 {
    pub coverage: NativeIntegrationAnalysisCoverageV1,
    pub gaps: Vec<NativeIntegrationAnalysisGapV1>,
}

impl NativeIntegrationAnalysisLaneV1 {
    fn validate(&self) -> Result<(), DomainError> {
        if (self.coverage == NativeIntegrationAnalysisCoverageV1::Complete) != self.gaps.is_empty()
            || self.gaps.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(DomainError::NonCanonical {
                field: "native integration analysis lane coverage",
            });
        }
        Ok(())
    }
}

/// Static conflict class produced from canonical generation evidence.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationSemanticConflictKindV1 {
    DivergentSymbol,
    SignatureDependent,
    TestWriteInteraction,
    DivergentSchema,
    MigrationOrder,
}

/// Source-local anchor for a semantic conflict finding.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationAnalysisAnchorV1 {
    pub generation_id: CodeGenerationId,
    pub logical_path: String,
    pub source_span: SourceSpan,
}

impl NativeIntegrationAnalysisAnchorV1 {
    fn validate(&self) -> Result<(), DomainError> {
        self.generation_id.validate()?;
        crate::validate_code_logical_path(&self.logical_path)?;
        self.source_span.validate()
    }
}

/// One deterministic conflict finding with source evidence from both branches.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationSemanticConflictV1 {
    pub kind: NativeIntegrationSemanticConflictKindV1,
    pub source: NativeIntegrationAnalysisAnchorV1,
    pub destination: NativeIntegrationAnalysisAnchorV1,
    pub evidence_digest: ManifestDigest,
}

impl NativeIntegrationSemanticConflictV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.source.validate()?;
        self.destination.validate()?;
        self.evidence_digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            ANALYSIS_CONFLICT_DIGEST_DOMAIN,
            self.kind,
            &self.source,
            &self.destination,
        ))
    }

    fn validate(&self) -> Result<(), DomainError> {
        self.source.validate()?;
        self.destination.validate()?;
        if self.compute_digest()? != self.evidence_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Daemon-produced semantic evidence for one exact native candidate tree.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationAnalysisReportV1 {
    pub merge_base: NativeIntegrationGenerationBindingV1,
    pub source: NativeIntegrationGenerationBindingV1,
    pub destination: NativeIntegrationGenerationBindingV1,
    pub candidate: NativeIntegrationGenerationBindingV1,
    pub graph: NativeIntegrationAnalysisLaneV1,
    pub tests: NativeIntegrationAnalysisLaneV1,
    pub schema: NativeIntegrationAnalysisLaneV1,
    pub migrations: NativeIntegrationAnalysisLaneV1,
    pub conflicts: Vec<NativeIntegrationSemanticConflictV1>,
    pub analyzer_revision: String,
    pub digest: ManifestDigest,
}

impl NativeIntegrationAnalysisReportV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.validate_fields()?;
        self.digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.compute_digest()? != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        [&self.graph, &self.tests, &self.schema, &self.migrations]
            .into_iter()
            .all(|lane| lane.coverage == NativeIntegrationAnalysisCoverageV1::Complete)
    }

    fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            ANALYSIS_REPORT_DIGEST_DOMAIN,
            &self.merge_base,
            &self.source,
            &self.destination,
            &self.candidate,
            &self.graph,
            &self.tests,
            &self.schema,
            &self.migrations,
            &self.conflicts,
            &self.analyzer_revision,
        ))
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.merge_base.validate()?;
        self.source.validate()?;
        self.destination.validate()?;
        self.candidate.validate()?;
        for lane in [&self.graph, &self.tests, &self.schema, &self.migrations] {
            lane.validate()?;
        }
        for conflict in &self.conflicts {
            conflict.validate()?;
        }
        if self.analyzer_revision.is_empty()
            || self.candidate.source_revision.is_some()
            || self.candidate.worktree_id != self.destination.worktree_id
            || self.candidate.reference != self.destination.reference
            || [
                &self.merge_base.source_tree,
                &self.source.source_tree,
                &self.destination.source_tree,
            ]
            .into_iter()
            .any(|tree| tree.format() != self.candidate.source_tree.format())
            || [
                &self.merge_base,
                &self.source,
                &self.destination,
                &self.candidate,
            ]
            .into_iter()
            .any(|binding| {
                binding.project_id != self.destination.project_id
                    || binding.repository_id != self.destination.repository_id
            })
            || self.conflicts.windows(2).any(|pair| {
                (&pair[0].kind, &pair[0].source, &pair[0].destination)
                    >= (&pair[1].kind, &pair[1].source, &pair[1].destination)
            })
        {
            return Err(DomainError::NonCanonical {
                field: "native integration analysis report",
            });
        }
        Ok(())
    }
}

/// Truthful preview classification. Only `MechanicalIntegrationEligible`
/// carries apply-eligible evidence.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum NativeIntegrationPreviewDispositionV1 {
    MechanicalIntegrationEligible(MechanicalIntegrationModeV1),
    AlreadyIntegrated,
    NativeConflict {
        conflict_digest: ManifestDigest,
    },
    SemanticReviewRequired {
        evidence_digest: ManifestDigest,
    },
    Partial {
        reason: NativeIntegrationUnavailabilityV1,
    },
    Unavailable {
        reason: NativeIntegrationUnavailabilityV1,
    },
}

/// Immutable preview over one exact repository snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationPreviewV1 {
    pub preview_id: NativeIntegrationPreviewId,
    pub selection: NativeIntegrationSelectionV1,
    pub repository_snapshot: NativeIntegrationRepositorySnapshotV1,
    pub grant_digest: ManifestDigest,
    pub policy_digest: ManifestDigest,
    pub analysis: Option<NativeIntegrationAnalysisReportV1>,
    pub disposition: NativeIntegrationPreviewDispositionV1,
    pub candidate_tree: Option<GitOidV1>,
    pub ordered_commits: Vec<GitOidV1>,
    pub created_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub preview_digest: ManifestDigest,
}

impl NativeIntegrationPreviewV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.validate_fields()?;
        self.preview_digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            PREVIEW_DIGEST_DOMAIN,
            &self.preview_id,
            self.selection.digest(),
            &self.repository_snapshot.digest,
            &self.grant_digest,
            &self.policy_digest,
            &self.analysis,
            &self.disposition,
            &self.candidate_tree,
            &self.ordered_commits,
            self.created_at,
            self.expires_at,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.compute_digest()? != self.preview_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.preview_id.validate()?;
        self.selection.validate()?;
        self.repository_snapshot.validate()?;
        for digest in [&self.grant_digest, &self.policy_digest] {
            digest.validate()?;
        }
        self.analysis
            .as_ref()
            .map_or(Ok(()), NativeIntegrationAnalysisReportV1::validate)?;
        if self.selection.project_id()? != &self.repository_snapshot.project_id
            || self.selection.repository_id()? != &self.repository_snapshot.repository_id
            || self.selection.source_ref()? != &self.repository_snapshot.source_ref
            || self.selection.destination_ref()? != &self.repository_snapshot.destination_ref
            || self.created_at.0 >= self.expires_at.0
        {
            return Err(DomainError::SnapshotMismatch {
                field: "native integration preview scope",
            });
        }
        let requires_analysis = matches!(
            self.disposition,
            NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
                | NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. }
        );
        if self.candidate_tree.is_some() != self.analysis.is_some()
            || (requires_analysis && self.analysis.is_none())
            || matches!(
                self.disposition,
                NativeIntegrationPreviewDispositionV1::AlreadyIntegrated
                    | NativeIntegrationPreviewDispositionV1::NativeConflict { .. }
            ) && self.analysis.is_some()
        {
            return Err(DomainError::SnapshotMismatch {
                field: "native integration candidate tree",
            });
        }
        if let (Some(candidate), Some(analysis)) = (&self.candidate_tree, &self.analysis)
            && (candidate != &analysis.candidate.source_tree
                || analysis.merge_base.source_revision.as_ref()
                    != Some(&self.repository_snapshot.merge_base)
                || analysis.source.source_revision.as_ref()
                    != Some(&self.repository_snapshot.source_tip)
                || analysis.source.source_tree != self.repository_snapshot.source_tree
                || analysis.destination.source_revision.as_ref()
                    != Some(&self.repository_snapshot.destination_tip)
                || analysis.destination.source_tree != self.repository_snapshot.destination_tree
                || analysis.source.reference.as_ref() != Some(&self.repository_snapshot.source_ref)
                || analysis.destination.reference.as_ref()
                    != Some(&self.repository_snapshot.destination_ref)
                || analysis.candidate.reference.as_ref()
                    != Some(&self.repository_snapshot.destination_ref))
        {
            return Err(DomainError::SnapshotMismatch {
                field: "native integration analysis binding",
            });
        }
        match (&self.disposition, &self.analysis) {
            (
                NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_),
                Some(analysis),
            ) if analysis.is_complete() && analysis.conflicts.is_empty() => {}
            (
                NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { evidence_digest },
                Some(analysis),
            ) if evidence_digest == &analysis.digest
                && (!analysis.is_complete() || !analysis.conflicts.is_empty()) => {}
            (NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_), _)
            | (NativeIntegrationPreviewDispositionV1::SemanticReviewRequired { .. }, _) => {
                return Err(DomainError::SnapshotMismatch {
                    field: "native integration semantic disposition",
                });
            }
            _ => {}
        }
        if let Some(candidate) = &self.candidate_tree {
            candidate.validate()?;
            if candidate.format() != self.repository_snapshot.object_format {
                return Err(DomainError::SnapshotMismatch {
                    field: "native integration candidate object format",
                });
            }
        }
        for commit in &self.ordered_commits {
            commit.validate()?;
            if commit.format() != self.repository_snapshot.object_format {
                return Err(DomainError::SnapshotMismatch {
                    field: "native integration ordered commit format",
                });
            }
        }
        Ok(())
    }
}

/// One-use, content-bound approval for an exact eligible preview.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationApprovalV1 {
    pub approval_id: NativeIntegrationApprovalId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub principal: ActorId,
    pub delegated_agent: Option<ActorId>,
    pub capability: CapabilityId,
    pub grant_digest: ManifestDigest,
    pub issued_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub approval_digest: ManifestDigest,
}

impl NativeIntegrationApprovalV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.validate_fields()?;
        self.approval_digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            APPROVAL_DIGEST_DOMAIN,
            &self.approval_id,
            &self.preview_id,
            &self.preview_digest,
            &self.principal,
            &self.delegated_agent,
            &self.capability,
            &self.grant_digest,
            self.issued_at,
            self.expires_at,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.compute_digest()? != self.approval_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.approval_id.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.principal.validate()?;
        self.delegated_agent
            .as_ref()
            .map_or(Ok(()), ActorId::validate)?;
        self.capability.validate()?;
        self.grant_digest.validate()?;
        if self.issued_at.0 >= self.expires_at.0 {
            return Err(DomainError::NonCanonical {
                field: "native integration approval expiry",
            });
        }
        Ok(())
    }
}

/// Durable transaction phase. `RefCommitStarted` is the cancellation boundary.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationPhaseV1 {
    Prepared,
    CandidateVerified,
    RefCommitStarted,
    FinalStateVerification,
    Terminal,
}

/// The only truthful terminal outcomes after recovery.
#[derive(Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeIntegrationTerminalOutcomeV1 {
    Committed,
    AbortedNoChange,
    RolledBack,
    NeedsInspection,
}

/// Durable transaction status used for status, cancellation, and restart.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationTransactionStatusV1 {
    pub transaction_id: NativeIntegrationTransactionId,
    pub preview_id: NativeIntegrationPreviewId,
    pub preview_digest: ManifestDigest,
    pub approval_id: NativeIntegrationApprovalId,
    pub repository_id: RepositoryId,
    pub destination_ref: RefId,
    pub expected_destination_tip: GitOidV1,
    pub candidate_tip: Option<GitOidV1>,
    pub phase: NativeIntegrationPhaseV1,
    pub phase_revision: u64,
    pub cancellation_requested: bool,
    pub terminal_outcome: Option<NativeIntegrationTerminalOutcomeV1>,
    pub updated_at: UtcMicros,
}

impl NativeIntegrationTransactionStatusV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.transaction_id.validate()?;
        self.preview_id.validate()?;
        self.preview_digest.validate()?;
        self.approval_id.validate()?;
        self.repository_id.validate()?;
        self.destination_ref.validate()?;
        self.expected_destination_tip.validate()?;
        self.candidate_tip
            .as_ref()
            .map_or(Ok(()), GitOidV1::validate)?;
        if self.phase_revision == 0
            || (self.phase == NativeIntegrationPhaseV1::Terminal) != self.terminal_outcome.is_some()
        {
            return Err(DomainError::NonCanonical {
                field: "native integration transaction status",
            });
        }
        Ok(())
    }
}

/// Final, content-bound apply evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeIntegrationReceiptV1 {
    pub status: NativeIntegrationTransactionStatusV1,
    pub final_ref_tip: GitOidV1,
    pub final_tree: GitOidV1,
    pub final_index_digest: ManifestDigest,
    pub final_worktree_digest: ManifestDigest,
    pub completed_at: UtcMicros,
    pub receipt_digest: ManifestDigest,
}

impl NativeIntegrationReceiptV1 {
    pub fn seal(mut self) -> Result<Self, DomainError> {
        self.validate_fields()?;
        self.receipt_digest = self.compute_digest()?;
        Ok(self)
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            RECEIPT_DIGEST_DOMAIN,
            &self.status,
            &self.final_ref_tip,
            &self.final_tree,
            &self.final_index_digest,
            &self.final_worktree_digest,
            self.completed_at,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        if self.compute_digest()? != self.receipt_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.status.validate()?;
        self.final_ref_tip.validate()?;
        self.final_tree.validate()?;
        self.final_index_digest.validate()?;
        self.final_worktree_digest.validate()?;
        if self.status.phase != NativeIntegrationPhaseV1::Terminal
            || self.status.terminal_outcome.is_none()
            || self.final_ref_tip.format() != self.final_tree.format()
        {
            return Err(DomainError::NonCanonical {
                field: "native integration receipt",
            });
        }
        Ok(())
    }
}

fn pending_digest() -> Result<ManifestDigest, DomainError> {
    canonical_sha256(&"pending")
}
