use super::*;
use crate::agents::hermes::HermesIntegration;
use crate::agents::{AgentIntegration, InstallContext, UpdatePluginOutcome};
use crate::memory::types::{AddFactRequest, FeedbackAction, FeedbackRequest, MemoryCategory};
use crate::sessions::{SessionMessageRecord, SessionRecord};

async fn test_initialize(path: &Path) -> (Database, bool) {
    let authority =
        crate::db::DatabaseAuthority::acquire_test(path, "Hermes migration test initialize")
            .unwrap();
    Database::initialize(path, &authority).await.unwrap()
}

async fn test_open(path: &Path) -> (Database, bool) {
    let authority =
        crate::db::DatabaseAuthority::acquire_test(path, "Hermes migration test open").unwrap();
    Database::open(path, &authority).await.unwrap()
}

async fn test_open_read_only(path: &Path) -> (Database, bool) {
    let authority =
        crate::db::DatabaseAuthority::acquire_test(path, "Hermes migration test read").unwrap();
    Database::open_read_only(path, &authority).await.unwrap()
}

fn mark_real_project(project: &Path) {
    fs::create_dir_all(project.join(".tracedecay")).unwrap();
    fs::write(project.join(".tracedecay/tracedecay.db"), []).unwrap();
}

#[tokio::test]
async fn registry_cleanup_preserves_reassigned_project_identity() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = temp.path().join("profile");
    let legacy_root = temp.path().join("home/.hermes");
    let corrected_root = temp.path().join("projects/hermes-agent");
    fs::create_dir_all(&legacy_root).unwrap();
    fs::create_dir_all(&corrected_root).unwrap();
    let registry = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    registry
        .upsert_code_project("reassigned", &corrected_root, None, None, None)
        .await
        .unwrap();

    remove_legacy_registry_metadata(
        &profile_root,
        Some("reassigned"),
        &legacy_root,
        &crate::migrate::hermes::RootRegistry,
    )
    .await
    .unwrap();

    assert!(registry.get_code_project("reassigned").await.is_some());
}

