use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
    HostAdmissionStatus,
};
use tracedecay::application::observation::{CaptureObservationRequest, ObservationCancellation};
use tracedecay::privacy::{ClaudeRecordParseErrorV1, parse_normalized_observation_record_v1};
use tracedecay::sessions::source::TranscriptSource;
use tracedecay::sessions::{
    SessionProvider, claude, codex, cursor, hermes, ingest_global_sources_for_provider,
};
use tracedecay::store::GlobalDbObservationStore;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::{ObservationReplayRequest, ObservationStore};

mod common;

use common::{
    EnvVarGuard, GLOBAL_DB_ENV_LOCK, git_program, open_lcm_db, spawn_tracedecay_daemon,
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
    let _home = EnvVarGuard::set("HOME", &home);
    let _userprofile = EnvVarGuard::set("USERPROFILE", &home);
    let boundary_project = initialize_boundary_project(&home);
    let _daemon = spawn_tracedecay_daemon(&home);
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
            let output = execute_host_boundary(provider, &home, &boundary_project, &request);
            assert_legal_host_response(provider, state, &case["response"], output);
        }

        states.sort_unstable();
        assert_eq!(
            states,
            ["degraded", "supported", "unavailable", "unknown"],
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

async fn execute_native_provider_path(provider: &str, home: &Path) -> HostAdmissionOutcome {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let project_id = ProjectId::new(format!("project.host-event.{provider}")).unwrap();
    let db = open_lcm_db(&tmp).await;
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
            codex::try_admit_codex_jsonl_observations_for_project(
                &transcript,
                &db,
                &project,
                project_id.clone(),
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
            let stats = claude::ingest_user_sessions(&db, &profile_root, None, Vec::new()).await;
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
            let stats = cursor::try_ingest_cursor_transcript_event(
                &event.to_string(),
                &db,
                project_id.clone(),
            )
            .await
            .unwrap();
            assert!(stats.bytes_consumed > 0, "Cursor native fixture");
            HostAdmissionScope::Project
        }
        "hermes" => {
            let hermes_home = tmp.path().join("hermes-home");
            write_hermes_native_fixture(&hermes_home, &project).await;
            let stats =
                hermes::ingest_homes(&db, &[hermes_home], &project, project_id.clone()).await;
            assert!(stats.messages_upserted > 0, "Hermes native fixture");
            HostAdmissionScope::Project
        }
        "kiro" => {
            assert!(
                std::process::Command::new(git_program())
                    .arg("init")
                    .arg(&project)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(
                tracedecay::storage::write_repository_identity_marker(
                    &project,
                    project_id.as_str()
                )
                .unwrap()
            );
            write_kiro_native_fixture(home, &project);
            let source = tracedecay::sessions::kiro::KiroSource::with_home(home);
            assert_eq!(source.transcript_paths(&project).len(), 1, "Kiro discovery");
            ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Kiro)).await;
            HostAdmissionScope::Project
        }
        other => panic!("unexpected provider {other}"),
    };

    let store = GlobalDbObservationStore::new(&db);
    assert!(
        !store
            .replay_observations(ObservationReplayRequest::new(0, 32).unwrap())
            .await
            .unwrap()
            .is_empty(),
        "{provider} native parser must reach observation authority"
    );
    let authorities = match scope {
        HostAdmissionScope::Project => HostAdmissionAuthorities::for_project(&db, project_id),
        HostAdmissionScope::Profile => HostAdmissionAuthorities::for_profile(&db),
    };
    HostAdmissionFacade::new(authorities).probe(provider, scope)
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
    let database = libsql::Builder::new_local(profile.join("state.db"))
        .build()
        .await
        .unwrap();
    let conn = database.connect().unwrap();
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
    .await
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
        libsql::params![
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
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, role, content, tool_calls, timestamp, finish_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, 'tool_calls')",
        libsql::params![
            session_id,
            fixture["role"].as_str(),
            fixture["content"].as_str(),
            fixture["tool_calls"].to_string(),
            fixture["timestamp"].as_f64(),
        ],
    )
    .await
    .unwrap();
}

// This admission contract test constructs normalized canonical provider-tagged records.
// Unlike the fixture test above, it does not exercise native host or provider parser fixtures.
#[tokio::test]
async fn cross_provider_host_admission_commit_before_ack_and_cancel_are_idempotent() {
    for provider in HOST_ADMISSION_PROVIDERS {
        let tmp = TempDir::new().unwrap();
        let db = open_lcm_db(&tmp).await;
        let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(&db));

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
        drop(db);

        let db = open_lcm_db(&tmp).await;
        let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(&db));
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
    let project_db = open_lcm_db(&project_tmp).await;
    let profile_db = open_lcm_db(&profile_tmp).await;
    let project_id = ProjectId::new("project.canonical-worktree").unwrap();
    let facade = HostAdmissionFacade::new(
        HostAdmissionAuthorities::for_project(&project_db, project_id.clone())
            .with_profile(&profile_db),
    );
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
            HostAdmissionStatus::Committed,
            "{session_id}"
        );
    }

    let project_store = GlobalDbObservationStore::new(&project_db);
    let project_rows = project_store
        .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
        .await
        .unwrap();
    assert_eq!(project_rows.len(), 2);

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
        project_store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
            .await
            .unwrap(),
        project_rows,
        "mismatched project identity must write nothing"
    );

    let profile_store = GlobalDbObservationStore::new(&profile_db);
    assert!(
        profile_store
            .replay_observations(ObservationReplayRequest::new(0, 10).unwrap())
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
        None,
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
