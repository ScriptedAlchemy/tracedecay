use tempfile::TempDir;
#[cfg(not(windows))]
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::global_db::ParseOffset;
use tracedecay::sessions::SessionProvider;
use tracedecay::sessions::cline_like::ClineLikeSource;
use tracedecay::sessions::source::{StoredCursor, TranscriptIngestError, TranscriptSource};
#[cfg(not(windows))]
use tracedecay_store::ObservationProjectionStore;

use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
#[cfg(not(windows))]
use crate::restart_atomicity::durable_table_count;
use crate::restart_atomicity::{
    ProjectSessionTestRuntime, assert_secret_absent_from_observation_sinks,
    ingest_global_sources_for_provider, mark_test_project, observation_source_cursor,
    open_project_session_db, set_projection_failure, try_ingest_source,
};
use crate::support::{
    assert_metadata_path_eq, create_git_repo_with_linked_worktree, init_git_repo, setup,
};

pub(super) fn vscode_storage_root(
    home: &std::path::Path,
    extension_id: &str,
) -> std::path::PathBuf {
    tracedecay::agents::vscode_data_dir(home)
        .join("User/globalStorage")
        .join(extension_id)
        .join("tasks")
}

async fn parse_offset_for_path(
    db: &ProjectSessionTestRuntime,
    path: &std::path::Path,
) -> Option<ParseOffset> {
    let path = path.to_string_lossy();
    if let Some(offset) = db.get_parse_offset(path.as_ref()).await {
        return Some(offset);
    }

    #[cfg(windows)]
    {
        let alternate = if path.contains('/') {
            path.replace('/', "\\")
        } else {
            path.replace('\\', "/")
        };
        if alternate != path {
            return db.get_parse_offset(&alternate).await;
        }
    }

    None
}

pub(super) async fn parse_offset_for_task_history(
    db: &ProjectSessionTestRuntime,
    _project: &std::path::Path,
    path: &std::path::Path,
) -> Option<ParseOffset> {
    if let Some(offset) = parse_offset_for_path(db, path).await {
        return Some(offset);
    }

    let task_dir = path.parent()?.file_name()?.to_string_lossy();
    let file_name = path.file_name()?.to_string_lossy();
    let expected_suffix = format!("{task_dir}/{file_name}");
    db.runtime()
        .project_parse_offset_by_suffix_for_test(&expected_suffix)
        .await
        .ok()
        .flatten()
}

pub(super) fn write_task(
    root: &std::path::Path,
    project: &std::path::Path,
    task_id: &str,
) -> std::path::PathBuf {
    write_task_with_api_filename(root, project, task_id, "api_conversation_history.json")
}

/// Write a Cline-family task using checked-in golden fixtures under
/// `tests/fixtures/transcript_golden/cline_like/`.
pub(super) fn write_task_with_api_filename(
    root: &std::path::Path,
    project: &std::path::Path,
    task_id: &str,
    api_filename: &str,
) -> std::path::PathBuf {
    let dir = root.join(task_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut metadata: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/transcript_golden/cline_like/input/task_metadata.json"
    ))
    .unwrap();
    metadata["workspacePath"] = serde_json::Value::String(project.to_string_lossy().into_owned());
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let api = dir.join(api_filename);
    let fixture_name = match api_filename {
        "api_messages.json" => api_filename,
        _ => "api_conversation_history.json",
    };
    let history = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/transcript_golden/cline_like/input")
            .join(fixture_name),
    )
    .unwrap();
    std::fs::write(&api, history).unwrap();
    std::fs::write(
        dir.join("ui_messages.json"),
        include_str!("../fixtures/transcript_golden/cline_like/input/ui_messages.json"),
    )
    .unwrap();
    api
}

