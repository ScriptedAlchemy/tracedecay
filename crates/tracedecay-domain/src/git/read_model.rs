//! Read-only native Git intelligence contracts (Plan 36, QUERY).
//!
//! These are pure typed values for repository status, working/staged/range
//! diff, bounded history, blame/line provenance, and `HunkRef` identity.
//! Native Git remains the authority for repository objects, refs, the index,
//! and the working tree; capture happens outside this crate through a fixed
//! read-only adapter. Nothing in this module grants mutation authority:
//! there are no staging, apply, index-transaction, ref-update, config, or
//! worktree-mutation types here. Unsupported or degraded repository states
//! (ignored collision, conflicted, detached, unborn, sparse, split-index,
//! submodule) are represented explicitly through [`GitCoverageV1`] rather
//! than guessed.

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::time::UtcMicros;
use crate::research::{DomainError, ManifestDigest, RepositoryId};

/// Schema/domain separator for the independently hashed `HunkRefV1` identity
/// (Plan 36, "`HunkRef` compare-and-swap contract").
pub const HUNK_REF_DIGEST_DOMAIN: &str = "tracedecay.git.hunkref.v1";

/// Schema version pinned into every minted `HunkRefV1`.
pub const HUNK_REF_SCHEMA_VERSION_V1: &str = "hunkref.v1";

/// Repository object format, derived from object-id length.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitObjectFormatV1 {
    Sha1,
    Sha256,
}

impl GitObjectFormatV1 {
    pub const fn oid_hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}

