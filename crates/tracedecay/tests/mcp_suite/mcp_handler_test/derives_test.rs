#![cfg(feature = "test-transport")]

//! `tracedecay_derives` through the production MCP `tools/call` path.
//!
//! The assertions are the text and JSON an agent receives. Occurrence ids are
//! the only runtime identity; every other field is a literal of the fixture.

use crate::support::{
    CaptureTransport, ProductionCompositionFixture, production_composition_fixture_with_sources,
    warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;
use tracedecay_mcp::McpTransport;

/// Lines are the fixture's source lines. `tracedecay_derives` reports the
/// item line (1-based), not the attribute line above it.
fn fixture_source() -> String {
    [
        "mod inner {",
        "    #[derive(Debug, Clone)]",
        "    pub enum Status {",
        "        Ready,",
        "    }",
        "}",
        "",
        "#[derive(serde::Serialize)]",
        "#[derive(Eq, CustomDerive)]",
        "pub struct NamedValue {",
        "    pub id: u32,",
        "}",
        "",
        "pub struct PlainValue;",
        "",
    ]
    .join("\n")
}

#[tokio::test]
async fn derives_reports_exact_attached_macro_names() {
    let fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), fixture_source()).unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("derives fixture server");
    warm_code_index_search(&server, "NamedValue").await;
    drop(server);

    let named = derive_symbol(
        &fixture,
        json!({"qualified_name": "src/lib.rs::NamedValue", "format": "json"}),
    )
    .await;
    assert_eq!(
        without_node_id(&named),
        json!({
            "name": "NamedValue",
            "qualified_name": "src/lib.rs::NamedValue",
            "kind": "struct",
            "file": "src/lib.rs",
            "line": 10,
            "derives": [
                {
                    "name": "CustomDerive",
                    "evidence_class": "syntax_exact",
                    "unavailable_fields": ["generated_trait_impl", "generated_methods"]
                },
                {
                    "name": "Eq",
                    "evidence_class": "syntax_exact",
                    "unavailable_fields": ["generated_trait_impl", "generated_methods"]
                },
                {
                    "name": "serde::Serialize",
                    "evidence_class": "syntax_exact",
                    "unavailable_fields": ["generated_trait_impl", "generated_methods"]
                }
            ]
        })
    );
    let named_id = node_id(&named);
    assert_eq!(
        derive_symbol(&fixture, json!({"node_id": named_id, "format": "json"})).await,
        named,
        "node_id lookup must return the same record as the qualified name"
    );
    assert_eq!(
        derive_symbol(
            &fixture,
            json!({"node_id": format!("code-symbol:{named_id}"), "format": "json"})
        )
        .await,
        named,
        "search evidence anchors unwrap to the same symbol"
    );
    assert_eq!(
        derive_symbol(
            &fixture,
            json!({
                "node_id": named_id,
                "qualified_name": "src/lib.rs::PlainValue",
                "format": "json"
            })
        )
        .await,
        named,
        "node_id wins when both selectors are present"
    );
    assert_eq!(
        call_text(
            &fixture,
            json!({"node_id": named_id, "format": "markdown"})
        )
        .await,
        format!(
            "- **NamedValue**\n  **kind:** struct\n  **file:** src/lib.rs\n  **line:** 10\n  **derives:** CustomDerive; Eq; serde::Serialize\n  **node_id:** `{named_id}`\n  **qualified_name:** `src/lib.rs::NamedValue`\n"
        )
    );
    assert_eq!(
        call_text(&fixture, json!({"node_id": named_id})).await,
        format!(
            "- **NamedValue**\n  **kind:** struct\n  **file:** src/lib.rs\n  **line:** 10\n  **derives:** CustomDerive; Eq; serde::Serialize\n  **node_id:** `{named_id}`\n  **qualified_name:** `src/lib.rs::NamedValue`\n"
        ),
        "omitted format is markdown, not JSON"
    );

    let status = derive_symbol(
        &fixture,
        json!({"qualified_name": "src/lib.rs::inner::Status", "format": "json"}),
    )
    .await;
    assert_eq!(
        without_node_id(&status),
        json!({
            "name": "Status",
            "qualified_name": "src/lib.rs::inner::Status",
            "kind": "enum",
            "file": "src/lib.rs",
            "line": 3,
            "derives": [
                {
                    "name": "Clone",
                    "evidence_class": "syntax_exact",
                    "unavailable_fields": ["generated_trait_impl", "generated_methods"]
                },
                {
                    "name": "Debug",
                    "evidence_class": "syntax_exact",
                    "unavailable_fields": ["generated_trait_impl", "generated_methods"]
                }
            ]
        })
    );

    let plain = derive_symbol(
        &fixture,
        json!({"qualified_name": "src/lib.rs::PlainValue", "format": "json"}),
    )
    .await;
    assert_eq!(
        without_node_id(&plain),
        json!({
            "name": "PlainValue",
            "qualified_name": "src/lib.rs::PlainValue",
            "kind": "struct",
            "file": "src/lib.rs",
            "line": 14,
            "derives": []
        })
    );

    let module = derive_symbol(
        &fixture,
        json!({"qualified_name": "src/lib.rs::inner", "format": "json"}),
    )
    .await;
    assert_eq!(
        without_node_id(&module),
        json!({
            "name": "inner",
            "qualified_name": "src/lib.rs::inner",
            "kind": "module",
            "file": "src/lib.rs",
            "line": 1,
            "derives": []
        })
    );

    assert_eq!(
        call_text(
            &fixture,
            json!({"qualified_name": "NamedValue", "format": "json"})
        )
        .await,
        "No matching symbol found.",
        "short names are not qualified-name matches"
    );
    assert_eq!(
        call_text(
            &fixture,
            json!({"qualified_name": "crate::does::not::exist", "format": "json"})
        )
        .await,
        "No matching symbol found."
    );
    assert_eq!(
        call_text(
            &fixture,
            json!({"node_id": "missing-symbol", "format": "json"})
        )
        .await,
        "No matching symbol found."
    );

    assert_eq!(
        call_error(&fixture, json!({})).await,
        json!({
            "code": -32602,
            "message": "missing required parameter: qualified_name or node_id",
            "data": {
                "tool": "tracedecay_derives",
                "reason_code": "missing_required_parameter",
                "retryable": false,
                "detail": "missing required parameter: qualified_name or node_id"
            }
        })
    );
    let empty_id = call_error(&fixture, json!({"node_id": ""})).await;
    assert_eq!(empty_id["code"], -32603);
    assert_eq!(
        empty_id["message"],
        "tool execution failed: config error: invalid parameter: node_id must not be empty"
    );
    assert_eq!(empty_id["data"]["tool"], "tracedecay_derives");
    let evidence_anchor = call_error(&fixture, json!({"node_id": "code-file:not-a-symbol"})).await;
    assert_eq!(evidence_anchor["code"], -32603);
    assert_eq!(
        evidence_anchor["message"],
        "tool execution failed: config error: invalid parameter: node_id `code-file:not-a-symbol` is an evidence anchor, not a graph symbol occurrence"
    );
    assert_eq!(evidence_anchor["data"]["tool"], "tracedecay_derives");

    fixture.harness.shutdown().await;
}

