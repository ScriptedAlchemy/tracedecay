use super::{
    CodeCloneIndexBudgetsV1, CodeCloneIndexCoverageV1, CodeCloneIndexObservationV1,
    CodeCloneIndexResourcesV1, CodeCloneIndexStatusV1,
};

fn observation() -> CodeCloneIndexObservationV1 {
    CodeCloneIndexObservationV1 {
        generation_id: "generation.clone.1".to_owned(),
        source_revision: Some("commit.clone.1".to_owned()),
        artifact_format_revision: Some(16),
        conservative_normalization_revision: 1,
        rename_normalization_revision: 1,
        coverage: CodeCloneIndexCoverageV1 {
            source_bodies: Some(0),
            eligible_source_bodies: Some(0),
            conservative_normalized_bodies: Some(0),
            near_fingerprint_bodies: Some(0),
            ..Default::default()
        },
        budgets: CodeCloneIndexBudgetsV1 {
            posting_rows: 16_384,
            candidate_bodies: 256,
            verification_bodies: 64,
            verification_token_work: 2_000_000,
            ..Default::default()
        },
        resources: CodeCloneIndexResourcesV1 {
            bytes_on_disk: Some(4_096),
            peak_scratch_memory_bytes: Some(512),
            changed_symbol_update_micros: Some(2_400),
            stale_invalidations: Some(1),
        },
    }
}

#[test]
fn clone_readiness_preserves_all_states_and_a_complete_zero() {
    let sample = observation();
    let statuses = [
        CodeCloneIndexStatusV1::Unavailable {
            reason: "artifact unreadable".to_owned(),
        },
        CodeCloneIndexStatusV1::Backfilling {
            observation: sample.clone(),
        },
        CodeCloneIndexStatusV1::Partial {
            observation: sample.clone(),
            omission_reasons: vec!["fingerprint successor missing".to_owned()],
        },
        CodeCloneIndexStatusV1::Ready {
            observation: sample.clone(),
        },
        CodeCloneIndexStatusV1::Stale {
            observation: sample,
            reason: "superseded generation".to_owned(),
        },
    ];
    let states = statuses
        .into_iter()
        .map(|status| {
            serde_json::to_value(status).expect("clone status serializes")["state"]
                .as_str()
                .expect("clone status is discriminated")
                .to_owned()
        })
        .collect::<Vec<_>>();

    assert_eq!(
        states,
        ["unavailable", "backfilling", "partial", "ready", "stale"]
    );
    let ready = serde_json::to_value(CodeCloneIndexStatusV1::Ready {
        observation: observation(),
    })
    .expect("ready clone status serializes");
    assert_eq!(ready["observation"]["coverage"]["source_bodies"], 0);
    assert_eq!(
        ready["observation"]["coverage"]["eligible_source_bodies"],
        0
    );
    assert_eq!(ready["observation"]["budgets"]["posting_rows"], 16_384);
    assert_eq!(ready["observation"]["budgets"]["candidate_bodies"], 256);
    assert_eq!(ready["observation"]["budgets"]["verification_bodies"], 64);
    assert_eq!(
        ready["observation"]["budgets"]["verification_token_work"],
        2_000_000
    );
    assert_eq!(
        ready["observation"]["resources"]["peak_scratch_memory_bytes"],
        512
    );
}
