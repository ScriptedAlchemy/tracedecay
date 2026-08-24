//! `tracedecay_rename_symbol` — apply-grade rename bound to preview evidence.
//!
//! The preview (`tracedecay_rename_preview`) reports the exact node identity;
//! the apply consumes it and must succeed only while that evidence still
//! matches the live tree: staleness refuses, invalid targets are denied, and a
//! partial-failure apply restores every already-written preimage.

use crate::support::*;
use crate::support::{
    handle_production_source_edit_tool_call as handle_tool_call,
    init_production_source_edit_project as init_test_project,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tracedecay::mcp::ToolResult;

/// A pricing crate whose caller shares the target's module, so both declaration
/// and call are extraction-attested by the production graph. The nested module
/// deliberately contains no target spelling; cross-module unresolved names are
/// covered by a separate fail-closed hazard journey.
async fn rename_fixture(project: &Path) {
    fs::create_dir_all(project.join("src/nested")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"rename-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub mod pricing;\npub mod nested;\n",
    )
    .unwrap();
    fs::write(project.join("src/nested/mod.rs"), "pub mod orders;\n").unwrap();
    fs::write(
        project.join("src/pricing.rs"),
        "//! pricing\n\
         pub struct LineItem {\n    pub unit_price: u64,\n    pub quantity: u32,\n}\n\n\
         /// Grand total in cents.\n\
         pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n\
         \x20   let mut total = 0u64;\n\
         \x20   for item in items {\n\
         \x20       total += item.unit_price * item.quantity as u64;\n\
         \x20   }\n\
         \x20   total\n\
         }\n\n\
         pub fn tally(items: &[LineItem]) -> u64 {\n\
         \x20   compute_grand_total(items)\n\
         }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/nested/orders.rs"),
        "//! orders\n\
         use crate::pricing::LineItem;\n\n\
         pub fn quantity(items: &[LineItem]) -> usize {\n\
         \x20   items.len()\n\
         }\n",
    )
    .unwrap();
}

/// Runs `tracedecay_rename_preview` for `symbol` and returns the exact node
/// identity the apply must be bound to.
async fn preview_node(cg: &ProductionSourceEditFixture, symbol: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let search = loop {
        match handle_tool_call(
            cg,
            "tracedecay_find_exact_symbol",
            json!({ "name": symbol, "limit": 20 }),
            None,
            None,
        )
        .await
        {
            Ok(result) => break result,
            Err(error)
                if error.to_string().contains("code-graph-unavailable")
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!("exact symbol lookup failed: {error}"),
        }
    };
    let search: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    let node_id = search["matches"]
        .as_array()
        .and_then(|matches| {
            matches.iter().find_map(|result| {
                (result["name"].as_str() == Some(symbol))
                    .then(|| result["id"].as_str())
                    .flatten()
            })
        })
        .unwrap_or_else(|| {
            panic!("symbol {symbol:?} missing from production code graph: {search}")
        });
    let result = handle_tool_call(
        cg,
        "tracedecay_rename_preview",
        json!({ "node_id": node_id }),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = extract_first_json_content(&result.value);
    let node = payload["node"].clone();
    assert!(node["id"].is_string(), "preview node identity: {payload}");
    assert!(
        node["qualified_name"].is_string(),
        "preview must report the qualified name the apply binds to: {payload}"
    );
    node
}

/// The apply arguments a caller assembles verbatim from the preview's node.
fn rename_args(node: &Value, new_name: &str) -> Value {
    json!({
        "node_id": node["id"],
        "qualified_name": node["qualified_name"],
        "kind": node["kind"],
        "file": node["file"],
        "old_name": node["name"],
        "new_name": new_name,
    })
}

async fn preview_rename(cg: &ProductionSourceEditFixture, node: &Value, new_name: &str) -> Value {
    let result = handle_tool_call(
        cg,
        "tracedecay_rename_symbol",
        rename_args(node, new_name),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = rename_payload(&result);
    assert_eq!(payload["success"], true, "rename preview: {payload}");
    assert_eq!(payload["dry_run"], true, "rename preview: {payload}");
    assert_eq!(
        payload["preview_digest"], payload["expected_state"],
        "rename preview must bind the exact candidate state: {payload}"
    );
    payload
}

fn accepted_apply_args(node: &Value, new_name: &str, preview: &Value, key: &str) -> Value {
    json!({
        "node_id": node["id"],
        "qualified_name": node["qualified_name"],
        "kind": node["kind"],
        "file": node["file"],
        "old_name": node["name"],
        "new_name": new_name,
        "dry_run": false,
        "expected_state": preview["expected_state"],
        "idempotency_key": key,
        "accepted_preview": {
            "preview_id": preview["preview_id"],
            "preview_digest": preview["preview_digest"],
            "plan_digest": preview["plan_digest"],
            "repository_revision": preview["repository_revision"],
            "graph_revision": preview["graph_revision"],
        },
    })
}

fn rename_payload(result: &ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text).unwrap_or_else(|e| panic!("rename payload not JSON: {e}\n{text}"))
}

#[tokio::test]
async fn test_rename_symbol_dry_run_default_reports_plan_and_writes_nothing() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();

    let node = preview_node(&cg, "compute_grand_total").await;
    let p = preview_rename(&cg, &node, "calculate_total_cents").await;
    assert_eq!(p["success"], true, "payload: {p}");
    assert_eq!(p["dry_run"], true, "default must be a dry run: {p}");
    assert_eq!(
        p["preview_digest"], p["expected_state"],
        "the accepted preview must echo the exact candidate-state CAS digest: {p}"
    );
    let files: Vec<&str> = p["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"src/pricing.rs"), "files: {files:?}\n{p}");
    assert_eq!(files.len(), 1, "only graph-bound files may be edited: {p}");
    assert!(
        p["reference_count"].as_u64().unwrap() >= 1,
        "the caller must be graph-attested: {p}"
    );
    let diff = p["diff"].as_str().unwrap();
    assert!(diff.contains("calculate_total_cents"), "diff: {diff}");

    // The dry run wrote nothing.
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        before_orders
    );
}

