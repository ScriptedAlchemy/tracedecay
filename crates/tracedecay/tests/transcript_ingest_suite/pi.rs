use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_sessions::runtime::SessionProvider;

use crate::restart_atomicity::{
    ingest_global_sources_for_provider, mark_test_project, open_project_session_db,
};
use crate::support::{init_git_repo, setup};

const SESSION_ID: &str = "5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90";
const FIXTURE_NAME: &str = "2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl";
const FIXTURE: &str = include_str!(
    "../../../../tests/fixtures/transcript_golden/pi/2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl"
);

/// Copy the fixture into Pi's per-cwd session directory under `agent_dir`.
fn install_fixture(agent_dir: &Path, project: &Path) -> PathBuf {
    let cwd = project.to_string_lossy();
    let encoded = cwd
        .strip_prefix('/')
        .unwrap_or(&cwd)
        .replace(['/', '\\', ':'], "-");
    let dir = agent_dir.join("sessions").join(format!("--{encoded}--"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(FIXTURE_NAME);
    std::fs::write(
        &path,
        FIXTURE.replace(
            "\"<PROJECT_ROOT>\"",
            &serde_json::to_string(project).unwrap(),
        ),
    )
    .unwrap();
    path
}

async fn assert_fixture_session_landed(home: &Path, project: &Path) {
    let db = open_project_session_db(project).await.unwrap();
    let stats =
        ingest_global_sources_for_provider(home, &db, project, Some(SessionProvider::Pi)).await;
    assert!(stats.messages_upserted > 0, "{stats:?}");

    let session = db.get_session("pi", SESSION_ID).await.unwrap();
    assert_eq!(
        session.started_at,
        Some(1_790_352_001),
        "first conversational entry"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert!(
        metadata["edited_files"]
            .as_array()
            .is_some_and(|files| files
                .iter()
                .any(|file| file["path"] == "src/billing/retry.rs")),
        "edited files: {metadata}"
    );

    let hits = db
        .search_session_messages("pi", None, "billing retry", 10)
        .await;
    let message_ids = hits
        .iter()
        .map(|hit| hit.message.message_id.as_str())
        .collect::<Vec<_>>();
    for entry in ["c2d3e4f5", "d3e4f5a6", "f5a6b7c8"] {
        let expected = format!("{SESSION_ID}:{entry}");
        assert!(
            message_ids.contains(&expected.as_str()),
            "{expected} missing from {message_ids:?}"
        );
    }
    assert!(hits.iter().all(|hit| hit.message.session_id == SESSION_ID));
}

#[tokio::test]
async fn pi_fixture_session_lands_in_the_project_store() {
    // An ambient `PI_CODING_AGENT_DIR` is outside this isolated home, so the
    // source keeps the home's default agent directory.
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    init_git_repo(&project);
    mark_test_project(&project);
    install_fixture(&home.join(".pi/agent"), &project);

    assert_fixture_session_landed(&home, &project).await;
}

#[tokio::test]
async fn pi_agent_dir_override_relocates_the_session_source_inside_the_home() {
    // `PI_CODING_AGENT_DIR` is a process variable, honored only when it names
    // a directory inside the home, so the journey runs in a child test process
    // whose environment points it at `<home>/relocated-pi-agent`.
    if !crate::common::in_child_test() {
        let tmp = TempDir::new().unwrap();
        let relocated = tmp.path().join("home").join("relocated-pi-agent");
        crate::common::rerun_test_in_child(
            "pi::pi_agent_dir_override_relocates_the_session_source_inside_the_home",
            &[("PI_CODING_AGENT_DIR", Some(relocated.as_os_str()))],
        );
        return;
    }
    let relocated =
        PathBuf::from(std::env::var_os("PI_CODING_AGENT_DIR").expect("child PI_CODING_AGENT_DIR"));
    let home = relocated
        .parent()
        .expect("relocated agent dir has a home")
        .to_path_buf();
    let project = home.parent().expect("fixture root").join("project");
    crate::support::init_project_at(&project);
    init_git_repo(&project);
    mark_test_project(&project);
    install_fixture(&relocated, &project);

    assert_fixture_session_landed(&home, &project).await;
}
