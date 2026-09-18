//! `tracedecay_source_edit_rollback` through the production MCP `tools/call`
//! path. Callers pass the completed move receipt; the tool restores retained
//! preimages or refuses, and exact retries replay that receipt.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, handle_real_server_tool_call_raw,
    init_production_source_edit_project, test_temp_dir, warm_code_index_search,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const LIB_RS: &str = "pub mod source;\npub mod dest;\n";
const SOURCE_RS: &str = "//! source\n\npub fn rollback_anchor() -> i32 {\n    7\n}\n";
const DEST_RS: &str = "//! dest\n\npub fn dest_marker() -> i32 {\n    0\n}\n";
const ANCHOR: &str = "pub fn rollback_anchor() -> i32 {\n    7\n}";
const MOVE_KEY: &str = "mcp-test.source-edit.move.rollback-anchor";
const FOREIGN_DEST: &str = "//! dest\n\npub fn peer_bytes() -> i32 {\n    9\n}\n";

struct MovedProject {
    _dir: TestTempDir,
    fixture: ProductionSourceEditFixture,
    project: PathBuf,
    effect_id: String,
    input_digest: String,
    committed_state: String,
    prior_expected_state: String,
}

fn read_project_file(project: &Path, relative: &str) -> String {
    fs::read_to_string(project.join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn assert_original_sources(project: &Path) {
    assert_eq!(read_project_file(project, "src/lib.rs"), LIB_RS);
    assert_eq!(read_project_file(project, "src/source.rs"), SOURCE_RS);
    assert_eq!(read_project_file(project, "src/dest.rs"), DEST_RS);
}

fn require_str<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing {pointer} in {value}"))
}

fn tool_payload(response: &Value) -> Value {
    assert_eq!(response["jsonrpc"], "2.0", "{response}");
    assert!(
        response.get("error").is_none_or(Value::is_null),
        "{response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP tool result had no text: {response}"));
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

async fn call_tool(fixture: &ProductionSourceEditFixture, name: &str, arguments: Value) -> Value {
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    handle_real_server_tool_call_raw(&server, name, arguments).await
}

fn rollback_args(
    idempotency_key: &str,
    effect_id: &str,
    input_digest: &str,
    expected_state: &str,
    confirm: bool,
) -> Value {
    json!({
        "effect_id": effect_id,
        "original_idempotency_key": MOVE_KEY,
        "idempotency_key": idempotency_key,
        "original_input_digest": input_digest,
        "expected_state": expected_state,
        "confirm": confirm,
        "format": "json",
    })
}

async fn open_moved_project() -> MovedProject {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), LIB_RS).unwrap();
    fs::write(project.join("src/source.rs"), SOURCE_RS).unwrap();
    fs::write(project.join("src/dest.rs"), DEST_RS).unwrap();
    let (fixture, _) = init_production_source_edit_project(&project).await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    warm_code_index_search(&server, "rollback_anchor").await;

    let preview = tool_payload(
        &call_tool(
            &fixture,
            "tracedecay_move_symbol",
            json!({
                "symbol": "rollback_anchor",
                "dest_file": "src/dest.rs",
                "dry_run": true,
                "format": "json",
            }),
        )
        .await,
    );
    let expected_state = require_str(&preview, "/expected_state").to_owned();
    let applied = tool_payload(
        &call_tool(
            &fixture,
            "tracedecay_move_symbol",
            json!({
                "symbol": "rollback_anchor",
                "dest_file": "src/dest.rs",
                "dry_run": false,
                "idempotency_key": MOVE_KEY,
                "expected_state": expected_state,
                "format": "json",
            }),
        )
        .await,
    );
    assert_eq!(applied["success"], json!(true), "{applied}");
    assert_eq!(applied["replayed"], json!(false), "{applied}");
    assert_eq!(applied["effect"]["idempotency_key"], MOVE_KEY, "{applied}");
    assert_eq!(
        applied["effect"]["receipt"]["outcome"],
        json!("completed"),
        "{applied}"
    );
    assert!(
        !read_project_file(&project, "src/source.rs").contains(ANCHOR),
        "move must remove the anchor before rollback can restore it"
    );
    assert!(
        read_project_file(&project, "src/dest.rs").contains(ANCHOR),
        "move must retain the anchor text in the destination"
    );

    MovedProject {
        _dir: dir,
        fixture,
        project,
        effect_id: require_str(&applied, "/effect/effect_id").to_owned(),
        input_digest: require_str(&applied, "/effect/receipt/input_digest").to_owned(),
        committed_state: require_str(&applied, "/effect/receipt/committed_state").to_owned(),
        prior_expected_state: require_str(&applied, "/effect/receipt/expected_state").to_owned(),
    }
}

