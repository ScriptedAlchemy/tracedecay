//! The canonical code-graph namespace and the legacy layout it replaced.
//!
//! A code graph is projected into the project's graph container under a
//! namespace derived from the code shard alone. Every generation of one shard
//! therefore publishes into the *same* projection, so publishing generation
//! N+1 supersedes N through the ordinary verified-head compare-and-swap and N
//! becomes historical replay the ordinary retirement path reclaims.
//!
//! The retired layout hashed the code generation into the namespace as well,
//! which gave every generation a projection of its own. A generation was then
//! the permanent verified head of that projection: `retire_replay` always
//! answered `CurrentVerifiedHead`, nothing ever superseded anything, and the
//! shared staging container accumulated the rows of every generation ever
//! published (issue #836).
//!
//! The two layouts occupy disjoint namespace prefixes on purpose. The prefix
//! is the migration discriminator: a persisted namespace read back from a
//! store is classified by inspection as canonical or legacy, with no reliance
//! on digest-space accidents and no naming-based compatibility. Nothing
//! derives, reads, or writes a legacy namespace; the classifier exists so the
//! retirement sweep can *recognize* legacy-layout rows it reclaims and report
//! the migration instead of leaving it silent.

use tracedecay_store::StoreShardIdV1;

use crate::{GraphDbError, GraphNamespace};

/// Prefix of the canonical, generation-agnostic code-graph namespace.
pub const CODE_GRAPH_SHARD_NAMESPACE_PREFIX: &str = "code-shard:";

/// Prefix of the retired per-generation code-graph namespace.
///
/// Only persisted state still carries it. It is never produced again.
pub const LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX: &str = "code-scope:";

const CODE_GRAPH_SHARD_NAMESPACE_DOMAIN: &str = "tracedecay.code-graph.shard.v2";

/// The one canonical namespace of a code shard's graph projection.
///
/// Deliberately generation-free: the namespace names *what* is projected (an
/// exact repository/worktree/ref or snapshot scope), never *which* generation
/// of it, so successive generations of one scope compete for a single verified
/// head instead of each owning an immortal projection.
pub fn code_graph_shard_namespace(
    code_shard_id: &StoreShardIdV1,
) -> Result<GraphNamespace, GraphDbError> {
    let digest =
        tracedecay_domain::canonical_sha256(&(CODE_GRAPH_SHARD_NAMESPACE_DOMAIN, code_shard_id))
            .map_err(|error| {
                GraphDbError::invalid(format!("derive canonical code graph namespace: {error}"))
            })?;
    GraphNamespace::new(format!(
        "{CODE_GRAPH_SHARD_NAMESPACE_PREFIX}{}",
        digest.as_str()
    ))
}

/// Whether `namespace` is a canonical per-shard code-graph namespace.
#[must_use]
pub fn is_code_graph_shard_namespace(namespace: &GraphNamespace) -> bool {
    is_code_graph_shard_namespace_str(namespace.as_str())
}

/// [`is_code_graph_shard_namespace`] over a namespace already read back as a
/// string from a persisted relational projection identity.
pub(crate) fn is_code_graph_shard_namespace_str(namespace: &str) -> bool {
    namespace.starts_with(CODE_GRAPH_SHARD_NAMESPACE_PREFIX)
}

/// Whether `namespace` was persisted under the retired per-generation layout.
///
/// True only for rows written before the cutover. The retirement sweep uses it
/// to report that a reclaimed projection was legacy-layout residue rather than
/// an ordinary superseded generation.
#[must_use]
pub fn is_legacy_per_generation_code_graph_namespace(namespace: &GraphNamespace) -> bool {
    is_legacy_per_generation_code_graph_namespace_str(namespace.as_str())
}

/// [`is_legacy_per_generation_code_graph_namespace`] over a namespace already
/// read back as a string from a persisted relational projection identity.
pub(crate) fn is_legacy_per_generation_code_graph_namespace_str(namespace: &str) -> bool {
    namespace.starts_with(LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{BrainId, ProjectId, RepositoryId, UserProfileId, WorktreeId};
    use tracedecay_store::{CodeShardScopeV1, StoreShardScopeV1};

    fn code_shard(worktree: &str) -> StoreShardIdV1 {
        StoreShardIdV1::new(
            BrainId::new("brain.namespace").unwrap(),
            UserProfileId::new("profile.namespace").unwrap(),
            StoreShardScopeV1::Code {
                project_id: ProjectId::new("project.namespace").unwrap(),
                repository_id: RepositoryId::new("repository.namespace").unwrap(),
                scope: CodeShardScopeV1::Worktree {
                    worktree_id: WorktreeId::new(worktree).unwrap(),
                },
            },
        )
    }

    #[test]
    fn one_shard_keeps_one_namespace_across_generations() {
        let shard = code_shard("worktree.primary");
        assert_eq!(
            code_graph_shard_namespace(&shard).unwrap(),
            code_graph_shard_namespace(&shard).unwrap(),
        );
    }

    #[test]
    fn distinct_shards_keep_distinct_namespaces() {
        assert_ne!(
            code_graph_shard_namespace(&code_shard("worktree.primary")).unwrap(),
            code_graph_shard_namespace(&code_shard("worktree.linked")).unwrap(),
        );
    }

    #[test]
    fn canonical_and_legacy_layouts_are_disjoint_by_prefix() {
        let canonical = code_graph_shard_namespace(&code_shard("worktree.primary")).unwrap();
        assert!(is_code_graph_shard_namespace(&canonical));
        assert!(!is_legacy_per_generation_code_graph_namespace(&canonical));

        let legacy = GraphNamespace::new(format!(
            "{LEGACY_PER_GENERATION_CODE_GRAPH_NAMESPACE_PREFIX}{}",
            "a".repeat(64)
        ))
        .unwrap();
        assert!(is_legacy_per_generation_code_graph_namespace(&legacy));
        assert!(!is_code_graph_shard_namespace(&legacy));
    }
}
