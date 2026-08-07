//! Single read-only Git repository authority.
//!
//! Repository topology, refs, HEAD, object format, operation state, status,
//! and bounded history are read through `gix`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice as _;
use tracedecay_domain::git::{
    GitChangeKindV1, GitDegradationV1, GitFileModeV1, GitHeadStateV1, GitObjectFormatV1, GitOidV1,
    GitOperationStateV1, GitStatusEntryV1, GitTrackedStatusV1,
};

mod history;
mod native_integration;
pub use history::{
    GitHistoryBudget, GitHistoryOptions, GitHistoryTermination, GitRepositoryHistory,
};
pub use native_integration::{
    GitNativeApplyOutcome, GitNativeIntegrationMode, GitNativePreflight,
    GitNativePreflightDisposition, GitNativeUnsupportedReason,
};

/// A typed failure from the in-process Git repository authority.
#[derive(Debug, thiserror::Error)]
pub enum GitRepositoryError {
    #[error("not a Git repository: {path}")]
    NotARepository { path: String },
    #[error("Git repository at {path} is unreadable: {detail}")]
    UnreadableRepository { path: String, detail: String },
    #[error("Git HEAD is unreadable: {detail}")]
    UnreadableHead { detail: String },
    #[error("Git repository {operation} failed: {detail}")]
    Operation {
        operation: &'static str,
        detail: String,
    },
    #[error(transparent)]
    Domain(#[from] tracedecay_domain::research::DomainError),
}

/// One resolved reference and its direct object target, if it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitReference {
    pub name: String,
    pub target: Option<GitOidV1>,
    pub symbolic_target: Option<String>,
}

/// Repository status without application-specific repository identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRepositoryStatus {
    pub head: GitHeadStateV1,
    pub operation: GitOperationStateV1,
    pub entries: Vec<GitStatusEntryV1>,
    pub degradations: BTreeSet<GitDegradationV1>,
}

/// One thread-safe `gix` repository authority.
#[derive(Debug)]
pub struct GitRepositoryAuthority {
    repository: gix::ThreadSafeRepository,
    worktree_root: Option<PathBuf>,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl GitRepositoryAuthority {
    /// Discover the repository containing `path`.
    pub fn discover(path: &Path) -> Result<Self, GitRepositoryError> {
        let repository = gix::discover_opts(
            path,
            gix::discover::upwards::Options::default(),
            repository_open_options(),
        )
        .map_err(|error| match error {
            gix::discover::Error::Discover(gix::discover::upwards::Error::NoGitRepository {
                ..
            }) => GitRepositoryError::NotARepository {
                path: path.display().to_string(),
            },
            error => GitRepositoryError::UnreadableRepository {
                path: path.display().to_string(),
                detail: error.to_string(),
            },
        })?;
        let worktree_root = repository
            .workdir()
            .map(|path| canonical(path, "worktree root"))
            .transpose()
            .map_err(|error| repository_error(path, error))?;
        let git_dir = canonical(repository.git_dir(), "Git directory")
            .map_err(|error| repository_error(path, error))?;
        let common_dir = canonical(repository.common_dir(), "Git common directory")
            .map_err(|error| repository_error(path, error))?;
        Ok(Self {
            repository: repository.into_sync(),
            worktree_root,
            git_dir,
            common_dir,
        })
    }

    /// Exact per-worktree checkout root, absent for bare repositories.
    pub fn worktree_root(&self) -> Option<&Path> {
        self.worktree_root.as_deref()
    }

    /// Exact per-worktree Git directory.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Shared repository common directory.
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// Repository object format from parsed Git configuration.
    pub fn object_format(&self) -> Result<GitObjectFormatV1, GitRepositoryError> {
        match self.repository.to_thread_local().object_hash() {
            gix::hash::Kind::Sha1 => Ok(GitObjectFormatV1::Sha1),
            gix::hash::Kind::Sha256 => Ok(GitObjectFormatV1::Sha256),
            format => Err(GitRepositoryError::Operation {
                operation: "object format",
                detail: format!("unsupported object format {format}"),
            }),
        }
    }

    /// Exact HEAD state for this repository or linked worktree.
    pub fn head(&self) -> Result<GitHeadStateV1, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        head_from_gix(&repository)
    }

