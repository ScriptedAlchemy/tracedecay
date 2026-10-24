//! A rollout's scope boundary resolves git identities off the polling worker,
//! once per path across every capture window of the rollout.

use std::sync::Mutex;
use std::thread::ThreadId;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity_cli_first,
};

use super::*;
use crate::admission::test_support::MemoryHostAdmission;
use crate::runtime::jsonl_observation_admission::{MAX_CAPTURE_WINDOW, MAX_CAPTURE_WINDOW_BYTES};
use crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;

static RESOLUTIONS: Mutex<Vec<(PathBuf, ThreadId)>> = Mutex::new(Vec::new());

fn recording_identity(path: &Path) -> GitRepositoryIdentityOutcome {
    RESOLUTIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((path.to_path_buf(), std::thread::current().id()));
    discover_repository_identity_cli_first(path)
}

fn write_rollout(path: &Path, cwd: &Path, messages: usize) -> u64 {
    let session_id = "scope-resolution-session";
    let mut lines = vec![json!({
        "timestamp": "2026-09-04T12:00:00.000Z",
        "type": "session_meta",
        "payload": {"id": session_id, "cwd": cwd}
    })];
    lines.extend((0..messages).map(|ordinal| {
        json!({
            "timestamp": "2026-09-04T12:00:01.004Z",
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "thread_id": session_id,
                "turn_id": format!("turn-{ordinal}"),
                "item": {
                    "type": "UserMessage",
                    "id": format!("user-item-{ordinal}"),
                    "content": [{"type": "text", "text": format!("message {ordinal}")}]
                }
            }
        })
    }));
    let encoded = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(path, &encoded).unwrap();
    encoded.len() as u64
}

#[test]
fn rollout_windows_resolve_each_git_identity_once_off_the_polling_worker() {
    install_test_shared_jsonl_preparation_authority();
    let caller = std::thread::current().id();
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let project = home.join("project");
    let session_cwd = project.join("src");
    std::fs::create_dir_all(&session_cwd).unwrap();
    let status = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program().expect("git executable"),
    )
    .args(["init", "--quiet"])
    .current_dir(&project)
    .status()
    .unwrap();
    assert!(status.success(), "git init failed");
    let rollout = home.join("rollout.jsonl");
    let messages = MAX_CAPTURE_WINDOW * 2 + 10;
    let rollout_len = write_rollout(&rollout, &session_cwd, messages);

    let project_id = ProjectId::new("project.codex-scope-resolution").unwrap();
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    let matchers = ProjectRootMatcherCache::with_identity_resolver(recording_identity);
    let mut windows = 0;
    let mut consumed = 0;
    loop {
        let progress =
            tracedecay_runtime_core::native_runtime::block_on(try_admit_codex_jsonl_observations(
                &rollout,
                CodexObservationAdmission::Project {
                    root: &project,
                    project_id: project_id.clone(),
                },
                &admission,
                Some(MAX_CAPTURE_WINDOW_BYTES),
                Some(MAX_CAPTURE_WINDOW),
                &cancellation,
                &matchers,
            ))
            .expect("process runtime")
            .expect("codex window admission");
        windows += 1;
        consumed += progress.bytes_consumed;
        if !progress.source_deferred {
            break;
        }
        assert!(windows < 10, "rollout admission never converged");
    }

    assert_eq!(windows, 3, "the rollout spans three capture windows");
    assert_eq!(consumed, rollout_len);
    assert_eq!(
        admission.observations().len(),
        messages + 1,
        "every message and the session_meta record are admitted"
    );
    let resolutions = RESOLUTIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter(|(path, _)| path.starts_with(&home))
        .cloned()
        .collect::<Vec<_>>();
    let mut resolved_paths = resolutions
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    resolved_paths.sort();
    assert_eq!(
        resolved_paths,
        vec![project.clone(), session_cwd.clone()],
        "the root and the session cwd each resolve once across every window"
    );
    assert!(
        resolutions.iter().all(|(_, thread)| *thread != caller),
        "git identity resolution ran on the polling worker: {resolutions:?}"
    );
}
