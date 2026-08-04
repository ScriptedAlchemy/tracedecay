use std::collections::BTreeSet;
use std::ops::ControlFlow;

use gix::bstr::ByteSlice as _;
use tracedecay_domain::git::{
    GitCommitIdentityV1, GitCommitMetadataV1, GitDegradationV1, GitOidV1,
};
use tracedecay_domain::research::canonical_sha256;
use tracedecay_domain::research::time::UtcMicros;

use super::{GitRepositoryAuthority, GitRepositoryError, operation};
use crate::cancellation::CancellationToken;

/// Fixed options for a bounded commit traversal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHistoryOptions {
    pub max_count: u32,
    pub first_parent: bool,
    pub path: Option<String>,
    pub follow_renames: bool,
}

/// Hard bounds for one commit-history read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitHistoryBudget {
    pub commits: usize,
    pub trees: usize,
    pub objects: usize,
}

impl GitHistoryBudget {
    fn for_max_count(max_count: u32) -> Self {
        let commits = (max_count.max(1) as usize)
            .saturating_mul(1024)
            .clamp(1024, 100_000);
        Self {
            commits,
            trees: commits.saturating_mul(1024).min(1_000_000),
            objects: commits.saturating_mul(2048).min(2_000_000),
        }
    }
}

/// The exact reason a history read stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHistoryTermination {
    Complete,
    OutputLimit,
    ShallowBoundary,
    Cancelled,
    CommitBudget,
    TreeBudget,
    ObjectBudget,
    UnreadableObject {
        object: Option<String>,
        detail: String,
    },
}

/// Bounded history without application-specific repository identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryHistory {
    pub commits: Vec<GitCommitMetadataV1>,
    pub truncated: bool,
    pub degradations: BTreeSet<GitDegradationV1>,
    pub termination: GitHistoryTermination,
}

impl GitRepositoryAuthority {
    /// Bounded in-process commit traversal.
    pub fn history(
        &self,
        options: &GitHistoryOptions,
    ) -> Result<GitRepositoryHistory, GitRepositoryError> {
        self.history_with_control(
            options,
            GitHistoryBudget::for_max_count(options.max_count),
            &CancellationToken::new(),
        )
    }

