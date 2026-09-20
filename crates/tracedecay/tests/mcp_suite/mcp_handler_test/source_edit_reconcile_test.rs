//! `tracedecay_source_edit_reconcile` through the production MCP dispatch.
//!
//! Publication is stopped after the journal is durable, which is the retained
//! `EffectUnknown` a caller concludes by inspecting the candidate file. The
//! assertions are the tool's own response and the bytes left on disk.

use crate::support::{
    ProductionSourceEditFixture, TestTempDir, close_production_source_edit_fixture, extract_text,
    handle_production_source_edit_tool_call, init_production_source_edit_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_mcp::ToolResult;

const RELATIVE_PATH: &str = "src/locked/edit.rs";
const PREIMAGE: &[u8] = b"pub fn before() {}\n";
const POSTIMAGE: &[u8] = b"pub fn after() {}\n";
const OLD: &str = "pub fn before() {}";
const NEW: &str = "pub fn after() {}";
const ABSENT_DIGEST: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

const NO_JOURNAL: &str = "project route error (source_edit.execution_failed): config error: no source edit effect requires reconciliation";
const IDENTITY_MISMATCH: &str = "project route error (source_edit.execution_failed): config error: source edit reconciliation identity does not match the retained effect";
const COMMITTED_MISMATCH: &str = "project route error (source_edit.execution_failed): config error: source edit committed-state inspection does not match the exact preview";
const ROLLED_BACK_MISMATCH: &str = "project route error (source_edit.execution_failed): config error: source edit rollback inspection does not match the admitted expected state";
const CONFIRM_REQUIRED: &str = "config error: source edit reconciliation requires confirm=true from the caller after it inspects the file; do not pause for a human";
const ATTEMPT_KEY_CONFLICT: &str =
    "config error: reconciliation attempt idempotency key must differ from the original edit key";
const COMMITTED_STATE_UNEXPECTED: &str =
    "config error: committed_state is only valid when disposition is confirm_committed";
const COMMITTED_STATE_REQUIRED: &str = "config error: missing required parameter: committed_state";
const INVALID_DISPOSITION: &str =
    "config error: invalid source edit reconciliation disposition: guess";

struct OpenedProject {
    _dir: TestTempDir,
    fixture: ProductionSourceEditFixture,
    file: PathBuf,
}

async fn open_project() -> OpenedProject {
    let dir = test_temp_dir();
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src/locked")).unwrap();
    fs::write(project.join(RELATIVE_PATH), PREIMAGE).unwrap();
    let fixture = init_production_source_edit_project(&project).await;
    OpenedProject {
        file: project.join(RELATIVE_PATH),
        fixture,
        _dir: dir,
    }
}

fn tool_json(result: ToolResult) -> Value {
    let text = extract_text(&result.value);
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

fn refusal(result: Result<ToolResult, TraceDecayError>) -> String {
    match result {
        Err(error) => error.to_string(),
        Ok(result) => panic!("reconcile must refuse, got {}", extract_text(&result.value)),
    }
}

async fn call_reconcile(
    fixture: &ProductionSourceEditFixture,
    args: Value,
) -> Result<ToolResult, TraceDecayError> {
    handle_production_source_edit_tool_call(
        fixture,
        "tracedecay_source_edit_reconcile",
        args,
        None,
        None,
    )
    .await
}

fn reconcile_args(
    effect_id: &str,
    original_key: &str,
    attempt_key: &str,
    input_digest: &str,
    disposition: &str,
    committed_state: Option<&str>,
) -> Value {
    let mut args = json!({
        "kind": "str_replace",
        "effect_id": effect_id,
        "idempotency_key": original_key,
        "attempt_idempotency_key": attempt_key,
        "input_digest": input_digest,
        "disposition": disposition,
        "confirm": true,
    });
    if let Some(committed_state) = committed_state {
        args["committed_state"] = Value::String(committed_state.to_owned());
    }
    args
}

async fn admitted_expected_state(fixture: &ProductionSourceEditFixture) -> String {
    let preview = handle_production_source_edit_tool_call(
        fixture,
        "tracedecay_str_replace",
        json!({
            "path": RELATIVE_PATH,
            "old_str": OLD,
            "new_str": NEW,
            "dry_run": true
        }),
        None,
        None,
    )
    .await
    .expect("source edit preview");
    let preview = tool_json(preview);
    preview["expected_state"]
        .as_str()
        .expect("preview expected state")
        .to_owned()
}

/// Stop the atomic publish after the journal is durable. Unix refuses the
/// temporary file in a non-writable parent; Windows holds the candidate
/// without share-delete so the same rename is refused.
struct PublicationHold {
    #[cfg(unix)]
    directory: PathBuf,
    #[cfg(unix)]
    permissions: fs::Permissions,
    #[cfg(windows)]
    _file: fs::File,
}

impl PublicationHold {
    #[cfg(unix)]
    fn acquire(directory: &Path) -> Self {
        let permissions = fs::metadata(directory).unwrap().permissions();
        let mut locked = permissions.clone();
        locked.set_readonly(true);
        fs::set_permissions(directory, locked).unwrap();
        Self {
            directory: directory.to_path_buf(),
            permissions,
        }
    }

    #[cfg(windows)]
    fn acquire(candidate: &Path) -> Self {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        let _file = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(candidate)
            .unwrap();
        Self { _file }
    }
}

impl Drop for PublicationHold {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = fs::set_permissions(&self.directory, self.permissions.clone());
        }
    }
}

