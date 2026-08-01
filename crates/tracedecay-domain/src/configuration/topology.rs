//! Typed worktree-topology policy values.
//!
//! This module expresses policy only. It does not inspect paths, create
//! worktrees, choose branches, invoke Git, run checks, or perform cleanup.

use std::collections::BTreeSet;
use std::fmt;
use std::num::{NonZeroU8, NonZeroU16, NonZeroU32, NonZeroU64};

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{
    CapabilityId, DomainError, LocatorDigest, ManifestDigest, RepositoryId, canonical_sha256,
};

const TOPOLOGY_DIGEST_DOMAIN: &str = "tracedecay.work-topology-policy.v1";

#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorktreePlacementRootId(String);

impl WorktreePlacementRootId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_ref_fragment(&value, "worktree placement root id")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_ref_fragment(&self.0, "worktree placement root id")
    }
}

impl<'de> Deserialize<'de> for WorktreePlacementRootId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for WorktreePlacementRootId {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

fn validate_ref_fragment(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty()
        || value.trim() != value
        || value.chars().any(char::is_control)
        || value.contains("..")
        || value.contains("//")
        || value.contains("@{")
        || value.starts_with('/')
        || value.ends_with('.')
        || value
            .bytes()
            .any(|byte| matches!(byte, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\'))
    {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// A canonical ref name is a ref fragment rooted at `refs/` that does not end
/// in a path separator.
fn validate_canonical_ref_name(value: &str) -> Result<(), DomainError> {
    const FIELD: &str = "canonical Git ref name";

    validate_ref_fragment(value, FIELD)?;
    if !value.starts_with("refs/") || value.ends_with('/') {
        return Err(DomainError::NonCanonical { field: FIELD });
    }
    Ok(())
}

/// A canonical ref prefix is a ref fragment that ends in a path separator.
fn validate_canonical_ref_prefix(value: &str) -> Result<(), DomainError> {
    const FIELD: &str = "canonical Git ref prefix";

    validate_ref_fragment(value, FIELD)?;
    if !value.ends_with('/') {
        return Err(DomainError::NonCanonical { field: FIELD });
    }
    Ok(())
}

/// A validated full native Git ref name, such as `refs/heads/main`.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct CanonicalGitRefNameV1(String);

impl CanonicalGitRefNameV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_canonical_ref_name(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_canonical_ref_name(&self.0)
    }
}

impl<'de> Deserialize<'de> for CanonicalGitRefNameV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for CanonicalGitRefNameV1 {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for CanonicalGitRefNameV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A validated ref/branch prefix. Branch naming accepts a short branch prefix
/// (for example `tracedecay/`) while protected-ref selectors use full
/// `refs/.../` prefixes.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct CanonicalGitRefPrefix(String);

impl CanonicalGitRefPrefix {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        validate_canonical_ref_prefix(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_canonical_ref_prefix(&self.0)
    }
}

impl<'de> Deserialize<'de> for CanonicalGitRefPrefix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for CanonicalGitRefPrefix {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for CanonicalGitRefPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reference-only filesystem locator. The raw path is sealed outside this
/// contract; only the privacy-bound locator digest and sealed-value digest are
/// representable here.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SensitiveFilesystemLocatorV1 {
    pub locator_digest: LocatorDigest,
    pub sealed_value_digest: ManifestDigest,
}

impl SensitiveFilesystemLocatorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.locator_digest.validate()?;
        self.sealed_value_digest.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "repositories")]
pub enum RepositoryPlacementScopeV1 {
    AllAuthorized,
    Allowlist(BTreeSet<RepositoryId>),
}

impl RepositoryPlacementScopeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::AllAuthorized => Ok(()),
            Self::Allowlist(repositories) => {
                if repositories.is_empty() {
                    return Err(DomainError::Empty {
                        field: "repository placement allowlist",
                    });
                }
                for repository in repositories {
                    repository.validate()?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRootPolicyV1 {
    pub root_id: WorktreePlacementRootId,
    pub locator: SensitiveFilesystemLocatorV1,
    pub repository_scope: RepositoryPlacementScopeV1,
    pub maximum_active_worktrees: NonZeroU16,
}

impl WorktreeRootPolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.root_id.validate()?;
        self.locator.validate()?;
        self.repository_scope.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "root_id")]
pub enum WorktreePlacementModeV1 {
    ExistingWorktreeOnly,
    SiblingOfPrimaryCheckout,
    RepositoryLocalRoot,
    ConfiguredRoot(WorktreePlacementRootId),
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum BranchTopologyKindV1 {
    NoBranches,
    Unbranched,
    IndependentBranches,
    LocalStack,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchTopologyPolicyV1 {
    pub allowed: BTreeSet<BranchTopologyKindV1>,
}

impl BranchTopologyPolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.allowed.is_empty() {
            return Err(DomainError::Empty {
                field: "allowed branch topology",
            });
        }
        Ok(())
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTopologyKindV1 {
    NoReview,
    IndependentReview,
    StandardPullRequests,
    GitHubStackedPullRequests,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum GitHubStackedPullRequestPolicyV1 {
    Disabled,
    ProbePrivatePreview,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewTopologyPolicyV1 {
    pub allowed: BTreeSet<ReviewTopologyKindV1>,
    pub github_stacked_prs: GitHubStackedPullRequestPolicyV1,
}

impl ReviewTopologyPolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.allowed.is_empty() {
            return Err(DomainError::Empty {
                field: "allowed review topology",
            });
        }
        if self
            .allowed
            .contains(&ReviewTopologyKindV1::GitHubStackedPullRequests)
            && (self.github_stacked_prs != GitHubStackedPullRequestPolicyV1::ProbePrivatePreview
                || !self
                    .allowed
                    .contains(&ReviewTopologyKindV1::StandardPullRequests))
        {
            return Err(DomainError::NonCanonical {
                field: "GitHub stacked pull request policy",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BranchNameComponentV1 {
    TaskIdDigestPrefix { bytes: NonZeroU8 },
    RepositorySlug,
    WorkClass,
    MonotonicCollisionOrdinal,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum BranchNameSeparatorV1 {
    Hyphen,
    Underscore,
    Slash,
}

impl BranchNameSeparatorV1 {
    pub const fn as_char(self) -> char {
        match self {
            Self::Hyphen => '-',
            Self::Underscore => '_',
            Self::Slash => '/',
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BranchCollisionPolicyV1 {
    Reject,
    AppendMonotonicOrdinal { maximum_attempts: NonZeroU16 },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchNamingPolicyV1 {
    pub prefix: CanonicalGitRefPrefix,
    pub components: Vec<BranchNameComponentV1>,
    pub separator: BranchNameSeparatorV1,
    pub maximum_bytes: NonZeroU16,
    pub collision: BranchCollisionPolicyV1,
}

impl BranchNamingPolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.prefix.validate()?;
        if self.components.is_empty() {
            return Err(DomainError::Empty {
                field: "branch name components",
            });
        }
        let mut task_prefixes = 0usize;
        let mut collision_components = 0usize;
        for component in &self.components {
            match component {
                BranchNameComponentV1::TaskIdDigestPrefix { bytes } => {
                    task_prefixes += 1;
                    if !(8..=20).contains(&bytes.get()) {
                        return Err(DomainError::NonCanonical {
                            field: "task id digest prefix bytes",
                        });
                    }
                }
                BranchNameComponentV1::MonotonicCollisionOrdinal => collision_components += 1,
                BranchNameComponentV1::RepositorySlug | BranchNameComponentV1::WorkClass => {}
            }
        }
        if task_prefixes > 1 {
            return Err(DomainError::DuplicateId {
                field: "task id digest prefix component",
            });
        }
        match self.collision {
            BranchCollisionPolicyV1::Reject if collision_components != 0 => {
                return Err(DomainError::NonCanonical {
                    field: "branch collision component",
                });
            }
            BranchCollisionPolicyV1::AppendMonotonicOrdinal { .. } if collision_components != 1 => {
                return Err(DomainError::NonCanonical {
                    field: "branch collision component",
                });
            }
            BranchCollisionPolicyV1::Reject
            | BranchCollisionPolicyV1::AppendMonotonicOrdinal { .. } => {}
        }
        let _ = self.separator.as_char();
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyConcurrencyPolicyV1 {
    pub maximum_active_per_repository: NonZeroU16,
    pub maximum_parallel_per_task: NonZeroU16,
    pub maximum_global_active: NonZeroU16,
    pub maximum_stack_depth: NonZeroU16,
}

impl TopologyConcurrencyPolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.maximum_parallel_per_task > self.maximum_active_per_repository
            || self.maximum_active_per_repository > self.maximum_global_active
        {
            return Err(DomainError::NonCanonical {
                field: "topology concurrency bounds",
            });
        }
        Ok(())
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum CrossMergeModeV1 {
    Disabled,
    ManualReceiptOnly,
    FastForwardOnly,
    MergeCommit,
    CherryPickExactCommits,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CrossMergePolicyV1 {
    pub allowed_modes: BTreeSet<CrossMergeModeV1>,
    pub default_mode: CrossMergeModeV1,
    pub allow_cross_repository: bool,
}

impl CrossMergePolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.allowed_modes.is_empty() || !self.allowed_modes.contains(&self.default_mode) {
            return Err(DomainError::NonCanonical {
                field: "cross merge policy",
            });
        }
        if self.allow_cross_repository
            && (self.default_mode != CrossMergeModeV1::ManualReceiptOnly
                || self.allowed_modes != BTreeSet::from([CrossMergeModeV1::ManualReceiptOnly]))
        {
            return Err(DomainError::NonCanonical {
                field: "cross repository merge policy",
            });
        }
        Ok(())
    }

    fn has_native_apply_mode(&self) -> bool {
        self.allowed_modes.iter().any(|mode| {
            matches!(
                mode,
                CrossMergeModeV1::FastForwardOnly
                    | CrossMergeModeV1::MergeCommit
                    | CrossMergeModeV1::CherryPickExactCommits
            )
        })
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCleanlinessRequirementV1 {
    RequireClean,
    AllowUntrackedOnlyForPreflight,
    ReadOnlyPreflightOnly,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredCheckV1 {
    pub capability_id: CapabilityId,
    pub expectation: RequiredCheckExpectationV1,
    pub maximum_age_seconds: NonZeroU32,
}

impl RequiredCheckV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.capability_id.validate()
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum RequiredCheckExpectationV1 {
    SuccessfulTerminal,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "count")]
pub enum ReviewRequirementV1 {
    None,
    IndependentReviewCount(NonZeroU16),
    CodeOwnerAndIndependentReview,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TopologyGatePolicyV1 {
    pub cleanliness: WorktreeCleanlinessRequirementV1,
    pub tests: Vec<RequiredCheckV1>,
    pub review: ReviewRequirementV1,
    pub require_fresh_preflight: bool,
    pub maximum_preflight_age_seconds: NonZeroU32,
}

impl TopologyGatePolicyV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        for check in &self.tests {
            check.validate()?;
        }
        let mut ids: Vec<_> = self
            .tests
            .iter()
            .map(|check| &check.capability_id)
            .collect();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(DomainError::DuplicateId {
                field: "topology required check capability",
            });
        }
        Ok(())
    }
}

#[derive(
    Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ProtectedRefSelectorV1 {
    NativeDefaultBranch,
    Exact(CanonicalGitRefNameV1),
    Prefix(CanonicalGitRefPrefix),
}

impl ProtectedRefSelectorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::NativeDefaultBranch => Ok(()),
            Self::Exact(name) => name.validate(),
            Self::Prefix(prefix) => prefix.validate(),
        }
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedRefDispositionV1 {
    Reject,
    RequireHumanApprovalAndIndependentReview,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProtectedRefRuleV1 {
    pub selector: ProtectedRefSelectorV1,
    pub disposition: ProtectedRefDispositionV1,
}

impl ProtectedRefRuleV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.selector.validate()
    }
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRewritePolicyV1 {
    ForbidForceAndRebase,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum TopologyEscalationPolicyV1 {
    Reject,
    RequireExplicitHumanApproval,
    RequireHumanApprovalAndIndependentReview,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AutomaticWorktreeGcV1 {
    Disabled,
    EligibleOnly {
        minimum_idle_seconds: NonZeroU64,
        maximum_per_run: NonZeroU16,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorktreeRetentionPolicyV1 {
    pub terminal_retention_seconds: Option<NonZeroU64>,
    pub abandoned_retention_seconds: Option<NonZeroU64>,
    pub maximum_retained_per_repository: Option<NonZeroU16>,
    pub automatic_gc: AutomaticWorktreeGcV1,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum TopologyNotificationLevelV1 {
    CriticalOnly,
    Lifecycle,
    Verbose,
}

/// Complete V1 policy. Partial values are intentionally impossible: callers
/// must provide the entire policy and validation rejects adapter-local defaults.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkTopologyPolicyV1 {
    pub schema_version: u16,
    pub placement: WorktreePlacementModeV1,
    pub roots: Vec<WorktreeRootPolicyV1>,
    pub branch_topology: BranchTopologyPolicyV1,
    pub review_topology: ReviewTopologyPolicyV1,
    pub branch_naming: BranchNamingPolicyV1,
    pub concurrency: TopologyConcurrencyPolicyV1,
    pub cross_merge: CrossMergePolicyV1,
    pub gates: TopologyGatePolicyV1,
    pub protected_refs: Vec<ProtectedRefRuleV1>,
    pub history_rewrite: HistoryRewritePolicyV1,
    pub escalation: TopologyEscalationPolicyV1,
    pub retention: WorktreeRetentionPolicyV1,
    pub notifications: TopologyNotificationLevelV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
pub struct TopologyPolicyDigestV1(pub ManifestDigest);

impl WorkTopologyPolicyV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(DomainError::NonCanonical {
                field: "work topology policy schema version",
            });
        }
        let mut root_ids = BTreeSet::new();
        for root in &self.roots {
            root.validate()?;
            if !root_ids.insert(root.root_id.clone()) {
                return Err(DomainError::DuplicateId {
                    field: "worktree placement root id",
                });
            }
        }
        match &self.placement {
            WorktreePlacementModeV1::ConfiguredRoot(root_id) => {
                root_id.validate()?;
                if self
                    .roots
                    .iter()
                    .filter(|root| &root.root_id == root_id)
                    .count()
                    != 1
                {
                    return Err(DomainError::UnknownReference {
                        field: "configured worktree placement root",
                    });
                }
            }
            WorktreePlacementModeV1::ExistingWorktreeOnly
            | WorktreePlacementModeV1::SiblingOfPrimaryCheckout
            | WorktreePlacementModeV1::RepositoryLocalRoot => {}
        }
        self.branch_topology.validate()?;
        self.review_topology.validate()?;
        self.branch_naming.validate()?;
        self.concurrency.validate()?;
        self.cross_merge.validate()?;
        self.gates.validate()?;
        if self.protected_refs.is_empty() {
            return Err(DomainError::Empty {
                field: "protected ref rules",
            });
        }
        let mut selectors = BTreeSet::new();
        for rule in &self.protected_refs {
            rule.validate()?;
            if !selectors.insert(rule.selector.clone()) {
                return Err(DomainError::DuplicateId {
                    field: "protected ref selector",
                });
            }
        }
        if !self.meets_protected_ref_floor() {
            return Err(DomainError::NonCanonical {
                field: "protected ref floor",
            });
        }
        if self.cross_merge.has_native_apply_mode()
            && (self.gates.cleanliness != WorktreeCleanlinessRequirementV1::RequireClean
                || self.gates.tests.is_empty()
                || !self.gates.require_fresh_preflight)
        {
            return Err(DomainError::NonCanonical {
                field: "native cross merge gate requirements",
            });
        }
        Ok(())
    }

    pub fn compute_digest(&self) -> Result<TopologyPolicyDigestV1, DomainError> {
        self.validate()?;
        Ok(TopologyPolicyDigestV1(canonical_sha256(&(
            TOPOLOGY_DIGEST_DOMAIN,
            self,
        ))?))
    }

    pub fn meets_protected_ref_floor(&self) -> bool {
        let required = [
            ProtectedRefSelectorV1::NativeDefaultBranch,
            ProtectedRefSelectorV1::Exact(
                CanonicalGitRefNameV1::new("refs/heads/main").expect("static ref is valid"),
            ),
            ProtectedRefSelectorV1::Exact(
                CanonicalGitRefNameV1::new("refs/heads/master").expect("static ref is valid"),
            ),
            ProtectedRefSelectorV1::Prefix(
                CanonicalGitRefPrefix::new("refs/remotes/").expect("static ref is valid"),
            ),
            ProtectedRefSelectorV1::Prefix(
                CanonicalGitRefPrefix::new("refs/tags/").expect("static ref is valid"),
            ),
        ];
        required.iter().all(|selector| {
            self.protected_refs.iter().any(|rule| {
                rule.selector == *selector && rule.disposition == ProtectedRefDispositionV1::Reject
            })
        })
    }
}

fn non_zero_u8(value: u8) -> NonZeroU8 {
    match NonZeroU8::new(value) {
        Some(value) => value,
        None => unreachable!("safe topology constants are nonzero"),
    }
}

fn non_zero_u16(value: u16) -> NonZeroU16 {
    match NonZeroU16::new(value) {
        Some(value) => value,
        None => unreachable!("safe topology constants are nonzero"),
    }
}

fn non_zero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => unreachable!("safe topology constants are nonzero"),
    }
}

/// Exact safe default required by the control-plane plan. It authorizes no
/// worktree creation, ref mutation, history rewrite, automatic cleanup, or
/// cross-repository integration.
pub fn safe_work_topology_policy_v1() -> WorkTopologyPolicyV1 {
    WorkTopologyPolicyV1 {
        schema_version: WorkTopologyPolicyV1::SCHEMA_VERSION,
        placement: WorktreePlacementModeV1::ExistingWorktreeOnly,
        roots: Vec::new(),
        branch_topology: BranchTopologyPolicyV1 {
            allowed: BTreeSet::from([
                BranchTopologyKindV1::NoBranches,
                BranchTopologyKindV1::Unbranched,
                BranchTopologyKindV1::IndependentBranches,
            ]),
        },
        review_topology: ReviewTopologyPolicyV1 {
            allowed: BTreeSet::from([
                ReviewTopologyKindV1::NoReview,
                ReviewTopologyKindV1::IndependentReview,
                ReviewTopologyKindV1::StandardPullRequests,
            ]),
            github_stacked_prs: GitHubStackedPullRequestPolicyV1::Disabled,
        },
        branch_naming: BranchNamingPolicyV1 {
            prefix: CanonicalGitRefPrefix::new("tracedecay/").expect("static prefix is valid"),
            components: vec![
                BranchNameComponentV1::TaskIdDigestPrefix {
                    bytes: non_zero_u8(10),
                },
                BranchNameComponentV1::WorkClass,
                BranchNameComponentV1::MonotonicCollisionOrdinal,
            ],
            separator: BranchNameSeparatorV1::Slash,
            maximum_bytes: non_zero_u16(200),
            collision: BranchCollisionPolicyV1::AppendMonotonicOrdinal {
                maximum_attempts: non_zero_u16(32),
            },
        },
        concurrency: TopologyConcurrencyPolicyV1 {
            maximum_active_per_repository: non_zero_u16(1),
            maximum_parallel_per_task: non_zero_u16(1),
            maximum_global_active: non_zero_u16(1),
            maximum_stack_depth: non_zero_u16(1),
        },
        cross_merge: CrossMergePolicyV1 {
            allowed_modes: BTreeSet::from([CrossMergeModeV1::Disabled]),
            default_mode: CrossMergeModeV1::Disabled,
            allow_cross_repository: false,
        },
        gates: TopologyGatePolicyV1 {
            cleanliness: WorktreeCleanlinessRequirementV1::RequireClean,
            tests: Vec::new(),
            review: ReviewRequirementV1::IndependentReviewCount(non_zero_u16(1)),
            require_fresh_preflight: true,
            maximum_preflight_age_seconds: non_zero_u32(300),
        },
        protected_refs: vec![
            ProtectedRefRuleV1 {
                selector: ProtectedRefSelectorV1::NativeDefaultBranch,
                disposition: ProtectedRefDispositionV1::Reject,
            },
            ProtectedRefRuleV1 {
                selector: ProtectedRefSelectorV1::Exact(
                    CanonicalGitRefNameV1::new("refs/heads/main").expect("static ref is valid"),
                ),
                disposition: ProtectedRefDispositionV1::Reject,
            },
            ProtectedRefRuleV1 {
                selector: ProtectedRefSelectorV1::Exact(
                    CanonicalGitRefNameV1::new("refs/heads/master").expect("static ref is valid"),
                ),
                disposition: ProtectedRefDispositionV1::Reject,
            },
            ProtectedRefRuleV1 {
                selector: ProtectedRefSelectorV1::Prefix(
                    CanonicalGitRefPrefix::new("refs/tags/").expect("static ref is valid"),
                ),
                disposition: ProtectedRefDispositionV1::Reject,
            },
            ProtectedRefRuleV1 {
                selector: ProtectedRefSelectorV1::Prefix(
                    CanonicalGitRefPrefix::new("refs/remotes/").expect("static ref is valid"),
                ),
                disposition: ProtectedRefDispositionV1::Reject,
            },
        ],
        history_rewrite: HistoryRewritePolicyV1::ForbidForceAndRebase,
        escalation: TopologyEscalationPolicyV1::Reject,
        retention: WorktreeRetentionPolicyV1 {
            terminal_retention_seconds: None,
            abandoned_retention_seconds: None,
            maximum_retained_per_repository: None,
            automatic_gc: AutomaticWorktreeGcV1::Disabled,
        },
        notifications: TopologyNotificationLevelV1::CriticalOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::ProtectedChange;

    #[test]
    fn safe_default_validates_and_forbids_native_apply_modes() {
        let policy = safe_work_topology_policy_v1();
        policy.validate().unwrap();
        assert_eq!(
            policy.compute_digest().unwrap(),
            policy.compute_digest().unwrap()
        );
        assert!(!policy.cross_merge.has_native_apply_mode());
        assert!(policy.meets_protected_ref_floor());
        assert_eq!(
            policy.history_rewrite,
            HistoryRewritePolicyV1::ForbidForceAndRebase
        );
        assert_eq!(
            policy.retention.automatic_gc,
            AutomaticWorktreeGcV1::Disabled
        );
    }

    #[test]
    fn safe_default_digest_is_deterministic_and_round_trips() {
        let policy = safe_work_topology_policy_v1();
        let digest = policy.compute_digest().unwrap();
        assert_eq!(policy.compute_digest().unwrap(), digest);

        let encoded = serde_json::to_value(&policy).unwrap();
        let decoded: WorkTopologyPolicyV1 = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, policy);
        assert_eq!(decoded.compute_digest().unwrap(), digest);
    }

    #[test]
    fn github_stack_requires_probe_and_standard_fallback() {
        let mut policy = safe_work_topology_policy_v1();
        policy
            .review_topology
            .allowed
            .insert(ReviewTopologyKindV1::GitHubStackedPullRequests);
        assert!(policy.validate().is_err());

        policy.review_topology.github_stacked_prs =
            GitHubStackedPullRequestPolicyV1::ProbePrivatePreview;
        policy.validate().unwrap();

        policy.review_topology.allowed =
            BTreeSet::from([ReviewTopologyKindV1::GitHubStackedPullRequests]);
        assert!(policy.validate().is_err());
    }

    #[test]
    fn protected_ref_floor_is_a_publication_and_forward_rollback_invariant() {
        let mut weakened = safe_work_topology_policy_v1();
        weakened.protected_refs.remove(0);
        assert!(!weakened.meets_protected_ref_floor());
        assert!(weakened.validate().is_err());
        assert!(
            ProtectedChange::ReplaceWorkTopologyPolicy(weakened)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn topology_dimensions_remain_independent() {
        let mut branch_only = safe_work_topology_policy_v1();
        branch_only
            .branch_topology
            .allowed
            .insert(BranchTopologyKindV1::LocalStack);
        branch_only.validate().unwrap();

        let mut review_only = safe_work_topology_policy_v1();
        review_only
            .review_topology
            .allowed
            .insert(ReviewTopologyKindV1::GitHubStackedPullRequests);
        review_only.review_topology.github_stacked_prs =
            GitHubStackedPullRequestPolicyV1::ProbePrivatePreview;
        review_only.validate().unwrap();

        let mut placement_only = safe_work_topology_policy_v1();
        placement_only.placement = WorktreePlacementModeV1::RepositoryLocalRoot;
        placement_only.validate().unwrap();
    }

    #[test]
    fn force_and_rebase_are_unrepresentable() {
        let encoded = serde_json::to_value(HistoryRewritePolicyV1::ForbidForceAndRebase).unwrap();
        assert_eq!(encoded, "forbid_force_and_rebase");
        assert!(
            serde_json::from_value::<HistoryRewritePolicyV1>(serde_json::json!("allow_force"))
                .is_err()
        );
    }

    #[test]
    fn configured_roots_require_exact_matching_root() {
        let mut policy = safe_work_topology_policy_v1();
        policy.placement = WorktreePlacementModeV1::ConfiguredRoot(
            WorktreePlacementRootId::new("root.missing").unwrap(),
        );
        assert!(policy.validate().is_err());
    }
}
