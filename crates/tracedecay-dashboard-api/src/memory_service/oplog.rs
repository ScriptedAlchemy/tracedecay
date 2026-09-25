//! Memory oplog payload.

use schemars::JsonSchema;
use serde::Serialize;

use super::super::DashboardState;
use crate::read_model::DashboardDomainStateV1;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::FactReadControl;

/// One canonical lineage operation. Operations without a fact target carry no
/// `fact_id`; the route does not expose mutation detail.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemoryOplogEventV1 {
    pub id: i64,
    pub ts: i64,
    pub op: String,
    pub fact_id: Option<String>,
}

/// `GET /api/plugins/holographic/oplog`, newest first.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemoryOplogPayloadV1 {
    pub events: Vec<MemoryOplogEventV1>,
    pub count: usize,
    pub limit: i64,
    /// Request lifecycle state when the read ended before a result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DashboardDomainStateV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub error: String,
}

impl MemoryOplogPayloadV1 {
    pub fn empty(limit: i64, error: impl Into<String>) -> Self {
        Self {
            events: Vec::new(),
            count: 0,
            limit,
            state: None,
            code: None,
            error: error.into(),
        }
    }
}

pub async fn oplog_payload(
    state: &DashboardState,
    limit: i64,
    read_control: &FactReadControl,
) -> MemoryOplogPayloadV1 {
    let bounded_limit = limit.clamp(1, 300) as usize;
    let result = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application
            .dashboard_oplog(bounded_limit, read_control)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(entries) => {
            let events: Vec<_> = entries
                .iter()
                .map(|entry| MemoryOplogEventV1 {
                    id: entry.id,
                    ts: entry.occurred_at.0,
                    op: entry.operation.clone(),
                    fact_id: entry
                        .fact
                        .as_ref()
                        .map(|fact| fact.fact_id().as_str().to_owned()),
                })
                .collect();
            MemoryOplogPayloadV1 {
                count: events.len(),
                events,
                ..MemoryOplogPayloadV1::empty(limit, "")
            }
        }
        Err(error) => MemoryOplogPayloadV1::empty(limit, error),
    }
}
