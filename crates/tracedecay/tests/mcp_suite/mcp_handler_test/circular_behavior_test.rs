//! `tracedecay_circular` as an MCP caller sees it.
//!
//! The cyclic project has two disjoint file cycles. `solo.rs` is only called
//! from `right.rs`, and `echo.rs` only calls itself, so neither is a file
//! cycle. The acyclic project has one function and no edges. Both answers are
//! asserted in full so an empty payload cannot stand in for either one.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    ProductionCompositionFixture, extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, warm_code_index_search,
};

fn write_disjoint_cycles(project: &Path) {
    let source = project.join("src");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        source.join("lib.rs"),
        "mod echo;\nmod left;\nmod mid;\nmod pair_a;\nmod pair_b;\nmod right;\nmod solo;\n",
    )
    .unwrap();
    fs::write(
        source.join("left.rs"),
        "use crate::mid::mid_fn;\npub fn left_fn() { mid_fn(); }\n",
    )
    .unwrap();
    fs::write(
        source.join("mid.rs"),
        "use crate::right::right_fn;\npub fn mid_fn() { right_fn(); }\n",
    )
    .unwrap();
    fs::write(
        source.join("right.rs"),
        "use crate::left::left_fn;\nuse crate::solo::solo_fn;\npub fn right_fn() { left_fn(); solo_fn(); }\n",
    )
    .unwrap();
    fs::write(
        source.join("pair_a.rs"),
        "use crate::pair_b::pair_b_fn;\npub fn pair_a_fn() { pair_b_fn(); }\n",
    )
    .unwrap();
    fs::write(
        source.join("pair_b.rs"),
        "use crate::pair_a::pair_a_fn;\npub fn pair_b_fn() { pair_a_fn(); }\n",
    )
    .unwrap();
    fs::write(source.join("solo.rs"), "pub fn solo_fn() -> u32 { 1 }\n").unwrap();
    fs::write(source.join("echo.rs"), "pub fn echo() { echo(); }\n").unwrap();
}

fn write_acyclic_project(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn leaf() -> u32 { 1 }\n").unwrap();
}

async fn warm(fixture: &ProductionCompositionFixture, query: &str) {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, query).await;
}

async fn call_circular(fixture: &ProductionCompositionFixture, arguments: Value) -> String {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let result = handle_real_server_tool_call(&server, "tracedecay_circular", arguments).await;
    extract_real_server_text(&result).to_owned()
}

fn parse_payload(text: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("tracedecay_circular JSON must parse: {error}\n{text}"))
}

const NAMED_CYCLES_MARKDOWN: &str = "\
# Circular Dependencies (2)

1. src/left.rs -> src/mid.rs -> src/right.rs -> src/left.rs
2. src/pair_a.rs -> src/pair_b.rs -> src/pair_a.rs
";

const BOUNDED_MARKDOWN: &str = "\
# Circular Dependencies (2)

1. src/left.rs -> … (2 further member(s) not shown of 3 at member_limit)

1 further cycle(s) not shown at limit 1; raise `limit` (max 200) to see more.
";

fn named_cycles_payload(limit: u64, member_limit: u64) -> Value {
    json!({
        "cycle_count": 2,
        "reported_cycle_count": 2,
        "omitted_cycle_count": 0,
        "limit": limit,
        "member_limit": member_limit,
        "cycles": [
            {
                "members": ["src/left.rs", "src/mid.rs", "src/right.rs"],
                "member_count": 3,
                "omitted_member_count": 0
            },
            {
                "members": ["src/pair_a.rs", "src/pair_b.rs"],
                "member_count": 2,
                "omitted_member_count": 0
            }
        ]
    })
}

fn largest_cycle_page(limit: u64, member_limit: u64) -> Value {
    json!({
        "cycle_count": 2,
        "reported_cycle_count": 1,
        "omitted_cycle_count": 1,
        "limit": limit,
        "member_limit": member_limit,
        "cycles": [
            {
                "members": ["src/left.rs"],
                "member_count": 3,
                "omitted_member_count": 2
            }
        ]
    })
}

/// A caller asking which files cycle gets those files, not a count, and a
/// caller asking a graph with no edges gets the empty answer rather than a
/// guessed cycle.
#[tokio::test]
async fn circular_names_the_cycles_and_an_empty_graph_stays_empty() {
    let cyclic = production_composition_fixture_with_sources(write_disjoint_cycles).await;
    warm(&cyclic, "left_fn").await;

    assert_eq!(
        parse_payload(&call_circular(&cyclic, json!({"format": "json"})).await),
        named_cycles_payload(25, 12)
    );
    assert_eq!(
        call_circular(&cyclic, json!({"format": "markdown"})).await,
        NAMED_CYCLES_MARKDOWN
    );
    assert_eq!(
        parse_payload(
            &call_circular(
                &cyclic,
                json!({"format": "json", "limit": 1, "member_limit": 1}),
            )
            .await
        ),
        largest_cycle_page(1, 1)
    );
    assert_eq!(
        call_circular(
            &cyclic,
            json!({"format": "markdown", "limit": 1, "member_limit": 1}),
        )
        .await,
        BOUNDED_MARKDOWN
    );
    // A zero bound is not "return nothing": the tool raises it to one cycle
    // of one member and says what it left out.
    assert_eq!(
        parse_payload(
            &call_circular(
                &cyclic,
                json!({"format": "json", "limit": 0, "member_limit": 0}),
            )
            .await
        ),
        largest_cycle_page(1, 1)
    );
    assert_eq!(
        parse_payload(
            &call_circular(
                &cyclic,
                json!({"format": "json", "limit": 1000, "member_limit": 1000}),
            )
            .await
        ),
        named_cycles_payload(200, 200)
    );
    cyclic.harness.shutdown().await;

    let acyclic = production_composition_fixture_with_sources(write_acyclic_project).await;
    warm(&acyclic, "leaf").await;
    assert_eq!(
        parse_payload(&call_circular(&acyclic, json!({"format": "json"})).await),
        json!({
            "cycle_count": 0,
            "reported_cycle_count": 0,
            "omitted_cycle_count": 0,
            "limit": 25,
            "member_limit": 12,
            "cycles": []
        })
    );
    assert_eq!(
        call_circular(&acyclic, json!({"format": "markdown"})).await,
        "No circular dependencies found.\n"
    );
    acyclic.harness.shutdown().await;
}