async fn assert_provider_ingests(
    provider: &str,
    source: ClineLikeSource,
    db: &ProjectSessionTestRuntime,
    ingest_project: &std::path::Path,
    transcript_project: &std::path::Path,
) {
    let stats = try_ingest_source(db, &source, ingest_project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 3);

    let results = db
        .search_session_messages(
            provider,
            Some(ingest_project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .any(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
    );
    // The `ts` fields land as per-message timestamps.
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_800_000_000))
    );
    assert!(
        results
            .iter()
            .any(|hit| hit.message.timestamp == Some(1_800_000_010))
    );
    let assistant = results
        .iter()
        .find(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
        .expect("assistant tool-use message should be searchable");
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["cline_like_task_cwd"], transcript_project);
    assert_metadata_path_eq(&metadata["cline_like_task_worktree"], transcript_project);
    assert_eq!(
        metadata["cline_like_task_location_provenance"].as_str(),
        Some("task_metadata")
    );
    assert!(metadata.get("usage").is_none());
    let usage_hits = db
        .search_session_messages(provider, None, "input_tokens", 10)
        .await;
    assert_eq!(usage_hits.len(), 1);
    assert_eq!(usage_hits[0].message.kind.as_deref(), Some("usage"));
    let usage_metadata: serde_json::Value = serde_json::from_str(
        usage_hits[0]
            .message
            .metadata_json
            .as_deref()
            .expect("usage metadata"),
    )
    .unwrap();
    assert_eq!(usage_metadata["usage"]["input_tokens"], 1200);
    assert_eq!(usage_metadata["usage"]["output_tokens"], 350);
    assert_eq!(usage_metadata["usage"]["cache_read_input_tokens"], 8000);
    assert_eq!(usage_metadata["usage"]["cache_creation_input_tokens"], 500);
    assert_eq!(usage_metadata["correlation"], "unavailable");
    let expected_content = serde_json::json!([
        {"type": "text", "text": "The billing pipeline regression is fixed."},
        {"type": "tool_use", "name": "read_file"}
    ]);
    let raw = db
        .lcm_load_raw_message(provider, &assistant.message.message_id)
        .await
        .expect("structured Cline-like content should be in raw LCM storage");
    assert_eq!(
        raw.content,
        serde_json::to_string(&expected_content).unwrap()
    );
    let session = db
        .get_session(provider, &assistant.message.session_id)
        .await
        .expect("Cline-like session should be stored");
    let session_metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["cline_like_task_cwd"], transcript_project);
    assert_metadata_path_eq(
        &session_metadata["cline_like_task_worktree"],
        transcript_project,
    );
    assert_eq!(
        session_metadata["cline_like_task_location_provenance"].as_str(),
        Some("task_metadata")
    );

    // ContentHash: unchanged full-rewrite file is a no-op.
    assert_eq!(
        try_ingest_source(db, &source, ingest_project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn cline_task_history_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &linked_worktree,
        "cline-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_provider_ingests(
        "cline",
        ClineLikeSource::cline_with_home(&home),
        &db,
        &project,
        &linked_worktree,
    )
    .await;
}

#[tokio::test]
async fn roo_code_task_history_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_task(
        &vscode_storage_root(&home, "rooveterinaryinc.roo-cline"),
        &linked_worktree,
        "roo-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_provider_ingests(
        "roo-code",
        ClineLikeSource::roo_code_with_home(&home),
        &db,
        &project,
        &linked_worktree,
    )
    .await;
}

#[tokio::test]
async fn kilo_task_history_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_task(
        &vscode_storage_root(&home, "kilocode.kilo-code"),
        &linked_worktree,
        "kilo-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_provider_ingests(
        "kilo",
        ClineLikeSource::kilo_with_home(&home),
        &db,
        &project,
        &linked_worktree,
    )
    .await;
}

#[tokio::test]
async fn cline_ui_messages_only_change_triggers_usage_refresh() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let api = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-ui-usage",
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
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );

    let ui_path = api.parent().unwrap().join("ui_messages.json");
    let committed = parse_offset_for_task_history(&db, &project, &api)
        .await
        .expect("initial task generation cursor");
    std::fs::write(&ui_path, r#"[{"type":"say","say":"api_req_started""#).unwrap();
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        0
    );
    assert_eq!(
        parse_offset_for_task_history(&db, &project, &api).await,
        Some(committed),
        "incomplete companion snapshot must not replace the committed generation"
    );

    std::fs::write(
        &ui_path,
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "type": "say",
                "say": "api_req_started",
                "ts": 1_800_000_005_i64,
                "text": serde_json::json!({
                    "tokensIn": 2200,
                    "tokensOut": 450,
                    "cacheReads": 9000,
                    "cacheWrites": 600
                }).to_string()
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        3
    );
    let usage = db
        .search_session_messages("cline", None, "input_tokens", 10)
        .await;
    assert!(usage.iter().any(|hit| {
        let metadata: serde_json::Value = serde_json::from_str(
            hit.message
                .metadata_json
                .as_deref()
                .expect("usage metadata"),
        )
        .unwrap();
        metadata["usage"]["input_tokens"] == 2200
    }));
}

