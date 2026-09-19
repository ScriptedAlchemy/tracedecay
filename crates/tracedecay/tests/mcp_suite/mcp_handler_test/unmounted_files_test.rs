//! `tracedecay_unmounted_files` as a host calls it: one `tools/call` on the
//! production MCP server, against a project whose reachable files are known
//! before the call.
//!
//! `src/gated.rs` exists and is declared under `#[cfg(feature = "never")]`.
//! Predicates are not evaluated, so that file is mounted. `src/nested/leaf.rs`
//! has no mounted parent of its own, so the repair climbs to `src/lib.rs`.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_first_json_content,
    production_composition_fixture_with_sources,
};

const TOOL: &str = "tracedecay_unmounted_files";

#[tokio::test]
async fn unmounted_files_names_only_files_no_entry_reaches() {
    let fixture = mount_probe().await;

    let markdown = call_markdown(&fixture, json!({})).await;
    assert_eq!(
        markdown_findings(&markdown),
        "\
- **src/nested/leaf.rs** (rust package `mount_probe`)
  **Fix:** add `mod leaf;` to src/lib.rs
- **src/orphan.rs** (rust package `mount_probe`)
  **Fix:** add `mod orphan;` to src/lib.rs
- **src/web_orphan.ts** (typescript package `web`)
  **Next:** delete it, or confirm it is reached through a blind spot listed above
",
        "default markdown findings\n{markdown}"
    );
    assert!(
        markdown.starts_with("## Unmounted Files\n**Unmounted file count:** 3\n\n"),
        "default markdown must state the true total before any section:\n{markdown}"
    );

    let payload = call_json(&fixture, json!({"format": "json"})).await;
    assert_eq!(
        report_body(&payload),
        json!({
            "unmounted_file_count": 3,
            "returned_count": 3,
            "omitted_count": 0,
            "complete": true,
            "limit": 200,
            "path": Value::Null,
            "ecosystem": Value::Null,
            "unmounted": [
                {
                    "file": "src/nested/leaf.rs",
                    "ecosystem": "rust",
                    "package": "mount_probe",
                    "manifest": "Cargo.toml",
                    "nearest_mounted_parent": "src/lib.rs",
                    "suggested_declaration": "mod leaf;"
                },
                {
                    "file": "src/orphan.rs",
                    "ecosystem": "rust",
                    "package": "mount_probe",
                    "manifest": "Cargo.toml",
                    "nearest_mounted_parent": "src/lib.rs",
                    "suggested_declaration": "mod orphan;"
                },
                {
                    "file": "src/web_orphan.ts",
                    "ecosystem": "typescript",
                    "package": "web",
                    "manifest": "package.json",
                    "nearest_mounted_parent": Value::Null,
                    "suggested_declaration": Value::Null
                }
            ]
        }),
        "{payload}"
    );
    assert_eq!(
        ecosystem_names(&payload),
        vec!["rust", "typescript", "go"],
        "{payload}"
    );
    assert_eq!(
        census(&payload, "rust"),
        json!({
            "ecosystem": "rust",
            "status": "audited",
            "package_count": 1,
            "entry_point_count": 1,
            "scanned_file_count": 6,
            "mounted_file_count": 4,
            "unclaimed_file_count": 0,
            "unmounted_file_count": 2,
            "note": Value::Null
        }),
        "{payload}"
    );
    assert_eq!(
        census(&payload, "typescript"),
        json!({
            "ecosystem": "typescript",
            "status": "audited",
            "package_count": 1,
            "entry_point_count": 1,
            "scanned_file_count": 3,
            "mounted_file_count": 2,
            "unclaimed_file_count": 0,
            "unmounted_file_count": 1,
            "note": Value::Null
        }),
        "{payload}"
    );
    assert_eq!(
        census(&payload, "go"),
        json!({
            "ecosystem": "go",
            "status": "unsupported",
            "package_count": 0,
            "entry_point_count": 0,
            "scanned_file_count": 1,
            "mounted_file_count": 0,
            "unclaimed_file_count": 1,
            "unmounted_file_count": 0,
            "note": "1 go source file(s) are present and were not audited, this report cannot say whether any of them is unreachable"
        }),
        "{payload}"
    );

    let rust_only = call_json(&fixture, json!({"format": "json", "ecosystem": "RUST"})).await;
    assert_eq!(rust_only["ecosystem"], json!("rust"), "{rust_only}");
    assert_eq!(rust_only["unmounted_file_count"], json!(2), "{rust_only}");
    assert_eq!(
        rust_only["unmounted"],
        json!([
            {
                "file": "src/nested/leaf.rs",
                "ecosystem": "rust",
                "package": "mount_probe",
                "manifest": "Cargo.toml",
                "nearest_mounted_parent": "src/lib.rs",
                "suggested_declaration": "mod leaf;"
            },
            {
                "file": "src/orphan.rs",
                "ecosystem": "rust",
                "package": "mount_probe",
                "manifest": "Cargo.toml",
                "nearest_mounted_parent": "src/lib.rs",
                "suggested_declaration": "mod orphan;"
            }
        ]),
        "{rust_only}"
    );
    assert_eq!(
        census(&rust_only, "typescript")["unmounted_file_count"],
        json!(1),
        "an ecosystem filter hides rows, not the other section: {rust_only}"
    );

    let nested = call_json(&fixture, json!({"format": "json", "path": "src/nested"})).await;
    assert_eq!(
        report_body(&nested),
        json!({
            "unmounted_file_count": 1,
            "returned_count": 1,
            "omitted_count": 0,
            "complete": true,
            "limit": 200,
            "path": "src/nested",
            "ecosystem": Value::Null,
            "unmounted": [
                {
                    "file": "src/nested/leaf.rs",
                    "ecosystem": "rust",
                    "package": "mount_probe",
                    "manifest": "Cargo.toml",
                    "nearest_mounted_parent": "src/lib.rs",
                    "suggested_declaration": "mod leaf;"
                }
            ]
        }),
        "{nested}"
    );
    assert_eq!(
        census(&nested, "rust")["unmounted_file_count"],
        json!(2),
        "a path filter hides rows, not the ecosystem total: {nested}"
    );

    let paged = call_json(&fixture, json!({"format": "json", "limit": 1})).await;
    assert_eq!(paged["unmounted_file_count"], json!(3), "{paged}");
    assert_eq!(paged["returned_count"], json!(1), "{paged}");
    assert_eq!(paged["omitted_count"], json!(2), "{paged}");
    assert_eq!(paged["complete"], json!(false), "{paged}");
    assert_eq!(paged["limit"], json!(1), "{paged}");
    assert_eq!(
        paged["unmounted"][0]["file"],
        json!("src/nested/leaf.rs"),
        "{paged}"
    );

    let clamped = call_json(&fixture, json!({"format": "json", "limit": 0})).await;
    assert_eq!(clamped["limit"], json!(1), "{clamped}");
    assert_eq!(clamped["returned_count"], json!(1), "{clamped}");
    assert_eq!(clamped["omitted_count"], json!(2), "{clamped}");
    assert_eq!(
        clamped["unmounted"][0]["file"],
        json!("src/nested/leaf.rs"),
        "{clamped}"
    );

    let partial = call_markdown(&fixture, json!({"limit": 1})).await;
    assert!(
        partial.starts_with(
            "## Unmounted Files\n**Unmounted file count:** 3\n**Coverage:** partial\n**Omitted:** 2 (raise `limit` to see them)\n"
        ),
        "a short page must say it omitted rows:\n{partial}"
    );
    assert_eq!(
        markdown_findings(&partial),
        "\
- **src/nested/leaf.rs** (rust package `mount_probe`)
  **Fix:** add `mod leaf;` to src/lib.rs
"
    );

    fixture.harness.shutdown().await;
}

