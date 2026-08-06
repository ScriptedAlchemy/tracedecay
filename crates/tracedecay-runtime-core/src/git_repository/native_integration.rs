//! Bounded in-process native integration mechanics.
//!
//! Preflight uses gix object memory so candidate objects never reach the real
//! object database. Apply recreates the exact candidate, verifies its tree,
//! creates only the fixed commit shape, and updates one destination ref with
//! an old/new compare-and-set.

use gix::bstr::ByteSlice as _;
use tracedecay_domain::git::GitOidV1;

use super::{GitRepositoryAuthority, GitRepositoryError, operation};
use crate::cancellation::CancellationToken;

const MAX_INTEGRATION_COMMITS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitNativeIntegrationMode {
    FastForward,
    TwoParentMerge,
    CherryPickExactCommits,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitNativePreflightDisposition {
    Eligible,
    AlreadyIntegrated,
    Conflict,
    Unsupported(GitNativeUnsupportedReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitNativeUnsupportedReason {
    NonFastForward,
    RootCommit,
    MergeCommit,
    SigningRequired,
    HooksConfigured,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitNativePreflight {
    pub disposition: GitNativePreflightDisposition,
    pub mode: GitNativeIntegrationMode,
    pub source_tip: GitOidV1,
    pub destination_tip: GitOidV1,
    pub source_tree: GitOidV1,
    pub destination_tree: GitOidV1,
    pub merge_base: GitOidV1,
    pub ordered_commits: Vec<GitOidV1>,
    pub candidate_tree: Option<GitOidV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitNativeApplyOutcome {
    pub old_tip: GitOidV1,
    pub new_tip: GitOidV1,
    pub final_tree: GitOidV1,
}

impl GitRepositoryAuthority {
    /// Preflight one exact pair without changing refs, index, worktree, or the
    /// real object database.
    pub fn preflight_native_integration(
        &self,
        source_ref: &str,
        destination_ref: &str,
        expected_source_tip: &GitOidV1,
        expected_destination_tip: &GitOidV1,
        mode: GitNativeIntegrationMode,
        cancellation: &CancellationToken,
    ) -> Result<GitNativePreflight, GitRepositoryError> {
        validate_ref_pair(source_ref, destination_ref)?;
        let repository = self.repository.to_thread_local().with_object_memory();
        let source_tip =
            exact_reference_tip(&repository, source_ref, expected_source_tip, "source ref")?;
        let destination_tip = exact_reference_tip(
            &repository,
            destination_ref,
            expected_destination_tip,
            "destination ref",
        )?;
        if cancellation.is_cancelled() {
            return Err(GitRepositoryError::Operation {
                operation: "native integration preflight",
                detail: "cancelled".to_owned(),
            });
        }
        let source_commit = repository
            .find_commit(source_tip)
            .map_err(|error| operation("native integration source commit", error))?;
        let destination_commit = repository
            .find_commit(destination_tip)
            .map_err(|error| operation("native integration destination commit", error))?;
        let source_tree = source_commit
            .tree_id()
            .map_err(|error| operation("native integration source tree", error))?
            .detach();
        let destination_tree = destination_commit
            .tree_id()
            .map_err(|error| operation("native integration destination tree", error))?
            .detach();
        let merge_base = repository
            .merge_base(source_tip, destination_tip)
            .map_err(|error| operation("native integration merge base", error))?
            .detach();
        let ordered_commits =
            linear_commit_closure(&repository, source_tip, merge_base, cancellation)?;
        let already_integrated = merge_base == source_tip;
        let fast_forward = merge_base == destination_tip;
        let unsupported_write_configuration = self.native_write_unsupported_reason();
        let (disposition, candidate_tree) = if let Some(reason) = unsupported_write_configuration {
            (GitNativePreflightDisposition::Unsupported(reason), None)
        } else if already_integrated {
            (GitNativePreflightDisposition::AlreadyIntegrated, None)
        } else {
            match mode {
                GitNativeIntegrationMode::FastForward if fast_forward => {
                    (GitNativePreflightDisposition::Eligible, Some(source_tree))
                }
                GitNativeIntegrationMode::FastForward => (
                    GitNativePreflightDisposition::Unsupported(
                        GitNativeUnsupportedReason::NonFastForward,
                    ),
                    None,
                ),
                GitNativeIntegrationMode::TwoParentMerge => {
                    merge_candidate(&repository, destination_tip, source_tip)?
                }
                GitNativeIntegrationMode::CherryPickExactCommits => cherry_pick_candidate(
                    &repository,
                    destination_tree,
                    &ordered_commits,
                    cancellation,
                )?,
            }
        };
        Ok(GitNativePreflight {
            disposition,
            mode,
            source_tip: oid(source_tip)?,
            destination_tip: oid(destination_tip)?,
            source_tree: oid(source_tree)?,
            destination_tree: oid(destination_tree)?,
            merge_base: oid(merge_base)?,
            ordered_commits: ordered_commits
                .into_iter()
                .map(oid)
                .collect::<Result<_, _>>()?,
            candidate_tree: candidate_tree.map(oid).transpose()?,
        })
    }

    /// Recreate and commit an exact eligible preflight with one destination
    /// ref CAS. Checked-out destination materialization remains ineligible at
    /// the adapter boundary until a native checkout transaction is supplied.
    pub fn apply_native_integration(
        &self,
        source_ref: &str,
        destination_ref: &str,
        expected_source_tip: &GitOidV1,
        expected_destination_tip: &GitOidV1,
        expected_candidate_tree: &GitOidV1,
        mode: GitNativeIntegrationMode,
        cancellation: &CancellationToken,
    ) -> Result<GitNativeApplyOutcome, GitRepositoryError> {
        let verified = self.preflight_native_integration(
            source_ref,
            destination_ref,
            expected_source_tip,
            expected_destination_tip,
            mode,
            cancellation,
        )?;
        if verified.disposition != GitNativePreflightDisposition::Eligible
            || verified.candidate_tree.as_ref() != Some(expected_candidate_tree)
        {
            return Err(GitRepositoryError::Operation {
                operation: "native integration apply",
                detail: "preflight compare-and-set failed".to_owned(),
            });
        }
        if cancellation.is_cancelled() {
            return Err(GitRepositoryError::Operation {
                operation: "native integration apply",
                detail: "cancelled".to_owned(),
            });
        }

        let repository = self.repository.to_thread_local();
        let source_tip = parse_oid(expected_source_tip, "source tip")?;
        let destination_tip = parse_oid(expected_destination_tip, "destination tip")?;
        let candidate_tree = parse_oid(expected_candidate_tree, "candidate tree")?;
        let new_tip = match mode {
            GitNativeIntegrationMode::FastForward => {
                update_ref_cas(&repository, destination_ref, destination_tip, source_tip)?;
                source_tip
            }
            GitNativeIntegrationMode::TwoParentMerge => {
                let mut outcome = repository
                    .merge_commits(
                        destination_tip,
                        source_tip,
                        default_labels(),
                        repository
                            .tree_merge_options()
                            .map_err(|error| operation("native integration merge options", error))?
                            .into(),
                    )
                    .map_err(|error| operation("native integration merge", error))?;
                if outcome
                    .tree_merge
                    .has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::default())
                {
                    return Err(GitRepositoryError::Operation {
                        operation: "native integration merge",
                        detail: "conflict after revalidation".to_owned(),
                    });
                }
                let tree = outcome
                    .tree_merge
                    .tree
                    .write()
                    .map_err(|error| operation("native integration merge tree", error))?
                    .detach();
                if tree != candidate_tree {
                    return Err(GitRepositoryError::Operation {
                        operation: "native integration merge",
                        detail: "candidate tree drift".to_owned(),
                    });
                }
                repository
                    .commit(
                        destination_ref,
                        fixed_merge_message(source_ref, destination_ref),
                        tree,
                        [destination_tip, source_tip],
                    )
                    .map_err(|error| operation("native integration merge commit", error))?
                    .detach()
            }
            GitNativeIntegrationMode::CherryPickExactCommits => {
                let ordered = verified
                    .ordered_commits
                    .iter()
                    .map(|id| parse_oid(id, "ordered commit"))
                    .collect::<Result<Vec<_>, _>>()?;
                materialize_cherry_pick_chain(
                    &repository,
                    destination_ref,
                    destination_tip,
                    candidate_tree,
                    &ordered,
                    cancellation,
                )?
            }
        };
        let final_tree = repository
            .find_commit(new_tip)
            .map_err(|error| operation("native integration final commit", error))?
            .tree_id()
            .map_err(|error| operation("native integration final tree", error))?
            .detach();
        if final_tree != candidate_tree {
            return Err(GitRepositoryError::Operation {
                operation: "native integration final verification",
                detail: "final tree differs from preview".to_owned(),
            });
        }
        Ok(GitNativeApplyOutcome {
            old_tip: expected_destination_tip.clone(),
            new_tip: oid(new_tip)?,
            final_tree: oid(final_tree)?,
        })
    }

    /// Roll back only the exact candidate ref tip written by this transaction.
    pub fn rollback_native_integration(
        &self,
        destination_ref: &str,
        committed_tip: &GitOidV1,
        expected_old_tip: &GitOidV1,
    ) -> Result<(), GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        update_ref_cas(
            &repository,
            destination_ref,
            parse_oid(committed_tip, "committed tip")?,
            parse_oid(expected_old_tip, "old tip")?,
        )
    }

    pub fn exact_reference_tip(&self, reference: &str) -> Result<GitOidV1, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        let mut reference = repository
            .find_reference(reference)
            .map_err(|error| operation("native integration ref probe", error))?;
        let target = reference
            .peel_to_id()
            .map_err(|error| operation("native integration ref probe", error))?
            .detach();
        oid(target)
    }

    pub fn commit_tree(&self, commit: &GitOidV1) -> Result<GitOidV1, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        let tree = repository
            .find_commit(parse_oid(commit, "native integration commit tree")?)
            .map_err(|error| operation("native integration commit tree", error))?
            .tree_id()
            .map_err(|error| operation("native integration commit tree", error))?
            .detach();
        oid(tree)
    }

    fn native_write_unsupported_reason(&self) -> Option<GitNativeUnsupportedReason> {
        let repository = self.repository.to_thread_local();
        let config = repository.config_snapshot();
        if config.boolean("commit.gpgSign").unwrap_or(false)
            || config.boolean("merge.gpgSign").unwrap_or(false)
        {
            return Some(GitNativeUnsupportedReason::SigningRequired);
        }
        if config.string("core.hooksPath").is_some() {
            return Some(GitNativeUnsupportedReason::HooksConfigured);
        }
        const WRITE_HOOKS: [&str; 6] = [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "post-merge",
            "post-rewrite",
        ];
        WRITE_HOOKS
            .iter()
            .any(|hook| self.common_dir.join("hooks").join(hook).is_file())
            .then_some(GitNativeUnsupportedReason::HooksConfigured)
    }
}

fn merge_candidate(
    repository: &gix::Repository,
    destination_tip: gix::ObjectId,
    source_tip: gix::ObjectId,
) -> Result<(GitNativePreflightDisposition, Option<gix::ObjectId>), GitRepositoryError> {
    let mut outcome = repository
        .merge_commits(
            destination_tip,
            source_tip,
            default_labels(),
            repository
                .tree_merge_options()
                .map_err(|error| operation("native integration merge options", error))?
                .into(),
        )
        .map_err(|error| operation("native integration merge", error))?;
    if outcome
        .tree_merge
        .has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::default())
    {
        return Ok((GitNativePreflightDisposition::Conflict, None));
    }
    let tree = outcome
        .tree_merge
        .tree
        .write()
        .map_err(|error| operation("native integration candidate tree", error))?
        .detach();
    Ok((GitNativePreflightDisposition::Eligible, Some(tree)))
}

