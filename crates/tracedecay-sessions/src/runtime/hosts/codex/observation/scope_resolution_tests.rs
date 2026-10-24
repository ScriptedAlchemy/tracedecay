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
use crate::runtime::shared::ProjectMembership;

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

fn enroll_non_git_project(profile: &tracedecay_runtime_core::config::ProfileRoot, project: &Path) {
    let store = tracedecay_runtime_core::storage::profile_sharded_data_root(
        profile.data_dir(),
        &tracedecay_runtime_core::storage::path_local_profile_project_id(project),
    );
    std::fs::create_dir_all(&store).unwrap();
    rusqlite::Connection::open(store.join(tracedecay_runtime_core::config::DB_FILENAME))
        .unwrap()
        .execute_batch("PRAGMA user_version = 1;")
        .unwrap();
}

#[tokio::test]
async fn scope_cache_preserves_non_git_profile_authority_and_enrollment_changes() {
    install_test_shared_jsonl_preparation_authority();
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = root.join("project");
    let cwd = project.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    let profiles = [
        tracedecay_runtime_core::config::ProfileRoot::under_home(root.join("owner-a")),
        tracedecay_runtime_core::config::ProfileRoot::under_home(root.join("owner-b")),
    ];
    enroll_non_git_project(&profiles[0], &project);
    assert_eq!(
        profiles[0].discover_project_root(&cwd),
        Some(project.clone())
    );
    assert_eq!(profiles[1].discover_project_root(&cwd), None);
    let rollout = root.join("non-git-rollout.jsonl");
    write_rollout(&rollout, &cwd, 1);
    let project_id = ProjectId::new("project.codex-non-git-profile").unwrap();
    let cancellation = ObservationCancellation::default();
    let roots = vec![project.clone()];

    // One process cache serves both owners. Enrolling the second profile
    // changes no file or Git topology in the project itself.
    for (owner, enrolled) in [(0, true), (1, false), (1, true), (0, true)] {
        if owner == 1 && enrolled {
            enroll_non_git_project(&profiles[1], &project);
        }
        let project_admission = MemoryHostAdmission::default();
        let profile_admission = MemoryHostAdmission::default();
        crate::runtime::with_transcript_source_profile(profiles[owner].clone(), async {
            try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
                &rollout,
                &project,
                project_id.clone(),
                &project_admission,
                None,
                &cancellation,
            )
            .await
            .unwrap();
            try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation(
                &rollout,
                None,
                &roots,
                &profile_admission,
                None,
                &cancellation,
            )
            .await
            .unwrap();
        })
        .await;
        assert_eq!(
            (
                project_admission.observations().len(),
                profile_admission.observations().len(),
            ),
            if enrolled { (2, 0) } else { (0, 2) },
            "owner {owner}, enrolled {enrolled}: project and profile stores must partition the real rollout"
        );
    }
}

#[tokio::test]
async fn scope_cache_retains_captured_absence_across_nested_profile_scopes() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let project = root.join("project");
    let cwd = project.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    let owner = tracedecay_runtime_core::config::ProfileRoot::under_home(root.join("owner"));
    enroll_non_git_project(&owner, &project);
    let cache = ProjectRootMatcherCache::default();
    crate::runtime::with_transcript_source_profile(owner.clone(), async {
        let without_owner = with_transcript_source_profile_sync(None, || {
            assert_eq!(crate::runtime::transcript_source_profile(), None);
            cache.get(&project)
        });
        assert_eq!(
            crate::runtime::transcript_source_profile(),
            Some(owner.clone())
        );
        let with_owner = cache.get(&project);
        assert!(!Arc::ptr_eq(&without_owner, &with_owner));
        assert_eq!(
            without_owner.contains_status(&cwd),
            ProjectMembership::NoMatch
        );
        with_transcript_source_profile_sync(None, || {
            assert_eq!(with_owner.contains_status(&cwd), ProjectMembership::Match);
            with_transcript_source_profile_sync(Some(owner.clone()), || {
                assert_eq!(
                    crate::runtime::transcript_source_profile(),
                    Some(owner.clone())
                );
            });
            assert_eq!(crate::runtime::transcript_source_profile(), None);
        });
        assert_eq!(crate::runtime::transcript_source_profile(), Some(owner));
    })
    .await;
    assert_eq!(crate::runtime::transcript_source_profile(), None);
}

