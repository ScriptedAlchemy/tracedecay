//! Scope-bound application authority for PR9 read-only Git intelligence.
//!
//! The transport supplies an already admitted project/repository/worktree
//! scope. This owner refuses scope drift before opening the existing typed
//! [`crate::git_query::GitQueryEngine`]. Missing authority and read failures
//! remain explicit typed unavailable outcomes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::ManifestDigest;
use tracedecay_domain::git::{GitBlameV1, GitDiffScopeV1, GitDiffV1, GitHistoryV1, HunkRefV1};

use crate::code_index::historical_query::{
    HistoricalGitQueryAdapter, HistoricalQueryError, HistoricalQueryRequestV1,
    HistoricalQueryResultV1, HistoricalSourceAuthorizationV1,
};
use crate::git_intelligence::{GitBlameRequest, GitHistoryRequest, NativeGitIntelligence};
use crate::git_query::{
    GitQueryBounds, GitQueryEngine, GitQueryEnvelopeV1, GitQueryError, GitStatusSummaryV1,
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
        preview_id: String,
        snapshot_digest: ManifestDigest,
    },
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
    Hunks(GitQueryEnvelopeV1<Vec<HunkRefV1>>),
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
pub enum GitReadOutcomeV1 {
    Complete {
        scope: ResolvedScope,
        result: GitReadResultV1,
    },
    Unavailable {
        reason: GitReadUnavailableReasonV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoricalGitReadUnavailableReasonV1 {
    AuthorityAbsent,
    ScopeMismatch,
    NotAuthorized,
    ReadFailed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HistoricalGitReadOutcomeV1 {
    Complete {
        scope: ResolvedScope,
        result: HistoricalQueryResultV1,
    },
    Unavailable {
        reason: HistoricalGitReadUnavailableReasonV1,
    },
}

/// One production-mounted read authority for an exact admitted checkout.
pub struct GitReadAuthorityV1 {
    project_root: PathBuf,
    scope: ResolvedScope,
}

impl GitReadAuthorityV1 {
    pub(crate) fn new(project_root: impl Into<PathBuf>, scope: ResolvedScope) -> Self {
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
                preview_id,
                snapshot_digest,
            } => engine
                .hunk_refs(bounds, scope, preview_id, snapshot_digest)
                .map(GitReadResultV1::Hunks),
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
        let identity_matches = crate::storage::read_repository_identity_marker(&self.project_root)
            .ok()
            .flatten()
            .and_then(|marker| {
                crate::repository_provenance::RepositoryProvenanceAdmissionContext::from_authoritative_project_marker(
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
            Err(
                HistoricalQueryError::MissingAuthorization
                | HistoricalQueryError::InvalidAuthorization
                | HistoricalQueryError::UnauthorizedCommit(_)
                | HistoricalQueryError::UnauthorizedPath(_),
            ) => HistoricalGitReadOutcomeV1::Unavailable {
                reason: HistoricalGitReadUnavailableReasonV1::NotAuthorized,
            },
            Err(
                HistoricalQueryError::ScopeMismatch | HistoricalQueryError::ProviderScopeMismatch,
            ) => HistoricalGitReadOutcomeV1::Unavailable {
                reason: HistoricalGitReadUnavailableReasonV1::ScopeMismatch,
            },
            Err(_) => HistoricalGitReadOutcomeV1::Unavailable {
                reason: HistoricalGitReadUnavailableReasonV1::ReadFailed,
            },
        }
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
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_domain::{ProjectId, RepositoryId, WorktreeId};

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
}
