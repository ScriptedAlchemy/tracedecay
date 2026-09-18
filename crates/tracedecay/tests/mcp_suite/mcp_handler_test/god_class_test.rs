//! `tracedecay_god_class` over the production MCP `tools/call` path.
//!
//! Expected rows are the fixture's member tallies. `Echo` has a constructor,
//! and `FatView` plus `hugeHelper` sit beside the classes, so a ranking that
//! counts the wrong symbols cannot match.

#![cfg(feature = "test-transport")]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    extract_real_server_text, extract_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, wait_for_current_graph,
};

fn write_class(
    project: &Path,
    relative: &str,
    name: &str,
    fields: usize,
    methods: usize,
    constructor: bool,
) {
    let mut source = format!("export class {name} {{\n");
    for index in 0..fields {
        let _ = writeln!(source, "  field{index}: number;");
    }
    if constructor {
        source.push_str("  constructor() {}\n");
    }
    for index in 0..methods {
        let _ = writeln!(
            source,
            "  method{index}(): number {{\n    return {index};\n  }}"
        );
    }
    source.push_str("}\n");
    let path = project.join(relative);
    fs::create_dir_all(path.parent().expect("class file parent")).unwrap();
    fs::write(path, source).unwrap();
}

fn write_typescript_god_class_project(project: &Path) {
    fs::write(
        project.join("package.json"),
        "{\"name\":\"god-class-fixture\",\"private\":true,\"type\":\"module\"}\n",
    )
    .unwrap();
    // fields, methods, constructor. Echo's constructor is not a method.
    for (relative, name, fields, methods, constructor) in [
        ("src/billing/alpha.ts", "Alpha", 5, 6, false),
        ("src/billing/bravo.ts", "Bravo", 5, 5, false),
        ("src/billing/charlie.ts", "Charlie", 5, 4, false),
        ("src/reports/delta.ts", "Delta", 4, 4, false),
        ("src/billing/echo.ts", "Echo", 4, 3, true),
        ("src/billing/foxtrot.ts", "Foxtrot", 3, 3, false),
        ("src/billing/golf.ts", "Golf", 3, 2, false),
        ("src/billing/hotel.ts", "Hotel", 2, 2, false),
        ("src/billing/india.ts", "India", 2, 1, false),
        ("src/billing/juliet.ts", "Juliet", 1, 1, false),
        ("src/billing/kilo.ts", "Kilo", 0, 1, false),
        ("src/billing/lima.ts", "Lima", 0, 0, false),
    ] {
        write_class(project, relative, name, fields, methods, constructor);
    }
    let mut noise = String::from("export interface FatView {\n");
    for index in 0..12 {
        let _ = writeln!(noise, "  view{index}(): void;");
    }
    noise.push_str(
        "}\n\nexport function hugeHelper(value: number): number {\n  return value + 1;\n}\n",
    );
    fs::write(project.join("src/billing/noise.ts"), noise).unwrap();
}

fn ranking_row(
    name: &str,
    kind: &str,
    file: &str,
    line: u64,
    methods: u64,
    fields: u64,
    total_members: u64,
) -> Value {
    json!({
        "name": name,
        "kind": kind,
        "file": file,
        "line": line,
        "methods": methods,
        "fields": fields,
        "total_members": total_members,
    })
}

fn typescript_ranking() -> Vec<Value> {
    vec![
        ranking_row("Alpha", "class", "src/billing/alpha.ts", 1, 6, 5, 11),
        ranking_row("Bravo", "class", "src/billing/bravo.ts", 1, 5, 5, 10),
        ranking_row("Charlie", "class", "src/billing/charlie.ts", 1, 4, 5, 9),
        ranking_row("Delta", "class", "src/reports/delta.ts", 1, 4, 4, 8),
        ranking_row("Echo", "class", "src/billing/echo.ts", 1, 3, 4, 7),
        ranking_row("Foxtrot", "class", "src/billing/foxtrot.ts", 1, 3, 3, 6),
        ranking_row("Golf", "class", "src/billing/golf.ts", 1, 2, 3, 5),
        ranking_row("Hotel", "class", "src/billing/hotel.ts", 1, 2, 2, 4),
        ranking_row("India", "class", "src/billing/india.ts", 1, 1, 2, 3),
        ranking_row("Juliet", "class", "src/billing/juliet.ts", 1, 1, 1, 2),
        ranking_row("Kilo", "class", "src/billing/kilo.ts", 1, 1, 0, 1),
        ranking_row("Lima", "class", "src/billing/lima.ts", 1, 0, 0, 0),
    ]
}

