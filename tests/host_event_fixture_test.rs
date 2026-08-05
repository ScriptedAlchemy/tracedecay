use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
    HostAdmissionStatus, HostAdmissionTestRuntimeV1,
};
use tracedecay::application::observation::{CaptureObservationRequest, ObservationCancellation};
use tracedecay::privacy::{ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay::sessions::source::TranscriptSource;
use tracedecay::sessions::{claude, codex, cursor, hermes};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, DurableObservationV1,
    LocatorDigest, ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1,
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, ProjectId, ProviderId, RetentionClass,
    SessionId, SourceAcquisitionCapabilitiesV1, SourceAcquisitionContractV1,
    SourceAggregateFrontierV1, SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceCoverageV1, SourceCursorV1, SourceDefinitionV1,
    SourceDeletionSemanticsV1, SourceInstanceId, SourceNativeObjectIdV1, SourceObjectObservationV1,
    SourceObjectRevisionV1, SourcePartitionFrontierV1, SourcePartitionIdV1,
    SourceRefetchStrategyV1, SourceSnapshotIdV1, UserProfileId, canonical_sha256,
};
use tracedecay_store::ObservationReplayRequest;

mod common;

use common::{
    EnvVarGuard, GLOBAL_DB_ENV_LOCK, git_program, spawn_tracedecay_daemon,
    tracedecay_command_with_home,
};

const FIXTURES: [(&str, &str); 5] = [
    (
        "codex",
        include_str!("fixtures/host_events/codex/baseline.json"),
    ),
    (
        "claude",
        include_str!("fixtures/host_events/claude/baseline.json"),
    ),
    (
        "cursor",
        include_str!("fixtures/host_events/cursor/baseline.json"),
    ),
    (
        "hermes",
        include_str!("fixtures/host_events/hermes/baseline.json"),
    ),
    (
        "kiro",
        include_str!("fixtures/host_events/kiro/baseline.json"),
    ),
];

