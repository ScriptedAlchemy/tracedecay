//! Read-only native Git intelligence contracts (Plan 36, PR9).
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
use crate::research::{DomainError, ManifestDigest, RepositoryId, WorktreeId, canonical_sha256};

/// Schema version label for the typed read-only Git intelligence payloads.
pub const GIT_INTELLIGENCE_SCHEMA_VERSION_V1: &str = "tracedecay.git-intelligence.v1";

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
    let valid = matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !valid {
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
/// never guessed (Plan 36, PR7 provenance rule carried into PR9 reads).
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

fn validate_path_label(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if value.trim() != value || value.chars().any(char::is_control) {
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

/// `HunkRef` operation direction (Plan 36): working tree to index, or index
/// to HEAD/base. No other direction is encodable.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum HunkDirectionV1 {
    WorkingTreeToIndex,
    IndexToHead,
}

/// Expected blob identity, or explicit absent-file state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitBlobExpectationV1 {
    Present(GitOidV1),
    AbsentFile,
}

impl GitBlobExpectationV1 {
    pub fn blob(&self) -> Option<&GitOidV1> {
        match self {
            Self::Present(oid) => Some(oid),
            Self::AbsentFile => None,
        }
    }
}

/// Expected index entry state for compare-and-swap: blob identity (or
/// absent), mode, and unmerged-stage state. `unmerged_stage` is `None` for a
/// merged (stage-0) entry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct GitIndexEntryExpectationV1 {
    pub blob: GitBlobExpectationV1,
    pub mode: Option<GitFileModeV1>,
    pub unmerged_stage: Option<u8>,
}

impl GitIndexEntryExpectationV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self.unmerged_stage {
            None => Ok(()),
            Some(0) => Err(DomainError::NonCanonical {
                field: "index unmerged stage",
            }),
            Some(stage) if stage <= 3 => Ok(()),
            Some(_) => Err(DomainError::NonCanonical {
                field: "index unmerged stage",
            }),
        }
    }
}

/// Build a full-selection bitmap for the requested hunk-line span.
pub fn full_hunk_selection_bitmap(line_count: u32) -> Vec<u64> {
    if line_count == 0 {
        return vec![0];
    }
    let words = line_count.div_ceil(64) as usize;
    let mut bitmap = vec![u64::MAX; words];
    let remainder = line_count % 64;
    if remainder != 0 {
        bitmap[words - 1] = (1u64 << remainder) - 1;
    }
    bitmap
}

/// Immutable hunk identity for compare-and-swap (Plan 36, "`HunkRef`
/// compare-and-swap contract"). A hunk is identified by exact repository,
/// direction, path, expected base/index/worktree identity, normalized hunk
/// header, context and patch digests, and the preview that issued the
/// reference — never by display ordinal or line number alone.
///
/// PR9 mints these as read-only identity evidence only. Applying them is a
/// PR11 daemon mutation path and is not representable here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HunkRefV1 {
    pub repository: RepositoryId,
    pub worktree: WorktreeId,
    pub direction: HunkDirectionV1,
    pub path: String,
    /// Old path for a rename or copy.
    pub original_path: Option<String>,
    pub expected_base_blob: GitBlobExpectationV1,
    pub expected_index_entry: GitIndexEntryExpectationV1,
    /// Expected working-tree identity when the operation reads the worktree:
    /// a native content digest or explicit absent-file state. `None` means
    /// the operation direction does not read the worktree.
    pub expected_worktree_blob: Option<GitBlobExpectationV1>,
    pub expected_worktree_mode: Option<GitFileModeV1>,
    /// Normalized `@@ -o,l +n,m @@` header text.
    pub hunk_header: String,
    pub context_digest: ManifestDigest,
    pub patch_digest: ManifestDigest,
    /// Selected hunk-line bitmap (little-endian word order, line 1 = bit 0
    /// of word 0). Full-hunk identity covers the larger old/new side so
    /// deletion-only hunks remain representable.
    pub selected_line_bitmap: Vec<u64>,
    /// Attributes/filter identity relevant to clean/smudge and EOL handling.
    pub attributes_digest: Option<ManifestDigest>,
    pub preview_id: String,
    pub schema_version: String,
    pub snapshot_digest: ManifestDigest,
}

