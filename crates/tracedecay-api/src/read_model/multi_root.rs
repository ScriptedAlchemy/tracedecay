//! Typed dashboard/HTTP projection of the canonical multi-root query page.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::{AuthorizedScopeSet, MultiRootQueryPageV1};
use tracedecay_domain::{ManifestDigest, ScopeSetId, ScopeSetRevision};

/// Capability discovery never infers multi-root support from filesystem paths.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MultiRootCapabilityV1 {
    Mounted {
        scope_set_id: ScopeSetId,
        revision: ScopeSetRevision,
        scope_set_digest: ManifestDigest,
        root_count: u32,
    },
    Unavailable {
        reason: String,
    },
}

impl MultiRootCapabilityV1 {
    pub fn mounted(scope_set: &AuthorizedScopeSet) -> Self {
        Self::Mounted {
            scope_set_id: scope_set.scope_set_id().clone(),
            revision: scope_set.revision(),
            scope_set_digest: scope_set.digest().clone(),
            root_count: u32::try_from(scope_set.roots().len()).unwrap_or(u32::MAX),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }
}

/// Wire-stable projection. The application page already owns every
/// continuation and per-root truthfulness invariant, so the API does not
/// reconstruct or flatten it.
#[derive(Clone, Debug, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(transparent)]
#[schemars(rename = "MultiRootQueryReadModelV1")]
pub struct MultiRootQueryReadModelV1<T>(
    #[schemars(with = "MultiRootQueryPageV1<serde_json::Value>")] pub MultiRootQueryPageV1<T>,
);

impl<T> From<MultiRootQueryPageV1<T>> for MultiRootQueryReadModelV1<T> {
    fn from(page: MultiRootQueryPageV1<T>) -> Self {
        Self(page)
    }
}
