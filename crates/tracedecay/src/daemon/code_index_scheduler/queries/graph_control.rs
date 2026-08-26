use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tracedecay_application::RequestContext;
use tracedecay_domain::{RetrievalBudget, UtcMicros};
use tracedecay_query::retrieval::graph::GraphExecutionControl;

pub(super) struct CallableGraphExecutionControl {
    request: RequestContext,
    started_at: Instant,
}

impl CallableGraphExecutionControl {
    pub(super) fn for_request(request: &RequestContext) -> Arc<dyn GraphExecutionControl> {
        Arc::new(Self {
            request: request.clone(),
            started_at: Instant::now(),
        })
    }
}

impl GraphExecutionControl for CallableGraphExecutionControl {
    fn is_cancelled(&self) -> bool {
        self.request.cancellation().is_cancelled()
    }

    fn elapsed_micros(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

pub(super) fn graph_budget_for_request(
    mut budget: RetrievalBudget,
    request: &RequestContext,
) -> RetrievalBudget {
    let remaining = current_utc_micros()
        .ok()
        .and_then(|now| request.deadline().expires_at.0.checked_sub(now.0))
        .and_then(|micros| u64::try_from(micros).ok())
        .unwrap_or(0);
    budget.deadline_micros = Some(
        budget
            .deadline_micros
            .map_or(remaining, |configured| configured.min(remaining)),
    );
    budget
}

pub(super) fn current_utc_micros() -> Result<UtcMicros, super::CallableCodeCursorError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| super::CallableCodeCursorError::Unavailable)?;
    i64::try_from(elapsed.as_micros())
        .map(UtcMicros)
        .map_err(|_| super::CallableCodeCursorError::Unavailable)
}