#[tokio::test]
async fn test_rename_symbol_apply_rewrites_declaration_and_callers() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    let preview = preview_rename(&cg, &node, "calculate_total_cents").await;
    let args = accepted_apply_args(
        &node,
        "calculate_total_cents",
        &preview,
        "rename.apply-and-replay",
    );
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", args.clone(), None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    assert_ne!(p["dry_run"], json!(true), "payload: {p}");
    assert_eq!(p["message"], "rename applied", "payload: {p}");

    let pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    assert!(
        pricing.contains("pub fn calculate_total_cents"),
        "declaration renamed: {pricing}"
    );
    assert!(
        !pricing.contains("compute_grand_total"),
        "old name gone from declaration: {pricing}"
    );
    let orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();
    assert!(
        pricing.contains("calculate_total_cents(items)"),
        "caller renamed: {pricing}"
    );
    assert!(
        !orders.contains("compute_grand_total"),
        "unrelated module remains free of the old name: {orders}"
    );

    // An exact idempotent replay returns the durable receipt without attempting
    // to reinterpret the now-retired node identity.
    let result2 = handle_tool_call(&cg, "tracedecay_rename_symbol", args, None, None)
        .await
        .unwrap();
    let p2 = rename_payload(&result2);
    assert_eq!(p2["success"], true, "idempotent replay: {p2}");
    assert_eq!(p2["replayed"], true, "idempotent replay: {p2}");
}

#[tokio::test]
async fn test_rename_symbol_stale_tree_refuses_before_writing() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    let preview = preview_rename(&cg, &node, "calculate_total_cents").await;

    // The tree moves after the preview: someone hand-renames the declaration
    // (no reindex). The bound evidence no longer matches the live source, so
    // the apply must refuse rather than rewrite whatever is there now.
    let moved = fs::read_to_string(project.join("src/pricing.rs"))
        .unwrap()
        .replace(
            "pub fn compute_grand_total",
            "pub fn compute_grand_total_v2",
        );
    fs::write(project.join("src/pricing.rs"), &moved).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();

    let args = accepted_apply_args(
        &node,
        "calculate_total_cents",
        &preview,
        "rename.stale-tree",
    );
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", args, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "stale evidence must refuse: {p}");
    assert_eq!(
        p["effect"]["execution"]["termination"], "failed",
        "source drift must terminate before the effect: {p}"
    );
    assert_eq!(
        p["effect"]["payload"]["failed"], true,
        "source drift must retain a typed pre-effect failure: {p}"
    );

    // Nothing was written: the moved tree is exactly as the human left it.
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        moved
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        before_orders
    );
}

