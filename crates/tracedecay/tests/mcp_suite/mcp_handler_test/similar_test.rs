#![cfg(feature = "test-transport")]

//! `tracedecay_similar` as an MCP client sees it: one symbol occurrence in,
//! token-verified copy paths and typed denials out.

use crate::support::{
    handle_real_server_tool_call_raw, production_composition_fixture_with_sources,
    warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;
use tracedecay::mcp::McpServer;

const SHARED_BODY: &str = "
        let one = parse(input);
        let two = transform(one);
        let three = validate(two);
        let four = persist(three);
        finish(four, input, one, two, three);
    ";

const EXACT_COPY_PATHS: [&str; 3] = ["src/commented.rs", "src/exact.rs", "src/formatted.rs"];
const RENAME_COPY_PATHS: [&str; 4] = [
    "src/commented.rs",
    "src/exact.rs",
    "src/formatted.rs",
    "src/renamed.rs",
];

#[tokio::test]
async fn tracedecay_similar_reports_verified_copy_paths_and_typed_denials() {
    let mut fixture = production_composition_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/source.rs"),
            format!("pub fn source_copy(input: Input) {{ {SHARED_BODY} }}\n"),
        )
        .unwrap();
        fs::write(
            project.join("src/exact.rs"),
            format!("pub fn exact_copy(input: Input) {{ {SHARED_BODY} }}\n"),
        )
        .unwrap();
        fs::write(
            project.join("src/formatted.rs"),
            format!("pub fn formatted_copy(input: Input) {{\n{SHARED_BODY}\n}}\n"),
        )
        .unwrap();
        fs::write(
            project.join("src/commented.rs"),
            format!("pub fn commented_copy(input: Input) {{ /* same body */ {SHARED_BODY} }}\n"),
        )
        .unwrap();
        fs::write(
            project.join("src/renamed.rs"),
            "
                pub fn renamed_copy(value: Input) {
                    let first = parse(value);
                    let second = transform(first);
                    let third = validate(second);
                    let fourth = persist(third);
                    finish(fourth, value, first, second, third);
                }
            ",
        )
        .unwrap();
        fs::write(
            project.join("src/divergent.rs"),
            "
                pub fn divergent(input: Input) {
                    let one = parse(input);
                    let two = transform(one);
                    let three = reject(two);
                    let four = persist(three);
                    finish(four, input, one, two, three);
                }
            ",
        )
        .unwrap();
        fs::write(
            project.join("src/unique.rs"),
            "
                pub fn unique_ledger(seed: u32) -> u32 {
                    let alpha = mix(seed);
                    let beta = fold(alpha);
                    let gamma = seal(beta);
                    let delta = audit(gamma);
                    let epsilon = publish(delta);
                    let zeta = archive(epsilon);
                    report(zeta, seed, alpha, beta, gamma, delta, epsilon)
                }
            ",
        )
        .unwrap();
        fs::write(
            project.join("src/tiny.rs"),
            "pub fn tiny() -> u32 {\n    1\n}\n",
        )
        .unwrap();
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production similar server");
    warm_code_index_search(&server, "source_copy").await;
    let project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("fixture project identity");
    let repository_id =
        tracedecay_code_index_runtime::code_index_scheduler::identity::repository_id_for(
            &fixture.project_root,
        )
        .expect("fixture repository identity");
    let source = symbol_id(&server, "source_copy", "src/source.rs").await;
    let tiny = symbol_id(&server, "tiny", "src/tiny.rs").await;
    let unique = symbol_id(&server, "unique_ledger", "src/unique.rs").await;

    let full = similar_payload(
        &server,
        json!({
            "project_id": project_id,
            "repository_id": repository_id,
            "target": {
                "kind": "symbol_occurrence",
                "symbol_occurrence_id": source,
            },
            "match_classes": ["conservative_exact", "rename_normalized_exact"],
            "result_limit": 10,
            "work_limit": 20,
            "cursor": null,
        }),
    )
    .await;
    assert_eq!(full["source"]["path"], "src/source.rs");
    assert_eq!(full["source"]["symbol_occurrence_id"], source);
    assert_eq!(full["coverage"], json!({"status": "complete"}));
    assert_eq!(full["families"].as_array().map(Vec::len), Some(2));
    assert_eq!(full["families"][0]["match_class"], "conservative_exact");
    assert_eq!(full["families"][0]["member_count"], 3);
    assert_eq!(full["families"][0]["complete"], true);
    assert_eq!(full["families"][0]["next_cursor"], Value::Null);
    assert_eq!(family_paths(&full["families"][0]), EXACT_COPY_PATHS);
    assert_eq!(
        full["families"][1]["match_class"],
        "rename_normalized_exact"
    );
    assert_eq!(full["families"][1]["member_count"], 4);
    assert_eq!(full["families"][1]["complete"], true);
    assert_eq!(full["families"][1]["next_cursor"], Value::Null);
    assert_eq!(family_paths(&full["families"][1]), RENAME_COPY_PATHS);

    let mut seen_rename_paths = Vec::new();
    let mut cursor = Value::Null;
    for page_index in 0..8 {
        let target = if page_index == 0 {
            json!({
                "kind": "source_range",
                "path": full["source"]["path"],
                "span": full["source"]["body_span"],
            })
        } else {
            json!({
                "kind": "symbol_occurrence",
                "symbol_occurrence_id": source,
            })
        };
        let mut arguments = json!({
            "project_id": project_id,
            "repository_id": repository_id,
            "target": target,
            "match_classes": ["rename_normalized_exact"],
            "result_limit": 1,
            "work_limit": 20,
            "cursor": null,
        });
        if page_index > 0 {
            arguments["cursor"] = cursor;
        }
        let page = similar_payload(&server, arguments).await;
        assert_eq!(page["source"]["path"], "src/source.rs");
        assert_eq!(page["families"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            page["families"][0]["match_class"],
            "rename_normalized_exact"
        );
        assert_eq!(page["families"][0]["member_count"], 1);
        let paths = family_paths(&page["families"][0]);
        assert_eq!(paths.len(), 1, "{page}");
        seen_rename_paths.push(paths[0].to_owned());
        cursor = page["families"][0]["next_cursor"].clone();
        if cursor.is_null() {
            assert_eq!(page["families"][0]["complete"], true);
            assert_eq!(page["coverage"], json!({"status": "complete"}));
            break;
        }
        assert_eq!(page["families"][0]["complete"], false);
        assert_eq!(page["coverage"], json!({"status": "partial"}));
        assert!(
            cursor.as_str().is_some_and(|value| !value.is_empty()),
            "{page}"
        );
    }
    seen_rename_paths.sort();
    assert!(
        cursor.is_null(),
        "rename pages did not finish: {seen_rename_paths:?}"
    );
    assert_eq!(seen_rename_paths, RENAME_COPY_PATHS);

    let excluded = similar_payload(
        &server,
        json!({
            "project_id": project_id,
            "repository_id": repository_id,
            "target": {
                "kind": "symbol_occurrence",
                "symbol_occurrence_id": tiny,
            },
            "match_classes": ["conservative_exact", "rename_normalized_exact"],
            "result_limit": 10,
            "work_limit": 20,
            "cursor": null,
        }),
    )
    .await;
    assert_eq!(excluded["source"]["path"], "src/tiny.rs");
    assert_eq!(excluded["families"], json!([]));
    assert_eq!(
        excluded["coverage"],
        json!({"status": "excluded_too_small", "minimum_tokens": 30})
    );

    let alone = similar_payload(
        &server,
        json!({
            "project_id": project_id,
            "repository_id": repository_id,
            "target": {
                "kind": "symbol_occurrence",
                "symbol_occurrence_id": unique,
            },
            "match_classes": ["conservative_exact", "rename_normalized_exact"],
            "result_limit": 10,
            "work_limit": 20,
            "cursor": null,
        }),
    )
    .await;
    assert_eq!(alone["source"]["path"], "src/unique.rs");
    assert_eq!(alone["families"], json!([]));
    assert_eq!(alone["coverage"], json!({"status": "complete"}));

    let unauthorized = similar_call(
        &server,
        json!({
            "project_id": project_id,
            "repository_id": "repository.unauthorized",
            "target": {
                "kind": "symbol_occurrence",
                "symbol_occurrence_id": source,
            },
            "match_classes": ["conservative_exact"],
            "result_limit": 10,
            "work_limit": 20,
            "cursor": null,
        }),
    )
    .await;
    assert_similar_denial(
        &unauthorized,
        "the selected source is outside the authorized repository scope",
    );

    let missing = similar_call(
        &server,
        json!({
            "project_id": project_id,
            "repository_id": repository_id,
            "target": {
                "kind": "symbol_occurrence",
                "symbol_occurrence_id": "symbol.v1.does-not-exist",
            },
            "match_classes": ["conservative_exact"],
            "result_limit": 10,
            "work_limit": 20,
            "cursor": null,
        }),
    )
    .await;
    assert_similar_denial(
        &missing,
        "the selected source has no body in the verified clone index",
    );

    fixture.harness.shutdown().await;
}

async fn symbol_id(server: &McpServer, name: &str, file: &str) -> String {
    let response = handle_real_server_tool_call_raw(
        server,
        "tracedecay_find_exact_symbol",
        json!({"name": name, "limit": 20}),
    )
    .await;
    let payload = tool_text(&response);
    payload["matches"]
        .as_array()
        .and_then(|matches| {
            matches
                .iter()
                .find(|item| item["name"] == name && item["file"] == file)
        })
        .and_then(|item| item["id"].as_str())
        .unwrap_or_else(|| panic!("exact symbol {name} in {file} missing: {payload}"))
        .to_owned()
}

async fn similar_payload(server: &McpServer, arguments: Value) -> Value {
    let response = similar_call(server, arguments).await;
    assert!(
        response["error"].is_null(),
        "tracedecay_similar failed: {response}"
    );
    assert_eq!(response["result"]["content"][0]["type"], "text");
    tool_text(&response)
}

async fn similar_call(server: &McpServer, arguments: Value) -> Value {
    handle_real_server_tool_call_raw(server, "tracedecay_similar", arguments).await
}

fn tool_text(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP text content missing: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("MCP JSON {error}: {text}"))
}

fn family_paths(family: &Value) -> Vec<&str> {
    let members = family["members"]
        .as_array()
        .unwrap_or_else(|| panic!("similar family has no members: {family}"));
    let mut paths = members
        .iter()
        .map(|member| {
            member["path"]
                .as_str()
                .unwrap_or_else(|| panic!("similar member has no path: {member}"))
        })
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths
}

fn assert_similar_denial(response: &Value, detail: &str) {
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert_eq!(response["error"]["message"], detail, "{response}");
    assert_eq!(response["error"]["data"]["tool"], "tracedecay_similar");
    assert_eq!(
        response["error"]["data"]["reason_code"],
        "similar-source-not-found"
    );
    assert_eq!(response["error"]["data"]["retryable"], false);
    assert_eq!(response["error"]["data"]["detail"], detail);
    assert!(response["result"].is_null(), "{response}");
}