    /// All ordinary repository refs in stable name order.
    pub fn references(&self) -> Result<Vec<GitReference>, GitRepositoryError> {
        let repository = self.repository.to_thread_local();
        let platform = repository
            .references()
            .map_err(|error| operation("references", error))?;
        let iter = platform
            .all()
            .map_err(|error| operation("references", error))?;
        let mut references = Vec::new();
        for reference in iter {
            let reference = reference.map_err(|error| operation("references", error))?;
            let target = reference.target();
            references.push(GitReference {
                name: reference.name().as_bstr().to_string(),
                target: target
                    .try_id()
                    .map(|target| GitOidV1::new(target.to_string()))
                    .transpose()?,
                symbolic_target: target.try_name().map(|name| name.as_bstr().to_string()),
            });
        }
        references.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(references)
    }

    /// Parsed in-progress operation state.
    pub fn operation_state(&self) -> GitOperationStateV1 {
        use gix::state::InProgress;

        match self.repository.to_thread_local().state() {
            None => GitOperationStateV1::None,
            Some(InProgress::Merge) => GitOperationStateV1::Merge,
            Some(
                InProgress::ApplyMailbox
                | InProgress::ApplyMailboxRebase
                | InProgress::Rebase
                | InProgress::RebaseInteractive,
            ) => GitOperationStateV1::Rebase,
            Some(InProgress::CherryPick | InProgress::CherryPickSequence) => {
                GitOperationStateV1::CherryPick
            }
            Some(InProgress::Revert | InProgress::RevertSequence) => GitOperationStateV1::Revert,
            Some(InProgress::Bisect) => GitOperationStateV1::Bisect,
        }
    }

    /// Live staged, unstaged, untracked, ignored, conflict, and submodule
    /// status directly from the current index and working tree.
    pub fn status(&self) -> Result<GitRepositoryStatus, GitRepositoryError> {
        use gix::diff::index::ChangeRef;
        use gix::dir::entry::Status as DirectoryStatus;
        use gix::status::Item;
        use gix::status::index_worktree::Item as IndexWorktreeItem;
        use gix::status::plumbing::index_as_worktree::{Change as WorktreeChange, EntryStatus};

        let repository = self.repository.to_thread_local();
        let mut platform = repository
            .status(gix::progress::Discard)
            .map_err(|error| operation("status", error))?
            .untracked_files(gix::status::UntrackedFiles::Files)
            .index_worktree_rewrites(None);
        platform.dirwalk_options_mut(|options| {
            options.set_emit_ignored(Some(gix::dir::walk::EmissionMode::Matching));
        });
        let status = platform
            .into_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|error| operation("status", error))?;