fn git_fixture(project: &Path, args: &[&str]) {
    let output =
        std::process::Command::new(tracedecay_runtime_core::git::try_git_program().unwrap())
            .args(args)
            .current_dir(project)
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn scope_cache_revalidates_a_worktree_replaced_by_an_unrelated_repository() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let projects = [root.join("project-a"), root.join("project-b")];
    for project in &projects {
        std::fs::create_dir_all(project).unwrap();
        git_fixture(project, &["init", "--quiet", "--initial-branch=main"]);
        git_fixture(
            project,
            &[
                "-c",
                "user.name=TraceDecay Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
    }
    let linked = root.join("linked");
    let linked_arg = linked.to_str().unwrap();
    git_fixture(
        &projects[0],
        &["worktree", "add", "--quiet", "-b", "linked", linked_arg],
    );
    let cwd = linked.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    let rollout = root.join("linked-rollout.jsonl");
    write_rollout(&rollout, &cwd, 1);
    let membership = || {
        projects.each_ref().map(|project| {
            crate::runtime::hosts::codex::codex_rollout_project_membership(&rollout, project)
        })
    };
    assert_eq!(
        membership(),
        [
            Some(ProjectMembership::Match),
            Some(ProjectMembership::NoMatch)
        ]
    );
    let cache = ProjectRootMatcherCache::default();
    let retained_linked_root = cache.get(&linked);
    assert_eq!(
        retained_linked_root.contains_status(&projects[0]),
        ProjectMembership::Match
    );
    git_fixture(&projects[0], &["worktree", "remove", "--force", linked_arg]);
    git_fixture(
        &projects[1],
        &["worktree", "add", "--quiet", "-b", "linked", linked_arg],
    );
    std::fs::create_dir_all(&cwd).unwrap();
    assert_eq!(
        membership(),
        [
            Some(ProjectMembership::NoMatch),
            Some(ProjectMembership::Match)
        ],
        "replacement must revoke the old project's match and admit the new project's previously excluded cwd"
    );
    assert_eq!(
        retained_linked_root.contains_status(&projects[0]),
        ProjectMembership::NoMatch
    );
    assert_eq!(
        cache.membership(&projects[0], &linked),
        ProjectMembership::NoMatch
    );
    assert_eq!(
        cache.membership(&projects[1], &linked),
        ProjectMembership::Match
    );
    git_fixture(&cwd, &["init", "--quiet", "--initial-branch=main"]);
    assert_eq!(
        membership(),
        [Some(ProjectMembership::NoMatch); 2],
        "a newly closer repository must replace the cached outer cwd identity"
    );
}

static CONTEXT_RESOLUTIONS: Mutex<Vec<(PathBuf, ThreadId)>> = Mutex::new(Vec::new());

fn recording_context_identity(path: &Path) -> GitRepositoryIdentityOutcome {
    CONTEXT_RESOLUTIONS
        .lock()
        .unwrap()
        .push((path.to_path_buf(), std::thread::current().id()));
    discover_repository_identity_cli_first(path)
}

#[test]
fn changed_context_scope_resolution_leaves_the_polling_worker() {
    install_test_shared_jsonl_preparation_authority();
    let caller = std::thread::current().id();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().canonicalize().unwrap().join("project");
    let initial = project.join("initial");
    let changed = project.join("changed");
    std::fs::create_dir_all(&initial).unwrap();
    std::fs::create_dir_all(&changed).unwrap();
    git_fixture(&project, &["init", "--quiet", "--initial-branch=main"]);
    let rollout = tmp.path().join("context-rollout.jsonl");
    write_rollout(&rollout, &initial, 1);
    let mut records = std::fs::read_to_string(&rollout).unwrap();
    records.push_str(
        &json!({"type":"turn_context", "payload":{"cwd":changed, "model":"context-model"}})
            .to_string(),
    );
    records.push('\n');
    records.push_str(&json!({"timestamp":"2026-09-04T12:00:02.004Z", "type":"event_msg", "payload":{
        "type":"item_completed", "thread_id":"scope-resolution-session", "turn_id":"after-context", "item":{
            "type":"UserMessage", "id":"after-context-item", "content":[{"type":"text","text":"after context"}]
        }
    }}).to_string());
    records.push('\n');
    std::fs::write(&rollout, records).unwrap();
    let admission = MemoryHostAdmission::default();
    let cache = ProjectRootMatcherCache::with_identity_resolver(recording_context_identity);
    let cancellation = ObservationCancellation::default();
    let progress =
        tracedecay_runtime_core::native_runtime::block_on(try_admit_codex_jsonl_observations(
            &rollout,
            CodexObservationAdmission::Project {
                root: &project,
                project_id: ProjectId::new("project.changed-context").unwrap(),
            },
            &admission,
            None,
            None,
            &cancellation,
            &cache,
        ))
        .unwrap()
        .unwrap();
    assert!(
        progress.frames_persisted >= 3,
        "real records must pass the admission boundary"
    );
    let resolutions = CONTEXT_RESOLUTIONS.lock().unwrap();
    assert!(
        resolutions.iter().any(|(path, _)| path == &changed),
        "the changed cwd must resolve"
    );
    assert!(
        resolutions.iter().all(|(_, thread)| *thread != caller),
        "all cwd resolution must leave polling worker: {resolutions:?}; caller={caller:?}"
    );
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