fn cherry_pick_candidate(
    repository: &gix::Repository,
    mut current_tree: gix::ObjectId,
    commits: &[gix::ObjectId],
    cancellation: &CancellationToken,
) -> Result<(GitNativePreflightDisposition, Option<gix::ObjectId>), GitRepositoryError> {
    for commit_id in commits {
        if cancellation.is_cancelled() {
            return Err(GitRepositoryError::Operation {
                operation: "native integration cherry-pick preflight",
                detail: "cancelled".to_owned(),
            });
        }
        let commit = repository
            .find_commit(*commit_id)
            .map_err(|error| operation("native integration cherry-pick commit", error))?;
        let decoded = commit
            .decode()
            .map_err(|error| operation("native integration cherry-pick decode", error))?;
        let mut parents = decoded.parents();
        let Some(parent) = parents.next() else {
            return Ok((
                GitNativePreflightDisposition::Unsupported(GitNativeUnsupportedReason::RootCommit),
                None,
            ));
        };
        if parents.next().is_some() {
            return Ok((
                GitNativePreflightDisposition::Unsupported(GitNativeUnsupportedReason::MergeCommit),
                None,
            ));
        }
        let parent_commit = repository
            .find_commit(parent)
            .map_err(|error| operation("native integration cherry-pick parent", error))?;
        let parent_tree = parent_commit
            .tree_id()
            .map_err(|error| operation("native integration cherry-pick parent", error))?
            .detach();
        let commit_tree = commit
            .tree_id()
            .map_err(|error| operation("native integration cherry-pick tree", error))?
            .detach();
        let mut outcome = repository
            .merge_trees(
                parent_tree,
                current_tree,
                commit_tree,
                default_tree_labels(),
                repository
                    .tree_merge_options()
                    .map_err(|error| operation("native integration cherry-pick options", error))?,
            )
            .map_err(|error| operation("native integration cherry-pick merge", error))?;
        if outcome
            .has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::default())
        {
            return Ok((GitNativePreflightDisposition::Conflict, None));
        }
        current_tree = outcome
            .tree
            .write()
            .map_err(|error| operation("native integration cherry-pick candidate", error))?
            .detach();
    }
    Ok((GitNativePreflightDisposition::Eligible, Some(current_tree)))
}

