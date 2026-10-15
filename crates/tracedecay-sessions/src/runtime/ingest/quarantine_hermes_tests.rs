use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_runtime_core::config::ProfileRoot;

use crate::admission::test_support::MemoryHostAdmission;
use crate::observation::ObservationCancellation;
use crate::runtime::SessionProvider;
use crate::runtime::hosts::codex::CodexDiscoveryHub;
use crate::runtime::ingest::project_provider::ProjectProviderRun;
use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
use crate::runtime::source::spin_until_jsonl_change_settled;

use super::project::with_transcript_source_profile;
use super::quarantine_contract::{assert_refused_source_contract, refuse_session};

/// One profile `state.db` holds both sessions. The refused session's rows come
/// first in `messages.id` order, so a sweep that stops at the refused session
/// never reaches the sibling.
#[tokio::test]
async fn hermes_keeps_the_refused_source_contract() {
    install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    // Hermes only discovers state databases for a project that is a git worktree.
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let init = std::process::Command::new(tracedecay_runtime_core::git::try_git_program().unwrap())
        .args(["init", "-q"])
        .current_dir(&project)
        .status()
        .unwrap();
    assert!(init.success());
    let profile = home.join(".hermes");
    std::fs::create_dir_all(&profile).unwrap();
    let state_db = profile.join("state.db");
    let connection = rusqlite::Connection::open(&state_db).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT);
             CREATE TABLE messages (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 content TEXT,
                 timestamp REAL NOT NULL
             );",
        )
        .unwrap();
    for (row, session) in ["refused", "sibling"].into_iter().enumerate() {
        connection
            .execute(
                "INSERT INTO sessions (id, cwd) VALUES (?1, ?2)",
                rusqlite::params![session, project.to_string_lossy()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES (?1, 'user', 'first turn', ?2)",
                rusqlite::params![session, row as f64],
            )
            .unwrap();
    }
    drop(connection);
    spin_until_jsonl_change_settled(&state_db);
    let hub = CodexDiscoveryHub::default();
    hub.register("project", Some(&home));
    let project_id = ProjectId::new("project-hermes-refused-source").unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let admission = MemoryHostAdmission::default();
    refuse_session(&admission, "refused");
    let cancellation = ObservationCancellation::default();

    assert_refused_source_contract(
        &admission,
        "sibling",
        || {
            let run = ProjectProviderRun {
                project_root: &project,
                project_id: &project_id,
                facade: &admission,
                scope: &scope,
                candidate: SessionProvider::Hermes,
                max_new_bytes: u64::MAX,
                cancellation: &cancellation,
                codex_discovery: Some((&hub, "project")),
            };
            with_transcript_source_profile(ProfileRoot::under_home(home.clone()), async move {
                run.run().await.outcome
            })
        },
        || {
            rusqlite::Connection::open(&state_db)
                .unwrap()
                .execute(
                    "UPDATE messages SET content = 'rewritten turn' WHERE session_id = 'refused'",
                    [],
                )
                .unwrap();
            spin_until_jsonl_change_settled(&state_db);
        },
    )
    .await;
}
