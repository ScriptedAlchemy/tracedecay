//! `tracedecay_move_symbol` as an MCP client sees it: a JSON-RPC `tools/call`
//! against the production project server, with the exact preview, the exact
//! files an apply writes, and the exact refusal text.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use crate::support::{
    handle_real_server_tool_call_raw, init_production_source_edit_project, test_temp_dir,
};

const LIB_RS: &str = "pub mod pricing;\npub mod orders;\n";
const PRICING_RS: &str = concat!(
    "//! pricing\n",
    "pub struct LineItem {\n",
    "    pub unit_price: u64,\n",
    "    pub quantity: u32,\n",
    "}\n",
    "\n",
    "/// Grand total in cents.\n",
    "pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n",
    "    let mut total = 0u64;\n",
    "    for item in items {\n",
    "        total += item.unit_price * item.quantity as u64;\n",
    "    }\n",
    "    total\n",
    "}\n",
);
const ORDERS_RS: &str = concat!(
    "//! orders\n",
    "use crate::pricing::{compute_grand_total, LineItem};\n",
    "\n",
    "pub fn tally(items: &[LineItem]) -> u64 {\n",
    "    compute_grand_total(items)\n",
    "}\n",
);
const PRICING_AFTER_MOVE: &str = concat!(
    "//! pricing\n",
    "pub struct LineItem {\n",
    "    pub unit_price: u64,\n",
    "    pub quantity: u32,\n",
    "}\n",
);
const MOVED_SPAN: &str = concat!(
    "/// Grand total in cents.\n",
    "pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n",
    "    let mut total = 0u64;\n",
    "    for item in items {\n",
    "        total += item.unit_price * item.quantity as u64;\n",
    "    }\n",
    "    total\n",
    "}",
);
const GRAND_TOTAL_RS: &str = concat!(
    "use crate::pricing::LineItem;\n\n",
    "/// Grand total in cents.\n",
    "pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n",
    "    let mut total = 0u64;\n",
    "    for item in items {\n",
    "        total += item.unit_price * item.quantity as u64;\n",
    "    }\n",
    "    total\n",
    "}\n",
);
const PREVIEW_DIFF: &str = concat!(
    "--- src/pricing.rs (source, remove)\n",
    "@@ -3,12 +3,3 @@\n",
    "     pub unit_price: u64,\n",
    "     pub quantity: u32,\n",
    " }\n",
    "-\n",
    "-/// Grand total in cents.\n",
    "-pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n",
    "-    let mut total = 0u64;\n",
    "-    for item in items {\n",
    "-        total += item.unit_price * item.quantity as u64;\n",
    "-    }\n",
    "-    total\n",
    "-}\n",
    "\n",
    "+++ src/grand_total.rs (destination, insert)\n",
    "@@ -1,0 +1,10 @@\n",
    "+use crate::pricing::LineItem;\n",
    "+\n",
    "+/// Grand total in cents.\n",
    "+pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n",
    "+    let mut total = 0u64;\n",
    "+    for item in items {\n",
    "+        total += item.unit_price * item.quantity as u64;\n",
    "+    }\n",
    "+    total\n",
    "+}",
);

fn write_pricing_crate(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/pricing.rs"), PRICING_RS).unwrap();
    fs::write(project.join("src/orders.rs"), ORDERS_RS).unwrap();
}

fn assert_pricing_crate_unchanged(project: &Path) {
    assert_eq!(
        fs::read_to_string(project.join("src/lib.rs")).unwrap(),
        LIB_RS
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_RS
    );
    assert_eq!(
        fs::read_to_string(project.join("src/orders.rs")).unwrap(),
        ORDERS_RS
    );
    assert!(!project.join("src/grand_total.rs").exists());
}

