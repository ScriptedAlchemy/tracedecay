//! Bounded native commit topology for preview-bound integration.
//!
//! Git object identity and ancestry come from `gix`. The deterministic
//! parent-first order is a presentation of that native graph, never a second
//! branch-stack or repository authority.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use tracedecay_domain::GitOidV1;
use tracedecay_runtime_core::cancellation::{CancellationToken, MonotonicDeadline};
use tracedecay_runtime_core::git_discovery::GitRepositoryIdentity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeIntegrationTopologyLimits {
    pub(crate) max_commits: usize,
    pub(crate) max_parent_edges: usize,
    pub(crate) max_decoded_commit_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeIntegrationTopology {
    pub(crate) merge_base: GitOidV1,
    pub(crate) ordered_source_only: Vec<GitOidV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeIntegrationTopologyFailure {
    RepositoryUnavailable,
    RepositoryIdentityChanged,
    MissingObject,
    NoMergeBase,
    AmbiguousMergeBase,
    UnsupportedHistory,
    CommitLimit,
    ParentEdgeLimit,
    DecodedCommitByteLimit,
    Cancelled,
    DeadlineExceeded,
}

pub(crate) fn capture_native_integration_topology(
    repository_identity: &GitRepositoryIdentity,
    source: &GitOidV1,
    destination: &GitOidV1,
    limits: NativeIntegrationTopologyLimits,
    cancellation: &CancellationToken,
    deadline: MonotonicDeadline,
) -> Result<NativeIntegrationTopology, NativeIntegrationTopologyFailure> {
    check_control(cancellation, deadline)?;
    if limits.max_commits == 0 {
        return Err(NativeIntegrationTopologyFailure::CommitLimit);
    }
    if limits.max_parent_edges == 0 {
        return Err(NativeIntegrationTopologyFailure::ParentEdgeLimit);
    }
    if limits.max_decoded_commit_bytes == 0 {
        return Err(NativeIntegrationTopologyFailure::DecodedCommitByteLimit);
    }

    let repository = gix::open(&repository_identity.worktree_root)
        .map_err(|_| NativeIntegrationTopologyFailure::RepositoryUnavailable)?;
    verify_repository_identity(&repository, repository_identity)?;
    reject_incomplete_or_rewritten_history(&repository)?;
    let source_id = parse_oid(source)?;
    let destination_id = parse_oid(destination)?;
    repository
        .find_commit(source_id)
        .and_then(|_| repository.find_commit(destination_id))
        .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?;
    check_control(cancellation, deadline)?;

    let merge_bases = repository
        .merge_bases_many(source_id, &[destination_id])
        .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?;
    let merge_base = match merge_bases.as_slice() {
        [] => return Err(NativeIntegrationTopologyFailure::NoMergeBase),
        [merge_base] => GitOidV1::new(merge_base.to_string())
            .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?,
        _ => return Err(NativeIntegrationTopologyFailure::AmbiguousMergeBase),
    };

    let walk = repository
        .rev_walk([source_id])
        .with_hidden([destination_id])
        .sorting(gix::revision::walk::Sorting::BreadthFirst)
        .all()
        .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?;
    let mut graph = BTreeMap::<GitOidV1, Vec<GitOidV1>>::new();
    let mut parent_edges = 0_usize;
    let mut decoded_commit_bytes = 0_usize;
    for entry in walk {
        check_control(cancellation, deadline)?;
        if graph.len() >= limits.max_commits {
            return Err(NativeIntegrationTopologyFailure::CommitLimit);
        }
        let entry = entry.map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?;
        let commit_bytes = entry
            .object()
            .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?
            .data
            .len();
        decoded_commit_bytes = decoded_commit_bytes
            .checked_add(commit_bytes)
            .ok_or(NativeIntegrationTopologyFailure::DecodedCommitByteLimit)?;
        if decoded_commit_bytes > limits.max_decoded_commit_bytes {
            return Err(NativeIntegrationTopologyFailure::DecodedCommitByteLimit);
        }
        let commit_id = GitOidV1::new(entry.id.to_string())
            .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)?;
        let parents = entry
            .parent_ids
            .into_iter()
            .map(|parent| {
                GitOidV1::new(parent.to_string())
                    .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)
            })
            .collect::<Result<Vec<_>, _>>()?;
        parent_edges = parent_edges
            .checked_add(parents.len())
            .ok_or(NativeIntegrationTopologyFailure::ParentEdgeLimit)?;
        if parent_edges > limits.max_parent_edges {
            return Err(NativeIntegrationTopologyFailure::ParentEdgeLimit);
        }
        graph.insert(commit_id, parents);
    }
    check_control(cancellation, deadline)?;

    Ok(NativeIntegrationTopology {
        merge_base,
        ordered_source_only: deterministic_parent_first(&graph)?,
    })
}

