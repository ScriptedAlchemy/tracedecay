//! Truthful gix-based change classification for incremental indexing.
//!
//! Filesystem events and hook payloads only *hint* which paths may have moved.
//! The authoritative changed/added/deleted/renamed set is always recomputed
//! here from gix's HEAD-tree/index/worktree status, so a duplicate hook, a
//! save-without-change, or a dropped watcher event can never fabricate work or
//! hide a real change.
//!
//! Committed, staged, unstaged, untracked, and deleted paths are kept
//! *distinct*. Deletions become tombstone candidates. Renames are deliberately
//! not tracked as a distinct class: for indexing a rename is just a deletion of
//! the source plus an addition of the destination, so rename detection is
//! disabled and gix reports the two halves independently. The scheduler
//! consumes the derived candidate and changed sets to build an incremental
//! batch.

use std::collections::BTreeSet;

use gix::bstr::ByteSlice;

/// Failure to classify worktree changes through gix.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClassificationErrorV1 {
    #[error("code-index classification: {0}")]
    Git(String),
}

/// The truthful disposition of one repository-relative path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorktreeChangeClassV1 {
    /// Added to the index versus HEAD (staged new file).
    StagedAdded,
    /// Content differs between HEAD tree and index (staged modification).
    StagedModified,
    /// Present in HEAD tree, removed from the index (staged deletion).
    StagedDeleted,
    /// Worktree content differs from the index (unstaged modification).
    UnstagedModified,
    /// Tracked file missing from the worktree (unstaged deletion).
    UnstagedDeleted,
    /// Present on disk with no index relation (untracked, incl. intent-to-add).
    Untracked,
    /// Merge conflict; content is not a single truthful revision.
    Conflicted,
}

impl WorktreeChangeClassV1 {
    /// Whether this class removes a path (so its prior chunks become tombstones)
    /// rather than presenting current content to index.
    pub(crate) fn is_deletion(self) -> bool {
        matches!(self, Self::StagedDeleted | Self::UnstagedDeleted)
    }

    /// Whether this class contributes a present file whose bytes should be
    /// hashed and considered for (re)indexing.
    pub(crate) fn presents_content(self) -> bool {
        !self.is_deletion()
    }
}

/// One classified path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClassifiedChangeV1 {
    pub path: String,
    pub class: WorktreeChangeClassV1,
}

/// A complete, truthful classification of one worktree snapshot.
#[derive(Clone, Debug, Default)]
pub(crate) struct WorktreeChangeClassificationV1 {
    /// Repository-relative paths tracked in the index (the committed/staged
    /// baseline present in the checkout).
    committed_baseline: BTreeSet<String>,
    changes: Vec<ClassifiedChangeV1>,
}

impl WorktreeChangeClassificationV1 {
    /// Classify the current status of `repository` truthfully.
    pub(crate) fn classify(repository: &gix::Repository) -> Result<Self, ClassificationErrorV1> {
        let index = repository
            .index_or_empty()
            .map_err(|error| ClassificationErrorV1::Git(error.to_string()))?;
        let committed_baseline = index
            .entries()
            .iter()
            .filter_map(|entry| {
                std::str::from_utf8(entry.path(&index).as_ref())
                    .ok()
                    .map(str::to_owned)
            })
            .collect::<BTreeSet<_>>();

        let mut changes = Vec::new();
        let status = repository
            .status(gix::progress::Discard)
            .map_err(|error| ClassificationErrorV1::Git(error.to_string()))?
            // Emit untracked files, but not ignored ones: ignored/generated
            // content is out of indexing scope.
            .untracked_files(gix::status::UntrackedFiles::Files)
            // Rename detection is deliberately disabled on both the tree↔index
            // and index↔worktree axes: for indexing a rename is just a deletion
            // plus an addition, and rewrite tracking is expensive. Content
            // reuse still happens via the content-addressed byte pool.
            .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
            .index_worktree_rewrites(None)
            // Submodule content belongs to its own repository identity and is
            // never indexed as part of this worktree.
            .index_worktree_submodules(None)
            .into_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|error| ClassificationErrorV1::Git(error.to_string()))?;
        for item in status {
            let item = item.map_err(|error| ClassificationErrorV1::Git(error.to_string()))?;
            let path = item.location().to_str_lossy().into_owned();
            if let Some(class) = classify_item(&item) {
                changes.push(ClassifiedChangeV1 { path, class });
            }
        }

