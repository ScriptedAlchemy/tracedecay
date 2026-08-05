use md5::{Digest, Md5};
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
    let temp = tempfile::TempDir::new().unwrap();
    let project = temp.path().join("project");
    let share = temp.path().join(".kimi");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(
        share.join("kimi.json"),
        json!({"work_dirs": [{"path": project}]}).to_string(),
    )
    .unwrap();
    let sessions = share.join("sessions").join(format!(
        "{:x}",
        Md5::digest(project.to_string_lossy().as_bytes())
    ));
    for ordinal in 0..count {
        let transcript = sessions
            .join(format!("session-{ordinal:04}"))
            .join("context.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(
            transcript,
            json!({"role": "user", "content": format!("original-{ordinal:04}")}).to_string() + "\n",
        )
        .unwrap();
    }
    (temp, project, sessions, KimiSource::with_share_dir(&share))
}

#[tokio::test]
async fn directory_generation_reset_recovers_a_recreated_entry() {
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
    let before = admission
        .get_parse_offset(&ObservationScopeV1::Profile, KIMI_DISCOVERY_FRONTIER_KEY)
        .await
        .unwrap()
        .unwrap();

    let recreated = sessions.join("session-0000").join("context.jsonl");
    std::fs::remove_dir_all(recreated.parent().unwrap()).unwrap();
    std::fs::create_dir_all(recreated.parent().unwrap()).unwrap();
    std::fs::write(
        &recreated,
        json!({"role": "user", "content": "recreated-entry"}).to_string() + "\n",
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
    let after = admission
        .get_parse_offset(&ObservationScopeV1::Profile, KIMI_DISCOVERY_FRONTIER_KEY)
        .await
        .unwrap()
        .unwrap();

    assert_ne!(after.file_id, before.file_id);
    assert!(admission.observations().iter().any(|stored| {
        stored
            .observation()
            .payload()
            .to_string()
            .contains("recreated-entry")
    }));
}