async fn seed_source(path: &Path, sessions: &[(&str, &Path)]) {
    let db = GlobalDb::open_at(path).await.expect("open source");
    for (ordinal, (session_id, project)) in sessions.iter().enumerate() {
        let project = project.to_string_lossy().to_string();
        assert!(
            db.upsert_session(&SessionRecord {
                provider: "hermes".into(),
                session_id: (*session_id).into(),
                project_key: project.clone(),
                project_path: project,
                title: Some("legacy".into()),
                started_at: Some(ordinal as i64 + 1),
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
        );
        assert!(
            db.upsert_session_message(&SessionMessageRecord {
                provider: "hermes".into(),
                message_id: format!("message-{session_id}"),
                session_id: (*session_id).into(),
                role: "user".into(),
                timestamp: Some(ordinal as i64 + 1),
                ordinal: 0,
                text: "keep this".into(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            })
            .await
        );
    }
}

async fn seed_memory_fact(path: &Path, content: &str) -> i64 {
    let (db, _) = test_initialize(path).await;
    MemoryStore::new(db.conn())
        .add_fact(
            AddFactRequest {
                content: content.to_string(),
                category: MemoryCategory::Decision,
                source: Some("hermes".to_string()),
                tags: vec!["legacy".to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(0.9),
                metadata: serde_json::json!({"migration_test": true}),
            },
            0.5,
        )
        .await
        .unwrap()
        .fact
        .unwrap()
        .fact_id
}

async fn seed_legacy_state_db_without_cwd(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                model TEXT,
                parent_session_id TEXT,
                started_at REAL NOT NULL,
                ended_at REAL,
                title TEXT,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0,
                cache_write_tokens INTEGER DEFAULT 0,
                reasoning_tokens INTEGER DEFAULT 0
             );
             CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_calls TEXT,
                tool_name TEXT,
                timestamp REAL NOT NULL,
                reasoning TEXT,
                active INTEGER NOT NULL DEFAULT 1
             );
             INSERT INTO sessions (
                id, source, model, started_at, ended_at, title
             ) VALUES (
                'legacy-state-session', 'tui', 'legacy-model', 1.0, 2.0, 'legacy state'
             );
             INSERT INTO messages (
                session_id, role, content, timestamp
             ) VALUES (
                'legacy-state-session', 'user', 'state row without cwd', 1.0
             );",
    )
    .await
    .unwrap();
}

async fn count(conn: &Connection, table: &str) -> i64 {
    let mut rows = conn
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn marker_count(target_db_path: &Path) -> usize {
    target_db_path
        .parent()
        .and_then(|root| fs::read_dir(root.join(LEDGER_DIR)).ok())
        .map(|entries| entries.flatten().count())
        .unwrap_or(0)
}

#[tokio::test]
async fn migrates_standard_profile_store_once() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    let source = hermes.join(".tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    seed_source(&source, &[("session-1", &project)]).await;
    seed_memory_fact(
        &source.with_file_name("tracedecay.db"),
        "legacy Hermes fact",
    )
    .await;

    let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(first.migrated.len(), 1, "{first:?}");
    assert!(first.migrated[0].rows_copied >= 3);
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 1);
    assert_eq!(count(target.conn(), "session_messages").await, 1);
    assert_eq!(count(target.conn(), "lcm_raw_messages").await, 1);
    assert_eq!(marker_count(&layout.sessions_db_path), 1);
    let (target_code, _) = test_open_read_only(&layout.graph_db_path).await;
    let facts = MemoryStore::new(target_code.conn())
        .list_facts(None, None, 10)
        .await
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].content, "legacy Hermes fact");
    assert!(facts[0].entities.contains(&"TraceDecay".to_string()));
    assert_eq!(
        target
            .get_session("hermes", "session-1")
            .await
            .unwrap()
            .project_path,
        GlobalDb::canonical_project_key(&project)
    );
    drop(target);

    let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(second.already_migrated.len(), 1, "{second:?}");
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 1);
    assert_eq!(marker_count(&layout.sessions_db_path), 1);
    let (target_code, _) = test_open_read_only(&layout.graph_db_path).await;
    assert_eq!(
        MemoryStore::new(target_code.conn())
            .list_facts(None, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    assert_eq!(count(source_after.conn(), "sessions").await, 1);
}

#[tokio::test]
async fn migration_marker_remerges_when_a_target_row_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    let source = hermes.join(".tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    seed_source(&source, &[("session-1", &project)]).await;

    let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(first.migrated.len(), 1, "{first:?}");
    let initial_rows_copied = first.migrated[0].rows_copied;
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
    assert_eq!(
            target
                .conn()
                .execute(
                    "DELETE FROM session_messages WHERE provider = 'hermes' AND message_id = 'message-session-1'",
                    (),
                )
                .await
                .unwrap(),
            1
        );
    drop(target);

    let repaired = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(repaired.migrated.len(), 1, "{repaired:?}");
    assert!(repaired.already_migrated.is_empty(), "{repaired:?}");
    assert_eq!(repaired.migrated[0].rows_copied, 1, "{repaired:?}");
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "session_messages").await, 1);
    drop(target);

    let verified = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(verified.already_migrated.len(), 1, "{verified:?}");
    assert_eq!(
        verified.already_migrated[0].rows_copied,
        initial_rows_copied + 1,
        "{verified:?}"
    );
    assert_eq!(marker_count(&layout.sessions_db_path), 1);

    let marker_path = fs::read_dir(layout.sessions_db_path.parent().unwrap().join(LEDGER_DIR))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut marker: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    marker["schema_version"] = serde_json::json!(1);
    marker.as_object_mut().unwrap().remove("target_project_id");
    marker.as_object_mut().unwrap().remove("target_db_path");
    fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

    let upgraded = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(upgraded.already_migrated.len(), 1, "{upgraded:?}");
    let mut marker: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
    assert_eq!(marker["schema_version"], 2);
    assert!(marker["target_project_id"].as_str().is_some());
    assert!(marker["target_db_path"].as_str().is_some());

    marker["target_project_id"] = serde_json::json!("proj_wrong_target");
    fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

    let mismatched = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(mismatched.failed.len(), 1, "{mismatched:?}");
    assert!(
        mismatched.failed[0]
            .reason
            .contains("different project store")
    );
}

