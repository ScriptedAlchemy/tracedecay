//! Behavior of `tracedecay_distribution` as agents observe it: a real MCP
//! `tools/call` against a known source tree, with the JSON payload compared
//! to a literal census of that tree.

#![cfg(feature = "test-transport")]

use std::fs;

use serde_json::{Value, json};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::ToolResult;

use crate::support::{
    ProductionCompositionFixture, extract_text, handle_real_server_tool_call_raw,
    production_composition_fixture_with_sources, warm_code_index_search,
};

const WIDGET_RS: &str = "\
pub struct Widget {
    pub label: String,
}

impl Widget {
    pub fn new(label: String) -> Self {
        Self { label }
    }
}

pub fn render(widget: &Widget) -> String {
    widget.label.clone()
}
";

const PALETTE_RS: &str = "\
pub enum Color {
    Red,
    Blue,
}

pub fn paint() {}
";

const EXTRA_RS: &str = "\
fn checks_widget() {}
";

struct DistributionFixture {
    production: ProductionCompositionFixture,
}

impl DistributionFixture {
    async fn open() -> Self {
        let production = production_composition_fixture_with_sources(|project| {
            fs::create_dir_all(project.join("src")).unwrap();
            fs::create_dir_all(project.join("notes")).unwrap();
            fs::write(project.join("src/widget.rs"), WIDGET_RS).unwrap();
            fs::write(project.join("src/palette.rs"), PALETTE_RS).unwrap();
            fs::write(project.join("notes/extra.rs"), EXTRA_RS).unwrap();
        })
        .await;
        let server = production
            .harness
            .server(&production.project_root)
            .expect("production distribution server");
        warm_code_index_search(&server, "render").await;
        Self { production }
    }

    async fn call(&self, mut arguments: Value) -> Result<ToolResult> {
        if let Some(object) = arguments.as_object_mut() {
            object
                .entry("format".to_owned())
                .or_insert_with(|| json!("json"));
        }
        let server = self
            .production
            .harness
            .server(&self.production.project_root)?;
        let response =
            handle_real_server_tool_call_raw(&server, "tracedecay_distribution", arguments).await;
        if !response["error"].is_null() {
            return Err(TraceDecayError::Config {
                message: response["error"].to_string(),
            });
        }
        Ok(ToolResult::new(response["result"].clone(), Vec::new()))
    }

    async fn close(self) {
        self.production.harness.shutdown().await;
    }
}

fn payload(result: &ToolResult) -> Value {
    serde_json::from_str(extract_text(&result.value))
        .unwrap_or_else(|error| panic!("distribution JSON: {error}; {}", result.value))
}

fn widget_kinds() -> Value {
    json!([
        {"kind": "field", "count": 1},
        {"kind": "function", "count": 1},
        {"kind": "impl", "count": 1},
        {"kind": "method", "count": 1},
        {"kind": "struct", "count": 1}
    ])
}

fn palette_kinds() -> Value {
    json!([
        {"kind": "enum", "count": 1},
        {"kind": "enum_variant", "count": 2},
        {"kind": "function", "count": 1}
    ])
}

fn src_summary() -> Value {
    json!({
        "path_filter": "src",
        "mode": "summary",
        "total_kinds": 7,
        "distribution": [
            {"kind": "enum_variant", "count": 2},
            {"kind": "function", "count": 2},
            {"kind": "enum", "count": 1},
            {"kind": "field", "count": 1},
            {"kind": "impl", "count": 1},
            {"kind": "method", "count": 1},
            {"kind": "struct", "count": 1}
        ]
    })
}

#[tokio::test]
async fn distribution_reports_the_kind_census_of_the_indexed_tree() {
    let fixture = DistributionFixture::open().await;

    let per_file = payload(
        &fixture
            .call(json!({"path": "src", "format": "json"}))
            .await
            .expect("per-file distribution"),
    );
    assert_eq!(
        per_file,
        json!({
            "path_filter": "src",
            "mode": "per_file",
            "file_count": 2,
            "total_file_count": 2,
            "omitted_file_count": 0,
            "files": [
                {"file": "src/widget.rs", "kinds": widget_kinds()},
                {"file": "src/palette.rs", "kinds": palette_kinds()}
            ]
        })
    );

    let summary = payload(
        &fixture
            .call(json!({"path": "src", "summary": true, "limit": 1, "format": "json"}))
            .await
            .expect("summary distribution"),
    );
    assert_eq!(summary, src_summary());

    let limited = json!({
        "path_filter": "src",
        "mode": "per_file",
        "file_count": 1,
        "total_file_count": 2,
        "omitted_file_count": 1,
        "files": [
            {"file": "src/widget.rs", "kinds": widget_kinds()}
        ]
    });
    assert_eq!(
        payload(
            &fixture
                .call(json!({"path": "src", "limit": 1, "format": "json"}))
                .await
                .expect("limited distribution")
        ),
        limited
    );
    assert_eq!(
        payload(
            &fixture
                .call(json!({"path": "src", "limit": 0, "format": "json"}))
                .await
                .expect("zero limit is clamped to one file")
        ),
        limited
    );

    assert_eq!(
        payload(
            &fixture
                .call(json!({"path": "vendor", "format": "json"}))
                .await
                .expect("unmatched path")
        ),
        json!({
            "path_filter": "vendor",
            "mode": "per_file",
            "file_count": 0,
            "total_file_count": 0,
            "omitted_file_count": 0,
            "files": []
        })
    );

    assert_eq!(
        payload(
            &fixture
                .call(json!({"format": "json"}))
                .await
                .expect("unscoped per-file distribution")
        ),
        json!({
            "path_filter": null,
            "mode": "per_file",
            "file_count": 3,
            "total_file_count": 3,
            "omitted_file_count": 0,
            "files": [
                {"file": "src/widget.rs", "kinds": widget_kinds()},
                {"file": "src/palette.rs", "kinds": palette_kinds()},
                {"file": "notes/extra.rs", "kinds": [{"kind": "function", "count": 1}]}
            ]
        })
    );

    let rejected = fixture
        .call(json!([]))
        .await
        .expect_err("non-object arguments");
    assert!(
        rejected
            .to_string()
            .contains("invalid arguments: tracedecay_distribution expects a JSON object"),
        "{rejected}"
    );

    fixture.close().await;
}
