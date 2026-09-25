//! The canonical code-graph namespace.
//!
//! A code graph is projected into the project's graph container under a
//! namespace derived from the code shard alone. Every generation of one shard
//! therefore publishes into the *same* projection, so publishing generation
//! N+1 supersedes N through the ordinary verified-head compare-and-swap and N
//! becomes historical replay the ordinary retirement path reclaims.

use tracedecay_store::StoreShardIdV1;

use crate::{GraphDbError, GraphNamespace};

/// Prefix of the canonical, generation-agnostic code-graph namespace.
pub const CODE_GRAPH_SHARD_NAMESPACE_PREFIX: &str = "code-shard:";

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
    fn canonical_namespace_carries_the_shard_prefix() {
        let canonical = code_graph_shard_namespace(&code_shard("worktree.primary")).unwrap();
        assert!(is_code_graph_shard_namespace(&canonical));
        let foreign = GraphNamespace::new(format!("code-scope:{}", "a".repeat(64))).unwrap();
        assert!(!is_code_graph_shard_namespace(&foreign));
    }
}
