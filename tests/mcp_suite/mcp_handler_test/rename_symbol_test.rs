//! `tracedecay_rename_symbol` — apply-grade rename bound to preview evidence.
//!
//! The preview (`tracedecay_rename_preview`) reports the exact node identity;
//! the apply consumes it and must succeed only while that evidence still
//! matches the live tree: staleness refuses, invalid targets are denied, and a
//! partial-failure apply restores every already-written preimage.

use crate::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tracedecay::mcp::ToolResult;
use tracedecay::tracedecay::TraceDecay;

/// A pricing crate whose caller lives in a *nested* directory and invokes the
/// symbol through a fully-qualified path (no `use` import), so every literal
/// occurrence of the name is graph-attested and the apply is a complete
/// rename. The nested directory also lets the rollback test make one file's
/// publish fail (read-only parent directory) while the other file's succeeds.
async fn rename_fixture(project: &Path) {
    fs::create_dir_all(project.join("src/nested")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub mod pricing;\npub mod nested;\n",
    )
    .unwrap();
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
         }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/nested/orders.rs"),
        "//! orders\n\
         use crate::pricing::LineItem;\n\n\
         pub fn tally(items: &[LineItem]) -> u64 {\n\
         \x20   crate::pricing::compute_grand_total(items)\n\
         }\n",
    )
    .unwrap();
}

/// Runs `tracedecay_rename_preview` for `symbol` and returns the exact node
/// identity the apply must be bound to.
async fn preview_node(cg: &TraceDecay, symbol: &str) -> Value {
    let node_id = find_node_id(cg, symbol).await;
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

fn rename_payload(result: &ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text).unwrap_or_else(|e| panic!("rename payload not JSON: {e}\n{text}"))
}

#[tokio::test]
async fn test_rename_symbol_dry_run_default_reports_plan_and_writes_nothing() {
    let dir = test_temp_dir();
    let project = dir.path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();

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
    let p = rename_payload(&result);
    assert_eq!(p["success"], true, "payload: {p}");
    assert_eq!(p["dry_run"], true, "default must be a dry run: {p}");
    let files: Vec<&str> = p["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"src/pricing.rs"), "files: {files:?}\n{p}");
    assert!(
        files.contains(&"src/nested/orders.rs"),
        "files: {files:?}\n{p}"
    );
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
    let project = dir.path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let node = preview_node(&cg, "compute_grand_total").await;
    let mut args = rename_args(&node, "calculate_total_cents");
    args["dry_run"] = json!(false);
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", args, None, None)
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
        orders.contains("crate::pricing::calculate_total_cents(items)"),
        "caller renamed: {orders}"
    );
    assert!(
        !orders.contains("compute_grand_total"),
        "old name gone from caller: {orders}"
    );

    // The graph was reindexed under the new identity: the preview evidence is
    // now stale, so replaying the exact same apply refuses instead of
    // half-matching.
    let mut replay = rename_args(&node, "calculate_total_cents");
    replay["dry_run"] = json!(false);
    let result2 = handle_tool_call(&cg, "tracedecay_rename_symbol", replay, None, None)
        .await
        .unwrap();
    let p2 = rename_payload(&result2);
    assert_eq!(p2["success"], false, "re-run must refuse: {p2}");
    assert!(
        p2["message"]
            .as_str()
            .unwrap()
            .contains("stale rename evidence"),
        "refusal names the staleness: {p2}"
    );
}

#[tokio::test]
async fn test_rename_symbol_stale_tree_refuses_before_writing() {
    let dir = test_temp_dir();
    let project = dir.path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let node = preview_node(&cg, "compute_grand_total").await;

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

    let mut args = rename_args(&node, "calculate_total_cents");
    args["dry_run"] = json!(false);
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", args, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "stale evidence must refuse: {p}");
    assert!(
        p["message"]
            .as_str()
            .unwrap()
            .contains("stale rename evidence"),
        "refusal names the staleness and the recompute path: {p}"
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
    let project = dir.path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();
    let node = preview_node(&cg, "compute_grand_total").await;

    // Not an identifier.
    let mut invalid = rename_args(&node, "not an identifier");
    invalid["dry_run"] = json!(false);
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", invalid, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "invalid name must be denied: {p}");
    assert!(
        p["message"].as_str().unwrap().contains("valid identifier"),
        "denial names the reason: {p}"
    );

    // Identical to the old name.
    let mut same = rename_args(&node, "compute_grand_total");
    same["dry_run"] = json!(false);
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", same, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "same-name rename must be denied: {p}");

    // Collides with an identifier already present in a touched file.
    let mut collision = rename_args(&node, "tally");
    collision["dry_run"] = json!(false);
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", collision, None, None)
        .await
        .unwrap();
    let p = rename_payload(&result);
    assert_eq!(p["success"], false, "collision must be denied: {p}");
    assert!(
        p["message"].as_str().unwrap().contains("collide"),
        "denial names the collision: {p}"
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

/// Partial-failure rollback: the caller file (lexically first) publishes, the
/// declaration file's publish fails (read-only parent directory), and the
/// already-published caller must be restored to its preimage — the workspace
/// is never left half-renamed.
#[cfg(unix)]
#[tokio::test]
async fn test_rename_symbol_partial_failure_restores_published_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = test_temp_dir();
    let project = dir.path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;
    cg.index_all().await.unwrap();

    let before_pricing = fs::read_to_string(project.join("src/pricing.rs")).unwrap();
    let before_orders = fs::read_to_string(project.join("src/nested/orders.rs")).unwrap();
    let node = preview_node(&cg, "compute_grand_total").await;

    // `src/` read-only blocks the temp-file publish of `src/pricing.rs` while
    // `src/nested/` stays writable, so `src/nested/orders.rs` (lexically
    // first) publishes and the later declaration write fails mid-apply.
    let src_dir = project.join("src");
    let writable = fs::metadata(&src_dir).unwrap().permissions();
    fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let mut args = rename_args(&node, "calculate_total_cents");
    args["dry_run"] = json!(false);
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

    // Rollback restored the already-published caller; the declaration was
    // never written. The workspace is byte-identical to the preimage.
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