#[tokio::test]
async fn source_edit_rollback_restores_move_preimages_and_replays_the_receipt() {
    let moved = open_moved_project().await;

    let unconfirmed = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            "mcp-test.source-edit.rollback.unconfirmed",
            &moved.effect_id,
            &moved.input_digest,
            &moved.committed_state,
            false,
        ),
    )
    .await;
    assert_eq!(unconfirmed["jsonrpc"], "2.0");
    assert_eq!(unconfirmed["id"], 1);
    assert!(unconfirmed.get("result").is_none_or(Value::is_null));
    assert_eq!(unconfirmed["error"]["code"], -32603);
    assert_eq!(
        unconfirmed["error"]["message"],
        "tool execution failed: config error: source edit rollback requires confirm=true from the caller after it checks the receipt; do not pause for a human"
    );
    assert_eq!(
        unconfirmed["error"]["data"]["tool"],
        "tracedecay_source_edit_rollback"
    );
    assert!(
        !read_project_file(&moved.project, "src/source.rs").contains(ANCHOR),
        "a refused confirmation must not restore the source preimage"
    );

    let same_key = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            MOVE_KEY,
            &moved.effect_id,
            &moved.input_digest,
            &moved.committed_state,
            true,
        ),
    )
    .await;
    assert_eq!(same_key["error"]["code"], -32603);
    assert_eq!(
        same_key["error"]["message"],
        "tool execution failed: config error: rollback idempotency key must differ from the original edit key"
    );
    assert!(
        read_project_file(&moved.project, "src/dest.rs").contains(ANCHOR),
        "a reused move key must not roll the destination back"
    );

    let restored_response = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            "mcp-test.source-edit.rollback.anchor",
            &moved.effect_id,
            &moved.input_digest,
            &moved.committed_state,
            true,
        ),
    )
    .await;
    let restored = tool_payload(&restored_response);
    assert_eq!(restored["success"], json!(true));
    assert_eq!(restored["reconciled"], json!(true));
    assert_eq!(restored["replayed"], json!(false));
    assert_eq!(
        restored["message"],
        "source edit rollback restored every retained preimage"
    );
    assert_eq!(restored["expected_state"], moved.committed_state);
    assert_eq!(restored["predicted_state"], moved.prior_expected_state);
    assert_eq!(restored["effect"]["effect_class"], "source_edit");
    assert_eq!(
        restored["effect"]["idempotency_key"],
        "mcp-test.source-edit.rollback.anchor"
    );
    assert_eq!(restored["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(restored["effect"]["receipt"]["effect_class"], "source_edit");
    assert_eq!(restored["effect"]["reconciliation"], "reconciled");
    assert_eq!(
        restored["effect"]["payload"]["operation"],
        "use-case.application.source-edit.rollback"
    );
    assert_eq!(restored["effect"]["payload"]["files"], json!([]));
    assert_eq!(restored["effect"]["payload"]["reconciled"], json!(true));
    assert_eq!(
        restored["effect"]["payload"]["durable_metadata_only"],
        json!(true)
    );
    assert_eq!(
        restored["effect"]["payload"]["message"],
        "source edit reconciliation completed"
    );
    assert_original_sources(&moved.project);
    let rollback_effect_id = require_str(&restored, "/effect/effect_id").to_owned();

    let replay_response = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            "mcp-test.source-edit.rollback.anchor",
            &moved.effect_id,
            &moved.input_digest,
            &moved.committed_state,
            true,
        ),
    )
    .await;
    let replay = tool_payload(&replay_response);
    assert_eq!(replay["replayed"], json!(true));
    assert_eq!(replay["success"], json!(true));
    assert_eq!(replay["failed"], json!(false));
    assert_eq!(replay["message"], "source edit reconciliation completed");
    assert_eq!(replay["expected_state"], moved.committed_state);
    assert_eq!(replay["predicted_state"], moved.prior_expected_state);
    assert_eq!(replay["effect"]["effect_id"], rollback_effect_id);
    assert_eq!(
        replay["effect"]["idempotency_key"],
        "mcp-test.source-edit.rollback.anchor"
    );
    assert_eq!(replay["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(
        replay["effect"]["receipt"]["committed_state"],
        moved.prior_expected_state
    );
    assert_eq!(
        replay["effect"]["payload"]["durable_metadata_only"],
        json!(true)
    );
    assert_eq!(replay["effect"]["payload"]["files"], json!([]));
    assert_eq!(replay["effect"]["payload"]["reconciled"], json!(true));
    assert_eq!(replay["effect"]["payload"]["success"], json!(true));
    assert_original_sources(&moved.project);

    let mismatched = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            "mcp-test.source-edit.rollback.mismatch",
            &moved.effect_id,
            &format!("sha256:{}", "0".repeat(64)),
            &moved.committed_state,
            true,
        ),
    )
    .await;
    assert_eq!(mismatched["jsonrpc"], "2.0");
    assert_eq!(mismatched["id"], 1);
    assert!(mismatched.get("result").is_none_or(Value::is_null));
    assert_eq!(mismatched["error"]["code"], -32602);
    assert_eq!(
        mismatched["error"]["message"],
        "tool project route failed: reason_code=source_edit.execution_failed retryable=false: config error: source edit rollback identity does not match the completed original effect"
    );
    assert_eq!(
        mismatched["error"]["data"]["tool"],
        "tracedecay_source_edit_rollback"
    );
    assert_eq!(
        mismatched["error"]["data"]["reason_code"],
        "source_edit.execution_failed"
    );
    assert_eq!(mismatched["error"]["data"]["retryable"], json!(false));
    assert_eq!(
        mismatched["error"]["data"]["detail"],
        "config error: source edit rollback identity does not match the completed original effect"
    );
    assert_original_sources(&moved.project);
}

