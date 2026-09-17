use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::id::{BindingId, CapabilityId, FeatureId};
use crate::manifest::canonicalize_set;
use crate::validation::CatalogValidationError;

/// Product surface taxonomy. These are references only; no adapter behavior
/// lives in the catalog crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSurface {
    Cli,
    Mcp,
    Http,
    Lsp,
    Dashboard,
}

/// Stable syntax owned by an adapter for one catalog binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SurfaceOperationName(String);

impl SurfaceOperationName {
    pub fn new(value: impl Into<String>) -> Result<Self, CatalogValidationError> {
        let value = value.into();
        if value.is_empty()
            || value.trim() != value
            || value.len() > 192
            || !value.is_ascii()
            || value
                .bytes()
                .any(|byte| !(byte.is_ascii_graphic() || byte == b' '))
            || value.contains("  ")
        {
            return Err(CatalogValidationError::InvalidValue {
                field: "surface operation name",
                reason: "must be a bounded canonical printable ASCII spelling",
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SurfaceOperationName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SurfaceOperationName {
    type Err = CatalogValidationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for SurfaceOperationName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Supported protocol revisions for a surface binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProtocolRevisionRange {
    minimum: u32,
    maximum: u32,
}

impl ProtocolRevisionRange {
    pub fn new(minimum: u32, maximum: u32) -> Result<Self, CatalogValidationError> {
        if minimum == 0 || maximum < minimum {
            return Err(CatalogValidationError::InvalidValue {
                field: "protocol revision range",
                reason: "revisions must be non-zero and ordered",
            });
        }
        Ok(Self { minimum, maximum })
    }

    pub const fn minimum(&self) -> u32 {
        self.minimum
    }

    pub const fn maximum(&self) -> u32 {
        self.maximum
    }

    pub const fn contains(&self, revision: u32) -> bool {
        revision >= self.minimum && revision <= self.maximum
    }
}

/// A bounded deprecation period for a formerly current surface spelling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BindingDeprecation {
    sunset_revision: u32,
}

impl BindingDeprecation {
    pub fn new(sunset_revision: u32) -> Result<Self, CatalogValidationError> {
        if sunset_revision == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "binding deprecation sunset revision",
                reason: "must be greater than zero",
            });
        }
        Ok(Self { sunset_revision })
    }

    pub const fn sunset_revision(&self) -> u32 {
        self.sunset_revision
    }
}

/// Lifecycle state of a surface spelling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BindingStatus {
    Current,
    Deprecated { deprecation: BindingDeprecation },
}

/// Input used to construct an immutable surface binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceBindingInputV1 {
    pub binding_id: BindingId,
    pub capability_id: CapabilityId,
    pub surface: BindingSurface,
    pub operation: SurfaceOperationName,
    pub protocol_revisions: ProtocolRevisionRange,
    pub required_features: Vec<FeatureId>,
    pub status: BindingStatus,
    pub alias_of: Option<BindingId>,
}

/// A surface spelling pointing at exactly one capability.
///
/// It deliberately contains no request schema, handler, authorization, effect,
/// storage, or fallback metadata. Those semantic fields resolve from the
/// referenced capability manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SurfaceBindingV1 {
    binding_id: BindingId,
    capability_id: CapabilityId,
    surface: BindingSurface,
    operation: SurfaceOperationName,
    protocol_revisions: ProtocolRevisionRange,
    required_features: Vec<FeatureId>,
    status: BindingStatus,
    alias_of: Option<BindingId>,
}

impl SurfaceBindingV1 {
    pub fn new(input: SurfaceBindingInputV1) -> Result<Self, CatalogValidationError> {
        let mut required_features = input.required_features;
        canonicalize_set(&mut required_features, "binding required features")?;
        Ok(Self {
            binding_id: input.binding_id,
            capability_id: input.capability_id,
            surface: input.surface,
            operation: input.operation,
            protocol_revisions: input.protocol_revisions,
            required_features,
            status: input.status,
            alias_of: input.alias_of,
        })
    }

    pub fn binding_id(&self) -> &BindingId {
        &self.binding_id
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub const fn surface(&self) -> BindingSurface {
        self.surface
    }

    pub fn operation(&self) -> &SurfaceOperationName {
        &self.operation
    }

    pub fn protocol_revisions(&self) -> &ProtocolRevisionRange {
        &self.protocol_revisions
    }

    pub fn required_features(&self) -> &[FeatureId] {
        &self.required_features
    }

    pub fn status(&self) -> &BindingStatus {
        &self.status
    }

    pub fn alias_of(&self) -> Option<&BindingId> {
        self.alias_of.as_ref()
    }

    pub const fn is_alias(&self) -> bool {
        self.alias_of.is_some()
    }
}
