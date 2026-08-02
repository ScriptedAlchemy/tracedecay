use serde_json::json;

use crate::daemon::session_temporal_refresh_scheduler::SessionTemporalRefreshWake;
use crate::mcp::tools::ToolResult;

fn dispatch_control(tool_name: &str) -> crate::mcp::tools::McpToolDispatchControl {
    let policy = crate::mcp::tools::execution_policy_for_tool(tool_name).unwrap();
    let cancellation = tracedecay_application::CancellationSignal::active(format!(
        "projectless-refresh-test.{tool_name}"
    ))
    .unwrap();
    crate::mcp::tools::McpToolDispatchControl::new(tool_name, policy, cancellation).unwrap()
}

#[tokio::test]
async fn projectless_lcm_preflight_reports_refresh_publication_failure() {
    let dispatch_control = dispatch_control("tracedecay_lcm_preflight");
    let response = super::super::projectless::complete_projectless_user_lcm_tool(
        json!(17),
        "tracedecay_lcm_preflight",
        &json!({"storage_scope": "user", "transcript_projection": true}),
        ToolResult::new(json!({"content": []}), Vec::new()),
        &SessionTemporalRefreshWake::unavailable(),
        &dispatch_control,
    )
    .await;

    let error = response.error.expect("refresh failure must fail the tool");
    let data = error.data.expect("refresh failure must retain typed data");
    assert_eq!(data["code"], "lcm_retrieval_service_unavailable");
    assert_eq!(data["reason"], "temporal_refresh_unavailable");
    assert_eq!(data["retryable"], true);
}

#[tokio::test]
async fn projectless_nonprojection_lcm_calls_remain_nonblocking_successes() {
    let dispatch_control = dispatch_control("tracedecay_lcm_compress");
    let response = super::super::projectless::complete_projectless_user_lcm_tool(
        json!(18),
        "tracedecay_lcm_compress",
        &json!({"storage_scope": "user"}),
        ToolResult::new(json!({"content": []}), Vec::new()),
        &SessionTemporalRefreshWake::unavailable(),
        &dispatch_control,
    )
    .await;

    assert!(response.error.is_none());
    assert!(response.result.is_some());
}

#[tokio::test]
async fn projectless_hook_uses_admitted_profile_scope_when_raw_scope_is_omitted() {
    let dispatch_control = dispatch_control("tracedecay_hook_runtime");
    let response = super::super::projectless::complete_projectless_hook_runtime_tool(
        json!(19),
        "tracedecay_hook_runtime",
        &json!({"action": "ingest_transcript"}),
        ToolResult::new(json!({"content": []}), Vec::new()),
        &SessionTemporalRefreshWake::unavailable(),
        &dispatch_control,
    )
    .await;

    let error = response
        .error
        .expect("projectless hook must join the profile refresh authority");
    let data = error.data.expect("refresh failure must retain typed data");
    assert_eq!(data["reason"], "temporal_refresh_unavailable");
    assert_eq!(data["retryable"], true);
}
