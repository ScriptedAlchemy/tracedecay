use super::*;
use crate::application::memory::MemoryApplication;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::FactOwnerV1;

async fn database(path: &Path) -> Database {
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(path, "fact proposal lifecycle test").unwrap();
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize)
        .await
        .unwrap()
        .0
}

fn request(content: &str) -> AddFactRequest {
    AddFactRequest {
        content: content.to_string(),
        category: MemoryCategory::Project,
        source: Some("fact-proposal-test".to_string()),
        tags: vec!["automation".to_string()],
        entities: vec!["TraceDecay".to_string()],
        trust: Some(0.9),
        metadata: serde_json::json!({"fixture": "fact-proposal-lifecycle"}),
    }
}

fn live_command(
    owner: FactOwnerV1,
    run_id: &str,
    proposal_id: &str,
    content: &str,
) -> tracedecay_store::ProjectMemoryFactAddCommandV1 {
    automation_fact_proposal_add_command(owner, request(content), run_id, proposal_id, None)
        .unwrap()
}

#[tokio::test]
async fn authority_submission_replays_once_and_rejection_is_cas_bound() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(&temp.path().join("memory.db")).await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let proposal_id = ProvenanceId::new("proposal-replay".to_string()).unwrap();
    let command = live_command(
        owner.clone(),
        "run-replay",
        proposal_id.as_str(),
        "Replay this exact authority proposal once",
    );

    let first = memory
        .submit_project_memory_fact_proposal(proposal_id.clone(), command.clone(), None)
        .await
        .unwrap();
    let replay = memory
        .submit_project_memory_fact_proposal(proposal_id.clone(), command, None)
        .await
        .unwrap();
    assert_eq!(first.proposal_id(), replay.proposal_id());
    assert_eq!(first.revision(), replay.revision());
    assert_eq!(
        first.state(),
        ProjectMemoryFactProposalStateV1::PendingApproval
    );

    let listed = list_fact_proposals(&memory, temp.path(), None, 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].proposal_id, proposal_id.as_str());

    let reviewer = proposal_actor("test:reviewer").unwrap();
    let rejected = memory
        .reject_project_memory_fact_proposal(
            proposal_id.clone(),
            first.revision(),
            reviewer.clone(),
            "fixture rejection".to_string(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.state(), ProjectMemoryFactProposalStateV1::Rejected);
    assert!(
        memory
            .reject_project_memory_fact_proposal(
                proposal_id,
                first.revision(),
                reviewer,
                "stale retry".to_string(),
            )
            .await
            .is_err(),
        "a stale revision must not overwrite a reviewed proposal"
    );
}

#[tokio::test]
async fn authority_collapses_duplicate_semantic_submissions_and_preserves_submission_order() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(&temp.path().join("memory.db")).await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let dashboard_root = temp.path().join("dashboard");
    let mut first = request("Keep the first submitted proposal first in the dashboard");
    first.metadata = serde_json::json!({
        "fixture": "fact-proposal-lifecycle",
        "reason": "first evidence annotation"
    });
    let first_request = serde_json::to_value(first).unwrap();
    let mut duplicate = request("Keep the first submitted proposal first in the dashboard");
    duplicate.metadata = serde_json::json!({
        "fixture": "fact-proposal-lifecycle",
        "reason": "later evidence annotation"
    });
    let duplicate_request = serde_json::to_value(duplicate).unwrap();
    let second_request = serde_json::to_value(request(
        "Keep the later submitted proposal after the first one",
    ))
    .unwrap();
    let accepted = vec![
        serde_json::json!({
            "add_fact_request": first_request,
            "proposal": {"source_index": 0},
            "validation": {"source_index": 0}
        }),
        serde_json::json!({
            "add_fact_request": duplicate_request,
            "proposal": {"source_index": 1},
            "validation": {"source_index": 1}
        }),
        serde_json::json!({
            "add_fact_request": second_request,
            "proposal": {"source_index": 2},
            "validation": {"source_index": 2}
        }),
    ];

    let recorded = record_session_fact_proposals(
        &memory,
        &dashboard_root,
        "run-duplicate-collapse",
        None,
        &accepted,
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        recorded.len(),
        2,
        "one exact semantic duplicate must be a no-op"
    );
    assert_eq!(
        recorded[0].validation,
        Some(serde_json::json!({"source_index": 0}))
    );

    let canonical = memory
        .list_project_memory_fact_proposals(None, None, 10)
        .await
        .unwrap();
    assert_eq!(canonical.proposals().len(), 2);

    for record in &recorded {
        apply_fact_proposal(&memory, &dashboard_root, &record.proposal_id, None)
            .await
            .unwrap();
    }
    let applied = list_fact_proposals(
        &memory,
        &dashboard_root,
        Some(FactProposalState::Applied),
        10,
    )
    .await
    .unwrap();
    assert_eq!(applied.len(), 2);
    assert_eq!(
        applied[0].add_fact_request.as_ref().unwrap().content,
        "Keep the first submitted proposal first in the dashboard"
    );
    assert_eq!(
        applied[1].add_fact_request.as_ref().unwrap().content,
        "Keep the later submitted proposal after the first one"
    );
    assert_eq!(
        applied[0].validation,
        Some(serde_json::json!({"source_index": 0}))
    );
}

#[tokio::test]
async fn authority_promotion_commits_one_canonical_fact_and_rejects_stale_cas() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(&temp.path().join("memory.db")).await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let proposal_id = ProvenanceId::new("proposal-promotion".to_string()).unwrap();
    let submitted = memory
        .submit_project_memory_fact_proposal(
            proposal_id.clone(),
            live_command(
                owner.clone(),
                "run-promotion",
                proposal_id.as_str(),
                "Promote this proposal into one canonical fact",
            ),
            None,
        )
        .await
        .unwrap();
    let promoted = memory
        .promote_project_memory_fact_proposal_with_disposition(
            ProjectMemoryFactProposalPromotionV1::new(
                owner.clone(),
                proposal_id.clone(),
                submitted.revision(),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        promoted.disposition(),
        ProjectMemoryFactProposalPromotionDispositionV1::NewlyPromoted
    );
    assert_eq!(
        promoted.proposal().state(),
        ProjectMemoryFactProposalStateV1::Applied
    );
    assert!(promoted.proposal().applied_fact_id().is_some());

    let replayed = memory
        .promote_project_memory_fact_proposal_with_disposition(
            ProjectMemoryFactProposalPromotionV1::new(
                owner.clone(),
                proposal_id.clone(),
                submitted.revision(),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        replayed.disposition(),
        ProjectMemoryFactProposalPromotionDispositionV1::AlreadyPromoted
    );

    assert!(
        memory
            .promote_project_memory_fact_proposal_with_disposition(
                ProjectMemoryFactProposalPromotionV1::new(
                    owner.clone(),
                    proposal_id.clone(),
                    submitted.revision(),
                    Some(proposal_actor("test:other-reviewer").unwrap()),
                )
                .unwrap(),
            )
            .await
            .is_err(),
        "a stale promotion with different authority input must not replay"
    );

    assert!(
        memory
            .reject_project_memory_fact_proposal(
                proposal_id,
                submitted.revision(),
                proposal_actor("test:reviewer").unwrap(),
                "stale transition".to_string(),
            )
            .await
            .is_err(),
        "a stale request cannot replace a promoted proposal"
    );
}
