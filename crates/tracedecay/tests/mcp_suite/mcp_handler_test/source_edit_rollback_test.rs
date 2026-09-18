//! Production MCP behavior for `tracedecay_source_edit_rollback`.
//!
//! Calls go through the daemon-owned source-edit server, the same dispatch a
//! host uses. Assertions are the workspace bytes and the tool payload a caller
//! reads back, not the catalog schema.

use crate::support::{
    expect_tool_error, extract_text, handle_production_source_edit_tool_call as handle_tool_call,
    init_production_source_edit_project as init_test_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tracedecay_mcp::ToolResult;

const LIB_RS: &str = "pub mod pricing;\npub mod orders;\n";
const PRICING_RS: &str = "//! pricing\n\
    pub struct LineItem {\n    pub unit_price: u64,\n    pub quantity: u32,\n}\n\n\
    /// Grand total in cents.\n\
    pub fn compute_grand_total(items: &[LineItem]) -> u64 {\n\
    \x20   let mut total = 0u64;\n\
    \x20   for item in items {\n\
    \x20       total += item.unit_price * item.quantity as u64;\n\
    \x20   }\n\
    \x20   total\n\
    }\n";
const ORDERS_RS: &str = "//! orders\n\
    use crate::pricing::{compute_grand_total, LineItem};\n\n\
    pub fn tally(items: &[LineItem]) -> u64 {\n    compute_grand_total(items)\n}\n";
const ORDERS_AFTER_REPLACE: &str = "//! orders\n\
    use crate::pricing::{compute_grand_total, LineItem};\n\n\
    pub fn tally(items: &[LineItem]) -> u64 {\n    compute_grand_total(items) // kept\n}\n";
const STALE_DESTINATION: &[u8] = b"fn foreign_bytes() {}\n";
const FOREIGN_INPUT_DIGEST: &str =
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn tool_json(result: &ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!(
            "tool payload was not JSON: {error}\n{text}\n{}",
            result.value
        )
    })
}

fn write_pricing_project(project: &Path) {
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/pricing.rs"), PRICING_RS).unwrap();
    fs::write(project.join("src/orders.rs"), ORDERS_RS).unwrap();
}

fn rollback_args(
    moved: &Value,
    original_idempotency_key: &str,
    idempotency_key: &str,
    original_input_digest: &str,
) -> Value {
    json!({
        "effect_id": moved["effect"]["effect_id"],
        "original_idempotency_key": original_idempotency_key,
        "idempotency_key": idempotency_key,
        "original_input_digest": original_input_digest,
        "expected_state": moved["effect"]["receipt"]["committed_state"],
        "confirm": true
    })
}