        let mut tracked = BTreeMap::<String, TrackedStatusBuilder>::new();
        let mut loose = BTreeMap::<String, GitStatusEntryV1>::new();
        for item in status {
            match item.map_err(|error| operation("status", error))? {
                Item::TreeIndex(change) => match change {
                    ChangeRef::Addition {
                        location,
                        entry_mode,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(GitChangeKindV1::Added, None, Some(mode(entry_mode)?), None);
                    }
                    ChangeRef::Deletion {
                        location,
                        entry_mode,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(
                                GitChangeKindV1::Deleted,
                                Some(mode(entry_mode)?),
                                None,
                                None,
                            );
                    }
                    ChangeRef::Modification {
                        location,
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(
                                GitChangeKindV1::Modified,
                                Some(mode(previous_entry_mode)?),
                                Some(mode(entry_mode)?),
                                None,
                            );
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        source_entry_mode,
                        location,
                        entry_mode,
                        copy,
                        ..
                    } => {
                        let path = path_text(location.as_ref(), "status")?;
                        let source = path_text(source_location.as_ref(), "status")?;
                        tracked
                            .entry(path.clone())
                            .or_insert_with(|| TrackedStatusBuilder::new(path))
                            .set_index(
                                if copy {
                                    GitChangeKindV1::Copied
                                } else {
                                    GitChangeKindV1::Renamed
                                },
                                Some(mode(source_entry_mode)?),
                                Some(mode(entry_mode)?),
                                Some(source),
                            );
                    }
                },
                Item::IndexWorktree(worktree) => match worktree {
                    IndexWorktreeItem::Modification {
                        entry,
                        rela_path,
                        status,
                        ..
                    } => {
                        let path = path_text(rela_path.as_ref(), "status")?;
                        match status {
                            EntryStatus::NeedsUpdate(_) => {}
                            // Porcelain reports `git add --intent-to-add` as a
                            // tracked entry with a worktree-side addition
                            // (` A`), never as `??` untracked.
                            EntryStatus::IntentToAdd => {
                                let builder = tracked
                                    .entry(path.clone())
                                    .or_insert_with(|| TrackedStatusBuilder::new(path));
                                builder.worktree = GitChangeKindV1::Added;
                                builder.index_mode = Some(mode(entry.mode)?);
                                builder.worktree_mode =
                                    worktree_mode(self.worktree_root.as_deref(), &builder.path)?;
                            }
                            EntryStatus::Conflict { entries, .. } => {
                                let builder = tracked
                                    .entry(path.clone())
                                    .or_insert_with(|| TrackedStatusBuilder::new(path));
                                builder.index = GitChangeKindV1::Unmerged;
                                builder.worktree = GitChangeKindV1::Unmerged;
                                builder.index_mode = entries
                                    .iter()
                                    .flatten()
                                    .next()
                                    .map(|entry| mode(entry.mode))
                                    .transpose()?;
                                builder.worktree_mode =
                                    worktree_mode(self.worktree_root.as_deref(), &builder.path)?;
                            }
                            EntryStatus::Change(change) => {
                                let builder = tracked
                                    .entry(path.clone())
                                    .or_insert_with(|| TrackedStatusBuilder::new(path));
                                if builder.index_mode.is_none() {
                                    builder.index_mode = Some(mode(entry.mode)?);
                                }
                                if builder.head_mode.is_none() {
                                    builder.head_mode = Some(mode(entry.mode)?);
                                }
                                builder.submodule |= entry.mode.is_submodule();
                                match change {
                                    WorktreeChange::Removed => {
                                        builder.worktree = GitChangeKindV1::Deleted;
                                        builder.worktree_mode = None;
                                    }
                                    WorktreeChange::Type { worktree_mode } => {
                                        builder.worktree = GitChangeKindV1::TypeChanged;
                                        builder.worktree_mode = Some(mode(worktree_mode)?);
                                    }
                                    WorktreeChange::Modification { .. } => {
                                        builder.worktree = GitChangeKindV1::Modified;
                                        builder.worktree_mode = worktree_mode(
                                            self.worktree_root.as_deref(),
                                            &builder.path,
                                        )?;
                                    }
                                    WorktreeChange::SubmoduleModification(_) => {
                                        builder.worktree = GitChangeKindV1::Modified;
                                        builder.worktree_mode = Some(mode(entry.mode)?);
                                        builder.submodule = true;
                                    }
                                }
                            }
                        }
                    }
                    IndexWorktreeItem::DirectoryContents { entry, .. } => {
                        let path = path_text(entry.rela_path.as_ref(), "status")?;
                        match entry.status {
                            DirectoryStatus::Ignored(_) => {
                                loose.insert(path.clone(), GitStatusEntryV1::Ignored { path });
                            }
                            DirectoryStatus::Untracked => {
                                loose.insert(path.clone(), GitStatusEntryV1::Untracked { path });
                            }
                            DirectoryStatus::Pruned | DirectoryStatus::Tracked => {}
                        }
                    }
                    IndexWorktreeItem::Rewrite { .. } => {}
                },
            }
        }

