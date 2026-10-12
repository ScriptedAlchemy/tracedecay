use std::path::Path;
use tracedecay_agent_hosts::hooks::{
    HookWorkspaceStatus, additional_context_json, codex_project_root_from_event,
    codex_user_prompt_submit_context_for_event, codex_workspace_status_from_event,
    cursor_project_root_from_event, cursor_session_start_json,
};
use tracedecay_runtime_core::config::ProfileRoot;
use tracedecay_runtime_core::storage::{pin_fixture_repository_identity, resolve_layout};

fn read_hook_analytics_events(root: &Path) -> Vec<serde_json::Value> {
    let path = root.join("hook_analytics.jsonl");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn analytics_contains(events: &[serde_json::Value], event: &str, category: Option<&str>) -> bool {
    events.iter().any(|item| {
        item["event"].as_str() == Some(event)
            && category.is_none_or(|category| item["category"].as_str() == Some(category))
    })
}

fn enroll_profile_project(project_root: &Path, project_id: &str) {
    pin_fixture_repository_identity(project_root, project_id).unwrap();
}

/// The composition root's hook runtime handle for `profile`, built explicitly
/// for each fixture. Hook and host behavior lives in `tracedecay-agent-hosts`,
/// which reaches registered project identity and the canonical store layout
/// only through this handle; no slot exists for a test binary to leave empty.
fn hook_runtime(
    profile: &ProfileRoot,
) -> tracedecay_agent_hosts::ports::hook_runtime::HookRuntimeV1 {
    tracedecay::hook_runtime(profile.clone())
}

/// A profile for marker-enrolled fixtures whose home sits beside, never at,
/// the fixture checkout.
fn fixture_profile(dir: &Path) -> ProfileRoot {
    ProfileRoot::under_home(dir.join("home"))
}

#[test]
fn test_cursor_project_root_uses_workspace_roots() {
    let dir = tempfile::tempdir().unwrap();
    enroll_profile_project(dir.path(), "proj_cursor_workspace_roots");
    let input = format!(
        r#"{{
            "hook_event_name": "beforeSubmitPrompt",
            "workspace_roots": [{}]
        }}"#,
        serde_json::to_string(dir.path().to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn test_cursor_project_root_uses_file_path_parent() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    enroll_profile_project(dir.path(), "proj_cursor_file_path");
    let file = src.join("lib.rs");
    let input = format!(
        r#"{{
            "hook_event_name": "afterFileEdit",
            "file_path": {}
        }}"#,
        serde_json::to_string(file.to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(dir.path().to_path_buf())
    );
}

#[test]
fn test_cursor_project_root_prefers_cwd_in_multi_root_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root-a");
    let root_b = dir.path().join("root-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    enroll_profile_project(&root_a, "proj_cursor_root_a");
    enroll_profile_project(&root_b, "proj_cursor_root_b");
    let cwd_b = root_b.join("src");
    std::fs::create_dir_all(&cwd_b).unwrap();

    let input = format!(
        r#"{{
            "hook_event_name": "beforeSubmitPrompt",
            "workspace_roots": [{}, {}],
            "cwd": {},
            "transcript_path": {}
        }}"#,
        serde_json::to_string(root_a.to_str().unwrap()).unwrap(),
        serde_json::to_string(root_b.to_str().unwrap()).unwrap(),
        serde_json::to_string(cwd_b.to_str().unwrap()).unwrap(),
        serde_json::to_string(root_b.join("agent-transcripts/s1.jsonl").to_str().unwrap()).unwrap()
    );

    assert_eq!(
        cursor_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(root_b)
    );
}

#[tokio::test]
async fn test_codex_user_prompt_submit_records_workspace_status_and_missing_session_hint() {
    let project = tempfile::tempdir().unwrap();
    let generic = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    enroll_profile_project(&project_root, "codex_prompt_analytics");
    let layout = resolve_layout(&project_root, profile.data_dir()).unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();

    let generic_event = serde_json::json!({
        "cwd": generic.path(),
        "session_id": "codex-generic-analytics",
        "prompt": "Who calls parse_user?"
    })
    .to_string();
    let generic_context =
        codex_user_prompt_submit_context_for_event(&hook_runtime(&profile), &generic_event).await;
    // Turn-local steering: a generic workspace still records its workspace
    // status but emits no prompt context.
    assert!(
        generic_context.is_empty(),
        "generic workspaces should emit no prompt steering: {generic_context}"
    );

    let prompt_event = serde_json::json!({
        "cwd": project_root,
        "prompt": "Please explain the impact of changing parse_user"
    })
    .to_string();
    let prompt_context =
        codex_user_prompt_submit_context_for_event(&hook_runtime(&profile), &prompt_event).await;
    assert!(prompt_context.contains("tracedecay hint:"));

    let profile_events = read_hook_analytics_events(&profile_root);
    assert!(profile_events.iter().any(|item| {
        item["event"].as_str() == Some("workspace_status")
            && item["workspace_status"].as_str() == Some("generic")
    }));

    let project_events = read_hook_analytics_events(&layout.data_root);
    assert!(project_events.iter().any(|item| {
        item["event"].as_str() == Some("workspace_status")
            && item["workspace_status"].as_str() == Some("initialized")
    }));
    assert!(analytics_contains(
        &project_events,
        "missing_session",
        Some("impact")
    ));
}

