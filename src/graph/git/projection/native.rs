//! Bounded native-`gix` reads for the Git health projection.

use std::path::Path;

use gix::bstr::ByteSlice;
use gix::traverse::tree::{Recorder, Visit, visit::Action};
use tracedecay_application::{
    GitHealthProjectionBindingV1, GitHealthProjectionPartialReasonV1, GitHealthProjectionSourceV1,
};
use tracedecay_domain::{GitOidV1, canonical_sha256};

use super::{
    CommitRecordV1, GENERATION_DOMAIN, GitHealthProjectionError, HISTORY_WINDOW_SECS,
    MAX_CHANGED_FILES_PER_COMMIT, MAX_COMMIT_RECORD_PATH_BYTES, MAX_DURABLE_FRONTIER,
    WINDOW_BUCKET_SECS, cancellation_checkpoint,
};
use crate::application::context::CancellationToken;

pub(crate) fn capture_source(
    repository_root: &Path,
    binding: &GitHealthProjectionBindingV1,
    now_epoch_secs: i64,
) -> Result<GitHealthProjectionSourceV1, GitHealthProjectionError> {
    binding
        .validate()
        .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    let identity =
        crate::daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve(repository_root)
            .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    if identity.repository_id() != &binding.scope.repository_id
        || identity.worktree_id() != &binding.scope.worktree_id
        || identity.head_ref() != binding.scope.reference.as_ref()
    {
        return Err(GitHealthProjectionError::ScopeDrift);
    }
    let commit = identity
        .head_commit()
        .ok_or_else(|| GitHealthProjectionError::Git("HEAD has no commit".to_owned()))
        .and_then(|commit| {
            GitOidV1::new(commit.as_str())
                .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
        })?;
    let tree = identity
        .head_tree()
        .ok_or_else(|| GitHealthProjectionError::Git("HEAD commit has no readable tree".to_owned()))
        .and_then(|tree| {
            GitOidV1::new(tree.as_str())
                .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))
        })?;
    let window_end_epoch_secs = now_epoch_secs
        .checked_sub(now_epoch_secs.rem_euclid(WINDOW_BUCKET_SECS))
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(
                "Git health window end is outside the supported range".to_owned(),
            )
        })?;
    let window_start_epoch_secs = window_end_epoch_secs
        .checked_sub(HISTORY_WINDOW_SECS)
        .ok_or_else(|| {
            GitHealthProjectionError::Corrupt(
                "Git health window start is outside the supported range".to_owned(),
            )
        })?;
    let projection_generation = canonical_sha256(&(
        GENERATION_DOMAIN,
        binding,
        &commit,
        &tree,
        window_start_epoch_secs,
        window_end_epoch_secs,
    ))
    .map_err(|error| GitHealthProjectionError::Corrupt(error.to_string()))?;
    Ok(GitHealthProjectionSourceV1 {
        binding: binding.clone(),
        commit,
        tree,
        projection_generation,
        window_start_epoch_secs,
        window_end_epoch_secs,
    })
}

pub(super) fn require_current_target(
    repository_root: &Path,
    binding: &GitHealthProjectionBindingV1,
    now_epoch_secs: i64,
    target: &GitHealthProjectionSourceV1,
) -> Result<(), GitHealthProjectionError> {
    let current = capture_source(repository_root, binding, now_epoch_secs)?;
    if &current == target {
        Ok(())
    } else {
        Err(GitHealthProjectionError::ScopeDrift)
    }
}

pub(super) enum CollectCommitError {
    PathLimit,
    Partial(GitHealthProjectionPartialReasonV1),
    Projection(GitHealthProjectionError),
}

