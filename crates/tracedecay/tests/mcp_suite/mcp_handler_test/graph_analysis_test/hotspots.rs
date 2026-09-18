//! `tracedecay_hotspots` through production MCP `tools/call`.
//!
//! Occurrence ids are minted per project, so two equal totals may swap order
//! across runs. The host-visible ranking of distinct degrees, the line and
//! degree of each named symbol, the default page, the clamped page, and the
//! zero-limit rejection are stable and asserted literally.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{MountedProductionProject, close_test_graph, handle_tool_call, init_test_project};
use crate::support::test_temp_dir;

const CHAIN_SOURCE: &str = "\
export function quiet(): number {\n\
  return 0;\n\
}\n\
\n\
export function leaf(): number {\n\
  return 1;\n\
}\n\
\n\
export function mid(): number {\n\
  return leaf();\n\
}\n\
\n\
export function hub(): number {\n\
  return mid();\n\
}\n\
";

fn write_package(project: &Path, name: &str) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("package.json"),
        format!("{{\"name\":\"{name}\",\"private\":true,\"type\":\"module\"}}\n"),
    )
    .unwrap();
}

fn write_chain_project(project: &Path) {
    write_package(project, "hotspots-chain");
    fs::write(project.join("src/calls.ts"), CHAIN_SOURCE).unwrap();
}

/// `hub` plus 101 callers. Returns the source length the savings footer
/// measures for `src/fanout.ts`.
fn write_fanout_project(project: &Path) -> usize {
    write_package(project, "hotspots-fanout");
    let mut source = String::from("export function hub(): number { return 1; }\n");
    for index in 0..101 {
        source.push_str(&format!(
            "export function caller{index}(): number {{ return hub(); }}\n"
        ));
    }
    let bytes = source.len();
    fs::write(project.join("src/fanout.ts"), source).unwrap();
    bytes
}

async fn call_hotspots(host: &MountedProductionProject, arguments: Value) -> Value {
    let result = handle_tool_call(host, "tracedecay_hotspots", arguments, None, None)
        .await
        .unwrap_or_else(|error| panic!("tracedecay_hotspots failed over production MCP: {error}"));
    result.value
}

fn content(result: &Value) -> &[Value] {
    result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("hotspots content missing: {result}"))
}

fn body_text(result: &Value) -> &str {
    let item = &content(result)[0];
    assert_eq!(item["type"], "text", "{result}");
    item["text"]
        .as_str()
        .unwrap_or_else(|| panic!("hotspots text missing: {result}"))
}

fn parse_body(result: &Value) -> Value {
    let text = body_text(result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("hotspots JSON did not parse: {error}\n{text}"))
}

fn assert_savings_footer(result: &Value, source_bytes: usize) {
    let items = content(result);
    assert_eq!(items.len(), 2, "{result}");
    assert_eq!(items[1]["type"], "text", "{result}");
    let footer = items[1]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("hotspots footer missing: {result}"));
    assert_eq!(
        footer,
        format!(
            "\ntracedecay_metrics: before={} after={}",
            source_bytes / 4,
            body_text(result).len() / 4
        )
    );
}

fn assert_symbol_id(id: &str) {
    let prefix = "symbol.v1.sha256:";
    let Some(hex) = id.strip_prefix(prefix) else {
        panic!("hotspot id {id} is not a sealed symbol occurrence");
    };
    assert_eq!(hex.len(), 64, "{id}");
    assert!(
        hex.chars().all(|character| character.is_ascii_hexdigit()),
        "{id}"
    );
}

fn assert_exact_hotspot(
    row: &Value,
    name: &str,
    file: &str,
    line: u64,
    incoming: u64,
    outgoing: u64,
    total: u64,
) {
    let id = row["id"]
        .as_str()
        .unwrap_or_else(|| panic!("hotspot id missing: {row}"));
    assert_symbol_id(id);
    assert_eq!(
        row,
        &json!({
            "id": id,
            "name": name,
            "kind": "function",
            "file": file,
            "line": line,
            "incoming": incoming,
            "outgoing": outgoing,
            "total": total,
        }),
        "{row}"
    );
}

fn hotspots(payload: &Value) -> &[Value] {
    let rows = payload["hotspots"]
        .as_array()
        .unwrap_or_else(|| panic!("hotspots array missing: {payload}"));
    assert_eq!(
        payload["hotspot_count"].as_u64(),
        Some(u64::try_from(rows.len()).expect("hotspot count fits")),
        "{payload}"
    );
    rows
}

