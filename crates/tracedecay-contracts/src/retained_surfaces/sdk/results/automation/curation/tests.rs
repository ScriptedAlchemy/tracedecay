use serde_json::{Value, json};
use tracedecay_domain::{
    FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, ProjectId, ProvenanceId,
    RunId,
};

use super::super::tests::automation_request;
use super::{
    MemoryAutomationCurationOperationEffectV1, MemoryAutomationCurationReceiptV1,
    curation_receipt_matches,
};
use crate::retained_surfaces::{
    AutomationCommittedReceiptV1, AutomationRunResultV1, AutomationRunSummaryV1,
    AutomationRunTerminalV1, AutomationTaskV1, FactCommitDispositionV1,
};

fn fact(label: &str) -> String {
    let owner = FactOwnerV1::Project {
        project_id: ProjectId::new("project.curation".to_owned()).expect("project id"),
    };
    let source = FactIdentitySourceV1::Application {
        operation_id: ProvenanceId::new(format!("operation.curation.{label}"))
            .expect("operation id"),
    };
    FactId::derive(&FactIdentityMaterialV1::new(owner, source).expect("identity material"))
        .expect("fact id")
        .as_str()
        .to_owned()
}

fn commit(fact_id: &str, label: &str, event_count: usize, active_assertion: Option<&str>) -> Value {
    let events = (0..event_count)
        .map(|index| format!("event.curation.{label}.{index}"))
        .collect::<Vec<_>>();
    json!({
        "disposition":"committed",
        "fact_id":fact_id,
        "owner":{"kind":"project","project_id":"project.curation"},
        "committed_event_ids":events,
        "last_event_id":events.last().expect("event"),
        "active_assertion_id":active_assertion,
    })
}

