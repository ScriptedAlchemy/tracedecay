use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::configuration::{SourceKindV1, UserProfileId};
use tracedecay_domain::{ActorId, ManifestDigest, ProjectId, UtcMicros, canonical_sha256};

/// A bounded, canonical identifier owned by the policy input schema.
///
/// It represents immutable references only; it is never a path, display
/// label, provider account, branch name, or native object identifier.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PolicyIdentifierV1(String);

impl PolicyIdentifierV1 {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 512
            || value.chars().any(char::is_control)
        {
            return Err("policy identifier must be non-empty, trimmed, bounded, and printable");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_valid(&self) -> bool {
        Self::new(self.0.clone()).is_ok()
    }
}

impl<'de> Deserialize<'de> for PolicyIdentifierV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for PolicyIdentifierV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub type SourceIdV1 = PolicyIdentifierV1;
pub type SourceBindingIdV1 = PolicyIdentifierV1;
pub type ResourceIdV1 = PolicyIdentifierV1;
pub type GrantIdV1 = PolicyIdentifierV1;

/// Owner identity is typed and exact. Mutable paths, collection membership,
/// labels, provider accounts, branch names, and native object IDs cannot
/// become owner authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SourceOwnerV1 {
    Project(ProjectId),
    Profile(UserProfileId),
}

impl SourceOwnerV1 {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Project(id) => id.validate().is_ok(),
            Self::Profile(id) => id.validate().is_ok(),
        }
    }
}

/// Operations that a source authorization decision may consider. The closed
/// enum prevents a caller from smuggling an unreviewed generic effect through
/// the policy boundary.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TypedOperationV1 {
    ProviderFetch,
    SourcePageContinuation,
    CanonicalAdmission,
    ShardSelection,
    StatisticsRead,
    GraphExpansion,
    Hydration,
    QueryPageContinuation,
    AnchorResolution,
    SummaryPublication,
    ModelContextDelivery,
    HostDelivery,
    UiRendering,
    Export,
    TelemetryWrite,
    AnalyzerAdmission,
    HistoricalRead,
}

/// A concrete sink receiving source-derived content or metadata.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SinkKindV1 {
    ProviderFetch,
    CanonicalStore,
    AnalyzerRuntime,
    LocalDurableStore,
    ModelContext,
    HostDelivery,
    UiRendering,
    Export,
    Telemetry,
    QueryResponse,
}

impl SinkKindV1 {
    pub const fn is_egress(self) -> bool {
        matches!(
            self,
            Self::ModelContext | Self::HostDelivery | Self::Export | Self::Telemetry
        )
    }
}

/// Ordered disclosure ceiling. Earlier variants are more restrictive.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureClassV1 {
    Metadata,
    Summary,
    SanitizedContent,
    RawContent,
}

/// Source sensitivity is an input classification, never inferred by policy.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SourceSensitivityV1 {
    NonSensitive,
    Sensitive,
    Restricted,
}

/// Non-waivable obligations accumulate across every authorization operand.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyConstraintV1 {
    LocalOnly,
    SanitizedOnly,
    NoRetention,
    NoModelContext,
    NoTelemetry,
    NoExport,
}

pub type PrivacyConstraintSetV1 = BTreeSet<PrivacyConstraintV1>;

/// Explicit resource limits. Intersections take the pointwise minimum.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BudgetSetV1 {
    pub requests: u64,
    pub bytes: u64,
    pub tokens: u64,
}

impl BudgetSetV1 {
    pub fn pointwise_min(&self, other: &Self) -> Self {
        Self {
            requests: self.requests.min(other.requests),
            bytes: self.bytes.min(other.bytes),
            tokens: self.tokens.min(other.tokens),
        }
    }

    pub fn contains(&self, requested: &Self) -> bool {
        requested.requests <= self.requests
            && requested.bytes <= self.bytes
            && requested.tokens <= self.tokens
    }
}

/// Immutable source-capture/storage identity. It intentionally contains no
/// owner, sink, disclosure, local privacy, or grant authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDefinitionV1 {
    pub source_id: SourceIdV1,
    pub source_kind: SourceKindV1,
    pub revision: u64,
    pub digest: ManifestDigest,
}

impl SourceDefinitionV1 {
    pub fn is_valid(&self) -> bool {
        self.source_id.is_valid() && self.revision > 0 && self.digest.validate().is_ok()
    }
}

/// Exact immutable definition snapshot supplied by the configuration
/// authority. Policy does not create, mutate, or persist this value.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceDefinitionSnapshotV1 {
    pub definition: SourceDefinitionV1,
}

