//! Durable ingest-coverage refusal census (read-only Doctor lane).
//!
//! Deterministic admission refusals advance source coverage with a durable
//! typed reason (`source_cursor_advances`) so ingestion converges instead of
//! re-reporting the same records forever. Those refusals are terminal by
//! design — re-admitting a deterministic refusal would deterministically fail
//! again — so the plan-conformant recovery is truthful surfacing: this census
//! counts the refused records per provider and reason for Doctor. Diagnosis is
//! strictly read-only; it never re-admits, clears, or rewrites coverage.

use serde::{Deserialize, Serialize};

use tracedecay_runtime_core::db::engine::QueryExecutor;
use tracedecay_store::ObservationCoverageReason;

use crate::RegisteredGlobalDb;

/// Refused source records recorded under one provider/reason pair.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ObservationRefusalCountV1 {
    pub provider: String,
    pub reason: String,
    pub count: u64,
}

/// Census over the durable cursor-advance ledger.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ObservationRefusalCensusV1 {
    /// The ledger was consulted; only refusal-shaped reasons are counted
    /// (expected dispositions such as blank or out-of-scope frames are not
    /// refusals). An empty census means nothing was durably refused.
    Observed {
        refusals: Vec<ObservationRefusalCountV1>,
    },
    /// The ledger could not be consulted.
    Unavailable,
}

impl RegisteredGlobalDb {
    /// Read-only census of durably refused source records.
    ///
    /// A store without the observation authority schema truthfully has an
    /// empty census: coverage never advanced past anything there. A reason
    /// string this binary does not recognize is counted conservatively — an
    /// unknown disposition must stay visible, never silently classified as
    /// benign.
    pub async fn observation_refusal_census(&self) -> ObservationRefusalCensusV1 {
        let snapshot = match self.read_snapshot().await {
            Ok(snapshot) => snapshot,
            Err(_) => return ObservationRefusalCensusV1::Unavailable,
        };
        census_from_snapshot(&snapshot).await
    }
}

async fn census_from_snapshot(conn: &impl QueryExecutor) -> ObservationRefusalCensusV1 {
    let table_present = match table_exists(conn, "source_cursor_advances").await {
        Ok(present) => present,
        Err(()) => return ObservationRefusalCensusV1::Unavailable,
    };
    if !table_present {
        return ObservationRefusalCensusV1::Observed {
            refusals: Vec::new(),
        };
    }
    let mut rows = match conn
        .query(
            "SELECT COALESCE(json_extract(source_json, '$.provider'), 'unknown') AS provider,
                    reason,
                    COUNT(*)
             FROM source_cursor_advances
             GROUP BY provider, reason
             ORDER BY provider, reason",
            (),
        )
        .await
    {
        Ok(rows) => rows,
        Err(_) => return ObservationRefusalCensusV1::Unavailable,
    };
    let mut refusals = Vec::new();
    loop {
        let row = match rows.next().await {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => return ObservationRefusalCensusV1::Unavailable,
        };
        let (Ok(provider), Ok(reason), Ok(count)) = (
            row.get::<String>(0),
            row.get::<String>(1),
            row.get::<i64>(2),
        ) else {
            return ObservationRefusalCensusV1::Unavailable;
        };
        let is_refusal = ObservationCoverageReason::try_from(reason.as_str())
            .map_or(true, ObservationCoverageReason::is_refusal);
        if !is_refusal {
            continue;
        }
        refusals.push(ObservationRefusalCountV1 {
            provider,
            reason,
            count: u64::try_from(count).unwrap_or(0),
        });
    }
    ObservationRefusalCensusV1::Observed { refusals }
}

async fn table_exists(conn: &impl QueryExecutor, table: &str) -> Result<bool, ()> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
        )
        .await
        .map_err(|_| ())?;
    Ok(rows.next().await.map_err(|_| ())?.is_some())
}

#[cfg(test)]
mod tests;
