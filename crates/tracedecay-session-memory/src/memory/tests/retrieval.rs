use tracedecay_domain::{PayloadAccessState, UtcMicros};
use tracedecay_store::{
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactRetrievalCommandV1,
    ProjectMemoryFactRetrievalOutcomeV1, ProjectMemoryFactRetrievalReceiptV1,
    ProjectMemoryFactStatusV1, ProjectMemoryFactUnavailableV1,
};

use super::{FakeAuthority, fact_id, id, owner, write_control};
use crate::memory::MemoryApplication;

#[tokio::test]
async fn retrieval_preserves_the_canonical_replay_receipt() {
    let owner = owner();
    let target = ProjectMemoryFactIdV1::new(
        owner.clone(),
        fact_id(owner.clone(), "operation.memory.retrieval"),
    )
    .unwrap();
    let request = ProjectMemoryFactRetrievalCommandV1::new(
        owner.clone(),
        id("operation.memory.retrieval"),
        vec![target.clone()],
        true,
    )
    .unwrap();
    let receipt = ProjectMemoryFactRetrievalReceiptV1::from_replay(
        owner.clone(),
        request.operation_id().clone(),
        request.input_digest().unwrap(),
        request.targets().to_vec(),
        request.recall(),
    )
    .unwrap();
    let projection = ProjectMemoryFactProjectionV1::Unavailable(
        ProjectMemoryFactUnavailableV1::new(
            ProjectMemoryFactStatusV1::new(
                owner.clone(),
                target.fact_id().clone(),
                PayloadAccessState::Deleted,
                UtcMicros(1),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let expected = ProjectMemoryFactRetrievalOutcomeV1::new(receipt, vec![projection]).unwrap();
    let authority = FakeAuthority::default();
    *authority.retrieval_outcome.lock().unwrap() = Some(expected.clone());
    let application = MemoryApplication::new(owner, authority).unwrap();

    let outcome = application
        .record_project_memory_fact_retrieval(request, &write_control())
        .await
        .unwrap();

    assert_eq!(outcome, expected);
    assert!(outcome.receipt().replayed());
}

#[tokio::test]
async fn retrieval_contract_failure_preserves_the_committed_authority_outcome() {
    let owner = owner();
    let target = ProjectMemoryFactIdV1::new(
        owner.clone(),
        fact_id(owner.clone(), "operation.memory.retrieval-settlement"),
    )
    .unwrap();
    let request = ProjectMemoryFactRetrievalCommandV1::new(
        owner.clone(),
        id("operation.memory.retrieval-request"),
        vec![target.clone()],
        false,
    )
    .unwrap();
    let receipt = ProjectMemoryFactRetrievalReceiptV1::recorded(
        owner.clone(),
        id("operation.memory.retrieval-other"),
        request.input_digest().unwrap(),
        request.targets().to_vec(),
        request.recall(),
    )
    .unwrap();
    let projection = ProjectMemoryFactProjectionV1::Unavailable(
        ProjectMemoryFactUnavailableV1::new(
            ProjectMemoryFactStatusV1::new(
                owner.clone(),
                target.fact_id().clone(),
                PayloadAccessState::Deleted,
                UtcMicros(1),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let expected = ProjectMemoryFactRetrievalOutcomeV1::new(receipt, vec![projection]).unwrap();
    let authority = FakeAuthority::default();
    *authority.retrieval_outcome.lock().unwrap() = Some(expected.clone());
    let application = MemoryApplication::new(owner, authority).unwrap();

    let error = application
        .record_project_memory_fact_retrieval(request, &write_control())
        .await
        .unwrap_err();

    let crate::memory::MemoryMutationError::InvalidAuthorityResult {
        authority_result, ..
    } = error
    else {
        panic!("authority contract failure must retain its committed outcome");
    };
    assert_eq!(authority_result, expected);
}
