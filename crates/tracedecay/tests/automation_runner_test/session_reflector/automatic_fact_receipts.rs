use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::json;
use tempfile::tempdir;
use tracedecay_agent_hosts::automation::automatic_facts::{
    AutomaticFactState, list_automatic_fact_receipts, load_automatic_fact_receipt,
    record_session_automatic_facts,
};

use crate::support::{init_project, project_memory_owner, test_automation_run_control};

#[tokio::test]
async fn session_automatic_facts_keep_paraphrases_distinct() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let owner = project_memory_owner(&cg);
    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        owner,
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let fact = |content: &str| {
        json!({
            "add_fact_request": {
                "content": content,
                "category": "project",
                "source_label": "session_reflector",
                "tags": ["session-reflector"],
                "entities": ["merge discipline"],
                "trust": 0.9,
                "metadata": {
                    "source_span": { "session_id": "s", "message_id": "m" },
                    "trust_reason": "repeated evidence"
                }
            }
        })
    };
    let batch = vec![
        fact(
            "Never merge a PR batch after a single flaky green pass; require stable \
             aggregate verification and a live PR-state recheck before merging",
        ),
        fact(
            "Before merging a PR batch, require stable aggregate verification and a \
             live PR-state recheck — a single flaky green pass is never enough to merge",
        ),
        fact(
            "A single flaky green pass is not enough: merging the PR batch needs \
             stable aggregate verification plus a live PR-state recheck first",
        ),
        fact(
            "Cursor composer ingestion reads cursorDiskKV with immutable read-only \
             SQLite opens and indexed primary-key lookups only",
        ),
    ];

    let recorded =
        record_session_automatic_facts(&memory, &run_control, "run-a", Some("evidence-a"), &batch)
            .await
            .unwrap();
    assert!(recorded.retry_error.is_none());
    assert_eq!(
        recorded.receipts.len(),
        4,
        "each terminal effect keeps its original evidence and identity"
    );

    let restated = vec![fact(
        "Require stable aggregate verification and live PR-state rechecks; never \
         merge the batch off one flaky green pass",
    )];
    let second = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-b",
        Some("evidence-b"),
        &restated,
    )
    .await
    .unwrap();
    assert!(second.retry_error.is_none());
    assert_eq!(second.receipts.len(), 1);

    let receipts = list_automatic_fact_receipts(
        &memory,
        Some(AutomaticFactState::Applied),
        10,
        run_control.read_control(),
    )
    .await
    .unwrap();
    assert_eq!(receipts.len(), 5);
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.add_fact_request.content.contains("flaky green"))
            .count(),
        4,
        "paraphrases remain independently applied"
    );
    assert!(
        receipts
            .iter()
            .any(|receipt| receipt.add_fact_request.content.contains("cursorDiskKV")),
        "distinct automatic effect preserved"
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.run_id == "run-a")
            .count(),
        4
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt.run_id == "run-b")
            .count(),
        1
    );
}

#[tokio::test]
async fn session_automatic_fact_receipts_remain_immutable_when_paraphrases_apply() {
    let temp = tempdir().unwrap();
    let cg = init_project(temp.path()).await;
    let memory = tracedecay_usecases::memory::MemoryApplication::new(
        project_memory_owner(&cg),
        tracedecay::store::memory::DatabaseFactStore::new(cg.db()),
    )
    .unwrap();
    let run_control = test_automation_run_control(Arc::new(AtomicBool::new(false)));
    let applied = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-old",
        Some("evidence-old"),
        &[json!({
            "add_fact_request": {
                "content": "Never merge a PR batch after a single flaky green pass; require stable \
                            aggregate verification and a live PR-state recheck before merging",
                "category": "project",
                "source_label": "session_reflector",
                "tags": ["session-reflector"],
                "entities": ["merge discipline"],
                "trust": 0.9,
                "metadata": {
                    "source_span": { "session_id": "s", "message_id": "m" },
                    "trust_reason": "repeated evidence"
                }
            }
        })],
    )
    .await
    .unwrap();
    assert!(applied.retry_error.is_none());
    assert_eq!(applied.receipts.len(), 1);
    let applied_id = applied.receipts[0].apply_id.clone();
    let applied_before =
        load_automatic_fact_receipt(&memory, &applied_id, run_control.read_control())
            .await
            .unwrap()
            .expect("applied automatic fact receipt");

    let paraphrase = json!({
        "add_fact_request": {
            "content": "Before merging a PR batch, require stable aggregate verification and a \
                        live PR-state recheck — a single flaky green pass is never enough to merge",
            "category": "project",
            "source_label": "session_reflector",
            "tags": ["session-reflector"],
            "entities": ["merge discipline"],
            "trust": 0.9,
            "metadata": {
                "source_span": { "session_id": "s", "message_id": "m" },
                "trust_reason": "repeated evidence"
            }
        }
    });
    let recorded = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-new",
        Some("evidence-new"),
        &[paraphrase],
    )
    .await
    .unwrap();
    assert!(recorded.retry_error.is_none());
    assert_eq!(
        recorded.receipts.len(),
        1,
        "a paraphrase commits as its own terminal automatic effect"
    );
    assert_eq!(recorded.receipts[0].state, AutomaticFactState::Applied);

    let receipts = list_automatic_fact_receipts(&memory, None, 10, run_control.read_control())
        .await
        .unwrap();
    assert_eq!(receipts.len(), 2, "new automatic effect committed");
    let applied_after =
        load_automatic_fact_receipt(&memory, &applied_id, run_control.read_control())
            .await
            .unwrap()
            .expect("original automatic fact receipt preserved");
    assert_eq!(
        applied_after, applied_before,
        "recording a paraphrase must not mutate a terminal receipt"
    );
}