#[tokio::test]
async fn source_edit_rollback_keeps_foreign_bytes_and_replays_the_refusal() {
    let moved = open_moved_project().await;
    let source_after_move = read_project_file(&moved.project, "src/source.rs");
    fs::write(moved.project.join("src/dest.rs"), FOREIGN_DEST).unwrap();

    let refused_response = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            "mcp-test.source-edit.rollback.foreign",
            &moved.effect_id,
            &moved.input_digest,
            &moved.committed_state,
            true,
        ),
    )
    .await;
    assert_eq!(refused_response["jsonrpc"], "2.0");
    assert!(refused_response.get("error").is_none_or(Value::is_null));
    assert_eq!(refused_response["result"]["isError"], json!(true));
    let refused = tool_payload(&refused_response);
    assert_eq!(refused["success"], json!(false));
    assert_eq!(refused["failed"], json!(true));
    assert_eq!(refused["replayed"], json!(false));
    assert_eq!(
        refused["message"],
        "source edit rollback refused stale or foreign workspace bytes"
    );
    assert_eq!(refused["expected_state"], moved.committed_state);
    assert_eq!(refused["effect"]["effect_class"], "source_edit");
    assert_eq!(
        refused["effect"]["idempotency_key"],
        "mcp-test.source-edit.rollback.foreign"
    );
    assert_eq!(refused["effect"]["receipt"]["outcome"], "failed");
    assert_eq!(refused["effect"]["reconciliation"], "reconciled");
    assert_eq!(
        read_project_file(&moved.project, "src/source.rs"),
        source_after_move
    );
    assert_eq!(
        read_project_file(&moved.project, "src/dest.rs"),
        FOREIGN_DEST
    );
    assert_eq!(read_project_file(&moved.project, "src/lib.rs"), LIB_RS);
    let refusal_effect_id = require_str(&refused, "/effect/effect_id").to_owned();

    let replay_response = call_tool(
        &moved.fixture,
        "tracedecay_source_edit_rollback",
        rollback_args(
            "mcp-test.source-edit.rollback.foreign",
            &moved.effect_id,
            &moved.input_digest,
            &moved.committed_state,
            true,
        ),
    )
    .await;
    assert_eq!(replay_response["result"]["isError"], json!(true));
    let replay = tool_payload(&replay_response);
    assert_eq!(replay["replayed"], json!(true));
    assert_eq!(replay["success"], json!(false));
    assert_eq!(replay["failed"], json!(true));
    assert_eq!(replay["message"], "source edit failed before the effect");
    assert_eq!(replay["effect"]["effect_id"], refusal_effect_id);
    assert_eq!(replay["effect"]["receipt"]["outcome"], "failed");
    assert_eq!(
        replay["effect"]["payload"]["durable_metadata_only"],
        json!(true)
    );
    assert_eq!(replay["effect"]["payload"]["failed"], json!(true));
    assert_eq!(replay["effect"]["payload"]["files"], json!([]));
    assert_eq!(
        read_project_file(&moved.project, "src/dest.rs"),
        FOREIGN_DEST
    );
    assert_eq!(
        read_project_file(&moved.project, "src/source.rs"),
        source_after_move
    );
}