const HOST_ADMISSION_PROVIDERS: &[&str] = &[
    "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
];

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_host_event_fixtures_execute_provider_admission_paths() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let host = TempDir::new().unwrap();
    let home = host.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data_root = home.join(".tracedecay");
    std::fs::create_dir_all(&data_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&data_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let _data_dir = EnvVarGuard::set(tracedecay::config::USER_DATA_DIR_ENV, &data_root);
    let boundary_project = initialize_boundary_project(&home);
    let init = tracedecay_command_with_home(&home)
        .arg("init")
        .current_dir(&boundary_project)
        .output()
        .expect("initialize host event fixture project");
    assert!(
        init.status.success(),
        "host event fixture init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let _daemon = spawn_tracedecay_daemon(&home);
    let transcript_path = write_claude_boundary_transcript(&home, &boundary_project);
    let unavailable = HostAdmissionFacade::new(HostAdmissionAuthorities::default());

    for (provider, fixture) in FIXTURES {
        let supported = execute_native_provider_path(provider, &home).await;
        assert_eq!(
            supported.status,
            HostAdmissionStatus::Supported,
            "{provider}"
        );
        let document: Value = serde_json::from_str(fixture).expect("valid host fixture JSON");
        assert_eq!(document["schema_version"], 1, "{provider}");
        assert_eq!(document["provider"], provider, "{provider}");

        let cases = document["cases"].as_array().expect("fixture cases");
        assert_eq!(cases.len(), 4, "{provider}");
        let mut states = Vec::new();

        for case in cases {
            let state = case["state"].as_str().expect("state");
            states.push(state);
            assert!(case["request"].is_object(), "{provider}/{state}");
            assert_redacted(provider, state, case);

            let actual = &case["admission"];
            match state {
                "supported" => assert_eq!(
                    actual,
                    &serde_json::to_value(supported).unwrap(),
                    "{provider}/{state}"
                ),
                "unavailable" => assert_eq!(
                    actual,
                    &serde_json::to_value(unavailable.probe(provider, HostAdmissionScope::Project))
                        .unwrap(),
                    "{provider}/{state}"
                ),
                "unknown" => assert_eq!(
                    actual,
                    &serde_json::to_value(
                        unavailable.probe(
                            case["admission_provider"]
                                .as_str()
                                .expect("unknown provider"),
                            HostAdmissionScope::Project,
                        )
                    )
                    .unwrap(),
                    "{provider}/{state}"
                ),
                "degraded" => assert_eq!(
                    actual,
                    &serde_json::to_value(HostAdmissionOutcome::spool_record_too_large()).unwrap(),
                    "{provider}/{state}"
                ),
                other => panic!("unexpected fixture state {other}"),
            }

            let request = materialize_host_request(
                &case["request"],
                provider,
                &boundary_project,
                &transcript_path,
            );
            let completed_before = hook_completed_rows(&boundary_project, &home, provider).len();
            let output = execute_host_boundary(provider, &home, &boundary_project, &request);
            assert_legal_host_response(provider, state, &case["response"], output);
            let completed = hook_completed_rows(&boundary_project, &home, provider);
            assert!(
                completed.len() > completed_before,
                "{provider}/{state} did not emit hook_completed analytics"
            );
            let row = completed
                .last()
                .expect("new hook_completed row should exist");
            assert_hook_completed_analytics(provider, state, &boundary_project, row);
        }

        states.sort_unstable();
        assert_eq!(
            states,
            ["degraded", "supported", "unavailable", "unknown"],
            "{provider}"
        );
    }
}

fn hook_completed_rows(project: &Path, home: &Path, provider: &str) -> Vec<Value> {
    let mut paths = vec![
        tracedecay::storage::resolve_layout_for_current_profile(project)
            .expect("resolve host fixture storage")
            .data_root
            .join("hook_analytics.jsonl"),
        home.join(".tracedecay/hook_analytics.jsonl"),
    ];
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .flat_map(|path| {
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|row| row["event"] == "hook_completed" && row["agent"] == provider)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn assert_hook_completed_analytics(provider: &str, state: &str, project: &Path, row: &Value) {
    let label = format!("{provider}/{state}");
    assert_eq!(row["schema_version"], 1, "{label}");
    assert_eq!(row["coverage"], "host_measured", "{label}");
    assert_eq!(row["agent"], provider, "{label}");
    for field in [
        "duration_us",
        "duration_ms",
        "hook_wall_time_us",
        "hook_wall_time_ms",
        "payload_bytes",
        "daemon_call_count",
    ] {
        assert!(row[field].as_u64().is_some(), "{label}/{field}: {row}");
    }
    assert!(
        row["payload_bytes"].as_u64().is_some_and(|bytes| bytes > 0),
        "{label}"
    );

    // Daemon IPC is conditional, and the row must say which happened rather
    // than render "no IPC" as a zero. A hook whose synchronous admission
    // budget expires before it reaches the daemon spools the event locally and
    // performs no IPC at all, so both the round-trip time and the byte count
    // are genuinely absent. Whenever a round trip did happen, both must be
    // attributed, and a recorded byte count is never a dishonest zero.
    let daemon_calls = row["daemon_call_count"].as_u64().unwrap_or_default();
    let ipc_bytes = &row["daemon_ipc_payload_bytes"];
    assert!(
        ipc_bytes.is_null() || ipc_bytes.as_u64().is_some_and(|bytes| bytes > 0),
        "{label}: recorded IPC bytes must be a real count, not zero: {row}"
    );
    if daemon_calls > 0 {
        assert!(row["daemon_rtt_us"].as_u64().is_some(), "{label}: {row}");
        assert!(
            ipc_bytes.as_u64().is_some_and(|bytes| bytes > 0),
            "{label}: a daemon round trip must attribute its IPC bytes: {row}"
        );
    } else {
        assert!(row["daemon_rtt_us"].is_null(), "{label}: {row}");
    }

    let timeout = &row["timeout"];
    assert!(timeout.is_object(), "{label}");
    assert!(
        timeout["budget_ms"].is_null() || timeout["budget_ms"].as_u64().is_some(),
        "{label}"
    );
    assert!(
        timeout["timed_out"].is_null() || timeout["timed_out"].as_bool().is_some(),
        "{label}"
    );

    let disposition = &row["disposition"];
    assert!(
        matches!(
            disposition["status"].as_str(),
            Some(
                "supported"
                    | "degraded"
                    | "unavailable"
                    | "unknown"
                    | "backpressured"
                    | "accepted_for_replay"
                    | "committed"
                    | "exact_duplicate"
            )
        ),
        "{label}: {disposition}"
    );
    assert!(disposition["retryable"].as_bool().is_some(), "{label}");
    assert!(
        matches!(
            disposition["class"].as_str(),
            Some("application" | "transport" | "timeout" | "cancellation" | "unknown")
        ),
        "{label}: {disposition}"
    );
    if let Some(reason) = disposition["reason_code"].as_str() {
        assert!(reason.len() <= 64, "{label}");
        assert!(
            reason
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "{label}"
        );
    }

    let project_path = project.to_string_lossy();
    let session_id = format!("{provider}-host-fixture");
    for private in [
        project_path.as_ref(),
        session_id.as_str(),
        "Inspect the fixture.",
    ] {
        assert_json_strings_omit(row, private, &label);
    }
}

fn assert_json_strings_omit(value: &Value, private: &str, label: &str) {
    match value {
        Value::String(value) => {
            assert!(!value.contains(private), "{label} leaked {private}");
        }
        Value::Array(values) => {
            for value in values {
                assert_json_strings_omit(value, private, label);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                assert_json_strings_omit(value, private, label);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn native_provider_fixtures_persist_external_source_receipts_across_restart() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let host = TempDir::new().unwrap();
    let home = host.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    for (provider, _) in FIXTURES {
        assert_eq!(
            execute_native_provider_path(provider, &home).await.status,
            HostAdmissionStatus::Supported,
            "{provider}"
        );
    }
}

fn initialize_boundary_project(home: &Path) -> std::path::PathBuf {
    let project = home.join("host-event-project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"host-event-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(project.join("src/lib.rs"), "pub fn host_fixture() {}\n").unwrap();
    let git = git_program();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "test@tracedecay.dev"][..],
        &["config", "user.name", "TraceDecay Test"][..],
        &["add", "."][..],
        &["commit", "-q", "-m", "fixture"][..],
    ] {
        assert!(
            Command::new(&git)
                .args(args)
                .current_dir(&project)
                .status()
                .unwrap()
                .success()
        );
    }
    project
}

fn write_claude_boundary_transcript(home: &Path, project: &Path) -> std::path::PathBuf {
    let path = home.join("claude-boundary.jsonl");
    let mut record: Value = serde_json::from_str(include_str!(
        "fixtures/provider_normalization/claude/assistant_tool_use.input.json"
    ))
    .unwrap();
    record["cwd"] = project.to_string_lossy().into_owned().into();
    std::fs::write(&path, format!("{record}\n")).unwrap();
    path
}

fn materialize_host_request(
    template: &Value,
    provider: &str,
    project: &Path,
    transcript: &Path,
) -> Value {
    match template {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| materialize_host_request(value, provider, project, transcript))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        materialize_host_request(value, provider, project, transcript),
                    )
                })
                .collect(),
        ),
        Value::String(value) => match value.as_str() {
            "<PROJECT_ROOT>" => Value::String(project.to_string_lossy().into_owned()),
            "<TRANSCRIPT_PATH>" => Value::String(transcript.to_string_lossy().into_owned()),
            "<SESSION_ID>" => Value::String(format!("{provider}-host-fixture")),
            "<REDACTED_PROMPT>" => Value::String("Inspect the fixture.".to_string()),
            _ => template.clone(),
        },
        _ => template.clone(),
    }
}

