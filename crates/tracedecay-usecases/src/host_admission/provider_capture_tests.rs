use std::path::{Path, PathBuf};

use serde_json::Value;
use tracedecay_domain::{CanonicalObservationEnvelopeV1, ObservationScopeV1};
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_sessions::runtime::cline_like::{
    ClineLikeSource, capture_cline_like_snapshot_observations,
};
use tracedecay_sessions::runtime::kiro::{KiroSource, capture_kiro_snapshot_observations};
use tracedecay_sessions::runtime::source::{TranscriptIngestError, TranscriptSource};
use tracedecay_store::ObservationReplayRequest;

use super::{HostAdmissionScope, HostAdmissionTestRuntimeV1};

fn cline_source_and_tasks(home: &Path, provider: &str) -> (ClineLikeSource, PathBuf) {
    match provider {
        "cline" => {
            let tasks = tracedecay_sessions::host_ports::vscode_data_dir(home)
                .join("User/globalStorage/saoudrizwan.claude-dev/tasks");
            (ClineLikeSource::cline_with_home(home), tasks)
        }
        "roo-code" => {
            let tasks = tracedecay_sessions::host_ports::vscode_data_dir(home)
                .join("User/globalStorage/rooveterinaryinc.roo-cline/tasks");
            (ClineLikeSource::roo_code_with_home(home), tasks)
        }
        "kilo" => {
            let tasks = home.join(".kilocode/cli/global/tasks");
            (ClineLikeSource::kilo_with_home(home), tasks)
        }
        other => panic!("unsupported Cline-family provider {other}"),
    }
}

fn write_checked_in_native_task(tasks: &Path, project: &Path, api_filename: &str) -> PathBuf {
    let task = tasks.join("checked-in-native");
    std::fs::create_dir_all(&task).unwrap();
    let mut metadata: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/transcript_golden/cline_like/input/task_metadata.json"
    ))
    .unwrap();
    metadata["workspacePath"] = Value::String(project.to_string_lossy().into_owned());
    std::fs::write(
        task.join("task_metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let fixture = match api_filename {
        "api_messages.json" => include_str!(
            "../../../../tests/fixtures/transcript_golden/cline_like/input/api_messages.json"
        ),
        "api_conversation_history.json" => include_str!(
            "../../../../tests/fixtures/transcript_golden/cline_like/input/api_conversation_history.json"
        ),
        other => panic!("unsupported checked-in Cline-family fixture {other}"),
    };
    let api = task.join(api_filename);
    std::fs::write(&api, fixture).unwrap();
    std::fs::write(
        task.join("ui_messages.json"),
        include_str!(
            "../../../../tests/fixtures/transcript_golden/cline_like/input/ui_messages.json"
        ),
    )
    .unwrap();
    api
}

fn cline_task_bytes(path: &Path) -> u64 {
    let task = path.parent().unwrap();
    [
        path.to_path_buf(),
        task.join("ui_messages.json"),
        task.join("task_metadata.json"),
        task.join("history_item.json"),
        task.join("history.json"),
    ]
    .into_iter()
    .filter_map(|path| std::fs::metadata(path).ok())
    .map(|metadata| metadata.len())
    .sum()
}

fn kiro_fixture_roots(home: &Path, project: &Path) -> (KiroSource, PathBuf, PathBuf) {
    let hash = "0123456789abcdef0123456789abcdef";
    let data_dir = tracedecay_sessions::host_ports::kiro_data_dir(home);
    let workspace_dir = data_dir
        .join("User/globalStorage/kiro.kiroagent")
        .join(hash);
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace_metadata = data_dir
        .join("User/workspaceStorage")
        .join(hash)
        .join("workspace.json");
    std::fs::create_dir_all(workspace_metadata.parent().unwrap()).unwrap();
    std::fs::write(
        &workspace_metadata,
        serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
    )
    .unwrap();
    (
        KiroSource::with_home(home),
        workspace_dir,
        workspace_metadata,
    )
}

fn kiro_snapshot_bytes(path: &Path, workspace_metadata: &Path) -> u64 {
    std::fs::metadata(path).unwrap().len() + std::fs::metadata(workspace_metadata).unwrap().len()
}