#[tokio::test]
async fn cline_usage_index_skips_unemitted_assistant_entries() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-skipped-assistant");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({
            "task": "Usage indexing",
            "workspacePath": project
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("api_conversation_history.json"),
        serde_json::json!([
            {"role": "assistant", "content": ""},
            {"role": "assistant", "content": "Emitted assistant usage target"}
        ])
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("ui_messages.json"),
        serde_json::json!([
            {
                "type": "say",
                "say": "api_req_started",
                "text": serde_json::json!({"tokensIn": 777}).to_string()
            }
        ])
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        2
    );
    let hits = db
        .search_session_messages("cline", None, "target", 10)
        .await;
    assert_eq!(hits.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(hits[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert!(metadata.get("usage").is_none());
    let usage = db
        .search_session_messages("cline", None, "input_tokens", 10)
        .await;
    assert_eq!(
        usage.len(),
        1,
        "usage hits: {:?}",
        usage
            .iter()
            .map(|hit| (&hit.message.message_id, &hit.message.text))
            .collect::<Vec<_>>()
    );
    let metadata: serde_json::Value =
        serde_json::from_str(usage[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["usage"]["input_tokens"], 777);
}

#[tokio::test]
async fn cline_unversioned_timestamp_free_identity_survives_insertion_and_reorder() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let api = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-stable-identity",
    );
    std::fs::write(
        &api,
        serde_json::json!([
            {"role": "user", "content": "Investigate the billing pipeline regression"},
            {
                "role": "assistant",
                "model": "claude-sonnet-4.6",
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "read_file"}
                ]
            },
            {
                "role": "assistant",
                "model": "claude-sonnet-4.6",
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "read_file"}
                ]
            }
        ])
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    let before = db
        .search_session_messages("cline", None, "regression is fixed", 10)
        .await;
    let mut assistant_ids = before
        .iter()
        .filter(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
        .map(|hit| hit.message.message_id.clone())
        .collect::<Vec<_>>();
    assistant_ids.sort();
    assistant_ids.dedup();
    assert_eq!(
        assistant_ids.len(),
        2,
        "identical messages need distinct IDs"
    );

    std::fs::write(
        &api,
        serde_json::json!([
            {
                "role": "assistant",
                "model": "claude-sonnet-4.6",
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "read_file"}
                ]
            },
            {"role": "user", "content": "New earlier context"},
            {"role": "user", "content": "Investigate the billing pipeline regression"},
            {
                "role": "assistant",
                "model": "claude-sonnet-4.6",
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "read_file"}
                ]
            }
        ])
        .to_string(),
    )
    .unwrap();
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let after = db
        .search_session_messages("cline", None, "regression is fixed", 10)
        .await;
    let mut reordered_ids = after
        .iter()
        .filter(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
        .map(|hit| hit.message.message_id.clone())
        .collect::<Vec<_>>();
    reordered_ids.sort();
    reordered_ids.dedup();
    assert_eq!(reordered_ids, assistant_ids);
}

#[test]
fn cline_complete_malformed_snapshot_is_typed_non_durable() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-malformed-json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({"workspacePath": project}).to_string(),
    )
    .unwrap();
    let api = dir.join("api_conversation_history.json");
    std::fs::write(&api, "{not-json]").unwrap();

    let source = ClineLikeSource::cline_with_home(&home);
    assert!(matches!(
        source.try_parse_new(&api, StoredCursor::default(), &project, None),
        Err(TranscriptIngestError::NonDurableRecord {
            provider: "cline",
            reason: "malformed snapshot JSON",
            ..
        })
    ));
}

