//! Session summary publication and source-horizon contracts.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{
    ComponentVersion, DataVersionDigest, RetrievalAnchorId, SanitizationReceiptRefV1, SessionId,
    UtcMicros,
};

use super::occurrence::{SessionContractError, SessionSummaryIdV1};

/// Exact source-time horizon covered by an immutable summary.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct SummarySourceHorizonV1 {
    pub knowledge_through: UtcMicros,
    pub valid_through: Option<UtcMicros>,
}

impl SummarySourceHorizonV1 {
    pub fn validate(self) -> Result<(), SessionContractError> {
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SummarySourceHorizonV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            knowledge_through: UtcMicros,
            #[serde(deserialize_with = "deserialize_required_option")]
            valid_through: Option<UtcMicros>,
        }

        fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
        where
            D: Deserializer<'de>,
            T: Deserialize<'de>,
        {
            Option::deserialize(deserializer)
        }

        let wire = Wire::deserialize(deserializer)?;
        let horizon = Self {
            knowledge_through: wire.knowledge_through,
            valid_through: wire.valid_through,
        };
        horizon.validate().map_err(serde::de::Error::custom)?;
        Ok(horizon)
    }
}

/// Publication metadata that binds a summary to its route and sanitization.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SummaryPublicationMetadataV1 {
    pub model_route: ComponentVersion,
    pub configuration_digest: DataVersionDigest,
    pub sanitization_receipt: SanitizationReceiptRefV1,
}

impl SummaryPublicationMetadataV1 {
    pub fn validate(&self) -> Result<(), SessionContractError> {
        self.model_route
            .validate()
            .and_then(|_| self.configuration_digest.validate())
            .and_then(|_| self.sanitization_receipt.validate())
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "summary publication metadata",
            })
    }
}

/// Immutable summary node with exact, identity-unique source anchors.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSummaryRecordV1 {
    summary_id: SessionSummaryIdV1,
    session_id: SessionId,
    summary_anchor_id: RetrievalAnchorId,
    source_anchors: Vec<RetrievalAnchorId>,
    source_horizon: SummarySourceHorizonV1,
    created_at: UtcMicros,
    predecessor_summary_id: Option<SessionSummaryIdV1>,
    publication: Option<SummaryPublicationMetadataV1>,
}

impl SessionSummaryRecordV1 {
    pub fn new(
        summary_id: SessionSummaryIdV1,
        session_id: SessionId,
        summary_anchor_id: RetrievalAnchorId,
        source_anchors: Vec<RetrievalAnchorId>,
        source_horizon: SummarySourceHorizonV1,
        created_at: UtcMicros,
    ) -> Result<Self, SessionContractError> {
        if source_anchors.is_empty() {
            return Err(SessionContractError::SummarySourcesRequired);
        }
        let mut unique = BTreeSet::new();
        if source_anchors
            .iter()
            .any(|source| !unique.insert(source.clone()))
        {
            return Err(SessionContractError::DuplicateSummarySource);
        }
        if created_at < source_horizon.knowledge_through {
            return Err(SessionContractError::InvalidSummaryHorizon);
        }
        source_horizon.validate()?;
        session_id
            .validate()
            .and_then(|_| summary_anchor_id.validate())
            .map_err(|_| SessionContractError::InvalidIdentity {
                field: "session summary",
            })?;
        for source in &source_anchors {
            source
                .validate()
                .map_err(|_| SessionContractError::InvalidIdentity {
                    field: "session summary source anchor",
                })?;
        }
        let source_anchors = unique.into_iter().collect();
        Ok(Self {
            summary_id,
            session_id,
            summary_anchor_id,
            source_anchors,
            source_horizon,
            created_at,
            predecessor_summary_id: None,
            publication: None,
        })
    }

    pub fn with_predecessor(
        mut self,
        predecessor: SessionSummaryIdV1,
    ) -> Result<Self, SessionContractError> {
        if self.summary_id == predecessor {
            return Err(SessionContractError::SummarySelfPredecessor);
        }
        self.predecessor_summary_id = Some(predecessor);
        Ok(self)
    }

    pub fn with_publication(
        mut self,
        publication: SummaryPublicationMetadataV1,
    ) -> Result<Self, SessionContractError> {
        publication.validate()?;
        self.publication = Some(publication);
        Ok(self)
    }

    pub fn summary_id(&self) -> &SessionSummaryIdV1 {
        &self.summary_id
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn summary_anchor_id(&self) -> &RetrievalAnchorId {
        &self.summary_anchor_id
    }

    pub fn source_anchors(&self) -> &[RetrievalAnchorId] {
        &self.source_anchors
    }

    pub fn source_horizon(&self) -> SummarySourceHorizonV1 {
        self.source_horizon
    }

    pub fn created_at(&self) -> UtcMicros {
        self.created_at
    }

    pub fn predecessor_summary_id(&self) -> Option<&SessionSummaryIdV1> {
        self.predecessor_summary_id.as_ref()
    }

    pub fn publication(&self) -> Option<&SummaryPublicationMetadataV1> {
        self.publication.as_ref()
    }
}

impl<'de> Deserialize<'de> for SessionSummaryRecordV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            summary_id: SessionSummaryIdV1,
            session_id: SessionId,
            summary_anchor_id: RetrievalAnchorId,
            source_anchors: Vec<RetrievalAnchorId>,
            source_horizon: SummarySourceHorizonV1,
            created_at: UtcMicros,
            predecessor_summary_id: Option<SessionSummaryIdV1>,
            publication: Option<SummaryPublicationMetadataV1>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let mut summary = Self::new(
            wire.summary_id,
            wire.session_id,
            wire.summary_anchor_id,
            wire.source_anchors,
            wire.source_horizon,
            wire.created_at,
        )
        .map_err(serde::de::Error::custom)?;
        if let Some(predecessor) = wire.predecessor_summary_id {
            summary = summary
                .with_predecessor(predecessor)
                .map_err(serde::de::Error::custom)?;
        }
        if let Some(publication) = wire.publication {
            summary = summary
                .with_publication(publication)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(summary)
    }
}