fn materialize_cherry_pick_chain(
    repository: &gix::Repository,
    destination_ref: &str,
    destination_tip: gix::ObjectId,
    expected_tree: gix::ObjectId,
    commits: &[gix::ObjectId],
    cancellation: &CancellationToken,
) -> Result<gix::ObjectId, GitRepositoryError> {
    let destination_commit = repository
        .find_commit(destination_tip)
        .map_err(|error| operation("native integration destination tree", error))?;
    let destination_tree = destination_commit
        .tree_id()
        .map_err(|error| operation("native integration destination tree", error))?
        .detach();
    let (_, tree) = cherry_pick_candidate(repository, destination_tree, commits, cancellation)?;
    if tree != Some(expected_tree) {
        return Err(GitRepositoryError::Operation {
            operation: "native integration cherry-pick apply",
            detail: "candidate tree drift".to_owned(),
        });
    }
    let mut parent = destination_tip;
    let mut current_tree = destination_tree;
    for commit_id in commits {
        if cancellation.is_cancelled() {
            return Err(GitRepositoryError::Operation {
                operation: "native integration cherry-pick apply",
                detail: "cancelled".to_owned(),
            });
        }
        let source = repository
            .find_commit(*commit_id)
            .map_err(|error| operation("native integration cherry-pick source", error))?;
        let decoded = source
            .decode()
            .map_err(|error| operation("native integration cherry-pick decode", error))?;
        let mut parents = decoded.parents();
        let source_parent = parents
            .next()
            .ok_or_else(|| GitRepositoryError::Operation {
                operation: "native integration cherry-pick apply",
                detail: "root commit reached after eligible preflight".to_owned(),
            })?;
        if parents.next().is_some() {
            return Err(GitRepositoryError::Operation {
                operation: "native integration cherry-pick apply",
                detail: "merge commit reached after eligible preflight".to_owned(),
            });
        }
        let parent_tree = repository
            .find_commit(source_parent)
            .map_err(|error| operation("native integration cherry-pick parent", error))?
            .tree_id()
            .map_err(|error| operation("native integration cherry-pick parent tree", error))?
            .detach();
        let source_tree = source
            .tree_id()
            .map_err(|error| operation("native integration cherry-pick source tree", error))?
            .detach();
        let mut outcome = repository
            .merge_trees(
                parent_tree,
                current_tree,
                source_tree,
                default_tree_labels(),
                repository
                    .tree_merge_options()
                    .map_err(|error| operation("native integration cherry-pick options", error))?,
            )
            .map_err(|error| operation("native integration cherry-pick merge", error))?;
        if outcome
            .has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::default())
        {
            return Err(GitRepositoryError::Operation {
                operation: "native integration cherry-pick apply",
                detail: "conflict after revalidation".to_owned(),
            });
        }
        current_tree = outcome
            .tree
            .write()
            .map_err(|error| operation("native integration cherry-pick tree", error))?
            .detach();
        let message = source
            .decode()
            .map_err(|error| operation("native integration cherry-pick message", error))?
            .message
            .to_str_lossy();
        parent = repository
            .new_commit(message.as_ref(), current_tree, [parent])
            .map_err(|error| operation("native integration cherry-pick commit", error))?
            .id()
            .detach();
    }
    let final_commit = repository
        .find_commit(parent)
        .map_err(|error| operation("native integration cherry-pick final tree", error))?;
    let final_tree = final_commit
        .tree_id()
        .map_err(|error| operation("native integration cherry-pick final tree", error))?
        .detach();
    if final_tree != expected_tree {
        return Err(GitRepositoryError::Operation {
            operation: "native integration cherry-pick apply",
            detail: "materialized tree drift".to_owned(),
        });
    }
    update_ref_cas(repository, destination_ref, destination_tip, parent)?;
    Ok(parent)
}

