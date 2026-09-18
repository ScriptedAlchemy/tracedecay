//! Behavioral proof of `tracedecay_source_edit_reconcile` through the production
//! MCP dispatch used by `handle_production_source_edit_tool_call`.
//!
//! The tool is not an edit retry. Callers inspect the live candidate file, then
//! attest that the retained uncertain effect either matches its preview or is
//! still the preimage. These tests hold the directory unwritable only long
//! enough for publication to fail after the journal is durable, which is the
//! production `EffectUnknown` boundary.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, close_production_source_edit_fixture,
    expect_tool_error, extract_first_json_content, handle_production_source_edit_tool_call,
    init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const INITIAL: &[u8] = b"fn old_name() {}\n";
const APPLIED: &[u8] = b"fn new_name() {}\n";
const UNRELATED: &[u8] = b"fn unrelated() {}\n";
const ABSENT_EFFECT_ID: &str = "effect.source-edit.reconcile.absent";
const ABSENT_EDIT_KEY: &str = "mcp-test.source-edit-reconcile.absent";
const ABSENT_ATTEMPT_KEY: &str = "mcp-test.source-edit-reconcile.absent-attempt";
const ABSENT_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOOL: &str = "tracedecay_source_edit_reconcile";

fn source_file(fixture: &ProductionSourceEditFixture) -> PathBuf {
    fixture.project_root.join("src/main.rs")
}

async fn open_fixture() -> (TestTempDir, ProductionSourceEditFixture) {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).expect("source directory");
    fs::write(project.join("src/main.rs"), INITIAL).expect("seed source");
    let (fixture, ()) = init_production_source_edit_project(&project).await;
    (dir, fixture)
}

async fn call_reconcile(
    fixture: &ProductionSourceEditFixture,
    args: Value,
) -> Result<Value, String> {
    match handle_production_source_edit_tool_call(fixture, TOOL, args, None, None).await {
        Ok(result) => Ok(extract_first_json_content(&result.value)),
        Err(error) => Err(expect_tool_error(Err(error))),
    }
}

fn assert_refused(result: Result<Value, String>, expected: &str) {
    match result {
        Ok(value) => panic!("expected reconciliation to refuse, got {value}"),
        Err(message) => assert_eq!(message, expected),
    }
}

fn reconcile_request(
    effect_id: &str,
    edit_key: &str,
    attempt_key: &str,
    input_digest: &str,
    disposition: &str,
    committed_state: Option<&str>,
    confirm: bool,
) -> Value {
    let mut args = json!({
        "kind": "str_replace",
        "effect_id": effect_id,
        "idempotency_key": edit_key,
        "attempt_idempotency_key": attempt_key,
        "input_digest": input_digest,
        "disposition": disposition,
        "confirm": confirm,
    });
    if let Some(committed_state) = committed_state {
        args["committed_state"] = json!(committed_state);
    }
    args
}

#[tokio::test]
async fn source_edit_reconcile_refuses_uninspected_and_unretained_requests() {
    let (_dir, fixture) = open_fixture().await;
    let file = source_file(&fixture);

    assert_refused(
        call_reconcile(
            &fixture,
            json!({
                "kind": "str_replace",
                "effect_id": ABSENT_EFFECT_ID,
                "idempotency_key": ABSENT_EDIT_KEY,
                "attempt_idempotency_key": ABSENT_ATTEMPT_KEY,
                "input_digest": ABSENT_DIGEST,
                "disposition": "confirm_rolled_back"
            }),
        )
        .await,
        "config error: source edit reconciliation requires confirm=true from the caller after it inspects the file; do not pause for a human",
    );
    assert_eq!(fs::read(&file).expect("read source"), INITIAL);

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                ABSENT_EFFECT_ID,
                ABSENT_EDIT_KEY,
                ABSENT_ATTEMPT_KEY,
                ABSENT_DIGEST,
                "confirm_rolled_back",
                None,
                false,
            ),
        )
        .await,
        "config error: source edit reconciliation requires confirm=true from the caller after it inspects the file; do not pause for a human",
    );

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                ABSENT_EFFECT_ID,
                ABSENT_EDIT_KEY,
                ABSENT_EDIT_KEY,
                ABSENT_DIGEST,
                "confirm_rolled_back",
                None,
                true,
            ),
        )
        .await,
        "config error: reconciliation attempt idempotency key must differ from the original edit key",
    );

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                ABSENT_EFFECT_ID,
                ABSENT_EDIT_KEY,
                ABSENT_ATTEMPT_KEY,
                ABSENT_DIGEST,
                "confirm_rolled_back",
                Some(ABSENT_DIGEST),
                true,
            ),
        )
        .await,
        "config error: committed_state is only valid when disposition is confirm_committed",
    );

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                ABSENT_EFFECT_ID,
                ABSENT_EDIT_KEY,
                ABSENT_ATTEMPT_KEY,
                ABSENT_DIGEST,
                "abandon",
                None,
                true,
            ),
        )
        .await,
        "config error: invalid source edit reconciliation disposition: abandon",
    );

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                ABSENT_EFFECT_ID,
                ABSENT_EDIT_KEY,
                ABSENT_ATTEMPT_KEY,
                ABSENT_DIGEST,
                "confirm_rolled_back",
                None,
                true,
            ),
        )
        .await,
        "project route error (source_edit.execution_failed): config error: no source edit effect requires reconciliation",
    );
    assert_eq!(fs::read(&file).expect("read source"), INITIAL);
    close_production_source_edit_fixture(fixture).await;
}

