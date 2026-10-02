//! File-scoped reads on a logical path the sealed generation never
//! published: `source_outline` and `file_dependents` must refuse
//! `not_found_or_not_authorized`, never answer a complete empty success.

use super::{
    GraphQueryFixture, call_production_tool, graph_query_fixture_with_sources,
    shutdown_graph_fixture,
};
use serde_json::{Value, json};
use std::fs;

const WALK_RS: &str = "pub struct Walk;\nimpl Walk {\n    pub fn read(&self) {}\n}\n";

async fn unknown_path_fixture() -> GraphQueryFixture {
    graph_query_fixture_with_sources(|project| {
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/walk.rs"), WALK_RS).unwrap();
        fs::write(project.join("src/lib.rs"), "mod walk;\nuse walk::Walk;\n").unwrap();
    })
    .await
}

#[tokio::test]
async fn file_scoped_reads_refuse_a_path_the_generation_never_published() {
    let fixture = unknown_path_fixture().await;
    for (tool, arguments) in [
        (
            "tracedecay_source_outline",
            json!({"file": "src/ghost.rs", "format": "json"}),
        ),
        (
            "tracedecay_file_dependents",
            json!({"file": "src/ghost.rs", "format": "json"}),
        ),
    ] {
        let result = call_production_tool(&fixture, tool, arguments, None, None)
            .await
            .unwrap_or_else(|error| panic!("{tool}: {error}"));
        let problem = &result.value["structuredContent"]["problem"];
        assert_eq!(
            (
                &result.value["isError"],
                &problem["kind"],
                &problem["code"],
                &problem["retry"],
            ),
            (
                &json!(true),
                &json!("not_found_or_not_authorized"),
                &json!("not_found_or_not_authorized"),
                &json!("never"),
            ),
            "{tool}: {}",
            result.value
        );
    }
    // The admission gate answers admitted paths unchanged.
    let result = call_production_tool(
        &fixture,
        "tracedecay_source_outline",
        json!({"file": "src/walk.rs", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("source_outline walk.rs: {error}"));
    let body: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .unwrap_or_default(),
    )
    .unwrap_or_else(|error| panic!("source_outline JSON: {error}: {}", result.value));
    assert!(
        body["outcome"]["value"]["payload"]["symbols"]
            .as_array()
            .is_some_and(|symbols| !symbols.is_empty()),
        "{body:#}"
    );
    shutdown_graph_fixture(fixture).await;
}