        for path in tracked.keys() {
            loose.remove(path);
        }
        let mut entries = tracked
            .into_values()
            .map(TrackedStatusBuilder::finish)
            .map(GitStatusEntryV1::Tracked)
            .collect::<Vec<_>>();
        entries.extend(loose.into_values());
        entries.sort_by(|left, right| left.path().cmp(right.path()));

        let head = self.head()?;
        let op_state = self.operation_state();
        let mut degradations = self.degradations(&repository, &head, op_state);
        if entries
            .iter()
            .any(|entry| matches!(entry, GitStatusEntryV1::Tracked(value) if value.is_conflicted()))
        {
            degradations.insert(GitDegradationV1::ConflictedState);
        }
        if entries
            .iter()
            .any(|entry| matches!(entry, GitStatusEntryV1::Tracked(value) if value.submodule))
        {
            degradations.insert(GitDegradationV1::SubmoduleState);
        }
        if has_ignored_collision(&entries) {
            degradations.insert(GitDegradationV1::IgnoredCollision);
        }
        Ok(GitRepositoryStatus {
            head,
            operation: op_state,
            entries,
            degradations,
        })
    }

    fn degradations(
        &self,
        repository: &gix::Repository,
        head: &GitHeadStateV1,
        operation: GitOperationStateV1,
    ) -> BTreeSet<GitDegradationV1> {
        let mut degradations = BTreeSet::new();
        match head {
            GitHeadStateV1::Detached { .. } => {
                degradations.insert(GitDegradationV1::DetachedHead);
            }
            GitHeadStateV1::Unborn { .. } => {
                degradations.insert(GitDegradationV1::UnbornBranch);
            }
            GitHeadStateV1::Attached { .. } => {}
        }
        if operation != GitOperationStateV1::None {
            degradations.insert(GitDegradationV1::InProgressOperation);
        }
        if repository
            .config_snapshot()
            .boolean("core.sparseCheckout")
            .unwrap_or(false)
        {
            degradations.insert(GitDegradationV1::SparseCheckout);
        }
        if std::fs::read_dir(&self.git_dir).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sharedindex.")
            })
        }) {
            degradations.insert(GitDegradationV1::SplitIndex);
        }
        if self
            .worktree_root
            .as_ref()
            .is_some_and(|root| root.join(".gitmodules").is_file())
        {
            degradations.insert(GitDegradationV1::SubmoduleState);
        }
        degradations
    }
}

#[derive(Debug)]
struct TrackedStatusBuilder {
    path: String,
    original_path: Option<String>,
    index: GitChangeKindV1,
    worktree: GitChangeKindV1,
    head_mode: Option<GitFileModeV1>,
    index_mode: Option<GitFileModeV1>,
    worktree_mode: Option<GitFileModeV1>,
    submodule: bool,
}

impl TrackedStatusBuilder {
    fn new(path: String) -> Self {
        Self {
            path,
            original_path: None,
            index: GitChangeKindV1::Unmodified,
            worktree: GitChangeKindV1::Unmodified,
            head_mode: None,
            index_mode: None,
            worktree_mode: None,
            submodule: false,
        }
    }

    fn set_index(
        &mut self,
        change: GitChangeKindV1,
        head_mode: Option<GitFileModeV1>,
        index_mode: Option<GitFileModeV1>,
        original_path: Option<String>,
    ) {
        self.index = change;
        self.head_mode = head_mode;
        self.worktree_mode.clone_from(&index_mode);
        self.index_mode = index_mode;
        self.original_path = original_path;
        self.submodule = self
            .index_mode
            .as_ref()
            .or(self.head_mode.as_ref())
            .is_some_and(GitFileModeV1::is_submodule);
    }

    fn finish(self) -> GitTrackedStatusV1 {
        GitTrackedStatusV1 {
            path: self.path,
            original_path: self.original_path,
            index: self.index,
            worktree: self.worktree,
            head_mode: self.head_mode,
            index_mode: self.index_mode,
            worktree_mode: self.worktree_mode,
            submodule: self.submodule,
        }
    }
}