fn linear_commit_closure(
    repository: &gix::Repository,
    source_tip: gix::ObjectId,
    merge_base: gix::ObjectId,
    cancellation: &CancellationToken,
) -> Result<Vec<gix::ObjectId>, GitRepositoryError> {
    let mut current = source_tip;
    let mut commits = Vec::new();
    while current != merge_base {
        if cancellation.is_cancelled() {
            return Err(GitRepositoryError::Operation {
                operation: "native integration dependency closure",
                detail: "cancelled".to_owned(),
            });
        }
        if commits.len() >= MAX_INTEGRATION_COMMITS {
            return Err(GitRepositoryError::Operation {
                operation: "native integration dependency closure",
                detail: format!("exceeds {MAX_INTEGRATION_COMMITS} commits"),
            });
        }
        let commit = repository
            .find_commit(current)
            .map_err(|error| operation("native integration dependency commit", error))?;
        let decoded = commit
            .decode()
            .map_err(|error| operation("native integration dependency decode", error))?;
        let mut parents = decoded.parents();
        let Some(parent) = parents.next() else {
            return Err(GitRepositoryError::Operation {
                operation: "native integration dependency closure",
                detail: "merge base is not on the first-parent chain".to_owned(),
            });
        };
        commits.push(current);
        current = parent;
    }
    commits.reverse();
    Ok(commits)
}