    /// Bounded traversal with caller-owned cancellation and resource limits.
    pub fn history_with_control(
        &self,
        options: &GitHistoryOptions,
        budget: GitHistoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<GitRepositoryHistory, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        let head = self.head()?;
        let op_state = self.operation_state();
        let mut degradations = self.degradations(&repository, &head, op_state);
        let Some(head_id) = head.commit() else {
            return Ok(GitRepositoryHistory {
                commits: Vec::new(),
                truncated: false,
                degradations,
                termination: GitHistoryTermination::Complete,
            });
        };
        let shallow =
            self.git_dir.join("shallow").is_file() || self.common_dir.join("shallow").is_file();
        if shallow {
            degradations.insert(GitDegradationV1::ShallowBoundary);
        }

        let tip = gix::hash::ObjectId::from_hex(head_id.as_str().as_bytes())
            .map_err(|error| operation("history", error))?;
        let mut walk =
            repository
                .rev_walk([tip])
                .sorting(gix::revision::walk::Sorting::ByCommitTime(
                    Default::default(),
                ));
        if options.first_parent {
            walk = walk.first_parent_only();
        }
        let walk = walk.all().map_err(|error| operation("history", error))?;
        let max_count = options.max_count.max(1) as usize;
        let mut path = options.path.clone();
        let mut commits = Vec::with_capacity(max_count.saturating_add(1));
        let mut counters = HistoryCounters::default();
        let mut termination = GitHistoryTermination::Complete;
        let mut walk = walk.peekable();

        while walk.peek().is_some() {
            if cancellation.is_cancelled() {
                termination = GitHistoryTermination::Cancelled;
                break;
            }
            if commits.len() > max_count {
                termination = GitHistoryTermination::OutputLimit;
                break;
            }
            if counters.commits >= budget.commits {
                termination = GitHistoryTermination::CommitBudget;
                break;
            }
            if counters.objects >= budget.objects {
                termination = GitHistoryTermination::ObjectBudget;
                break;
            }
            counters.commits += 1;
            counters.objects += 1;

            let Some(info) = walk.next() else {
                break;
            };
            let info = match info {
                Ok(info) => info,
                Err(error) => {
                    termination = unreadable_history(None, error);
                    break;
                }
            };
            let commit = match repository.find_commit(info.id) {
                Ok(commit) => commit,
                Err(error) => {
                    termination = unreadable_history(Some(info.id.to_string()), error);
                    break;
                }
            };
            if let Some(selected_path) = path.as_mut() {
                match commit_touches_path(
                    &commit,
                    selected_path,
                    options.follow_renames,
                    budget,
                    &mut counters,
                    cancellation,
                ) {
                    Ok(PathRead {
                        touched,
                        termination: path_termination,
                    }) => {
                        if touched {
                            commits.push(commit_metadata(&commit)?);
                        }
                        if let Some(path_termination) = path_termination {
                            termination = path_termination;
                            break;
                        }
                        continue;
                    }
                    Err(error) => {
                        termination = unreadable_history(Some(commit.id().to_string()), error);
                        break;
                    }
                }
            }
            commits.push(commit_metadata(&commit)?);
        }

        if termination == GitHistoryTermination::Complete {
            if commits.len() > max_count {
                termination = GitHistoryTermination::OutputLimit;
            } else if shallow {
                termination = GitHistoryTermination::ShallowBoundary;
            }
        }
        let truncated = termination != GitHistoryTermination::Complete;
        if commits.len() > max_count {
            commits.truncate(max_count);
        }
        if truncated {
            degradations.insert(GitDegradationV1::TruncatedOutput);
        }
        if matches!(termination, GitHistoryTermination::UnreadableObject { .. }) {
            degradations.insert(GitDegradationV1::UnreadableState);
        }
        Ok(GitRepositoryHistory {
            commits,
            truncated,
            degradations,
            termination,
        })
    }
}

fn commit_metadata(commit: &gix::Commit<'_>) -> Result<GitCommitMetadataV1, GitRepositoryError> {
    let decoded = commit
        .decode()
        .map_err(|error| operation("history", error))?;
    let author = decoded
        .author()
        .map_err(|error| operation("history", error))?;
    let committer = decoded
        .committer()
        .map_err(|error| operation("history", error))?;
    let message = decoded.message.to_str_lossy();
    let subject = message
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(512)
        .collect();
    Ok(GitCommitMetadataV1 {
        commit: GitOidV1::new(commit.id().to_string())?,
        tree: GitOidV1::new(decoded.tree().to_string())?,
        parents: decoded
            .parents()
            .map(|parent| GitOidV1::new(parent.to_string()))
            .collect::<Result<_, _>>()?,
        author: GitCommitIdentityV1 {
            name: author.name.to_str_lossy().into_owned(),
            email: author.email.to_str_lossy().into_owned(),
            at: UtcMicros(author.seconds().saturating_mul(1_000_000)),
        },
        committer: GitCommitIdentityV1 {
            name: committer.name.to_str_lossy().into_owned(),
            email: committer.email.to_str_lossy().into_owned(),
            at: UtcMicros(committer.seconds().saturating_mul(1_000_000)),
        },
        subject,
        message_digest: canonical_sha256(&message.as_ref())?,
    })
}