#[tokio::test]
async fn unmounted_files_refuses_arguments_that_are_not_an_object() {
    let fixture = mount_probe().await;
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, TOOL, json!([]))
        .await
        .expect("production MCP answers a tools/call");

    assert!(response.result.is_none(), "{response:?}");
    let error = response
        .error
        .expect("non-object arguments are a tool error");
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "tool execution failed: config error: invalid arguments: tracedecay_unmounted_files expects a JSON object"
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("tool"))
            .and_then(Value::as_str),
        Some(TOOL)
    );

    fixture.harness.shutdown().await;
}

async fn mount_probe() -> ProductionCompositionFixture {
    production_composition_fixture_with_sources(|root| {
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"mount_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(
            root,
            "src/lib.rs",
            "pub mod kept;\npub mod declared;\n#[cfg(feature = \"never\")]\nmod gated;\n",
        );
        write(root, "src/kept.rs", "pub fn kept() {}\n");
        write(root, "src/declared.rs", "pub fn declared() {}\n");
        write(root, "src/orphan.rs", "pub fn orphan() {}\n");
        write(root, "src/gated.rs", "pub fn gated() {}\n");
        write(root, "src/nested/leaf.rs", "pub fn leaf() {}\n");
        write(
            root,
            "package.json",
            "{\"name\":\"web\",\"main\":\"./src/index.ts\"}\n",
        );
        write(
            root,
            "src/index.ts",
            "import { kept } from \"./kept\";\nexport const app = kept;\n",
        );
        write(root, "src/kept.ts", "export const kept = 1;\n");
        write(root, "src/web_orphan.ts", "export const orphan = 1;\n");
        write(root, "cmd/main.go", "package main\n\nfunc main() {}\n");
    })
    .await
}