fn execute_host_boundary(provider: &str, home: &Path, project: &Path, request: &Value) -> Output {
    let subcommand = match provider {
        "claude" => "hook-claude-session-start",
        "codex" => "hook-codex-session-start",
        "cursor" => "hook-cursor-session-start",
        "hermes" => "hook-hermes-terminal-receipt",
        "kiro" => "hook-kiro-prompt-submit",
        other => panic!("unexpected provider {other}"),
    };
    let mut command = tracedecay_command_with_home(home);
    command
        .arg(subcommand)
        .current_dir(project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn native host boundary");
    child
        .stdin
        .take()
        .expect("host boundary stdin")
        .write_all(request.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().expect("host boundary output")
}

fn assert_external_source_contract(
    provider: &str,
    scope: &HostAdmissionScope,
    project_id: &ProjectId,
    observation: &DurableObservationV1,
) {
    let envelope: CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observation.payload().clone()).unwrap();
    assert_eq!(envelope.provider().as_str(), provider);

    let capabilities = SourceAcquisitionCapabilitiesV1::new(
        [SourceCaptureModeV1::Poll].into_iter().collect(),
        [SourceRefetchStrategyV1::WholeRoot].into_iter().collect(),
        [SourceDeletionSemanticsV1::ExplicitOnly]
            .into_iter()
            .collect(),
    )
    .unwrap();
    let definition = SourceDefinitionV1::new(
        SourceInstanceId::new(format!("source.host-observation.{provider}")).unwrap(),
        1,
        SourceAcquisitionContractV1::new(envelope.provider().clone(), capabilities).unwrap(),
        SourceCaptureModeV1::Poll,
        SourceRefetchStrategyV1::WholeRoot,
        SourceDeletionSemanticsV1::ExplicitOnly,
        1,
    )
    .unwrap();
    let owner = match scope {
        HostAdmissionScope::Project => SourceBindingOwnerV1::Project(project_id.clone()),
        HostAdmissionScope::Profile => SourceBindingOwnerV1::Profile(
            UserProfileId::new(format!("profile.host-observation.{provider}")).unwrap(),
        ),
    };
    let privacy_domain =
        tracedecay_domain::PrivacyDomainId::new(format!("privacy.host-observation.{provider}"))
            .unwrap();
    let scope_tag = match scope {
        HostAdmissionScope::Project => "project",
        HostAdmissionScope::Profile => "profile",
    };
    let native_root_digest = canonical_sha256(&("native-root", provider, scope_tag))
        .expect("canonical native root digest");
    let native_root = LocatorDigest::new(native_root_digest.as_str()).unwrap();
    let binding = SourceBindingV1::new(
        &definition,
        owner.clone(),
        privacy_domain.clone(),
        native_root.clone(),
        1,
    )
    .unwrap();
    binding.validate_against(&definition).unwrap();
    assert_ne!(definition.definition_digest, binding.binding_digest);

    let alternate_owner = match owner {
        SourceBindingOwnerV1::Project(_) => SourceBindingOwnerV1::Profile(
            UserProfileId::new(format!("profile.host-observation.alternate.{provider}")).unwrap(),
        ),
        SourceBindingOwnerV1::Profile(_) => SourceBindingOwnerV1::Project(project_id.clone()),
    };
    let alternate_binding =
        SourceBindingV1::new(&definition, alternate_owner, privacy_domain, native_root, 1).unwrap();
    assert_ne!(binding.binding_id, alternate_binding.binding_id);

    let partition = SourcePartitionIdV1::new(canonical_sha256(&("partition", provider)).unwrap());
    let snapshot =
        SourceSnapshotIdV1::new(canonical_sha256(&("snapshot", provider, 1_u64)).unwrap());
    let object = SourceNativeObjectIdV1::new(
        canonical_sha256(&("native-object", observation.observation_id())).unwrap(),
    );
    let object_revision =
        SourceObjectRevisionV1::new(canonical_sha256(&("revision", provider, 1_u64)).unwrap());
    let retained = SourceObjectObservationV1::new(
        object.clone(),
        object_revision,
        canonical_sha256(observation).unwrap(),
        SourceContentStateV1::Live,
    )
    .unwrap();
    let binding_identity = binding.immutable_identity().unwrap();
    let partial = SourcePartitionFrontierV1::new(
        binding_identity.clone(),
        partition.clone(),
        Some(SourceCursorV1::new(
            canonical_sha256(&("cursor", provider, 1_u64)).unwrap(),
        )),
        Some(snapshot),
        Some(SourceCursorV1::new(
            canonical_sha256(&("continuation", provider, 1_u64)).unwrap(),
        )),
        SourceCoverageV1::Partial,
        1,
        None,
        canonical_sha256(&("input", provider, 1_u64)).unwrap(),
    )
    .unwrap();
    let partial_aggregate =
        SourceAggregateFrontierV1::with_updated_partition(binding_identity, None, partial).unwrap();

    assert_eq!(partial_aggregate.coverage(), SourceCoverageV1::Partial);
    assert_eq!(
        partial_aggregate
            .partition(&partition)
            .unwrap()
            .last_complete_snapshot(),
        None
    );
    assert_eq!(retained.content_state(), SourceContentStateV1::Live);
    assert_eq!(retained.native_object(), &object);
}

async fn execute_native_provider_path(provider: &str, home: &Path) -> HostAdmissionOutcome {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = ProjectId::new(format!("project.host-event.{provider}")).unwrap();
    assert!(
        Command::new(git_program())
            .arg("init")
            .arg(&project)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        tracedecay::storage::write_repository_identity_marker(&project, project_id.as_str())
            .unwrap()
    );
    tracedecay::storage::write_enrollment_marker(
        &project,
        &tracedecay::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: tracedecay::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        tmp.path().join("profile"),
        &project,
        project_id.clone(),
    )
    .await
    .unwrap();
    let facade = runtime.facade();
    let scope = match provider {
        "codex" => {
            let transcript = tmp.path().join("codex-golden-session.jsonl");
            let mut meta: Value = serde_json::from_str(include_str!(
                "fixtures/provider_normalization/codex/session_meta.input.json"
            ))
            .unwrap();
            meta["payload"]["cwd"] = project.to_string_lossy().into_owned().into();
            let message =
                include_str!("fixtures/provider_normalization/codex/agent_message.input.json");
            std::fs::write(&transcript, format!("{}\n{message}\n", meta)).unwrap();
            codex::try_admit_codex_jsonl_observations_for_project_with_admission(
                &transcript,
                &project,
                project_id.clone(),
                &facade,
                None,
            )
            .await
            .unwrap();
            HostAdmissionScope::Project
        }
        "claude" => {
            let session_id = "claude-golden-session";
            let transcript_dir = home.join(".claude/projects/host-event-fixture");
            std::fs::create_dir_all(&transcript_dir).unwrap();
            let mut record: Value = serde_json::from_str(include_str!(
                "fixtures/provider_normalization/claude/assistant_tool_use.input.json"
            ))
            .unwrap();
            record["cwd"] = tmp.path().to_string_lossy().into_owned().into();
            std::fs::write(
                transcript_dir.join(format!("{session_id}.jsonl")),
                format!("{record}\n"),
            )
            .unwrap();
            let profile_root = home.join(".tracedecay");
            std::fs::create_dir_all(&profile_root).unwrap();
            let stats = claude::ingest_user_sessions_with_admission(
                &profile_root,
                None,
                Vec::new(),
                &facade,
            )
            .await;
            assert!(stats.messages_upserted > 0, "Claude native fixture");
            HostAdmissionScope::Profile
        }
        "cursor" => {
            let transcript = tmp.path().join("cursor-golden-session.jsonl");
            let record: Value = serde_json::from_str(include_str!(
                "fixtures/provider_normalization/cursor/tool_use.input.json"
            ))
            .unwrap();
            std::fs::write(&transcript, format!("{record}\n")).unwrap();
            let event = json!({
                "session_id": "cursor-golden-session",
                "transcript_path": transcript,
                "workspace_roots": [project],
                "cwd": project,
            });
            let stats = cursor::try_ingest_cursor_transcript_event_capped_with_admission(
                &event.to_string(),
                project_id.clone(),
                &facade,
                None,
            )
            .await
            .unwrap();
            assert!(stats.bytes_consumed > 0, "Cursor native fixture");
            HostAdmissionScope::Project
        }
        "hermes" => {
            let hermes_home = tmp.path().join("hermes-home");
            write_hermes_native_fixture(&hermes_home, &project).await;
            let stats = hermes::ingest_homes_capped_with_admission(
                &[hermes_home],
                &project,
                project_id.clone(),
                &facade,
                None,
            )
            .await
            .stats;
            assert!(stats.messages_upserted > 0, "Hermes native fixture");
            HostAdmissionScope::Project
        }
        "kiro" => {
            write_kiro_native_fixture(home, &project);
            let source = tracedecay::sessions::kiro::KiroSource::with_home(home);
            assert_eq!(source.transcript_paths(&project).len(), 1, "Kiro discovery");
            let capture = tracedecay::sessions::kiro::capture_kiro_snapshot_observations(
                &facade,
                &source,
                &project,
                ObservationScopeV1::Project {
                    project_id: project_id.clone(),
                },
                None,
                &ObservationCancellation::default(),
            )
            .await
            .unwrap();
            assert!(
                capture.stats.messages_upserted > 0,
                "Kiro native fixture must admit observations"
            );
            assert!(
                !capture.deferred_by_byte_cap,
                "Kiro native fixture must not defer on the byte cap"
            );
            HostAdmissionScope::Project
        }
        other => panic!("unexpected provider {other}"),
    };

    let observations = runtime
        .replay_observations(scope, ObservationReplayRequest::new(0, 32).unwrap())
        .await
        .unwrap();
    assert!(
        !observations.is_empty(),
        "{provider} native parser must reach observation authority"
    );
    assert_external_source_contract(provider, &scope, &project_id, observations[0].observation());
    let committed = runtime
        .external_source_receipt_for_test(scope, observations[0].commit_receipt())
        .await
        .unwrap()
        .expect("sanitized host observation must reach the canonical external-source store");
    let outcome = facade.probe(provider, scope);
    drop(facade);
    drop(runtime);

    let reopened =
        HostAdmissionTestRuntimeV1::project(tmp.path().join("profile"), &project, project_id)
            .await
            .unwrap();
    assert_eq!(
        reopened
            .external_source_receipt_for_test(scope, observations[0].commit_receipt())
            .await
            .unwrap(),
        Some(committed),
        "external-source receipt and its published projection must survive runtime restart"
    );
    outcome
}

fn encode_workspace_path(path: &Path) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for byte in path.as_os_str().as_encoded_bytes() {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            output.push(TABLE[((buffer >> bits) & 0x3f) as usize] as char);
        }
    }
    if bits > 0 {
        output.push(TABLE[((buffer << (6 - bits)) & 0x3f) as usize] as char);
    }
    output.replace('/', "_")
}