impl SourceDefinitionSnapshotV1 {
    pub fn is_valid(&self) -> bool {
        self.definition.is_valid()
    }
}

/// Binding identity attaches one definition to exactly one typed owner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceBindingV1 {
    Project {
        binding_id: SourceBindingIdV1,
        source_id: SourceIdV1,
        project_id: ProjectId,
        revision: u64,
        digest: ManifestDigest,
    },
    Profile {
        binding_id: SourceBindingIdV1,
        source_id: SourceIdV1,
        profile_id: UserProfileId,
        revision: u64,
        digest: ManifestDigest,
    },
}

impl SourceBindingV1 {
    pub fn source_id(&self) -> &SourceIdV1 {
        match self {
            Self::Project { source_id, .. } | Self::Profile { source_id, .. } => source_id,
        }
    }

    pub fn owner(&self) -> SourceOwnerV1 {
        match self {
            Self::Project { project_id, .. } => SourceOwnerV1::Project(project_id.clone()),
            Self::Profile { profile_id, .. } => SourceOwnerV1::Profile(profile_id.clone()),
        }
    }

    pub fn revision(&self) -> u64 {
        match self {
            Self::Project { revision, .. } | Self::Profile { revision, .. } => *revision,
        }
    }

    pub fn digest(&self) -> &ManifestDigest {
        match self {
            Self::Project { digest, .. } | Self::Profile { digest, .. } => digest,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::Project {
                binding_id,
                source_id,
                project_id,
                revision,
                digest,
            } => {
                binding_id.is_valid()
                    && source_id.is_valid()
                    && project_id.validate().is_ok()
                    && *revision > 0
                    && digest.validate().is_ok()
            }
            Self::Profile {
                binding_id,
                source_id,
                profile_id,
                revision,
                digest,
            } => {
                binding_id.is_valid()
                    && source_id.is_valid()
                    && profile_id.validate().is_ok()
                    && *revision > 0
                    && digest.validate().is_ok()
            }
        }
    }
}

/// Exact immutable binding snapshot supplied by the configuration authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceBindingSnapshotV1 {
    pub binding: SourceBindingV1,
}

impl SourceBindingSnapshotV1 {
    pub fn is_valid(&self) -> bool {
        self.binding.is_valid()
    }
}

/// Typed owner resolution with an explicit revision and digest. A caller must
/// not substitute paths or display labels for this authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedOwnerScopeV1 {
    pub owner: SourceOwnerV1,
    pub revision: u64,
    pub digest: ManifestDigest,
}

impl ResolvedOwnerScopeV1 {
    pub fn is_valid(&self) -> bool {
        self.owner.is_valid() && self.revision > 0 && self.digest.validate().is_ok()
    }
}

/// Plan-20 source policy metadata. This is deliberately separate from source
/// definition identity so mutable policy cannot become capture identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourcePolicyMetadataSnapshotV1 {
    pub source_id: SourceIdV1,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub sensitivity: SourceSensitivityV1,
    pub disclosure_ceiling: DisclosureClassV1,
    pub eligible_sinks: BTreeSet<SinkKindV1>,
    pub eligible_operations: BTreeSet<TypedOperationV1>,
    pub mandatory_privacy: PrivacyConstraintSetV1,
}

impl SourcePolicyMetadataSnapshotV1 {
    pub fn is_valid(&self) -> bool {
        self.source_id.is_valid()
            && self.policy_revision > 0
            && self.policy_digest.validate().is_ok()
    }
}

/// Current sink policy supplied by the owning configuration/application
/// authority. It is a read-only policy input to this crate.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SinkPolicySnapshotV1 {
    pub sink: SinkKindV1,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub disclosure_ceiling: DisclosureClassV1,
    pub mandatory_privacy: PrivacyConstraintSetV1,
    pub available: bool,
}

impl SinkPolicySnapshotV1 {
    pub fn is_valid(&self) -> bool {
        self.policy_revision > 0 && self.policy_digest.validate().is_ok()
    }
}

/// The exact requested subset that must be contained by the effective grant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequestedSourceAccessV1 {
    pub resource: ResourceIdV1,
    pub operation: TypedOperationV1,
    pub sink: SinkKindV1,
    pub disclosure: DisclosureClassV1,
    pub budget: BudgetSetV1,
}

impl RequestedSourceAccessV1 {
    pub fn is_valid(&self) -> bool {
        self.resource.is_valid()
    }
}

/// Explicit completeness/freshness of required immutable inputs.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationSnapshotStateV1 {
    Complete,
    Partial,
    Missing,
    Stale,
    Ambiguous,
}