#[tokio::test]
async fn source_edit_rollback_restores_retained_move_preimages_and_replays_the_receipt() {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    write_pricing_project(&project);
    let (fixture, _env) = init_test_project(&project).await;
    let pricing = project.join("src/pricing.rs");
    let orders = project.join("src/orders.rs");
    let library = project.join("src/lib.rs");
    let destination = project.join("src/grand_total.rs");

    let moved = handle_tool_call(
        &fixture,
        "tracedecay_move_symbol",
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "dry_run": false,
            "idempotency_key": "mcp-test.source-edit-rollback.move"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let moved = tool_json(&moved);
    assert_eq!(moved["success"], true, "{moved}");
    assert_eq!(moved["message"], "move applied", "{moved}");
    assert_eq!(moved["replayed"], false, "{moved}");
    assert_eq!(
        moved["effect"]["receipt"]["outcome"], "completed",
        "{moved}"
    );
    assert_eq!(
        moved["effect"]["receipt"]["operation"], "use-case.application.source-edit.move-symbol",
        "{moved}"
    );
    assert_eq!(
        moved["effect"]["idempotency_key"], "mcp-test.source-edit-rollback.move",
        "{moved}"
    );
    let pricing_after_move = fs::read(&pricing).unwrap();
    assert_ne!(pricing_after_move, PRICING_RS.as_bytes());
    assert!(
        !String::from_utf8(pricing_after_move)
            .unwrap()
            .contains("pub fn compute_grand_total"),
        "move must take the function out of pricing.rs before rollback can restore it"
    );
    let destination_after_move = fs::read_to_string(&destination).unwrap();
    assert!(
        destination_after_move.contains("pub fn compute_grand_total"),
        "destination before rollback:\n{destination_after_move}"
    );

    let rolled = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            &moved,
            "mcp-test.source-edit-rollback.move",
            "mcp-test.source-edit-rollback.restore",
            moved["effect"]["receipt"]["input_digest"].as_str().unwrap(),
        ),
        None,
        None,
    )
    .await
    .unwrap();
    let rolled = tool_json(&rolled);

    assert_eq!(fs::read(&pricing).unwrap(), PRICING_RS.as_bytes());
    assert_eq!(fs::read(&orders).unwrap(), ORDERS_RS.as_bytes());
    assert_eq!(fs::read(&library).unwrap(), LIB_RS.as_bytes());
    assert!(
        !destination.exists(),
        "rollback must delete the file the move created"
    );
    assert_eq!(rolled["success"], true, "{rolled}");
    assert_eq!(rolled["reconciled"], true, "{rolled}");
    assert_eq!(rolled["replayed"], false, "{rolled}");
    assert_eq!(
        rolled["message"], "source edit rollback restored every retained preimage",
        "{rolled}"
    );
    assert_eq!(rolled["effect"]["effect_class"], "source_edit", "{rolled}");
    assert_eq!(
        rolled["effect"]["idempotency_key"], "mcp-test.source-edit-rollback.restore",
        "{rolled}"
    );
    assert_eq!(rolled["effect"]["reconciliation"], "reconciled", "{rolled}");
    assert_eq!(
        rolled["effect"]["receipt"]["outcome"], "completed",
        "{rolled}"
    );
    assert_eq!(
        rolled["effect"]["receipt"]["operation"], "use-case.application.source-edit.rollback",
        "{rolled}"
    );
    assert_eq!(
        rolled["effect"]["receipt"]["idempotency_key"], "mcp-test.source-edit-rollback.restore",
        "{rolled}"
    );
    assert_eq!(
        rolled["expected_state"], moved["effect"]["receipt"]["committed_state"],
        "{rolled}"
    );
    assert_eq!(
        rolled["predicted_state"], moved["effect"]["receipt"]["expected_state"],
        "{rolled}"
    );
    assert_eq!(
        rolled["effect"]["receipt"]["committed_state"],
        moved["effect"]["receipt"]["expected_state"],
        "{rolled}"
    );
    assert_eq!(rolled["effect"]["payload"]["success"], true, "{rolled}");
    assert_eq!(rolled["effect"]["payload"]["reconciled"], true, "{rolled}");
    assert_eq!(
        rolled["effect"]["payload"]["durable_metadata_only"], true,
        "{rolled}"
    );
    assert_eq!(
        rolled["effect"]["payload"]["operation"], "use-case.application.source-edit.rollback",
        "{rolled}"
    );
    assert_eq!(
        rolled["effect"]["payload"]["message"], "source edit reconciliation completed",
        "{rolled}"
    );
    assert_eq!(rolled["effect"]["payload"]["files"], json!([]), "{rolled}");

    let replay = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            &moved,
            "mcp-test.source-edit-rollback.move",
            "mcp-test.source-edit-rollback.restore",
            moved["effect"]["receipt"]["input_digest"].as_str().unwrap(),
        ),
        None,
        None,
    )
    .await
    .unwrap();
    let replay = tool_json(&replay);

    // Replay reads the durable receipt. That receipt keeps reconciled metadata,
    // so the caller sees the retained sentence instead of the live restore one.
    assert_eq!(fs::read(&pricing).unwrap(), PRICING_RS.as_bytes());
    assert_eq!(fs::read(&orders).unwrap(), ORDERS_RS.as_bytes());
    assert_eq!(fs::read(&library).unwrap(), LIB_RS.as_bytes());
    assert!(!destination.exists());
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(
        replay["message"], "source edit reconciliation completed",
        "{replay}"
    );
    assert_eq!(
        replay["effect"]["effect_id"], rolled["effect"]["effect_id"],
        "{replay}"
    );
    assert_eq!(
        replay["effect"]["idempotency_key"], "mcp-test.source-edit-rollback.restore",
        "{replay}"
    );
    assert_eq!(
        replay["effect"]["receipt"]["outcome"], "completed",
        "{replay}"
    );
    assert_eq!(
        replay["effect"]["receipt"]["committed_state"],
        rolled["effect"]["receipt"]["committed_state"],
        "{replay}"
    );
}

#[tokio::test]
async fn source_edit_rollback_refuses_stale_workspace_bytes() {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    write_pricing_project(&project);
    let (fixture, _env) = init_test_project(&project).await;
    let pricing = project.join("src/pricing.rs");
    let destination = project.join("src/grand_total.rs");

    let moved = handle_tool_call(
        &fixture,
        "tracedecay_move_symbol",
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "dry_run": false,
            "idempotency_key": "mcp-test.source-edit-rollback.stale-move"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let moved = tool_json(&moved);
    assert_eq!(moved["success"], true, "{moved}");
    let pricing_after_move = fs::read(&pricing).unwrap();
    fs::write(&destination, STALE_DESTINATION).unwrap();

    let refused = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            &moved,
            "mcp-test.source-edit-rollback.stale-move",
            "mcp-test.source-edit-rollback.stale",
            moved["effect"]["receipt"]["input_digest"].as_str().unwrap(),
        ),
        None,
        None,
    )
    .await
    .unwrap();
    let refused = tool_json(&refused);

    assert_eq!(fs::read(&destination).unwrap(), STALE_DESTINATION);
    assert_eq!(fs::read(&pricing).unwrap(), pricing_after_move);
    assert_ne!(pricing_after_move, PRICING_RS.as_bytes());
    assert_eq!(refused["success"], false, "{refused}");
    assert_eq!(refused["failed"], true, "{refused}");
    assert_eq!(refused["replayed"], false, "{refused}");
    assert_eq!(
        refused["message"], "source edit rollback refused stale or foreign workspace bytes",
        "{refused}"
    );
    assert_eq!(
        refused["effect"]["effect_class"], "source_edit",
        "{refused}"
    );
    assert_eq!(
        refused["effect"]["idempotency_key"], "mcp-test.source-edit-rollback.stale",
        "{refused}"
    );
    assert_eq!(
        refused["effect"]["receipt"]["outcome"], "failed",
        "{refused}"
    );
    assert!(
        refused["effect"]["receipt"]["committed_state"].is_null(),
        "{refused}"
    );
    assert_eq!(
        refused["expected_state"], moved["effect"]["receipt"]["committed_state"],
        "{refused}"
    );
}