fn write_kiro_native_fixture(home: &Path, project: &Path) {
    let directory = tracedecay::agents::kiro_data_dir(home)
        .join("User/globalStorage/kiro.kiroagent/workspace-sessions")
        .join(encode_workspace_path(project));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("sess-golden.json"),
        include_str!("fixtures/provider_normalization/kiro/workspace_session.input.json"),
    )
    .unwrap();
}

async fn write_hermes_native_fixture(home: &Path, project: &Path) {
    let profile = home.join("profiles/host-event");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("config.yaml"),
        format!(
            "plugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: {}\n",
            serde_json::to_string(project.to_string_lossy().as_ref()).unwrap()
        ),
    )
    .unwrap();
    let conn = rusqlite::Connection::open(profile.join("state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, source TEXT NOT NULL, model TEXT, started_at REAL NOT NULL,
            ended_at REAL, cwd TEXT, title TEXT, parent_session_id TEXT,
            input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0
         );
         CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, role TEXT NOT NULL,
            content TEXT, tool_call_id TEXT, tool_calls TEXT, tool_name TEXT,
            timestamp REAL NOT NULL, token_count INTEGER, finish_reason TEXT,
            reasoning TEXT, observed INTEGER DEFAULT 0, active INTEGER NOT NULL DEFAULT 1
         );",
    )
    .unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/provider_normalization/hermes/assistant_tool_call.input.json"
    ))
    .unwrap();
    let session_id = fixture["session_id"].as_str().unwrap();
    conn.execute(
        "INSERT INTO sessions (
            id, source, model, started_at, ended_at, cwd, title,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens
         ) VALUES (?1, 'tui', ?2, ?3, ?3, ?4, 'Host event fixture', ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            session_id,
            fixture["session_model"].as_str(),
            fixture["timestamp"].as_f64(),
            project.to_string_lossy().as_ref(),
            fixture["session_input_tokens"].as_i64(),
            fixture["session_output_tokens"].as_i64(),
            fixture["session_cache_read_tokens"].as_i64(),
            fixture["session_cache_write_tokens"].as_i64(),
            fixture["session_reasoning_tokens"].as_i64(),
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, tool_calls, timestamp, finish_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, 'tool_calls')",
        rusqlite::params![
            session_id,
            fixture["role"].as_str(),
            fixture["content"].as_str(),
            fixture["tool_calls"].to_string(),
            fixture["timestamp"].as_f64(),
        ],
    )
    .unwrap();
}

