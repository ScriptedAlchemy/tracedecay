//! Canonical session refresh target and idempotency-key contracts.

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::SessionId;

use super::coverage::SessionSourceFrontierV1;
use super::occurrence::{SessionContractError, SessionSourceIdV1};

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshSourceTargetV1 {
    source_id: SessionSourceIdV1,
    observed_frontier: SessionSourceFrontierV1,
    target_watermark: SessionSourceFrontierV1,
}

impl SessionRefreshSourceTargetV1 {
    pub fn new(
        source_id: SessionSourceIdV1,
        observed_frontier: SessionSourceFrontierV1,
        target_watermark: SessionSourceFrontierV1,
    ) -> Result<Self, SessionContractError> {
        if target_watermark < observed_frontier {
            return Err(SessionContractError::InvalidRefreshSourceFrontier);
        }
        Ok(Self {
            source_id,
            observed_frontier,
            target_watermark,
        })
    }

    pub fn source_id(&self) -> &SessionSourceIdV1 {
        &self.source_id
    }

    pub const fn observed_frontier(&self) -> SessionSourceFrontierV1 {
        self.observed_frontier
    }

    pub const fn target_watermark(&self) -> SessionSourceFrontierV1 {
        self.target_watermark
    }
}

impl<'de> Deserialize<'de> for SessionRefreshSourceTargetV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source_id: SessionSourceIdV1,
            observed_frontier: SessionSourceFrontierV1,
            target_watermark: SessionSourceFrontierV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.source_id,
            wire.observed_frontier,
            wire.target_watermark,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SessionRefreshKeyV1 {
    store_root_id: String,
    session_id: SessionId,
    sources: Vec<SessionRefreshSourceTargetV1>,
    projector_version: String,
    configuration_digest: String,
}

impl SessionRefreshKeyV1 {
    pub fn new(
        store_root_id: impl Into<String>,
        session_id: SessionId,
        mut sources: Vec<SessionRefreshSourceTargetV1>,
        projector_version: impl Into<String>,
        configuration_digest: impl Into<String>,
    ) -> Result<Self, SessionContractError> {
        let store_root_id = canonical_component(store_root_id.into(), "store_root_id")?;
        let projector_version = canonical_component(projector_version.into(), "projector_version")?;
        let configuration_digest =
            canonical_component(configuration_digest.into(), "configuration_digest")?;
        if sources.is_empty() {
            return Err(SessionContractError::RefreshSourcesRequired);
        }
        sources.sort();
        if sources
            .windows(2)
            .any(|pair| pair[0].source_id == pair[1].source_id)
        {
            return Err(SessionContractError::DuplicateRefreshSource);
        }
        Ok(Self {
            store_root_id,
            session_id,
            sources,
            projector_version,
            configuration_digest,
        })
    }

    pub fn sources(&self) -> &[SessionRefreshSourceTargetV1] {
        &self.sources
    }

    pub fn store_root_id(&self) -> &str {
        &self.store_root_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn projector_version(&self) -> &str {
        &self.projector_version
    }

    pub fn configuration_digest(&self) -> &str {
        &self.configuration_digest
    }
}

impl<'de> Deserialize<'de> for SessionRefreshKeyV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            store_root_id: String,
            session_id: SessionId,
            sources: Vec<SessionRefreshSourceTargetV1>,
            projector_version: String,
            configuration_digest: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.store_root_id,
            wire.session_id,
            wire.sources,
            wire.projector_version,
            wire.configuration_digest,
        )
        .map_err(serde::de::Error::custom)
    }
}

fn canonical_component(value: String, field: &'static str) -> Result<String, SessionContractError> {
    if crate::canonical_text::is_canonical_text_within(
        &value,
        crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
    ) {
        Ok(value)
    } else {
        Err(SessionContractError::InvalidIdentity { field })
    }
}
