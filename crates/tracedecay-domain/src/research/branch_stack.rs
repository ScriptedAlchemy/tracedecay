//! Canonical, provider-independent branch-stack identity and topology.
//!
//! A stack revision binds repository/ref/tip/worktree proofs from one frozen
//! worktree inventory. It contains no filesystem paths and does not infer
//! edges from branch names, remotes, pull requests, or provider ordering.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use super::{
    BranchStackId, BranchStackRevisionId, CommitId, DomainError, ManifestDigest, ProjectId, RefId,
    RepositoryId, StackNodeId, WorktreeId, WorktreeInventorySnapshotId, canonical_sha256,
};

const BRANCH_STACK_REVISION_DIGEST_DOMAIN_V1: &str = "tracedecay.branch-stack.revision.v1";

/// Monotonic epoch of the worktree inventory frozen into a stack revision.
#[derive(Clone, Copy, Debug, JsonSchema, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorktreeInventoryEpoch(u64);

impl WorktreeInventoryEpoch {
    pub fn new(value: u64) -> Result<Self, DomainError> {
        if value == 0 {
            return Err(DomainError::NonCanonical {
                field: "worktree inventory epoch",
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn validate(self) -> Result<(), DomainError> {
        Self::new(self.0).map(|_| ())
    }
}

impl<'de> Deserialize<'de> for WorktreeInventoryEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Authoritative source that declared the exact stack topology.
#[derive(
    Clone, Copy, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum BranchStackSourceV1 {
    ExplicitDeclaration,
    AcceptedTaskBranchTopology,
}

/// One visible branch/ref at one immutable tip.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchStackNodeV1 {
    pub node_id: StackNodeId,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub reference: RefId,
    pub tip: CommitId,
    pub worktree_id: Option<WorktreeId>,
}

impl BranchStackNodeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.node_id.validate()?;
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.reference.validate()?;
        self.tip.validate()?;
        self.worktree_id
            .as_ref()
            .map_or(Ok(()), WorktreeId::validate)
    }
}

/// A declared dependency edge; propagation direction remains an application
/// decision and is never inferred from this edge.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct BranchStackEdgeV1 {
    pub dependency: StackNodeId,
    pub dependent: StackNodeId,
}

impl BranchStackEdgeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.dependency.validate()?;
        self.dependent.validate()?;
        if self.dependency == self.dependent {
            return Err(DomainError::NonCanonical {
                field: "branch stack self edge",
            });
        }
        Ok(())
    }
}

/// Immutable branch-stack projection at one exact inventory revision.
#[derive(Clone, Debug, JsonSchema, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchStackRevisionV1 {
    pub stack_id: BranchStackId,
    pub revision_id: BranchStackRevisionId,
    pub inventory_snapshot_id: WorktreeInventorySnapshotId,
    pub inventory_epoch: WorktreeInventoryEpoch,
    pub source: BranchStackSourceV1,
    pub nodes: Vec<BranchStackNodeV1>,
    pub edges: Vec<BranchStackEdgeV1>,
    canonical_order: Vec<StackNodeId>,
    pub digest: ManifestDigest,
}

impl BranchStackRevisionV1 {
    pub fn new(
        stack_id: BranchStackId,
        revision_id: BranchStackRevisionId,
        inventory_snapshot_id: WorktreeInventorySnapshotId,
        inventory_epoch: WorktreeInventoryEpoch,
        source: BranchStackSourceV1,
        mut nodes: Vec<BranchStackNodeV1>,
        mut edges: Vec<BranchStackEdgeV1>,
    ) -> Result<Self, DomainError> {
        nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        edges.sort();
        let canonical_order = validate_topology(&nodes, &edges)?;
        let digest = canonical_sha256(&(
            BRANCH_STACK_REVISION_DIGEST_DOMAIN_V1,
            &stack_id,
            &revision_id,
            &inventory_snapshot_id,
            inventory_epoch,
            source,
            &nodes,
            &edges,
            &canonical_order,
        ))?;
        let revision = Self {
            stack_id,
            revision_id,
            inventory_snapshot_id,
            inventory_epoch,
            source,
            nodes,
            edges,
            canonical_order,
            digest,
        };
        revision.validate()?;
        Ok(revision)
    }

