use std::path::Path;

use tracedecay_agent_hosts::hooks::{
    HookWorkspaceStatus, build_codex_session_context_for_workspace, codex_apply_patch_rel_paths,
    cursor_session_start_json, cursor_should_run_sync, native_capture_material,
};
use tracedecay_domain::UtcMicros;
use tracedecay_hooks::{HookHostV1, NativeHookCaptureSourceV1, NativeHookDecodeError};

#[test]
fn codex_unindexed_workspace_context_preserves_tool_routing() {
    let context = build_codex_session_context_for_workspace(
        HookWorkspaceStatus::UnindexedProject,
        Some("last indexed 7m ago"),
    );

    assert!(context.contains("literal or regex text -> tracedecay_grep"));
    assert!(context.contains("symbol name -> tracedecay_search"));
    assert!(context.contains("concept -> tracedecay_context"));
    assert!(context.contains("tracedecay_diagnostics"));
}

#[test]
fn codex_apply_patch_paths_stay_inside_the_project() {
    let project_root = Path::new("/workspace/project");
    let command = "*** Begin Patch\n*** Update File: src/lib.rs\n*** Add File: ../secret.txt\n*** Move to: src/moved.rs\n*** End Patch";

    assert_eq!(
        codex_apply_patch_rel_paths(command, project_root, project_root),
        ["src/lib.rs", "src/moved.rs"]
    );
}

#[test]
fn cursor_session_behavior_preserves_debounce_and_workspace_identity() {
    assert!(cursor_should_run_sync(120, None, 30));
    assert!(!cursor_should_run_sync(120, Some(100), 30));
    assert!(cursor_should_run_sync(130, Some(100), 30));

    let response = cursor_session_start_json(Some(Path::new("/workspace/project")), "ready");
    let response: serde_json::Value = serde_json::from_str(&response).expect("valid hook response");
    assert_eq!(response["additional_context"], "ready");
    assert_eq!(
        response["env"]["TRACEDECAY_PROJECT_ROOT"],
        "/workspace/project"
    );
}

#[test]
fn native_identity_ignores_provider_content_but_preserves_typed_ids() {
    let first = br#"{
        "session_id":"session-1","turn_id":"turn-1","transcript_path":null,
        "cwd":"/workspace/one","hook_event_name":"Stop","model":"model",
        "permission_mode":"default","stop_hook_active":false,
        "last_assistant_message":"secret one"
    }"#;
    let second = br#"{
        "session_id":"session-1","turn_id":"turn-1","transcript_path":null,
        "cwd":"/workspace/two","hook_event_name":"Stop","model":"model",
        "permission_mode":"default","stop_hook_active":false,
        "last_assistant_message":"secret two"
    }"#;
    let source = NativeHookCaptureSourceV1::Host(HookHostV1::Codex);

    let first = native_capture_material(source, first, UtcMicros(42)).expect("first material");
    let second = native_capture_material(source, second, UtcMicros(42)).expect("second material");

    assert_eq!(first, second);
}

#[test]
fn installed_but_unsupported_events_remain_successful_noop_candidates() {
    let codex_subagent = br#"{
        "session_id":"session-1","turn_id":"turn-1",
        "cwd":"/workspace/project","hook_event_name":"SubagentStart"
    }"#;
    let cursor_session_end = br#"{
        "session_id":"session-1","conversation_id":"conversation-1",
        "cwd":"/workspace/project","hook_event_name":"sessionEnd"
    }"#;

    assert!(matches!(
        native_capture_material(
            NativeHookCaptureSourceV1::Host(HookHostV1::Codex),
            codex_subagent,
            UtcMicros(42),
        ),
        Err(NativeHookDecodeError::UnsupportedNativeEvent)
    ));
    assert!(matches!(
        native_capture_material(
            NativeHookCaptureSourceV1::Host(HookHostV1::CursorDesktop),
            cursor_session_end,
            UtcMicros(42),
        ),
        Err(NativeHookDecodeError::UnsupportedNativeEvent)
    ));
}

#[tokio::test]
async fn unregistered_root_ports_fail_closed() {
    let error = tracedecay_agent_hosts::ports::hook_runtime::daemon_tool_json(
        None,
        "tracedecay_status",
        serde_json::json!({}),
        false,
    )
    .await
    .expect_err("unregistered daemon port must not fabricate success");
    assert!(error.to_string().contains("no daemon tool invoker"));
    assert!(
        tracedecay_agent_hosts::ports::hook_runtime::resolve_project_root_with_identity(Path::new(
            "/workspace/project",
        ))
        .await
        .is_none()
    );
    assert!(
        tracedecay_agent_hosts::ports::hook_runtime::hook_timings_enabled(Path::new(
            "/workspace/project"
        ))
        .is_none()
    );
}
