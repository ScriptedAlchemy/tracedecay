use tracedecay_application::retained_surfaces::{
    FactReadOptionsV1, FactStoreSearchRequestV1, MemoryScopeV1, RetainedProjectSelectorV1,
};
use tracedecay_application::{CancellationStage, RetainedSurfaceExecutionErrorV1};
use tracedecay_domain::{FactOwnerV1, ProjectId};
use tracedecay_store::FactStoreError;

use super::{
    EFFECT_CANCELLATION_STAGES, MAX_RETAINED_FACT_LIMIT, READ_CANCELLATION_STAGES, fact_limit,
    map_store_error, search_logical_effect,
};

#[test]
fn mutation_prerequisites_and_search_telemetry_use_distinct_stages() {
    assert_eq!(
        READ_CANCELLATION_STAGES.before,
        CancellationStage::BeforeRead
    );
    assert_eq!(
        READ_CANCELLATION_STAGES.active,
        CancellationStage::DuringRead
    );
    assert_eq!(
        EFFECT_CANCELLATION_STAGES.before,
        CancellationStage::BeforeEffect
    );
    assert_eq!(
        EFFECT_CANCELLATION_STAGES.active,
        CancellationStage::EffectInFlight
    );
}

#[test]
fn retained_limits_reject_zero_and_oversized_pages() {
    assert_eq!(fact_limit(None), Ok(20));
    assert_eq!(fact_limit(Some(1)), Ok(1));
    assert_eq!(fact_limit(Some(MAX_RETAINED_FACT_LIMIT as u64)), Ok(200));
    assert_eq!(
        fact_limit(Some(0)),
        Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
    );
    assert_eq!(
        fact_limit(Some((MAX_RETAINED_FACT_LIMIT + 1) as u64)),
        Err(RetainedSurfaceExecutionErrorV1::InvalidRequest)
    );
}

#[test]
fn graph_failures_keep_distinct_retained_terminal_states() {
    assert_eq!(
        map_store_error(FactStoreError::GraphCancelled),
        RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::DuringRead)
    );
    assert_eq!(
        map_store_error(FactStoreError::ReadCancelled),
        RetainedSurfaceExecutionErrorV1::Cancelled(CancellationStage::DuringRead)
    );
    assert_eq!(
        map_store_error(FactStoreError::GraphBudgetExhausted),
        RetainedSurfaceExecutionErrorV1::Saturated
    );
    assert_eq!(
        map_store_error(FactStoreError::GraphDeadlineExceeded),
        RetainedSurfaceExecutionErrorV1::TimedOut(CancellationStage::DuringRead)
    );
    assert_eq!(
        map_store_error(FactStoreError::OperationConflict),
        RetainedSurfaceExecutionErrorV1::Conflict
    );
    assert_eq!(
        map_store_error(FactStoreError::GraphResetRequired {
            owner: FactOwnerV1::Profile,
            reason: "profile graph reset".to_owned(),
        }),
        RetainedSurfaceExecutionErrorV1::ProfileResetRequired
    );
    assert_eq!(
        map_store_error(FactStoreError::GraphResetRequired {
            owner: FactOwnerV1::Project {
                project_id: ProjectId::new("project.graph-reset").expect("project id"),
            },
            reason: "project graph reset".to_owned(),
        }),
        RetainedSurfaceExecutionErrorV1::ProjectResetRequired
    );
}

#[test]
fn logical_search_identity_excludes_equivalent_routing_fields() {
    let project_id = ProjectId::new("project.retained-logical-search").expect("project id");
    let owner = FactOwnerV1::Project {
        project_id: project_id.clone(),
    };
    let direct = FactStoreSearchRequestV1 {
        query: "canonical identity".to_owned(),
        options: FactReadOptionsV1::default(),
        after: None,
    };
    let mut explicitly_routed = direct.clone();
    explicitly_routed.options.memory_scope = Some(MemoryScopeV1::Project);
    explicitly_routed.options.project_selector = Some(RetainedProjectSelectorV1 { project_id });

    assert_eq!(
        search_logical_effect(&owner, &direct),
        search_logical_effect(&owner, &explicitly_routed)
    );
}
