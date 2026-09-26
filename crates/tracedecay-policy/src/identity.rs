//! Identifiers and the canonical digest shared by the policy evaluators.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

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
        Self::validate(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_valid(&self) -> bool {
        Self::validate(&self.0).is_ok()
    }

    fn validate(value: &str) -> Result<(), &'static str> {
        if value.is_empty()
            || value.trim() != value
            || value.len() > 512
            || value.chars().any(char::is_control)
        {
            return Err("policy identifier must be non-empty, trimmed, bounded, and printable");
        }
        Ok(())
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
