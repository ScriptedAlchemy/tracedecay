use std::path::Path;

use serde_json::json;
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

fn write_session(home: &Path, project: &Path, session: &str, messages: &[&str]) {
    let dir = home.join(".claude/projects/-project");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let body = messages
        .iter()
        .enumerate()
        .map(|(index, content)| {
            format!(
                "{}\n",
                json!({
                    "type": "user",
                    "sessionId": session,
                    "uuid": format!("{session}-message-{index}"),
                    "timestamp": format!("2026-09-28T00:00:0{index}Z"),
                    "cwd": project,
                    "message": {"role": "user", "content": content}
                })
            )
        })
        .collect::<String>();
    let staged = path.with_extension("tmp");
    std::fs::write(&staged, body).unwrap();
    std::fs::rename(&staged, &path).unwrap();
    spin_until_jsonl_change_settled(&path);
}

#[tokio::test]
async fn claude_keeps_the_refused_source_contract() {
    install_test_shared_jsonl_preparation_authority();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();
    write_session(&home, &project, "refused", &["first turn"]);
    write_session(&home, &project, "sibling", &["first turn"]);
    let hub = CodexDiscoveryHub::default();
    hub.register("project", Some(&home));
    let project_id = ProjectId::new("project-claude-refused-source").unwrap();
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
                candidate: SessionProvider::Claude,
                max_new_bytes: u64::MAX,
                cancellation: &cancellation,
                codex_discovery: Some((&hub, "project")),
            };
            with_transcript_source_profile(ProfileRoot::under_home(home.clone()), async move {
                run.run().await.outcome
            })
        },
        || {
            write_session(&home, &project, "refused", &["first turn", "second turn"]);
        },
    )
    .await;
}