#[tokio::test]
async fn migrates_pinned_memory_store_without_session_store() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    fs::create_dir_all(hermes.join(".tracedecay")).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    seed_memory_fact(
        &hermes.join(".tracedecay/tracedecay.db"),
        "facts survive without sessions",
    )
    .await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let (target, _) = test_open_read_only(&layout.graph_db_path).await;
    let facts = MemoryStore::new(target.conn())
        .list_facts(None, None, 10)
        .await
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].content, "facts survive without sessions");
}

#[tokio::test]
async fn migrates_pinned_state_db_rows_without_cwd_before_unpin() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    fs::create_dir_all(&hermes).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    let state_db = hermes.join("state.db");
    seed_legacy_state_db_without_cwd(&state_db).await;

    let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(first.migrated.len(), 1, "{first:?}");
    assert_eq!(first.migrated[0].source_db, state_db);
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 1);
    assert_eq!(count(target.conn(), "session_messages").await, 1);
    assert!(
        fs::read_to_string(hermes.join("config.yaml"))
            .unwrap()
            .contains("project_root"),
        "the migration layer must leave the pin for lifecycle cutover"
    );

    let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(second.already_migrated.len(), 1, "{second:?}");
    assert_eq!(count(target.conn(), "session_messages").await, 1);
}

#[tokio::test]
async fn failed_state_db_import_preserves_project_pin() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    fs::create_dir_all(&hermes).unwrap();
    let config = format!(
        "plugins:\n  tracedecay:\n    project_root: {}\n",
        project.display()
    );
    fs::write(hermes.join("config.yaml"), &config).unwrap();
    fs::write(hermes.join("state.db"), b"not sqlite").unwrap();

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert_eq!(
        fs::read_to_string(hermes.join("config.yaml")).unwrap(),
        config
    );
}

#[tokio::test]
async fn named_profile_upgrade_refreshes_in_place_without_default_cutover() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let legacy_profile = user_home.join(".hermes/profiles/work");
    let legacy_plugin = legacy_profile.join("plugins/tracedecay");
    fs::create_dir_all(&legacy_plugin).unwrap();
    fs::write(legacy_plugin.join("plugin.yaml"), "name: tracedecay\n").unwrap();
    let legacy_config = format!(
        "plugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: {}\n",
        project.display()
    );
    fs::write(legacy_profile.join("config.yaml"), &legacy_config).unwrap();
    seed_legacy_state_db_without_cwd(&legacy_profile.join("state.db")).await;

    let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(first.migrated.len(), 1, "{first:?}");

    let default_config = user_home.join(".hermes/config.yaml");
    fs::write(&default_config, "memory:\n  provider: other\n").unwrap();
    let ctx = InstallContext {
        home: user_home.clone(),
        tracedecay_bin: "/bin/tracedecay".to_string(),
        tool_permissions: crate::agents::expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };
    let outcome = HermesIntegration.update_plugin(&ctx).unwrap();
    assert!(matches!(
        outcome,
        UpdatePluginOutcome::Refreshed(paths) if paths == vec![legacy_plugin.clone()]
    ));
    assert!(legacy_plugin.join("plugin.yaml").is_file());
    assert_eq!(
        fs::read_to_string(legacy_profile.join("config.yaml")).unwrap(),
        legacy_config
    );
    assert!(
        !user_home
            .join(".hermes/plugins/tracedecay/plugin.yaml")
            .exists()
    );

    let retry_migration = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(
        retry_migration.already_migrated.len(),
        1,
        "{retry_migration:?}"
    );
    fs::write(&default_config, "").unwrap();
    let outcome = HermesIntegration.update_plugin(&ctx).unwrap();
    assert!(matches!(
        outcome,
        UpdatePluginOutcome::Refreshed(paths) if paths == vec![legacy_plugin.clone()]
    ));
    assert!(legacy_plugin.join("plugin.yaml").is_file());
    assert!(
        !user_home
            .join(".hermes/plugins/tracedecay/plugin.yaml")
            .exists()
    );

    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "session_messages").await, 1);
}

