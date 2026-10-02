//! Durable ingest-coverage refusal census (read-only Doctor lane).
//!
//! Deterministic admission refusals advance source coverage with a durable
//! typed reason (`source_cursor_advances`) so ingestion converges instead of
//! re-reporting the same records forever. Those refusals are terminal by
//! design, re-admitting a deterministic refusal would deterministically fail
//! again, so the plan-conformant recovery is truthful surfacing: this census
//! names each refused record's provider, session, covered range, and reason
//! for Doctor. It reads only the live store, so a store that is reset takes
//! its refusals with it. Diagnosis is strictly read-only; it never
//! re-admits, clears, or rewrites coverage.

use tracedecay_contracts::doctor::{IngestRefusalCensusReadV1, IngestRefusalV1};
use tracedecay_domain::ObservationSourceIdentityV1;
use tracedecay_runtime_core::db::engine::QueryExecutor;
use tracedecay_store::{ObservationCoverageReason, ObservationCoverageV1};

use crate::RegisteredGlobalDb;

/// Merge the censuses of every consulted store; one unreadable store makes
/// the whole read unknown.
#[must_use]
pub fn ingest_refusal_read_from_censuses(
    censuses: &[IngestRefusalCensusReadV1],
) -> IngestRefusalCensusReadV1 {
    let mut merged = Vec::new();
    for census in censuses {
        match census {
            IngestRefusalCensusReadV1::Observed { refusals } => {
                merged.extend(refusals.iter().cloned());
            }
            IngestRefusalCensusReadV1::Unknown => return IngestRefusalCensusReadV1::Unknown,
        }
    }
    merged.sort();
    IngestRefusalCensusReadV1::Observed { refusals: merged }
}

impl RegisteredGlobalDb {
    /// Read-only census of durably refused source records.
    ///
    /// A store without the observation authority schema truthfully has an
    /// empty census: coverage never advanced past anything there. A reason
    /// string this binary does not recognize is kept conservatively as a
    /// fixed-size fingerprint, so an unknown disposition stays visible
    /// without letting corrupt durable text escape through Doctor. A row
    /// whose source or coverage no longer decodes makes the census unknown.
    #[hotpath::skip]
    pub async fn observation_refusal_census(&self) -> IngestRefusalCensusReadV1 {
        let snapshot = match self.read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(_) => return IngestRefusalCensusReadV1::Unknown,
        };
        census_from_snapshot(&snapshot)
            .await
            .unwrap_or(IngestRefusalCensusReadV1::Unknown)
    }
}

async fn census_from_snapshot(conn: &impl QueryExecutor) -> Option<IngestRefusalCensusReadV1> {
    if !table_exists(conn, "source_cursor_advances").await? {
        return Some(IngestRefusalCensusReadV1::Observed {
            refusals: Vec::new(),
        });
    }
    let mut rows = conn
        .query(
            "SELECT source_json, coverage_json, reason FROM source_cursor_advances",
            (),
        )
        .await
        .ok()?;
    let mut refusals = Vec::new();
    while let Some(row) = rows.next().await.ok()? {
        let reason = row.get::<String>(2).ok()?;
        let reason = match ObservationCoverageReason::try_from(reason.as_str()) {
            Ok(reason) if !reason.is_refusal() => continue,
            Ok(reason) => reason.as_str().to_owned(),
            Err(unknown) => unknown.fingerprint().as_str().to_owned(),
        };
        let source: ObservationSourceIdentityV1 =
            serde_json::from_str(&row.get::<String>(0).ok()?).ok()?;
        let coverage: ObservationCoverageV1 =
            serde_json::from_str(&row.get::<String>(1).ok()?).ok()?;
        refusals.push(IngestRefusalV1 {
            provider: source.provider().as_str().to_owned(),
            session_id: source.session_id().as_str().to_owned(),
            reason,
            start: coverage.range().start(),
            end: coverage.range().end(),
        });
    }
    refusals.sort();
    Some(IngestRefusalCensusReadV1::Observed { refusals })
}

async fn table_exists(conn: &impl QueryExecutor, table: &str) -> Option<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
        )
        .await
        .ok()?;
    Some(rows.next().await.ok()?.is_some())
}

#[cfg(test)]
mod tests;