#[tokio::test]
async fn cline_incomplete_snapshot_does_not_advance_content_hash_cursor() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-invalid-json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({"workspacePath": project}).to_string(),
    )
    .unwrap();
    let api = dir.join("api_conversation_history.json");
    std::fs::write(&api, r#"[{"role":"user","content":"still writing""#).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 0);

    assert!(
        parse_offset_for_task_history(&db, &project, &api)
            .await
            .is_none(),
        "incomplete changed task history must not advance its cursor"
    );

    std::fs::write(
        &api,
        serde_json::json!([{"role":"user","content":"completed later"}]).to_string(),
    )
    .unwrap();
    assert_eq!(
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
            .messages_upserted,
        1
    );
}

#[tokio::test]
async fn cline_missing_metadata_waits_for_later_metadata_before_advancing_cursor() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-missing-metadata");
    std::fs::create_dir_all(&dir).unwrap();
    let api = dir.join("api_conversation_history.json");
    std::fs::write(
        &api,
        serde_json::json!([
            {"role": "user", "content": "Metadata missing prompt"}
        ])
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 0);

    assert!(
        parse_offset_for_task_history(&db, &project, &api)
            .await
            .is_none(),
        "metadata-less task should not advance its cursor"
    );

    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({
            "task": "Metadata arrived later",
            "workspacePath": project
        })
        .to_string(),
    )
    .unwrap();

    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 1);

    let offset = parse_offset_for_task_history(&db, &project, &api)
        .await
        .expect("task should advance once metadata is available");
    assert_ne!(offset.byte_offset, 0);
}

#[tokio::test]
async fn cline_like_task_for_other_project_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let other = tmp.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &other,
        "other-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    let stats = try_ingest_source(
        &db,
        &ClineLikeSource::cline_with_home(&home),
        &project,
        None,
    )
    .await
    .unwrap();
    assert_eq!(stats.messages_upserted, 0);
}

