//! Truthful gix-based change classification for incremental indexing.
//!
//! Filesystem events and hook payloads only *hint* which paths may have moved.
//! The authoritative changed/added/deleted/renamed set is always recomputed
//! here from gix's HEAD-tree/index/worktree status, so a duplicate hook, a
//! save-without-change, or a dropped watcher event can never fabricate work or
//! hide a real change.
//!
//! Committed, staged, unstaged, untracked, deleted, and renamed paths are kept
//! *distinct*. Deletions become tombstone candidates; renames carry explicit
//! source→destination lineage. The scheduler consumes the derived candidate and
//! changed sets to build an incremental batch.

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
    /// Index rename versus HEAD tree.
    StagedRenamed,
    /// Worktree content differs from the index (unstaged modification).
    UnstagedModified,
    /// Tracked file missing from the worktree (unstaged deletion).
    UnstagedDeleted,
    /// Present on disk with no index relation (untracked, incl. intent-to-add).
    Untracked,
    /// Worktree rename detected versus the index.
    UnstagedRenamed,
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

/// One classified path and, for renames, its prior location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClassifiedChangeV1 {
    pub path: String,
    pub class: WorktreeChangeClassV1,
    /// For rename classes, the prior repository-relative path.
    pub rename_source: Option<String>,
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
            let (class, rename_source) = classify_item(&item);
            changes.push(ClassifiedChangeV1 {
                path,
                class,
                rename_source,
            });
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
    /// every staged, unstaged, untracked, or renamed path (deletions included so
    /// tombstones flow through). This is a hint to narrow work, not the identity
    /// authority; the generation planner still compares content digests.
    pub(crate) fn changed_paths(&self) -> BTreeSet<String> {
        let mut changed = BTreeSet::new();
        for change in &self.changes {
            changed.insert(change.path.clone());
            if let Some(source) = &change.rename_source {
                changed.insert(source.clone());
            }
        }
        changed
    }

    /// Paths removed from the present snapshot (staged or unstaged deletions).
    pub(crate) fn deleted_paths(&self) -> BTreeSet<String> {
        self.changes
            .iter()
            .filter(|change| change.class.is_deletion())
            .map(|change| change.path.clone())
            .collect()
    }

    /// Explicit rename lineage as `(source, destination)` pairs.
    pub(crate) fn renames(&self) -> Vec<(String, String)> {
        self.changes
            .iter()
            .filter_map(|change| {
                change
                    .rename_source
                    .clone()
                    .map(|source| (source, change.path.clone()))
            })
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

/// Map one gix status item to a truthful class and optional rename source.
fn classify_item(item: &gix::status::Item) -> (WorktreeChangeClassV1, Option<String>) {
    use gix::diff::index::ChangeRef;
    use gix::status::Item;
    use gix::status::index_worktree::Item as IndexWorktreeItem;
    use gix::status::plumbing::index_as_worktree::{Change as WorktreeChange, EntryStatus};

    match item {
        // HEAD tree ↔ index: staged changes.
        Item::TreeIndex(change) => match change {
            ChangeRef::Addition { .. } => (WorktreeChangeClassV1::StagedAdded, None),
            ChangeRef::Deletion { .. } => (WorktreeChangeClassV1::StagedDeleted, None),
            ChangeRef::Modification { .. } => (WorktreeChangeClassV1::StagedModified, None),
            ChangeRef::Rewrite {
                source_location, ..
            } => (
                WorktreeChangeClassV1::StagedRenamed,
                Some(source_location.to_str_lossy().into_owned()),
            ),
        },
        // Index ↔ worktree: unstaged / untracked changes.
        Item::IndexWorktree(worktree) => match worktree {
            IndexWorktreeItem::Modification { status, .. } => match status {
                EntryStatus::Change(WorktreeChange::Removed) => {
                    (WorktreeChangeClassV1::UnstagedDeleted, None)
                }
                EntryStatus::Conflict { .. } => (WorktreeChangeClassV1::Conflicted, None),
                EntryStatus::IntentToAdd => (WorktreeChangeClassV1::Untracked, None),
                // Any other worktree change (content, type, submodule) is an
                // unstaged modification of present content.
                EntryStatus::Change(_) | EntryStatus::NeedsUpdate(_) => {
                    (WorktreeChangeClassV1::UnstagedModified, None)
                }
            },
            IndexWorktreeItem::DirectoryContents { .. } => (WorktreeChangeClassV1::Untracked, None),
            IndexWorktreeItem::Rewrite { source, .. } => (
                WorktreeChangeClassV1::UnstagedRenamed,
                Some(source.rela_path().to_str_lossy().into_owned()),
            ),
        },
    }
}
