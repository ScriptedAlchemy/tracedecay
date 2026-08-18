//! Canonical project-memory adapter for Explorer knowledge search.

use std::sync::Arc;

use serde_json::json;
use tracedecay_application::memory::FactSearchGraphCoverageV1;
use tracedecay_store::FactReadControl;

use super::{ExplorerQueryRequestV1, ExplorerSourceIdV1, ExplorerSourceProgressV1, ready_source};
use crate::application::context::CancellationToken;
use crate::memory_service::MemoryFactsCoverageV1;
use crate::read_model::DashboardCoverageCompletenessV1;
use crate::{DashboardHttpRequestControlV1, DashboardState, memory_service};

fn coverage_summary(
    coverage: &MemoryFactsCoverageV1,
    row_count: usize,
) -> (Option<u64>, Vec<String>) {
    let facts_complete = matches!(
        coverage.completeness,
        DashboardCoverageCompletenessV1::Complete
    );
    let graph_complete = matches!(
        coverage.graph,
        None | Some(
            FactSearchGraphCoverageV1::Complete { .. } | FactSearchGraphCoverageV1::NotApplicable
        )
    );
    let mut omissions = Vec::new();
    if !facts_complete {
        omissions.push("canonical fact search page is bounded".to_owned());
    }
    if !graph_complete {
        omissions.push("verified memory graph coverage is incomplete".to_owned());
    }
    (
        (facts_complete && graph_complete).then_some(row_count as u64),
        omissions,
    )
}

fn require_first_page(offset: i64) -> Result<(), &'static str> {
    (offset == 0)
        .then_some(())
        .ok_or("knowledge search requires a canonical cursor before pagination")
}

pub(super) async fn knowledge_source(
    state: &DashboardState,
    request: &ExplorerQueryRequestV1,
    control: &DashboardHttpRequestControlV1,
    cancellation: CancellationToken,
) -> ExplorerSourceProgressV1 {
    if let Err(message) = require_first_page(request.offset) {
        return ExplorerSourceProgressV1::error(
            ExplorerSourceIdV1::Knowledge,
            "knowledge_cursor_required",
            message,
        );
    }
    let request_cancellation = control.cancellation().clone();
    let source_cancellation = cancellation.clone();
    let request_deadline = control.deadline();
    let read_control = FactReadControl::new(Arc::new(move || {
        request_cancellation.is_cancelled()
            || source_cancellation.is_cancelled()
            || request_deadline
                .is_elapsed_at(crate::application::context::application_observed_at())
    }));
    let facts = match memory_service::fetch_facts(
        state,
        &request.query,
        request.limit,
        &read_control,
    )
    .await
    {
        Ok(facts) => facts,
        Err(_)
            if control
                .deadline()
                .is_elapsed_at(crate::application::context::application_observed_at()) =>
        {
            return ExplorerSourceProgressV1::timed_out(
                ExplorerSourceIdV1::Knowledge,
                "knowledge_query_timed_out",
                "knowledge query exceeded the admitted request deadline",
            );
        }
        Err(_) if control.cancellation().is_cancelled() || cancellation.is_cancelled() => {
            return ExplorerSourceProgressV1::cancelled(
                ExplorerSourceIdV1::Knowledge,
                "knowledge_query_cancelled",
                "knowledge query was cancelled",
            );
        }
        Err(_) => {
            return ExplorerSourceProgressV1::error(
                ExplorerSourceIdV1::Knowledge,
                "knowledge_query_failed",
                "knowledge query unavailable",
            );
        }
    };
    if control
        .deadline()
        .is_elapsed_at(crate::application::context::application_observed_at())
    {
        return ExplorerSourceProgressV1::timed_out(
            ExplorerSourceIdV1::Knowledge,
            "knowledge_query_timed_out",
            "knowledge query exceeded the admitted request deadline",
        );
    }
    if control.cancellation().is_cancelled() || cancellation.is_cancelled() {
        return ExplorerSourceProgressV1::cancelled(
            ExplorerSourceIdV1::Knowledge,
            "knowledge_query_cancelled",
            "knowledge query was cancelled",
        );
    }
    let (total, omissions) = coverage_summary(&facts.coverage, facts.rows.len());
    ready_source(
        ExplorerSourceIdV1::Knowledge,
        request,
        facts.rows,
        total,
        json!({
            "authority": "canonical_project_memory_search",
            "coverage": facts.coverage,
        }),
        "facts",
        omissions,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_total_requires_complete_fact_and_verified_graph_coverage() {
        let (total, omissions) = coverage_summary(
            &MemoryFactsCoverageV1 {
                completeness: DashboardCoverageCompletenessV1::Complete,
                limit: 3,
                graph: Some(FactSearchGraphCoverageV1::Complete {
                    root_count: 1,
                    relation_count: 1,
                    expanded_fact_count: 1,
                }),
                examined: None,
                eligible: None,
            },
            3,
        );
        assert_eq!(total, Some(3));
        assert!(omissions.is_empty());

        let (total, omissions) = coverage_summary(
            &MemoryFactsCoverageV1 {
                completeness: DashboardCoverageCompletenessV1::Partial,
                limit: 3,
                graph: Some(FactSearchGraphCoverageV1::Degraded {
                    reason:
                        tracedecay_application::memory::FactSearchGraphDegradationV1::Unavailable,
                }),
                examined: None,
                eligible: None,
            },
            3,
        );
        assert_eq!(total, None);
        assert_eq!(omissions.len(), 2);
    }

    #[test]
    fn knowledge_search_rejects_offset_pagination_without_a_canonical_cursor() {
        assert_eq!(require_first_page(0), Ok(()));
        assert_eq!(
            require_first_page(1),
            Err("knowledge search requires a canonical cursor before pagination")
        );
    }

    #[test]
    fn bounded_page_does_not_advertise_a_rejected_numeric_continuation() {
        let request = ExplorerQueryRequestV1 {
            query: "cache".to_owned(),
            limit: 1,
            offset: 0,
        };
        let source = ready_source(
            ExplorerSourceIdV1::Knowledge,
            &request,
            vec![json!({"fact_id": "fact.fixture"})],
            Some(2),
            json!({}),
            "facts",
            Vec::new(),
        );

        assert_eq!(source.page.and_then(|page| page.next_offset), None);
    }
}