#[tokio::test]
async fn same_content_memory_fact_merges_trust_and_feedback_once() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    let source_sessions = hermes.join(".tracedecay/sessions.db");
    let source_memory = hermes.join(".tracedecay/tracedecay.db");
    fs::create_dir_all(source_sessions.parent().unwrap()).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    seed_source(&source_sessions, &[("session", &project)]).await;
    let source_fact_id = seed_memory_fact(&source_memory, "shared durable fact").await;
    let (source_db, _) = test_open(&source_memory).await;
    MemoryStore::new(source_db.conn())
        .record_feedback_event(FeedbackRequest {
            fact_id: source_fact_id,
            action: FeedbackAction::Helpful,
            source: Some("legacy-hermes".to_string()),
            note: Some("source evidence".to_string()),
        })
        .await
        .unwrap();
    drop(source_db);

    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let (target_db, _) = test_initialize(&layout.graph_db_path).await;
    let target_store = MemoryStore::new(target_db.conn());
    let target_fact = target_store
        .add_fact(
            AddFactRequest {
                content: "shared durable fact".to_string(),
                category: MemoryCategory::Project,
                source: Some("target".to_string()),
                tags: vec!["target".to_string()],
                entities: vec!["Target".to_string()],
                trust: Some(0.2),
                metadata: serde_json::json!({"target": true}),
            },
            0.5,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    target_store
        .record_feedback_event(FeedbackRequest {
            fact_id: target_fact.fact_id,
            action: FeedbackAction::Unhelpful,
            source: Some("target".to_string()),
            note: Some("target evidence".to_string()),
        })
        .await
        .unwrap();
    drop(target_db);

    let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(first.migrated.len(), 1, "{first:?}");
    let (target_db, _) = test_open_read_only(&layout.graph_db_path).await;
    let facts = MemoryStore::new(target_db.conn())
        .list_facts(None, None, 10)
        .await
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].helpful_count, 1);
    assert_eq!(facts[0].unhelpful_count, 1);
    assert!(facts[0].tags.contains(&"legacy".to_string()));
    assert!(facts[0].tags.contains(&"target".to_string()));
    assert_eq!(count(target_db.conn(), "memory_feedback_events").await, 2);
    drop(target_db);

    let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(second.already_migrated.len(), 1, "{second:?}");
    let (target_db, _) = test_open_read_only(&layout.graph_db_path).await;
    assert_eq!(count(target_db.conn(), "memory_feedback_events").await, 2);
    let facts = MemoryStore::new(target_db.conn())
        .list_facts(None, None, 10)
        .await
        .unwrap();
    assert_eq!(facts[0].helpful_count, 1);
    assert_eq!(facts[0].unhelpful_count, 1);
}

#[tokio::test]
async fn conflicting_existing_message_blocks_migration() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    let source = hermes.join(".tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    seed_source(&source, &[("session-1", &project)]).await;

    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    seed_source(&layout.sessions_db_path, &[("session-1", &project)]).await;
    let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
    assert!(
        target
            .upsert_session_message(&SessionMessageRecord {
                provider: "hermes".into(),
                message_id: "message-session-1".into(),
                session_id: "session-1".into(),
                role: "user".into(),
                timestamp: Some(1),
                ordinal: 0,
                text: "conflicting target content".into(),
                kind: None,
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            })
            .await
    );
    drop(target);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert!(report.failed[0].reason.contains("conflicts"));
}

#[tokio::test]
async fn nonidentical_session_identity_collision_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let hermes = user_home.join(".hermes");
    let source = hermes.join(".tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        hermes.join("config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        ),
    )
    .unwrap();
    seed_source(&source, &[("session-1", &project)]).await;

    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    seed_source(&layout.sessions_db_path, &[("session-1", &project)]).await;
    let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
    target
        .conn()
        .execute(
            "UPDATE sessions SET title = 'different target title'
                 WHERE provider = 'hermes' AND session_id = 'session-1'",
            (),
        )
        .await
        .unwrap();
    drop(target);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert!(report.failed[0].reason.contains("collides"));
    assert!(report.failed[0].reason.contains("sessions"));
}