/// Drops receipt identities and candidate-state digests, which are hashes of
/// the live workspace, and returns the preview digest the caller must pass
/// back to apply.
fn stable_payload(text: &str) -> (String, Value) {
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("move_symbol text was not JSON: {error}\n{text}"));
    let mut object = match payload {
        Value::Object(object) => object,
        other => panic!("move_symbol payload was not an object: {other}"),
    };
    let expected_value = object.remove("expected_state");
    let expected_state = expected_value
        .as_ref()
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("move_symbol omitted expected_state: {object:?}"));
    assert!(
        expected_state.len() == "sha256:".len() + 64
            && expected_state.starts_with("sha256:")
            && expected_state[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "expected_state must be a sha256 digest, got {expected_state}"
    );
    object.remove("predicted_state");
    object.remove("effect");
    (expected_state, Value::Object(object))
}

async fn call_move_symbol(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    let response =
        handle_real_server_tool_call_raw(server, "tracedecay_move_symbol", arguments).await;
    assert_eq!(response["jsonrpc"], json!("2.0"), "{response}");
    assert_eq!(response["id"], json!(1), "{response}");
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "tools/call failed at the protocol layer: {response}"
    );
    assert_eq!(
        response["result"]["content"][0]["type"],
        json!("text"),
        "{response}"
    );
    response["result"].clone()
}

#[tokio::test]
async fn dry_run_then_apply_moves_compute_grand_total() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    write_pricing_crate(project);
    let fixture = init_production_source_edit_project(project).await;
    let server = fixture
        .harness
        .server(project)
        .expect("production project server");

    let preview = call_move_symbol(
        &server,
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "format": "json"
        }),
    )
    .await;
    assert!(
        preview
            .get("isError")
            .is_none_or(|value| value == &json!(false))
    );
    let text = preview["content"][0]["text"].as_str().unwrap();
    let (expected_state, stable) = stable_payload(text);
    assert_eq!(
        stable,
        json!({
            "success": true,
            "symbol": "compute_grand_total (function)",
            "source_file": "src/pricing.rs",
            "dest_file": "src/grand_total.rs",
            "moved_span": MOVED_SPAN,
            "dry_run": true,
            "diff": PREVIEW_DIFF,
            "applied_imports": ["use crate::pricing::LineItem;"],
            "impact": [
                {
                    "kind": "caller_reference",
                    "file": "src/orders.rs",
                    "detail": "`tally` in src/orders.rs references `compute_grand_total` via `crate::pricing`; the path is now `crate::grand_total`",
                    "suggestion": "retarget the reference from `crate::pricing::compute_grand_total` to `crate::grand_total::compute_grand_total`"
                },
                {
                    "kind": "module_missing",
                    "file": "src/lib.rs",
                    "detail": "module `grand_total` for src/grand_total.rs is not declared in the crate",
                    "suggestion": "add `mod grand_total;` to src/lib.rs"
                }
            ],
            "message": "dry run. Nothing written; preview only (move previewed)",
            "replayed": false
        }),
        "preview: {text}"
    );
    assert_pricing_crate_unchanged(project);

    let applied = call_move_symbol(
        &server,
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "dry_run": false,
            "idempotency_key": "move-symbol-behavior-apply",
            "expected_state": expected_state,
            "format": "json"
        }),
    )
    .await;
    assert!(
        applied
            .get("isError")
            .is_none_or(|value| value == &json!(false))
    );
    let applied_text = applied["content"][0]["text"].as_str().unwrap();
    let (_committed, stable_applied) = stable_payload(applied_text);
    assert_eq!(
        stable_applied,
        json!({
            "success": true,
            "symbol": "compute_grand_total (function)",
            "source_file": "src/pricing.rs",
            "dest_file": "src/grand_total.rs",
            "moved_span": MOVED_SPAN,
            "applied_imports": ["use crate::pricing::LineItem;"],
            "impact": [
                {
                    "kind": "caller_reference",
                    "file": "src/orders.rs",
                    "detail": "`tally` in src/orders.rs references `compute_grand_total` via `crate::pricing`; the path is now `crate::grand_total`",
                    "suggestion": "retarget the reference from `crate::pricing::compute_grand_total` to `crate::grand_total::compute_grand_total`"
                },
                {
                    "kind": "module_missing",
                    "file": "src/lib.rs",
                    "detail": "module `grand_total` for src/grand_total.rs is not declared in the crate",
                    "suggestion": "add `mod grand_total;` to src/lib.rs"
                }
            ],
            "message": "move applied",
            "replayed": false
        }),
        "apply: {applied_text}"
    );
    assert_eq!(
        fs::read_to_string(project.join("src/pricing.rs")).unwrap(),
        PRICING_AFTER_MOVE
    );
    assert_eq!(
        fs::read_to_string(project.join("src/grand_total.rs")).unwrap(),
        GRAND_TOTAL_RS
    );
    assert_eq!(
        fs::read_to_string(project.join("src/lib.rs")).unwrap(),
        LIB_RS
    );
    assert_eq!(
        fs::read_to_string(project.join("src/orders.rs")).unwrap(),
        ORDERS_RS
    );
}

