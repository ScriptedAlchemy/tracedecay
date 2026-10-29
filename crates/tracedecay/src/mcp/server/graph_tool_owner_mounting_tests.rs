//! The core owner project open publishes before the session stores mount:
//! a call whose tool declares the project session stores answers the typed
//! `mounting` problem, while a graph-only call is answered.

use std::sync::Arc;

use serde_json::{Value, json};

use super::McpServer;
use tracedecay_code_index_runtime::CodeIndexSchedulerRegistryV1;
use tracedecay_code_index_runtime::project_reads::project_code_graph_projection_read_port;
use tracedecay_daemon_service::DaemonCodeGraphReadAdmission;
use tracedecay_graph_query::admitted_verified_graph_query_port_with_source;
use tracedecay_mcp::transport::JsonRpcRequest;

async fn call_tool(server: &McpServer, name: &str, arguments: Value) -> Value {
    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "tools/call".to_string(),
        params: Some(json!({"name": name, "arguments": arguments})),
    };
    let response = server
        .handle_request(&request)
        .await
        .expect("tool call should produce a response");
    response
        .result
        .unwrap_or_else(|| panic!("{name} JSON-RPC error: {:?}", response.error))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_store_tool_on_the_core_owner_is_mounting_while_graph_tools_answer() {
    let (mut context, dir, _profile) =
        crate::test_support::host_admission::registered_git_project_context_for_test(
            "project.core-owner",
        )
        .await;
    // Core composition publishes real graph authority before the session
    // stores mount. An unseated scheduler represents an unavailable index.
    let scope = context
        .admitted_project_scope
        .clone()
        .expect("registered project scope");
    let graph_admission: super::CodeGraphReadAdmissionPort =
        Arc::new(DaemonCodeGraphReadAdmission::production(
            context.cg.project_root().to_path_buf(),
            scope.clone(),
            Arc::clone(context.cg.configuration_runtime()),
        ));
    let graph_projection = project_code_graph_projection_read_port(
        CodeIndexSchedulerRegistryV1::new(1),
        context.cg.project_root().to_path_buf(),
        scope,
    );
    let graph_query = admitted_verified_graph_query_port_with_source(
        Arc::clone(&graph_admission),
        Arc::clone(&graph_projection),
        context.cg.source_read_context(),
    );
    context = context
        .with_code_graph_read_admission_port(graph_admission)
        .with_code_graph_projection_read_port(graph_projection)
        .with_verified_graph_query_port(graph_query);
    // The core owner serves before the project session store mounts.
    context.project_session_db = None;
    let server =
        crate::daemon::retained_test_support::mcp_server_with_project_retained_owner_for_test(
            context,
        )
        .await
        .expect("core-owner MCP test server");

    let pr_context = call_tool(
        &server,
        "tracedecay_pr_context",
        json!({"view": "symbols", "base_ref": "main", "head_ref": "HEAD", "cursor": "continuation"}),
    )
    .await;
    assert_eq!(
        pr_context["structuredContent"]["problem"]["code"],
        json!("application.runtime.mounting"),
        "{pr_context}"
    );

    let brief = call_tool(
        &server,
        "tracedecay_pr_context",
        json!({"base_ref": "HEAD", "head_ref": "HEAD", "format": "json"}),
    )
    .await;
    let text = brief["content"][0]["text"]
        .as_str()
        .expect("brief tool text");
    let served: tracedecay_contracts::retrieval::PrContextBriefV1 =
        serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {brief}"));
    let head = std::process::Command::new(
        tracedecay_runtime_core::git::try_git_program().expect("git executable"),
    )
    .args(["rev-parse", "HEAD"])
    .current_dir(dir.path())
    .output()
    .expect("fixture HEAD");
    assert!(head.status.success());
    assert_eq!(
        served.head_oid,
        String::from_utf8(head.stdout).unwrap().trim()
    );
    assert_eq!(served.base_oid, served.head_oid);
    assert_eq!(served.files_changed, 0);
    assert_eq!(
        served.source_analysis.state,
        tracedecay_contracts::retrieval::PrContextSourceStateV1::Ready
    );
    assert_eq!(
        served.graph_analysis.state,
        tracedecay_contracts::retrieval::PrContextSourceStateV1::Unavailable
    );
    assert!(served.freshness.is_none());

    let active = call_tool(
        &server,
        "tracedecay_active_project",
        json!({"format": "json"}),
    )
    .await;
    let text = active["content"][0]["text"].as_str().expect("tool text");
    let answer: Value = serde_json::from_str(text).expect("active project JSON");
    assert_eq!(
        answer["project_id"],
        json!("project.core-owner"),
        "{answer}"
    );
    // The reported root is spelled for the host (plain vs verbatim, 8.3
    // aliases); assert it identifies the fixture dir, not a spelling.
    let reported_root = answer["project_root"]
        .as_str()
        .expect("project_root string");
    assert!(
        tracedecay_runtime_core::path_safety::same_canonical_path(
            std::path::Path::new(reported_root),
            dir.path()
        ),
        "{answer}"
    );
}