#[tokio::test]
async fn ambiguous_metadata_is_preserved_and_reported() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    mark_real_project(&first);
    mark_real_project(&second);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("first", &first), ("second", &second)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(report.unresolved[0].reason.contains("ambiguous"));
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    assert_eq!(count(source_after.conn(), "sessions").await, 2);
    assert!(
        !crate::storage::resolve_layout(&first, &profile_root)
            .unwrap()
            .sessions_db_path
            .exists()
    );
}

#[tokio::test]
async fn one_unpinned_metadata_project_is_migrated() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &project)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert_eq!(
        report.migrated[0].target_project,
        project.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn moved_pinned_project_resolves_through_registered_alias() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let legacy_project = temp.path().join("project-before-move");
    let current_project = temp.path().join("project-after-move");
    mark_real_project(&legacy_project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        user_home.join(".hermes/config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            legacy_project.display()
        ),
    )
    .unwrap();
    seed_source(&source, &[("session", &legacy_project)]).await;

    fs::create_dir_all(&profile_root).unwrap();
    let registry = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    registry
        .upsert_code_project("stable-project", &legacy_project, None, None, None)
        .await
        .unwrap();
    fs::rename(&legacy_project, &current_project).unwrap();
    registry
        .upsert_code_project("stable-project", &current_project, None, None, None)
        .await
        .unwrap();
    drop(registry);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert_eq!(
        report.migrated[0].target_project,
        current_project.canonicalize().unwrap()
    );
    let target =
        GlobalDb::open_read_only_at(&profile_root.join("projects/stable-project/sessions.db"))
            .await
            .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 1);
    assert!(
        !crate::storage::resolve_layout(&current_project, &profile_root)
            .unwrap()
            .sessions_db_path
            .exists(),
        "migration must not create a second path-hash shard"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn moved_project_resolves_through_canonicalized_missing_parent_alias() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let physical_parent = temp.path().join("physical");
    let alias_parent = temp.path().join("alias");
    fs::create_dir_all(&physical_parent).unwrap();
    std::os::unix::fs::symlink(&physical_parent, &alias_parent).unwrap();
    let legacy_alias = alias_parent.join("project-before-move");
    let legacy_physical = physical_parent.join("project-before-move");
    let current_project = physical_parent.join("project-after-move");
    mark_real_project(&legacy_alias);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        user_home.join(".hermes/config.yaml"),
        format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            legacy_alias.display()
        ),
    )
    .unwrap();
    seed_source(&source, &[("session", &legacy_alias)]).await;

    fs::create_dir_all(&profile_root).unwrap();
    let registry = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    registry
        .upsert_code_project("stable-project", &legacy_physical, None, None, None)
        .await
        .unwrap();
    fs::rename(&legacy_physical, &current_project).unwrap();
    registry
        .upsert_code_project("stable-project", &current_project, None, None, None)
        .await
        .unwrap();
    drop(registry);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert_eq!(
        report.migrated[0].target_project,
        current_project.canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn removed_unprovable_symlink_metadata_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let legacy_project = temp.path().join("project-before-move");
    let project_alias = temp.path().join("project-link");
    let current_project = temp.path().join("project-after-move");
    mark_real_project(&legacy_project);
    std::os::unix::fs::symlink(&legacy_project, &project_alias).unwrap();
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &legacy_project)]).await;
    let source_rw = GlobalDb::open_at(&source).await.unwrap();
    source_rw
        .conn()
        .execute(
            "UPDATE sessions SET project_path = ?1 WHERE session_id = 'session'",
            [project_alias.to_string_lossy().to_string()],
        )
        .await
        .unwrap();
    drop(source_rw);

    fs::create_dir_all(&profile_root).unwrap();
    let registry = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    registry
        .upsert_code_project("stable-project", &project_alias, None, None, None)
        .await
        .unwrap();
    fs::remove_file(&project_alias).unwrap();
    fs::rename(&legacy_project, &current_project).unwrap();
    registry
        .upsert_code_project("stable-project", &current_project, None, None, None)
        .await
        .unwrap();
    drop(registry);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert!(report.migrated.is_empty(), "{report:?}");
    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(
        !profile_root
            .join("projects/stable-project/sessions.db")
            .is_file()
    );
}