async fn call_json(fixture: &ProductionCompositionFixture, arguments: Value) -> Value {
    let response = call(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "tracedecay_unmounted_files failed: {:?}",
        response.error
    );
    let result = response.result.as_ref().expect("tools/call result");
    extract_first_json_content(result)
}

async fn call_markdown(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let response = call(fixture, arguments).await;
    assert!(
        response.error.is_none(),
        "tracedecay_unmounted_files failed: {:?}",
        response.error
    );
    let result = response.result.as_ref().expect("tools/call result");
    result["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                let text = item.get("text").and_then(Value::as_str)?;
                text.contains("## Unmounted Files").then_some(text)
            })
        })
        .unwrap_or_else(|| panic!("missing unmounted-files markdown in {result}"))
        .to_owned()
}

async fn call(
    fixture: &ProductionCompositionFixture,
    arguments: Value,
) -> tracedecay_mcp::JsonRpcResponse {
    fixture
        .harness
        .call_tool(&fixture.project_root, TOOL, arguments)
        .await
        .expect("production MCP answers a tools/call")
}

fn report_body(payload: &Value) -> Value {
    json!({
        "unmounted_file_count": payload["unmounted_file_count"],
        "returned_count": payload["returned_count"],
        "omitted_count": payload["omitted_count"],
        "complete": payload["complete"],
        "limit": payload["limit"],
        "path": payload["path"],
        "ecosystem": payload["ecosystem"],
        "unmounted": payload["unmounted"],
    })
}

fn ecosystem_names(payload: &Value) -> Vec<&str> {
    payload["ecosystems"]
        .as_array()
        .unwrap_or_else(|| panic!("ecosystems array: {payload}"))
        .iter()
        .map(|entry| {
            entry["ecosystem"]
                .as_str()
                .unwrap_or_else(|| panic!("ecosystem name: {entry}"))
        })
        .collect()
}

fn census(payload: &Value, name: &str) -> Value {
    let section = payload["ecosystems"]
        .as_array()
        .unwrap_or_else(|| panic!("ecosystems array: {payload}"))
        .iter()
        .find(|entry| entry["ecosystem"] == name)
        .unwrap_or_else(|| panic!("missing ecosystem {name}: {payload}"));
    json!({
        "ecosystem": section["ecosystem"],
        "status": section["status"],
        "package_count": section["package_count"],
        "entry_point_count": section["entry_point_count"],
        "scanned_file_count": section["scanned_file_count"],
        "mounted_file_count": section["mounted_file_count"],
        "unclaimed_file_count": section["unclaimed_file_count"],
        "unmounted_file_count": section["unmounted_file_count"],
        "note": section["note"],
    })
}

fn markdown_findings(markdown: &str) -> &str {
    markdown
        .split_once("### Findings\n")
        .map(|(_, findings)| findings)
        .unwrap_or_else(|| panic!("missing findings section:\n{markdown}"))
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
    fs::write(path, contents).expect("write fixture file");
}
