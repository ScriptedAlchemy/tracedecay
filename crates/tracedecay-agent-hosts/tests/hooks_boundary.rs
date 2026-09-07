use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};
use tracedecay_agent_hosts::hooks::{
    HookWorkspaceStatus, build_codex_session_context_for_workspace, codex_apply_patch_rel_paths,
    cursor_session_start_json, native_capture_material,
};
use tracedecay_agent_hosts::ports::hook_runtime;
use tracedecay_application::ResolvedScope;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_domain::{ProjectId, UtcMicros};
use tracedecay_hooks::{
    DaemonHookEvent, HookHostV1, NativeHookCaptureSourceV1, NativeHookDecodeError,
};
use tracedecay_runtime_core::storage::StoreLayout;

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
fn cursor_session_behavior_preserves_workspace_identity() {
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

/// Two independently constructed handles coexist in one process and each
/// answers only for itself: there is no slot to win, so the fixture that
/// built the second handle cannot inherit the first one's capabilities.
#[tokio::test]
async fn two_hook_runtime_handles_coexist_without_first_registration_wins() {
    fn runtime(
        timing_gate: hook_runtime::HookTimingGate,
        daemon_tool: hook_runtime::DaemonToolInvoker,
    ) -> hook_runtime::HookRuntimeV1 {
        fn project_root(_: &Path) -> Pin<Box<dyn Future<Output = Option<PathBuf>> + Send + '_>> {
            Box::pin(async { None })
        }
        fn scope(_: &Path, _: &ProjectId) -> Result<ResolvedScope, String> {
            Err("fixture has no scope resolver".to_owned())
        }
        fn notify(_: &Path, _: DaemonHookEvent) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
            Box::pin(async {})
        }
        fn initialized(_: &Path) -> bool {
            false
        }
        fn layout(
            _: &Path,
        ) -> Pin<Box<dyn Future<Output = tracedecay_domain::errors::Result<StoreLayout>> + Send + '_>>
        {
            Box::pin(async {
                Err(TraceDecayError::Config {
                    message: "fixture has no store layout".to_owned(),
                })
            })
        }
        hook_runtime::HookRuntimeV1 {
            daemon_tool,
            project_root_resolver: project_root,
            scope_resolver: scope,
            event_notifier: notify,
            timing_gate,
            project_initialization_gate: initialized,
            store_layout_resolver: layout,
        }
    }
    fn timings_on(_: &Path) -> Option<bool> {
        Some(true)
    }
    fn timings_off(_: &Path) -> Option<bool> {
        Some(false)
    }
    fn tool_first<'a>(
        _: Option<&'a Path>,
        _: &'a str,
        _: Value,
        _: bool,
    ) -> Pin<Box<dyn Future<Output = tracedecay_domain::errors::Result<Value>> + Send + 'a>> {
        Box::pin(async { Ok(json!({ "handle": "first" })) })
    }
    fn tool_second<'a>(
        _: Option<&'a Path>,
        _: &'a str,
        _: Value,
        _: bool,
    ) -> Pin<Box<dyn Future<Output = tracedecay_domain::errors::Result<Value>> + Send + 'a>> {
        Box::pin(async {
            Err(TraceDecayError::Config {
                message: "second handle has no daemon".to_owned(),
            })
        })
    }

    let first = runtime(timings_on, tool_first);
    let second = runtime(timings_off, tool_second);
    let root = Path::new("/workspace/project");

    assert_eq!(first.hook_timings_enabled(root), Some(true));
    assert_eq!(second.hook_timings_enabled(root), Some(false));
    assert_eq!(
        first
            .daemon_tool_json(None, "tracedecay_status", json!({}), false)
            .await
            .expect("first handle's daemon answers"),
        json!({ "handle": "first" })
    );
    let error = second
        .daemon_tool_json(None, "tracedecay_status", json!({}), false)
        .await
        .expect_err("second handle must not borrow the first handle's daemon");
    assert!(error.to_string().contains("second handle has no daemon"));
}