fn commit_touches_path(
    commit: &gix::Commit<'_>,
    path: &mut String,
    follow_renames: bool,
    budget: GitHistoryBudget,
    counters: &mut HistoryCounters,
    cancellation: &CancellationToken,
) -> Result<PathRead, GitRepositoryError> {
    if cancellation.is_cancelled() {
        return Ok(PathRead::stopped(GitHistoryTermination::Cancelled));
    }
    if counters.trees >= budget.trees {
        return Ok(PathRead::stopped(GitHistoryTermination::TreeBudget));
    }
    if counters.objects >= budget.objects {
        return Ok(PathRead::stopped(GitHistoryTermination::ObjectBudget));
    }
    counters.trees += 1;
    counters.objects += 1;
    let tree = commit.tree().map_err(|error| operation("history", error))?;
    let parents = commit.parent_ids().collect::<Vec<_>>();
    if parents.is_empty() {
        if let Some(termination) = charge_nested_path(path, 1, budget, counters, cancellation) {
            return Ok(PathRead::stopped(termination));
        }
        return Ok(PathRead {
            touched: tree_entry(&tree, path)?.is_some(),
            termination: None,
        });
    }
    if counters.objects >= budget.objects {
        return Ok(PathRead::stopped(GitHistoryTermination::ObjectBudget));
    }
    counters.objects += 1;
    let parent_object = parents[0]
        .object()
        .map_err(|error| operation("history", error))?;
    let parent = parent_object
        .try_into_commit()
        .map_err(|error| operation("history", error))?;
    if counters.trees >= budget.trees {
        return Ok(PathRead::stopped(GitHistoryTermination::TreeBudget));
    }
    if counters.objects >= budget.objects {
        return Ok(PathRead::stopped(GitHistoryTermination::ObjectBudget));
    }
    counters.trees += 1;
    counters.objects += 1;
    let parent_tree = parent.tree().map_err(|error| operation("history", error))?;
    if !follow_renames {
        if let Some(termination) = charge_nested_path(path, 2, budget, counters, cancellation) {
            return Ok(PathRead::stopped(termination));
        }
        return Ok(PathRead {
            touched: tree_entry(&tree, path)? != tree_entry(&parent_tree, path)?,
            termination: None,
        });
    }
    if let Some(termination) = bounded_tree_inventory(&parent_tree, budget, counters, cancellation)?
    {
        return Ok(PathRead::stopped(termination));
    }
    if let Some(termination) = bounded_tree_inventory(&tree, budget, counters, cancellation)? {
        return Ok(PathRead::stopped(termination));
    }
    let remaining_objects = budget.objects.saturating_sub(counters.objects);
    if remaining_objects == 0 {
        return Ok(PathRead::stopped(GitHistoryTermination::ObjectBudget));
    }

    let selected = path.as_bytes();
    let mut touched = false;
    let mut previous_path = None;
    let mut stopped = None;
    let mut changes = parent_tree
        .changes()
        .map_err(|error| operation("history", error))?;
    changes.options(|options| {
        options.track_path();
        options.track_rewrites(Some(gix::diff::Rewrites {
            limit: remaining_objects.isqrt().max(1),
            ..Default::default()
        }));
    });
    let outcome = changes
        .for_each_to_obtain_tree(&tree, |change| {
            if cancellation.is_cancelled() {
                stopped = Some(GitHistoryTermination::Cancelled);
                return Ok::<_, std::convert::Infallible>(ControlFlow::Break(()));
            }
            use gix::object::tree::diff::Change;
            match change {
                Change::Addition { location, .. }
                | Change::Deletion { location, .. }
                | Change::Modification { location, .. } => {
                    if location.as_bytes() == selected {
                        touched = true;
                    }
                }
                Change::Rewrite {
                    source_location,
                    location,
                    copy,
                    ..
                } => {
                    if location.as_bytes() == selected {
                        touched = true;
                        if !copy {
                            previous_path = source_location.to_str().ok().map(str::to_owned);
                        }
                    } else if source_location.as_bytes() == selected {
                        touched = true;
                    }
                }
            }
            Ok(ControlFlow::Continue(()))
        })
        .map_err(|error| operation("history", error))?;

    if let Some(outcome) = outcome {
        counters.objects = counters
            .objects
            .saturating_add(outcome.num_similarity_checks);
        if outcome.num_similarity_checks_skipped_for_rename_tracking_due_to_limit > 0 {
            stopped = Some(GitHistoryTermination::ObjectBudget);
        }
    }
    if let Some(previous_path) = previous_path {
        *path = previous_path;
    }
    Ok(PathRead {
        touched,
        termination: stopped,
    })
}

fn tree_entry(
    tree: &gix::Tree<'_>,
    path: &str,
) -> Result<Option<(gix::hash::ObjectId, gix::object::tree::EntryMode)>, GitRepositoryError> {
    tree.lookup_entry_by_path(path)
        .map(|entry| entry.map(|entry| (entry.object_id(), entry.mode())))
        .map_err(|error| operation("history", error))
}

