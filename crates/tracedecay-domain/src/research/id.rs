use std::collections::BTreeSet;
use std::fmt;
use std::ops::Deref;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::DomainError;

pub(crate) fn validate_canonical_string(
    value: &str,
    field: &'static str,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if value.trim() != value || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

macro_rules! string_id {
    ($($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed canonical identity: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                validate_canonical_string(&value, stringify!($name))?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                validate_canonical_string(&self.0, stringify!($name))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    )+};
}

/// Reject values that are not an algorithm-tagged, lowercase-hex integrity
/// digest: `sha256:`/`blake3:` over 64 hex characters, `sha512:` over 128.
///
/// Every digest newtype in the domain — research, code-intelligence, and
/// retrieval alike — accepts and rejects exactly this set.
pub(crate) fn validate_integrity_digest(
    value: &str,
    field: &'static str,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }

    let valid = value
        .split_once(':')
        .and_then(|(algorithm, encoded)| {
            let expected_len = match algorithm {
                "sha256" | "blake3" => 64,
                "sha512" => 128,
                _ => return None,
            };
            Some(
                encoded.len() == expected_len
                    && encoded
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            )
        })
        .unwrap_or(false);

    if !valid {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// Emit the constructor, accessor, validator, and conversions shared by every
/// arm of [`digest_id!`].
macro_rules! digest_id_body {
    ($name:ident, $error:ty, $map:path) => {
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, $error> {
                let value = value.into();
                $crate::research::id::validate_integrity_digest(&value, stringify!($name))
                    .map_err($map)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), $error> {
                $crate::research::id::validate_integrity_digest(&self.0, stringify!($name))
                    .map_err($map)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

/// Declare one or more algorithm-tagged integrity-digest newtypes.
///
/// `$error` is the contract error the constructors surface and `$map` converts
/// the shared [`validate_integrity_digest`] failure into it, so modules that
/// already speak [`DomainError`] pass `std::convert::identity`. The `@schema`
/// arm additionally derives `JsonSchema`.
macro_rules! digest_id {
    (@schema $error:ty, $map:path; $($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed algorithm-tagged integrity digest: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::research::id::digest_id_body!($name, $error, $map);
    )+};

    ($error:ty, $map:path; $($name:ident),+ $(,)?) => {$(
        #[doc = concat!("Strongly typed algorithm-tagged integrity digest: `", stringify!($name), "`.")]
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        $crate::research::id::digest_id_body!($name, $error, $map);
    )+};
}

pub(crate) use digest_id;
pub(crate) use digest_id_body;

string_id!(
    EntityId,
    EntityVersionId,
    ProviderId,
    HostInstanceId,
    SourceStoreId,
    SourceInstanceId,
    SessionId,
    ThreadId,
    TurnId,
    MessageId,
    AgentInstanceId,
    ToolInvocationId,
    OrchestrationObservationId,
    OrchestrationAgentLabel,
    GoalId,
    TaskId,
    RunId,
    AttemptId,
    ProposalId,
    WorkCommandId,
    WorkLeaseId,
    WorkArtifactId,
    WorkCancellationRequestId,
    WorkProviderRouteId,
    WorkflowDefinitionId,
    WorkflowStepId,
    WorkflowOutputName,
    WorkflowOperationRef,
    RepositoryId,
    ProjectId,
    WorktreeId,
    WorktreeInventorySnapshotId,
    BranchStackId,
    BranchStackRevisionId,
    StackNodeId,
    RefId,
    CommitId,
    TreeId,
    BlobId,
    RepositoryCaptureId,
    ProjectionGenerationId,
    ObservationId,
    FactId,
    FactAssertionId,
    FactEvidenceId,
    FactEventId,
    ResearchAnchorId,
    RetrievalAnchorId,
    CanonicalSourceOccurrenceSetIdV1,
    RetrieverContributionIdV1,
    EvidenceSpanProjectionReceiptIdV1,
    EvidenceAssemblyPublicationReceiptIdV1,
    RetrievalRecipeId,
    ResearchManifestId,
    ManifestId,
    PrivacyDomainId,
    ShardId,
    ActorId,
    SanitizationReceiptId,
    ComponentVersion,
    SchemaVersion,
    CatalogGenerationId,
    UseCaseId,
    CapabilityId,
    AuditReceiptId,
    QueryId,
    ScopeResolutionId,
    ProvenanceId,
    StoreAuthorityId,
    BrainNodeId,
    BrainId,
);

digest_id!(
    @schema DomainError, std::convert::identity;
    ManifestDigest,
    LocatorDigest,
    AccessPolicyDigest,
    RegistryManifestDigest,
    DataVersionDigest,
);

/// Monotonic epoch of the writer authority for a shard.
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(transparent)]
pub struct AuthorityEpoch(pub u64);

/// A serialized sequence that is guaranteed to be non-empty and identity-unique.
///
/// This helper is intentionally narrow: it exists for the three research contracts
/// whose anchor lists otherwise repeated identical empty/duplicate validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmptyUniqueVec<T>(Vec<T>);

impl<T: Ord> NonEmptyUniqueVec<T> {
    pub fn new(values: Vec<T>, field: &'static str) -> Result<Self, DomainError> {
        if values.is_empty() {
            return Err(DomainError::Empty { field });
        }
        ensure_unique(values.iter(), field)?;
        Ok(Self(values))
    }
}

impl<T> NonEmptyUniqueVec<T> {
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl<T> Deref for NonEmptyUniqueVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Serialize> Serialize for NonEmptyUniqueVec<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for NonEmptyUniqueVec<T>
where
    T: Deserialize<'de> + Ord,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(
            Vec::<T>::deserialize(deserializer)?,
            "non-empty unique collection",
        )
        .map_err(serde::de::Error::custom)
    }
}

pub(crate) fn ensure_unique<'a, T, I>(values: I, field: &'static str) -> Result<(), DomainError>
where
    T: 'a + Ord,
    I: IntoIterator<Item = &'a T>,
{
    let mut seen = BTreeSet::new();
    if values.into_iter().any(|value| !seen.insert(value)) {
        return Err(DomainError::DuplicateId { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_digest_types_accept_supported_algorithms() {
        let sha256 = format!("sha256:{}", "a".repeat(64));
        let sha512 = format!("sha512:{}", "b".repeat(128));
        let blake3 = format!("blake3:{}", "c".repeat(64));

        assert!(ManifestDigest::new(&sha256).is_ok());
        assert!(LocatorDigest::new(&sha256).is_ok());
        assert!(AccessPolicyDigest::new(&sha256).is_ok());
        assert!(RegistryManifestDigest::new(&sha256).is_ok());
        assert!(DataVersionDigest::new(&sha256).is_ok());
        assert!(ManifestDigest::new(sha512).is_ok());
        assert!(ManifestDigest::new(blake3).is_ok());
    }

    #[test]
    fn integrity_digests_reject_non_cryptographic_or_noncanonical_values() {
        let malformed = [
            "catalog-digest-synthetic-001".to_owned(),
            "a".repeat(64),
            format!("md5:{}", "a".repeat(32)),
            format!("SHA256:{}", "a".repeat(64)),
            format!("sha256:{}A", "a".repeat(63)),
            format!("sha256:{}g", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
        ];

        for value in malformed {
            assert!(
                ManifestDigest::new(&value).is_err(),
                "accepted malformed digest {value}"
            );
        }
    }

    #[test]
    fn integrity_digest_deserialization_is_checked() {
        let value = serde_json::json!("catalog-digest-synthetic-001");
        assert!(serde_json::from_value::<ManifestDigest>(value).is_err());
    }
}