#[tokio::test]
async fn migrates_profile_shard_misidentified_as_hermes_project() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let hermes = user_home.join(".hermes");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let legacy_shard = profile_root.join("projects/legacy-hermes-identity");
    let source = legacy_shard.join(crate::storage::SESSIONS_DB_FILENAME);
    fs::create_dir_all(&legacy_shard).unwrap();
    let manifest = crate::storage::StoreManifest {
        schema_version: crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("legacy-hermes-identity".into()),
        store_kind: crate::storage::StoreKind::CodeProject,
        storage_mode: crate::storage::StorageMode::ProfileSharded,
        project_root: hermes.clone(),
        data_root: legacy_shard.clone(),
        graph_db_relpath: PathBuf::from("tracedecay.db"),
        sessions_db_relpath: PathBuf::from(crate::storage::SESSIONS_DB_FILENAME),
        branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
    };
    fs::write(
        legacy_shard.join(crate::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let registry = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    registry
        .upsert_code_project("legacy-hermes-identity", &hermes, None, None, None)
        .await
        .unwrap();
    seed_source(&source, &[("session", &project)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert_eq!(report.migrated[0].source_db, source);
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    assert_eq!(count(source_after.conn(), "sessions").await, 1);
    let target_layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    assert_ne!(target_layout.sessions_db_path, source);
    let target = GlobalDb::open_read_only_at(&target_layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 1);
    assert!(source.is_file());
    assert!(
        registry
            .get_code_project("legacy-hermes-identity")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn migrates_hermes_owned_profile_shard_sessions_to_user_and_cleans_registry() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let hermes = user_home.join(".hermes");
    let legacy_shard = profile_root.join("projects/legacy-hermes-projectless");
    let source = legacy_shard.join(crate::storage::SESSIONS_DB_FILENAME);
    fs::create_dir_all(&legacy_shard).unwrap();
    let manifest = crate::storage::StoreManifest {
        schema_version: crate::storage::STORE_MANIFEST_SCHEMA_VERSION,
        project_id: Some("legacy-hermes-projectless".into()),
        store_kind: crate::storage::StoreKind::CodeProject,
        storage_mode: crate::storage::StorageMode::ProfileSharded,
        project_root: hermes.clone(),
        data_root: legacy_shard.clone(),
        graph_db_relpath: PathBuf::from("tracedecay.db"),
        sessions_db_relpath: PathBuf::from(crate::storage::SESSIONS_DB_FILENAME),
        branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
    };
    fs::write(
        legacy_shard.join(crate::storage::STORE_MANIFEST_FILENAME),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let registry = GlobalDb::open_at(&profile_root.join("global.db"))
        .await
        .unwrap();
    registry
        .upsert_code_project("legacy-hermes-projectless", &hermes, None, None, None)
        .await
        .unwrap();
    seed_source(&source, &[("session", &hermes)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.migrated[0].source_db, source);
    assert_eq!(report.migrated[0].target_project, Path::new("user"));
    let target_path = crate::sessions::user_sessions_db_path(&profile_root);
    let target = GlobalDb::open_read_only_at(&target_path).await.unwrap();
    let session = target.get_session("hermes", "session").await.unwrap();
    assert_eq!(session.project_key, "user");
    assert_eq!(session.project_path, "user");
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    for table in ["sessions", "session_messages"] {
        assert_eq!(
            count(source_after.conn(), table).await,
            count(target.conn(), table).await,
            "row parity for {table}"
        );
    }
    assert_eq!(marker_count(&target_path), 1);
    assert!(source.is_file());
    assert!(
        registry
            .get_code_project("legacy-hermes-projectless")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn migrates_older_source_with_missing_current_columns_and_lcm_tables() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let source_handle = libsql::Builder::new_local(&source).build().await.unwrap();
    let source_conn = source_handle.connect().unwrap();
    source_conn
        .execute_batch(
            "CREATE TABLE sessions (
                    provider TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    project_key TEXT NOT NULL,
                    project_path TEXT NOT NULL,
                    title TEXT,
                    PRIMARY KEY(provider, session_id)
                 );
                 CREATE TABLE session_messages (
                    provider TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    PRIMARY KEY(provider, message_id)
                 );",
        )
        .await
        .unwrap();
    let project_text = project.to_string_lossy().to_string();
    source_conn
        .execute(
            "INSERT INTO sessions(provider, session_id, project_key, project_path, title)
                 VALUES ('hermes', 'old-session', ?1, ?1, 'old')",
            [project_text],
        )
        .await
        .unwrap();
    source_conn
        .execute(
            "INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
                 VALUES ('hermes', 'old-message', 'old-session', 'user', 0, 'old text')",
            (),
        )
        .await
        .unwrap();
    drop(source_conn);
    drop(source_handle);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 1);
    assert_eq!(count(target.conn(), "session_messages").await, 1);
    assert_eq!(count(target.conn(), "lcm_raw_messages").await, 0);
}

#[tokio::test]
async fn projectless_profile_sessions_migrate_to_user_store_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let hermes = user_home.join(".hermes");
    let source = hermes.join(".tracedecay/sessions.db");
    let source_memory = hermes.join(".tracedecay/tracedecay.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let source_fact_id = seed_memory_fact(&source_memory, "unscoped legacy fact").await;
    seed_source(&source, &[("session", &hermes)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert_eq!(report.unresolved[0].source_db, source_memory);
    assert!(report.unresolved[0].reason.contains("preserved"));
    let target_path = crate::sessions::user_sessions_db_path(&profile_root);
    let target = GlobalDb::open_read_only_at(&target_path).await.unwrap();
    let session = target.get_session("hermes", "session").await.unwrap();
    assert_eq!(session.project_path, "user");
    assert!(!crate::memory::user::user_memory_db_path(&profile_root).exists());
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    assert_eq!(count(source_after.conn(), "sessions").await, 1);
    drop(source_after);
    let (source_memory_after, _) = test_open_read_only(&source_memory).await;
    assert!(
        MemoryStore::new(source_memory_after.conn())
            .get_fact(source_fact_id)
            .await
            .unwrap()
            .is_some()
    );

    let retry = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(retry.already_migrated.len(), 1, "{retry:?}");
    assert_eq!(retry.unresolved.len(), 1, "{retry:?}");
    assert_eq!(count(target.conn(), "sessions").await, 1);
}

#[tokio::test]
async fn malformed_metadata_is_preserved_not_misrouted_to_user() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &user_home)]).await;
    let source_rw = GlobalDb::open_at(&source).await.unwrap();
    source_rw
            .conn()
            .execute(
                "UPDATE sessions SET project_key = '', project_path = '', metadata_json = '{invalid' WHERE session_id = 'session'",
                (),
            )
            .await
            .unwrap();
    drop(source_rw);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(report.migrated.is_empty());
    assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
}

#[tokio::test]
async fn structurally_invalid_metadata_is_preserved_not_misrouted_to_user() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &user_home)]).await;
    let source_rw = GlobalDb::open_at(&source).await.unwrap();
    source_rw
            .conn()
            .execute(
                "UPDATE sessions SET project_key = '', project_path = '', metadata_json = '{\"project_root\":42}' WHERE session_id = 'session'",
                (),
            )
            .await
            .unwrap();
    drop(source_rw);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(report.migrated.is_empty());
    assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
}

