//! Operational-health retrieval port.

use std::sync::Arc;

use tracedecay_contracts::EvidenceDomain;
use tracedecay_contracts::retrieval::{
    HealthReadRequest, HealthReadResult, OperationalRetrievalPort, RetrievalPortContext,
    RetrievalPortOutcome,
};
use tracedecay_graph_query::SourceReadContext;

use super::{completed, now_observed};

pub struct TraceDecayHealthPortV1 {
    source_runtime: Arc<SourceReadContext>,
}

impl TraceDecayHealthPortV1 {
    pub fn new(source_runtime: Arc<SourceReadContext>) -> Self {
        Self { source_runtime }
    }
}

impl OperationalRetrievalPort for TraceDecayHealthPortV1 {
    fn health_read(
        &self,
        _context: &RetrievalPortContext<'_>,
        _request: &HealthReadRequest,
    ) -> RetrievalPortOutcome<HealthReadResult> {
        let serving_db_exists = self.source_runtime.db().canonical_database_path().is_file();
        let status = if serving_db_exists && !self.source_runtime.is_read_only() {
            "ok"
        } else if serving_db_exists {
            "read_only"
        } else {
            "degraded"
        };
        completed(
            HealthReadResult {
                status: status.to_owned(),
            },
            EvidenceDomain::Operational,
            now_observed(),
        )
    }
}