fn exact_reference_tip(
    repository: &gix::Repository,
    reference: &str,
    expected: &GitOidV1,
    operation_name: &'static str,
) -> Result<gix::ObjectId, GitRepositoryError> {
    let mut reference = repository
        .find_reference(reference)
        .map_err(|error| operation(operation_name, error))?;
    let target = reference
        .peel_to_id()
        .map_err(|error| operation(operation_name, error))?
        .detach();
    if target != parse_oid(expected, operation_name)? {
        return Err(GitRepositoryError::Operation {
            operation: operation_name,
            detail: "reference compare-and-set mismatch".to_owned(),
        });
    }
    Ok(target)
}

fn update_ref_cas(
    repository: &gix::Repository,
    reference: &str,
    old: gix::ObjectId,
    new: gix::ObjectId,
) -> Result<(), GitRepositoryError> {
    repository
        .reference(
            reference,
            new,
            gix::refs::transaction::PreviousValue::MustExistAndMatch(old.into()),
            "tracedecay native integration",
        )
        .map_err(|error| operation("native integration ref compare-and-set", error))?;
    Ok(())
}

fn validate_ref_pair(source: &str, destination: &str) -> Result<(), GitRepositoryError> {
    if source == destination
        || !source.starts_with("refs/heads/")
        || !destination.starts_with("refs/heads/")
    {
        return Err(GitRepositoryError::Operation {
            operation: "native integration ref selection",
            detail: "requires distinct full local branch refs".to_owned(),
        });
    }
    Ok(())
}

fn parse_oid(
    value: &GitOidV1,
    operation_name: &'static str,
) -> Result<gix::ObjectId, GitRepositoryError> {
    gix::ObjectId::from_hex(value.as_str().as_bytes())
        .map_err(|error| operation(operation_name, error))
}

fn oid(value: gix::ObjectId) -> Result<GitOidV1, GitRepositoryError> {
    GitOidV1::new(value.to_string()).map_err(|error| GitRepositoryError::Operation {
        operation: "native integration object identity",
        detail: error.to_string(),
    })
}

fn default_labels() -> gix::merge::blob::builtin_driver::text::Labels<'static> {
    gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some("base".into()),
        current: Some("destination".into()),
        other: Some("source".into()),
    }
}

fn default_tree_labels() -> gix::merge::blob::builtin_driver::text::Labels<'static> {
    gix::merge::blob::builtin_driver::text::Labels {
        ancestor: Some("parent".into()),
        current: Some("destination".into()),
        other: Some("commit".into()),
    }
}

fn fixed_merge_message(source_ref: &str, destination_ref: &str) -> String {
    format!("Merge {source_ref} into {destination_ref}")
}