fn verify_repository_identity(
    repository: &gix::Repository,
    expected: &GitRepositoryIdentity,
) -> Result<(), NativeIntegrationTopologyFailure> {
    let worktree_root = repository
        .workdir()
        .ok_or(NativeIntegrationTopologyFailure::RepositoryIdentityChanged)?
        .canonicalize()
        .map_err(|_| NativeIntegrationTopologyFailure::RepositoryUnavailable)?;
    let git_dir = repository
        .git_dir()
        .canonicalize()
        .map_err(|_| NativeIntegrationTopologyFailure::RepositoryUnavailable)?;
    let common_dir = repository
        .common_dir()
        .canonicalize()
        .map_err(|_| NativeIntegrationTopologyFailure::RepositoryUnavailable)?;
    if worktree_root != expected.worktree_root
        || git_dir != expected.git_dir
        || common_dir != expected.common_dir
    {
        return Err(NativeIntegrationTopologyFailure::RepositoryIdentityChanged);
    }
    Ok(())
}

fn deterministic_parent_first(
    graph: &BTreeMap<GitOidV1, Vec<GitOidV1>>,
) -> Result<Vec<GitOidV1>, NativeIntegrationTopologyFailure> {
    let mut remaining_parents = BTreeMap::<GitOidV1, usize>::new();
    let mut children = BTreeMap::<GitOidV1, Vec<GitOidV1>>::new();
    for (commit, parents) in graph {
        let closure_parents = parents
            .iter()
            .filter(|parent| graph.contains_key(*parent))
            .count();
        remaining_parents.insert(commit.clone(), closure_parents);
        for parent in parents.iter().filter(|parent| graph.contains_key(*parent)) {
            children
                .entry(parent.clone())
                .or_default()
                .push(commit.clone());
        }
    }
    for dependents in children.values_mut() {
        dependents.sort_unstable();
        dependents.dedup();
    }

    let mut ready = remaining_parents
        .iter()
        .filter_map(|(commit, count)| (*count == 0).then_some(commit.clone()))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(graph.len());
    while let Some(commit) = ready.pop_first() {
        ordered.push(commit.clone());
        if let Some(dependents) = children.get(&commit) {
            for dependent in dependents {
                let Some(count) = remaining_parents.get_mut(dependent) else {
                    return Err(NativeIntegrationTopologyFailure::MissingObject);
                };
                let Some(replacement) = count.checked_sub(1) else {
                    return Err(NativeIntegrationTopologyFailure::MissingObject);
                };
                *count = replacement;
                if replacement == 0 {
                    ready.insert(dependent.clone());
                }
            }
        }
    }
    if ordered.len() != graph.len() {
        return Err(NativeIntegrationTopologyFailure::UnsupportedHistory);
    }
    Ok(ordered)
}

fn reject_incomplete_or_rewritten_history(
    repository: &gix::Repository,
) -> Result<(), NativeIntegrationTopologyFailure> {
    let configuration = repository.config_snapshot();
    let has_promisor_remote = configuration
        .sections_by_name("remote")
        .into_iter()
        .flatten()
        .filter_map(|section| section.value("promisor"))
        .any(|value| git_config_boolean_is_true(value.as_ref()));
    if repository.is_shallow()
        || repository.common_dir().join("info/grafts").is_file()
        || configuration.string("extensions.partialclone").is_some()
        || has_promisor_remote
    {
        return Err(NativeIntegrationTopologyFailure::UnsupportedHistory);
    }
    let references = repository
        .references()
        .map_err(|_| NativeIntegrationTopologyFailure::RepositoryUnavailable)?;
    let mut replacements = references
        .prefixed("refs/replace/")
        .map_err(|_| NativeIntegrationTopologyFailure::RepositoryUnavailable)?;
    match replacements.next() {
        Some(Ok(_)) => Err(NativeIntegrationTopologyFailure::UnsupportedHistory),
        Some(Err(_)) => Err(NativeIntegrationTopologyFailure::RepositoryUnavailable),
        None => Ok(()),
    }
}

fn git_config_boolean_is_true(value: &[u8]) -> bool {
    value.is_empty()
        || value.eq_ignore_ascii_case(b"true")
        || value.eq_ignore_ascii_case(b"yes")
        || value.eq_ignore_ascii_case(b"on")
        || value == b"1"
}

fn parse_oid(value: &GitOidV1) -> Result<gix::ObjectId, NativeIntegrationTopologyFailure> {
    gix::ObjectId::from_hex(value.as_str().as_bytes())
        .map_err(|_| NativeIntegrationTopologyFailure::MissingObject)
}

fn check_control(
    cancellation: &CancellationToken,
    deadline: MonotonicDeadline,
) -> Result<(), NativeIntegrationTopologyFailure> {
    if cancellation.is_cancelled() {
        return Err(NativeIntegrationTopologyFailure::Cancelled);
    }
    if deadline.is_elapsed_at(Instant::now()) {
        return Err(NativeIntegrationTopologyFailure::DeadlineExceeded);
    }
    Ok(())
}