// This admission contract test constructs normalized canonical provider-tagged records.
// Unlike the fixture test above, it does not exercise native host or provider parser fixtures.
#[tokio::test]
async fn cross_provider_host_admission_commit_before_ack_and_cancel_are_idempotent() {
    for provider in HOST_ADMISSION_PROVIDERS {
        let tmp = TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let facade = runtime.facade();

        let probe = facade.probe(provider, HostAdmissionScope::Profile);
        assert_eq!(probe.status, HostAdmissionStatus::Supported, "{provider}");
        assert_eq!(
            facade
                .accept_replay(provider, HostAdmissionScope::Profile)
                .status,
            HostAdmissionStatus::AcceptedForReplay,
            "{provider}"
        );

        let cancelled = ObservationCancellation::default();
        cancelled.cancel();
        let cancelled_outcome = facade
            .capture(host_capture_request(
                provider,
                &format!("{provider}.host.cancel"),
                0,
                1,
                "cancelled host admission",
                cancelled,
            ))
            .await;
        assert_eq!(
            cancelled_outcome.status,
            HostAdmissionStatus::Backpressured,
            "{provider}: cancellation maps to bounded host outcome, got {cancelled_outcome:?}"
        );
        assert_eq!(
            cancelled_outcome.reason_code,
            Some("admission_cancelled"),
            "{provider}"
        );

        let committed = facade
            .capture(host_capture_request(
                provider,
                &format!("{provider}.host.commit"),
                0,
                1,
                "host admission payload",
                ObservationCancellation::default(),
            ))
            .await;
        assert_eq!(
            committed.status,
            HostAdmissionStatus::Committed,
            "{provider}: first capture must commit, got {committed:?}"
        );

        let duplicate = facade
            .capture(host_capture_request(
                provider,
                &format!("{provider}.host.commit"),
                0,
                1,
                "host admission payload",
                ObservationCancellation::default(),
            ))
            .await;
        assert_eq!(
            duplicate.status,
            HostAdmissionStatus::ExactDuplicate,
            "{provider}: exact retry must be ExactDuplicate, got {duplicate:?}"
        );

        drop(facade);
        drop(runtime);

        let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .unwrap();
        let facade = runtime.facade();
        let restarted = facade
            .capture(host_capture_request(
                provider,
                &format!("{provider}.host.commit"),
                0,
                1,
                "host admission payload",
                ObservationCancellation::default(),
            ))
            .await;
        assert_eq!(
            restarted.status,
            HostAdmissionStatus::ExactDuplicate,
            "{provider}: restart commit-before-ack retry must be ExactDuplicate, got {restarted:?}"
        );
        assert_eq!(
            facade
                .accept_replay(provider, HostAdmissionScope::Profile)
                .status,
            HostAdmissionStatus::AcceptedForReplay,
            "{provider}"
        );
    }
}

