//! File editing tool handlers: `str_replace`, `multi_str_replace`, `insert_at`,
//! `ast_grep_rewrite`.

use serde_json::{Value, json};
use tracedecay_application::{
    ApiMigrationPlanRequestV1, ApiMigrationPlanV1, CancellationSignal, Deadline, EffectId,
    IdempotencyKey, RequestId, SourceEditKind, SourceEditReconciliationDispositionV1,
    SourceEditRequest,
};
use tracedecay_domain::ManifestDigest;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::server::{
    SourceEditExecutor, SourceEditInvocationV1, SourceEditReconciliationExecutor,
    SourceEditReconciliationInvocationV1,
};
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render;
use super::support::{generic_tool_result, rendered_tool_result};

fn missing_required_param(name: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("missing required parameter: {name}"),
    }
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| missing_required_param(name))
}

fn required_array<'a>(args: &'a Value, name: &str) -> Result<&'a [Value]> {
    args.get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| missing_required_param(name))
}

/// Reads the shared `dry_run` edit flag (default `false`): when set, an edit
/// primitive validates and computes the resulting content but writes nothing,
/// returning a preview diff instead.
fn dry_run_arg(args: &Value) -> bool {
    args.get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Reads the shared `verify` edit flag (default `false`): when set, a real
/// (non-dry-run) successful edit re-runs file-scoped diagnostics and attaches a
/// compact verdict to the result. Off by default to keep edits fast; compound
/// refactor tools are expected to default it on.
fn verify_arg(args: &Value) -> bool {
    args.get("verify").and_then(Value::as_bool).unwrap_or(false)
}

async fn source_edit_tool_result(
    cg: &TraceDecay,
    args: &Value,
    request: SourceEditRequest,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let idempotency_key = optional_idempotency_key(args)?;
    let expected_state = optional_expected_state(args)?;
    if !request.dry_run() && (idempotency_key.is_none() || expected_state.is_none()) {
        return Err(TraceDecayError::Config {
            message: "source edit apply requires a fresh idempotency_key and the expected_state returned by a preview"
                .to_owned(),
        });
    }
    let SourceEditInvocationContext {
        executor,
        request_id,
        deadline,
        cancellation,
        ..
    } = invocation;
    let (Some(executor), Some(request_id), Some(deadline), Some(cancellation)) =
        (executor, request_id, deadline, cancellation)
    else {
        return Err(TraceDecayError::Config {
            message: "daemon-owned source edit authority is unavailable".to_owned(),
        });
    };
    let result = executor(SourceEditInvocationV1 {
        edit: request,
        idempotency_key,
        expected_state,
        request_id,
        deadline,
        cancellation,
    })
    .await?;
    let value = result.value();
    let touched_files = result.outcome.touched_files(result.dry_run);
    let success = result.outcome.success();
    let tool_result =
        rendered_tool_result(Some(cg.project_root()), args, &value, touched_files, || {
            result
                .outcome
                .as_move()
                .map_or_else(|| render::generic_md(&value), move_result_md)
        })
        .with_semantic_error(!success);
    if success {
        Ok(tool_result)
    } else {
        Ok(tool_result.with_failure_message(result.outcome.message()))
    }
}

#[derive(Clone)]
pub(super) struct SourceEditInvocationContext {
    pub(super) executor: Option<SourceEditExecutor>,
    pub(super) reconciliation_executor: Option<SourceEditReconciliationExecutor>,
    pub(super) request_id: Option<RequestId>,
    pub(super) deadline: Option<Deadline>,
    pub(super) cancellation: Option<CancellationSignal>,
}

pub(super) async fn handle_source_edit_reconcile(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    if args.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(TraceDecayError::Config {
            message: "source edit reconciliation requires confirm=true".to_owned(),
        });
    }
    let kind = serde_json::from_value::<SourceEditKind>(json!(required_str(&args, "kind")?))
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid source edit kind: {error}"),
        })?;
    let effect_id =
        EffectId::new(required_str(&args, "effect_id")?).map_err(source_edit_identity_error)?;
    let idempotency_key = IdempotencyKey::new(required_str(&args, "idempotency_key")?)
        .map_err(source_edit_identity_error)?;
    let attempt_idempotency_key =
        IdempotencyKey::new(required_str(&args, "attempt_idempotency_key")?)
            .map_err(source_edit_identity_error)?;
    if attempt_idempotency_key == idempotency_key {
        return Err(TraceDecayError::Config {
            message:
                "reconciliation attempt idempotency key must differ from the original edit key"
                    .to_owned(),
        });
    }
    let input_digest = ManifestDigest::new(required_str(&args, "input_digest")?)
        .map_err(source_edit_identity_error)?;
    let disposition = match required_str(&args, "disposition")? {
        "confirm_committed" => {
            let committed_state = ManifestDigest::new(required_str(&args, "committed_state")?)
                .map_err(source_edit_identity_error)?;
            SourceEditReconciliationDispositionV1::ConfirmCommitted { committed_state }
        }
        "confirm_rolled_back" => {
            if args.get("committed_state").is_some() {
                return Err(TraceDecayError::Config {
                    message: "committed_state is only valid when disposition is confirm_committed"
                        .to_owned(),
                });
            }
            SourceEditReconciliationDispositionV1::ConfirmRolledBack
        }
        value => {
            return Err(TraceDecayError::Config {
                message: format!("invalid source edit reconciliation disposition: {value}"),
            });
        }
    };
    let SourceEditInvocationContext {
        reconciliation_executor,
        request_id,
        deadline,
        cancellation,
        ..
    } = invocation;
    let (Some(executor), Some(request_id), Some(deadline), Some(cancellation)) =
        (reconciliation_executor, request_id, deadline, cancellation)
    else {
        return Err(TraceDecayError::Config {
            message: "daemon-owned source edit reconciliation authority is unavailable".to_owned(),
        });
    };
    let result = executor(SourceEditReconciliationInvocationV1 {
        kind,
        effect_id,
        idempotency_key,
        attempt_idempotency_key,
        input_digest,
        disposition,
        request_id,
        deadline,
        cancellation,
    })
    .await?;
    let value = result.value();
    let success = result.outcome.success();
    let tool_result = generic_tool_result(Some(cg.project_root()), &args, &value, Vec::new())
        .with_semantic_error(!success);
    if success {
        Ok(tool_result)
    } else {
        Ok(tool_result.with_failure_message(result.outcome.message()))
    }
}

