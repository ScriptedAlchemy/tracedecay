//! `tracedecay_rename_symbol`, apply-grade rename bound to preview evidence.
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
use tracedecay_mcp::ToolResult;

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
        p["effect"]["receipt"]["outcome"], "failed",
        "source drift must retain a failed durable receipt: {p}"
    );
    assert_eq!(
        p["effect"]["payload"]["success"], false,
        "source drift must retain the denied operation outcome: {p}"
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

    // The apply failed, either as a typed error or a failed durable effect,
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

/// Source of `src/pricing.rs` in [`rename_fixture`], byte for byte.
const PRICING_SOURCE: &str = r#"//! pricing
pub struct LineItem {
    pub unit_price: u64,
    pub quantity: u32,
}

/// Grand total in cents.
pub fn compute_grand_total(items: &[LineItem]) -> u64 {
    let mut total = 0u64;
    for item in items {
        total += item.unit_price * item.quantity as u64;
    }
    total
}

pub fn tally(items: &[LineItem]) -> u64 {
    compute_grand_total(items)
}
"#;

/// `PRICING_SOURCE` after `compute_grand_total` becomes `calculate_total_cents`.
const RENAMED_PRICING_SOURCE: &str = r#"//! pricing
pub struct LineItem {
    pub unit_price: u64,
    pub quantity: u32,
}

/// Grand total in cents.
pub fn calculate_total_cents(items: &[LineItem]) -> u64 {
    let mut total = 0u64;
    for item in items {
        total += item.unit_price * item.quantity as u64;
    }
    total
}

pub fn tally(items: &[LineItem]) -> u64 {
    calculate_total_cents(items)
}
"#;

/// Source of `src/nested/orders.rs` in [`rename_fixture`], byte for byte.
const ORDERS_SOURCE: &str = r#"//! orders
use crate::pricing::LineItem;

pub fn quantity(items: &[LineItem]) -> usize {
    items.len()
}
"#;

/// Exact dry-run diff for renaming `compute_grand_total` in `src/pricing.rs`.
///
/// The preview renderer prefixes a context line with one space and a removed
/// or added line with `-` or `+`, so a blank source line is a single marker
/// character. The hunk has no trailing newline.
const RENAME_PREVIEW_DIFF: &str = concat!(
    "--- src/pricing.rs\n",
    "@@ -5,14 +5,14 @@\n",
    " }\n",
    " \n",
    " /// Grand total in cents.\n",
    "-pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n",
    "-    let mut total = 0u64;\n",
    "-    for item in items {\n",
    "-        total += item.unit_price * item.quantity as u64;\n",
    "-    }\n",
    "-    total\n",
    "-}\n",
    "-\n",
    "-pub fn tally(items: &[LineItem]) -> u64 {\n",
    "-    compute_grand_total(items)\n",
    "+pub fn calculate_total_cents(items: &[LineItem]) -> u64 {\n",
    "+    let mut total = 0u64;\n",
    "+    for item in items {\n",
    "+        total += item.unit_price * item.quantity as u64;\n",
    "+    }\n",
    "+    total\n",
    "+}\n",
    "+\n",
    "+pub fn tally(items: &[LineItem]) -> u64 {\n",
    "+    calculate_total_cents(items)\n",
    " }",
);

const RENAMED_SITES: &str = r#"[
  {
    "disposition": "changed",
    "end_byte": 137,
    "expected_bytes": "compute_grand_total",
    "file": "src/pricing.rs",
    "kind": "declaration",
    "line": 8,
    "reason": "exact graph-bound occurrence",
    "replacement_bytes": "calculate_total_cents",
    "start_byte": 118
  },
  {
    "disposition": "changed",
    "end_byte": 358,
    "expected_bytes": "compute_grand_total",
    "file": "src/pricing.rs",
    "kind": "resolved_call",
    "line": 17,
    "reason": "exact graph-bound occurrence",
    "replacement_bytes": "calculate_total_cents",
    "start_byte": 339
  }
]"#;

