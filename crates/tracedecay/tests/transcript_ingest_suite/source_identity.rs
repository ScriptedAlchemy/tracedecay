//! Persisted admission evidence for native ingest source identity (#927).
//!
//! Each provider is captured separately so a Cline path-key miss is not
//! collapsed into a Cursor project-filter miss or a Codex v2 cursor miss.

use tempfile::TempDir;
use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::{
    ClineTranscriptStream, ObservationSourceIdentityV1, ProviderId, SessionId,
};
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::cline_like::ClineLikeSource;
use tracedecay_sessions::runtime::cursor::ingest_cursor_transcript_event;

use crate::cline_like::{parse_offset_for_task_history, vscode_storage_root, write_task};
use crate::codex::write_codex_rollout_with_structured_events;
use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    ingest_global_sources_for_provider, mark_test_project, observation_source_cursor,
    observation_source_cursor_for_key, open_project_session_db, try_ingest_source,
};
use crate::support::{assert_metadata_path_eq, init_git_repo, setup};

#[tokio::test]
async fn cline_parse_offset_lookup_uses_path_identity_not_display_text() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let api = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-identity",
    );
    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        3
    );
    let committed = parse_offset_for_task_history(&db, &project, &api)
        .await
        .expect("Cline API parse offset must be admitted");

    assert_eq!(
        db.get_parse_offset(api.to_string_lossy().as_ref()).await,
        Some(committed),
        "Cline cursor lookup must use the admitted path identity"
    );
    #[cfg(windows)]
    {
        let slash_flipped = api.to_string_lossy().replace('\\', "/");
        assert_eq!(db.get_parse_offset(&slash_flipped).await, Some(committed));
    }
    #[cfg(not(windows))]
    assert!(
        db.get_parse_offset(&format!(r"{}\literal-backslash", api.to_string_lossy()))
            .await
            .is_none(),
        "a Unix literal backslash must not alias a path separator"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cline_registered_ingest_keeps_api_and_ui_cursors_on_their_own_sources() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let session_id = "cline-source-split";
    write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        session_id,
    );

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cline)).await;

    let api_cursor = observation_source_cursor(&db, "cline", session_id, &project)
        .await
        .expect("API stream cursor");
    let ui_key = "ui_messages";
    let ui_cursor = observation_source_cursor_for_key(&db, "cline", session_id, ui_key)
        .await
        .expect("UI stream cursor");
    assert_ne!(
        api_cursor.source(),
        ui_cursor.source(),
        "Cline API and UI streams must stay independently ordered"
    );
    let provider = ProviderId::new("cline").unwrap();
    let session = SessionId::new(session_id).unwrap();
    let expected_api = ClineTranscriptStream::ApiHistory
        .source_identity(provider.clone(), session.clone())
        .unwrap();
    let expected_ui = ClineTranscriptStream::UiMessages
        .source_identity(provider, session)
        .unwrap();
    assert_eq!(api_cursor.source(), &expected_api);
    assert_eq!(ui_cursor.source(), &expected_ui);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn codex_registered_ingest_uses_the_host_v2_source_identity() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let session_id = "codex-source-v2";
    write_codex_rollout_with_structured_events(&home, &project, session_id);

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Codex)).await;

    let expected =
        tracedecay_sessions::runtime::codex::codex_observation_source_v2(session_id).unwrap();
    let cursor = db
        .runtime()
        .project_observation_source_cursor_for_test(&expected)
        .await
        .unwrap()
        .expect("Codex v2 source cursor");
    assert_eq!(cursor.source(), &expected);

    let legacy = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("codex").unwrap(),
        SessionId::new(session_id).unwrap(),
    )
    .unwrap();
    assert_eq!(
        db.runtime()
            .project_observation_source_cursor_for_test(&legacy)
            .await
            .unwrap(),
        None,
        "the pre-v2 provider/session identity must remain unused"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cursor_search_uses_path_identity_for_the_selected_project() {
    let tmp = TempDir::new().unwrap();
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let profile = tmp.path().join("profile");
    let _env_guards = [
        EnvVarGuard::set("TRACEDECAY_DATA_DIR", &profile),
        EnvVarGuard::set("HOME", tmp.path().join("home")),
        EnvVarGuard::set("USERPROFILE", tmp.path().join("home")),
    ];
    let project = tmp.path().join("project");
    crate::support::init_project_at(&project);
    init_git_repo(&project);
    let project_id = mark_test_project(&project);

    let transcript = tmp.path().join("cursor-session.jsonl");
    std::fs::write(
        &transcript,
        r#"{"role":"user","message":{"content":[{"type":"text","text":"Please check billing ingestion from Cursor transcripts."}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"The billing ingestion plan is ready."}]}}
"#,
    )
    .unwrap();

    let runtime = HostAdmissionTestRuntimeV1::project(&profile, &project, project_id.clone())
        .await
        .unwrap();
    let event = serde_json::json!({
        "session_id": "cursor-session",
        "transcript_path": transcript,
        "cwd": project,
    });
    let stats =
        ingest_cursor_transcript_event(&event.to_string(), &runtime.facade(), project_id).await;
    assert_eq!(stats.messages_upserted, 2);

    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("cursor").unwrap(),
        SessionId::new("cursor-session").unwrap(),
    )
    .unwrap();
    let cursor = runtime
        .project_observation_source_cursor_for_test(&source)
        .await
        .unwrap()
        .expect("Cursor admission must write a cursor under the session source");
    assert_eq!(cursor.source(), &source);

    let results = runtime
        .search_project_session_messages_for_test(
            "cursor",
            Some(project.to_string_lossy().as_ref()),
            "billing ingestion",
            10,
        )
        .await
        .unwrap();
    assert_eq!(
        results.len(),
        2,
        "Cursor projection must stay searchable under the selected project's path identity"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(results[0].session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["cursor_session_cwd"], &project);
}
