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
            // TraceDecay's own project-local private store is daemon-written
            // state, never checkout content. Counting it as a worktree change
            // would make every enrolled checkout permanently "dirty" — see
            // [`is_tracedecay_owned_state_path`].
            if is_tracedecay_owned_state_path(&path) {
                continue;
            }
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

/// Whether `path` (repository-relative, forward-slash) names `TraceDecay`'s own
/// project-local private store directory or something inside it.
///
/// Enrolling a project writes `.tracedecay/enrollment.json` into the checkout
/// (`tracedecay_runtime_core::storage::identity::enrollment_marker_path`), and
/// the daemon keeps other private state there. Those bytes are produced by
/// `TraceDecay`, are never checkout content, and are not indexable source. Unless
/// the user happens to ignore `.tracedecay/`, gix reports them as untracked, so
/// treating them as a worktree change makes *every* enrolled checkout look
/// permanently dirty: no capture can then seal an exact HEAD tree, no published
/// generation carries a `source_revision`, and exact-scope admission refuses
/// forever with `lsp-code-index-source-revision-unavailable`.
///
/// The legacy indexing lane already applies this rule
/// (`crate::tracedecay::indexing::is_tracedecay_state_path`); it is private to
/// that module, so the code-index classifier states it independently rather
/// than widening a legacy surface.
fn is_tracedecay_owned_state_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized == crate::config::TRACEDECAY_DIR
        || normalized
            .strip_prefix(crate::config::TRACEDECAY_DIR)
            .is_some_and(|suffix| suffix.starts_with('/'))
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git failed: {args:?}");
    }

    /// A committed checkout with no ambient ignore rules. The operator's global
    /// excludes file is deliberately pointed at a path that does not exist, so
    /// this test observes the same untracked set a user without a
    /// `.tracedecay/` ignore entry would.
    fn committed_repo() -> TempDir {
        let root = TempDir::new().expect("root");
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.name", "T"]);
        git(root.path(), &["config", "user.email", "t@example.invalid"]);
        git(
            root.path(),
            &[
                "config",
                "core.excludesFile",
                root.path().join("absent-global-excludes").to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "init"]);
        root
    }

    #[test]
    fn tracedecay_state_paths_are_recognised() {
        assert!(is_tracedecay_owned_state_path(".tracedecay"));
        assert!(is_tracedecay_owned_state_path(
            ".tracedecay/enrollment.json"
        ));
        assert!(is_tracedecay_owned_state_path(
            ".tracedecay/code-index/generation-0.json"
        ));
        assert!(!is_tracedecay_owned_state_path(".tracedecay-notes/a.rs"));
        assert!(!is_tracedecay_owned_state_path("src/.tracedecay/a.rs"));
        assert!(!is_tracedecay_owned_state_path("src/lib.rs"));
    }

    /// Enrolling a project writes `.tracedecay/enrollment.json` into the
    /// checkout. That is `TraceDecay`'s own state, so a checkout that is
    /// otherwise clean must stay classified clean — otherwise no capture can
    /// seal an exact HEAD tree and exact-scope admission refuses forever with
    /// `lsp-code-index-source-revision-unavailable`.
    #[test]
    fn enrollment_state_does_not_make_a_committed_checkout_dirty() {
        let repo = committed_repo();
        let repository = gix::open(repo.path()).expect("open repository");
        assert!(
            WorktreeChangeClassificationV1::classify(&repository)
                .expect("classify committed checkout")
                .changes()
                .is_empty(),
            "a committed checkout starts clean"
        );

        std::fs::create_dir_all(repo.path().join(".tracedecay")).unwrap();
        std::fs::write(
            repo.path().join(".tracedecay/enrollment.json"),
            "{\"project_id\":\"project.test\",\"storage_mode\":\"profile_sharded\"}\n",
        )
        .unwrap();

        let classification = WorktreeChangeClassificationV1::classify(&repository)
            .expect("classify enrolled checkout");
        assert!(
            classification.changes().is_empty(),
            "TraceDecay's own private store is not a worktree change: {:?}",
            classification.changes()
        );
        assert!(
            !classification
                .candidate_paths()
                .iter()
                .any(|path| path.starts_with(".tracedecay/")),
            "TraceDecay's own private store is never an indexing candidate"
        );
    }

    /// The exclusion is scoped to `TraceDecay`'s own directory only; real
    /// untracked source still classifies the worktree as dirty.
    #[test]
    fn untracked_source_still_classifies_as_a_change() {
        let repo = committed_repo();
        std::fs::create_dir_all(repo.path().join(".tracedecay")).unwrap();
        std::fs::write(repo.path().join(".tracedecay/enrollment.json"), "{}\n").unwrap();
        std::fs::write(repo.path().join("src/extra.rs"), "pub fn b() {}\n").unwrap();

        let repository = gix::open(repo.path()).expect("open repository");
        let classification =
            WorktreeChangeClassificationV1::classify(&repository).expect("classify");
        assert_eq!(
            classification
                .changed_paths()
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["src/extra.rs".to_owned()],
            "only the untracked source file is a change"
        );
    }
}
