//! Named multi-root collection resolution for read surfaces.
//!
//! A named collection is a persisted [`AuthorizedScopeSet`]: its revision is
//! frozen at compare-and-swap time and its members are canonically ordered by
//! the scope-set authority. This module owns only target selection precedence
//! and read mapping — it never resolves paths, reads storage, or widens
//! authority.

use tracedecay_domain::ScopeSetId;

use super::AuthorizedScopeSet;

/// Selects the collection a read surface must resolve.
///
/// The default collection never outranks an explicit target: when both are
/// present the explicit target wins unconditionally, and the default is only
/// consulted when the caller named nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiRootCollectionSelectorV1 {
    explicit_target: Option<ScopeSetId>,
    default_collection: Option<ScopeSetId>,
}

impl MultiRootCollectionSelectorV1 {
    pub fn new(
        explicit_target: Option<ScopeSetId>,
        default_collection: Option<ScopeSetId>,
    ) -> Self {
        Self {
            explicit_target,
            default_collection,
        }
    }

    pub fn target(&self) -> Option<&ScopeSetId> {
        self.explicit_target
            .as_ref()
            .or(self.default_collection.as_ref())
    }
}

/// Typed unavailable states for named-collection resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiRootCollectionUnavailableV1 {
    /// The read surface has no daemon application transport at all.
    TransportNotAdmitted,
    /// No explicit target was named and no default collection is configured.
    NoCollectionNamed,
    /// The named collection has no persisted scope set for this project.
    CollectionNotPersisted { scope_set_id: ScopeSetId },
    /// The transport answered, but not with a usable persisted scope set.
    AuthorityUnavailable { detail: String },
}

impl MultiRootCollectionUnavailableV1 {
    /// Human-readable reason carried on the wire capability projection.
    pub fn reason(&self) -> String {
        match self {
            Self::TransportNotAdmitted => {
                "the daemon application transport is not admitted for this dashboard".to_owned()
            }
            Self::NoCollectionNamed => {
                "no default multi-root collection is configured; name an explicit collection"
                    .to_owned()
            }
            Self::CollectionNotPersisted { scope_set_id } => format!(
                "multi-root collection {} names no persisted scope set for this project",
                scope_set_id.as_str()
            ),
            Self::AuthorityUnavailable { detail } => {
                format!("the multi-root collection authority is unavailable: {detail}")
            }
        }
    }
}

/// Resolution of one named multi-root collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MultiRootCollectionResolutionV1 {
    /// The frozen scope-set revision with canonical member order.
    Mounted { scope_set: AuthorizedScopeSet },
    Unavailable {
        reason: MultiRootCollectionUnavailableV1,
    },
}