#[tokio::test]
async fn source_edit_rollback_refuses_unconfirmed_duplicate_keys_and_non_move_effects() {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    write_pricing_project(&project);
    let (fixture, _env) = init_test_project(&project).await;
    let pricing = project.join("src/pricing.rs");
    let orders = project.join("src/orders.rs");

    let unconfirmed = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        json!({ "confirm": false }),
        None,
        None,
    )
    .await;
    assert_eq!(
        expect_tool_error(unconfirmed),
        "config error: source edit rollback requires confirm=true from the caller after it checks the receipt; do not pause for a human"
    );
    assert_eq!(fs::read(&pricing).unwrap(), PRICING_RS.as_bytes());
    assert_eq!(fs::read(&orders).unwrap(), ORDERS_RS.as_bytes());

    let replaced = handle_tool_call(
        &fixture,
        "tracedecay_str_replace",
        json!({
            "path": "src/orders.rs",
            "old_str": "compute_grand_total(items)",
            "new_str": "compute_grand_total(items) // kept",
            "idempotency_key": "mcp-test.source-edit-rollback.non-move"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let replaced = tool_json(&replaced);
    assert_eq!(replaced["success"], true, "{replaced}");
    assert_eq!(fs::read(&orders).unwrap(), ORDERS_AFTER_REPLACE.as_bytes());

    let non_move = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            &replaced,
            "mcp-test.source-edit-rollback.non-move",
            "mcp-test.source-edit-rollback.non-move-rollback",
            replaced["effect"]["receipt"]["input_digest"]
                .as_str()
                .unwrap(),
        ),
        None,
        None,
    )
    .await;
    assert_eq!(
        expect_tool_error(non_move),
        "project route error (source_edit.execution_failed): config error: source edit effect has no retained rollback material"
    );
    assert_eq!(fs::read(&orders).unwrap(), ORDERS_AFTER_REPLACE.as_bytes());
    assert_eq!(fs::read(&pricing).unwrap(), PRICING_RS.as_bytes());

    let moved = handle_tool_call(
        &fixture,
        "tracedecay_move_symbol",
        json!({
            "symbol": "compute_grand_total",
            "dest_file": "src/grand_total.rs",
            "dry_run": false,
            "idempotency_key": "mcp-test.source-edit-rollback.identity-move"
        }),
        None,
        None,
    )
    .await
    .unwrap();
    let moved = tool_json(&moved);
    assert_eq!(moved["success"], true, "{moved}");
    let pricing_after_move = fs::read(&pricing).unwrap();
    assert_ne!(pricing_after_move, PRICING_RS.as_bytes());

    let same_key = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            &moved,
            "mcp-test.source-edit-rollback.identity-move",
            "mcp-test.source-edit-rollback.identity-move",
            moved["effect"]["receipt"]["input_digest"].as_str().unwrap(),
        ),
        None,
        None,
    )
    .await;
    assert_eq!(
        expect_tool_error(same_key),
        "config error: rollback idempotency key must differ from the original edit key"
    );
    assert_eq!(fs::read(&pricing).unwrap(), pricing_after_move);

    let mismatched = handle_tool_call(
        &fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            &moved,
            "mcp-test.source-edit-rollback.identity-move",
            "mcp-test.source-edit-rollback.mismatch",
            FOREIGN_INPUT_DIGEST,
        ),
        None,
        None,
    )
    .await;
    assert_eq!(
        expect_tool_error(mismatched),
        "project route error (source_edit.execution_failed): config error: source edit rollback identity does not match the completed original effect"
    );
    assert_eq!(fs::read(&pricing).unwrap(), pricing_after_move);
    let destination = fs::read_to_string(project.join("src/grand_total.rs")).unwrap();
    assert!(
        destination.contains("pub fn compute_grand_total"),
        "identity mismatch must leave the moved function in place:\n{destination}"
    );
}
