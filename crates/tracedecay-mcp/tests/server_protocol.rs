use rmcp::model::{CallToolRequestParams, ClientCapabilities, Implementation};
use serde_json::json;
use tracedecay_mcp::JsonRpcRequest;
use tracedecay_mcp::server::{
    McpDispatchParams, McpDispatchRequest, McpMethod, classify_mcp_method,
    dispatch_is_independent_read, initialize_result, resources_list_result,
};

#[test]
fn protocol_owner_classifies_every_transport_method() {
    assert_eq!(classify_mcp_method("initialize"), McpMethod::Initialize);
    assert_eq!(classify_mcp_method("tools/list"), McpMethod::ToolsList);
    assert_eq!(classify_mcp_method("tools/call"), McpMethod::ToolsCall);
    assert_eq!(
        classify_mcp_method("resources/list"),
        McpMethod::ResourcesList
    );
    assert_eq!(
        classify_mcp_method("resources/read"),
        McpMethod::ResourcesRead
    );
    assert_eq!(
        classify_mcp_method("notifications/cancelled"),
        McpMethod::Cancelled
    );
    assert_eq!(
        classify_mcp_method(tracedecay_hooks::core_events::HOOK_EVENT_METHOD),
        McpMethod::HookEvent
    );
    assert_eq!(classify_mcp_method("unknown"), McpMethod::Unknown);
}

#[test]
fn initialize_payload_uses_composed_product_metadata() {
    let result = initialize_result("9.8.7", "transport instructions");

    assert_eq!(result["protocolVersion"], json!("2024-11-05"));
    assert_eq!(result["serverInfo"]["name"], json!("tracedecay"));
    assert_eq!(result["serverInfo"]["version"], json!("9.8.7"));
    assert_eq!(result["instructions"], json!("transport instructions"));
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
}

#[test]
fn typed_and_legacy_envelopes_share_read_classification() {
    let legacy = JsonRpcRequest {
        jsonrpc: "2.0".to_owned(),
        id: Some(json!(1)),
        method: "tools/call".to_owned(),
        params: Some(json!({
            "name": "tracedecay_search",
            "arguments": {"query": "server owner"}
        })),
    };
    let raw = McpDispatchRequest::from_legacy(&legacy);
    let typed = McpDispatchRequest::typed(
        json!(1),
        "tools/call",
        McpDispatchParams::ToolsCall(CallToolRequestParams::new("tracedecay_search")),
    );

    assert_eq!(raw.method_class(), typed.method_class());
    assert_eq!(raw.tool_name(), typed.tool_name());
    assert!(dispatch_is_independent_read(
        raw.method_class(),
        raw.tool_name(),
        |tool_name| tool_name == "tracedecay_search",
    ));
    assert!(dispatch_is_independent_read(
        typed.method_class(),
        typed.tool_name(),
        |tool_name| tool_name == "tracedecay_search",
    ));

    let initialize = rmcp::model::InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("cursor", "1"),
    );
    let typed_initialize = McpDispatchRequest::typed(
        json!("init"),
        "initialize",
        McpDispatchParams::Initialize(&initialize),
    );
    assert_eq!(typed_initialize.client_info_name(), Some("cursor"));
}

#[test]
fn resource_catalog_remains_transport_owned() {
    let resources = resources_list_result();
    let uris = resources["resources"]
        .as_array()
        .expect("resource catalog")
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        uris,
        [
            "tracedecay://status",
            "tracedecay://files",
            "tracedecay://overview",
            "tracedecay://branches",
            "tracedecay://schema",
        ]
    );
}
