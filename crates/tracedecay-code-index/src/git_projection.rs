//! Immutable Git topology projection over a verified graph snapshot.

mod declared_topology;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    GitCommitMetadataV1, GitCoverageV1, GitHeadStateV1, GitHistoryV1, GitOidV1, ManifestDigest,
    RefId, RepositoryId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId,
    GraphGenerationManifest, GraphGenerationRelation, GraphIdempotencyKey, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProjectorRevision,
    GraphProperty, GraphPropertyName, GraphRelationId, GraphRelationKind, GraphTraversalDirection,
    GraphWatermark, SourceGeneration, TraversalRequest, VerifiedGraphSnapshot,
};

use declared_topology::validate_declared_topology;
pub use declared_topology::{GitBranchStackBindingV1, GitWorktreeOccupancyV1};

const GIT_PROJECTION: &str = "git-topology";
const COMMIT_LABEL: &str = "GitCommit";
const BOUNDARY_COMMIT_LABEL: &str = "GitBoundaryCommit";
const REF_LABEL: &str = "GitRef";
const METADATA_LABEL: &str = "GitTopologyMetadata";
const COMMIT_RECORD_PROPERTY: &str = "commit-record";
const COMMIT_OID_PROPERTY: &str = "commit-oid";
const REF_RECORD_PROPERTY: &str = "ref-record";
const METADATA_RECORD_PROPERTY: &str = "metadata-record";
const PARENT_RELATION: &str = "GitParent";
const REF_TARGET_RELATION: &str = "GitRefTarget";

pub const GIT_TOPOLOGY_PROJECTOR_REVISION_V1: &str = "git-topology-projector.v1";

struct NeverCancelled;

