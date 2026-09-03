use std::sync::Arc;

use tracedecay_domain::FactOwnerV1;
use tracedecay_store::{
    FactReadControl, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactFeedbackHistoryQueryV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactListQueryV1, ProjectMemoryFactSearchGraphCoverageV1,
    ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchQuery,
};

use super::{FakeAuthority, fact_id, id, owner};
use crate::memory::{MemoryApplication, MemoryApplicationError};

#[tokio::test]
async fn reads_use_finite_owner_bound_authority_methods() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let fact_id = fact_id(owner(), "operation.project-memory.read");
    let target = ProjectMemoryFactIdV1::new(owner(), fact_id).unwrap();
    let search = ProjectMemoryFactSearchQuery::new(
        owner(),
        ProjectMemoryFactSearchKindV1::Search,
        Some("project-memory fixture".to_owned()),
        None,
        10,
    )
    .unwrap();
    let read_control = FactReadControl::new(Arc::new(|| false));

    assert!(
        application
            .list_project_memory_facts(
                ProjectMemoryFactListQueryV1::new(owner(), None, None, None, 10).unwrap(),
                &read_control,
            )
            .await
            .unwrap()
            .facts()
            .is_empty()
    );
    assert!(
        application
            .find_project_memory_contradictions(
                ProjectMemoryFactContradictionQueryV1::new(owner(), None, 500_000, 10).unwrap(),
                &read_control,
            )
            .await
            .unwrap()
            .contradictions()
            .is_empty()
    );
    let read_control_address = std::ptr::from_ref(&read_control).addr();
    let search_page = application
        .search_project_memory_facts(search, &read_control)
        .await
        .unwrap();
    assert!(search_page.hits().is_empty());
    assert_eq!(
        search_page.graph_coverage(),
        ProjectMemoryFactSearchGraphCoverageV1::NotMounted
    );
    {
        let forwarded = application.authority.search_read_controls.lock().unwrap();
        assert_eq!(forwarded.as_slice(), &[read_control_address]);
    }
    assert!(
        application
            .get_project_memory_fact(target.clone(), &read_control)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        application
            .get_project_memory_history(
                ProjectMemoryFactHistoryQueryV1::new(target.clone(), None, 10).unwrap(),
                &read_control,
            )
            .await
            .unwrap()
            .events()
            .is_empty()
    );
    let status = application
        .project_memory_status(&read_control)
        .await
        .unwrap();
    assert_eq!(status.owner(), &owner());
    assert!(
        application
            .get_project_memory_feedback_history(
                ProjectMemoryFactFeedbackHistoryQueryV1::new(target.clone(), None, 10).unwrap(),
                &read_control,
            )
            .await
            .unwrap()
            .events()
            .is_empty()
    );
    assert!(
        application
            .inspect_project_memory_fact(target, &read_control)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        application
            .find_exact_fact_by_content("canonical memory fixture", &read_control)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        application
            .get_project_memory_automatic_fact_receipt(
                id("apply.memory.read-control"),
                &read_control,
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        application
            .list_project_memory_automatic_fact_receipts(None, None, 10, &read_control)
            .await
            .unwrap()
            .receipts()
            .is_empty()
    );
    assert_eq!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .as_slice(),
        [
            "list",
            "contradictions",
            "search",
            "get",
            "history",
            "status",
            "feedback-history",
            "inspect",
            "exact-content",
            "automatic-fact-get",
            "automatic-fact-list",
        ]
    );
}

#[tokio::test]
async fn read_owner_mismatch_never_reaches_authority() {
    let application = MemoryApplication::new(owner(), FakeAuthority::default()).unwrap();
    let read_control = FactReadControl::new(Arc::new(|| false));
    let error = application
        .list_project_memory_facts(
            ProjectMemoryFactListQueryV1::new(FactOwnerV1::Profile, None, None, None, 10).unwrap(),
            &read_control,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        MemoryApplicationError::OwnerMismatch { .. }
    ));
    assert!(
        application
            .authority
            .authority_calls
            .lock()
            .unwrap()
            .is_empty()
    );
}
