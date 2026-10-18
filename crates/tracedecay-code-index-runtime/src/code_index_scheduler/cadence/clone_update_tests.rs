use std::path::PathBuf;

use tracedecay_domain::{CodeGenerationId, ContentDigest};

use super::{
    CodeIndexArrivalV1, CodeIndexCadenceOutcomeV1, CodeIndexCadenceTelemetryV1,
    CodeIndexCadenceTriggerV1, CodeIndexCloneUpdateV1, CodeIndexEventToReadyReceiptV1,
};

fn noop_receipt(index: usize) -> CodeIndexEventToReadyReceiptV1 {
    let wake_micros = i64::try_from(index).expect("fixture index");
    CodeIndexEventToReadyReceiptV1::new(
        PathBuf::from("/tmp/clone-project"),
        CodeIndexCadenceTriggerV1::HookHint,
        CodeIndexArrivalV1::Observed { wake_micros },
        700,
        800,
        CodeIndexCadenceOutcomeV1::Noop {
            snapshot_content_identity: ContentDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("content digest"),
        },
        false,
    )
}

#[test]
fn latest_clone_update_survives_cadence_receipt_rollover() {
    let project_root = PathBuf::from("/tmp/clone-project");
    let generation_id = CodeGenerationId::new("generation.clone.2").expect("generation identity");
    let published = CodeIndexEventToReadyReceiptV1::new(
        project_root.clone(),
        CodeIndexCadenceTriggerV1::HookHint,
        CodeIndexArrivalV1::Observed { wake_micros: 100 },
        200,
        600,
        CodeIndexCadenceOutcomeV1::Published {
            generation_id: generation_id.clone(),
            reextracted_files: 1,
            changed_chunks: 1,
            reused_chunks: 1,
            clone_payloads_reused: Some(1),
            clone_stale_invalidations: Some(1),
            clone_body_changes_observed: Some(true),
        },
        false,
    );
    let mut telemetry = CodeIndexCadenceTelemetryV1::default();
    telemetry.record(published);
    for index in 0..CodeIndexCadenceTelemetryV1::CAPACITY {
        telemetry.record(noop_receipt(index));
    }

    assert_eq!(
        telemetry.latest_clone_update(&project_root, &generation_id),
        Some(CodeIndexCloneUpdateV1 {
            payloads_reused: Some(1),
            stale_invalidations: Some(1),
            changed_symbol_update_micros: Some(500),
        })
    );

    let unchanged_generation =
        CodeGenerationId::new("generation.clone.3").expect("generation identity");
    telemetry.record(CodeIndexEventToReadyReceiptV1::new(
        project_root.clone(),
        CodeIndexCadenceTriggerV1::HookHint,
        CodeIndexArrivalV1::Observed { wake_micros: 900 },
        1_000,
        1_400,
        CodeIndexCadenceOutcomeV1::Published {
            generation_id: unchanged_generation.clone(),
            reextracted_files: 1,
            changed_chunks: 0,
            reused_chunks: 1,
            clone_payloads_reused: Some(1),
            clone_stale_invalidations: Some(0),
            clone_body_changes_observed: Some(false),
        },
        false,
    ));
    assert_eq!(
        telemetry
            .latest_clone_update(&project_root, &unchanged_generation)
            .and_then(|update| update.changed_symbol_update_micros),
        None
    );
}