fn assert_chain_ranking(payload: &Value) {
    let rows = hotspots(payload);
    assert_eq!(rows.len(), 4, "{payload}");
    assert_exact_hotspot(&rows[0], "mid", "src/calls.ts", 9, 1, 1, 2);
    assert_exact_hotspot(&rows[3], "quiet", "src/calls.ts", 1, 0, 0, 0);
    let mut tied = [rows[1].clone(), rows[2].clone()];
    tied.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["name"].as_str().unwrap_or(""))
    });
    assert_exact_hotspot(&tied[0], "hub", "src/calls.ts", 13, 0, 1, 1);
    assert_exact_hotspot(&tied[1], "leaf", "src/calls.ts", 5, 1, 0, 1);
    assert!(
        rows.windows(2)
            .all(|pair| pair[0]["total"].as_u64() >= pair[1]["total"].as_u64()),
        "chain ranking is not highest degree first: {payload}"
    );
}

fn assert_fanout_page(payload: &Value, expected_count: usize) {
    let rows = hotspots(payload);
    assert_eq!(rows.len(), expected_count, "{payload}");
    assert_exact_hotspot(&rows[0], "hub", "src/fanout.ts", 1, 101, 0, 101);
    let mut seen = Vec::new();
    for row in rows.iter().skip(1) {
        let name = row["name"]
            .as_str()
            .unwrap_or_else(|| panic!("caller name missing: {row}"));
        let index: u64 = name
            .strip_prefix("caller")
            .unwrap_or_else(|| panic!("non-caller in the fan-out page: {row}"))
            .parse()
            .unwrap_or_else(|_| panic!("caller index missing: {row}"));
        assert!(index < 101, "caller outside the fixture: {row}");
        assert_exact_hotspot(row, name, "src/fanout.ts", index + 2, 0, 1, 1);
        seen.push(index);
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), expected_count - 1, "{payload}");
}

fn assert_clamped_truncation(payload: &Value) {
    assert_eq!(payload["truncated"], true, "{payload}");
    assert_eq!(payload["retrieve_tool"], "tracedecay_retrieve", "{payload}");
    assert_eq!(payload["retrieve_ttl_seconds"], 86_400, "{payload}");
    let preview_chars = payload["preview_chars"]
        .as_u64()
        .unwrap_or_else(|| panic!("preview_chars missing: {payload}"));
    assert_eq!(preview_chars, 11_928, "{payload}");
    let original_chars = payload["original_chars"]
        .as_u64()
        .unwrap_or_else(|| panic!("original_chars missing: {payload}"));
    assert!(
        original_chars > preview_chars,
        "clamped body must not fit in the preview: {payload}"
    );
    let preview = payload["preview"]
        .as_str()
        .unwrap_or_else(|| panic!("preview missing: {payload}"));
    assert_eq!(preview.chars().count() as u64, preview_chars, "{preview}");
    let marker = r#"{"hotspot_count":100,"hotspots":["#;
    let array = preview
        .strip_prefix(marker)
        .unwrap_or_else(|| panic!("clamped preview did not start with 100 rows: {preview}"));
    assert!(
        array.starts_with('{'),
        "clamped preview omitted the hub object: {preview}"
    );
    let mut depth = 0_i32;
    let mut end = None;
    for (index, byte) in array.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.unwrap_or_else(|| panic!("clamped hub object was cut off: {preview}"));
    let first: Value = serde_json::from_str(&array[..=end]).unwrap_or_else(|error| {
        panic!(
            "clamped hub object did not parse: {error}\n{}",
            &array[..=end]
        )
    });
    assert_exact_hotspot(&first, "hub", "src/fanout.ts", 1, 101, 0, 101);

    let handle = payload["handle"]
        .as_str()
        .unwrap_or_else(|| panic!("truncation handle missing: {payload}"));
    assert!(
        handle.starts_with("rh_") && handle.len() > "rh_".len(),
        "{payload}"
    );
    let expires = payload["retrieve_expires_at"]
        .as_i64()
        .unwrap_or_else(|| panic!("retrieve expiry missing: {payload}"));
    let instruction = payload["retrieve_instruction"]
        .as_str()
        .unwrap_or_else(|| panic!("retrieve instruction missing: {payload}"));
    assert!(instruction.contains(handle), "{instruction}");
    assert!(instruction.contains(&expires.to_string()), "{instruction}");
    assert!(
        instruction.contains(&preview_chars.to_string()),
        "{instruction}"
    );
    assert!(
        instruction.contains(&original_chars.to_string()),
        "{instruction}"
    );
    assert!(instruction.contains("tracedecay_retrieve"), "{instruction}");
}