#[cfg(unix)]
struct DeniedCreates {
    path: PathBuf,
    original: fs::Permissions,
}

#[cfg(unix)]
impl DeniedCreates {
    fn on(path: &Path) -> Self {
        let original = fs::metadata(path)
            .expect("source directory metadata")
            .permissions();
        let mut denied = original.clone();
        denied.set_readonly(true);
        fs::set_permissions(path, denied).expect("deny creates in the candidate directory");
        Self {
            path: path.to_path_buf(),
            original,
        }
    }
}

#[cfg(unix)]
impl Drop for DeniedCreates {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.original.clone());
    }
}

#[cfg(unix)]
struct InterruptedEdit {
    apply_args: Value,
    expected_state: String,
    predicted_state: String,
    effect_id: String,
    input_digest: String,
    unknown: Value,
}

#[cfg(unix)]
async fn interrupt_str_replace(
    fixture: &ProductionSourceEditFixture,
    edit_key: &str,
) -> InterruptedEdit {
    let preview = handle_production_source_edit_tool_call(
        fixture,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "dry_run": true
        }),
        None,
        None,
    )
    .await
    .expect("preview str_replace");
    let preview = extract_first_json_content(&preview.value);
    let expected_state = preview["expected_state"]
        .as_str()
        .expect("preview expected_state")
        .to_owned();
    assert_eq!(preview["success"], true, "{preview}");
    assert_eq!(
        fs::read(source_file(fixture)).expect("read source"),
        INITIAL
    );

    let apply_args = json!({
        "path": "src/main.rs",
        "old_str": "old_name",
        "new_str": "new_name",
        "idempotency_key": edit_key,
        "expected_state": expected_state,
    });
    let unknown = {
        let _denied = DeniedCreates::on(&fixture.project_root.join("src"));
        let result = handle_production_source_edit_tool_call(
            fixture,
            "tracedecay_str_replace",
            apply_args.clone(),
            None,
            None,
        )
        .await
        .expect("interrupted apply returns a tool result");
        extract_first_json_content(&result.value)
    };

    assert_eq!(unknown["success"], false, "{unknown}");
    assert_eq!(unknown["effect_unknown"], true, "{unknown}");
    assert_eq!(unknown["replayed"], false, "{unknown}");
    assert_eq!(
        unknown["effect"]["effect_class"], "source_edit",
        "{unknown}"
    );
    assert_eq!(unknown["effect"]["idempotency_key"], edit_key, "{unknown}");
    assert_eq!(
        unknown["effect"]["receipt"]["outcome"], "effect_unknown",
        "{unknown}"
    );
    assert_eq!(unknown["effect"]["reconciliation"], "pending", "{unknown}");
    assert_eq!(
        unknown["effect"]["receipt"]["committed_state"],
        Value::Null,
        "{unknown}"
    );
    assert_eq!(unknown["expected_state"], expected_state, "{unknown}");
    assert!(
        unknown["message"].as_str().is_some_and(|message| message.starts_with(
            "source edit effect is unknown and requires reconciliation: create source edit temporary file"
        )),
        "{unknown}"
    );
    assert_eq!(
        fs::read(source_file(fixture)).expect("read source"),
        INITIAL
    );

    let effect_id = unknown["effect"]["effect_id"]
        .as_str()
        .expect("effect id")
        .to_owned();
    let input_digest = unknown["effect"]["receipt"]["input_digest"]
        .as_str()
        .expect("input digest")
        .to_owned();
    let predicted_state = unknown["predicted_state"]
        .as_str()
        .expect("predicted state")
        .to_owned();
    InterruptedEdit {
        apply_args,
        expected_state,
        predicted_state,
        effect_id,
        input_digest,
        unknown,
    }
}