async fn derive_symbol(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let text = call_text(fixture, arguments).await;
    let payload: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("tracedecay_derives JSON: {error}\n{text}")
    });
    let items = payload
        .as_array()
        .unwrap_or_else(|| panic!("tracedecay_derives must return a symbol array: {payload}"));
    assert_eq!(items.len(), 1, "{payload}");
    items[0].clone()
}

fn without_node_id(symbol: &Value) -> Value {
    let mut symbol = symbol.clone();
    let node_id = symbol
        .as_object_mut()
        .expect("symbol record is a JSON object")
        .remove("node_id");
    assert!(
        node_id
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "node_id must be a non-empty occurrence id, got {node_id:?}"
    );
    symbol
}

fn node_id(symbol: &Value) -> String {
    symbol["node_id"]
        .as_str()
        .unwrap_or_else(|| panic!("node_id missing: {symbol}"))
        .to_owned()
}

async fn call_text(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    match call_derives(fixture, arguments).await {
        Ok(result) => result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("MCP text content missing: {result}"))
            .to_owned(),
        Err(error) => panic!("expected a tool result, got JSON-RPC error: {error}"),
    }
}

async fn call_error(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    match call_derives(fixture, arguments).await {
        Ok(result) => panic!("expected a JSON-RPC error, got: {result}"),
        Err(error) => error,
    }
}

async fn call_derives(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
) -> Result<Value, Value> {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("derives fixture server");
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_derives",
            "arguments": arguments,
        }
    });
    let mut transport = CaptureTransport {
        incoming: Some(request.to_string()),
        output: String::new(),
    };
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("real MCP server tool call");
    let response: Value =
        serde_json::from_str(transport.output.trim()).expect("JSON-RPC response");
    if !response["error"].is_null() {
        return Err(response["error"].clone());
    }
    Ok(response["result"].clone())
}