#[tokio::test]
async fn vanished_hermes_owned_path_is_preserved_as_unresolved() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let vanished_project = user_home.join(".hermes/plugins/vanished-project");
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &vanished_project)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert!(report.migrated.is_empty(), "{report:?}");
    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    assert_eq!(count(source_after.conn(), "sessions").await, 1);
}

#[tokio::test]
async fn durable_project_under_hermes_home_remains_project_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = user_home.join(".hermes/workspaces/real-project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &project)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.migrated.len(), 1, "{report:?}");
    assert!(report.unresolved.is_empty(), "{report:?}");
    assert_eq!(
        report.migrated[0].target_project,
        project.canonicalize().unwrap()
    );
    assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
}

#[tokio::test]
async fn existing_unregistered_directory_is_not_assumed_projectless() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let unregistered = temp.path().join("unregistered-project");
    fs::create_dir_all(&unregistered).unwrap();
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &unregistered)]).await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(report.migrated.is_empty());
    assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
}

#[tokio::test]
async fn same_session_resolved_and_unresolved_projects_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    let vanished = temp.path().join("vanished-project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &project)]).await;
    let source_rw = GlobalDb::open_at(&source).await.unwrap();
    source_rw
        .conn()
        .execute(
            "UPDATE sessions SET project_path = ?1 WHERE session_id = 'session'",
            [vanished.to_string_lossy().to_string()],
        )
        .await
        .unwrap();
    drop(source_rw);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(report.migrated.is_empty());
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    assert!(!layout.sessions_db_path.exists());
}