fn settled(receipt: Value) -> MemoryAutomationCurationReceiptV1 {
    let mut settled = serde_json::from_value::<MemoryAutomationCurationReceiptV1>(json!({
        "receipt": receipt,
        "canonical_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    }))
    .expect("typed curation receipt");
    settled.canonical_digest = settled.canonical_digest().expect("canonical digest");
    settled
}

fn six_effect_receipt() -> MemoryAutomationCurationReceiptV1 {
    let added = fact("added");
    let updated = fact("updated");
    let winner = fact("winner");
    let loser = fact("loser");
    let removed = fact("removed");
    let normalized = fact("normalized");
    let source = fact("source");
    let target = fact("target");
    let evidence = fact("evidence");
    settled(json!({
        "owner":{"kind":"project","project_id":"project.curation"},
        "operation_id":"operation.curation.batch",
        "input_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "automation_run_id":"run.memory.curation",
        "operation_effects":[
            {"kind":"add","fact_id":added,"disposition":"added","closest_fact_id":null,"similarity_millionths":null,"commit":commit(&added,"add",1,Some("assertion.curation.add"))},
            {"kind":"update","fact_id":updated,"trust_delta_millionths":100000,"commit":commit(&updated,"update",1,Some("assertion.curation.update"))},
            {"kind":"merge","outcome":{"operation_id":"operation.curation.merge","input_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","winner_fact_id":winner,"content_updated":true,"deleted_loser_fact_ids":[loser],"commit_receipts":[commit(&winner,"merge-winner",2,Some("assertion.curation.winner")),commit(&loser,"merge-loser",2,None)]}},
            {"kind":"remove","target_fact_id":removed,"disposition":"removed","remaining_fact_count":7,"commit":commit(&removed,"remove",1,None)},
            {"kind":"normalize_tags","fact_id":normalized,"commit":commit(&normalized,"normalize",2,Some("assertion.curation.normalized"))},
            {"kind":"link_facts","source_fact_id":source,"target_fact_id":target,"relation":{"kind":"supports","evidence_fact_ids":[evidence],"confidence_millionths":800000,"provenance":{"source_label":"automation:memory-curator","sanitization_receipt":{"receipt":{"receipt_id":"receipt.curation.relation","sanitizer_version":"sanitizer.memory.v1"},"disposition":"accepted","sensitivity":"non_sensitive","payload":{"digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","byte_len":128}}}},"disposition":"linked","commit":commit(&source,"link",1,Some("assertion.curation.source"))}
        ],
        "replay_fact_id":added,
        "replay_event_id":"event.curation.add.0",
        "changed_fact_ids":[added,updated,winner,loser,removed,normalized,source,target],
        "accepted_operations":6,
        "facts_added":1,
        "facts_updated":1,
        "facts_merged":1,
        "facts_removed":1,
        "normalized_tags":1,
        "facts_linked":1
    }))
}

fn no_op_receipt() -> MemoryAutomationCurationReceiptV1 {
    let duplicate = fact("duplicate");
    let removed = fact("already-removed");
    settled(json!({
        "owner":{"kind":"project","project_id":"project.curation"},
        "operation_id":"operation.curation.noop",
        "input_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        "automation_run_id":"run.memory.curation",
        "operation_effects":[
            {"kind":"add","fact_id":duplicate,"disposition":"near_duplicate","closest_fact_id":duplicate,"similarity_millionths":1000000,"commit":null},
            {"kind":"remove","target_fact_id":removed,"disposition":"already_removed","remaining_fact_count":7,"commit":null}
        ],
        "replay_fact_id":null,
        "replay_event_id":null,
        "changed_fact_ids":[],
        "accepted_operations":2,
        "facts_added":0,
        "facts_updated":0,
        "facts_merged":0,
        "facts_removed":0,
        "normalized_tags":0,
        "facts_linked":0
    }))
}

fn matches(receipt: &MemoryAutomationCurationReceiptV1) -> bool {
    curation_receipt_matches(&RunId::new("run.memory.curation").expect("run id"), receipt)
}

fn redigest(receipt: &mut MemoryAutomationCurationReceiptV1) {
    receipt.canonical_digest = receipt.canonical_digest().expect("canonical digest");
}

#[test]
fn six_effect_receipt_preserves_ordered_mutation_and_relation_authority() {
    assert!(matches(&six_effect_receipt()));
}

#[test]
fn all_noop_receipt_retains_acceptance_without_fabricating_mutations_or_anchors() {
    let receipt = no_op_receipt();
    assert_eq!(receipt.receipt.accepted_operations, 2);
    assert!(receipt.receipt.changed_fact_ids.is_empty());
    assert!(receipt.receipt.replay_fact_id.is_none());
    assert!(receipt.receipt.replay_event_id.is_none());
    assert!(matches(&receipt));

    let result = AutomationRunResultV1 {
        run_id: RunId::new("run.memory.curation").expect("run id"),
        task: AutomationTaskV1::MemoryCurator,
        request_digest: automation_request("run.memory.curation", AutomationTaskV1::MemoryCurator)
            .input_digest()
            .expect("request digest"),
        terminal: AutomationRunTerminalV1::Completed {
            summary: AutomationRunSummaryV1 {
                reviewed_count: 2,
                accepted_count: 2,
                rejected_count: 0,
                skipped_count: 0,
            },
        },
        committed_receipts: vec![AutomationCommittedReceiptV1::Curation(receipt)],
    };
    assert!(result.matches_terminal());

    let mut wrong_count = result;
    let AutomationRunTerminalV1::Completed { summary } = &mut wrong_count.terminal else {
        panic!("completed fixture")
    };
    summary.accepted_count = 0;
    summary.reviewed_count = 0;
    assert!(!wrong_count.matches_terminal());
}

#[test]
fn curation_summary_anchors_and_events_are_not_relabelable() {
    let canonical = six_effect_receipt();

    let mut wrong_accepted = canonical.clone();
    wrong_accepted.receipt.accepted_operations = 5;
    redigest(&mut wrong_accepted);
    assert!(!matches(&wrong_accepted));

    let mut missing_anchor = canonical.clone();
    missing_anchor.receipt.replay_event_id = None;
    redigest(&mut missing_anchor);
    assert!(!matches(&missing_anchor));

    let mut duplicate_event = canonical;
    let first_event = match &duplicate_event.receipt.operation_effects[0] {
        MemoryAutomationCurationOperationEffectV1::Add {
            commit: Some(commit),
            ..
        } => commit.committed_event_ids[0].clone(),
        _ => panic!("add fixture"),
    };
    let MemoryAutomationCurationOperationEffectV1::Update { commit, .. } =
        &mut duplicate_event.receipt.operation_effects[1]
    else {
        panic!("update fixture")
    };
    commit.committed_event_ids[0] = first_event.clone();
    commit.last_event_id = first_event;
    redigest(&mut duplicate_event);
    assert!(!matches(&duplicate_event));
}

#[test]
fn effect_limit_and_batch_disposition_are_exact() {
    let mut too_many = no_op_receipt();
    let effect = too_many.receipt.operation_effects[0].clone();
    too_many.receipt.operation_effects = vec![effect; 257];
    too_many.receipt.accepted_operations = 257;
    redigest(&mut too_many);
    assert!(!matches(&too_many));

    let mut mixed_dispositions = six_effect_receipt();
    let MemoryAutomationCurationOperationEffectV1::Update { commit, .. } =
        &mut mixed_dispositions.receipt.operation_effects[1]
    else {
        panic!("update fixture")
    };
    commit.disposition = FactCommitDispositionV1::IdempotentReplay;
    redigest(&mut mixed_dispositions);
    assert!(!matches(&mixed_dispositions));
}

#[test]
fn merge_commit_order_and_changed_union_are_exact() {
    let canonical = six_effect_receipt();

    let mut swapped_commits = canonical.clone();
    let MemoryAutomationCurationOperationEffectV1::Merge { outcome } =
        &mut swapped_commits.receipt.operation_effects[2]
    else {
        panic!("merge fixture")
    };
    outcome.commit_receipts.swap(0, 1);
    redigest(&mut swapped_commits);
    assert!(!matches(&swapped_commits));

    let mut reordered_changed = canonical;
    reordered_changed.receipt.changed_fact_ids.swap(0, 1);
    redigest(&mut reordered_changed);
    assert!(!matches(&reordered_changed));
}
