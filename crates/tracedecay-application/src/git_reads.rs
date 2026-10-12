//! Scope-bound application authority for query read-only Git intelligence.
//!
//! The transport supplies an already admitted project/repository/worktree
//! scope. This owner refuses scope drift before opening the existing typed
//! [`crate::git_query::GitQueryEngine`]. Missing authority and read failures
//! remain explicit typed unavailable outcomes.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracedecay_contracts::ResolvedScope;

use crate::git_intelligence::NativeGitIntelligence;
use crate::git_query::{GitQueryBounds, GitQueryEngine, GitQueryEnvelopeV1, GitQueryError};
use tracedecay_contracts::git::{
    GitBlameRequest, GitHistoryRequest, GitHunkPreviewEntryV1, GitHunkPreviewInputV1,
    GitIntelligenceError, GitReadRequestV1,
};
pub use tracedecay_contracts::historical_query::{
    HistoricalGitReadOutcomeV1, HistoricalGitReadUnavailableReasonV1,
};

// The typed read payload enum is a canonical public wire contract owned by
// the application crate (`git::public_wire`), shared with SDK schema
// generation.
pub use tracedecay_contracts::git::GitReadResultV1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitReadUnavailableReasonV1 {
    AuthorityAbsent,
    ScopeMismatch,
    Cancelled,
    TimedOut,
    OutputLimitExceeded,
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
        )
        .with_execution_bounds(
            bounds.deadline,
            bounds.cancel.clone(),
            usize::try_from(bounds.max_bytes).unwrap_or(usize::MAX),
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
            Err(GitQueryError::Adapter(GitIntelligenceError::Cancelled)) => {
                GitReadOutcomeV1::Unavailable {
                    reason: GitReadUnavailableReasonV1::Cancelled,
                }
            }
            Err(GitQueryError::Adapter(GitIntelligenceError::DeadlineExceeded)) => {
                GitReadOutcomeV1::Unavailable {
                    reason: GitReadUnavailableReasonV1::TimedOut,
                }
            }
            Err(GitQueryError::Adapter(GitIntelligenceError::OutputLimitExceeded { .. }))
            | Err(GitQueryError::ByteBoundExceeded { .. }) => GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::OutputLimitExceeded,
            },
            Err(_) => GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::ReadFailed,
            },
        }
    }
}

#[tracing::instrument(name = "usecases.git.execute_read", level = "trace", skip_all)]
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_contracts::git::DaemonGitHunkPreviewBindingV1;
    use tracedecay_domain::{
        GitIndexPreviewId, ManifestDigest, ProjectId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_runtime_core::cancellation::CancellationToken;

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
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cancelled = GitQueryBounds {
            cancel: Some(cancellation),
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
    fn subprocess_output_bound_is_a_distinct_typed_unavailable_outcome() {
        let root = TempDir::new().expect("tempdir");
        let initialized = std::process::Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
        .args(["init", "--quiet"])
        .current_dir(root.path())
        .status()
        .expect("initialize git fixture");
        assert!(initialized.success());
        let scope = scope("output-bound");
        let authority = GitReadAuthorityV1::new(root.path(), scope.clone());
        let bounds = GitQueryBounds {
            max_bytes: 1,
            ..GitQueryBounds::default()
        };

        assert_eq!(
            authority.read(&scope, &GitReadRequestV1::Status, &bounds),
            GitReadOutcomeV1::Unavailable {
                reason: GitReadUnavailableReasonV1::OutputLimitExceeded,
            }
        );
    }
}
