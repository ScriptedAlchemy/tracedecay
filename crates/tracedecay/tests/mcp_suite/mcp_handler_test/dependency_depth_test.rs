//! User-visible `tracedecay_dependency_depth` answers over the production MCP
//! server. Depth counts dependency edges after cycles collapse to one SCC, and
//! `implements` / `extends` edges must not become file dependencies.

#![cfg(feature = "test-transport")]

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call,
    production_composition_fixture_with_sources, warm_code_index_search,
};

fn write_dependency_depth_sources(project: &Path) {
    let source = project.join("src");
    fs::create_dir_all(source.join("layers")).unwrap();
    fs::create_dir_all(source.join("looped")).unwrap();
    fs::write(source.join("lib.rs"), "pub mod layers;\npub mod looped;\n").unwrap();
    fs::write(
        source.join("layers/mod.rs"),
        "pub mod top;\npub mod mid;\npub mod leaf;\npub mod aside;\npub mod marker;\n",
    )
    .unwrap();
    // `top` calls `mid` calls `leaf`. Module declarations are containment, not
    // file dependencies, so this is the only chain under `src/layers`.
    fs::write(
        source.join("layers/leaf.rs"),
        "pub fn leaf() -> u32 { 1 }\n",
    )
    .unwrap();
    fs::write(
        source.join("layers/mid.rs"),
        "use crate::layers::leaf::leaf;\npub fn mid() -> u32 { leaf() }\n",
    )
    .unwrap();
    fs::write(
        source.join("layers/top.rs"),
        "use crate::layers::mid::mid;\npub fn top() -> u32 { mid() }\n",
    )
    .unwrap();
    // A derive and a trait in different files used to glue those files
    // together through resolver-fuzzy `implements` / `extends` edges.
    fs::write(
        source.join("layers/aside.rs"),
        "#[derive(Debug, Clone)]\npub struct Aside;\n",
    )
    .unwrap();
    fs::write(source.join("layers/marker.rs"), "pub trait Marker {}\n").unwrap();
    fs::write(
        source.join("looped/mod.rs"),
        "pub mod left;\npub mod right;\npub mod down;\n",
    )
    .unwrap();
    fs::write(
        source.join("looped/left.rs"),
        "use crate::looped::right::right;\npub fn left() -> u32 { right() }\n",
    )
    .unwrap();
    fs::write(
        source.join("looped/right.rs"),
        "use crate::looped::left::left;\npub fn right() -> u32 { left() }\n",
    )
    .unwrap();
    fs::write(
        source.join("looped/down.rs"),
        "use crate::looped::left::left;\npub fn down() -> u32 { left() }\n",
    )
    .unwrap();
}

async fn dependency_depth(server: &McpServer, arguments: Value) -> Value {
    let result =
        handle_real_server_tool_call(server, "tracedecay_dependency_depth", arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_dependency_depth must answer with JSON: {error}\n{text}")
    })
}

/// `top -> mid -> leaf` is two edges. `aside` and `marker` stay length-1
/// chains: derive and trait metadata is not a file dependency.
#[tokio::test]
async fn dependency_depth_reports_the_call_chain_and_ignores_derives() {
    let fixture = production_composition_fixture_with_sources(write_dependency_depth_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "leaf").await;

    let payload = dependency_depth(
        &server,
        json!({"path": "src/layers", "limit": 10, "format": "json"}),
    )
    .await;
    assert_eq!(payload["max_depth"], 2, "payload: {payload}");
    assert_eq!(payload["ideal_depth"], 3, "payload: {payload}");
    assert_eq!(
        payload["depth_score"].as_f64(),
        Some(1.0),
        "payload: {payload}"
    );
    assert_eq!(
        payload["chains"],
        json!([
            {
                "file": "src/layers/leaf.rs",
                "depth": 2,
                "chain": ["src/layers/top.rs", "src/layers/mid.rs", "src/layers/leaf.rs"]
            },
            {
                "file": "src/layers/mid.rs",
                "depth": 1,
                "chain": ["src/layers/top.rs", "src/layers/mid.rs"]
            },
            {
                "file": "src/layers/aside.rs",
                "depth": 0,
                "chain": ["src/layers/aside.rs"]
            },
            {
                "file": "src/layers/marker.rs",
                "depth": 0,
                "chain": ["src/layers/marker.rs"]
            },
            {
                "file": "src/layers/mod.rs",
                "depth": 0,
                "chain": ["src/layers/mod.rs"]
            },
            {
                "file": "src/layers/top.rs",
                "depth": 0,
                "chain": ["src/layers/top.rs"]
            }
        ]),
        "payload: {payload}"
    );

    // `limit` truncates the ranked chain list. It does not change `max_depth`.
    let limited = dependency_depth(
        &server,
        json!({"path": "src/layers", "limit": 1, "format": "json"}),
    )
    .await;
    assert_eq!(limited["max_depth"], 2, "limited: {limited}");
    assert_eq!(limited["ideal_depth"], 3, "limited: {limited}");
    assert_eq!(
        limited["depth_score"].as_f64(),
        Some(1.0),
        "limited: {limited}"
    );
    assert_eq!(
        limited["chains"],
        json!([
            {
                "file": "src/layers/leaf.rs",
                "depth": 2,
                "chain": ["src/layers/top.rs", "src/layers/mid.rs", "src/layers/leaf.rs"]
            }
        ]),
        "limited: {limited}"
    );

    fixture.harness.shutdown().await;
}

/// A pair of mutual calls is one component. The answer is the single edge
/// into that component, not a walk that never ends.
#[tokio::test]
async fn dependency_depth_collapses_a_mutual_call_cycle() {
    let fixture = production_composition_fixture_with_sources(write_dependency_depth_sources).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "left").await;

    let payload = dependency_depth(&server, json!({"path": "src/looped", "format": "json"})).await;
    assert_eq!(payload["max_depth"], 1, "payload: {payload}");
    assert_eq!(payload["ideal_depth"], 2, "payload: {payload}");
    assert_eq!(
        payload["depth_score"].as_f64(),
        Some(1.0),
        "payload: {payload}"
    );

    let chains = payload["chains"]
        .as_array()
        .unwrap_or_else(|| panic!("chains array missing: {payload}"));
    assert_eq!(
        chains
            .iter()
            .map(|entry| entry["file"].as_str().unwrap_or(""))
            .collect::<Vec<_>>(),
        vec![
            "src/looped/left.rs",
            "src/looped/down.rs",
            "src/looped/mod.rs",
        ],
        "payload: {payload}"
    );
    assert_eq!(chains[0]["depth"], 1, "payload: {payload}");
    let chain = chains[0]["chain"]
        .as_array()
        .unwrap_or_else(|| panic!("cycle chain missing: {payload}"));
    assert_eq!(chain.len(), 2, "payload: {payload}");
    assert_eq!(chain[0], "src/looped/down.rs", "payload: {payload}");
    let cycle_member = chain[1].as_str().unwrap_or("");
    assert!(
        cycle_member == "src/looped/left.rs" || cycle_member == "src/looped/right.rs",
        "the collapsed component's representative must be one of its files: {payload}"
    );

    fixture.harness.shutdown().await;
}