async fn retain_unpublished_effect(
    opened: &OpenedProject,
    original_key: &str,
    expected_state: &str,
) -> Value {
    #[cfg(unix)]
    let _hold = PublicationHold::acquire(opened.file.parent().unwrap());
    #[cfg(windows)]
    let _hold = PublicationHold::acquire(&opened.file);
    let unknown = handle_production_source_edit_tool_call(
        &opened.fixture,
        "tracedecay_str_replace",
        json!({
            "path": RELATIVE_PATH,
            "old_str": OLD,
            "new_str": NEW,
            "idempotency_key": original_key,
            "expected_state": expected_state,
        }),
        None,
        None,
    )
    .await
    .expect("unpublished source edit");
    drop(_hold);
    let unknown = tool_json(unknown);
    assert_eq!(unknown["success"], false);
    assert_eq!(unknown["effect_unknown"], true);
    assert_eq!(unknown["replayed"], false);
    assert_eq!(unknown["effect"]["receipt"]["outcome"], "effect_unknown");
    assert_eq!(unknown["effect"]["reconciliation"], "pending");
    let unknown_message = unknown["message"].as_str().expect("effect unknown message");
    #[cfg(unix)]
    assert!(
        unknown_message.contains("create source edit temporary file"),
        "{unknown_message}"
    );
    #[cfg(windows)]
    assert!(
        unknown_message.contains("atomically publish source edit candidate"),
        "{unknown_message}"
    );
    assert_eq!(unknown["effect"]["idempotency_key"], original_key);
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);
    unknown
}

fn assert_reconcile_attempt(value: &Value, attempt_key: &str, replayed: bool) {
    assert_eq!(value["success"], true);
    assert_eq!(value["replayed"], replayed);
    if replayed {
        assert_eq!(value["message"], "source edit reconciliation completed");
        assert_eq!(value["effect"]["payload"]["reconciled"], true);
        assert_eq!(value["effect"]["payload"]["success"], true);
        assert_eq!(
            value["effect"]["payload"]["message"],
            "source edit reconciliation completed"
        );
    } else {
        assert_eq!(value["reconciled"], true);
        assert_eq!(
            value["message"],
            "source edit reconciliation attempt completed"
        );
        assert_eq!(value["effect"]["payload"]["reconciled"], true);
        assert_eq!(
            value["effect"]["payload"]["message"],
            "source edit reconciliation completed"
        );
    }
    assert_eq!(value["effect"]["effect_class"], "source_edit");
    assert_eq!(value["effect"]["idempotency_key"], attempt_key);
    assert_eq!(value["effect"]["reconciliation"], "reconciled");
    assert_eq!(value["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(value["effect"]["receipt"]["idempotency_key"], attempt_key);
    assert_eq!(
        value["effect"]["receipt"]["operation"],
        "use-case.application.source-edit.reconcile"
    );
}

#[tokio::test]
async fn reconcile_refuses_uninspected_and_absent_effects() {
    let opened = open_project().await;
    let base = json!({
        "kind": "str_replace",
        "effect_id": "effect.missing.reconcile",
        "idempotency_key": "mcp.source-edit-reconcile.missing.original",
        "attempt_idempotency_key": "mcp.source-edit-reconcile.missing.attempt",
        "input_digest": ABSENT_DIGEST,
        "disposition": "confirm_rolled_back",
    });

    let mut missing_confirm = base.clone();
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, missing_confirm.clone()).await),
        CONFIRM_REQUIRED
    );
    missing_confirm["confirm"] = Value::Bool(false);
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, missing_confirm).await),
        CONFIRM_REQUIRED
    );

    let mut same_key = base.clone();
    same_key["confirm"] = Value::Bool(true);
    same_key["attempt_idempotency_key"] = same_key["idempotency_key"].clone();
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, same_key).await),
        ATTEMPT_KEY_CONFLICT
    );

    let mut guessed = base.clone();
    guessed["confirm"] = Value::Bool(true);
    guessed["disposition"] = Value::String("guess".to_owned());
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, guessed).await),
        INVALID_DISPOSITION
    );

    let mut unexpected_state = base.clone();
    unexpected_state["confirm"] = Value::Bool(true);
    unexpected_state["committed_state"] = Value::String(ABSENT_DIGEST.to_owned());
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, unexpected_state).await),
        COMMITTED_STATE_UNEXPECTED
    );

    let mut missing_state = base.clone();
    missing_state["confirm"] = Value::Bool(true);
    missing_state["disposition"] = Value::String("confirm_committed".to_owned());
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, missing_state).await),
        COMMITTED_STATE_REQUIRED
    );

    let mut absent = base;
    absent["confirm"] = Value::Bool(true);
    assert_eq!(
        refusal(call_reconcile(&opened.fixture, absent).await),
        NO_JOURNAL
    );
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);

    close_production_source_edit_fixture(opened.fixture).await;
}