#[tokio::test]
async fn mixed_user_and_project_sessions_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(
        &source,
        &[("user-session", &user_home), ("project-session", &project)],
    )
    .await;

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.unresolved.len(), 1, "{report:?}");
    assert!(report.unresolved[0].reason.contains("ambiguous"));
    assert!(!crate::sessions::user_sessions_db_path(&profile_root).exists());
    let project_layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    assert!(!project_layout.sessions_db_path.exists());
}

#[tokio::test]
async fn future_source_schema_is_rejected_without_target_changes() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &project)]).await;
    let source_rw = GlobalDb::open_at(&source).await.unwrap();
    source_rw
        .conn()
        .execute(
            "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'lcm'",
            [crate::sessions::lcm::LCM_SCHEMA_VERSION + 1],
        )
        .await
        .unwrap();
    drop(source_rw);

    let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert!(report.failed[0].reason.contains("newer"));
    assert!(
        !crate::storage::resolve_layout(&project, &profile_root)
            .unwrap()
            .sessions_db_path
            .exists()
    );
}

#[tokio::test]
async fn injected_failure_rolls_back_and_retry_converges() {
    let temp = tempfile::tempdir().unwrap();
    let user_home = temp.path().join("home");
    let profile_root = temp.path().join("tracedecay-profile");
    let project = temp.path().join("project");
    mark_real_project(&project);
    let source = user_home.join(".hermes/.tracedecay/sessions.db");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    seed_source(&source, &[("session", &project)]).await;
    let layout = crate::storage::resolve_layout(&project, &profile_root).unwrap();
    let target = GlobalDb::open_at(&layout.sessions_db_path).await.unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 0);
    drop(target);

    let failed = migrate_legacy_hermes_stores_inner(
        &user_home,
        &profile_root,
        &[user_home.join(".hermes")],
        Some("sessions"),
        &crate::migrate::hermes::RootRegistry,
        &crate::agents::hermes::read_config_pinned_project_root,
        &crate::migrate::hermes::RootHermesStateImporter,
    )
    .await;
    assert_eq!(failed.failed.len(), 1, "{failed:?}");
    let target = GlobalDb::open_read_only_at(&layout.sessions_db_path)
        .await
        .unwrap();
    assert_eq!(count(target.conn(), "sessions").await, 0);
    assert_eq!(marker_count(&layout.sessions_db_path), 0);
    drop(target);
    let source_after = GlobalDb::open_read_only_at(&source).await.unwrap();
    assert_eq!(count(source_after.conn(), "sessions").await, 1);
    drop(source_after);

    let retry = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
    assert_eq!(retry.migrated.len(), 1, "{retry:?}");
}

#[test]
fn single_legacy_home_profile_scan_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let standard = temp.path().join("home/.hermes");
    fs::create_dir_all(standard.join("profiles/alpha")).unwrap();
    let profiles = legacy_profile_dirs(&standard);
    assert_eq!(
        profiles,
        vec![standard.clone(), standard.join("profiles/alpha")]
    );
}