    pub fn canonical_order(&self) -> &[StackNodeId] {
        &self.canonical_order
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            BRANCH_STACK_REVISION_DIGEST_DOMAIN_V1,
            &self.stack_id,
            &self.revision_id,
            &self.inventory_snapshot_id,
            self.inventory_epoch,
            self.source,
            &self.nodes,
            &self.edges,
            &self.canonical_order,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.stack_id.validate()?;
        self.revision_id.validate()?;
        self.inventory_snapshot_id.validate()?;
        self.inventory_epoch.validate()?;
        self.digest.validate()?;
        if self
            .nodes
            .windows(2)
            .any(|nodes| nodes[0].node_id >= nodes[1].node_id)
        {
            return Err(DomainError::NonCanonical {
                field: "branch stack node order",
            });
        }
        if self.edges.windows(2).any(|edges| edges[0] >= edges[1]) {
            return Err(DomainError::NonCanonical {
                field: "branch stack edge order",
            });
        }
        let canonical_order = validate_topology(&self.nodes, &self.edges)?;
        if self.canonical_order != canonical_order {
            return Err(DomainError::NonCanonical {
                field: "branch stack canonical order",
            });
        }
        if self.compute_digest()? != self.digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

fn validate_topology(
    nodes: &[BranchStackNodeV1],
    edges: &[BranchStackEdgeV1],
) -> Result<Vec<StackNodeId>, DomainError> {
    let Some(first) = nodes.first() else {
        return Err(DomainError::Empty {
            field: "branch stack nodes",
        });
    };
    let mut node_ids = BTreeSet::new();
    let mut references = BTreeSet::new();
    let mut worktrees = BTreeSet::new();
    for node in nodes {
        node.validate()?;
        if node.project_id != first.project_id {
            return Err(DomainError::SnapshotMismatch {
                field: "branch stack node project",
            });
        }
        if node.repository_id != first.repository_id {
            return Err(DomainError::SnapshotMismatch {
                field: "branch stack node repository",
            });
        }
        if !node_ids.insert(node.node_id.clone()) {
            return Err(DomainError::DuplicateId {
                field: "branch stack node",
            });
        }
        if !references.insert(node.reference.clone()) {
            return Err(DomainError::DuplicateId {
                field: "branch stack node reference",
            });
        }
        if node
            .worktree_id
            .as_ref()
            .is_some_and(|worktree| !worktrees.insert(worktree.clone()))
        {
            return Err(DomainError::DuplicateId {
                field: "branch stack node worktree",
            });
        }
    }

    let mut indegree = node_ids
        .iter()
        .cloned()
        .map(|node_id| (node_id, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = node_ids
        .iter()
        .cloned()
        .map(|node_id| (node_id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut unique_edges = BTreeSet::new();
    for edge in edges {
        edge.validate()?;
        if !node_ids.contains(&edge.dependency) || !node_ids.contains(&edge.dependent) {
            return Err(DomainError::UnknownReference {
                field: "branch stack edge node",
            });
        }
        if !unique_edges.insert(edge.clone()) {
            return Err(DomainError::DuplicateId {
                field: "branch stack edge",
            });
        }
        dependents
            .get_mut(&edge.dependency)
            .expect("validated dependency node")
            .insert(edge.dependent.clone());
        *indegree
            .get_mut(&edge.dependent)
            .expect("validated dependent node") += 1;
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(node_id, degree)| (*degree == 0).then_some(node_id.clone()))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(node_id) = ready.pop_first() {
        order.push(node_id.clone());
        for dependent in &dependents[&node_id] {
            let degree = indegree
                .get_mut(dependent)
                .expect("validated dependent node");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(dependent.clone());
            }
        }
    }
    if order.len() != nodes.len() {
        return Err(DomainError::NonCanonical {
            field: "branch stack cycle",
        });
    }
    Ok(order)
}