/// Content truth is independent from access truth. Policy never infers
/// authoritative deletion from access loss or incomplete input.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExternalContentStatusV1 {
    Live,
    Partial,
    TemporarilyUnavailable,
    AuthoritativeDeleted,
}

/// Authorization coverage is separate from content status. It captures a
/// mixed visible/authorized resource set without exposing hidden counts.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationCoverageV1 {
    Complete,
    Partial,
}

/// All policy inputs are immutable values. The caller supplies the clock and
/// all source/policy/sink state; policy performs no lookup or refresh.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAuthorizationInputV1 {
    pub definition: SourceDefinitionSnapshotV1,
    pub binding: SourceBindingSnapshotV1,
    pub source_grant: super::grant::CapabilityGrantV1,
    pub requester_grant: super::grant::CapabilityGrantV1,
    pub resolved_owner_scope: ResolvedOwnerScopeV1,
    pub requested_access: RequestedSourceAccessV1,
    pub source_policy: SourcePolicyMetadataSnapshotV1,
    pub sink_policy: SinkPolicySnapshotV1,
    pub content_status: ExternalContentStatusV1,
    pub requested_coverage: AuthorizationCoverageV1,
    pub snapshot_state: AuthorizationSnapshotStateV1,
    pub requester: ActorId,
    pub policy_revision: u64,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    /// Immutable evidence references only. The evaluator records these in its
    /// trace but never dereferences, fetches, or persists them.
    #[serde(default)]
    pub evidence_references: BTreeSet<PolicyIdentifierV1>,
    pub evaluated_at: UtcMicros,
}

impl SourceAuthorizationInputV1 {
    pub fn is_structurally_valid(&self) -> bool {
        self.definition.is_valid()
            && self.binding.is_valid()
            && self.source_grant.is_valid()
            && self.requester_grant.is_valid()
            && self.resolved_owner_scope.is_valid()
            && self.requested_access.is_valid()
            && self.source_policy.is_valid()
            && self.sink_policy.is_valid()
            && self.requester.validate().is_ok()
            && self.policy_revision > 0
            && self.policy_digest.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
    }

    /// Digest of all input facts, including explicit time and content state.
    pub fn input_digest(&self) -> ManifestDigest {
        policy_digest("tracedecay.policy.source-authorization-input.v1", self)
    }

    /// Digest of every authority/configuration surface that a sink proof pins.
    /// The explicit clock and content truth are intentionally excluded: they
    /// are re-evaluated at the sink, rather than silently reused.
    pub fn authority_fingerprint(&self) -> ManifestDigest {
        #[derive(Serialize)]
        struct AuthoritySurface<'a> {
            definition: &'a SourceDefinitionSnapshotV1,
            binding: &'a SourceBindingSnapshotV1,
            source_grant: &'a super::grant::CapabilityGrantV1,
            requester_grant: &'a super::grant::CapabilityGrantV1,
            resolved_owner_scope: &'a ResolvedOwnerScopeV1,
            requested_access: &'a RequestedSourceAccessV1,
            source_policy: &'a SourcePolicyMetadataSnapshotV1,
            sink_policy: &'a SinkPolicySnapshotV1,
            requester: &'a ActorId,
            policy_revision: u64,
            policy_digest: &'a ManifestDigest,
            configuration_digest: &'a ManifestDigest,
        }

        policy_digest(
            "tracedecay.policy.source-authorization-authority-surface.v1",
            &AuthoritySurface {
                definition: &self.definition,
                binding: &self.binding,
                source_grant: &self.source_grant,
                requester_grant: &self.requester_grant,
                resolved_owner_scope: &self.resolved_owner_scope,
                requested_access: &self.requested_access,
                source_policy: &self.source_policy,
                sink_policy: &self.sink_policy,
                requester: &self.requester,
                policy_revision: self.policy_revision,
                policy_digest: &self.policy_digest,
                configuration_digest: &self.configuration_digest,
            },
        )
    }
}

/// Stable digest helper used for immutable, serializable policy inputs.
pub(crate) fn policy_digest<T: Serialize>(domain: &'static str, value: &T) -> ManifestDigest {
    match canonical_sha256(&(domain, value)) {
        Ok(digest) => digest,
        Err(_) => {
            // This can only be reached if a future serializable policy type
            // violates canonical JSON requirements. Preserve a deterministic
            // non-authorizing digest rather than panic or consult external
            // state.
            ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))
                .expect("static policy fallback digest is canonical")
        }
    }
}
