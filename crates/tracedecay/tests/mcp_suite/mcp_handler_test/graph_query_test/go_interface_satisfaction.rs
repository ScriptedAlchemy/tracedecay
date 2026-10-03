//! `implementations` over Go interfaces indexed through the production
//! capture path: a type in another file that carries the method set is an
//! implementor, and an interface whose method set the project cannot see is
//! disclosed as partial coverage instead of a complete empty answer.

use super::{call_production_tool, graph_query_fixture_with_sources, shutdown_graph_fixture};
use crate::support::extract_text;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tracedecay-code-extraction/fixtures/go-satisfaction"
);

fn copy_fixture(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

async fn implementations(
    fixture: &super::GraphQueryFixture,
    interface: &str,
) -> (Vec<String>, Value) {
    let result = call_production_tool(
        fixture,
        "tracedecay_implementations",
        json!({"selector": {"selector": "trait", "name": interface}, "format": "json"}),
        None,
        None,
    )
    .await
    .expect("implementations");
    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    let evidence = payload["outcome"]["value"].clone();
    let mut names = evidence["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("implementations evidence missing: {payload:#}"))
        .iter()
        .map(|item| {
            item["symbol"]["qualified_name"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    names.sort();
    (names, evidence)
}

fn unsupported_omission(evidence: &Value) -> bool {
    evidence["omissions"]
        .as_array()
        .is_some_and(|omissions| omissions.iter().any(|o| o["reason"] == "unsupported"))
}

#[tokio::test]
async fn go_implementations_cross_files_and_disclose_undecided_interfaces() {
    let fixture =
        graph_query_fixture_with_sources(|project| copy_fixture(Path::new(FIXTURE_ROOT), project))
            .await;

    let (names, evidence) = implementations(&fixture, "Adder").await;
    assert_eq!(names, vec!["calc/simple.go::Simple"], "{evidence:#}");
    assert_eq!(
        evidence["coverage"]["completeness"], "complete",
        "{evidence:#}"
    );
    assert!(!unsupported_omission(&evidence), "{evidence:#}");

    let (names, evidence) = implementations(&fixture, "Rows").await;
    assert!(names.is_empty(), "{evidence:#}");
    assert_eq!(
        evidence["coverage"]["completeness"], "partial",
        "io.Reader is outside the project, so Rows cannot be complete: {evidence:#}"
    );
    assert!(unsupported_omission(&evidence), "{evidence:#}");

    shutdown_graph_fixture(fixture).await;
}