#[tokio::test]
async fn checked_in_cline_family_snapshots_preserve_receipts_through_failures_and_restart() {
    for (provider, api_filename) in [
        ("cline", "api_conversation_history.json"),
        ("roo-code", "api_messages.json"),
        ("kilo", "api_conversation_history.json"),
    ] {
        let temp = tempfile::TempDir::new().expect("temp Cline-family storage");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let (source, tasks) = cline_source_and_tasks(temp.path(), provider);
        let api = write_checked_in_native_task(&tasks, &project, api_filename);
        let profile_root = temp.path().join("profile");
        let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .expect("open canonical observation runtime");
        let facade = runtime.facade();

        let first = capture_cline_like_snapshot_observations(
            &facade,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("{provider}: checked-in capture failed: {error}"));
        assert_eq!(first.stats.messages_upserted, 3, "{provider}");

        let observations = runtime
            .replay_observations(
                HostAdmissionScope::Profile,
                ObservationReplayRequest::new(0, 16).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(observations.len(), 3, "{provider}");
        let receipts = observations
            .iter()
            .map(|item| {
                let envelope: CanonicalObservationEnvelopeV1 =
                    serde_json::from_value(item.observation().payload().clone())
                        .unwrap_or_else(|error| panic!("{provider}: canonical envelope: {error}"));
                assert_eq!(envelope.provider().as_str(), provider);
                item.commit_receipt().clone()
            })
            .collect::<Vec<_>>();
        for receipt in &receipts {
            let committed = runtime
                .external_source_receipt_for_test(HostAdmissionScope::Profile, receipt)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{provider}: missing external-source receipt"));
            assert_eq!(committed.projection().effects().len(), 1, "{provider}");
        }

        for _ in 0..16 {
            capture_cline_like_snapshot_observations(
                &facade,
                &source,
                &project,
                ObservationScopeV1::Profile,
                None,
                &ObservationCancellation::default(),
            )
            .await
            .unwrap_or_else(|error| panic!("{provider}: duplicate storm failed: {error}"));
        }
        assert_eq!(
            runtime
                .replay_observations(
                    HostAdmissionScope::Profile,
                    ObservationReplayRequest::new(0, 16).unwrap(),
                )
                .await
                .unwrap()
                .len(),
            3,
            "{provider}: duplicate storm must coalesce"
        );

        let original = std::fs::read(&api).unwrap();
        std::fs::write(&api, b"[{\"role\":}]").unwrap();
        let poison = capture_cline_like_snapshot_observations(
            &facade,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .expect_err("malformed replacement must remain non-durable");
        assert!(
            matches!(
                poison,
                TranscriptIngestError::NonDurableRecord {
                    reason: "malformed snapshot JSON",
                    ..
                }
            ),
            "{provider}: {poison:?}"
        );
        std::fs::write(&api, original).unwrap();

        let cancelled = ObservationCancellation::default();
        cancelled.cancel();
        let cancelled_outcome = capture_cline_like_snapshot_observations(
            &facade,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &cancelled,
        )
        .await
        .expect_err("cancelled capture must stop before persistence");
        assert!(
            matches!(
                cancelled_outcome,
                TranscriptIngestError::NonDurableRecord {
                    reason: "admission_cancelled",
                    ..
                }
            ),
            "{provider}: {cancelled_outcome:?}"
        );
        drop(facade);
        drop(runtime);

        let reopened = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .expect("reopen canonical observation runtime");
        for receipt in &receipts {
            let committed = reopened
                .external_source_receipt_for_test(HostAdmissionScope::Profile, receipt)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{provider}: receipt lost across restart"));
            assert_eq!(committed.projection().effects().len(), 1, "{provider}");
        }
        let facade = reopened.facade();
        let replay = capture_cline_like_snapshot_observations(
            &facade,
            &source,
            &project,
            ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("{provider}: restart replay failed: {error}"));
        assert_eq!(
            replay.stats.messages_upserted, 0,
            "{provider}: acknowledgement-boundary replay must be exact"
        );
    }
}

#[tokio::test]
async fn cline_byte_budget_charges_once_and_defers_second_before_parse() {
    let temp = tempfile::TempDir::new().expect("temp Cline storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (source, tasks) = cline_source_and_tasks(temp.path(), "cline");
    let first_task = tasks.join("a-first");
    std::fs::create_dir_all(&first_task).unwrap();
    std::fs::write(
        first_task.join("api_messages.json"),
        serde_json::json!([{"role": "assistant", "content": "first"}]).to_string(),
    )
    .unwrap();
    std::fs::write(
        first_task.join("task_metadata.json"),
        serde_json::json!({"cwd": project}).to_string(),
    )
    .unwrap();
    let second_task = tasks.join("z-hostile");
    std::fs::create_dir_all(&second_task).unwrap();
    let hostile = format!("[{}", "x".repeat(256));
    std::fs::write(second_task.join("api_messages.json"), &hostile).unwrap();
    std::fs::write(
        second_task.join("task_metadata.json"),
        serde_json::json!({"cwd": project}).to_string(),
    )
    .unwrap();
    let paths = source.transcript_paths(&project);
    assert_eq!(paths.len(), 2);
    let first_bytes = cline_task_bytes(&paths[0]);
    let second_bytes = cline_task_bytes(&paths[1]);

    let runtime = HostAdmissionTestRuntimeV1::profile(temp.path().join("profile"))
        .await
        .expect("open registered observation runtime");
    let facade = runtime.facade();
    let cancellation = ObservationCancellation::default();
    let deferred = capture_cline_like_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("second unit must defer without parsing malformed JSON");
    assert!(deferred.deferred_by_byte_cap);
    assert_eq!(deferred.stats.messages_upserted, 1);
    assert_eq!(deferred.bytes_consumed, first_bytes);

    std::fs::remove_dir_all(first_task).unwrap();
    let err = capture_cline_like_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(second_bytes),
        &cancellation,
    )
    .await
    .expect_err("deferred malformed snapshot must remain retryable");
    assert!(matches!(
        err,
        TranscriptIngestError::NonDurableRecord {
            reason: "malformed snapshot JSON",
            ..
        }
    ));
}

#[tokio::test]
async fn kiro_byte_budget_charges_once_and_defers_second_before_parse() {
    let temp = tempfile::TempDir::new().expect("temp Kiro storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (source, workspace_dir, workspace_metadata) = kiro_fixture_roots(temp.path(), &project);
    let first_path = workspace_dir.join("a-first.chat");
    std::fs::write(
        &first_path,
        serde_json::json!({
            "executionId": "first",
            "chat": [{"role": "human", "content": "first"}]
        })
        .to_string(),
    )
    .unwrap();
    let hostile = format!("{{\"executionId\":\"hostile\",\"chat\":{}", "x".repeat(256));
    let second_path = workspace_dir.join("z-hostile.chat");
    std::fs::write(&second_path, &hostile).unwrap();
    let paths = source.transcript_paths(&project);
    assert_eq!(paths, vec![first_path.clone(), second_path.clone()]);
    let first_bytes = kiro_snapshot_bytes(&first_path, &workspace_metadata);
    let second_bytes = kiro_snapshot_bytes(&second_path, &workspace_metadata);

    let runtime = HostAdmissionTestRuntimeV1::profile(temp.path().join("profile"))
        .await
        .expect("open registered observation runtime");
    let facade = runtime.facade();
    let cancellation = ObservationCancellation::default();
    let deferred = capture_kiro_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("second unit must defer without parsing malformed JSON");
    assert!(deferred.deferred_by_byte_cap);
    assert_eq!(deferred.stats.messages_upserted, 1);
    assert_eq!(deferred.bytes_consumed, first_bytes);

    std::fs::remove_file(first_path).unwrap();
    let err = capture_kiro_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(second_bytes),
        &cancellation,
    )
    .await
    .expect_err("deferred malformed snapshot must remain retryable");
    assert!(matches!(
        err,
        TranscriptIngestError::NonDurableRecord {
            reason: "malformed snapshot JSON",
            ..
        }
    ));
}

#[tokio::test]
async fn kiro_aggregate_budget_replay_charges_committed_prefix_and_retries_suffix() {
    let temp = tempfile::TempDir::new().expect("temp Kiro storage");
    let project = temp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let (source, workspace_dir, workspace_metadata) = kiro_fixture_roots(temp.path(), &project);
    for id in ["a", "b"] {
        std::fs::write(
            workspace_dir.join(format!("{id}.chat")),
            serde_json::json!({
                "executionId": id,
                "chat": [{"role": "human", "content": format!("message-{id}")}]
            })
            .to_string(),
        )
        .unwrap();
    }
    let paths = source.transcript_paths(&project);
    assert_eq!(paths.len(), 2);
    let first_bytes = kiro_snapshot_bytes(&paths[0], &workspace_metadata);
    let second_bytes = kiro_snapshot_bytes(&paths[1], &workspace_metadata);
    let full_cap = first_bytes.saturating_add(second_bytes);
    let runtime = HostAdmissionTestRuntimeV1::profile(temp.path().join("profile"))
        .await
        .expect("open registered observation runtime");
    let facade = runtime.facade();
    let cancellation = ObservationCancellation::default();

    let first = capture_kiro_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("first bounded sweep");
    assert_eq!(first.stats.messages_upserted, 1);
    assert_eq!(first.bytes_consumed, first_bytes);
    assert!(first.deferred_by_byte_cap);

    let second = capture_kiro_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(first_bytes),
        &cancellation,
    )
    .await
    .expect("committed prefix replay");
    assert_eq!(second.stats.messages_upserted, 0);
    assert_eq!(second.bytes_consumed, first_bytes);
    assert!(second.deferred_by_byte_cap);

    let resumed = capture_kiro_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(full_cap),
        &cancellation,
    )
    .await
    .expect("deferred suffix replay");
    assert_eq!(resumed.stats.messages_upserted, 1);
    assert_eq!(resumed.bytes_consumed, full_cap);
    assert!(!resumed.deferred_by_byte_cap);

    let complete = capture_kiro_snapshot_observations(
        &facade,
        &source,
        &project,
        ObservationScopeV1::Profile,
        Some(full_cap),
        &cancellation,
    )
    .await
    .expect("complete replay");
    assert_eq!(complete.stats.messages_upserted, 0);
    assert_eq!(complete.bytes_consumed, full_cap);
    assert!(!complete.deferred_by_byte_cap);
}
