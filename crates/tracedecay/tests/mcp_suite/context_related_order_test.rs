//! `tracedecay_context` ranks a selected symbol's neighbors before cutting
//! them to `max_nodes`, and reports what the cut left out.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, warm_code_index_search,
};

/// `hub` calls five functions. `elder` also calls three helpers (degree 4)
/// and `cherry` one (degree 2); the rest touch only `hub` (degree 1).
const SOURCE: &str = "\
pub fn hub() -> u32 {
    damson() + banana() + elder() + apple() + cherry()
}
pub fn apple() -> u32 { 1 }
pub fn banana() -> u32 { 2 }
pub fn cherry() -> u32 { zed_one() }
pub fn damson() -> u32 { 4 }
pub fn elder() -> u32 { zed_one() + zed_two() + zed_three() }
pub fn zed_one() -> u32 { 5 }
pub fn zed_two() -> u32 { 6 }
pub fn zed_three() -> u32 { 7 }
";

fn write_hub_project(dest: &Path) {
    fs::create_dir_all(dest.join("src")).expect("source dir");
    fs::write(dest.join("src/lib.rs"), SOURCE).expect("hub source");
}

fn names<'a>(payload: &'a Value, field: &str) -> Vec<&'a str> {
    payload[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} missing in {payload}"))
        .iter()
        .map(|symbol| symbol["name"].as_str().expect("symbol name"))
        .collect()
}

#[tokio::test]
async fn context_ranks_related_symbols_before_the_cut_and_reports_the_omission() {
    let production = production_composition_fixture_with_sources(write_hub_project).await;
    let server = production
        .harness
        .server(&production.project_root)
        .expect("hub server");
    warm_code_index_search(&server, "hub").await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_context",
        // Source bodies make the answer wait for the graph instead of
        // racing it against the search.
        json!({"task": "hub", "max_nodes": 3, "include_code": true, "format": "json"}),
    )
    .await;
    assert_ne!(result["isError"], Value::Bool(true), "{result}");
    let payload: Value =
        serde_json::from_str(extract_real_server_text(&result)).expect("context JSON");

    assert_eq!(names(&payload, "symbols"), ["hub"], "{payload}");
    assert_eq!(
        names(&payload, "related_symbols"),
        ["elder", "cherry", "apple"],
        "calls first, then degree descending, then qualified name: {payload}"
    );
    assert_eq!(
        payload["related_omission"],
        json!({"total": 5, "omitted": 2, "total_is_lower_bound": false}),
        "{payload}"
    );
    assert_eq!(
        payload["retrieval"]["related"],
        json!({"state": "ran", "budget": 3, "admitted": 3, "truncated": true}),
        "{payload}"
    );
}