pub(super) fn collect_commit_record(
    repository: &gix::Repository,
    oid: &GitOidV1,
    cancellation: &CancellationToken,
) -> Result<CommitRecordV1, CollectCommitError> {
    cancellation_checkpoint(cancellation).map_err(CollectCommitError::Projection)?;
    let object_id = gix::ObjectId::from_hex(oid.as_str().as_bytes()).map_err(|error| {
        CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
    })?;
    let commit = repository
        .find_object(object_id)
        .map_err(|error| {
            CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
        })?
        .try_into_commit()
        .map_err(|error| {
            CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
        })?;
    let committed_at_epoch_secs = commit
        .time()
        .map_err(|error| {
            CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
        })?
        .seconds;
    let tree = GitOidV1::new(
        commit
            .tree_id()
            .map_err(|error| {
                CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
            })?
            .detach()
            .to_string(),
    )
    .map_err(|error| {
        CollectCommitError::Projection(GitHealthProjectionError::Corrupt(error.to_string()))
    })?;
    let parents = commit
        .parent_ids()
        .map(|parent| {
            GitOidV1::new(parent.detach().to_string()).map_err(|error| {
                CollectCommitError::Projection(GitHealthProjectionError::Corrupt(error.to_string()))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if parents.len() > MAX_DURABLE_FRONTIER {
        return Err(CollectCommitError::Partial(
            GitHealthProjectionPartialReasonV1::FrontierLimit,
        ));
    }
    let mut changed_files = if let Some(first_parent) = commit.parent_ids().next() {
        let parent_commit = repository
            .find_object(first_parent.detach())
            .map_err(|error| {
                CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
            })?
            .try_into_commit()
            .map_err(|error| {
                CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
            })?;
        changed_files_between(
            &parent_commit.tree().map_err(|error| {
                CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
            })?,
            &commit.tree().map_err(|error| {
                CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
            })?,
            cancellation,
        )?
    } else {
        let tree = commit.tree().map_err(|error| {
            CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
        })?;
        collect_root_tree_paths(&tree, || cancellation.is_cancelled())?
    };
    changed_files.sort();
    changed_files.dedup();
    cancellation_checkpoint(cancellation).map_err(CollectCommitError::Projection)?;
    Ok(CommitRecordV1 {
        oid: oid.clone(),
        tree,
        committed_at_epoch_secs,
        parents,
        changed_files,
    })
}

pub(super) fn collect_root_tree_paths(
    tree: &gix::Tree<'_>,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Vec<String>, CollectCommitError> {
    let mut visitor = BoundedRootVisitor::new(&mut is_cancelled);
    let traversal = tree.traverse().breadthfirst(&mut visitor);
    if visitor.cancelled {
        return Err(CollectCommitError::Projection(
            GitHealthProjectionError::Cancelled,
        ));
    }
    if visitor.bound_exceeded {
        return Err(CollectCommitError::PathLimit);
    }
    traversal.map_err(|error| {
        CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
    })?;
    if let Some(error) = visitor.path_error {
        return Err(CollectCommitError::Projection(error));
    }
    Ok(visitor.paths)
}

struct BoundedRootVisitor<'a, F> {
    recorder: Recorder,
    is_cancelled: &'a mut F,
    paths: Vec<String>,
    path_bytes: usize,
    cancelled: bool,
    bound_exceeded: bool,
    path_error: Option<GitHealthProjectionError>,
}

impl<'a, F: FnMut() -> bool> BoundedRootVisitor<'a, F> {
    fn new(is_cancelled: &'a mut F) -> Self {
        Self {
            recorder: Recorder::default(),
            is_cancelled,
            paths: Vec::new(),
            path_bytes: 0,
            cancelled: false,
            bound_exceeded: false,
            path_error: None,
        }
    }

    fn checkpoint(&mut self) -> bool {
        if (self.is_cancelled)() {
            self.cancelled = true;
            false
        } else {
            true
        }
    }
}

impl<F: FnMut() -> bool> Visit for BoundedRootVisitor<'_, F> {
    fn pop_back_tracked_path_and_set_current(&mut self) {
        self.recorder.pop_back_tracked_path_and_set_current();
    }

    fn pop_front_tracked_path_and_set_current(&mut self) {
        self.recorder.pop_front_tracked_path_and_set_current();
    }

    fn push_back_tracked_path_component(&mut self, component: &gix::bstr::BStr) {
        self.recorder.push_back_tracked_path_component(component);
    }

    fn push_path_component(&mut self, component: &gix::bstr::BStr) {
        self.recorder.push_path_component(component);
    }

    fn pop_path_component(&mut self) {
        self.recorder.pop_path_component();
    }

    fn visit_tree(&mut self, _entry: &gix::objs::tree::EntryRef<'_>) -> Action {
        if self.checkpoint() {
            std::ops::ControlFlow::Continue(true)
        } else {
            std::ops::ControlFlow::Break(())
        }
    }

    fn visit_nontree(&mut self, _entry: &gix::objs::tree::EntryRef<'_>) -> Action {
        if !self.checkpoint() {
            return std::ops::ControlFlow::Break(());
        }
        if self.paths.len() >= MAX_CHANGED_FILES_PER_COMMIT {
            self.bound_exceeded = true;
            return std::ops::ControlFlow::Break(());
        }
        let path = match exact_path(self.recorder.path().as_bytes()) {
            Ok(path) => path,
            Err(error) => {
                self.path_error = Some(error);
                return std::ops::ControlFlow::Break(());
            }
        };
        let Some(next_bytes) = self.path_bytes.checked_add(path.len()) else {
            self.bound_exceeded = true;
            return std::ops::ControlFlow::Break(());
        };
        if next_bytes > MAX_COMMIT_RECORD_PATH_BYTES {
            self.bound_exceeded = true;
            return std::ops::ControlFlow::Break(());
        }
        self.path_bytes = next_bytes;
        self.paths.push(path);
        std::ops::ControlFlow::Continue(true)
    }
}

fn changed_files_between(
    from: &gix::Tree<'_>,
    to: &gix::Tree<'_>,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, CollectCommitError> {
    let mut changed = Vec::new();
    let mut path_bytes = 0usize;
    let mut bound_exceeded = false;
    let mut path_error = None;
    from.changes()
        .map_err(|error| {
            CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
        })?
        .for_each_to_obtain_tree(to, |change| {
            if cancellation.is_cancelled() {
                return Ok::<_, std::convert::Infallible>(std::ops::ControlFlow::Break(()));
            }
            use gix::object::tree::diff::Change;
            let mut push_path = |path: &[u8]| {
                if changed.len() >= MAX_CHANGED_FILES_PER_COMMIT {
                    bound_exceeded = true;
                    return false;
                }
                match exact_path(path) {
                    Ok(path) => {
                        path_bytes = path_bytes.saturating_add(path.len());
                        if path_bytes > MAX_COMMIT_RECORD_PATH_BYTES {
                            bound_exceeded = true;
                            return false;
                        }
                        changed.push(path);
                    }
                    Err(error) => path_error = Some(error),
                }
                path_error.is_none()
            };
            let keep_going = match change {
                Change::Addition {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Modification {
                    location,
                    entry_mode,
                    ..
                }
                | Change::Deletion {
                    location,
                    entry_mode,
                    ..
                } => entry_mode.is_tree() || push_path(location.as_bytes()),
                Change::Rewrite {
                    source_location,
                    source_entry_mode,
                    location,
                    entry_mode,
                    ..
                } => {
                    (source_entry_mode.is_tree() || push_path(source_location.as_bytes()))
                        && (entry_mode.is_tree() || push_path(location.as_bytes()))
                }
            };
            Ok(if keep_going {
                std::ops::ControlFlow::Continue(())
            } else {
                std::ops::ControlFlow::Break(())
            })
        })
        .map_err(|error| {
            CollectCommitError::Projection(GitHealthProjectionError::Git(error.to_string()))
        })?;
    cancellation_checkpoint(cancellation).map_err(CollectCommitError::Projection)?;
    if let Some(error) = path_error {
        return Err(CollectCommitError::Projection(error));
    }
    if bound_exceeded {
        return Err(CollectCommitError::PathLimit);
    }
    Ok(changed)
}

fn exact_path(path: &[u8]) -> Result<String, GitHealthProjectionError> {
    std::str::from_utf8(path)
        .map(str::to_owned)
        .map_err(|_| GitHealthProjectionError::Git("Git path is not valid UTF-8".to_owned()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AncestorCheckV1 {
    Ancestor,
    NotAncestor,
    TraversalLimit,
}

pub(super) fn is_ancestor_bounded(
    repository: &gix::Repository,
    ancestor: &GitOidV1,
    head: &GitOidV1,
    max_commits: usize,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<AncestorCheckV1, GitHealthProjectionError> {
    if is_cancelled() {
        return Err(GitHealthProjectionError::Cancelled);
    }
    let ancestor_id = gix::ObjectId::from_hex(ancestor.as_str().as_bytes())
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    let head_id = gix::ObjectId::from_hex(head.as_str().as_bytes())
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    if ancestor_id == head_id {
        return Ok(AncestorCheckV1::Ancestor);
    }
    let walk = repository
        .rev_walk([head_id])
        .all()
        .map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
    for (ordinal, info) in walk.enumerate() {
        if is_cancelled() {
            return Err(GitHealthProjectionError::Cancelled);
        }
        if ordinal >= max_commits {
            return Ok(AncestorCheckV1::TraversalLimit);
        }
        let info = info.map_err(|error| GitHealthProjectionError::Git(error.to_string()))?;
        if info.id == ancestor_id {
            return Ok(AncestorCheckV1::Ancestor);
        }
    }
    Ok(AncestorCheckV1::NotAncestor)
}