#[tokio::test]
async fn cline_like_user_scope_includes_only_unregistered_tasks() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let projectless = tmp.path().join("general-chat");
    std::fs::create_dir_all(&projectless).unwrap();
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    write_task(&root, &project, "registered-task");
    write_task(&root, &projectless, "user-task");
    let mixed = write_task(&root, &project, "mixed-task");
    std::fs::write(
        mixed.parent().unwrap().join("task_metadata.json"),
        serde_json::json!({
            "workspacePath": project,
            "otherDirectory": projectless
        })
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home).for_user_scope(vec![project.clone()]);
    let stats = try_ingest_source(&db, &source, tmp.path(), None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 3);
    assert!(db.get_session("cline", "registered-task").await.is_none());
    assert!(db.get_session("cline", "mixed-task").await.is_none());
    let session = db.get_session("cline", "user-task").await.unwrap();
    assert_eq!(session.project_key, "user");
    assert_eq!(session.project_path, "user");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cline_family_secrets_are_sanitized_before_observation_and_projection() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (provider, extension_id, selected_provider) in [
        ("cline", "saoudrizwan.claude-dev", SessionProvider::Cline),
        (
            "roo-code",
            "rooveterinaryinc.roo-cline",
            SessionProvider::RooCode,
        ),
        ("kilo", "kilocode.kilo-code", SessionProvider::Kilo),
    ] {
        let tmp = TempDir::new().unwrap();
        let (home, project) = setup(&tmp);
        let _home = EnvVarGuard::set("HOME", &home);
        init_git_repo(&project);
        mark_test_project(&project);
        let history = write_task(
            &vscode_storage_root(&home, extension_id),
            &project,
            &format!("{provider}-secret"),
        );
        let secret = format!("sk-proj-{provider}-canary-1234567890");
        std::fs::write(
            history,
            serde_json::json!([{
                "role": "user",
                "content": format!("{provider} sanitizer safe text: {secret}"),
                "ts": 1_800_000_000_i64
            }])
            .to_string(),
        )
        .unwrap();
        let db = open_project_session_db(&project).await.unwrap();

        assert!(
            ingest_global_sources_for_provider(&db, &project, Some(selected_provider))
                .await
                .messages_upserted
                > 0,
            "{provider}: sanitized input should project"
        );
        assert_eq!(
            db.search_session_messages(provider, None, "sanitizer safe text", 10)
                .await
                .len(),
            1,
            "{provider}: safe text remains searchable"
        );
        assert_secret_absent_from_observation_sinks(&db, provider, &secret).await;
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cline_like_replacement_projection_replay_is_deterministic() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Table-driven across the three Cline-like storage roots.
    for (provider, extension_id, selected_provider) in [
        ("cline", "saoudrizwan.claude-dev", SessionProvider::Cline),
        (
            "roo-code",
            "rooveterinaryinc.roo-cline",
            SessionProvider::RooCode,
        ),
        ("kilo", "kilocode.kilo-code", SessionProvider::Kilo),
    ] {
        let tmp = TempDir::new().unwrap();
        let (home, project) = setup(&tmp);
        let _home = EnvVarGuard::set("HOME", &home);
        init_git_repo(&project);
        mark_test_project(&project);
        let root = vscode_storage_root(&home, extension_id);
        let session_id = format!("{provider}-fault");
        let history = write_task(&root, &project, &session_id);

        let db = open_project_session_db(&project).await.unwrap();
        let _ = ingest_global_sources_for_provider(&db, &project, Some(selected_provider)).await;
        assert_eq!(
            db.session_message_count().await.unwrap(),
            3,
            "{provider}: initial durable message cardinality"
        );
        let prefix_cursor = observation_source_cursor(&db, provider, &session_id, &project)
            .await
            .unwrap_or_else(|| panic!("{provider}: committed observation cursor"));
        assert_eq!(prefix_cursor.position(), 3, "{provider}: initial frontier");
        drop(db);

        // Exact restart is a no-op.
        let replay = open_project_session_db(&project).await.unwrap();
        assert_eq!(
            ingest_global_sources_for_provider(&replay, &project, Some(selected_provider))
                .await
                .messages_upserted,
            0,
            "{provider}: restart no-op"
        );
        assert_eq!(
            observation_source_cursor(&replay, provider, &session_id, &project).await,
            Some(prefix_cursor.clone()),
            "{provider}: frontier unchanged on restart"
        );

        // Replacement with an extra durable turn, interrupted by projection failure.
        std::fs::write(
            &history,
            serde_json::to_string_pretty(&serde_json::json!([
                {
                    "role": "user",
                    "content": "Investigate the billing pipeline regression",
                    "ts": 1_800_000_000_i64
                },
                {
                    "role": "assistant",
                    "content": "The billing pipeline regression is fixed.",
                    "ts": 1_800_000_010_i64
                },
                {
                    "role": "user",
                    "content": format!("{provider} projection retry suffix"),
                    "ts": 1_800_000_020_i64
                }
            ]))
            .unwrap(),
        )
        .unwrap();
        set_projection_failure(&replay, true).await;
        let _ =
            ingest_global_sources_for_provider(&replay, &project, Some(selected_provider)).await;
        let committed_cursor = observation_source_cursor(&replay, provider, &session_id, &project)
            .await
            .unwrap_or_else(|| panic!("{provider}: committed observation cursor"));
        assert_ne!(
            committed_cursor.generation(),
            prefix_cursor.generation(),
            "{provider}: replacement starts a new snapshot generation"
        );
        assert_eq!(
            committed_cursor.position(),
            3,
            "{provider}: observation frontier commits before projection acknowledgement"
        );
        assert_eq!(
            replay.session_message_count().await.unwrap(),
            3,
            "{provider}: failed projection preserves prior durable cardinality"
        );
        assert!(
            replay
                .search_session_messages(provider, None, "projection retry suffix", 10)
                .await
                .is_empty(),
            "{provider}: failed suffix must stay non-durable"
        );

        set_projection_failure(&replay, false).await;
        let _ =
            ingest_global_sources_for_provider(&replay, &project, Some(selected_provider)).await;
        assert_eq!(
            replay
                .search_session_messages(provider, None, "projection retry suffix", 10)
                .await
                .len(),
            1,
            "{provider}: recovered suffix searchable"
        );
        assert_eq!(
            observation_source_cursor(&replay, provider, &session_id, &project).await,
            Some(committed_cursor),
            "{provider}: retry must not advance the committed observation frontier"
        );
        assert_eq!(
            ingest_global_sources_for_provider(&replay, &project, Some(selected_provider),)
                .await
                .messages_upserted,
            0,
            "{provider}: post-recovery replay"
        );
    }
}