#[tokio::test]
async fn test_rename_symbol_denies_invalid_and_colliding_names() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();
    let node = preview_node(&cg, "compute_grand_total").await;

    // A denied preview has no acceptance to apply.
    let invalid = rename_args(&node, "not an identifier");
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", invalid, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "invalid name must be denied: {p}");
    assert!(
        p["hazards"]
            .as_array()
            .is_some_and(|hazards| hazards.iter().any(|hazard| {
                hazard["kind"] == "invalid_identifier" && hazard["blocking"] == true
            })),
        "denial must retain the typed invalid-identifier hazard: {p}"
    );

    // Identical to the old name.
    let same = rename_args(&node, "compute_grand_total");
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", same, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "same-name rename must be denied: {p}");

    // Collides with an identifier already present in a touched file.
    let collision = rename_args(&node, "tally");
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", collision, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "collision must be denied: {p}");
    assert!(
        p["hazards"]
            .as_array()
            .is_some_and(|hazards| hazards.iter().any(|hazard| {
                hazard["kind"] == "namespace_collision" && hazard["blocking"] == true
            })),
        "denial must retain the typed namespace-collision hazard: {p}"
    );

    // Every denial wrote nothing.
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        before_orders
    );
}

#[tokio::test]
async fn test_rename_symbol_blocks_unresolved_cross_module_spelling() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    fs::write(
        project.join("src/nested/orders.rs"),
        "//! orders\n\
         use crate::pricing::{LineItem, compute_grand_total};\n\n\
         pub fn order_total(items: &[LineItem]) -> u64 {\n\
         \x20   compute_grand_total(items)\n\
         }\n",
    )
    .unwrap();
    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    let result = handle_tool_call(
        &cg,
        "tracedecay_rename_symbol",
        rename_args(&node, "calculate_total_cents"),
        None,
        None,
    )
    .await
    .unwrap();
    let payload = rename_payload(&result);

    assert_eq!(payload["success"], false, "unresolved spelling: {payload}");
    assert!(
        payload["hazards"].as_array().is_some_and(|hazards| hazards
            .iter()
            .any(|hazard| { hazard["kind"] == "ambiguous_symbol" && hazard["blocking"] == true })),
        "unresolved spelling must be a blocking graph hazard: {payload}"
    );
    assert!(
        payload["sites"]
            .as_array()
            .is_some_and(|sites| sites.iter().any(|site| {
                site["file"] == "src/nested/orders.rs" && site["kind"] == "unresolved_text"
            })),
        "hazard must identify the unresolved cross-module site: {payload}"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        before_orders
    );
}

/// Publication failure: a read-only parent prevents the atomic publish and the
/// workspace remains byte-identical to its preimage.
#[cfg(unix)]
#[tokio::test]
async fn test_rename_symbol_publication_failure_preserves_preimage() {
    use std::os::unix::fs::PermissionsExt;

    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();
    let node = preview_node(&cg, "compute_grand_total").await;
    let preview = preview_rename(&cg, &node, "calculate_total_cents").await;

    // `src/` read-only blocks the temp-file publish of `src/pricing.rs`.
    let src_dir = project.join("src");
    let writable = fs::metadata(&src_dir).unwrap().permissions();
    fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let args = accepted_apply_args(
        &node,
        "calculate_total_cents",
        &preview,
        "rename.publication-failure",
    );
    let apply = handle_tool_call(&cg, "tracedecay_rename_symbol", args, None, None).await;

    // Restore permissions before asserting so the tempdir always cleans up.
    fs::set_permissions(&src_dir, writable).unwrap();

    // The apply failed — either as a typed error or a failed durable effect —
    // and never reported success.
    match apply {
        Ok(result) => {
            let p = rename_payload(&result);
            assert_ne!(p["success"], json!(true), "payload: {p}");
        }
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("rename aborted") || message.contains("reconciliation"),
                "unexpected failure shape: {message}"
            );
        }
    }

    // The workspace is byte-identical to the preimage.
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        before_pricing,
        "declaration file must be untouched"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        before_orders,
        "published caller must be rolled back to its preimage"
    );
}