#[tokio::test]
async fn source_edit_rollback_refuses_an_edit_without_retained_preimages() {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/main.rs"), "fn old_name() {}\n").unwrap();
    let (fixture, _) = init_production_source_edit_project(&project).await;

    let preview = tool_payload(
        &call_tool(
            &fixture,
            "tracedecay_str_replace",
            json!({
                "path": "src/main.rs",
                "old_str": "old_name",
                "new_str": "new_name",
                "dry_run": true,
                "format": "json",
            }),
        )
        .await,
    );
    let expected_state = require_str(&preview, "/expected_state").to_owned();
    let applied = tool_payload(
        &call_tool(
            &fixture,
            "tracedecay_str_replace",
            json!({
                "path": "src/main.rs",
                "old_str": "old_name",
                "new_str": "new_name",
                "idempotency_key": "mcp-test.source-edit.replace.no-preimage",
                "expected_state": expected_state,
                "format": "json",
            }),
        )
        .await,
    );
    assert_eq!(applied["success"], json!(true), "{applied}");
    assert_eq!(
        read_project_file(&project, "src/main.rs"),
        "fn new_name() {}\n"
    );

    let refused = call_tool(
        &fixture,
        "tracedecay_source_edit_rollback",
        json!({
            "effect_id": require_str(&applied, "/effect/effect_id"),
            "original_idempotency_key": "mcp-test.source-edit.replace.no-preimage",
            "idempotency_key": "mcp-test.source-edit.rollback.no-preimage",
            "original_input_digest": require_str(&applied, "/effect/receipt/input_digest"),
            "expected_state": require_str(&applied, "/effect/receipt/committed_state"),
            "confirm": true,
            "format": "json",
        }),
    )
    .await;
    assert_eq!(refused["jsonrpc"], "2.0");
    assert_eq!(refused["id"], 1);
    assert!(refused.get("result").is_none_or(Value::is_null));
    assert_eq!(refused["error"]["code"], -32602);
    assert_eq!(
        refused["error"]["message"],
        "tool project route failed: reason_code=source_edit.execution_failed retryable=false: config error: source edit effect has no retained rollback material"
    );
    assert_eq!(
        refused["error"]["data"]["tool"],
        "tracedecay_source_edit_rollback"
    );
    assert_eq!(
        refused["error"]["data"]["detail"],
        "config error: source edit effect has no retained rollback material"
    );
    assert_eq!(
        read_project_file(&project, "src/main.rs"),
        "fn new_name() {}\n"
    );
}
