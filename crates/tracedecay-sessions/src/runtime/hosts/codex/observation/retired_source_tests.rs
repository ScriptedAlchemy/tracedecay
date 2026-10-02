//! The retired per-session Codex source identity has no admission authority.
//!
//! Stores that hold observation rows written under it predate the unified
//! observation identity and are refused with the scoped session-store reset
//! before admission runs. A leftover cursor for it in an admitted store must
//! therefore neither gate nor narrow what the canonical source admits.

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::ObservationSourceRangeV1;
use tracedecay_store::observation::ObservationCursorAdvance;

use super::*;
use crate::admission::test_support::MemoryHostAdmission;
use crate::runtime::hosts::codex::try_admit_codex_jsonl_observations_for_project_with_admission;
use crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;

const SESSION_ID: &str = "retired-source-session";

fn write_rollout(dir: &Path, project: &Path) -> PathBuf {
    let path = dir.join("rollout.jsonl");
    let lines = [
        json!({
            "timestamp": "2026-09-03T21:08:01.000Z",
            "type": "session_meta",
            "payload": {"id": SESSION_ID, "cwd": project}
        }),
        json!({
            "timestamp": "2026-09-03T21:08:01.250Z",
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {"last_token_usage": {
                "input_tokens": 10,
                "output_tokens": 2,
                "cached_input_tokens": 3,
                "reasoning_output_tokens": 1,
                "total_tokens": 12
            }}}
        }),
        json!({
            "timestamp": "2026-09-03T21:08:01.300Z",
            "type": "event_msg",
            "payload": {"type": "item_completed", "item": {
                "type": "UserMessage",
                "id": "retired-source-user-item",
                "content": [{"type": "text", "text": "Admit every frame once."}]
            }}
        }),
    ];
    std::fs::write(
        &path,
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    path
}

#[tokio::test]
async fn retired_source_cursor_neither_gates_nor_narrows_canonical_admission() {
    install_test_shared_jsonl_preparation_authority();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let path = write_rollout(tmp.path(), &project);
    let project_id = ProjectId::new("project.retired-codex-source").unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };

    // The file's generation checkpoint, as any admission of it records it.
    let scratch = MemoryHostAdmission::default();
    try_admit_codex_jsonl_observations_for_project_with_admission(
        &path,
        &project,
        project_id.clone(),
        &scratch,
        None,
    )
    .await
    .unwrap();
    let checkpoint = scratch
        .get_source_cursor(&codex_observation_source_v2(SESSION_ID).unwrap(), &scope)
        .await
        .unwrap()
        .unwrap();

    let admission = MemoryHostAdmission::default();
    let retired_source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER).unwrap(),
        SessionId::new(SESSION_ID).unwrap(),
    )
    .unwrap();
    admission
        .advance_non_durable_source_cursor(
            ObservationCursorAdvance::new(
                retired_source,
                scope.clone(),
                checkpoint.generation(),
                None,
                ObservationSourceRangeV1::new(0, checkpoint.position()).unwrap(),
                ObservationCoverageReason::UnsupportedFact,
            )
            .unwrap()
            .with_resume_checkpoint(
                checkpoint.file_identity().unwrap(),
                checkpoint.resume_fingerprint().unwrap(),
            ),
            ObservationCancellation::default(),
        )
        .await
        .unwrap();

    let first = try_admit_codex_jsonl_observations_for_project_with_admission(
        &path,
        &project,
        project_id.clone(),
        &admission,
        None,
    )
    .await
    .unwrap();
    assert!(!first.source_deferred);
    assert_eq!(first.frames_persisted, 3);
    assert_eq!(admission.observations().len(), 3);

    let second = try_admit_codex_jsonl_observations_for_project_with_admission(
        &path, &project, project_id, &admission, None,
    )
    .await
    .unwrap();
    assert_eq!(second.frames_persisted, 0);
    assert_eq!(admission.observations().len(), 3);
}