impl MultiRootCollectionResolutionV1 {
    /// Maps one persisted scope-set read for `target`.
    ///
    /// A mounted answer is revalidated so it always carries the frozen
    /// revision, canonical member order, and matching digest; a persisted row
    /// answering for a different collection id is an authority fault, not a
    /// silent alias.
    pub fn from_persisted_read(target: &ScopeSetId, read: Option<AuthorizedScopeSet>) -> Self {
        let Some(scope_set) = read else {
            return Self::Unavailable {
                reason: MultiRootCollectionUnavailableV1::CollectionNotPersisted {
                    scope_set_id: target.clone(),
                },
            };
        };
        if scope_set.scope_set_id() != target {
            return Self::Unavailable {
                reason: MultiRootCollectionUnavailableV1::AuthorityUnavailable {
                    detail: format!(
                        "persisted scope set {} does not answer for collection {}",
                        scope_set.scope_set_id().as_str(),
                        target.as_str()
                    ),
                },
            };
        }
        if let Err(error) = scope_set.validate() {
            return Self::Unavailable {
                reason: MultiRootCollectionUnavailableV1::AuthorityUnavailable {
                    detail: error.to_string(),
                },
            };
        }
        Self::Mounted { scope_set }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, ScopeSetId, ScopeSetRevision,
        UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::{
        MultiRootCollectionResolutionV1, MultiRootCollectionSelectorV1,
        MultiRootCollectionUnavailableV1,
    };
    use crate::multi_root::{AuthorizedScopeSet, AuthorizedScopeSetAuthority};
    use crate::{
        CancellationContext, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
        RequestId, ResolvedScope,
    };

    const CAPABILITY: &str = "capability.multi-root.query";
    const USE_CASE: &str = "use-case.multi-root.query";

    fn collection(name: &str) -> ScopeSetId {
        ScopeSetId::new(name).expect("collection id")
    }

    fn persisted_scope_set(name: &str) -> AuthorizedScopeSet {
        let scope = ResolvedScope::new(
            ProjectId::new("project.fixture").expect("project"),
            RepositoryId::new("repository.fixture").expect("repository"),
            WorktreeId::new("worktree.main").expect("worktree"),
            Some(RefId::new("refs/heads/main").expect("reference")),
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            "grant.collection".to_owned().try_into().expect("grant id"),
            1,
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
            ActorId::new("actor.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([CapabilityId::new(CAPABILITY).expect("capability")]),
            BTreeSet::from([UseCaseId::new(USE_CASE).expect("use case")]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.requester").expect("actor"),
            scope,
            grant,
            RequestId::new("request.collection").expect("request id"),
            Deadline::new(UtcMicros(900)).expect("deadline"),
            CancellationContext::active("cancel.collection").expect("cancellation"),
        )
        .expect("context");
        AuthorizedScopeSetAuthority::authorize(
            collection(name),
            ScopeSetRevision::new(1).expect("revision"),
            vec![context],
            &CapabilityId::new(CAPABILITY).expect("capability"),
            &UseCaseId::new(USE_CASE).expect("use case"),
            UtcMicros(10),
        )
        .expect("authorized scope set")
    }

    #[test]
    fn explicit_target_always_outranks_the_default_collection() {
        let selector = MultiRootCollectionSelectorV1::new(
            Some(collection("scope-set.explicit")),
            Some(collection("scope-set.default")),
        );
        assert_eq!(selector.target(), Some(&collection("scope-set.explicit")));
    }

    #[test]
    fn default_collection_answers_only_when_nothing_explicit_is_named() {
        let with_default =
            MultiRootCollectionSelectorV1::new(None, Some(collection("scope-set.default")));
        assert_eq!(
            with_default.target(),
            Some(&collection("scope-set.default"))
        );

        let unnamed = MultiRootCollectionSelectorV1::new(None, None);
        assert_eq!(unnamed.target(), None);
    }

    #[test]
    fn persisted_read_mounts_the_frozen_revision_and_canonical_members() {
        let scope_set = persisted_scope_set("scope-set.collection");
        let resolution = MultiRootCollectionResolutionV1::from_persisted_read(
            &collection("scope-set.collection"),
            Some(scope_set.clone()),
        );
        assert_eq!(
            resolution,
            MultiRootCollectionResolutionV1::Mounted { scope_set }
        );
    }

    #[test]
    fn missing_persisted_scope_set_is_typed_not_persisted() {
        let resolution = MultiRootCollectionResolutionV1::from_persisted_read(
            &collection("scope-set.missing"),
            None,
        );
        assert_eq!(
            resolution,
            MultiRootCollectionResolutionV1::Unavailable {
                reason: MultiRootCollectionUnavailableV1::CollectionNotPersisted {
                    scope_set_id: collection("scope-set.missing"),
                },
            }
        );
    }

    #[test]
    fn a_scope_set_answering_for_another_collection_is_an_authority_fault() {
        let scope_set = persisted_scope_set("scope-set.other");
        let resolution = MultiRootCollectionResolutionV1::from_persisted_read(
            &collection("scope-set.requested"),
            Some(scope_set),
        );
        let MultiRootCollectionResolutionV1::Unavailable {
            reason: MultiRootCollectionUnavailableV1::AuthorityUnavailable { detail },
        } = resolution
        else {
            panic!("mismatched collection identity must not mount");
        };
        assert!(detail.contains("scope-set.other"));
        assert!(detail.contains("scope-set.requested"));
    }
}