#[cfg(unix)]
fn assert_attempt_completed(value: &Value, attempt_key: &str) {
    assert_eq!(value["success"], true, "{value}");
    assert_eq!(value["reconciled"], true, "{value}");
    assert_eq!(value["replayed"], false, "{value}");
    assert_eq!(
        value["message"], "source edit reconciliation attempt completed",
        "{value}"
    );
    assert!(value.get("durable_metadata_only").is_none(), "{value}");
    assert_eq!(value["effect"]["effect_class"], "source_edit", "{value}");
    assert_eq!(value["effect"]["idempotency_key"], attempt_key, "{value}");
    assert_eq!(value["effect"]["reconciliation"], "reconciled", "{value}");
    assert_eq!(
        value["effect"]["receipt"]["outcome"], "completed",
        "{value}"
    );
    assert_eq!(
        value["effect"]["receipt"]["operation"], "use-case.application.source-edit.reconcile",
        "{value}"
    );
    assert_eq!(value["effect"]["payload"]["success"], true, "{value}");
    assert_eq!(value["effect"]["payload"]["reconciled"], true, "{value}");
    assert_eq!(
        value["effect"]["payload"]["durable_metadata_only"], true,
        "{value}"
    );
    assert_eq!(
        value["effect"]["payload"]["message"], "source edit reconciliation completed",
        "{value}"
    );
    assert_eq!(value["effect"]["payload"]["files"], json!([]), "{value}");
}

#[cfg(unix)]
fn assert_replay(replay: &Value, first: &Value) {
    assert_eq!(replay["success"], true, "{replay}");
    assert_eq!(replay["reconciled"], true, "{replay}");
    assert_eq!(replay["replayed"], true, "{replay}");
    assert_eq!(replay["durable_metadata_only"], true, "{replay}");
    assert_eq!(
        replay["message"], "source edit reconciliation completed",
        "{replay}"
    );
    assert_eq!(replay["effect"]["effect_id"], first["effect"]["effect_id"]);
    assert_eq!(replay["effect"]["receipt"], first["effect"]["receipt"]);
}

#[cfg(unix)]
#[tokio::test]
async fn source_edit_reconcile_confirms_rolled_back_bytes_and_replays() {
    let (_dir, fixture) = open_fixture().await;
    let file = source_file(&fixture);
    let interrupted =
        interrupt_str_replace(&fixture, "mcp-test.source-edit-reconcile.rolled-back").await;
    let attempt_key = "mcp-test.source-edit-reconcile.rolled-back-attempt";

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                &interrupted.effect_id,
                "mcp-test.source-edit-reconcile.rolled-back",
                attempt_key,
                &interrupted.input_digest,
                "confirm_committed",
                Some(&interrupted.expected_state),
                true,
            ),
        )
        .await,
        "project route error (source_edit.execution_failed): config error: source edit committed-state inspection does not match the exact preview",
    );
    assert_eq!(fs::read(&file).expect("read source"), INITIAL);

    let confirmed = call_reconcile(
        &fixture,
        reconcile_request(
            &interrupted.effect_id,
            "mcp-test.source-edit-reconcile.rolled-back",
            attempt_key,
            &interrupted.input_digest,
            "confirm_rolled_back",
            None,
            true,
        ),
    )
    .await
    .expect("rolled-back confirmation");
    assert_attempt_completed(&confirmed, attempt_key);
    assert_eq!(
        confirmed["effect"]["receipt"]["committed_state"],
        interrupted.expected_state
    );
    assert_ne!(
        confirmed["effect"]["effect_id"],
        interrupted.unknown["effect"]["effect_id"]
    );
    assert_eq!(fs::read(&file).expect("read source"), INITIAL);

    let replay = call_reconcile(
        &fixture,
        reconcile_request(
            &interrupted.effect_id,
            "mcp-test.source-edit-reconcile.rolled-back",
            attempt_key,
            &interrupted.input_digest,
            "confirm_rolled_back",
            None,
            true,
        ),
    )
    .await
    .expect("reconciliation replay");
    assert_replay(&replay, &confirmed);
    assert_eq!(fs::read(&file).expect("read source"), INITIAL);

    let original_retry = handle_production_source_edit_tool_call(
        &fixture,
        "tracedecay_str_replace",
        interrupted.apply_args,
        None,
        None,
    )
    .await
    .expect("original edit retry");
    let original_retry = extract_first_json_content(&original_retry.value);
    assert_eq!(original_retry["success"], false, "{original_retry}");
    assert_eq!(original_retry["reconciled"], true, "{original_retry}");
    assert_eq!(original_retry["replayed"], true, "{original_retry}");
    assert_eq!(
        original_retry["message"], "source edit reconciliation completed",
        "{original_retry}"
    );
    assert_eq!(
        original_retry["operation"], "use-case.application.source-edit.str-replace",
        "{original_retry}"
    );
    assert_eq!(fs::read(&file).expect("read source"), INITIAL);

    let fresh = handle_production_source_edit_tool_call(
        &fixture,
        "tracedecay_str_replace",
        json!({
            "path": "src/main.rs",
            "old_str": "old_name",
            "new_str": "new_name",
            "idempotency_key": "mcp-test.source-edit-reconcile.after-rollback",
            "expected_state": interrupted.expected_state,
        }),
        None,
        None,
    )
    .await
    .expect("edit after reconciliation");
    let fresh = extract_first_json_content(&fresh.value);
    assert_eq!(fresh["success"], true, "{fresh}");
    assert_eq!(fresh["replayed"], false, "{fresh}");
    assert_eq!(fs::read(&file).expect("read source"), APPLIED);
    close_production_source_edit_fixture(fixture).await;
}