#[derive(Default)]
struct HistoryCounters {
    commits: usize,
    trees: usize,
    objects: usize,
}

struct PathRead {
    touched: bool,
    termination: Option<GitHistoryTermination>,
}

impl PathRead {
    fn stopped(termination: GitHistoryTermination) -> Self {
        Self {
            touched: false,
            termination: Some(termination),
        }
    }
}

fn unreadable_history(
    object: Option<String>,
    error: impl std::fmt::Display,
) -> GitHistoryTermination {
    GitHistoryTermination::UnreadableObject {
        object,
        detail: error.to_string(),
    }
}

fn charge_nested_path(
    path: &str,
    tree_count: usize,
    budget: GitHistoryBudget,
    counters: &mut HistoryCounters,
    cancellation: &CancellationToken,
) -> Option<GitHistoryTermination> {
    if cancellation.is_cancelled() {
        return Some(GitHistoryTermination::Cancelled);
    }
    let nested_trees = path
        .as_bytes()
        .iter()
        .filter(|byte| **byte == b'/')
        .count()
        .saturating_mul(tree_count);
    if counters.trees.saturating_add(nested_trees) > budget.trees {
        return Some(GitHistoryTermination::TreeBudget);
    }
    if counters.objects.saturating_add(nested_trees) > budget.objects {
        return Some(GitHistoryTermination::ObjectBudget);
    }
    counters.trees += nested_trees;
    counters.objects += nested_trees;
    None
}

fn bounded_tree_inventory(
    tree: &gix::Tree<'_>,
    budget: GitHistoryBudget,
    counters: &mut HistoryCounters,
    cancellation: &CancellationToken,
) -> Result<Option<GitHistoryTermination>, GitRepositoryError> {
    let mut visitor = BoundedTreeVisitor {
        budget,
        counters,
        cancellation,
        termination: None,
    };
    match tree.traverse().breadthfirst(&mut visitor) {
        Ok(()) => Ok(None),
        Err(gix::traverse::tree::breadthfirst::Error::Cancelled) => Ok(visitor.termination.take()),
        Err(error) => Err(operation("history", error)),
    }
}

struct BoundedTreeVisitor<'a> {
    budget: GitHistoryBudget,
    counters: &'a mut HistoryCounters,
    cancellation: &'a CancellationToken,
    termination: Option<GitHistoryTermination>,
}

impl BoundedTreeVisitor<'_> {
    fn visit(&mut self, tree: bool) -> gix::traverse::tree::visit::Action {
        if self.cancellation.is_cancelled() {
            self.termination = Some(GitHistoryTermination::Cancelled);
            return ControlFlow::Break(());
        }
        if tree && self.counters.trees >= self.budget.trees {
            self.termination = Some(GitHistoryTermination::TreeBudget);
            return ControlFlow::Break(());
        }
        if self.counters.objects >= self.budget.objects {
            self.termination = Some(GitHistoryTermination::ObjectBudget);
            return ControlFlow::Break(());
        }
        self.counters.objects += 1;
        if tree {
            self.counters.trees += 1;
        }
        ControlFlow::Continue(true)
    }
}

impl gix::traverse::tree::Visit for BoundedTreeVisitor<'_> {
    fn pop_back_tracked_path_and_set_current(&mut self) {}

    fn pop_front_tracked_path_and_set_current(&mut self) {}

    fn push_back_tracked_path_component(&mut self, _component: &gix::bstr::BStr) {}

    fn push_path_component(&mut self, _component: &gix::bstr::BStr) {}

    fn pop_path_component(&mut self) {}

    fn visit_tree(
        &mut self,
        _entry: &gix::objs::tree::EntryRef<'_>,
    ) -> gix::traverse::tree::visit::Action {
        self.visit(true)
    }

    fn visit_nontree(
        &mut self,
        _entry: &gix::objs::tree::EntryRef<'_>,
    ) -> gix::traverse::tree::visit::Action {
        self.visit(false)
    }
}
