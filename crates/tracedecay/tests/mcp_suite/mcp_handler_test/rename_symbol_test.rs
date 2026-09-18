//! `tracedecay_rename_symbol` as a host calls it: one `tools/call` on the
//! production MCP server the daemon composition mounts.
//!
//! The preview (`tracedecay_rename_preview`) reports the exact node identity;
//! the apply consumes it and must succeed only while that evidence still
//! matches the live tree: staleness refuses, invalid targets are denied, and a
//! publication failure leaves every file byte-identical to its preimage.

use crate::support::{
    ProductionSourceEditFixture, extract_first_json_content,
    init_production_source_edit_project as init_test_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::Duration;

const PRICING_BEFORE: &str = r#"//! pricing
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

const PRICING_AFTER: &str = r#"//! pricing
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

/// Single-hunk preview the dry run must return for `PRICING_BEFORE` → `PRICING_AFTER`.
const PRICING_DIFF: &str = "\
--- src/pricing.rs
@@ -5,14 +5,14 @@
 }
 
 /// Grand total in cents.
-pub fn compute_grand_total(items: &[LineItem]) -> u64 {
-    let mut total = 0u64;
-    for item in items {
-        total += item.unit_price * item.quantity as u64;
-    }
-    total
-}
-
-pub fn tally(items: &[LineItem]) -> u64 {
-    compute_grand_total(items)
+pub fn calculate_total_cents(items: &[LineItem]) -> u64 {
+    let mut total = 0u64;
+    for item in items {
+        total += item.unit_price * item.quantity as u64;
+    }
+    total
+}
+
+pub fn tally(items: &[LineItem]) -> u64 {
+    calculate_total_cents(items)
 }
";

const ORDERS_BEFORE: &str = r#"//! orders
use crate::pricing::LineItem;

pub fn quantity(items: &[LineItem]) -> usize {
    items.len()
}
"#;

const ORDERS_CROSS_MODULE: &str = r#"//! orders
use crate::pricing::{LineItem, compute_grand_total};

pub fn order_total(items: &[LineItem]) -> u64 {
    compute_grand_total(items)
}
"#;

const BLOCKED_MESSAGE: &str =
    "rename blocked by stale, ambiguous, unsupported, or colliding evidence";

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
    fs::write(project.join("src/pricing.rs"), PRICING_BEFORE).unwrap();
    fs::write(project.join("src/nested/orders.rs"), ORDERS_BEFORE).unwrap();
}

fn assert_workspace_unchanged(project: &Path) {
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_BEFORE
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        ORDERS_BEFORE
    );
}

