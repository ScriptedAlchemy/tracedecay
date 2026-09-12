use serde_json::json;
use tracedecay_domain::ObservationScopeV1;

use crate::admission::{HostAdmission, test_support::MemoryHostAdmission};
use crate::observation::ObservationCancellation;

use super::{
    KIMI_DISCOVERY_FRONTIER_KEY, KimiSource, MAX_SESSION_FILES, capture_kimi_observations,
};

fn populated_source(
    count: usize,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    KimiSource,
) {
    crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::TempDir::new().unwrap();
    let project = temp.path().join("project");
    let share = temp.path().join(".kimi-code");
    let sessions = share.join("sessions/wd_project");
    std::fs::create_dir_all(&project).unwrap();
    for ordinal in 0..count {
        let session = sessions.join(format!("session-{ordinal:04}"));
        let transcript = session.join("agents/main/wire.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            session.join("state.json"),
            json!({
                "id": format!("session-{ordinal:04}"),
                "version": 2,
                "cwd": project,
                "agents": {"main": {"type": "main"}}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            transcript,
            json!({
                "type": "context.append_message",
                "agentId": "main",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": format!("original-{ordinal:04}")}],
                    "toolCalls": []
                },
                "time": 1_789_228_081_157_u64
            })
            .to_string()
                + "\n",
        )
        .unwrap();
    }
    (temp, project, sessions, KimiSource::with_share_dir(&share))
}

#[tokio::test]
async fn durable_queue_revisits_a_recreated_entry() {
    let (_temp, project, sessions, source) = populated_source(MAX_SESSION_FILES + 1);
    let admission = MemoryHostAdmission::default();
    for _ in 0..2 {
        capture_kimi_observations(
            &admission,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
    }
    assert_eq!(admission.observations().len(), MAX_SESSION_FILES + 1);
    let recreated = sessions.join("session-0000/agents/main/wire.jsonl");
    std::fs::write(
        &recreated,
        json!({
            "type": "context.append_message",
            "message": {"role": "user", "content": "recreated-entry"},
            "time": 1_789_228_081_158_u64
        })
        .to_string()
            + "\n",
    )
    .unwrap();

    for _ in 0..2 {
        capture_kimi_observations(
            &admission,
            &KimiSource::with_share_dir(&source.share_dir),
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
    }
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("recreated-entry")
    }));
}

#[tokio::test]
async fn cancelled_discovery_does_not_advance_the_durable_frontier() {
    let (_temp, project, _sessions, source) = populated_source(MAX_SESSION_FILES + 1);
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome = capture_kimi_observations(
        &admission,
        &source,
        &project,
        ObservationScopeV1::Profile,
        None,
        &cancellation,
    )
    .await
    .unwrap();

    assert!(outcome.deferred);
    assert!(admission.observations().is_empty());
    assert!(
        admission
            .get_parse_offset(&ObservationScopeV1::Profile, KIMI_DISCOVERY_FRONTIER_KEY,)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cancellation_after_discovery_preserves_the_exact_frontier() {
    let (_temp, project, sessions, source) = populated_source(MAX_SESSION_FILES + 1);
    let admission = MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Profile;
    let cursor = admission
        .enqueue_discovery_paths(
            &scope,
            "kimi",
            vec![sessions.join("session-0000/agents/main/wire.jsonl")],
        )
        .await
        .unwrap()
        .unwrap();
    let original = tracedecay_store::ParseOffset {
        byte_offset: 17,
        mtime: 23,
        file_id: cursor.sequence,
    };
    admission
        .advance_parse_offset(&scope, KIMI_DISCOVERY_FRONTIER_KEY, original)
        .await
        .unwrap();
    let cancellation = ObservationCancellation::default();
    admission.cancel_on_next_discovery_queue_read(cancellation.clone());

    let outcome = capture_kimi_observations(
        &admission,
        &source,
        &project,
        scope.clone(),
        None,
        &cancellation,
    )
    .await
    .unwrap();

    assert!(outcome.deferred);
    assert_eq!(
        admission
            .get_parse_offset(&scope, KIMI_DISCOVERY_FRONTIER_KEY)
            .await
            .unwrap(),
        Some(original)
    );
}