fn validate_git_oid(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if !crate::canonical_text::is_git_object_id(value) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// A native Git object id (commit, tree, or blob), lowercase hex, SHA-1 or
/// SHA-256 length. This is identity evidence only; it never authorizes
/// object reconstruction or traversal outside native Git.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct GitOidV1(String);

impl GitOidV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_git_oid(&value, "GitOidV1")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub const fn format(&self) -> GitObjectFormatV1 {
        if self.0.len() == 64 {
            GitObjectFormatV1::Sha256
        } else {
            GitObjectFormatV1::Sha1
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_git_oid(&self.0, "GitOidV1")
    }
}

impl<'de> Deserialize<'de> for GitOidV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for GitOidV1 {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::fmt::Display for GitOidV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_file_mode(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    let valid = value.len() == 6 && value.bytes().all(|byte| (b'0'..=b'7').contains(&byte));
    if !valid {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// A native Git file mode as stored in tree/index records (six octal digits,
/// e.g. `100644`, `100755`, `120000` symlink, `160000` gitlink/submodule).
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct GitFileModeV1(String);

impl GitFileModeV1 {
    pub const REGULAR: &'static str = "100644";
    pub const EXECUTABLE: &'static str = "100755";
    pub const SYMLINK: &'static str = "120000";
    pub const GITLINK: &'static str = "160000";

    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_file_mode(&value, "GitFileModeV1")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_submodule(&self) -> bool {
        self.0 == Self::GITLINK
    }

    pub fn is_symlink(&self) -> bool {
        self.0 == Self::SYMLINK
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_file_mode(&self.0, "GitFileModeV1")
    }
}

impl<'de> Deserialize<'de> for GitFileModeV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for GitFileModeV1 {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::fmt::Display for GitFileModeV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Native HEAD state. Missing, unborn, and detached states are explicit,
/// never guessed (Plan 36, PR7 provenance rule carried into query reads).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GitHeadStateV1 {
    Attached { branch: String, commit: GitOidV1 },
    Detached { commit: GitOidV1 },
    Unborn { branch: String },
}

impl GitHeadStateV1 {
    pub fn commit(&self) -> Option<&GitOidV1> {
        match self {
            Self::Attached { commit, .. } | Self::Detached { commit } => Some(commit),
            Self::Unborn { .. } => None,
        }
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Attached { branch, .. } | Self::Unborn { branch } => Some(branch),
            Self::Detached { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Attached { branch, commit } => {
                validate_path_label(branch, "head branch")?;
                commit.validate()
            }
            Self::Detached { commit } => commit.validate(),
            Self::Unborn { branch } => validate_path_label(branch, "head branch"),
        }
    }
}

/// In-progress native Git operation state, read from repository metadata.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum GitOperationStateV1 {
    #[default]
    None,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Sequencer,
    Unknown,
}

/// Typed coverage/degradation reasons for a read-only Git result. A result
/// carrying any degradation is truthful but not complete; callers must not
/// treat it as a clean full view.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitDegradationV1 {
    /// Ignored content shares a directory with live tracked/untracked
    /// entries, so the untracked/ignored view may be collapsed by Git.
    IgnoredCollision,
    /// Unmerged index stages are present.
    ConflictedState,
    DetachedHead,
    UnbornBranch,
    SparseCheckout,
    SplitIndex,
    /// Submodule entries exist; the adapter does not recurse into them.
    SubmoduleState,
    UnreadableState,
    UnsupportedObjectFormat,
    InProgressOperation,
    ShallowBoundary,
    TruncatedOutput,
}

/// Typed coverage of a read-only Git result: the sorted, de-duplicated set
/// of degradations observed while capturing it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitCoverageV1 {
    pub degradations: Vec<GitDegradationV1>,
}

impl GitCoverageV1 {
    pub fn complete() -> Self {
        Self::default()
    }

    pub fn degraded(mut degradations: Vec<GitDegradationV1>) -> Self {
        degradations.sort_unstable();
        degradations.dedup();
        Self { degradations }
    }

    pub fn is_complete(&self) -> bool {
        self.degradations.is_empty()
    }

    /// Whether any recorded degradation means state was left unread.
    ///
    /// `IgnoredCollision` only records that Git may collapse the untracked and
    /// ignored view when ignored content shares a directory with live entries.
    /// Tracked entries, the index tree, and the index checksum are all still
    /// captured exactly, so it is not evidence that a read failed. Counting it
    /// as one made every index transaction ineligible in any repository that
    /// keeps an ignored directory beside tracked files — `target/`,
    /// `node_modules/`, `.tracedecay/`.
    pub fn leaves_state_unread(&self) -> bool {
        self.degradations
            .iter()
            .any(|degradation| *degradation != GitDegradationV1::IgnoredCollision)
    }

    pub fn records(&self, degradation: GitDegradationV1) -> bool {
        self.degradations.contains(&degradation)
    }

    pub fn record(&mut self, degradation: GitDegradationV1) {
        if !self.records(degradation) {
            self.degradations.push(degradation);
            self.degradations.sort_unstable();
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let mut sorted = self.degradations.clone();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted != self.degradations {
            return Err(DomainError::NonCanonical {
                field: "git coverage degradations",
            });
        }
        Ok(())
    }
}

pub(super) fn validate_path_label(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if !crate::canonical_text::is_canonical_text(value) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// Native change kind for one side (index or worktree) of a status entry,
/// or for a whole-file diff record.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKindV1 {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unmerged,
}

/// One tracked status record (porcelain v2 ordinary, rename, or unmerged
/// entry). `index` is the staged (HEAD→index) side; `worktree` is the
/// unstaged (index→worktree) side.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitTrackedStatusV1 {
    pub path: String,
    /// Source path for a rename or copy.
    pub original_path: Option<String>,
    pub index: GitChangeKindV1,
    pub worktree: GitChangeKindV1,
    /// Native tree/index/worktree modes emitted by porcelain v2. A missing
    /// side is represented as `None`, never reconstructed from the path.
    pub head_mode: Option<GitFileModeV1>,
    pub index_mode: Option<GitFileModeV1>,
    pub worktree_mode: Option<GitFileModeV1>,
    /// True when the entry is a gitlink (submodule) record.
    pub submodule: bool,
}

impl GitTrackedStatusV1 {
    pub fn is_conflicted(&self) -> bool {
        self.index == GitChangeKindV1::Unmerged || self.worktree == GitChangeKindV1::Unmerged
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_path_label(&self.path, "status path")?;
        if let Some(original) = &self.original_path {
            validate_path_label(original, "status original path")?;
        }
        for mode in [
            self.head_mode.as_ref(),
            self.index_mode.as_ref(),
            self.worktree_mode.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            mode.validate()?;
        }
        Ok(())
    }
}

/// One status entry: a tracked record with staged/unstaged sides, an
/// untracked path, or an ignored path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitStatusEntryV1 {
    Tracked(GitTrackedStatusV1),
    Untracked { path: String },
    Ignored { path: String },
}

impl GitStatusEntryV1 {
    pub fn path(&self) -> &str {
        match self {
            Self::Tracked(tracked) => &tracked.path,
            Self::Untracked { path } | Self::Ignored { path } => path,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Tracked(tracked) => tracked.validate(),
            Self::Untracked { path } => validate_path_label(path, "untracked path"),
            Self::Ignored { path } => validate_path_label(path, "ignored path"),
        }
    }
}

/// Typed repository status: HEAD state, in-progress operation, every
/// staged/unstaged/untracked/ignored/renamed/conflicted/submodule entry,
/// and explicit coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitStatusV1 {
    pub repository: RepositoryId,
    pub head: GitHeadStateV1,
    pub operation: GitOperationStateV1,
    pub entries: Vec<GitStatusEntryV1>,
    pub coverage: GitCoverageV1,
}