fn source_edit_identity_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("invalid source edit reconciliation identity: {error}"),
    }
}

fn optional_idempotency_key(args: &Value) -> Result<Option<IdempotencyKey>> {
    args.get("idempotency_key")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| missing_required_param("idempotency_key"))
                .and_then(|value| {
                    IdempotencyKey::new(value).map_err(|error| TraceDecayError::Config {
                        message: format!("invalid idempotency_key: {error}"),
                    })
                })
        })
        .transpose()
}

fn optional_expected_state(args: &Value) -> Result<Option<ManifestDigest>> {
    args.get("expected_state")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| missing_required_param("expected_state"))
                .and_then(|value| {
                    ManifestDigest::new(value).map_err(|error| TraceDecayError::Config {
                        message: format!("invalid expected_state: {error}"),
                    })
                })
        })
        .transpose()
}

pub(super) async fn handle_str_replace(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let old_str = required_str(&args, "old_str")?;
    let new_str = required_str(&args, "new_str")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::StrReplace {
            path: path.to_owned(),
            old_str: old_str.to_owned(),
            new_str: new_str.to_owned(),
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

pub(super) async fn handle_multi_str_replace(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let replacements = required_array(&args, "replacements")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let parsed_replacements: Vec<(String, String)> = replacements
        .iter()
        .filter_map(|pair| {
            let arr = pair.as_array()?;
            if arr.len() != 2 {
                return None;
            }
            let old = arr[0].as_str()?;
            let new = arr[1].as_str()?;
            Some((old.to_owned(), new.to_owned()))
        })
        .collect();

    if parsed_replacements.len() != replacements.len() {
        return Err(TraceDecayError::Config {
            message: "each replacement must be an array of exactly 2 strings".to_string(),
        });
    }

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::MultiStrReplace {
            path: path.to_owned(),
            replacements: parsed_replacements,
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

pub(super) async fn handle_insert_at(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let anchor = required_str(&args, "anchor")?;
    let content = required_str(&args, "content")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    let before = args.get("before").and_then(Value::as_bool).unwrap_or(false);

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::InsertAt {
            path: path.to_owned(),
            anchor: anchor.to_owned(),
            content: content.to_owned(),
            before,
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

pub(super) async fn handle_replace_symbol(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let new_source = required_str(&args, "new_source")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::ReplaceSymbol {
            symbol: symbol.to_owned(),
            new_source: new_source.to_owned(),
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

pub(super) async fn handle_insert_at_symbol(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let content = required_str(&args, "content")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);
    let position = args
        .get("position")
        .and_then(|v| v.as_str())
        .unwrap_or("after");

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::InsertAtSymbol {
            symbol: symbol.to_owned(),
            content: content.to_owned(),
            position: position.to_owned(),
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

pub(super) async fn handle_move_symbol(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let symbol = required_str(&args, "symbol")?;
    let dest_file = required_str(&args, "dest_file")?;
    // The impact report is the product; applying is opt-in.
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
    let update_references = args
        .get("update_references")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::MoveSymbol {
            symbol: symbol.to_owned(),
            dest_file: dest_file.to_owned(),
            dry_run,
            update_references,
        },
        invocation,
    )
    .await
}

/// Human-readable markdown for a move result: the outcome line, applied
/// imports, the impact report (the centerpiece), and the preview diff.
fn move_result_md(result: &crate::types::MoveResult) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let verb = if result.dry_run {
        "Would move"
    } else {
        "Moved"
    };
    let _ = writeln!(
        out,
        "## {verb} `{}`\n\n{} → {}\n\n{}",
        result.symbol, result.source_file, result.dest_file, result.message
    );
    if !result.applied_imports.is_empty() {
        out.push_str("\n### Auto-inserted imports (destination)\n");
        for imp in &result.applied_imports {
            let _ = writeln!(out, "- `{}`", imp.trim());
        }
    }
    out.push_str("\n### Impact\n");
    if result.impact.is_empty() {
        out.push_str("Clean move — no references, dependencies, or module concerns detected.\n");
    } else {
        for hint in &result.impact {
            let loc = hint
                .line
                .map_or_else(|| hint.file.clone(), |l| format!("{}:{}", hint.file, l));
            let _ = writeln!(out, "- **{}** ({}) — {}", hint.kind, loc, hint.detail);
            if let Some(sug) = &hint.suggestion {
                let _ = writeln!(out, "  - suggestion: {sug}");
            }
        }
    }
    if let Some(diff) = &result.diff {
        let _ = write!(out, "\n### Preview diff\n```diff\n{diff}\n```\n");
    }
    out
}

pub(super) async fn handle_ast_grep_rewrite(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let path = required_str(&args, "path")?;
    let pattern = required_str(&args, "pattern")?;
    let rewrite = required_str(&args, "rewrite")?;
    let dry_run = dry_run_arg(&args);
    let verify = verify_arg(&args);

    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::AstGrepRewrite {
            path: path.to_owned(),
            pattern: pattern.to_owned(),
            rewrite: rewrite.to_owned(),
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

pub(super) async fn handle_api_migration_plan(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let request = serde_json::from_value::<ApiMigrationPlanRequestV1>(json!({
        "family_id": required_str(&args, "family_id")?,
        "operations": required_array(&args, "operations")?,
    }))
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid API migration plan request: {error}"),
    })?;
    let plan = crate::application::api_migration::plan_api_migration(cg, request).await?;
    let value = serde_json::to_value(&plan).map_err(|error| TraceDecayError::Config {
        message: format!("cannot render API migration plan: {error}"),
    })?;
    let touched_files = plan
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    Ok(
        generic_tool_result(Some(cg.project_root()), &args, &value, touched_files)
            .with_semantic_error(plan.blocked),
    )
}

pub(super) async fn handle_api_migration_apply(
    cg: &TraceDecay,
    args: Value,
    invocation: SourceEditInvocationContext,
) -> Result<ToolResult> {
    let plan = serde_json::from_value::<ApiMigrationPlanV1>(
        args.get("plan")
            .cloned()
            .ok_or_else(|| missing_required_param("plan"))?,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid API migration plan: {error}"),
    })?;
    let plan_digest = ManifestDigest::new(required_str(&args, "plan_digest")?)
        .map_err(source_edit_identity_error)?;
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
    let verify = args.get("verify").and_then(Value::as_bool).unwrap_or(true);
    source_edit_tool_result(
        cg,
        &args,
        SourceEditRequest::ApiMigrationApply {
            plan,
            plan_digest,
            dry_run,
            verify,
        },
        invocation,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;
    use tracedecay_domain::UtcMicros;
    use tracedecay_store::ProjectId;

    use super::*;
    use crate::application::edit::{SourceEditApplicationResult, SourceEditOutcome};
    use crate::tracedecay::TraceDecayOpenOptions;
    use crate::types::EditResult;

    const EXPECTED_STATE: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PREDICTED_STATE: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SeenInvocation {
        dry_run: bool,
        idempotency_key: Option<String>,
        expected_state: Option<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RoutedInvocation {
        edit: SourceEditRequest,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: tracedecay_application::CancellationContext,
    }

    fn digest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).unwrap()
    }

    fn api_migration_plan() -> ApiMigrationPlanV1 {
        ApiMigrationPlanV1 {
            family_id: "family.mcp-route".to_owned(),
            repository_revision: "HEAD".to_owned(),
            graph_revision: digest(EXPECTED_STATE),
            operations: Vec::new(),
            sites: Vec::new(),
            files: Vec::new(),
            blocked: false,
            plan_digest: digest(PREDICTED_STATE),
        }
    }

    async fn fixture_graph(project_root: &Path) -> (TraceDecay, crate::db::DaemonDatabaseScope) {
        let profile_root = project_root.join(".tracedecay-test-profile");
        let open_options = TraceDecayOpenOptions {
            profile_root: Some(profile_root.clone()),
            global_db_path: Some(profile_root.join("global.db")),
        };
        let identity = crate::daemon::profile_identity::load_or_create(&profile_root).unwrap();
        let database_scope = crate::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "mcp-source-edit-test-runtime",
        )
        .unwrap();
        let runtime_registry = Arc::new(
            crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
                identity,
            )
            .await
            .unwrap(),
        );
        let profile_database = runtime_registry.profile_database().await.unwrap();
        let store_layout = TraceDecay::resolve_first_touch_configuration_layout(
            project_root,
            &open_options,
            profile_database.as_ref(),
        )
        .await
        .unwrap();
        let project_id = ProjectId::new(
            store_layout
                .identity
                .project_id
                .clone()
                .expect("fixture layout has a project identity"),
        )
        .unwrap();
        crate::storage::write_enrollment_marker(
            project_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
        let configuration_database = runtime_registry
            .project_sessions(
                project_id,
                [
                    project_root.to_path_buf(),
                    store_layout.project_root.clone(),
                ],
            )
            .await
            .unwrap();
        let graph = TraceDecay::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            configuration_database,
            profile_database,
            runtime_registry,
        )
        .await
        .unwrap();
        (graph, database_scope)
    }

    fn invocation_context(executor: Option<SourceEditExecutor>) -> SourceEditInvocationContext {
        SourceEditInvocationContext {
            executor,
            reconciliation_executor: None,
            request_id: Some(RequestId::new("request.mcp.source-edit.fixture").unwrap()),
            deadline: Some(Deadline::new(UtcMicros(i64::MAX)).unwrap()),
            cancellation: Some(
                CancellationSignal::active("cancel.mcp.source-edit.fixture").unwrap(),
            ),
        }
    }

    fn recording_executor(seen: Arc<Mutex<Vec<SeenInvocation>>>) -> SourceEditExecutor {
        Arc::new(move |invocation| {
            seen.lock().unwrap().push(SeenInvocation {
                dry_run: invocation.edit.dry_run(),
                idempotency_key: invocation
                    .idempotency_key
                    .as_ref()
                    .map(|key| key.as_str().to_owned()),
                expected_state: invocation
                    .expected_state
                    .as_ref()
                    .map(|state| state.as_str().to_owned()),
            });
            let dry_run = invocation.edit.dry_run();
            Box::pin(async move {
                Ok(SourceEditApplicationResult {
                    outcome: SourceEditOutcome::Edit(EditResult {
                        success: true,
                        file_path: "src/lib.rs".to_owned(),
                        matched_str: "old".to_owned(),
                        new_str: "new".to_owned(),
                        dry_run,
                        message: "source edit fixture completed".to_owned(),
                        ..EditResult::default()
                    }),
                    dry_run,
                    expected_state: digest(EXPECTED_STATE),
                    predicted_state: Some(digest(PREDICTED_STATE)),
                    verification: None,
                    effect: None,
                    replayed: false,
                })
            })
        })
    }

    fn route_recording_executor(seen: Arc<Mutex<Vec<RoutedInvocation>>>) -> SourceEditExecutor {
        Arc::new(move |invocation| {
            let dry_run = invocation.edit.dry_run();
            seen.lock().unwrap().push(RoutedInvocation {
                edit: invocation.edit,
                request_id: invocation.request_id,
                deadline: invocation.deadline,
                cancellation: invocation.cancellation.context(),
            });
            Box::pin(async move {
                Ok(SourceEditApplicationResult {
                    outcome: SourceEditOutcome::Edit(EditResult {
                        success: true,
                        file_path: "src/lib.rs".to_owned(),
                        dry_run,
                        message: "source edit route fixture completed".to_owned(),
                        ..EditResult::default()
                    }),
                    dry_run,
                    expected_state: digest(EXPECTED_STATE),
                    predicted_state: Some(digest(PREDICTED_STATE)),
                    verification: None,
                    effect: None,
                    replayed: false,
                })
            })
        })
    }

    #[tokio::test]
    async fn eight_source_edit_handlers_forward_exact_variants_defaults_and_controls() {
        let project = tempdir().unwrap();
        let (graph, _database_scope) = fixture_graph(project.path()).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let executor = route_recording_executor(Arc::clone(&seen));

        handle_str_replace(
            &graph,
            json!({"path":"src/lib.rs","old_str":"old","new_str":"new","dry_run":true}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        handle_multi_str_replace(
            &graph,
            json!({"path":"src/lib.rs","replacements":[["old","new"]],"dry_run":true}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        handle_insert_at(
            &graph,
            json!({"path":"src/lib.rs","anchor":"1","content":"new","dry_run":true}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        handle_ast_grep_rewrite(
            &graph,
            json!({"path":"src/lib.rs","pattern":"old","rewrite":"new","dry_run":true}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        handle_replace_symbol(
            &graph,
            json!({"symbol":"old","new_source":"fn new() {}","dry_run":true}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        handle_insert_at_symbol(
            &graph,
            json!({"symbol":"old","content":"fn new() {}","dry_run":true}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        handle_move_symbol(
            &graph,
            json!({"symbol":"old","dest_file":"src/new.rs"}),
            invocation_context(Some(Arc::clone(&executor))),
        )
        .await
        .unwrap();
        let plan = api_migration_plan();
        handle_api_migration_apply(
            &graph,
            json!({
                "plan": plan,
                "plan_digest": PREDICTED_STATE
            }),
            invocation_context(Some(executor)),
        )
        .await
        .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.iter()
                .map(|invocation| invocation.edit.kind())
                .collect::<Vec<_>>(),
            vec![
                SourceEditKind::StrReplace,
                SourceEditKind::MultiStrReplace,
                SourceEditKind::InsertAt,
                SourceEditKind::AstGrepRewrite,
                SourceEditKind::ReplaceSymbol,
                SourceEditKind::InsertAtSymbol,
                SourceEditKind::MoveSymbol,
                SourceEditKind::ApiMigrationApply,
            ]
        );
        assert!(seen.iter().all(|invocation| invocation.edit.dry_run()));
        assert!(matches!(
            &seen[2].edit,
            SourceEditRequest::InsertAt {
                before: false,
                verify: false,
                ..
            }
        ));
        assert!(matches!(
            &seen[5].edit,
            SourceEditRequest::InsertAtSymbol {
                position,
                verify: false,
                ..
            } if position == "after"
        ));
        assert!(matches!(
            &seen[6].edit,
            SourceEditRequest::MoveSymbol {
                update_references: false,
                ..
            }
        ));
        assert!(matches!(
            &seen[7].edit,
            SourceEditRequest::ApiMigrationApply {
                plan,
                plan_digest,
                dry_run: true,
                verify: true,
            } if plan.family_id == "family.mcp-route"
                && &plan.plan_digest == plan_digest
        ));
        for invocation in seen.iter() {
            assert_eq!(
                invocation.request_id.as_str(),
                "request.mcp.source-edit.fixture"
            );
            assert_eq!(invocation.deadline.expires_at, UtcMicros(i64::MAX));
            assert_eq!(
                invocation.cancellation.token_id.as_str(),
                "cancel.mcp.source-edit.fixture"
            );
            assert!(!invocation.cancellation.is_cancelled());
        }
    }

    #[tokio::test]
    async fn preview_accepts_no_effect_identity_and_returns_expected_state() {
        let project = tempdir().unwrap();
        let (graph, _database_scope) = fixture_graph(project.path()).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let result = handle_str_replace(
            &graph,
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "dry_run": true,
                "format": "json"
            }),
            invocation_context(Some(recording_executor(Arc::clone(&seen)))),
        )
        .await
        .unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![SeenInvocation {
                dry_run: true,
                idempotency_key: None,
                expected_state: None,
            }]
        );
        assert!(result.value.to_string().contains(EXPECTED_STATE));
    }

    #[tokio::test]
    async fn preview_remains_unavailable_until_source_edit_owner_is_installed() {
        let project = tempdir().unwrap();
        let (graph, _database_scope) = fixture_graph(project.path()).await;
        let error = handle_str_replace(
            &graph,
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "dry_run": true,
            }),
            invocation_context(None),
        )
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("daemon-owned source edit authority is unavailable")
        );
    }

    #[tokio::test]
    async fn apply_requires_preview_idempotency_and_expected_state() {
        let project = tempdir().unwrap();
        let (graph, _database_scope) = fixture_graph(project.path()).await;
        for args in [
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "idempotency_key": "edit.missing-expected"
            }),
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "expected_state": EXPECTED_STATE
            }),
        ] {
            let error = handle_str_replace(&graph, args, invocation_context(None))
                .await
                .unwrap_err();
            assert!(error.to_string().contains(
                "requires a fresh idempotency_key and the expected_state returned by a preview"
            ));
        }
    }

    #[tokio::test]
    async fn apply_forwards_exact_idempotency_key_and_expected_state() {
        let project = tempdir().unwrap();
        let (graph, _database_scope) = fixture_graph(project.path()).await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        handle_str_replace(
            &graph,
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "idempotency_key": "edit.mcp-exact",
                "expected_state": EXPECTED_STATE,
                "format": "json"
            }),
            invocation_context(Some(recording_executor(Arc::clone(&seen)))),
        )
        .await
        .unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![SeenInvocation {
                dry_run: false,
                idempotency_key: Some("edit.mcp-exact".to_owned()),
                expected_state: Some(EXPECTED_STATE.to_owned()),
            }]
        );
    }
}