#[tokio::test]
#[cfg(not(windows))]
#[allow(clippy::await_holding_lock)]
async fn cline_delimiter_ambiguous_native_ids_survive_restart_and_rebuild() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let left_path = write_task(&root, &project, "a:b");
    let right_path = write_task(&root, &project, "a");
    std::fs::write(
        left_path,
        serde_json::json!([{
            "id": "c",
            "role": "assistant",
            "content": "Cline delimiter collision fixture",
            "ts": 1_800_000_000_i64
        }])
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        right_path,
        serde_json::json!([{
            "id": "b:c",
            "role": "assistant",
            "content": "Cline delimiter collision fixture",
            "ts": 1_800_000_000_i64
        }])
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Cline)).await;
    let hits = db
        .search_session_messages("cline", None, "delimiter collision fixture", 10)
        .await;
    assert_eq!(hits.len(), 2);
    let mut ids = hits
        .iter()
        .map(|hit| hit.message.message_id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 2);
    assert!(
        ids.iter()
            .all(|id| id.starts_with("cline-like.message-id.v2."))
    );
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&reopened, &project, Some(SessionProvider::Cline))
            .await
            .messages_upserted,
        0
    );
    let committed = durable_table_count(&reopened, "observations").await;
    let store = reopened
        .runtime()
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    loop {
        if store
            .rebuild_projection(committed)
            .await
            .unwrap()
            .is_complete()
        {
            break;
        }
    }
    let rebuilt = reopened
        .search_session_messages("cline", None, "delimiter collision fixture", 10)
        .await;
    assert_eq!(rebuilt.len(), 2);
    assert_ne!(rebuilt[0].message.message_id, rebuilt[1].message.message_id);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn golden_fixture_ingests_through_each_provider_discriminator() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/transcript_golden/cline_like/manifest.json"
    ))
    .expect("cline_like golden manifest");
    assert_eq!(manifest["family"], "cline_like");
    assert!(
        manifest["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .any(|note| note
                .as_str()
                .is_some_and(|text| text.contains("UnknownVersion"))),
        "manifest must document the UnknownVersion protocol gap"
    );

    for (provider, extension_id, selected_provider, api_filename) in [
        (
            "cline",
            "saoudrizwan.claude-dev",
            SessionProvider::Cline,
            "api_conversation_history.json",
        ),
        (
            "roo-code",
            "rooveterinaryinc.roo-cline",
            SessionProvider::RooCode,
            "api_messages.json",
        ),
        (
            "kilo",
            "kilocode.kilo-code",
            SessionProvider::Kilo,
            "api_conversation_history.json",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let (home, project) = setup(&tmp);
        let _home = EnvVarGuard::set("HOME", &home);
        init_git_repo(&project);
        mark_test_project(&project);
        let root = vscode_storage_root(&home, extension_id);
        let session_id = format!("{provider}-golden");
        let api_path = write_task_with_api_filename(&root, &project, &session_id, api_filename);
        assert!(
            api_path.ends_with(api_filename),
            "{provider}: must exercise real api history filename {api_filename}"
        );
        assert!(
            !api_path
                .to_string_lossy()
                .contains("api_conversation_history.json")
                || api_filename == "api_conversation_history.json",
            "{provider}: Roo must not silently fall back to Cline filename"
        );

        let db = open_project_session_db(&project).await.unwrap();
        let _ = ingest_global_sources_for_provider(&db, &project, Some(selected_provider)).await;
        let hits = db
            .search_session_messages(provider, None, "billing pipeline", 10)
            .await;
        assert!(
            hits.iter()
                .any(|hit| hit.message.tool_names.as_deref() == Some("read_file")),
            "{provider}: golden tool_use name must reach searchable projection via production ingest"
        );
        assert!(
            hits.iter().all(|hit| hit.message.provider == provider),
            "{provider}: searchable rows must carry the provider discriminator, not a relabel"
        );
        assert!(
            hits.iter().all(|hit| hit.message.source_path.is_none()),
            "{provider}: canonical projection must not retain the native source path"
        );
    }
}