impl GitStatusV1 {
    fn tracked_entries(&self) -> impl Iterator<Item = &GitTrackedStatusV1> {
        self.entries.iter().filter_map(|entry| match entry {
            GitStatusEntryV1::Tracked(tracked) => Some(tracked),
            _ => None,
        })
    }

    pub fn staged_count(&self) -> usize {
        self.tracked_entries()
            .filter(|entry| {
                !matches!(
                    entry.index,
                    GitChangeKindV1::Unmodified | GitChangeKindV1::Unmerged
                )
            })
            .count()
    }

    pub fn unstaged_count(&self) -> usize {
        self.tracked_entries()
            .filter(|entry| {
                !matches!(
                    entry.worktree,
                    GitChangeKindV1::Unmodified | GitChangeKindV1::Unmerged
                )
            })
            .count()
    }

    pub fn conflicted_count(&self) -> usize {
        self.tracked_entries()
            .filter(|entry| entry.is_conflicted())
            .count()
    }

    pub fn untracked_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, GitStatusEntryV1::Untracked { .. }))
            .count()
    }

    pub fn ignored_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| matches!(entry, GitStatusEntryV1::Ignored { .. }))
            .count()
    }

    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.head.validate()?;
        self.coverage.validate()?;
        let mut paths: Vec<&str> = self.entries.iter().map(GitStatusEntryV1::path).collect();
        paths.sort_unstable();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DomainError::DuplicateId {
                field: "status entry path",
            });
        }
        for entry in &self.entries {
            entry.validate()?;
        }
        Ok(())
    }
}

/// Diff scope: unstaged worktree changes, staged index changes, or an exact
/// commit range. Range diffs are read-only evidence and carry no index
/// relationship, so they cannot mint an applicable `HunkRefV1`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum GitDiffScopeV1 {
    WorkingTree,
    Staged,
    CommitRange { base: GitOidV1, head: GitOidV1 },
}

/// One structured diff hunk. The hunk body is not retained; `patch_digest`
/// is the canonical digest of the normalized header plus body lines, which
/// is the stable hunk identity evidence (Plan 36 bounded-result rule).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitHunkV1 {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// Function/section heading from the hunk header when Git emitted one.
    pub section: Option<String>,
    pub patch_digest: ManifestDigest,
}

impl GitHunkV1 {
    /// Normalized `@@ -o,l +n,m @@` header text (counts always explicit).
    pub fn normalized_header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_lines, self.new_start, self.new_lines
        )
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        // Git addresses a zero-length side by the line after which content is
        // inserted (0 for the top of the file); a non-empty side starts at 1.
        if self.old_lines > 0 && self.old_start == 0 {
            return Err(DomainError::NonCanonical {
                field: "hunk old range",
            });
        }
        if self.new_lines > 0 && self.new_start == 0 {
            return Err(DomainError::NonCanonical {
                field: "hunk new range",
            });
        }
        Ok(())
    }
}

/// One file's structured diff record: change kind, modes, blob identities,
/// binary/submodule classification, bounded line totals, and hunks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitFileDiffV1 {
    pub path: String,
    /// Source path for a rename or copy.
    pub original_path: Option<String>,
    pub change: GitChangeKindV1,
    pub old_mode: Option<GitFileModeV1>,
    pub new_mode: Option<GitFileModeV1>,
    pub old_blob: Option<GitOidV1>,
    pub new_blob: Option<GitOidV1>,
    pub binary: bool,
    /// True when the entry is a gitlink (submodule) change.
    pub submodule: bool,
    /// Inserted/deleted line totals; absent for binary and submodule records.
    pub insertions: Option<u32>,
    pub deletions: Option<u32>,
    pub hunks: Vec<GitHunkV1>,
}

impl GitFileDiffV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_path_label(&self.path, "diff path")?;
        if let Some(original) = &self.original_path {
            validate_path_label(original, "diff original path")?;
        }
        let is_rename_like = matches!(
            self.change,
            GitChangeKindV1::Renamed | GitChangeKindV1::Copied
        );
        if self.original_path.is_some() != is_rename_like {
            return Err(DomainError::NonCanonical {
                field: "diff original path",
            });
        }
        if (self.binary || self.submodule) && !self.hunks.is_empty() {
            return Err(DomainError::NonCanonical {
                field: "binary or submodule diff hunks",
            });
        }
        if (self.binary || self.submodule)
            != (self.insertions.is_none() && self.deletions.is_none())
        {
            return Err(DomainError::NonCanonical {
                field: "diff line totals",
            });
        }
        for hunk in &self.hunks {
            hunk.validate()?;
        }
        Ok(())
    }
}

