use serde_json::json;
use tracedecay_application::retained_surfaces::{
    MemoryAutomationCommittedReceiptV1, MemoryAutomationCurationReceiptV1, MemoryAutomationTaskV1,
};
use tracedecay_domain::{FactId, FactOwnerV1, ProvenanceId, RunId, canonical_sha256};
use tracedecay_store::ProjectMemoryFactCurationReceiptV1;

use super::{project_curation_receipt, project_run_summary};

fn fact(suffix: char) -> FactId {
    FactId::new(format!(
        "fact.{}.{}",
        "0".repeat(64),
        suffix.to_string().repeat(64)
    ))
    .expect("fact id")
}

#[test]
fn all_noop_curation_projects_accepted_effects_without_mutation_or_anchors() {
    let owner = FactOwnerV1::Profile;
    let run_id = RunId::new("run.projection.all-noop").expect("run id");
    let duplicate = fact('1');
    let absent = fact('2');
    let receipt: ProjectMemoryFactCurationReceiptV1 = serde_json::from_value(json!({
        "owner": owner,
        "operation_id": ProvenanceId::new("operation.projection.all-noop").expect("operation id"),
        "input_digest": "a".repeat(64),
        "automation_run_id": run_id,
        "operation_effects": [{
            "kind": "add",
            "fact_id": duplicate,
            "disposition": "near_duplicate",
            "closest_fact_id": duplicate,
            "similarity_millionths": 1_000_000,
            "commit": null,
        }, {
            "kind": "remove",
            "target_fact_id": absent,
            "disposition": "not_found",
            "remaining_fact_count": 0,
            "commit": null,
        }],
        "replay_fact_id": null,
        "replay_event_id": null,
        "changed_fact_ids": [],
        "accepted_operations": 2,
        "facts_added": 0,
        "facts_updated": 0,
        "facts_merged": 0,
        "facts_removed": 0,
        "normalized_tags": 0,
        "facts_linked": 0,
    }))
    .expect("canonical all-noop store receipt");

    let projected = project_curation_receipt(&receipt, receipt.replayed())
        .expect("project canonical all-noop receipt");
    assert_eq!(projected.accepted_operations, 2);
    assert_eq!(projected.facts_added, 0);
    assert_eq!(projected.facts_removed, 0);
    assert!(projected.changed_fact_ids.is_empty());
    assert!(projected.replay_fact_id.is_none());
    assert!(projected.replay_event_id.is_none());

    let canonical_digest = canonical_sha256(&(
        "tracedecay.memory-automation-run.curation-receipt.v1",
        &projected,
    ))
    .expect("canonical digest");
    let summary = project_run_summary(
        MemoryAutomationTaskV1::MemoryCurator,
        &[MemoryAutomationCommittedReceiptV1::Curation(
            MemoryAutomationCurationReceiptV1 {
                receipt: projected,
                canonical_digest,
            },
        )],
    )
    .expect("all-noop summary");

    assert_eq!(summary.reviewed_count, 2);
    assert_eq!(summary.accepted_count, 2);
    assert_eq!(summary.rejected_count, 0);
}
