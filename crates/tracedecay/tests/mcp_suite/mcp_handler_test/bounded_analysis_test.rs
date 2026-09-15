//! Bounded-answer contracts for whole-repository analysis tools.
//!
//! `tracedecay_circular` answers a question whose complete result grows with
//! the repository. A declared bound has to
//! shape the answer *before* it is rendered, so a bounded request never lands
//! in the response-truncation envelope and never needs a manual
//! `tracedecay_retrieve` round trip. When the complete answer does not fit the
//! work budget, the tool must say so with continuation evidence instead of
//! silently reporting a short or empty list.

#![cfg(feature = "test-transport")]

use crate::support::*;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::fs;

/// Mirrors `crate::mcp::tools::MAX_RESPONSE_CHARS`, the point at which a tool
/// response is replaced by a preview envelope plus a retrieval handle.
const MAX_RESPONSE_CHARS: usize = 15_000;

fn assert_not_truncated(text: &str, tool: &str) {
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("{tool} must answer with JSON: {error}\n{text}"));
    assert!(
        payload.get("truncated").is_none() && payload.get("handle").is_none(),
        "{tool} answered a bounded request with a truncation envelope ({} chars); \
         the declared bound must shape the answer before rendering:\n{}",
        text.len(),
        &text[..text.len().min(600)]
    );
    assert!(
        text.len() <= MAX_RESPONSE_CHARS,
        "{tool} bounded response is {} chars, over the {MAX_RESPONSE_CHARS} budget",
        text.len()
    );
}

/// Builds a wide file-level dependency cycle: each file calls the next file's
/// function and the last calls the first, so the whole set is one strongly
/// connected component. Rendered in full, one such component already exceeds
/// the response budget — which is exactly the shape a real workspace produces.
fn write_wide_cycle(project: &std::path::Path, module_count: usize, prefix: &str) {
    let source_dir = project.join("src");
    fs::create_dir_all(&source_dir).unwrap();
    let names: Vec<String> = (0..module_count)
        .map(|index| {
            format!(
                "{prefix}_workspace_module_workspace_module_workspace_module_workspace_module_{index:04}"
            )
        })
        .collect();
    let mut lib = String::new();
    for name in &names {
        let _ = writeln!(lib, "pub mod {name};");
    }
    fs::write(source_dir.join("lib.rs"), lib).unwrap();
    for index in 0..module_count {
        let next_index = (index + 1) % module_count;
        let next = &names[next_index];
        fs::write(
            source_dir.join(format!("{}.rs", names[index])),
            format!(
                "use crate::{next}::marker_{next_index:04};\n\
                 pub fn marker_{index:04}() -> u32 {{ marker_{next_index:04}() }}\n"
            ),
        )
        .unwrap();
    }
}

/// `limit` declares how many cycles to report. The reported entries must fit
/// the response budget on their own: a huge strongly connected component has
/// to be reported as a bounded member list plus truthful total/omitted counts,
/// not as an unbounded path dump that the transport then truncates.
#[tokio::test]
async fn circular_honours_the_declared_limit_within_the_response_budget() {
    let fixture = production_composition_fixture_with_sources(|project| {
        write_wide_cycle(project, 240, "alpha");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "marker_0000").await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_circular",
        json!({"limit": 3, "format": "json"}),
    )
    .await;
    let text = extract_real_server_text(&result);
    assert_not_truncated(text, "tracedecay_circular");

    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["limit"], 3, "payload: {payload}");
    let cycles = payload["cycles"]
        .as_array()
        .unwrap_or_else(|| panic!("cycles array missing: {payload}"));
    assert!(
        cycles.len() <= 3,
        "the declared limit must bound the reported cycles: {payload}"
    );
    let total_cycles = payload["cycle_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("cycle_count missing: {payload}"));
    assert!(
        total_cycles >= 1,
        "the fixture forms at least one cycle: {payload}"
    );
    assert_eq!(
        payload["reported_cycle_count"].as_u64(),
        Some(cycles.len() as u64),
        "payload: {payload}"
    );

    // The widest component is reported with a bounded member list, and the
    // entry states how many members it left out.
    let widest = &cycles[0];
    let member_count = widest["member_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("each cycle must report its member count: {payload}"));
    let members = widest["members"]
        .as_array()
        .unwrap_or_else(|| panic!("each cycle must report bounded members: {payload}"));
    let omitted_members = widest["omitted_member_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("each cycle must report omitted members: {payload}"));
    assert_eq!(
        member_count,
        members.len() as u64 + omitted_members,
        "member accounting must add up: {payload}"
    );
    assert!(
        member_count > members.len() as u64,
        "a 240-file component must be bounded, not rendered whole: {payload}"
    );
    fixture.harness.shutdown().await;
}

/// The markdown rendering of the same bounded request must also fit the budget
/// and must state what it omitted.
#[tokio::test]
async fn circular_markdown_states_bounded_membership() {
    let fixture = production_composition_fixture_with_sources(|project| {
        write_wide_cycle(project, 240, "beta");
    })
    .await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "marker_0000").await;

    let result = handle_real_server_tool_call(
        &server,
        "tracedecay_circular",
        json!({"limit": 1, "format": "markdown"}),
    )
    .await;
    let text = extract_real_server_text(&result);
    assert!(
        text.len() <= MAX_RESPONSE_CHARS,
        "bounded markdown is {} chars, over the {MAX_RESPONSE_CHARS} budget:\n{}",
        text.len(),
        &text[..text.len().min(600)]
    );
    assert!(
        !text.contains("truncated at"),
        "a bounded markdown answer must not be transport-truncated:\n{}",
        &text[..text.len().min(600)]
    );
    assert!(
        text.contains("further member(s) not shown"),
        "the bounded member list must state its omission:\n{}",
        &text[..text.len().min(1200)]
    );
    fixture.harness.shutdown().await;
}