#[tokio::test]
async fn move_symbol_refuses_unsafe_or_stale_requests() {
    let dir = test_temp_dir();
    let project_root = dir.path().join("project");
    let project = project_root.as_path();
    write_pricing_crate(project);
    let fixture = init_production_source_edit_project(project).await;
    let server = fixture
        .harness
        .server(project)
        .expect("production project server");

    let missing = call_move_symbol(
        &server,
        json!({
            "symbol": "not_a_symbol",
            "dest_file": "src/grand_total.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(missing["isError"], json!(true), "{missing}");
    let missing_text = missing["content"][0]["text"].as_str().unwrap();
    let (_state, stable_missing) = stable_payload(missing_text);
    assert_eq!(
        stable_missing,
        json!({
            "success": false,
            "failed": true,
            "message": "source edit failed before the effect: config error: symbol 'not_a_symbol' not found",
            "replayed": false
        }),
        "missing symbol: {missing_text}"
    );
    assert_pricing_crate_unchanged(project);

    let escaped = call_move_symbol(
        &server,
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "../grand_total.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(escaped["isError"], json!(true), "{escaped}");
    let escaped_text = escaped["content"][0]["text"].as_str().unwrap();
    let (_state, stable_escaped) = stable_payload(escaped_text);
    assert_eq!(
        stable_escaped,
        json!({
            "success": false,
            "failed": true,
            "message": "source edit failed before the effect: config error: destination path must not contain '..'",
            "replayed": false
        }),
        "escaped destination: {escaped_text}"
    );
    assert_pricing_crate_unchanged(project);

    let same_file = call_move_symbol(
        &server,
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/pricing.rs",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(same_file["isError"], json!(true), "{same_file}");
    let same_file_text = same_file["content"][0]["text"].as_str().unwrap();
    let (_state, stable_same_file) = stable_payload(same_file_text);
    assert_eq!(
        stable_same_file,
        json!({
            "success": false,
            "symbol": "compute_grand_total (function)",
            "source_file": "src/pricing.rs",
            "dest_file": "src/pricing.rs",
            "dry_run": true,
            "message": "destination is the symbol's own file (src/pricing.rs); nothing to move",
            "replayed": false
        }),
        "same file: {same_file_text}"
    );
    assert_pricing_crate_unchanged(project);

    let preview = call_move_symbol(
        &server,
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "format": "json"
        }),
    )
    .await;
    let preview_text = preview["content"][0]["text"].as_str().unwrap();
    let (_state, _) = stable_payload(preview_text);
    let stale = call_move_symbol(
        &server,
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "dry_run": false,
            "idempotency_key": "move-symbol-behavior-stale",
            "expected_state": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "format": "json"
        }),
    )
    .await;
    assert_eq!(stale["isError"], json!(true), "{stale}");
    let stale_text = stale["content"][0]["text"].as_str().unwrap();
    let (_state, stable_stale) = stable_payload(stale_text);
    assert_eq!(
        stable_stale,
        json!({
            "success": false,
            "failed": true,
            "message": "source edit failed before the effect",
            "replayed": false
        }),
        "stale expected_state: {stale_text}"
    );
    assert_pricing_crate_unchanged(project);
}