/// Typed diff result for one scope with explicit coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitDiffV1 {
    pub repository: RepositoryId,
    pub scope: GitDiffScopeV1,
    pub files: Vec<GitFileDiffV1>,
    pub coverage: GitCoverageV1,
}

impl GitDiffV1 {
    pub fn files_changed(&self) -> usize {
        self.files.len()
    }

    pub fn insertions(&self) -> u64 {
        self.files
            .iter()
            .filter_map(|f| f.insertions)
            .map(u64::from)
            .sum()
    }

    pub fn deletions(&self) -> u64 {
        self.files
            .iter()
            .filter_map(|f| f.deletions)
            .map(u64::from)
            .sum()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.coverage.validate()?;
        let mut paths: Vec<&str> = self.files.iter().map(|file| file.path.as_str()).collect();
        paths.sort_unstable();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DomainError::DuplicateId {
                field: "diff file path",
            });
        }
        for file in &self.files {
            file.validate()?;
        }
        Ok(())
    }
}

/// Author/committer identity and timestamp evidence for one commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitCommitIdentityV1 {
    pub name: String,
    pub email: String,
    pub at: UtcMicros,
}

/// Bounded commit metadata. The full message is not retained;
/// `message_digest` is its canonical digest evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitCommitMetadataV1 {
    pub commit: GitOidV1,
    pub tree: GitOidV1,
    pub parents: Vec<GitOidV1>,
    pub author: GitCommitIdentityV1,
    pub committer: GitCommitIdentityV1,
    /// First line of the commit message, bounded at capture.
    pub subject: String,
    pub message_digest: ManifestDigest,
}

/// Bounded commit history in native traversal order. `truncated` is true
/// when the capture bound cut the walk; shallow/partial-clone boundaries are
/// coverage degradations, never silently clean.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHistoryV1 {
    pub repository: RepositoryId,
    pub commits: Vec<GitCommitMetadataV1>,
    pub truncated: bool,
    pub coverage: GitCoverageV1,
}

impl GitHistoryV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.coverage.validate()?;
        let mut commits: Vec<&GitOidV1> =
            self.commits.iter().map(|commit| &commit.commit).collect();
        commits.sort_unstable();
        if commits.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DomainError::DuplicateId {
                field: "history commit",
            });
        }
        Ok(())
    }
}

/// Why blame/line provenance is unavailable for a path.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitBlameAvailabilityV1 {
    Available,
    PathNotTracked,
    UnbornBranch,
    BinaryFile,
}

/// Rename-following evidence for one blamed line (`previous` record).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitBlamePreviousV1 {
    pub commit: GitOidV1,
    pub path: String,
}

/// Line provenance for one final (current) line.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitBlameLineV1 {
    /// 1-based line number in the blamed revision.
    pub final_line: u32,
    /// 1-based line number in the origin commit.
    pub origin_line: u32,
    pub commit: GitOidV1,
    pub author: GitCommitIdentityV1,
    /// True when the origin commit is a history boundary (e.g. shallow root).
    pub boundary: bool,
    pub previous: Option<GitBlamePreviousV1>,
}

impl GitBlameLineV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.final_line == 0 || self.origin_line == 0 {
            return Err(DomainError::NonCanonical {
                field: "blame line number",
            });
        }
        if let Some(previous) = &self.previous {
            validate_path_label(&previous.path, "blame previous path")?;
        }
        Ok(())
    }
}

/// Typed blame result: per-line provenance plus boundary, rename-following,
/// and unavailable states.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitBlameV1 {
    pub repository: RepositoryId,
    pub path: String,
    pub lines: Vec<GitBlameLineV1>,
    pub availability: GitBlameAvailabilityV1,
    pub coverage: GitCoverageV1,
}

impl GitBlameV1 {
    pub fn is_available(&self) -> bool {
        self.availability == GitBlameAvailabilityV1::Available
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_path_label(&self.path, "blame path")?;
        self.coverage.validate()?;
        if self.availability != GitBlameAvailabilityV1::Available && !self.lines.is_empty() {
            return Err(DomainError::NonCanonical {
                field: "blame availability lines",
            });
        }
        for pair in self.lines.windows(2) {
            if pair[0].final_line >= pair[1].final_line {
                return Err(DomainError::NonCanonical {
                    field: "blame final line order",
                });
            }
        }
        for line in &self.lines {
            line.validate()?;
        }
        Ok(())
    }
}