#[derive(Serialize)]
struct HunkRefDigestEnvelope<'a> {
    domain: &'static str,
    hunk_ref: &'a HunkRefV1,
}

impl HunkRefV1 {
    pub fn selected_line_count(&self) -> u64 {
        self.selected_line_bitmap
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum()
    }

    pub fn selects_line(&self, line: u32) -> bool {
        if line == 0 {
            return false;
        }
        let index = (line - 1) as usize;
        self.selected_line_bitmap
            .get(index / 64)
            .is_some_and(|word| word & (1u64 << (index % 64)) != 0)
    }

    /// Canonical domain-separated digest of this hunk reference. This digest
    /// is the `HunkRef` identity used by preview/apply compare-and-swap.
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        canonical_sha256(&HunkRefDigestEnvelope {
            domain: HUNK_REF_DIGEST_DOMAIN,
            hunk_ref: self,
        })
    }

    /// Verify a previously issued digest against this reference.
    pub fn verify_digest(&self, digest: &ManifestDigest) -> Result<(), DomainError> {
        if &self.compute_digest()? == digest {
            Ok(())
        } else {
            Err(DomainError::DigestMismatch)
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_path_label(&self.path, "hunk ref path")?;
        if let Some(original) = &self.original_path {
            validate_path_label(original, "hunk ref original path")?;
        }
        validate_path_label(&self.hunk_header, "hunk ref header")?;
        validate_path_label(&self.preview_id, "hunk ref preview id")?;
        validate_path_label(&self.schema_version, "hunk ref schema version")?;
        self.expected_index_entry.validate()?;
        match (&self.direction, &self.expected_worktree_blob) {
            (HunkDirectionV1::WorkingTreeToIndex, Some(GitBlobExpectationV1::Present(_)))
                if self.expected_worktree_mode.is_some() => {}
            (HunkDirectionV1::WorkingTreeToIndex, Some(GitBlobExpectationV1::AbsentFile))
                if self.expected_worktree_mode.is_none() => {}
            (HunkDirectionV1::IndexToHead, None) if self.expected_worktree_mode.is_none() => {}
            _ => {
                return Err(DomainError::NonCanonical {
                    field: "hunk ref worktree expectation",
                });
            }
        }
        if self.selected_line_bitmap.is_empty()
            || self.selected_line_bitmap.iter().all(|word| *word == 0)
        {
            return Err(DomainError::Empty {
                field: "hunk ref selected line bitmap",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SHA1_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA1_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SHA1_C: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const DIGEST_X: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    const DIGEST_Y: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn oid(value: &str) -> GitOidV1 {
        GitOidV1::new(value).unwrap()
    }

    fn digest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).unwrap()
    }

    fn repository() -> RepositoryId {
        RepositoryId::new("repository.fixture").unwrap()
    }

    fn hunk(old: (u32, u32), new: (u32, u32)) -> GitHunkV1 {
        GitHunkV1 {
            old_start: old.0,
            old_lines: old.1,
            new_start: new.0,
            new_lines: new.1,
            section: None,
            patch_digest: digest(DIGEST_X),
        }
    }

    fn file_diff(path: &str, change: GitChangeKindV1) -> GitFileDiffV1 {
        GitFileDiffV1 {
            path: path.to_owned(),
            original_path: None,
            change,
            old_mode: None,
            new_mode: None,
            old_blob: None,
            new_blob: None,
            binary: false,
            submodule: false,
            insertions: Some(1),
            deletions: Some(0),
            hunks: vec![hunk((1, 1), (1, 2))],
        }
    }

    fn identity(name: &str) -> GitCommitIdentityV1 {
        GitCommitIdentityV1 {
            name: name.to_owned(),
            email: format!("{name}@example.com"),
            at: UtcMicros(1_700_000_000_000_000),
        }
    }

    fn commit(value: &str) -> GitCommitMetadataV1 {
        GitCommitMetadataV1 {
            commit: oid(value),
            tree: oid(SHA1_C),
            parents: vec![],
            author: identity("author"),
            committer: identity("committer"),
            subject: "subject".to_owned(),
            message_digest: digest(DIGEST_X),
        }
    }

    fn hunk_ref() -> HunkRefV1 {
        HunkRefV1 {
            repository: repository(),
            worktree: WorktreeId::new("worktree.fixture").unwrap(),
            direction: HunkDirectionV1::WorkingTreeToIndex,
            path: "src/main.rs".to_owned(),
            original_path: None,
            expected_base_blob: GitBlobExpectationV1::Present(oid(SHA1_A)),
            expected_index_entry: GitIndexEntryExpectationV1 {
                blob: GitBlobExpectationV1::Present(oid(SHA1_A)),
                mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                unmerged_stage: None,
            },
            expected_worktree_blob: Some(GitBlobExpectationV1::Present(oid(SHA1_B))),
            expected_worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
            hunk_header: "@@ -1,3 +1,4 @@".to_owned(),
            context_digest: digest(DIGEST_X),
            patch_digest: digest(DIGEST_Y),
            selected_line_bitmap: full_hunk_selection_bitmap(4),
            attributes_digest: None,
            preview_id: "preview.fixture".to_owned(),
            schema_version: HUNK_REF_SCHEMA_VERSION_V1.to_owned(),
            snapshot_digest: digest(DIGEST_X),
        }
    }

    #[test]
    fn git_oid_accepts_sha1_and_sha256_and_derives_format() {
        let sha1 = oid(SHA1_A);
        assert_eq!(sha1.format(), GitObjectFormatV1::Sha1);
        assert_eq!(GitObjectFormatV1::Sha1.oid_hex_len(), 40);

        let sha256 = oid(&"d".repeat(64));
        assert_eq!(sha256.format(), GitObjectFormatV1::Sha256);
        assert_eq!(GitObjectFormatV1::Sha256.oid_hex_len(), 64);
    }

    #[test]
    fn git_oid_rejects_noncanonical_values() {
        for bad in [
            "",
            "abc",
            &"a".repeat(39),
            &"a".repeat(41),
            &"A".repeat(40),
            &"g".repeat(40),
            &"a".repeat(63),
        ] {
            assert!(GitOidV1::new(bad).is_err(), "accepted oid {bad:?}");
        }
        assert!(serde_json::from_value::<GitOidV1>(json!("not-an-oid")).is_err());
    }

    #[test]
    fn file_mode_validation_and_kind_helpers() {
        let regular = GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap();
        assert!(!regular.is_submodule());
        assert!(!regular.is_symlink());
        assert!(
            GitFileModeV1::new(GitFileModeV1::GITLINK)
                .unwrap()
                .is_submodule()
        );
        assert!(
            GitFileModeV1::new(GitFileModeV1::SYMLINK)
                .unwrap()
                .is_symlink()
        );

        for bad in ["", "10064", "1006444", "10084a", "888888"] {
            assert!(GitFileModeV1::new(bad).is_err(), "accepted mode {bad:?}");
        }
    }

    #[test]
    fn head_state_roundtrips_and_exposes_commit() {
        let attached = GitHeadStateV1::Attached {
            branch: "main".to_owned(),
            commit: oid(SHA1_A),
        };
        let detached = GitHeadStateV1::Detached {
            commit: oid(SHA1_A),
        };
        let unborn = GitHeadStateV1::Unborn {
            branch: "main".to_owned(),
        };

        assert_eq!(attached.commit(), Some(&oid(SHA1_A)));
        assert_eq!(attached.branch(), Some("main"));
        assert_eq!(detached.branch(), None);
        assert_eq!(unborn.commit(), None);

        for state in [attached, detached, unborn] {
            state.validate().unwrap();
            let wire = serde_json::to_string(&state).unwrap();
            assert_eq!(
                serde_json::from_str::<GitHeadStateV1>(&wire).unwrap(),
                state
            );
        }
    }

    #[test]
    fn coverage_dedupes_sorts_and_reports_completeness() {
        let mut coverage = GitCoverageV1::complete();
        assert!(coverage.is_complete());

        coverage = GitCoverageV1::degraded(vec![
            GitDegradationV1::SubmoduleState,
            GitDegradationV1::DetachedHead,
            GitDegradationV1::DetachedHead,
        ]);
        assert!(!coverage.is_complete());
        assert!(coverage.records(GitDegradationV1::DetachedHead));
        assert_eq!(coverage.degradations.len(), 2);
        coverage.validate().unwrap();

        coverage.record(GitDegradationV1::SparseCheckout);
        coverage.record(GitDegradationV1::SparseCheckout);
        assert_eq!(coverage.degradations.len(), 3);
        coverage.validate().unwrap();

        let mut unsorted = coverage.clone();
        unsorted.degradations.reverse();
        if unsorted.degradations != coverage.degradations {
            assert!(unsorted.validate().is_err());
        }
    }

    #[test]
    fn status_counts_and_cleanliness() {
        let status = GitStatusV1 {
            repository: repository(),
            head: GitHeadStateV1::Attached {
                branch: "main".to_owned(),
                commit: oid(SHA1_A),
            },
            operation: GitOperationStateV1::None,
            entries: vec![
                GitStatusEntryV1::Tracked(GitTrackedStatusV1 {
                    path: "staged.txt".to_owned(),
                    original_path: None,
                    index: GitChangeKindV1::Added,
                    worktree: GitChangeKindV1::Unmodified,
                    head_mode: None,
                    index_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                    worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                    submodule: false,
                }),
                GitStatusEntryV1::Tracked(GitTrackedStatusV1 {
                    path: "dirty.txt".to_owned(),
                    original_path: None,
                    index: GitChangeKindV1::Unmodified,
                    worktree: GitChangeKindV1::Modified,
                    head_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                    index_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                    worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                    submodule: false,
                }),
                GitStatusEntryV1::Tracked(GitTrackedStatusV1 {
                    path: "conflict.txt".to_owned(),
                    original_path: None,
                    index: GitChangeKindV1::Unmerged,
                    worktree: GitChangeKindV1::Unmerged,
                    head_mode: None,
                    index_mode: None,
                    worktree_mode: Some(GitFileModeV1::new(GitFileModeV1::REGULAR).unwrap()),
                    submodule: false,
                }),
                GitStatusEntryV1::Untracked {
                    path: "new.txt".to_owned(),
                },
                GitStatusEntryV1::Ignored {
                    path: "app.log".to_owned(),
                },
            ],
            coverage: GitCoverageV1::complete(),
        };

        assert_eq!(status.staged_count(), 1);
        assert_eq!(status.unstaged_count(), 1);
        assert_eq!(status.conflicted_count(), 1);
        assert_eq!(status.untracked_count(), 1);
        assert_eq!(status.ignored_count(), 1);
        assert!(!status.is_clean());
        status.validate().unwrap();

        let clean = GitStatusV1 {
            entries: vec![],
            ..status
        };
        assert!(clean.is_clean());
    }

    #[test]
    fn status_rejects_duplicate_paths() {
        let entry = GitStatusEntryV1::Untracked {
            path: "same.txt".to_owned(),
        };
        let status = GitStatusV1 {
            repository: repository(),
            head: GitHeadStateV1::Unborn {
                branch: "main".to_owned(),
            },
            operation: GitOperationStateV1::None,
            entries: vec![entry.clone(), entry],
            coverage: GitCoverageV1::complete(),
        };
        assert_eq!(
            status.validate(),
            Err(DomainError::DuplicateId {
                field: "status entry path"
            })
        );
    }

    #[test]
    fn hunk_range_invariants_match_git_addressing() {
        // Pure insertion at the top of the file: old side addressed at 0,0.
        hunk((0, 0), (1, 3)).validate().unwrap();
        // Normal replacement hunk.
        hunk((1, 3), (1, 4)).validate().unwrap();
        // A non-empty side cannot start at line 0.
        assert!(hunk((0, 2), (1, 2)).validate().is_err());
        assert!(hunk((1, 2), (0, 2)).validate().is_err());
        assert_eq!(hunk((0, 0), (1, 3)).normalized_header(), "@@ -0,0 +1,3 @@");
    }

    #[test]
    fn file_diff_invariants_for_binary_submodule_and_renames() {
        let mut binary = file_diff("blob.bin", GitChangeKindV1::Modified);
        binary.binary = true;
        binary.insertions = None;
        binary.deletions = None;
        binary.hunks = vec![];
        binary.validate().unwrap();

        let mut invalid = binary.clone();
        invalid.hunks = vec![hunk((1, 1), (1, 1))];
        assert!(invalid.validate().is_err());

        let mut renamed = file_diff("new.rs", GitChangeKindV1::Renamed);
        renamed.original_path = Some("old.rs".to_owned());
        renamed.validate().unwrap();

        renamed.original_path = None;
        assert!(renamed.validate().is_err());

        let mut misplaced = file_diff("plain.rs", GitChangeKindV1::Modified);
        misplaced.original_path = Some("old.rs".to_owned());
        assert!(misplaced.validate().is_err());
    }

    #[test]
    fn diff_rejects_duplicate_file_paths() {
        let diff = GitDiffV1 {
            repository: repository(),
            scope: GitDiffScopeV1::WorkingTree,
            files: vec![
                file_diff("same.rs", GitChangeKindV1::Modified),
                file_diff("same.rs", GitChangeKindV1::Modified),
            ],
            coverage: GitCoverageV1::complete(),
        };
        assert_eq!(
            diff.validate(),
            Err(DomainError::DuplicateId {
                field: "diff file path"
            })
        );
    }

    #[test]
    fn history_rejects_duplicate_commits() {
        let history = GitHistoryV1 {
            repository: repository(),
            commits: vec![commit(SHA1_A), commit(SHA1_A)],
            truncated: false,
            coverage: GitCoverageV1::complete(),
        };
        assert_eq!(
            history.validate(),
            Err(DomainError::DuplicateId {
                field: "history commit"
            })
        );
    }

    #[test]
    fn blame_availability_invariants() {
        let unavailable = GitBlameV1 {
            repository: repository(),
            path: "missing.rs".to_owned(),
            lines: vec![],
            availability: GitBlameAvailabilityV1::PathNotTracked,
            coverage: GitCoverageV1::complete(),
        };
        unavailable.validate().unwrap();
        assert!(!unavailable.is_available());

        let mut incoherent = unavailable.clone();
        incoherent.lines = vec![GitBlameLineV1 {
            final_line: 1,
            origin_line: 1,
            commit: oid(SHA1_A),
            author: identity("author"),
            boundary: false,
            previous: None,
        }];
        assert!(incoherent.validate().is_err());

        let available = GitBlameV1 {
            repository: repository(),
            path: "tracked.rs".to_owned(),
            lines: vec![
                GitBlameLineV1 {
                    final_line: 1,
                    origin_line: 1,
                    commit: oid(SHA1_A),
                    author: identity("author"),
                    boundary: false,
                    previous: None,
                },
                GitBlameLineV1 {
                    final_line: 2,
                    origin_line: 2,
                    commit: oid(SHA1_B),
                    author: identity("author"),
                    boundary: true,
                    previous: Some(GitBlamePreviousV1 {
                        commit: oid(SHA1_C),
                        path: "old.rs".to_owned(),
                    }),
                },
            ],
            availability: GitBlameAvailabilityV1::Available,
            coverage: GitCoverageV1::complete(),
        };
        available.validate().unwrap();
        assert!(available.is_available());

        let mut disordered = available.clone();
        disordered.lines.swap(0, 1);
        assert!(disordered.validate().is_err());
    }

    #[test]
    fn hunk_ref_selection_bitmap_counts_and_queries_lines() {
        let bitmap = full_hunk_selection_bitmap(70);
        assert_eq!(bitmap.len(), 2);
        assert_eq!(bitmap[1], 0b111111);

        let reference = HunkRefV1 {
            selected_line_bitmap: bitmap,
            ..hunk_ref()
        };
        assert_eq!(reference.selected_line_count(), 70);
        assert!(reference.selects_line(1));
        assert!(reference.selects_line(70));
        assert!(!reference.selects_line(71));
        assert!(!reference.selects_line(0));
        reference.validate().unwrap();

        assert!(full_hunk_selection_bitmap(64)[0] == u64::MAX);
        let mut empty = hunk_ref();
        empty.selected_line_bitmap = vec![];
        assert!(empty.validate().is_err());
        let mut zero = hunk_ref();
        zero.selected_line_bitmap = vec![0];
        assert!(zero.validate().is_err());
    }

    #[test]
    fn hunk_ref_digest_is_domain_separated_stable_and_self_verifying() {
        let reference = hunk_ref();
        let digest = reference.compute_digest().unwrap();
        assert_eq!(digest, reference.compute_digest().unwrap());
        reference.verify_digest(&digest).unwrap();
        assert_eq!(
            reference.verify_digest(&ManifestDigest::new(DIGEST_Y).unwrap()),
            Err(DomainError::DigestMismatch)
        );

        // Domain separation: the same payload hashed under a different domain
        // separator cannot collide with the HunkRef digest.
        let foreign = canonical_sha256(&serde_json::json!({
            "domain": "tracedecay.other.v1",
            "hunk_ref": serde_json::to_value(&reference).unwrap(),
        }))
        .unwrap();
        assert_ne!(digest, foreign);
    }

    #[test]
    fn hunk_ref_digest_detects_independent_field_drift() {
        let reference = hunk_ref();
        let digest = reference.compute_digest().unwrap();

        let mutations: Vec<HunkRefV1> = vec![
            HunkRefV1 {
                path: "src/other.rs".to_owned(),
                ..reference.clone()
            },
            HunkRefV1 {
                direction: HunkDirectionV1::IndexToHead,
                ..reference.clone()
            },
            HunkRefV1 {
                expected_base_blob: GitBlobExpectationV1::AbsentFile,
                ..reference.clone()
            },
            HunkRefV1 {
                hunk_header: "@@ -1,3 +1,5 @@".to_owned(),
                ..reference.clone()
            },
            HunkRefV1 {
                selected_line_bitmap: full_hunk_selection_bitmap(5),
                ..reference.clone()
            },
            HunkRefV1 {
                preview_id: "preview.other".to_owned(),
                ..reference.clone()
            },
            HunkRefV1 {
                snapshot_digest: ManifestDigest::new(DIGEST_Y).unwrap(),
                ..reference.clone()
            },
        ];

        for mutated in mutations {
            assert!(
                mutated.verify_digest(&digest).is_err(),
                "field drift must invalidate the HunkRef digest"
            );
        }
    }

    #[test]
    fn git_values_roundtrip_through_serde() {
        let status = GitStatusV1 {
            repository: repository(),
            head: GitHeadStateV1::Detached {
                commit: oid(SHA1_A),
            },
            operation: GitOperationStateV1::Merge,
            entries: vec![GitStatusEntryV1::Ignored {
                path: "app.log".to_owned(),
            }],
            coverage: GitCoverageV1::degraded(vec![GitDegradationV1::IgnoredCollision]),
        };
        let diff = GitDiffV1 {
            repository: repository(),
            scope: GitDiffScopeV1::CommitRange {
                base: oid(SHA1_A),
                head: oid(SHA1_B),
            },
            files: vec![file_diff("src/a.rs", GitChangeKindV1::Modified)],
            coverage: GitCoverageV1::complete(),
        };
        let history = GitHistoryV1 {
            repository: repository(),
            commits: vec![commit(SHA1_A)],
            truncated: true,
            coverage: GitCoverageV1::degraded(vec![GitDegradationV1::TruncatedOutput]),
        };
        let reference = hunk_ref();

        for value in [
            serde_json::to_string(&status).unwrap(),
            serde_json::to_string(&diff).unwrap(),
            serde_json::to_string(&history).unwrap(),
            serde_json::to_string(&reference).unwrap(),
        ] {
            assert!(serde_json::from_str::<serde_json::Value>(&value).is_ok());
        }

        let status_wire = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serde_json::from_str::<GitStatusV1>(&status_wire).unwrap(),
            status
        );
        let diff_wire = serde_json::to_string(&diff).unwrap();
        assert_eq!(serde_json::from_str::<GitDiffV1>(&diff_wire).unwrap(), diff);
        let history_wire = serde_json::to_string(&history).unwrap();
        assert_eq!(
            serde_json::from_str::<GitHistoryV1>(&history_wire).unwrap(),
            history
        );
        let ref_wire = serde_json::to_string(&reference).unwrap();
        assert_eq!(
            serde_json::from_str::<HunkRefV1>(&ref_wire).unwrap(),
            reference
        );
    }
}
