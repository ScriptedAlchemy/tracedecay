use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{DomainError, UtcMicros};

/// Maximum lifetime of one explicit Work expertise consent grant.
pub const MAX_WORK_EXPERTISE_CONSENT_LIFETIME_MICROS_V1: i64 = 30 * 24 * 60 * 60 * 1_000_000;

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum WorkExpertiseCategoryV1 {
    Language,
    Framework,
    Architecture,
    Testing,
    Operations,
    Security,
    Domain,
}

/// Explicit, expiring consent for ephemeral expertise context.
///
/// User-profile and project authorization are separate registered settings;
/// Work requires both and uses only their category intersection.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkExpertiseConsentV1 {
    pub schema_version: u16,
    pub enabled: bool,
    pub granted_at: Option<UtcMicros>,
    pub expires_at: Option<UtcMicros>,
    pub allowed_categories: BTreeSet<WorkExpertiseCategoryV1>,
}

impl WorkExpertiseConsentV1 {
    pub const SCHEMA_VERSION: u16 = 1;

    pub const fn disabled() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            enabled: false,
            granted_at: None,
            expires_at: None,
            allowed_categories: BTreeSet::new(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != Self::SCHEMA_VERSION {
            return Err(DomainError::NonCanonical {
                field: "work expertise consent schema version",
            });
        }
        if !self.enabled {
            if self.granted_at.is_none()
                && self.expires_at.is_none()
                && self.allowed_categories.is_empty()
            {
                return Ok(());
            }
            return Err(DomainError::NonCanonical {
                field: "disabled work expertise consent",
            });
        }
        let (Some(granted_at), Some(expires_at)) = (self.granted_at, self.expires_at) else {
            return Err(DomainError::NonCanonical {
                field: "enabled work expertise consent timestamps",
            });
        };
        let Some(lifetime) = expires_at.0.checked_sub(granted_at.0) else {
            return Err(DomainError::NonCanonical {
                field: "work expertise consent lifetime",
            });
        };
        if lifetime <= 0
            || lifetime > MAX_WORK_EXPERTISE_CONSENT_LIFETIME_MICROS_V1
            || self.allowed_categories.is_empty()
        {
            return Err(DomainError::NonCanonical {
                field: "enabled work expertise consent",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_is_disabled_by_default() {
        WorkExpertiseConsentV1::disabled()
            .validate()
            .expect("disabled consent is canonical");
    }

    #[test]
    fn consent_rejects_an_unbounded_lifetime() {
        let consent = WorkExpertiseConsentV1 {
            schema_version: WorkExpertiseConsentV1::SCHEMA_VERSION,
            enabled: true,
            granted_at: Some(UtcMicros(1)),
            expires_at: Some(UtcMicros(2 + MAX_WORK_EXPERTISE_CONSENT_LIFETIME_MICROS_V1)),
            allowed_categories: BTreeSet::from([WorkExpertiseCategoryV1::Language]),
        };
        assert!(consent.validate().is_err());
    }
}
