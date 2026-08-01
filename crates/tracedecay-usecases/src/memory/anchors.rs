//! Daemon-authorized evidence anchor resolution boundary.

use std::error::Error as StdError;
use std::future::Future;

use thiserror::Error;

use tracedecay_domain::{DomainError, FactOwnerV1, RetrievalAnchorId, RetrievalAnchorRecordV2};
use tracedecay_store::FactStore;

use crate::anchor_resolution::{
    EvidenceAnchorReportResolver, EvidenceAnchorResolutionReport,
};

use super::MemoryApplication;
use super::error::MemoryApplicationError;

/// Immutable daemon-authorized evidence record suitable for materialization in
/// a fact shard. It deliberately reuses the canonical retrieval-anchor model.
#[derive(Clone, Debug)]
pub struct ResolvedEvidenceAnchorV1 {
    record: RetrievalAnchorRecordV2,
}

impl ResolvedEvidenceAnchorV1 {
    pub fn new(record: RetrievalAnchorRecordV2) -> Result<Self, DomainError> {
        record.validate()?;
        Ok(Self { record })
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        self.record.anchor_id()
    }

    pub fn record(&self) -> &RetrievalAnchorRecordV2 {
        &self.record
    }

    pub fn into_record(self) -> RetrievalAnchorRecordV2 {
        self.record
    }
}

#[derive(Debug, Error)]
pub enum EvidenceAnchorResolutionError {
    #[error("evidence anchor {anchor_id} is unavailable from the daemon authority")]
    Unavailable { anchor_id: RetrievalAnchorId },
    #[error("evidence anchor resolver operation {operation} failed")]
    Authority {
        operation: &'static str,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
}

/// Daemon/ingress-only boundary for resolving observation evidence that lives
/// outside the fact shard. Implementations must not expose a database handle.
pub trait EvidenceAnchorResolver: Send + Sync {
    fn resolve_evidence_anchor(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> impl Future<Output = Result<ResolvedEvidenceAnchorV1, EvidenceAnchorResolutionError>> + Send;
}

impl<A: FactStore> MemoryApplication<A> {
    /// Resolves a daemon-authorized observation anchor before the caller
    /// materializes the returned record in `FactWriteBatch::new_anchors`.
    /// The fact shard never performs a cross-database anchor lookup itself.
    pub async fn resolve_evidence_anchor<R: EvidenceAnchorResolver>(
        &self,
        resolver: &R,
        anchor_id: RetrievalAnchorId,
    ) -> Result<RetrievalAnchorRecordV2, MemoryApplicationError> {
        anchor_id
            .validate()
            .map_err(MemoryApplicationError::InvalidEvidenceAnchor)?;
        let resolved = resolver
            .resolve_evidence_anchor(self.owner.clone(), anchor_id.clone())
            .await?;
        let record = resolved.into_record();
        if record.anchor_id() != &anchor_id
            || FactOwnerV1::from(record.owner().clone()) != self.owner
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner",
            });
        }
        Ok(record)
    }

    /// Resolves a daemon-authorized observation anchor into its typed
    /// resolution report (state, coverage, watermark drift, and bounding
    /// authorization) before the caller materializes any returned record.
    /// The same owner and identity checks as `resolve_evidence_anchor` apply:
    /// a report never silently switches owner or anchor identity.
    pub async fn resolve_evidence_anchor_report<R: EvidenceAnchorReportResolver>(
        &self,
        resolver: &R,
        anchor_id: RetrievalAnchorId,
    ) -> Result<EvidenceAnchorResolutionReport, MemoryApplicationError> {
        anchor_id
            .validate()
            .map_err(MemoryApplicationError::InvalidEvidenceAnchor)?;
        let report = resolver
            .resolve_evidence_anchor_report(self.owner.clone(), anchor_id.clone())
            .await?;
        if report.anchor_id() != &anchor_id {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor report identity",
            });
        }
        if let Some(record) = report.record()
            && (record.anchor_id() != &anchor_id
                || FactOwnerV1::from(record.owner().clone()) != self.owner)
        {
            return Err(MemoryApplicationError::InvalidAuthorityResult {
                invariant: "resolved evidence anchor identity and owner",
            });
        }
        Ok(report)
    }
}