/// Caller-visible site fields. Identity digests and byte offsets are omitted
/// because they are addresses, not the rename the caller observes.
fn visible_sites(payload: &Value) -> Value {
    Value::Array(
        payload["sites"]
            .as_array()
            .map(|sites| {
                sites
                    .iter()
                    .map(|site| {
                        json!({
                            "kind": site["kind"],
                            "disposition": site["disposition"],
                            "file": site["file"],
                            "line": site["line"],
                            "expected_bytes": site["expected_bytes"],
                            "replacement_bytes": site["replacement_bytes"],
                            "reason": site["reason"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn visible_hazards(payload: &Value) -> Value {
    Value::Array(
        payload["hazards"]
            .as_array()
            .map(|hazards| {
                hazards
                    .iter()
                    .map(|hazard| {
                        json!({
                            "kind": hazard["kind"],
                            "blocking": hazard["blocking"],
                            "message": hazard["message"],
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn matching(items: &Value, pred: impl Fn(&Value) -> bool) -> Value {
    Value::Array(
        items
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| pred(item))
            .cloned()
            .collect(),
    )
}

/// One production `tools/call`. JSON is the public `format` a host requests
/// when it wants the structured payload; a protocol error is not a rename.
async fn call_json(
    fixture: &ProductionSourceEditFixture,
    tool_name: &str,
    arguments: Value,
) -> Value {
    let response = tools_call(fixture, tool_name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{tool_name} did not answer tools/call: {error}"));
    let result = response
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("{tool_name} returned no tools/call result: {response:?}"));
    extract_first_json_content(result)
}

async fn tools_call(
    fixture: &ProductionSourceEditFixture,
    tool_name: &str,
    mut arguments: Value,
) -> Result<tracedecay_mcp::JsonRpcResponse, String> {
    if let Some(object) = arguments.as_object_mut() {
        object
            .entry("format".to_owned())
            .or_insert_with(|| json!("json"));
    }
    let response = fixture
        .harness
        .call_tool(&fixture.project_root, tool_name, arguments)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(error) = &response.error {
        return Err(format!("{error:?}"));
    }
    Ok(response)
}

/// Runs `tracedecay_rename_preview` for `symbol` and returns the exact node
/// identity the apply must be bound to.
async fn preview_node(fixture: &ProductionSourceEditFixture, symbol: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let search = loop {
        match tools_call(
            fixture,
            "tracedecay_find_exact_symbol",
            json!({ "name": symbol, "limit": 20 }),
        )
        .await
        {
            Ok(response) => {
                let result = response.result.as_ref().unwrap_or_else(|| {
                    panic!("exact symbol lookup returned no tools/call result: {response:?}")
                });
                break extract_first_json_content(result);
            }
            Err(error)
                if error.contains("code-graph-unavailable")
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!("exact symbol lookup failed: {error}"),
        }
    };
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
    let payload = call_json(
        fixture,
        "tracedecay_rename_preview",
        json!({ "node_id": node_id }),
    )
    .await;
    let node = payload["node"].clone();
    assert_eq!(node["id"], node_id, "preview node identity: {payload}");
    assert_eq!(
        node["name"], symbol,
        "preview must report the looked-up symbol: {payload}"
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

async fn preview_rename(
    fixture: &ProductionSourceEditFixture,
    node: &Value,
    new_name: &str,
) -> Value {
    let payload = call_json(
        fixture,
        "tracedecay_rename_symbol",
        rename_args(node, new_name),
    )
    .await;
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

#[tokio::test]
async fn test_rename_symbol_dry_run_default_reports_plan_and_writes_nothing() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    assert_eq!(node["name"], "compute_grand_total");
    assert_eq!(node["kind"], "function");
    assert_eq!(node["file"], "src/pricing.rs");
    assert_eq!(
        node["qualified_name"],
        "src/pricing.rs::compute_grand_total"
    );

    let p = preview_rename(&cg, &node, "calculate_total_cents").await;
    assert_eq!(p["success"], true, "payload: {p}");
    assert_eq!(p["dry_run"], true, "default must be a dry run: {p}");
    assert_eq!(
        p["message"], "dry run. Nothing written; preview only (rename previewed)",
        "payload: {p}"
    );
    assert_eq!(p["symbol"], "src/pricing.rs::compute_grand_total", "{p}");
    assert_eq!(p["old_name"], "compute_grand_total");
    assert_eq!(p["new_name"], "calculate_total_cents");
    assert_eq!(
        p["preview_digest"], p["expected_state"],
        "the accepted preview must echo the exact candidate-state CAS digest: {p}"
    );
    assert_eq!(
        p["files"],
        json!([{ "file": "src/pricing.rs", "replaced_count": 2 }]),
        "{p}"
    );
    assert_eq!(p["reference_count"], 1, "{p}");
    assert_eq!(
        p["dispositions"],
        json!({ "changed": 2, "unchanged": 0, "skipped": 0, "blocked": 0 }),
        "{p}"
    );
    assert_eq!(
        visible_sites(&p),
        json!([
            {
                "kind": "declaration",
                "disposition": "changed",
                "file": "src/pricing.rs",
                "line": 8,
                "expected_bytes": "compute_grand_total",
                "replacement_bytes": "calculate_total_cents",
                "reason": "exact graph-bound occurrence"
            },
            {
                "kind": "resolved_call",
                "disposition": "changed",
                "file": "src/pricing.rs",
                "line": 17,
                "expected_bytes": "compute_grand_total",
                "replacement_bytes": "calculate_total_cents",
                "reason": "exact graph-bound occurrence"
            }
        ]),
        "{p}"
    );
    assert_eq!(
        p["impact"],
        json!({
            "callers": ["src/pricing.rs::tally"],
            "reexports": [],
            "affected_files": ["src/pricing.rs"],
            "affected_tests": []
        }),
        "{p}"
    );
    assert_eq!(p["diff"], PRICING_DIFF, "diff: {}", p["diff"]);
    assert_eq!(visible_hazards(&p), json!([]), "{p}");

    assert_workspace_unchanged(project);
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
    let p = call_json(&cg, "tracedecay_rename_symbol", args.clone()).await;
    assert_eq!(p["success"], true, "payload: {p}");
    assert_eq!(p["replayed"], false, "payload: {p}");
    assert_eq!(p["message"], "rename applied", "payload: {p}");
    assert_eq!(p["old_name"], "compute_grand_total");
    assert_eq!(p["new_name"], "calculate_total_cents");
    assert_eq!(
        p["files"],
        json!([{ "file": "src/pricing.rs", "replaced_count": 2 }]),
        "{p}"
    );

    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_AFTER
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        ORDERS_BEFORE
    );

    // An exact idempotent replay returns the durable receipt without attempting
    // to reinterpret the now-retired node identity.
    let p2 = call_json(&cg, "tracedecay_rename_symbol", args).await;
    assert_eq!(p2["success"], true, "idempotent replay: {p2}");
    assert_eq!(p2["replayed"], true, "idempotent replay: {p2}");
    assert_eq!(
        p2["operation"], "use-case.application.source-edit.rename-symbol",
        "{p2}"
    );
    assert_eq!(p2["files"], json!(["src/pricing.rs"]), "{p2}");
    assert_eq!(p2["change_count"], 2, "{p2}");
    assert_eq!(p2["finding_count"], 0, "{p2}");
    assert_eq!(p2["durable_metadata_only"], true, "{p2}");
    assert_eq!(
        p2["message"], "source edit completed; detailed edit output was not retained",
        "{p2}"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_AFTER,
        "replay must not rewrite the applied source"
    );
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
    let p = call_json(&cg, "tracedecay_rename_symbol", args).await;
    assert_eq!(p["success"], false, "stale evidence must refuse: {p}");
    assert_eq!(
        p["message"], BLOCKED_MESSAGE,
        "stale evidence must refuse: {p}"
    );
    assert_eq!(
        visible_hazards(&p),
        json!([
            {
                "kind": "stale_evidence",
                "blocking": true,
                "message": "src/pricing.rs no longer matches the admitted graph generation"
            }
        ]),
        "{p}"
    );
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

    let node = preview_node(&cg, "compute_grand_total").await;

    // A denied preview has no acceptance to apply.
    let invalid = rename_args(&node, "not an identifier");
    let p = call_json(&cg, "tracedecay_rename_symbol", invalid).await;
    assert_eq!(p["success"], false, "invalid name must be denied: {p}");
    assert_eq!(p["dry_run"], true, "{p}");
    assert_eq!(p["new_name"], "not an identifier");
    assert_eq!(
        p["message"], "rename requires valid old and new identifiers",
        "{p}"
    );
    assert_eq!(
        visible_hazards(&p),
        json!([{
            "kind": "invalid_identifier",
            "blocking": true,
            "message": "rename requires valid old and new identifiers"
        }]),
        "{p}"
    );

    // Identical to the old name.
    let same = rename_args(&node, "compute_grand_total");
    let p = call_json(&cg, "tracedecay_rename_symbol", same).await;
    assert_eq!(p["success"], false, "same-name rename must be denied: {p}");
    assert_eq!(
        p["message"], "new name is identical to the bound old name",
        "{p}"
    );
    assert_eq!(
        visible_hazards(&p),
        json!([{
            "kind": "invalid_identifier",
            "blocking": true,
            "message": "new name is identical to the bound old name"
        }]),
        "{p}"
    );

    // Collides with an identifier already present in a touched file.
    let collision_message = "`tally` already occurs in src/pricing.rs; collision, shadowing, or changed resolution is possible";
    let collision = rename_args(&node, "tally");
    let p = call_json(&cg, "tracedecay_rename_symbol", collision).await;
    assert_eq!(p["success"], false, "collision must be denied: {p}");
    assert_eq!(p["message"], BLOCKED_MESSAGE, "{p}");
    assert_eq!(p["new_name"], "tally");
    assert_eq!(
        visible_hazards(&p),
        json!([
            {
                "kind": "namespace_collision",
                "blocking": true,
                "message": collision_message
            },
            {
                "kind": "shadowing",
                "blocking": true,
                "message": collision_message
            },
            {
                "kind": "changed_resolution",
                "blocking": true,
                "message": collision_message
            }
        ]),
        "{p}"
    );

    assert_workspace_unchanged(project);
}

#[tokio::test]
async fn test_rename_symbol_blocks_unresolved_cross_module_spelling() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    rename_fixture(project).await;
    fs::write(project.join("src/nested/orders.rs"), ORDERS_CROSS_MODULE).unwrap();
    let (cg, _env) = init_test_project(project).await;

    let node = preview_node(&cg, "compute_grand_total").await;
    let payload = call_json(
        &cg,
        "tracedecay_rename_symbol",
        rename_args(&node, "calculate_total_cents"),
    )
    .await;

    assert_eq!(payload["success"], false, "unresolved spelling: {payload}");
    assert_eq!(payload["dry_run"], true, "{payload}");
    assert_eq!(payload["message"], BLOCKED_MESSAGE, "{payload}");
    assert_eq!(
        matching(&visible_sites(&payload), |site| {
            site["file"] == "src/nested/orders.rs"
        }),
        json!([
            {
                "kind": "unresolved_text",
                "disposition": "blocked",
                "file": "src/nested/orders.rs",
                "line": 2,
                "expected_bytes": "compute_grand_total",
                "replacement_bytes": "compute_grand_total",
                "reason": "unresolved code spelling may bind this symbol"
            },
            {
                "kind": "unresolved_text",
                "disposition": "blocked",
                "file": "src/nested/orders.rs",
                "line": 5,
                "expected_bytes": "compute_grand_total",
                "replacement_bytes": "compute_grand_total",
                "reason": "unresolved code spelling may bind this symbol"
            }
        ]),
        "{payload}"
    );
    assert_eq!(
        matching(&visible_hazards(&payload), |hazard| {
            hazard["kind"] == "ambiguous_symbol"
        }),
        json!([
            {
                "kind": "ambiguous_symbol",
                "blocking": true,
                "message": "unresolved code spelling may bind this symbol"
            },
            {
                "kind": "ambiguous_symbol",
                "blocking": true,
                "message": "unresolved code spelling may bind this symbol"
            }
        ]),
        "{payload}"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_BEFORE
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        ORDERS_CROSS_MODULE
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

    let node = preview_node(&cg, "compute_grand_total").await;
    let preview = preview_rename(&cg, &node, "calculate_total_cents").await;

    // `src/` read-only blocks the temp-file publish of `src/pricing.rs`.
    // The guard restores write permission even if the tool call panics, so
    // the temp directory can still be removed.
    let src_dir = project.join("src");
    let writable = fs::metadata(&src_dir).unwrap().permissions();
    fs::set_permissions(&src_dir, fs::Permissions::from_mode(0o555)).unwrap();
    let _restore = RestoreWrite {
        path: src_dir,
        permissions: writable,
    };

    let args = accepted_apply_args(
        &node,
        "calculate_total_cents",
        &preview,
        "rename.publication-failure",
    );
    // Publication refusal is a typed tool result, not a successful rename.
    let p = call_json(&cg, "tracedecay_rename_symbol", args).await;
    assert_eq!(p["success"], false, "payload: {p}");

    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_BEFORE,
        "declaration file must be untouched"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/nested/orders.rs")).unwrap(),
        ORDERS_BEFORE,
        "published caller must be rolled back to its preimage"
    );
}

#[cfg(unix)]
struct RestoreWrite {
    path: std::path::PathBuf,
    permissions: fs::Permissions,
}

#[cfg(unix)]
impl Drop for RestoreWrite {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, self.permissions.clone()).unwrap();
    }
}
