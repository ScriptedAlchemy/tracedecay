//! Pure identities and typed outcomes for authorized multi-root operations.
//!
//! These contracts contain no paths or root-resolution behavior. Every root is
//! bound by the canonical digest of the already-resolved application scope.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{DomainError, ManifestDigest, canonical_sha256};

const ROOT_GENERATION_DIGEST_DOMAIN_V1: &str = "tracedecay.multi-root.generation.v1";

/// Stable identity of one authorized scope-set record.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ScopeSetId(String);

impl ScopeSetId {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 512
            || value.chars().any(char::is_control)
        {
            return Err(DomainError::NonCanonical {
                field: "scope set id",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        Self::new(self.0.clone()).map(|_| ())
    }
}

impl<'de> Deserialize<'de> for ScopeSetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for ScopeSetId {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for ScopeSetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Monotonic optimistic-concurrency revision of one scope set.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ScopeSetRevision(u64);

impl ScopeSetRevision {
    pub fn new(value: u64) -> Result<Self, DomainError> {
        if value == 0 {
            return Err(DomainError::NonCanonical {
                field: "scope set revision",
            });
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn checked_next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .ok_or(DomainError::NonCanonical {
                field: "scope set revision",
            })
            .and_then(Self::new)
    }

    pub fn validate(self) -> Result<(), DomainError> {
        Self::new(self.0).map(|_| ())
    }
}

impl<'de> Deserialize<'de> for ScopeSetRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

macro_rules! digest_revision {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(ManifestDigest);

        impl $name {
            pub fn new(value: ManifestDigest) -> Result<Self, DomainError> {
                value.validate()?;
                Ok(Self(value))
            }

            pub fn digest(&self) -> &ManifestDigest {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                self.0
                    .validate()
                    .map_err(|_| DomainError::NonCanonical { field: $field })
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(ManifestDigest::deserialize(deserializer)?)
                    .map_err(serde::de::Error::custom)
            }
        }
    };
}

digest_revision!(CollectionRevision, "collection revision");
digest_revision!(StackRevision, "stack revision");

/// Immutable collection and stack revisions for one exact resolved root.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RootGenerationV1 {
    pub scope_digest: ManifestDigest,
    pub collection_revision: CollectionRevision,
    pub stack_revision: StackRevision,
    pub generation_digest: ManifestDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootGenerationWireV1 {
    scope_digest: ManifestDigest,
    collection_revision: CollectionRevision,
    stack_revision: StackRevision,
    generation_digest: ManifestDigest,
}

impl<'de> Deserialize<'de> for RootGenerationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RootGenerationWireV1::deserialize(deserializer)?;
        let generation = Self::new(
            wire.scope_digest,
            wire.collection_revision,
            wire.stack_revision,
        )
        .map_err(serde::de::Error::custom)?;
        if generation.generation_digest != wire.generation_digest {
            return Err(serde::de::Error::custom(
                "root generation digest does not match its frozen revisions",
            ));
        }
        Ok(generation)
    }
}

impl RootGenerationV1 {
    pub fn new(
        scope_digest: ManifestDigest,
        collection_revision: CollectionRevision,
        stack_revision: StackRevision,
    ) -> Result<Self, DomainError> {
        scope_digest.validate()?;
        collection_revision.validate()?;
        stack_revision.validate()?;
        let generation_digest = canonical_sha256(&(
            ROOT_GENERATION_DIGEST_DOMAIN_V1,
            &scope_digest,
            &collection_revision,
            &stack_revision,
        ))?;
        Ok(Self {
            scope_digest,
            collection_revision,
            stack_revision,
            generation_digest,
        })
    }

    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        canonical_sha256(&(
            ROOT_GENERATION_DIGEST_DOMAIN_V1,
            &self.scope_digest,
            &self.collection_revision,
            &self.stack_revision,
        ))
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.scope_digest.validate()?;
        self.collection_revision.validate()?;
        self.stack_revision.validate()?;
        self.generation_digest.validate()?;
        if self.compute_digest()? != self.generation_digest {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }
}

/// Typed explanation for a root that returned usable but incomplete data.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopePartialReasonV1 {
    Incomplete,
    Stale,
    BudgetExceeded,
    RootDenied,
    RootUnavailable,
}

/// Typed explanation for a root that could not return usable data.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeUnavailableReasonV1 {
    AuthorityUnavailable,
    RootMissing,
    StoreUnavailable,
}

/// Truthful outcome for one authorized root or an aggregate over roots.
///
/// `Denied` and `Unavailable` carry no value and therefore cannot be confused
/// with a successful empty result.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "outcome", content = "value", rename_all = "snake_case")]
pub enum ScopeOutcome<T> {
    Exact(T),
    Partial {
        value: T,
        reason: ScopePartialReasonV1,
    },
    Denied,
    Unavailable {
        reason: ScopeUnavailableReasonV1,
    },
}

impl<T> ScopeOutcome<T> {
    pub const fn has_value(&self) -> bool {
        matches!(self, Self::Exact(_) | Self::Partial { .. })
    }

    pub fn as_ref(&self) -> ScopeOutcome<&T> {
        match self {
            Self::Exact(value) => ScopeOutcome::Exact(value),
            Self::Partial { value, reason } => ScopeOutcome::Partial {
                value,
                reason: *reason,
            },
            Self::Denied => ScopeOutcome::Denied,
            Self::Unavailable { reason } => ScopeOutcome::Unavailable { reason: *reason },
        }
    }

    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> ScopeOutcome<U> {
        match self {
            Self::Exact(value) => ScopeOutcome::Exact(map(value)),
            Self::Partial { value, reason } => ScopeOutcome::Partial {
                value: map(value),
                reason,
            },
            Self::Denied => ScopeOutcome::Denied,
            Self::Unavailable { reason } => ScopeOutcome::Unavailable { reason },
        }
    }
}

/// One typed outcome pinned to the digest of an exact resolved root.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RootScopeOutcomeV1<T> {
    pub scope_digest: ManifestDigest,
    pub outcome: ScopeOutcome<T>,
}

impl<T> RootScopeOutcomeV1<T> {
    pub fn new(
        scope_digest: ManifestDigest,
        outcome: ScopeOutcome<T>,
    ) -> Result<Self, DomainError> {
        scope_digest.validate()?;
        Ok(Self {
            scope_digest,
            outcome,
        })
    }
}

impl RootScopeOutcomeV1<RootGenerationV1> {
    pub fn validate_generation(&self) -> Result<(), DomainError> {
        self.scope_digest.validate()?;
        match &self.outcome {
            ScopeOutcome::Exact(generation)
            | ScopeOutcome::Partial {
                value: generation, ..
            } => {
                generation.validate()?;
                if generation.scope_digest != self.scope_digest {
                    return Err(DomainError::SnapshotMismatch {
                        field: "root generation scope",
                    });
                }
            }
            ScopeOutcome::Denied | ScopeOutcome::Unavailable { .. } => {}
        }
        Ok(())
    }
}
