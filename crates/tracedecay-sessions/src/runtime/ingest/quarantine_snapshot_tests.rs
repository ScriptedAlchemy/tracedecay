use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::json;
use tracedecay_domain::{ObservationScopeV1, ProjectId};
use tracedecay_runtime_core::config::ProfileRoot;

use crate::admission::test_support::MemoryHostAdmission;
use crate::host_ports::{kiro_data_dir, vscode_data_dir};
use crate::observation::ObservationCancellation;
use crate::runtime::SessionProvider;
use crate::runtime::hosts::codex::CodexDiscoveryHub;
use crate::runtime::ingest::project_provider::ProjectProviderRun;
use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
use crate::runtime::source::spin_until_jsonl_change_settled;

use super::project::with_transcript_source_profile;
use super::quarantine_contract::{assert_refused_source_contract, refuse_session};

pub(super) fn write_settled(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let staged = path.with_extension("staged");
    std::fs::write(&staged, body).unwrap();
    std::fs::rename(&staged, path).unwrap();
    spin_until_jsonl_change_settled(path);
}

pub(super) fn settle(path: &Path) {
    spin_until_jsonl_change_settled(path);
}

/// A canonical temp home holding an empty `project` directory.
pub(super) fn home_with_project() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    (temp, home, project)
}

/// Drives the real project provider run for `candidate` through the
/// refused-source contract, refusing every capture for `refused`.
pub(super) async fn provider_keeps_refused_source_contract(
    candidate: SessionProvider,
    home: &Path,
    project: &Path,
    refused: &'static str,
    sibling: &str,
    change_refused_source: impl FnOnce(),
) {
    install_test_shared_jsonl_preparation_authority();
    let hub = CodexDiscoveryHub::default();
    hub.register("project", Some(home));
    let project_id = ProjectId::new(format!("project-{}-refused-source", candidate.id())).unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let admission = MemoryHostAdmission::default();
    refuse_session(&admission, refused);
    let cancellation = ObservationCancellation::default();

    assert_refused_source_contract(
        &admission,
        sibling,
        || {
            let run = ProjectProviderRun {
                project_root: project,
                project_id: &project_id,
                facade: &admission,
                scope: &scope,
                candidate,
                max_new_bytes: u64::MAX,
                cancellation: &cancellation,
                codex_discovery: Some((&hub, "project")),
            };
            with_transcript_source_profile(
                ProfileRoot::under_home(home.to_path_buf()),
                async move { run.run().await.outcome },
            )
        },
        change_refused_source,
    )
    .await;
}

const KIRO_WORKSPACE_HASH: &str = "0123456789abcdef0123456789abcdef";

fn kiro_chat(message: &str) -> String {
    json!({"chat": [{"role": "human", "content": message}]}).to_string()
}

#[tokio::test]
async fn kiro_keeps_the_refused_source_contract() {
    let (_temp, home, project) = home_with_project();
    let data_dir = kiro_data_dir(&home);
    let workspace_dir = data_dir
        .join("User/globalStorage/kiro.kiroagent")
        .join(KIRO_WORKSPACE_HASH);
    write_settled(
        &data_dir
            .join("User/workspaceStorage")
            .join(KIRO_WORKSPACE_HASH)
            .join("workspace.json"),
        &json!({"folder": format!("file://{}", project.display())}).to_string(),
    );
    // Snapshots are swept in path order, so the refused one comes first; with
    // no id key the session id is the file stem.
    let refused = workspace_dir.join("a-refused.chat");
    write_settled(&refused, &kiro_chat("first turn"));
    write_settled(
        &workspace_dir.join("b-healthy.chat"),
        &kiro_chat("first turn"),
    );
    settle(&workspace_dir);

    provider_keeps_refused_source_contract(
        SessionProvider::Kiro,
        &home,
        &project,
        "a-refused",
        "b-healthy",
        || {
            write_settled(&refused, &kiro_chat("rewritten turn"));
            settle(&workspace_dir);
        },
    )
    .await;
}

fn cline_like_tasks_root(home: &Path, candidate: SessionProvider) -> PathBuf {
    let extension = match candidate {
        SessionProvider::Cline => "saoudrizwan.claude-dev",
        SessionProvider::RooCode => "rooveterinaryinc.roo-cline",
        SessionProvider::Kilo => "kilocode.kilo-code",
        other => panic!("{} is not a Cline-family provider", other.id()),
    };
    vscode_data_dir(home)
        .join("User")
        .join("globalStorage")
        .join(extension)
        .join("tasks")
}

fn cline_ui_messages(tokens_in: u64) -> String {
    json!([{
        "type": "say",
        "say": "api_req_started",
        "ts": 1_800_000_005,
        "text": json!({"tokensIn": tokens_in, "tokensOut": 350}).to_string()
    }])
    .to_string()
}

fn write_cline_task(task: &Path, project: &Path) {
    write_settled(
        &task.join("task_metadata.json"),
        &json!({"cwd": project}).to_string(),
    );
    write_settled(
        &task.join("api_conversation_history.json"),
        &json!([{"role": "user", "content": "first turn"}]).to_string(),
    );
    write_settled(&task.join("ui_messages.json"), &cline_ui_messages(1_200));
}

/// Task directories are swept newest mtime first, so the healthy task is
/// backdated behind the refused one. The refused task's change touches only
/// its `ui_messages.json`, which must be enough to release the quarantine.
async fn cline_like_keeps_the_refused_source_contract(candidate: SessionProvider) {
    let (_temp, home, project) = home_with_project();
    let tasks = cline_like_tasks_root(&home, candidate);
    let refused = tasks.join("refused-task");
    let healthy = tasks.join("healthy-task");
    write_cline_task(&healthy, &project);
    write_cline_task(&refused, &project);
    filetime::set_file_mtime(
        &healthy,
        filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(3_600)),
    )
    .unwrap();
    settle(&healthy);
    settle(&refused);

    provider_keeps_refused_source_contract(
        candidate,
        &home,
        &project,
        "refused-task",
        "healthy-task",
        || {
            write_settled(&refused.join("ui_messages.json"), &cline_ui_messages(2_400));
            settle(&refused);
        },
    )
    .await;
}

#[tokio::test]
async fn cline_keeps_the_refused_source_contract() {
    cline_like_keeps_the_refused_source_contract(SessionProvider::Cline).await;
}

#[tokio::test]
async fn roo_code_keeps_the_refused_source_contract() {
    cline_like_keeps_the_refused_source_contract(SessionProvider::RooCode).await;
}

#[tokio::test]
async fn kilo_keeps_the_refused_source_contract() {
    cline_like_keeps_the_refused_source_contract(SessionProvider::Kilo).await;
}