impl GraphCancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitTopologyRefV1 {
    pub reference: RefId,
    pub target: Option<GitOidV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitTopologyProjectionV1 {
    pub repository: RepositoryId,
    pub head: GitHeadStateV1,
    pub refs: Vec<GitTopologyRefV1>,
    pub history: GitHistoryV1,
    pub ref_watermark: ManifestDigest,
    pub branch_stacks: Vec<GitBranchStackBindingV1>,
    pub worktree_occupancies: Vec<GitWorktreeOccupancyV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GitTopologyMetadataV1 {
    repository: RepositoryId,
    ref_watermark: ManifestDigest,
    coverage: GitCoverageV1,
    branch_stacks: Vec<GitBranchStackBindingV1>,
    worktree_occupancies: Vec<GitWorktreeOccupancyV1>,
}

impl GitTopologyProjectionV1 {
    pub fn validate(&self) -> Result<(), GitTopologyProjectionError> {
        self.repository
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        self.history
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        self.ref_watermark
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        if self.history.repository != self.repository {
            return Err(GitTopologyProjectionError::RepositoryMismatch);
        }
        validate_refs(&self.refs)?;
        validate_declared_topology(
            &self.repository,
            &self.branch_stacks,
            &self.worktree_occupancies,
        )?;
        let expected = git_topology_ref_watermark(&self.repository, &self.head, &self.refs)?;
        if self.ref_watermark != expected {
            return Err(GitTopologyProjectionError::RefWatermarkMismatch);
        }
        Ok(())
    }

    pub fn with_declared_topology(
        mut self,
        mut branch_stacks: Vec<GitBranchStackBindingV1>,
        mut worktree_occupancies: Vec<GitWorktreeOccupancyV1>,
    ) -> Result<Self, GitTopologyProjectionError> {
        branch_stacks.sort_by(declared_topology::compare_branch_stack_bindings);
        worktree_occupancies.sort_by(declared_topology::compare_worktree_occupancies);
        self.branch_stacks = branch_stacks;
        self.worktree_occupancies = worktree_occupancies;
        self.validate()?;
        Ok(self)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum GitTopologyProjectionError {
    #[error("Git topology contract violation: {0}")]
    Contract(String),
    #[error("Git topology projection belongs to another repository")]
    RepositoryMismatch,
    #[error("Git topology refs are duplicated or out of order")]
    NonCanonicalRefs,
    #[error("Git topology declared branch stacks are duplicated or out of order")]
    NonCanonicalBranchStacks,
    #[error("Git topology worktree occupancies are duplicated or out of order")]
    NonCanonicalWorktreeOccupancies,
    #[error("Git topology ref watermark does not match its canonical ref snapshot")]
    RefWatermarkMismatch,
    #[error("Git topology generation does not match")]
    GenerationMismatch,
    #[error("Git topology projection is stale")]
    Stale {
        projected: ManifestDigest,
        current: ManifestDigest,
    },
    #[error("Git topology declared binding is stale: {detail}")]
    StaleBinding { detail: &'static str },
    #[error("Git topology operation was cancelled")]
    Cancelled,
    #[error("Git topology traversal budget was exhausted")]
    BudgetExhausted,
    #[error("Git topology store is unavailable: {0}")]
    Unavailable(String),
    #[error("Git topology store is corrupt: {0}")]
    Corrupt(String),
}

impl From<GraphDbError> for GitTopologyProjectionError {
    fn from(error: GraphDbError) -> Self {
        match error {
            GraphDbError::Cancelled => Self::Cancelled,
            GraphDbError::BudgetExhausted { .. } | GraphDbError::DeadlineExceeded => {
                Self::BudgetExhausted
            }
            GraphDbError::InvalidRequest { message } => Self::Contract(message),
            GraphDbError::Corrupt { message }
            | GraphDbError::ResetRequired { message }
            | GraphDbError::DurabilityUncertain { message }
            | GraphDbError::ProjectionMismatch { message, .. }
            | GraphDbError::GenerationMismatch { message, .. } => Self::Corrupt(message),
            GraphDbError::Conflict => {
                Self::Unavailable("Git topology publication conflict".to_owned())
            }
            GraphDbError::Unavailable { message } => Self::Unavailable(message),
            GraphDbError::Closed => Self::Unavailable("graph store is closed".to_owned()),
        }
    }
}

pub fn git_topology_projection_identity(
    namespace: GraphNamespace,
) -> Result<GraphProjectionIdentity, GitTopologyProjectionError> {
    Ok(GraphProjectionIdentity::new(
        namespace,
        GraphProjectionId::new(GIT_PROJECTION)?,
    ))
}

pub fn git_topology_namespace(
    repository: &RepositoryId,
) -> Result<GraphNamespace, GitTopologyProjectionError> {
    repository
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    let digest = canonical_sha256(&("tracedecay.git-topology-namespace.v1", repository))
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    GraphNamespace::new(format!("git-scope:{}", digest.as_str())).map_err(Into::into)
}

pub fn git_topology_ref_watermark(
    repository: &RepositoryId,
    head: &GitHeadStateV1,
    refs: &[GitTopologyRefV1],
) -> Result<ManifestDigest, GitTopologyProjectionError> {
    repository
        .validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    head.validate()
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    validate_refs(refs)?;
    canonical_sha256(&(
        "tracedecay.git-topology-ref-watermark.v1",
        repository,
        head,
        refs,
    ))
    .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))
}

pub fn git_topology_generation_id(
    projection: &GitTopologyProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphGenerationId, GitTopologyProjectionError> {
    projection.validate()?;
    let digest = canonical_sha256(&(
        "tracedecay.git-topology-generation.v1",
        projection,
        projector_revision,
    ))
    .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    GraphGenerationId::new(format!("git-topology:{}", digest.as_str())).map_err(Into::into)
}

pub fn git_topology_idempotency_key(
    projection: &GitTopologyProjectionV1,
    projector_revision: &GraphProjectorRevision,
) -> Result<GraphIdempotencyKey, GitTopologyProjectionError> {
    let generation = git_topology_generation_id(projection, projector_revision)?;
    GraphIdempotencyKey::new(format!("publish:{}", generation.as_str())).map_err(Into::into)
}

pub fn build_git_topology_manifest_checked(
    identity: GraphProjectionIdentity,
    projection: &GitTopologyProjectionV1,
    projector_revision: &GraphProjectorRevision,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphGenerationManifest, GitTopologyProjectionError> {
    check()?;
    projection.validate()?;
    if identity.projection.as_str() != GIT_PROJECTION {
        return Err(GitTopologyProjectionError::Contract(
            "Git topology projection identity uses a foreign projector".to_owned(),
        ));
    }

    let commits = projection
        .history
        .commits
        .iter()
        .map(|commit| (commit.commit.clone(), commit))
        .collect::<BTreeMap<_, _>>();
    let mut all_oids = commits.keys().cloned().collect::<BTreeSet<_>>();
    for commit in commits.values() {
        check()?;
        all_oids.extend(commit.parents.iter().cloned());
    }
    all_oids.extend(
        projection
            .refs
            .iter()
            .filter_map(|reference| reference.target.clone()),
    );

    let mut entities = Vec::with_capacity(
        all_oids
            .len()
            .saturating_add(projection.refs.len())
            .saturating_add(1),
    );
    entities.push(metadata_entity(projection)?);
    for oid in all_oids {
        entities.push(commit_entity(oid.clone(), commits.get(&oid).copied())?);
    }
    for reference in &projection.refs {
        entities.push(ref_entity(reference)?);
    }

    let mut relations = Vec::new();
    for commit in commits.values() {
        for (parent_ordinal, parent) in commit.parents.iter().enumerate() {
            check()?;
            relations.push(parent_relation(
                &identity,
                &commit.commit,
                parent,
                parent_ordinal,
            )?);
        }
    }
    for reference in &projection.refs {
        if let Some(target) = &reference.target {
            relations.push(ref_target_relation(&identity, reference, target)?);
        }
    }

    let generation = git_topology_generation_id(projection, projector_revision)?;
    let source_digest = canonical_sha256(&(
        "tracedecay.git-topology-source.v1",
        &projection.repository,
        &projection.head,
        &projection.ref_watermark,
        &projection.branch_stacks,
        &projection.worktree_occupancies,
    ))
    .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
    GraphGenerationManifest::new_checked(
        identity,
        generation,
        SourceGeneration::new(source_digest.as_str())?,
        GraphWatermark::new(projection.ref_watermark.as_str())?,
        Vec::new(),
        entities,
        relations,
        check,
    )
    .map_err(Into::into)
}

#[derive(Clone)]
pub struct GitTopologyProjectionStore {
    snapshot: Arc<VerifiedGraphSnapshot>,
    projection: GraphProjectionIdentity,
    generation: GraphGenerationId,
    repository: RepositoryId,
    ref_watermark: ManifestDigest,
    coverage: GitCoverageV1,
    branch_stacks: Vec<GitBranchStackBindingV1>,
    worktree_occupancies: Vec<GitWorktreeOccupancyV1>,
}

impl fmt::Debug for GitTopologyProjectionStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitTopologyProjectionStore")
            .field("projection", &self.projection)
            .field("generation", &self.generation)
            .field("repository", &self.repository)
            .field("ref_watermark", &self.ref_watermark)
            .finish_non_exhaustive()
    }
}

impl GitTopologyProjectionStore {
    pub fn from_verified_snapshot(
        snapshot: VerifiedGraphSnapshot,
        projection: &GitTopologyProjectionV1,
    ) -> Result<Self, GitTopologyProjectionError> {
        let revision =
            GraphProjectorRevision::try_from(GIT_TOPOLOGY_PROJECTOR_REVISION_V1.to_owned())?;
        let expected = git_topology_generation_id(projection, &revision)?;
        if snapshot.generation() != &expected {
            return Err(GitTopologyProjectionError::GenerationMismatch);
        }
        let store = Self::from_verified_snapshot_verified(snapshot, Arc::new(NeverCancelled))?;
        if store.repository != projection.repository
            || store.ref_watermark != projection.ref_watermark
            || store.coverage != projection.history.coverage
            || store.branch_stacks != projection.branch_stacks
            || store.worktree_occupancies != projection.worktree_occupancies
        {
            return Err(GitTopologyProjectionError::GenerationMismatch);
        }
        Ok(store)
    }

    pub fn from_verified_snapshot_verified(
        snapshot: VerifiedGraphSnapshot,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Self, GitTopologyProjectionError> {
        if snapshot.projection().projection.as_str() != GIT_PROJECTION {
            return Err(GitTopologyProjectionError::Contract(
                "verified snapshot uses a foreign Git topology projector".to_owned(),
            ));
        }
        let metadata = read_metadata(&snapshot, cancellation)?;
        Ok(Self {
            projection: snapshot.projection().clone(),
            generation: snapshot.generation().clone(),
            repository: metadata.repository,
            ref_watermark: metadata.ref_watermark,
            coverage: metadata.coverage,
            branch_stacks: metadata.branch_stacks,
            worktree_occupancies: metadata.worktree_occupancies,
            snapshot: Arc::new(snapshot),
        })
    }

    pub fn repository(&self) -> &RepositoryId {
        &self.repository
    }

    pub fn ref_watermark(&self) -> &ManifestDigest {
        &self.ref_watermark
    }

    pub fn generation(&self) -> &GraphGenerationId {
        &self.generation
    }

    pub fn coverage(&self) -> &GitCoverageV1 {
        &self.coverage
    }

    pub fn verify_ref_watermark(
        &self,
        current: &ManifestDigest,
    ) -> Result<(), GitTopologyProjectionError> {
        if &self.ref_watermark == current {
            Ok(())
        } else {
            Err(GitTopologyProjectionError::Stale {
                projected: self.ref_watermark.clone(),
                current: current.clone(),
            })
        }
    }

    pub fn ancestors(
        &self,
        commit: &GitOidV1,
        max_depth: usize,
        max_results: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GitOidV1>, GitTopologyProjectionError> {
        self.traverse(
            commit,
            GraphTraversalDirection::Outgoing,
            max_depth,
            max_results,
            cancellation,
        )
    }

    pub fn descendants(
        &self,
        commit: &GitOidV1,
        max_depth: usize,
        max_results: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GitOidV1>, GitTopologyProjectionError> {
        self.traverse(
            commit,
            GraphTraversalDirection::Incoming,
            max_depth,
            max_results,
            cancellation,
        )
    }

    pub fn merge_base(
        &self,
        left: &GitOidV1,
        right: &GitOidV1,
        max_depth: usize,
        max_visits: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GitOidV1>, GitTopologyProjectionError> {
        if left == right {
            return Ok(Some(left.clone()));
        }
        let left_depths =
            self.ancestor_depths(left, max_depth, max_visits, Arc::clone(&cancellation))?;
        let right_depths = self.ancestor_depths(right, max_depth, max_visits, cancellation)?;
        Ok(left_depths
            .iter()
            .filter_map(|(oid, left_depth)| {
                right_depths
                    .get(oid)
                    .map(|right_depth| (oid, left_depth.max(right_depth)))
            })
            .min_by(|(left_oid, left_depth), (right_oid, right_depth)| {
                left_depth
                    .cmp(right_depth)
                    .then_with(|| left_oid.cmp(right_oid))
            })
            .map(|(oid, _)| oid.clone()))
    }

    fn traverse(
        &self,
        commit: &GitOidV1,
        direction: GraphTraversalDirection,
        max_depth: usize,
        max_results: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<GitOidV1>, GitTopologyProjectionError> {
        validate_budget(max_depth, max_results)?;
        commit
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        let result = self.snapshot.traverse(TraversalRequest {
            namespace: self.projection.namespace.clone(),
            start: commit_entity_id(commit)?,
            relation_kinds: BTreeSet::from([GraphRelationKind::new(PARENT_RELATION)?]),
            direction,
            max_depth,
            max_visits: max_results.saturating_add(1),
            max_results,
            cancellation: Arc::clone(&cancellation),
        })?;
        result
            .visits
            .into_iter()
            .map(|visit| self.commit_oid(&visit.entity, Arc::clone(&cancellation)))
            .filter(|result| result.as_ref() != Ok(commit))
            .collect()
    }

    fn ancestor_depths(
        &self,
        commit: &GitOidV1,
        max_depth: usize,
        max_visits: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<BTreeMap<GitOidV1, usize>, GitTopologyProjectionError> {
        validate_budget(max_depth, max_visits)?;
        let result = self.snapshot.traverse(TraversalRequest {
            namespace: self.projection.namespace.clone(),
            start: commit_entity_id(commit)?,
            relation_kinds: BTreeSet::from([GraphRelationKind::new(PARENT_RELATION)?]),
            direction: GraphTraversalDirection::Outgoing,
            max_depth,
            max_visits,
            max_results: max_visits,
            cancellation: Arc::clone(&cancellation),
        })?;
        let mut depths = BTreeMap::from([(commit.clone(), 0)]);
        for visit in result.visits {
            depths.insert(
                self.commit_oid(&visit.entity, Arc::clone(&cancellation))?,
                visit.depth,
            );
        }
        Ok(depths)
    }

    fn commit_oid(
        &self,
        reference: &GraphEntityRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GitOidV1, GitTopologyProjectionError> {
        let entity = self
            .snapshot
            .entity(reference, cancellation)?
            .ok_or_else(|| {
                GitTopologyProjectionError::Corrupt(
                    "Git topology traversal reached a missing commit".to_owned(),
                )
            })?;
        let property = entity
            .properties
            .get(&GraphPropertyName::new(COMMIT_OID_PROPERTY)?)
            .ok_or_else(|| {
                GitTopologyProjectionError::Corrupt(
                    "Git topology commit is missing its object ID".to_owned(),
                )
            })?;
        let GraphProperty::String(value) = property else {
            return Err(GitTopologyProjectionError::Corrupt(
                "Git topology commit object ID has the wrong type".to_owned(),
            ));
        };
        GitOidV1::new(value.clone())
            .map_err(|error| GitTopologyProjectionError::Corrupt(error.to_string()))
    }
}

fn validate_budget(max_depth: usize, max_results: usize) -> Result<(), GitTopologyProjectionError> {
    if max_depth == 0 || max_results == 0 {
        Err(GitTopologyProjectionError::Contract(
            "Git topology traversal bounds must be positive".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_refs(refs: &[GitTopologyRefV1]) -> Result<(), GitTopologyProjectionError> {
    for reference in refs {
        reference
            .reference
            .validate()
            .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        if let Some(target) = &reference.target {
            target
                .validate()
                .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))?;
        }
    }
    if refs
        .windows(2)
        .any(|pair| pair[0].reference >= pair[1].reference)
    {
        return Err(GitTopologyProjectionError::NonCanonicalRefs);
    }
    Ok(())
}

fn metadata_entity(
    projection: &GitTopologyProjectionV1,
) -> Result<GraphEntity, GitTopologyProjectionError> {
    let metadata = GitTopologyMetadataV1 {
        repository: projection.repository.clone(),
        ref_watermark: projection.ref_watermark.clone(),
        coverage: projection.history.coverage.clone(),
        branch_stacks: projection.branch_stacks.clone(),
        worktree_occupancies: projection.worktree_occupancies.clone(),
    };
    GraphEntity::new(
        metadata_entity_id()?,
        BTreeSet::from([GraphLabel::new(METADATA_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(METADATA_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(&metadata)?),
        )]),
    )
    .map_err(Into::into)
}

fn read_metadata(
    snapshot: &VerifiedGraphSnapshot,
    cancellation: Arc<dyn GraphCancellation>,
) -> Result<GitTopologyMetadataV1, GitTopologyProjectionError> {
    let reference = GraphEntityRef::new(snapshot.projection().clone(), metadata_entity_id()?);
    let entity = snapshot.entity(&reference, cancellation)?.ok_or_else(|| {
        GitTopologyProjectionError::Corrupt("Git topology metadata is missing".to_owned())
    })?;
    let property = entity
        .properties
        .get(&GraphPropertyName::new(METADATA_RECORD_PROPERTY)?)
        .ok_or_else(|| {
            GitTopologyProjectionError::Corrupt(
                "Git topology metadata record is missing".to_owned(),
            )
        })?;
    let GraphProperty::Bytes(encoded) = property else {
        return Err(GitTopologyProjectionError::Corrupt(
            "Git topology metadata record has the wrong type".to_owned(),
        ));
    };
    let metadata: GitTopologyMetadataV1 = serde_json::from_slice(encoded)
        .map_err(|error| GitTopologyProjectionError::Corrupt(error.to_string()))?;
    metadata
        .repository
        .validate()
        .map_err(|error| GitTopologyProjectionError::Corrupt(error.to_string()))?;
    metadata
        .ref_watermark
        .validate()
        .map_err(|error| GitTopologyProjectionError::Corrupt(error.to_string()))?;
    metadata
        .coverage
        .validate()
        .map_err(|error| GitTopologyProjectionError::Corrupt(error.to_string()))?;
    validate_declared_topology(
        &metadata.repository,
        &metadata.branch_stacks,
        &metadata.worktree_occupancies,
    )
    .map_err(|error| GitTopologyProjectionError::Corrupt(error.to_string()))?;
    Ok(metadata)
}

fn commit_entity(
    oid: GitOidV1,
    commit: Option<&GitCommitMetadataV1>,
) -> Result<GraphEntity, GitTopologyProjectionError> {
    let mut labels = BTreeSet::from([GraphLabel::new(COMMIT_LABEL)?]);
    let mut properties = BTreeMap::from([(
        GraphPropertyName::new(COMMIT_OID_PROPERTY)?,
        GraphProperty::String(oid.as_str().to_owned()),
    )]);
    if let Some(commit) = commit {
        properties.insert(
            GraphPropertyName::new(COMMIT_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(commit)?),
        );
    } else {
        labels.insert(GraphLabel::new(BOUNDARY_COMMIT_LABEL)?);
    }
    GraphEntity::new(commit_entity_id(&oid)?, labels, properties).map_err(Into::into)
}

fn ref_entity(reference: &GitTopologyRefV1) -> Result<GraphEntity, GitTopologyProjectionError> {
    GraphEntity::new(
        ref_entity_id(&reference.reference)?,
        BTreeSet::from([GraphLabel::new(REF_LABEL)?]),
        BTreeMap::from([(
            GraphPropertyName::new(REF_RECORD_PROPERTY)?,
            GraphProperty::Bytes(serialize(reference)?),
        )]),
    )
    .map_err(Into::into)
}

fn parent_relation(
    projection: &GraphProjectionIdentity,
    commit: &GitOidV1,
    parent: &GitOidV1,
    ordinal: usize,
) -> Result<GraphGenerationRelation, GitTopologyProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "parent",
            &format!("{}\0{}\0{ordinal}", commit.as_str(), parent.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), commit_entity_id(commit)?),
        GraphEntityRef::new(projection.clone(), commit_entity_id(parent)?),
        GraphRelationKind::new(PARENT_RELATION)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn ref_target_relation(
    projection: &GraphProjectionIdentity,
    reference: &GitTopologyRefV1,
    target: &GitOidV1,
) -> Result<GraphGenerationRelation, GitTopologyProjectionError> {
    GraphGenerationRelation::new(
        GraphRelationId::new(stable_identity(
            "ref-target",
            &format!("{}\0{}", reference.reference.as_str(), target.as_str()),
        ))?,
        GraphEntityRef::new(projection.clone(), ref_entity_id(&reference.reference)?),
        GraphEntityRef::new(projection.clone(), commit_entity_id(target)?),
        GraphRelationKind::new(REF_TARGET_RELATION)?,
        BTreeMap::new(),
    )
    .map_err(Into::into)
}

fn commit_entity_id(oid: &GitOidV1) -> Result<GraphEntityId, GitTopologyProjectionError> {
    GraphEntityId::new(stable_identity("commit", oid.as_str())).map_err(Into::into)
}

fn ref_entity_id(reference: &RefId) -> Result<GraphEntityId, GitTopologyProjectionError> {
    GraphEntityId::new(stable_identity("ref", reference.as_str())).map_err(Into::into)
}

fn metadata_entity_id() -> Result<GraphEntityId, GitTopologyProjectionError> {
    GraphEntityId::new(stable_identity("metadata", GIT_PROJECTION)).map_err(Into::into)
}

fn stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}

fn serialize(value: &impl Serialize) -> Result<Vec<u8>, GitTopologyProjectionError> {
    serde_json::to_vec(value)
        .map_err(|error| GitTopologyProjectionError::Contract(error.to_string()))
}