fn assert_ranking(payload: &Value, expected: &[Value]) {
    let keys = payload
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>());
    assert_eq!(
        keys,
        Some(vec!["result_count".to_owned(), "ranking".to_owned()]),
        "{payload}"
    );
    assert_eq!(payload["result_count"], json!(expected.len()), "{payload}");
    let ranking = payload["ranking"]
        .as_array()
        .unwrap_or_else(|| panic!("ranking is not an array: {payload}"));
    assert_eq!(ranking.len(), expected.len(), "{payload}");
    let mut ids = Vec::new();
    for (item, expected_row) in ranking.iter().zip(expected) {
        let mut row = item.clone();
        let id = row
            .as_object_mut()
            .unwrap_or_else(|| panic!("ranking row is not an object: {item}"))
            .remove("id");
        let id = id
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("ranking row is missing a string id: {item}"));
        assert!(!id.is_empty(), "{item}");
        ids.push(id.to_owned());
        assert_eq!(&row, expected_row, "{payload}");
    }
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "duplicate occurrence ids: {ids:?}");
}

async fn god_class_json(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_god_class", arguments).await;
    assert_ne!(
        result["isError"],
        json!(true),
        "tracedecay_god_class failed: {}",
        extract_real_server_text(&result)
    );
    let text = extract_real_server_text(&result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_god_class did not return JSON: {error}\n{text}"))
}

fn delta_markdown(id: &str) -> String {
    format!(
        "\
**result_count:** 1

## ranking
- **Delta**
  **kind:** class
  **file:** src/reports/delta.ts
  **line:** 1
  **id:** `{id}`
  **fields:** 4
  **methods:** 4
  **total_members:** 8
"
    )
}

#[tokio::test]
async fn god_class_ranks_classes_by_member_count() {
    let fixture =
        production_composition_fixture_with_sources(write_typescript_god_class_project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;
    let full = typescript_ranking();

    let uncapped = god_class_json(&server, json!({"limit": 100, "format": "json"})).await;
    assert_ranking(&uncapped, &full);

    let default_limit = god_class_json(&server, json!({"format": "json"})).await;
    assert_ranking(&default_limit, &full[..10]);

    let top = god_class_json(&server, json!({"limit": 1, "format": "json"})).await;
    assert_ranking(&top, &full[..1]);

    // Delta lives under src/reports. FatView's 12 methods must not enter.
    let billing_indexes = [0, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11];
    let billing: Vec<Value> = billing_indexes
        .into_iter()
        .map(|index| full[index].clone())
        .collect();
    let scoped = god_class_json(
        &server,
        json!({"path": "src/billing", "limit": 100, "format": "json"}),
    )
    .await;
    assert_ranking(&scoped, &billing);

    let reports = god_class_json(
        &server,
        json!({"path": "src/reports", "limit": 100, "format": "json"}),
    )
    .await;
    assert_ranking(&reports, &full[3..4]);
    let delta_id = reports["ranking"][0]["id"]
        .as_str()
        .expect("Delta occurrence id");

    let missing = god_class_json(
        &server,
        json!({"path": "src/missing", "limit": 100, "format": "json"}),
    )
    .await;
    assert_ranking(&missing, &[]);

    let markdown = god_class_json_text(
        &server,
        json!({"path": "src/reports", "limit": 1, "format": "markdown"}),
    )
    .await;
    let expected_markdown = delta_markdown(delta_id);
    assert_eq!(markdown, expected_markdown);

    // The suite JSON helper injects `format: json` when it is absent. This
    // call does not, so an omitted format is the advertised markdown default.
    let response = fixture
        .harness
        .call_tool(
            &fixture.project_root,
            "tracedecay_god_class",
            json!({"path": "src/reports", "limit": 1}),
        )
        .await
        .expect("omitted-format god class call");
    assert!(
        response.error.is_none(),
        "omitted-format god class call failed: {:?}",
        response.error
    );
    let default_text = extract_text(response.result.as_ref().expect("god class result"));
    assert_eq!(default_text, expected_markdown);

    fixture.harness.shutdown().await;
}

async fn god_class_json_text(server: &McpServer, arguments: Value) -> String {
    let result = handle_real_server_tool_call(server, "tracedecay_god_class", arguments).await;
    assert_ne!(
        result["isError"],
        json!(true),
        "tracedecay_god_class failed: {}",
        extract_real_server_text(&result)
    );
    extract_real_server_text(&result).to_owned()
}

fn write_rust_god_class_project(project: &Path) {
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"god_class_struct\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "\
pub struct Account {
    pub id: u64,
    pub name: String,
    pub active: bool,
}

impl Account {
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }

    pub fn disable(&mut self) {
        self.active = false;
    }
}

pub enum Status {
    Open,
    Closed,
    Pending,
    Archived,
}

pub fn leftover() -> u32 {
    1
}
",
    )
    .unwrap();
}

#[tokio::test]
async fn god_class_counts_struct_fields_not_impl_methods() {
    let fixture = production_composition_fixture_with_sources(write_rust_god_class_project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production MCP server");
    wait_for_current_graph(&server).await;

    let ranked = god_class_json(&server, json!({"limit": 100, "format": "json"})).await;
    assert_ranking(
        &ranked,
        &[ranking_row("Account", "struct", "src/lib.rs", 1, 0, 3, 3)],
    );

    let elsewhere = god_class_json(
        &server,
        json!({"path": "tests", "limit": 100, "format": "json"}),
    )
    .await;
    assert_ranking(&elsewhere, &[]);

    fixture.harness.shutdown().await;
}
