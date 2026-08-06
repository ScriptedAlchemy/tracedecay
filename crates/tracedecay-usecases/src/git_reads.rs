//! Scope-bound application authority for query read-only Git intelligence.
//!
//! The transport supplies an already admitted project/repository/worktree
//! scope. This owner refuses scope drift before opening the existing typed
//! [`crate::git_query::GitQueryEngine`]. Missing authority and read failures
//! remain explicit typed unavailable outcomes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_application::{AuthorizedScopeSet, ResolvedScope};
use tracedecay_domain::git::{GitBlameV1, GitDiffScopeV1, GitDiffV1, GitHistoryV1, HunkRefV1};
use tracedecay_domain::{
    GitIndexPreviewId, ManifestDigest, RootScopeOutcomeV1, ScopeOutcome, ScopePartialReasonV1,
    ScopeSetId, ScopeSetRevision, ScopeUnavailableReasonV1, UtcMicros,
};

use tracedecay_application::git::{GitBlameRequest, GitHistoryRequest};
// SEAM: the native `git` spawn adapter is still root-owned
// (`src/git_intelligence.rs`). See `SEAMS.md`.
use crate::git_intelligence::NativeGitIntelligence;
use crate::git_query::{
    GitQueryBounds, GitQueryEngine, GitQueryEnvelopeV1, GitQueryError, GitStatusSummaryV1,
};
// The historical outcome projection moved down beside the adapter that
// produces it; both this owner and the extracted search evaluator mount the
// same values.
use tracedecay_application::historical_query::{
    HistoricalGitQueryAdapter, HistoricalQueryRequestV1, HistoricalSourceAuthorizationV1,
};
pub use tracedecay_application::historical_query::{
    HistoricalGitReadOutcomeV1, HistoricalGitReadUnavailableReasonV1,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum GitReadRequestV1 {
    Status,
    Diff {
        scope: GitDiffScopeV1,
    },
    History {
        max_count: u32,
        path: Option<String>,
        follow: bool,
        first_parent: bool,
    },
    Blame {
        path: String,
        follow_renames: bool,
    },
    Hunks {
        scope: GitDiffScopeV1,
        #[serde(skip)]
        daemon_binding: Option<DaemonGitHunkPreviewBindingV1>,
    },
}

/// Daemon-private binding injected only after exact native snapshot capture.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonGitHunkPreviewBindingV1 {
    pub preview_id: GitIndexPreviewId,
    pub snapshot_digest: ManifestDigest,
    pub expires_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHunkPreviewEntryV1 {
    pub digest: ManifestDigest,
    pub hunk: HunkRefV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHunkPreviewInputV1 {
    pub preview_input_id: GitIndexPreviewId,
    pub repository_snapshot_digest: ManifestDigest,
    pub expires_at: UtcMicros,
    pub hunks: Vec<GitHunkPreviewEntryV1>,
}

impl GitReadRequestV1 {
    pub fn capability_id(&self) -> &'static str {
        match self {
            Self::Status => "capability.application.git.status",
            Self::Diff { .. } => "capability.application.git.diff",
            Self::History { .. } => "capability.application.git.history",
            Self::Blame { .. } => "capability.application.git.blame",
            Self::Hunks { .. } => "capability.application.git.hunks",
        }
    }

    pub fn use_case_id(&self) -> &'static str {
        match self {
            Self::Status => "use-case.application.git.status",
            Self::Diff { .. } => "use-case.application.git.diff",
            Self::History { .. } => "use-case.application.git.history",
            Self::Blame { .. } => "use-case.application.git.blame",
            Self::Hunks { .. } => "use-case.application.git.hunks",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "query", content = "result", rename_all = "snake_case")]
pub enum GitReadResultV1 {
    Status(GitQueryEnvelopeV1<GitStatusSummaryV1>),
    Diff(GitQueryEnvelopeV1<GitDiffV1>),
    History(GitQueryEnvelopeV1<GitHistoryV1>),
    Blame(GitQueryEnvelopeV1<GitBlameV1>),
    Hunks(GitQueryEnvelopeV1<GitHunkPreviewInputV1>),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitReadUnavailableReasonV1 {
    AuthorityAbsent,
    ScopeMismatch,
    Cancelled,
    TimedOut,
    ReadFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
// Boxing Complete would ripple through admission/match sites; size gap is accepted.
#[allow(clippy::large_enum_variant)]
pub enum GitReadOutcomeV1 {
    Complete {
        scope: ResolvedScope,
        result: GitReadResultV1,
    },
    Unavailable {
        reason: GitReadUnavailableReasonV1,
    },
}

/// Exact-root Git read adapter used by the federated authority.
pub trait GitRootReadPort {
    fn scope(&self) -> &ResolvedScope;

    fn read(&self, request: &GitReadRequestV1, bounds: &GitQueryBounds) -> GitReadOutcomeV1;
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MultiRootGitMountError {
    #[error("multi-root Git mount contains an unknown or duplicate root")]
    RootSetMismatch,
    #[error("multi-root Git scope set is invalid: {0}")]
    InvalidScopeSet(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MultiRootGitReadOutcomeV1 {
    pub scope_set_id: ScopeSetId,
    pub scope_set_revision: ScopeSetRevision,
    pub scope_set_digest: ManifestDigest,
    pub roots: Vec<RootScopeOutcomeV1<GitReadResultV1>>,
    pub aggregate: ScopeOutcome<Vec<GitReadResultV1>>,
}

/// Production-capable Git federation over an immutable authorized scope set.
/// Missing root adapters remain typed unavailable instead of disappearing.
pub struct MultiRootGitReadAuthorityV1<P> {
    scope_set: AuthorizedScopeSet,
    roots: BTreeMap<ManifestDigest, P>,
}

impl<P> MultiRootGitReadAuthorityV1<P>
where
    P: GitRootReadPort,
{
    pub fn new(
        scope_set: AuthorizedScopeSet,
        roots: Vec<P>,
    ) -> Result<Self, MultiRootGitMountError> {
        scope_set
            .validate()
            .map_err(|error| MultiRootGitMountError::InvalidScopeSet(error.to_string()))?;
        let mut mounted = BTreeMap::new();
        for root in roots {
            let scope = root.scope();
            if !scope_set
                .roots()
                .iter()
                .any(|candidate| candidate.scope() == scope)
                || mounted.insert(scope.scope_digest.clone(), root).is_some()
            {
                return Err(MultiRootGitMountError::RootSetMismatch);
            }
        }
        Ok(Self {
            scope_set,
            roots: mounted,
        })
    }

    pub fn read(
        &self,
        request: &GitReadRequestV1,
        bounds: &GitQueryBounds,
    ) -> MultiRootGitReadOutcomeV1 {
        let mut roots = Vec::with_capacity(self.scope_set.roots().len());
        let mut values = Vec::new();
        let mut unavailable = false;
        for authorized_root in self.scope_set.roots() {
            let scope = authorized_root.scope();
            let outcome = self.roots.get(&scope.scope_digest).map_or(
                ScopeOutcome::Unavailable {
                    reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
                },
                |root| match root.read(request, bounds) {
                    GitReadOutcomeV1::Complete {
                        scope: returned_scope,
                        result,
                    } if returned_scope == *scope => {
                        values.push(result.clone());
                        ScopeOutcome::Exact(result)
                    }
                    GitReadOutcomeV1::Complete { .. } => {
                        unavailable = true;
                        ScopeOutcome::Unavailable {
                            reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
                        }
                    }
                    GitReadOutcomeV1::Unavailable { reason } => {
                        unavailable = true;
                        ScopeOutcome::Unavailable {
                            reason: git_unavailable_reason(reason),
                        }
                    }
                },
            );
            if matches!(outcome, ScopeOutcome::Unavailable { .. }) {
                unavailable = true;
            }
            roots.push(RootScopeOutcomeV1 {
                scope_digest: scope.scope_digest.clone(),
                outcome,
            });
        }
        let aggregate = if unavailable && !values.is_empty() {
            ScopeOutcome::Partial {
                value: values,
                reason: ScopePartialReasonV1::RootUnavailable,
            }
        } else if unavailable {
            ScopeOutcome::Unavailable {
                reason: ScopeUnavailableReasonV1::AuthorityUnavailable,
            }
        } else {
            ScopeOutcome::Exact(values)
        };
        MultiRootGitReadOutcomeV1 {
            scope_set_id: self.scope_set.scope_set_id().clone(),
            scope_set_revision: self.scope_set.revision(),
            scope_set_digest: self.scope_set.digest().clone(),
            roots,
            aggregate,
        }
    }
}

fn git_unavailable_reason(reason: GitReadUnavailableReasonV1) -> ScopeUnavailableReasonV1 {
    match reason {
        GitReadUnavailableReasonV1::AuthorityAbsent | GitReadUnavailableReasonV1::ScopeMismatch => {
            ScopeUnavailableReasonV1::AuthorityUnavailable
        }
        GitReadUnavailableReasonV1::Cancelled
        | GitReadUnavailableReasonV1::TimedOut
        | GitReadUnavailableReasonV1::ReadFailed => ScopeUnavailableReasonV1::StoreUnavailable,
    }
}

/// One production-mounted read authority for an exact admitted checkout.
pub struct GitReadAuthorityV1 {
    project_root: PathBuf,
    scope: ResolvedScope,
}

impl GitReadAuthorityV1 {
    pub fn new(project_root: impl Into<PathBuf>, scope: ResolvedScope) -> Self {
        Self {
            project_root: project_root.into(),
            scope,
        }
    }

    pub fn read(
        &self,
        selected_scope: &ResolvedScope,
        request: &GitReadRequestV1,
        bounds: &GitQueryBounds,
    ) -> GitReadOutcomeV1 {
        if selected_scope != &self.scope {
            return GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::ScopeMismatch,
            };
        }

        let adapter = NativeGitIntelligence::new(
            self.project_root.clone(),
            self.scope.repository_id.clone(),
            self.scope.worktree_id.clone(),
        );
        let engine = GitQueryEngine::new(&adapter);
        let result = match request {
            GitReadRequestV1::Status => engine.status_summary(bounds).map(GitReadResultV1::Status),
            GitReadRequestV1::Diff { scope } => {
                engine.scoped_diff(bounds, scope).map(GitReadResultV1::Diff)
            }
            GitReadRequestV1::History {
                max_count,
                path,
                follow,
                first_parent,
            } => engine
                .bounded_history(
                    bounds,
                    &GitHistoryRequest {
                        max_count: *max_count,
                        path: path.clone(),
                        follow: *follow,
                        first_parent: *first_parent,
                    },
                )
                .map(GitReadResultV1::History),
            GitReadRequestV1::Blame {
                path,
                follow_renames,
            } => engine
                .path_blame(
                    bounds,
                    &GitBlameRequest {
                        path: path.clone(),
                        follow_renames: *follow_renames,
                    },
                )
                .map(GitReadResultV1::Blame),
            GitReadRequestV1::Hunks {
                scope,
                daemon_binding,
            } => daemon_binding.as_ref().map_or_else(
                || Err(GitQueryError::DaemonPreviewBindingAbsent),
                |binding| {
                    engine
                        .hunk_refs(
                            bounds,
                            scope,
                            binding.preview_id.as_str(),
                            &binding.snapshot_digest,
                        )
                        .and_then(|envelope| {
                            let hunks = envelope
                                .value
                                .into_iter()
                                .map(|hunk| {
                                    let digest = hunk.compute_digest().map_err(|error| {
                                        GitQueryError::Serialization(error.to_string())
                                    })?;
                                    Ok(GitHunkPreviewEntryV1 { digest, hunk })
                                })
                                .collect::<Result<Vec<_>, GitQueryError>>()?;
                            Ok(GitReadResultV1::Hunks(GitQueryEnvelopeV1 {
                                value: GitHunkPreviewInputV1 {
                                    preview_input_id: binding.preview_id.clone(),
                                    repository_snapshot_digest: binding.snapshot_digest.clone(),
                                    expires_at: binding.expires_at,
                                    hunks,
                                },
                                coverage: envelope.coverage,
                                truncated_by_bound: envelope.truncated_by_bound,
                            }))
                        })
                },
            ),
        };

        match result {
            Ok(result) => GitReadOutcomeV1::Complete {
                scope: self.scope.clone(),
                result,
            },
            Err(GitQueryError::Cancelled) => GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::Cancelled,
            },
            Err(GitQueryError::DeadlineExceeded) => GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::TimedOut,
            },
            Err(_) => GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::ReadFailed,
            },
        }
    }

    /// Mount the historical code-index join on this exact admitted checkout.
    pub fn read_historical(
        &self,
        selected_scope: &ResolvedScope,
        authorization: Option<&HistoricalSourceAuthorizationV1>,
        request: &HistoricalQueryRequestV1,
    ) -> HistoricalGitReadOutcomeV1 {
        if selected_scope != &self.scope {
            return HistoricalGitReadOutcomeV1::Unavailable {
                reason: HistoricalGitReadUnavailableReasonV1::ScopeMismatch,
            };
        }
        let identity_matches = tracedecay_runtime_core::storage::read_repository_identity_marker(&self.project_root)
            .ok()
            .flatten()
            .and_then(|marker| {
                tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
                    &self.project_root,
                    &self.scope.project_id,
                    &marker,
                )
            })
            .is_some_and(|identity| {
                identity.matches_admitted_identity(
                    &self.scope.project_id,
                    &self.scope.repository_id,
                    &self.scope.worktree_id,
                )
            });
        if !identity_matches {
            return HistoricalGitReadOutcomeV1::Unavailable {
                reason: HistoricalGitReadUnavailableReasonV1::ScopeMismatch,
            };
        }
        let adapter = NativeGitIntelligence::new(
            self.project_root.clone(),
            self.scope.repository_id.clone(),
            self.scope.worktree_id.clone(),
        );
        match HistoricalGitQueryAdapter::new(&adapter, self.scope.clone())
            .query(authorization, request)
        {
            Ok(result) => HistoricalGitReadOutcomeV1::Complete {
                scope: self.scope.clone(),
                result,
            },
            Err(error) => HistoricalGitReadOutcomeV1::Unavailable {
                reason: HistoricalGitReadUnavailableReasonV1::from_query_error(&error),
            },
        }
    }
}

impl GitRootReadPort for GitReadAuthorityV1 {
    fn scope(&self) -> &ResolvedScope {
        &self.scope
    }

    fn read(&self, request: &GitReadRequestV1, bounds: &GitQueryBounds) -> GitReadOutcomeV1 {
        GitReadAuthorityV1::read(self, &self.scope, request, bounds)
    }
}

pub fn execute_git_read(
    authority: Option<&GitReadAuthorityV1>,
    selected_scope: &ResolvedScope,
    request: &GitReadRequestV1,
    bounds: &GitQueryBounds,
) -> GitReadOutcomeV1 {
    match authority {
        Some(authority) => authority.read(selected_scope, request, bounds),
        None => GitReadOutcomeV1::Unavailable {
            reason: GitReadUnavailableReasonV1::AuthorityAbsent,
        },
    }
}

pub fn execute_historical_git_read(
    authority: Option<&GitReadAuthorityV1>,
    selected_scope: &ResolvedScope,
    source_authorization: Option<&HistoricalSourceAuthorizationV1>,
    request: &HistoricalQueryRequestV1,
) -> HistoricalGitReadOutcomeV1 {
    match authority {
        Some(authority) => authority.read_historical(selected_scope, source_authorization, request),
        None => HistoricalGitReadOutcomeV1::Unavailable {
            reason: HistoricalGitReadUnavailableReasonV1::AuthorityAbsent,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_application::{
        AuthorizedScopeSetAuthority, CancellationContext, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext, RequestId,
    };
    use tracedecay_domain::{
        ActorId, GitCoverageV1, ProjectId, RepositoryId, ScopeOutcome, ScopeSetId,
        ScopeSetRevision, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    fn scope(suffix: &str) -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new(format!("project.{suffix}")).expect("project"),
            RepositoryId::new(format!("repository.{suffix}")).expect("repository"),
            WorktreeId::new(format!("worktree.{suffix}")).expect("worktree"),
            None,
        )
        .expect("scope")
    }

    fn request_context(scope: ResolvedScope, suffix: &str) -> RequestContext {
        let capability = CapabilityId::new("capability.application.git.status").unwrap();
        let use_case = UseCaseId::new("use-case.application.git.status").unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!("grant.git.{suffix}")).unwrap(),
            1,
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            ActorId::new("actor.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.requester").unwrap(),
            scope,
            grant,
            RequestId::new(format!("request.git.{suffix}")).unwrap(),
            Deadline::new(UtcMicros(900)).unwrap(),
            CancellationContext::active(format!("cancel.git.{suffix}")).unwrap(),
        )
        .unwrap()
    }

    struct FakeGitRoot {
        scope: ResolvedScope,
    }

    impl GitRootReadPort for FakeGitRoot {
        fn scope(&self) -> &ResolvedScope {
            &self.scope
        }

        fn read(&self, _request: &GitReadRequestV1, _bounds: &GitQueryBounds) -> GitReadOutcomeV1 {
            GitReadOutcomeV1::Complete {
                scope: self.scope.clone(),
                result: GitReadResultV1::Hunks(GitQueryEnvelopeV1 {
                    value: GitHunkPreviewInputV1 {
                        preview_input_id: GitIndexPreviewId::new("preview.test").unwrap(),
                        repository_snapshot_digest: ManifestDigest::new(format!(
                            "sha256:{}",
                            "a".repeat(64)
                        ))
                        .unwrap(),
                        expires_at: UtcMicros(10),
                        hunks: Vec::new(),
                    },
                    coverage: GitCoverageV1::complete(),
                    truncated_by_bound: false,
                }),
            }
        }
    }

    #[test]
    fn absent_authority_is_typed_unavailable() {
        assert_eq!(
            execute_git_read(
                None,
                &scope("selected"),
                &GitReadRequestV1::Status,
                &GitQueryBounds::default(),
            ),
            GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::AuthorityAbsent,
            }
        );
    }

    #[test]
    fn hunk_request_wire_never_accepts_or_exposes_daemon_preview_binding() {
        let request = GitReadRequestV1::Hunks {
            scope: tracedecay_domain::GitDiffScopeV1::Staged,
            daemon_binding: Some(DaemonGitHunkPreviewBindingV1 {
                preview_id: GitIndexPreviewId::new("preview.private").unwrap(),
                snapshot_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                expires_at: UtcMicros(30),
            }),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"query": "hunks", "scope": {"scope": "staged"}})
        );
        let decoded: GitReadRequestV1 = serde_json::from_value(
            serde_json::json!({"query": "hunks", "scope": {"scope": "staged"}}),
        )
        .unwrap();
        assert!(matches!(
            decoded,
            GitReadRequestV1::Hunks {
                daemon_binding: None,
                ..
            }
        ));
    }

    #[test]
    fn authority_refuses_a_different_project_worktree_scope_before_reading() {
        let root = TempDir::new().expect("tempdir");
        let authority = GitReadAuthorityV1::new(root.path(), scope("mounted"));

        assert_eq!(
            execute_git_read(
                Some(&authority),
                &scope("other"),
                &GitReadRequestV1::Status,
                &GitQueryBounds::default(),
            ),
            GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::ScopeMismatch,
            }
        );
    }

    #[test]
    fn cancellation_and_deadline_are_distinct_typed_unavailable_outcomes() {
        let root = TempDir::new().expect("tempdir");
        let scope = scope("mounted");
        let authority = GitReadAuthorityV1::new(root.path(), scope.clone());
        let cancelled = GitQueryBounds {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..GitQueryBounds::default()
        };
        let timed_out = GitQueryBounds {
            deadline: Some(
                Instant::now()
                    .checked_sub(Duration::from_millis(1))
                    .unwrap(),
            ),
            ..GitQueryBounds::default()
        };

        assert_eq!(
            authority.read(&scope, &GitReadRequestV1::Status, &cancelled),
            GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::Cancelled,
            }
        );
        assert_eq!(
            authority.read(&scope, &GitReadRequestV1::Status, &timed_out),
            GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::TimedOut,
            }
        );
    }

    #[test]
    fn multi_root_git_read_preserves_an_unmounted_root_as_partial() {
        let main = scope("main");
        let linked = scope("linked");
        let contexts = vec![
            request_context(main.clone(), "main"),
            request_context(linked.clone(), "linked"),
        ];
        let scope_set = AuthorizedScopeSetAuthority::authorize(
            ScopeSetId::new("scope-set.git").unwrap(),
            ScopeSetRevision::new(1).unwrap(),
            contexts,
            &CapabilityId::new("capability.application.git.status").unwrap(),
            &UseCaseId::new("use-case.application.git.status").unwrap(),
            UtcMicros(10),
        )
        .unwrap();
        let authority =
            MultiRootGitReadAuthorityV1::new(scope_set, vec![FakeGitRoot { scope: main }]).unwrap();

        let result = authority.read(&GitReadRequestV1::Status, &GitQueryBounds::default());

        assert!(matches!(result.aggregate, ScopeOutcome::Partial { .. }));
        assert_eq!(result.roots.len(), 2);
        assert_eq!(
            result
                .roots
                .iter()
                .filter(|root| matches!(&root.outcome, ScopeOutcome::Unavailable { .. }))
                .count(),
            1
        );
    }
}