/// Preserve the repository's normal configuration and attribute semantics
/// while rejecting `GIT_*` redirection from the daemon environment.
fn repository_open_options() -> gix::open::Options {
    let mut permissions = gix::open::Permissions::secure();
    permissions.env.git_prefix = gix::sec::Permission::Deny;
    permissions.env.objects = gix::sec::Permission::Deny;
    permissions.config.env = false;
    gix::open::Options::default().permissions(permissions)
}

fn head_from_gix(repository: &gix::Repository) -> Result<GitHeadStateV1, GitRepositoryError> {
    let head = repository
        .head()
        .map_err(|error| GitRepositoryError::UnreadableHead {
            detail: error.to_string(),
        })?;
    let branch = head
        .referent_name()
        .and_then(|name| name.as_bstr().to_str().ok())
        .and_then(|name| name.strip_prefix("refs/heads/"))
        .map(str::to_owned);
    match (head.id(), branch) {
        (Some(commit), Some(branch)) => Ok(GitHeadStateV1::Attached {
            branch,
            commit: GitOidV1::new(commit.to_string())?,
        }),
        (Some(commit), None) => Ok(GitHeadStateV1::Detached {
            commit: GitOidV1::new(commit.to_string())?,
        }),
        (None, Some(branch)) => Ok(GitHeadStateV1::Unborn { branch }),
        (None, None) => Err(GitRepositoryError::UnreadableHead {
            detail: "HEAD has neither a commit nor a branch".to_owned(),
        }),
    }
}

fn repository_error(path: &Path, error: impl std::fmt::Display) -> GitRepositoryError {
    GitRepositoryError::UnreadableRepository {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

fn path_text(
    path: &gix::bstr::BStr,
    operation_name: &'static str,
) -> Result<String, GitRepositoryError> {
    path.to_str()
        .map(str::to_owned)
        .map_err(|error| GitRepositoryError::Operation {
            operation: operation_name,
            detail: error.to_string(),
        })
}

fn mode(mode: gix::index::entry::Mode) -> Result<GitFileModeV1, GitRepositoryError> {
    GitFileModeV1::new(format!("{:06o}", mode.bits())).map_err(Into::into)
}

fn worktree_mode(
    root: Option<&Path>,
    path: &str,
) -> Result<Option<GitFileModeV1>, GitRepositoryError> {
    let Some(root) = root else {
        return Ok(None);
    };
    let metadata = match std::fs::symlink_metadata(root.join(path)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(operation("status worktree mode", error)),
    };
    let value = if metadata.file_type().is_symlink() {
        GitFileModeV1::SYMLINK
    } else if metadata.is_dir() {
        GitFileModeV1::GITLINK
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o111 != 0 {
                GitFileModeV1::EXECUTABLE
            } else {
                GitFileModeV1::REGULAR
            }
        }
        #[cfg(not(unix))]
        {
            GitFileModeV1::REGULAR
        }
    };
    GitFileModeV1::new(value).map(Some).map_err(Into::into)
}

fn has_ignored_collision(entries: &[GitStatusEntryV1]) -> bool {
    let ignored = entries
        .iter()
        .filter_map(|entry| match entry {
            GitStatusEntryV1::Ignored { path } => Some(path.trim_end_matches('/')),
            _ => None,
        })
        .collect::<Vec<_>>();
    entries.iter().any(|entry| {
        let path = match entry {
            GitStatusEntryV1::Ignored { .. } => return false,
            _ => entry.path(),
        };
        ignored.iter().any(|ignored_path| {
            parent_dir(ignored_path) == parent_dir(path)
                || path.starts_with(&format!("{ignored_path}/"))
                || (!parent_dir(path).is_empty()
                    && ignored_path.starts_with(&format!("{}/", parent_dir(path))))
        })
    })
}

fn parent_dir(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn canonical(path: &Path, operation_name: &'static str) -> Result<PathBuf, GitRepositoryError> {
    path.canonicalize()
        .map_err(|error| operation(operation_name, error))
}

fn operation(operation: &'static str, error: impl std::fmt::Display) -> GitRepositoryError {
    GitRepositoryError::Operation {
        operation,
        detail: error.to_string(),
    }
}