#[tokio::test]
async fn hotspots_ranks_symbols_by_edge_degree_and_clamps_limit() {
    let chain_dir = test_temp_dir();
    let chain_root = chain_dir.path().join("project");
    write_chain_project(&chain_root);
    let (chain, _env) = init_test_project(&chain_root).await;

    let chain_default = call_hotspots(&chain, json!({"format": "json"})).await;
    let chain_limit_one = call_hotspots(&chain, json!({"format": "json", "limit": 1})).await;
    let chain_markdown = call_hotspots(&chain, json!({"format": "markdown", "limit": 1})).await;
    let chain_rejected = chain
        .harness
        .call_tool(
            &chain.project_root,
            "tracedecay_hotspots",
            json!({"limit": 0, "format": "json"}),
        )
        .await
        .expect("zero limit still reaches the MCP server");
    close_test_graph(chain).await;

    let chain_default_payload = parse_body(&chain_default);
    assert_chain_ranking(&chain_default_payload);
    assert_savings_footer(&chain_default, CHAIN_SOURCE.len());

    let chain_one_payload = parse_body(&chain_limit_one);
    let one = hotspots(&chain_one_payload);
    assert_eq!(one.len(), 1, "{chain_one_payload}");
    assert_exact_hotspot(&one[0], "mid", "src/calls.ts", 9, 1, 1, 2);
    assert_savings_footer(&chain_limit_one, CHAIN_SOURCE.len());

    let mid_id = one[0]["id"].as_str().expect("mid occurrence id").to_owned();
    assert_eq!(
        body_text(&chain_markdown),
        format!(
            "**hotspot_count:** 1\n\n## hotspots\n- **mid**\n  **kind:** function\n  **file:** src/calls.ts\n  **line:** 9\n  **id:** `{mid_id}`\n  **incoming:** 1\n  **outgoing:** 1\n  **total:** 2\n"
        )
    );
    assert_savings_footer(&chain_markdown, CHAIN_SOURCE.len());

    let rejected = chain_rejected.error.expect("zero limit is a tool error");
    assert_eq!(rejected.code, -32603);
    assert_eq!(
        rejected.message,
        "tool execution failed: config error: invalid parameter: tracedecay_hotspots requires limit to be at least 1"
    );
    assert_eq!(
        rejected.data,
        Some(json!({
            "tool": "tracedecay_hotspots",
            "cli_fallback": "This tool is also available from the shell: `tracedecay tool hotspots ...` (`tracedecay tool hotspots --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
        }))
    );

    let fanout_dir = test_temp_dir();
    let fanout_root = fanout_dir.path().join("project");
    let fanout_bytes = write_fanout_project(&fanout_root);
    let (fanout, _env) = init_test_project(&fanout_root).await;
    let fanout_default = call_hotspots(&fanout, json!({"format": "json"})).await;
    let fanout_capped = call_hotspots(&fanout, json!({"format": "json", "limit": 250})).await;
    let fanout_one = call_hotspots(&fanout, json!({"format": "json", "limit": 1})).await;
    close_test_graph(fanout).await;

    let fanout_default_payload = parse_body(&fanout_default);
    assert_fanout_page(&fanout_default_payload, 10);
    assert_savings_footer(&fanout_default, fanout_bytes);

    let fanout_one_payload = parse_body(&fanout_one);
    let fanout_top = hotspots(&fanout_one_payload);
    assert_eq!(fanout_top.len(), 1, "{fanout_one_payload}");
    assert_exact_hotspot(&fanout_top[0], "hub", "src/fanout.ts", 1, 101, 0, 101);
    assert_savings_footer(&fanout_one, fanout_bytes);

    assert_clamped_truncation(&parse_body(&fanout_capped));
    assert_savings_footer(&fanout_capped, fanout_bytes);
}