/// Drop digests, receipts, and occurrence ids. What remains is the rename a
/// caller can check without knowing the fixture's git commit or graph generation.
/// Hazard order is not part of that contract when several hazards share a
/// message and no site id, so hazards are compared sorted by kind.
fn observable_rename(payload: &Value) -> Value {
    let mut payload = payload.clone();
    let Some(object) = payload.as_object_mut() else {
        return payload;
    };
    if let Some(sites) = object.get_mut("sites").and_then(Value::as_array_mut) {
        for site in sites {
            if let Some(site) = site.as_object_mut() {
                site.remove("site_id");
                site.remove("source_node_id");
            }
        }
    }
    if let Some(hazards) = object.get_mut("hazards").and_then(Value::as_array_mut) {
        for hazard in hazards.iter_mut() {
            if let Some(hazard) = hazard.as_object_mut() {
                hazard.remove("site_id");
            }
        }
        hazards.sort_by(|left, right| {
            left["kind"]
                .as_str()
                .unwrap_or("")
                .cmp(right["kind"].as_str().unwrap_or(""))
                .then(
                    left["message"]
                        .as_str()
                        .unwrap_or("")
                        .cmp(right["message"].as_str().unwrap_or("")),
                )
        });
    }
    for key in [
        "preview_id",
        "preview_digest",
        "plan_digest",
        "graph_revision",
        "repository_revision",
        "expected_state",
        "predicted_state",
        "verification",
        "effect",
    ] {
        object.remove(key);
    }
    payload
}

fn assert_pricing_fixture_bytes(project: &Path) {
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_SOURCE
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        ORDERS_SOURCE
    );
}

#[tokio::test]
async fn test_rename_symbol_literal_dry_run_plan() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    assert_pricing_fixture_bytes(project);
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    assert_eq!(node["name"], "compute_grand_total");
    assert_eq!(node["kind"], "function");
    assert_eq!(node["file"], "src/pricing.rs");
    assert_eq!(
        node["qualified_name"],
        "src/pricing.rs::compute_grand_total"
    );

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
    let sites: Value = serde_json::from_str(RENAMED_SITES).unwrap();
    assert_eq!(
        observable_rename(&payload),
        json!({
            "success": true,
            "dry_run": true,
            "message": "dry run. Nothing written; preview only (rename previewed)",
            "symbol": "src/pricing.rs::compute_grand_total",
            "old_name": "compute_grand_total",
            "new_name": "calculate_total_cents",
            "files": [{"file": "src/pricing.rs", "replaced_count": 2}],
            "reference_count": 1,
            "sites": sites,
            "dispositions": {"changed": 2, "unchanged": 0, "skipped": 0, "blocked": 0},
            "impact": {
                "callers": ["src/pricing.rs::tally"],
                "reexports": [],
                "affected_files": ["src/pricing.rs"],
                "affected_tests": []
            },
            "diff": RENAME_PREVIEW_DIFF,
            "replayed": false
        })
    );
    assert_pricing_fixture_bytes(project);
}