#[cfg(unix)]
#[tokio::test]
async fn source_edit_reconcile_confirms_committed_bytes_only_when_disk_matches() {
    let (_dir, fixture) = open_fixture().await;
    let file = source_file(&fixture);
    let interrupted =
        interrupt_str_replace(&fixture, "mcp-test.source-edit-reconcile.committed").await;
    let attempt_key = "mcp-test.source-edit-reconcile.committed-attempt";

    fs::write(&file, UNRELATED).expect("write unrelated bytes");
    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                &interrupted.effect_id,
                "mcp-test.source-edit-reconcile.committed",
                attempt_key,
                &interrupted.input_digest,
                "confirm_committed",
                Some(&interrupted.predicted_state),
                true,
            ),
        )
        .await,
        "project route error (source_edit.execution_failed): config error: source edit committed-state inspection does not match the exact preview",
    );
    assert_eq!(fs::read(&file).expect("read source"), UNRELATED);

    assert_refused(
        call_reconcile(
            &fixture,
            reconcile_request(
                &interrupted.effect_id,
                "mcp-test.source-edit-reconcile.committed",
                "mcp-test.source-edit-reconcile.committed-rollback-attempt",
                &interrupted.input_digest,
                "confirm_rolled_back",
                None,
                true,
            ),
        )
        .await,
        "project route error (source_edit.execution_failed): config error: source edit rollback inspection does not match the admitted expected state",
    );
    assert_eq!(fs::read(&file).expect("read source"), UNRELATED);

    fs::write(&file, APPLIED).expect("write previewed bytes");
    let confirmed = call_reconcile(
        &fixture,
        reconcile_request(
            &interrupted.effect_id,
            "mcp-test.source-edit-reconcile.committed",
            attempt_key,
            &interrupted.input_digest,
            "confirm_committed",
            Some(&interrupted.predicted_state),
            true,
        ),
    )
    .await
    .expect("committed confirmation");
    assert_attempt_completed(&confirmed, attempt_key);
    assert_eq!(
        confirmed["effect"]["receipt"]["committed_state"],
        interrupted.predicted_state
    );
    assert_eq!(fs::read(&file).expect("read source"), APPLIED);

    let replay = call_reconcile(
        &fixture,
        reconcile_request(
            &interrupted.effect_id,
            "mcp-test.source-edit-reconcile.committed",
            attempt_key,
            &interrupted.input_digest,
            "confirm_committed",
            Some(&interrupted.predicted_state),
            true,
        ),
    )
    .await
    .expect("committed reconciliation replay");
    assert_replay(&replay, &confirmed);
    assert_eq!(fs::read(&file).expect("read source"), APPLIED);

    let original_retry = handle_production_source_edit_tool_call(
        &fixture,
        "tracedecay_str_replace",
        interrupted.apply_args,
        None,
        None,
    )
    .await
    .expect("original edit retry after commit confirmation");
    let original_retry = extract_first_json_content(&original_retry.value);
    assert_eq!(original_retry["success"], true, "{original_retry}");
    assert_eq!(original_retry["reconciled"], true, "{original_retry}");
    assert_eq!(original_retry["replayed"], true, "{original_retry}");
    assert_eq!(
        original_retry["message"], "source edit reconciliation completed",
        "{original_retry}"
    );
    assert_eq!(
        original_retry["effect"]["effect_id"],
        interrupted.unknown["effect"]["effect_id"]
    );
    assert_eq!(fs::read(&file).expect("read source"), APPLIED);
    close_production_source_edit_fixture(fixture).await;
}
