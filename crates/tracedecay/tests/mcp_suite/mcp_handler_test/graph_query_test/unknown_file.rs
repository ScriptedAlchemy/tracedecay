//! File-scoped code reads refuse a path the admitted generation never
//! published, instead of claiming complete coverage of an empty answer.

use super::{
    GraphQueryFixture, call_production_tool, graph_query_fixture_with_sources,
    shutdown_graph_fixture,
};
use crate::support::{expect_tool_refusal, extract_text};
use serde_json::{Value, json};
use std::fs;

async fn unknown_file_fixture() -> GraphQueryFixture {
    graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(
            project.join("src/walk.rs"),
            "pub struct Walk;\nimpl Walk {\n    pub fn read(&self) {}\n}\n",
        )
        .unwrap();
        fs::write(
            project.join("src/lib.rs"),
            "mod walk;\nmod empty;\nuse walk::Walk;\npub fn known(walk: &Walk) { walk.read(); }\n",
        )
        .unwrap();
        fs::write(project.join("src/empty.rs"), "// no symbols here\n").unwrap();
    })
    .await
}

async fn answer(fixture: &GraphQueryFixture, tool: &str, file: &str) -> Value {
    let result = call_production_tool(
        fixture,
        tool,
        json!({"file": file, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("{tool} {file}: {error}"));
    serde_json::from_str(extract_text(&result.value))
        .unwrap_or_else(|error| panic!("{tool} {file} JSON ({error}): {}", result.value))
}

async fn assert_refused(fixture: &GraphQueryFixture, tool: &str, file: &str) {
    let problem = expect_tool_refusal(
        call_production_tool(
            fixture,
            tool,
            json!({"file": file, "format": "json"}),
            None,
            None,
        )
        .await,
    );
    assert_eq!(
        problem["kind"], "not_found_or_not_authorized",
        "{tool} {file}: {problem}"
    );
}

#[tokio::test]
async fn file_scoped_reads_refuse_a_path_the_graph_never_published() {
    let fixture = unknown_file_fixture().await;

    assert_refused(&fixture, "tracedecay_source_outline", "no/such/file.rs").await;
    assert_refused(&fixture, "tracedecay_file_dependents", "no/such/file.ts").await;

    let dependents = answer(&fixture, "tracedecay_file_dependents", "src/walk.rs").await;
    assert_eq!(
        dependents["outcome"]["value"]["payload"]["dependent_files"],
        json!(["src/lib.rs"]),
        "{dependents:#}"
    );
    let outline = answer(&fixture, "tracedecay_source_outline", "src/walk.rs").await;
    let names = outline["outcome"]["value"]["payload"]["symbols"]
        .as_array()
        .unwrap_or_else(|| panic!("outline symbols: {outline:#}"))
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect::<Vec<_>>();
    assert!(
        names.contains(&"Walk") && names.contains(&"read"),
        "{outline:#}"
    );

    let empty = answer(&fixture, "tracedecay_source_outline", "src/empty.rs").await;
    assert_eq!(
        empty["outcome"]["value"]["payload"]["symbols"],
        json!([]),
        "{empty:#}"
    );
    assert!(empty["problem"].is_null(), "{empty:#}");

    shutdown_graph_fixture(fixture).await;
}