#[tokio::test]
async fn test_rename_symbol_literal_applied_source() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    assert_pricing_fixture_bytes(project);
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    let preview = preview_rename(&cg, &node, "calculate_total_cents").await;
    let args = accepted_apply_args(
        &node,
        "calculate_total_cents",
        &preview,
        "rename.literal-applied-source",
    );
    let result = handle_tool_call(&cg, "tracedecay_rename_symbol", args.clone(), None, None)
        .await
        .unwrap();
    let payload = rename_payload(&result);

    assert_eq!(payload["success"], true);
    assert_eq!(payload["message"], "rename applied");
    assert_eq!(payload["replayed"], false);
    assert_eq!(payload.get("dry_run"), None);
    assert_eq!(payload["old_name"], "compute_grand_total");
    assert_eq!(payload["new_name"], "calculate_total_cents");
    assert_eq!(
        payload["files"],
        json!([{"file": "src/pricing.rs", "replaced_count": 2}])
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        RENAMED_PRICING_SOURCE
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        ORDERS_SOURCE
    );

    let replay = handle_tool_call(&cg, "tracedecay_rename_symbol", args, None, None)
        .await
        .unwrap();
    let replayed = rename_payload(&replay);
    assert_eq!(
        json!({
            "success": replayed["success"],
            "replayed": replayed["replayed"],
            "durable_metadata_only": replayed["durable_metadata_only"],
            "message": replayed["message"],
            "files": replayed["files"],
            "change_count": replayed["change_count"],
            "finding_count": replayed["finding_count"],
            "operation": replayed["operation"],
        }),
        json!({
            "success": true,
            "replayed": true,
            "durable_metadata_only": true,
            "message": "source edit completed; detailed edit output was not retained",
            "files": ["src/pricing.rs"],
            "change_count": 2,
            "finding_count": 0,
            "operation": "operation.application.rename_symbol",
        })
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        RENAMED_PRICING_SOURCE
    );
}

#[tokio::test]
async fn test_rename_symbol_literal_denials() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    assert_pricing_fixture_bytes(project);
    let (cg, _env) = init_test_project(project).await;
    let node = preview_node(&cg, "compute_grand_total").await;

    let invalid = handle_tool_call(
        &cg,
        "tracedecay_rename_symbol",
        rename_args(&node, "not an identifier"),
        None,
        None,
    )
    .await
    .unwrap();
    let invalid = rename_payload(&invalid);
    assert_eq!(
        observable_rename(&invalid),
        json!({
            "success": false,
            "dry_run": true,
            "message": "rename requires valid old and new identifiers",
            "symbol": "src/pricing.rs::compute_grand_total",
            "old_name": "compute_grand_total",
            "new_name": "not an identifier",
            "reference_count": 0,
            "dispositions": {"changed": 0, "unchanged": 0, "skipped": 0, "blocked": 0},
            "hazards": [{
                "kind": "invalid_identifier",
                "blocking": true,
                "message": "rename requires valid old and new identifiers"
            }],
            "impact": {
                "callers": [],
                "reexports": [],
                "affected_files": [],
                "affected_tests": []
            },
            "replayed": false
        })
    );

    let same = handle_tool_call(
        &cg,
        "tracedecay_rename_symbol",
        rename_args(&node, "compute_grand_total"),
        None,
        None,
    )
    .await
    .unwrap();
    let same = rename_payload(&same);
    assert_eq!(same["success"], false);
    assert_eq!(
        same["message"],
        "new name is identical to the bound old name"
    );
    assert_eq!(
        observable_rename(&same)["hazards"],
        json!([{
            "kind": "invalid_identifier",
            "blocking": true,
            "message": "new name is identical to the bound old name"
        }])
    );

    let collision = handle_tool_call(
        &cg,
        "tracedecay_rename_symbol",
        rename_args(&node, "tally"),
        None,
        None,
    )
    .await
    .unwrap();
    let collision = rename_payload(&collision);
    assert_eq!(
        collision["message"],
        "rename blocked by stale, ambiguous, unsupported, or colliding evidence"
    );
    assert_eq!(
        observable_rename(&collision)["hazards"],
        json!([
            {
                "kind": "changed_resolution",
                "blocking": true,
                "message": "`tally` already occurs in src/pricing.rs; collision, shadowing, or changed resolution is possible"
            },
            {
                "kind": "namespace_collision",
                "blocking": true,
                "message": "`tally` already occurs in src/pricing.rs; collision, shadowing, or changed resolution is possible"
            },
            {
                "kind": "shadowing",
                "blocking": true,
                "message": "`tally` already occurs in src/pricing.rs; collision, shadowing, or changed resolution is possible"
            }
        ])
    );
    assert_eq!(collision["success"], false);
    assert_pricing_fixture_bytes(project);
}