        Ok(Self {
            committed_baseline,
            changes,
        })
    }

    /// Present files worth hashing and considering for (re)indexing: the
    /// committed baseline, plus untracked/added/rename-destination paths, minus
    /// any path removed by a deletion.
    pub(crate) fn candidate_paths(&self) -> BTreeSet<String> {
        let mut candidates = self.committed_baseline.clone();
        for change in &self.changes {
            if change.class.presents_content() {
                candidates.insert(change.path.clone());
            } else {
                candidates.remove(&change.path);
            }
        }
        candidates
    }

    /// Paths whose indexing evidence changed relative to the last generation:
    /// every staged, unstaged, or untracked path (deletions included so
    /// tombstones flow through). This is a hint to narrow work, not the identity
    /// authority; the generation planner still compares content digests.
    pub(crate) fn changed_paths(&self) -> BTreeSet<String> {
        self.changes
            .iter()
            .map(|change| change.path.clone())
            .collect()
    }

    /// Paths removed from the present snapshot (staged or unstaged deletions).
    pub(crate) fn deleted_paths(&self) -> BTreeSet<String> {
        self.changes
            .iter()
            .filter(|change| change.class.is_deletion())
            .map(|change| change.path.clone())
            .collect()
    }

    /// All classified changes (for reporting and tests).
    pub(crate) fn changes(&self) -> &[ClassifiedChangeV1] {
        &self.changes
    }

    /// The class recorded for `path`, if any change touched it.
    pub(crate) fn class_of(&self, path: &str) -> Option<WorktreeChangeClassV1> {
        self.changes
            .iter()
            .find(|change| change.path == path)
            .map(|change| change.class)
    }
}

/// Map one gix status item to a truthful class, or `None` for items that carry
/// no indexing signal on their own.
///
/// Rename detection is disabled on both status axes (see [`classify`]), so gix
/// reports a move as an independent deletion of the source plus an addition of
/// the destination. The `Rewrite` variants therefore cannot occur here; they
/// are mapped to `None` defensively rather than fabricating a rename class.
fn classify_item(item: &gix::status::Item) -> Option<WorktreeChangeClassV1> {
    use gix::diff::index::ChangeRef;
    use gix::status::Item;
    use gix::status::index_worktree::Item as IndexWorktreeItem;
    use gix::status::plumbing::index_as_worktree::{Change as WorktreeChange, EntryStatus};

    Some(match item {
        // HEAD tree ↔ index: staged changes.
        Item::TreeIndex(change) => match change {
            ChangeRef::Addition { .. } => WorktreeChangeClassV1::StagedAdded,
            ChangeRef::Deletion { .. } => WorktreeChangeClassV1::StagedDeleted,
            ChangeRef::Modification { .. } => WorktreeChangeClassV1::StagedModified,
            // Unreachable with rename detection disabled; skip rather than
            // classify a move as anything other than delete + add.
            ChangeRef::Rewrite { .. } => return None,
        },
        // Index ↔ worktree: unstaged / untracked changes.
        Item::IndexWorktree(worktree) => match worktree {
            IndexWorktreeItem::Modification { status, .. } => match status {
                EntryStatus::Change(WorktreeChange::Removed) => {
                    WorktreeChangeClassV1::UnstagedDeleted
                }
                EntryStatus::Conflict { .. } => WorktreeChangeClassV1::Conflicted,
                EntryStatus::IntentToAdd => WorktreeChangeClassV1::Untracked,
                // Any other worktree change (content, type, submodule) is an
                // unstaged modification of present content.
                EntryStatus::Change(_) | EntryStatus::NeedsUpdate(_) => {
                    WorktreeChangeClassV1::UnstagedModified
                }
            },
            IndexWorktreeItem::DirectoryContents { .. } => WorktreeChangeClassV1::Untracked,
            // Unreachable with rewrite tracking disabled; skip defensively.
            IndexWorktreeItem::Rewrite { .. } => return None,
        },
    })
}