#[tokio::test]
async fn unpublished_effect_confirms_rolled_back_and_releases_the_file() {
    let opened = open_project().await;
    let expected_state = admitted_expected_state(&opened.fixture).await;
    let original_key = "mcp.source-edit-reconcile.rolled-back.original";
    let attempt_key = "mcp.source-edit-reconcile.rolled-back.attempt";
    let unknown = retain_unpublished_effect(&opened, original_key, &expected_state).await;
    let effect_id = unknown["effect"]["effect_id"]
        .as_str()
        .expect("effect id")
        .to_owned();
    let input_digest = unknown["effect"]["receipt"]["input_digest"]
        .as_str()
        .expect("input digest")
        .to_owned();

    assert_eq!(
        refusal(
            call_reconcile(
                &opened.fixture,
                reconcile_args(
                    "effect.mcp.reconcile.wrong",
                    original_key,
                    "mcp.source-edit-reconcile.rolled-back.wrong-identity",
                    &input_digest,
                    "confirm_rolled_back",
                    None,
                ),
            )
            .await
        ),
        IDENTITY_MISMATCH
    );
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);

    let concluded = tool_json(
        call_reconcile(
            &opened.fixture,
            reconcile_args(
                &effect_id,
                original_key,
                attempt_key,
                &input_digest,
                "confirm_rolled_back",
                None,
            ),
        )
        .await
        .expect("confirm rolled back"),
    );
    assert_reconcile_attempt(&concluded, attempt_key, false);
    assert_eq!(
        concluded["effect"]["receipt"]["committed_state"],
        unknown["expected_state"]
    );
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);

    let replay = tool_json(
        call_reconcile(
            &opened.fixture,
            reconcile_args(
                &effect_id,
                original_key,
                attempt_key,
                &input_digest,
                "confirm_rolled_back",
                None,
            ),
        )
        .await
        .expect("replay rolled back"),
    );
    assert_reconcile_attempt(&replay, attempt_key, true);
    assert_eq!(
        replay["effect"]["effect_id"],
        concluded["effect"]["effect_id"]
    );
    assert_eq!(
        replay["effect"]["receipt"]["committed_state"],
        unknown["expected_state"]
    );
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);

    let original_retry = tool_json(
        handle_production_source_edit_tool_call(
            &opened.fixture,
            "tracedecay_str_replace",
            json!({
                "path": RELATIVE_PATH,
                "old_str": OLD,
                "new_str": NEW,
                "idempotency_key": original_key,
                "expected_state": expected_state,
            }),
            None,
            None,
        )
        .await
        .expect("original edit retry"),
    );
    assert_eq!(original_retry["success"], false);
    assert_eq!(original_retry["replayed"], true);
    assert_eq!(original_retry["effect"]["reconciliation"], "reconciled");
    assert_eq!(original_retry["effect"]["payload"]["reconciled"], true);
    assert_eq!(original_retry["effect"]["payload"]["success"], false);
    assert_eq!(
        original_retry["message"],
        "source edit reconciliation completed"
    );
    assert_eq!(original_retry["effect"]["idempotency_key"], original_key);
    assert_eq!(original_retry["effect"]["receipt"]["outcome"], "failed");
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);

    let follow_up = tool_json(
        handle_production_source_edit_tool_call(
            &opened.fixture,
            "tracedecay_str_replace",
            json!({
                "path": RELATIVE_PATH,
                "old_str": OLD,
                "new_str": NEW,
                "idempotency_key": "mcp.source-edit-reconcile.rolled-back.follow-up",
                "expected_state": expected_state,
            }),
            None,
            None,
        )
        .await
        .expect("follow-up edit"),
    );
    assert_eq!(follow_up["success"], true);
    assert_eq!(follow_up["replayed"], false);
    assert_eq!(follow_up["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(fs::read(&opened.file).unwrap(), POSTIMAGE);

    close_production_source_edit_fixture(opened.fixture).await;
}

#[tokio::test]
async fn mismatched_inspection_keeps_bytes_and_confirm_committed_keeps_the_postimage() {
    let opened = open_project().await;
    let expected_state = admitted_expected_state(&opened.fixture).await;
    let original_key = "mcp.source-edit-reconcile.committed.original";
    let attempt_key = "mcp.source-edit-reconcile.committed.attempt";
    let unknown = retain_unpublished_effect(&opened, original_key, &expected_state).await;
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

    assert_eq!(
        refusal(
            call_reconcile(
                &opened.fixture,
                reconcile_args(
                    &effect_id,
                    original_key,
                    "mcp.source-edit-reconcile.committed.too-early",
                    &input_digest,
                    "confirm_committed",
                    Some(&predicted_state),
                ),
            )
            .await
        ),
        COMMITTED_MISMATCH
    );
    assert_eq!(fs::read(&opened.file).unwrap(), PREIMAGE);

    fs::write(&opened.file, POSTIMAGE).unwrap();
    assert_eq!(
        refusal(
            call_reconcile(
                &opened.fixture,
                reconcile_args(
                    &effect_id,
                    original_key,
                    "mcp.source-edit-reconcile.committed.wrong-disposition",
                    &input_digest,
                    "confirm_rolled_back",
                    None,
                ),
            )
            .await
        ),
        ROLLED_BACK_MISMATCH
    );
    assert_eq!(fs::read(&opened.file).unwrap(), POSTIMAGE);

    let concluded = tool_json(
        call_reconcile(
            &opened.fixture,
            reconcile_args(
                &effect_id,
                original_key,
                attempt_key,
                &input_digest,
                "confirm_committed",
                Some(&predicted_state),
            ),
        )
        .await
        .expect("confirm committed"),
    );
    assert_reconcile_attempt(&concluded, attempt_key, false);
    assert_eq!(
        concluded["effect"]["receipt"]["committed_state"],
        predicted_state
    );
    assert_eq!(fs::read(&opened.file).unwrap(), POSTIMAGE);

    let replay = tool_json(
        call_reconcile(
            &opened.fixture,
            reconcile_args(
                &effect_id,
                original_key,
                attempt_key,
                &input_digest,
                "confirm_committed",
                Some(&predicted_state),
            ),
        )
        .await
        .expect("replay committed"),
    );
    assert_reconcile_attempt(&replay, attempt_key, true);
    assert_eq!(
        replay["effect"]["effect_id"],
        concluded["effect"]["effect_id"]
    );
    assert_eq!(
        replay["effect"]["receipt"]["committed_state"],
        predicted_state
    );
    assert_eq!(fs::read(&opened.file).unwrap(), POSTIMAGE);

    let original_retry = tool_json(
        handle_production_source_edit_tool_call(
            &opened.fixture,
            "tracedecay_str_replace",
            json!({
                "path": RELATIVE_PATH,
                "old_str": OLD,
                "new_str": NEW,
                "idempotency_key": original_key,
                "expected_state": expected_state,
            }),
            None,
            None,
        )
        .await
        .expect("original edit retry"),
    );
    assert_eq!(original_retry["success"], true);
    assert_eq!(original_retry["replayed"], true);
    assert_eq!(original_retry["effect"]["reconciliation"], "reconciled");
    assert_eq!(original_retry["effect"]["payload"]["reconciled"], true);
    assert_eq!(original_retry["effect"]["payload"]["success"], true);
    assert_eq!(
        original_retry["message"],
        "source edit reconciliation completed"
    );
    assert_eq!(original_retry["effect"]["idempotency_key"], original_key);
    assert_eq!(original_retry["effect"]["receipt"]["outcome"], "completed");
    assert_eq!(
        original_retry["effect"]["receipt"]["committed_state"],
        predicted_state
    );
    assert_eq!(fs::read(&opened.file).unwrap(), POSTIMAGE);

    close_production_source_edit_fixture(opened.fixture).await;
}