#[tokio::test]
async fn canonical_and_linked_worktree_events_share_retained_project_authority() {
    let project_tmp = TempDir::new().unwrap();
    let profile_tmp = TempDir::new().unwrap();
    assert!(
        Command::new(git_program())
            .arg("init")
            .arg(project_tmp.path())
            .status()
            .unwrap()
            .success()
    );
    let project_id = ProjectId::new("project.canonical-worktree").unwrap();
    assert!(
        tracedecay::storage::write_repository_identity_marker(
            project_tmp.path(),
            project_id.as_str()
        )
        .unwrap()
    );
    tracedecay::storage::write_enrollment_marker(
        project_tmp.path(),
        &tracedecay::storage::EnrollmentMarker {
            project_id: project_id.as_str().to_owned(),
            storage_mode: tracedecay::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        profile_tmp.path().join("profile"),
        project_tmp.path(),
        project_id.clone(),
    )
    .await
    .unwrap();
    let facade = runtime.facade();
    let scope = ObservationScopeV1::Project { project_id };

    for (session_id, record_id) in [
        ("session.canonical-checkout", "codex.canonical.event"),
        ("session.linked-worktree", "codex.linked.event"),
    ] {
        let outcome = facade
            .capture(host_capture_request_in_scope(
                "codex",
                session_id,
                record_id,
                ObservationSourceRangeV1::new(0, 1).unwrap(),
                "project-scoped payload",
                scope.clone(),
                ObservationCancellation::default(),
            ))
            .await;
        assert_eq!(
            outcome.status,
            HostAdmissionStatus::AcceptedForReplay,
            "{session_id}"
        );
        assert_eq!(
            outcome.reason_code,
            Some("external_source_projection_pending"),
            "{session_id}"
        );
    }
    let repeated_source_request = || {
        host_capture_request_in_scope_with_expected_cursor(
            "codex",
            "session.canonical-checkout",
            "codex.canonical.follow-up",
            ObservationSourceRangeV1::new(1, 2).unwrap(),
            "project-scoped follow-up",
            scope.clone(),
            Some(
                ObservationSourceCursorV1::for_ordering(
                    ObservationSourceIdentityV1::for_provider(
                        ProviderId::new("codex").unwrap(),
                        SessionId::new("session.canonical-checkout").unwrap(),
                    )
                    .unwrap(),
                    scope.clone(),
                    ObservationSourceGenerationV1::new(9).unwrap(),
                    ObservationOrderingDomainV1::SqliteRowId,
                    1,
                )
                .unwrap(),
            ),
            ObservationCancellation::default(),
        )
    };
    let repeated_source_outcome = facade.capture(repeated_source_request()).await;
    assert_eq!(
        repeated_source_outcome.status,
        HostAdmissionStatus::AcceptedForReplay,
        "{repeated_source_outcome:?}"
    );
    assert_eq!(
        repeated_source_outcome.reason_code,
        Some("external_source_projection_pending")
    );

    let project_rows = runtime
        .replay_observations(
            HostAdmissionScope::Project,
            ObservationReplayRequest::new(0, 10).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(project_rows.len(), 3);
    let first_source_commit = runtime
        .external_source_receipt_for_test(
            HostAdmissionScope::Project,
            project_rows[0].commit_receipt(),
        )
        .await
        .unwrap()
        .unwrap();
    let advanced_source_commit = runtime
        .external_source_receipt_for_test(
            HostAdmissionScope::Project,
            project_rows[2].commit_receipt(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first_source_commit.source_frontier().binding(),
        advanced_source_commit.source_frontier().binding()
    );
    assert_eq!(
        first_source_commit
            .source_frontier()
            .partitions()
            .values()
            .next()
            .unwrap()
            .sequence(),
        1
    );
    assert_eq!(
        advanced_source_commit
            .source_frontier()
            .partitions()
            .values()
            .next()
            .unwrap()
            .sequence(),
        2
    );
    let exact_replay = facade.capture(repeated_source_request()).await;
    assert_eq!(exact_replay.status, HostAdmissionStatus::ExactDuplicate);
    assert_eq!(
        exact_replay.reason_code,
        Some("external_source_projection_pending")
    );
    assert_eq!(
        runtime
            .external_source_receipt_for_test(
                HostAdmissionScope::Project,
                project_rows[2].commit_receipt(),
            )
            .await
            .unwrap(),
        Some(advanced_source_commit),
        "exact replay must preserve the committed frontier and projection receipt"
    );

    let mismatched = facade
        .capture(host_capture_request_in_scope(
            "codex",
            "session.wrong-project",
            "codex.wrong-project.event",
            ObservationSourceRangeV1::new(0, 1).unwrap(),
            "must not persist",
            ObservationScopeV1::Project {
                project_id: ProjectId::new("project.other").unwrap(),
            },
            ObservationCancellation::default(),
        ))
        .await;
    assert_eq!(mismatched.status, HostAdmissionStatus::Unavailable);
    assert!(!mismatched.retryable);
    assert_eq!(mismatched.reason_code, Some("project_authority_mismatch"));
    assert_eq!(
        runtime
            .replay_observations(
                HostAdmissionScope::Project,
                ObservationReplayRequest::new(0, 10).unwrap(),
            )
            .await
            .unwrap(),
        project_rows,
        "mismatched project identity must write nothing"
    );

    assert!(
        runtime
            .replay_observations(
                HostAdmissionScope::Profile,
                ObservationReplayRequest::new(0, 10).unwrap(),
            )
            .await
            .unwrap()
            .is_empty(),
        "project events must not fall back to profile authority"
    );
}

fn host_capture_request(
    provider: &str,
    record_id: &str,
    start: u64,
    end: u64,
    text: &str,
    cancellation: ObservationCancellation,
) -> CaptureObservationRequest {
    host_capture_request_in_scope(
        provider,
        &format!("session.host-fixture.{provider}"),
        record_id,
        ObservationSourceRangeV1::new(start, end).unwrap(),
        text,
        ObservationScopeV1::Profile,
        cancellation,
    )
}

fn host_capture_request_in_scope(
    provider: &str,
    session_id: &str,
    record_id: &str,
    range: ObservationSourceRangeV1,
    text: &str,
    scope: ObservationScopeV1,
    cancellation: ObservationCancellation,
) -> CaptureObservationRequest {
    host_capture_request_in_scope_with_expected_cursor(
        provider,
        session_id,
        record_id,
        range,
        text,
        scope,
        None,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
fn host_capture_request_in_scope_with_expected_cursor(
    provider: &str,
    session_id: &str,
    record_id: &str,
    range: ObservationSourceRangeV1,
    text: &str,
    scope: ObservationScopeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    cancellation: ObservationCancellation,
) -> CaptureObservationRequest {
    let record = json!({ "text": text });
    let encoded = serde_json::to_vec(&record).unwrap();
    let ordering_domain = ObservationOrderingDomainV1::SqliteRowId;
    let provider_owned = provider.to_owned();
    let record_owned = record_id.to_owned();
    let session_id = session_id.to_owned();
    let canonical_session_id = session_id.clone();
    let parsed =
        parse_normalized_observation_record_v1(&encoded, range, ordering_domain, move |native| {
            CanonicalObservationEnvelopeV1::new(
                ProviderId::new(&provider_owned).unwrap(),
                "message",
                ObservationId::new(record_owned.clone()).unwrap(),
                CanonicalObservationRelationsV1::new(
                    SessionId::new(canonical_session_id.clone()).unwrap(),
                )
                .with_message_id(ObservationId::new(record_owned.clone()).unwrap()),
                vec![CanonicalObservationFactV1::Message {
                    role: CanonicalMessageRoleV1::Assistant,
                    content: native,
                    model: None,
                    timestamp: None,
                }],
                CanonicalObservationEvidenceV1::new(ordering_domain, range),
            )
            .map_err(|_| ClaudeRecordParseErrorV1::NormalizationFailed)
        })
        .unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider).unwrap(),
        SessionId::new(session_id).unwrap(),
    )
    .unwrap();
    CaptureObservationRequest::new(
        parsed,
        ObservationIdentityMaterialV1::for_native_record(
            source,
            scope,
            ObservationSourceGenerationV1::new(9).unwrap(),
            range,
            ordering_domain,
            ObservationId::new(record_id).unwrap(),
        )
        .unwrap(),
        expected_cursor,
        RetentionClass::new("retention.host-fixture-test").unwrap(),
        cancellation,
    )
    .unwrap()
}

fn assert_legal_host_response(provider: &str, state: &str, expected: &Value, output: Output) {
    assert_eq!(
        output.status.code().unwrap_or(-1),
        expected["exit_code"].as_i64().unwrap() as i32,
        "{provider}/{state} exit code"
    );
    let stderr = String::from_utf8(output.stderr).expect("host stderr is UTF-8");
    assert_eq!(
        stderr.trim_end(),
        expected["stderr"].as_str().unwrap(),
        "{provider}/{state} stderr"
    );
    let mut stdout = String::from_utf8(output.stdout).expect("host stdout is UTF-8");
    stdout.truncate(stdout.trim_end().len());
    if matches!(provider, "claude" | "codex" | "cursor") {
        let mut document: Value = serde_json::from_str(&stdout).unwrap_or_else(|error| {
            panic!("{provider}/{state} emitted illegal JSON stdout: {error}: {stdout:?}")
        });
        let context = if provider == "cursor" {
            &mut document["additional_context"]
        } else {
            &mut document["hookSpecificOutput"]["additionalContext"]
        };
        assert!(context.is_string(), "{provider}/{state} context response");
        *context = Value::String("<REDACTED_CONTEXT>".to_string());
        if provider == "cursor" {
            let project_root = &mut document["env"]["TRACEDECAY_PROJECT_ROOT"];
            assert!(
                project_root.is_string(),
                "{provider}/{state} project-root response"
            );
            *project_root = Value::String("<PROJECT_ROOT>".to_string());
        }
        let expected: Value = serde_json::from_str(expected["stdout"].as_str().unwrap())
            .expect("fixture response stdout is legal JSON");
        assert_eq!(document, expected, "{provider}/{state} stdout");
    } else {
        assert_eq!(
            stdout,
            expected["stdout"].as_str().unwrap(),
            "{provider}/{state} stdout"
        );
    }
}

fn assert_redacted(provider: &str, state: &str, case: &Value) {
    let encoded = serde_json::to_string(case).unwrap();
    for forbidden in [
        "/home/",
        "C:\\\\Users\\",
        "api_key",
        "access_token",
        "secret",
        "hostname",
    ] {
        assert!(
            !encoded
                .to_ascii_lowercase()
                .contains(&forbidden.to_ascii_lowercase()),
            "{provider}/{state} contains forbidden data: {forbidden}"
        );
    }
}
