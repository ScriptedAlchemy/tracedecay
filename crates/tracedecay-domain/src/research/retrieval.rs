use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::id::LocatorDigest;
use super::time::UtcMicros;

/// Keyed locator digest whose value is meaningful only inside its privacy domain.
///
/// This is intentionally not interchangeable with [`LocatorDigest`]. Callers
/// must construct it through the validating string constructor after computing
/// the locator digest with the privacy-domain key.
///
/// ```compile_fail,E0308
/// use tracedecay_domain::research::{LocatorDigest, PrivacyDomainBoundLocatorDigest};
///
/// fn cannot_use_unkeyed_digest(digest: LocatorDigest) {
///     let _: PrivacyDomainBoundLocatorDigest = digest;
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PrivacyDomainBoundLocatorDigest(LocatorDigest);

impl PrivacyDomainBoundLocatorDigest {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        Self::try_from(value.into())
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.0.validate()
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl TryFrom<String> for PrivacyDomainBoundLocatorDigest {
    type Error = DomainError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        LocatorDigest::try_from(value).map(Self)
    }
}

impl TryFrom<&str> for PrivacyDomainBoundLocatorDigest {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_from(value.to_owned())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadAccessState {
    Eligible,
    Redacted,
    Quarantined,
    RetentionExpired,
    Deleted,
    Unavailable,
    Ambiguous,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AnchorDurabilityClass {
    DurableEvidence,
    RetentionBound { expires_at: UtcMicros },
    Archived,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZERO_SHA256: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn privacy_domain_bound_locator_digest_validates_and_round_trips() {
        let locator = PrivacyDomainBoundLocatorDigest::new(ZERO_SHA256).unwrap();
        let locator_json = serde_json::to_string(&locator).unwrap();
        assert_eq!(locator_json, format!("\"{ZERO_SHA256}\""));
        assert_eq!(
            serde_json::from_str::<PrivacyDomainBoundLocatorDigest>(&locator_json).unwrap(),
            locator
        );

        assert!(PrivacyDomainBoundLocatorDigest::new("not-a-digest").is_err());
    }

    #[test]
    fn privacy_domain_bound_locator_digest_is_not_a_generic_digest_alias() {
        use std::any::TypeId;

        assert_ne!(
            TypeId::of::<PrivacyDomainBoundLocatorDigest>(),
            TypeId::of::<LocatorDigest>()
        );
    }
}