#[test]
fn test_codex_workspace_status_distinguishes_generic_and_project_like_dirs() {
    let profile_dir = tempfile::tempdir().unwrap();
    let profile = ProfileRoot::new(profile_dir.path());
    // Markers under the process temp root are refused so ephemeral agent
    // worktrees are not enrolled as projects. Keep the generic case in TMPDIR
    // (must stay Generic) and place project-like fixtures outside it.
    let generic = tempfile::tempdir().unwrap();
    let generic_event = serde_json::json!({ "cwd": generic.path() }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &generic_event),
        HookWorkspaceStatus::Generic
    );

    // The target directory usually sits inside this checkout, whose
    // repository may be an enrolled TraceDecay project; a fresh repository
    // bounds project discovery so the outcome never depends on the checkout.
    let outside_dir = tempfile::tempdir_in(crate::common::fixture::cargo_target_tmpdir())
        .expect("fixture root outside temp");
    let outside = outside_dir.path();
    gix::init(outside).expect("fixture repository boundary");

    let project_like = outside.join("cargo-marker");
    std::fs::create_dir_all(&project_like).unwrap();
    std::fs::write(project_like.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let project_event = serde_json::json!({ "cwd": project_like }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &project_event),
        HookWorkspaceStatus::UnindexedProject
    );

    let git_like = outside.join("git-marker");
    std::fs::create_dir_all(&git_like).unwrap();
    std::fs::create_dir(git_like.join(".git")).unwrap();
    let nested = git_like.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let git_event = serde_json::json!({ "cwd": nested }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &git_event),
        HookWorkspaceStatus::UnindexedProject
    );
}

#[test]
fn test_codex_workspace_status_detects_initialized_trace_decay_project() {
    let project = tempfile::tempdir().unwrap();
    let profile_dir = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile_dir.path().canonicalize().unwrap();
    let profile = ProfileRoot::new(&profile_root);
    enroll_profile_project(&project_root, "codex_workspace_status_initialized");

    let nested = project_root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let event = serde_json::json!({ "cwd": nested }).to_string();
    assert_eq!(
        codex_workspace_status_from_event(&profile, &event),
        HookWorkspaceStatus::Initialized
    );
}

#[test]
fn test_cursor_session_start_json_sets_context_and_env_root() {
    let dir = tempfile::tempdir().unwrap();
    let json = cursor_session_start_json(Some(dir.path()), "hello context");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["additional_context"], "hello context");
    assert_eq!(
        v["env"]["TRACEDECAY_PROJECT_ROOT"].as_str(),
        Some(dir.path().to_string_lossy().as_ref())
    );
}

#[test]
fn test_cursor_session_start_json_without_root_omits_env_path() {
    let json = cursor_session_start_json(None, "ctx");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["additional_context"], "ctx");
    assert!(v["env"].get("TRACEDECAY_PROJECT_ROOT").is_none());
}

// ---------------------------------------------------------------------------
// Codex hook handlers
// ---------------------------------------------------------------------------

#[test]
fn test_codex_session_start_context_uses_hook_specific_output() {
    assert_eq!(
        additional_context_json("SessionStart", "hello context"),
        r#"{"hookSpecificOutput":{"additionalContext":"hello context","hookEventName":"SessionStart"}}"#
    );
}

#[test]
fn test_codex_project_root_uses_cwd() {
    let dir = tempfile::tempdir().unwrap();
    enroll_profile_project(dir.path(), "proj_codex_cwd");
    let input = format!(
        r#"{{
            "hook_event_name": "PostToolUse",
            "cwd": {}
        }}"#,
        serde_json::to_string(dir.path().to_str().unwrap()).unwrap()
    );

    assert_eq!(
        codex_project_root_from_event(&fixture_profile(dir.path()), &input),
        Some(dir.path().to_path_buf())
    );
}
