//! Source-edit tools served through the canonical application surface.
//!
//! Every transport decodes the arguments with the shared source-edit decoder,
//! dispatches the typed invocation, and renders the result as these tools
//! always have: typed refusals stay project-route errors, and a completed
//! edit reports its touched files and failure message.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::source_edit::SourceEditSurfaceResultV1;
use tracedecay_contracts::{
    ApplicationOutcome, ApplicationProblem, ApplicationProblemRecord, CancellationSignal, Deadline,
    InvocationTarget, PageRequest, RequestId, SafeDiagnostic,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, DaemonInvocationExecutor, RequestedOutputFormat,
    parse_source_edit_arguments,
};
use tracedecay_daemon_service::application_surface::{
    execute_application_surface, resolve_application_surface_dispatch_with_controls,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, BindingSurface};

use crate::ToolResult;
use crate::handlers::{generic_tool_result, rendered_tool_result};
use crate::tools::render;

/// Controls and daemon authority for one source-edit tool call.
#[derive(Clone)]
pub struct SourceEditInvocationContext<'a> {
    pub executor: Option<&'a dyn DaemonInvocationExecutor>,
    pub target: InvocationTarget,
    pub request_id: Option<RequestId>,
    pub deadline: Option<Deadline>,
    pub cancellation: Option<CancellationSignal>,
}

fn unavailable_authority(operation: ApplicationSurfaceOperation) -> TraceDecayError {
    let authority = match operation {
        ApplicationSurfaceOperation::SourceEditRollback => "source edit rollback authority",
        ApplicationSurfaceOperation::SourceEditReconcile => "source edit reconciliation authority",
        _ => "source edit authority",
    };
    TraceDecayError::Config {
        message: format!("daemon-owned {authority} is unavailable"),
    }
}

fn adapter_error(error: ApplicationSurfaceAdapterError) -> TraceDecayError {
    match error {
        ApplicationSurfaceAdapterError::DaemonUnreachable {
            reason_code,
            detail,
        } => ApplicationProblem::unavailable(SafeDiagnostic {
            code: reason_code,
            message: detail,
        })
        .into_trace_decay_error(),
        error => TraceDecayError::Config {
            message: error.to_string(),
        },
    }
}

/// A completed source-edit invocation: the typed result, or the daemon's
/// typed refusal.
pub type SourceEditOutcome =
    std::result::Result<SourceEditSurfaceResultV1, ApplicationProblemRecord>;

/// Run one source-edit tool on `surface` and render its tool result.
#[hotpath::measure(label = "mcp.edit.total", future = true)]
pub async fn source_edit_tool(
    project_root: Option<&Path>,
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    args: Value,
    invocation: SourceEditInvocationContext<'_>,
) -> Result<ToolResult> {
    let outcome = run_source_edit(surface, operation, &args, invocation).await?;
    render_source_edit_outcome(project_root, operation, &args, outcome)
}

/// Decode, dispatch, and settle one source-edit tool call. `Err` is an
/// argument or transport failure the caller reports as-is.
pub async fn run_source_edit(
    surface: BindingSurface,
    operation: ApplicationSurfaceOperation,
    args: &Value,
    invocation: SourceEditInvocationContext<'_>,
) -> Result<SourceEditOutcome> {
    let request = parse_source_edit_arguments(operation, args)
        .map_err(|message| TraceDecayError::Config { message })?;
    let SourceEditInvocationContext {
        executor,
        target,
        request_id,
        deadline,
        cancellation,
    } = invocation;
    let (Some(executor), Some(request_id), Some(deadline), Some(cancellation)) =
        (executor, request_id, deadline, cancellation)
    else {
        return Err(unavailable_authority(operation));
    };
    let mut dispatched = resolve_application_surface_dispatch_with_controls(
        surface,
        operation,
        request_id,
        request,
        PageRequest::first(10).map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?,
        Some(deadline),
        cancellation,
        RequestedOutputFormat::Json,
    )
    .map_err(adapter_error)?;
    dispatched.invocation.invocation.scope = target;
    let result = hotpath::future!(
        execute_application_surface(operation, dispatched, Some(executor)),
        label = "mcp.edit.execute"
    )
    .await
    .map_err(adapter_error)?;
    let envelope = match result.result {
        Ok(envelope) => envelope,
        Err(problem) => return Ok(Err(*problem.problem)),
    };
    let ApplicationOutcome::Result(value) = envelope.outcome else {
        return Err(unexpected_outcome());
    };
    serde_json::from_value(value)
        .map(Ok)
        .map_err(|_| unexpected_outcome())
}

/// Render a settled source edit as the edit tools always have.
pub fn render_source_edit_outcome(
    project_root: Option<&Path>,
    operation: ApplicationSurfaceOperation,
    args: &Value,
    outcome: SourceEditOutcome,
) -> Result<ToolResult> {
    let result = outcome.map_err(|problem| problem.into_source().into_trace_decay_error())?;
    render_source_edit_result(project_root, operation, args, &result)
}

fn unexpected_outcome() -> TraceDecayError {
    TraceDecayError::Config {
        message: "source edit invocation returned an unexpected outcome".to_owned(),
    }
}

fn render_source_edit_result(
    project_root: Option<&Path>,
    operation: ApplicationSurfaceOperation,
    args: &Value,
    result: &SourceEditSurfaceResultV1,
) -> Result<ToolResult> {
    let value = serde_json::to_value(result)?;
    let success = result.outcome.success();
    let tool_result = match operation {
        ApplicationSurfaceOperation::SourceEditRollback
        | ApplicationSurfaceOperation::SourceEditReconcile => {
            generic_tool_result(project_root, args, &value, Vec::new())
        }
        _ => rendered_tool_result(
            project_root,
            args,
            &value,
            result.outcome.touched_files(),
            || {
                result
                    .outcome
                    .as_move()
                    .map_or_else(|| render::generic_md(&value), move_result_md)
            },
        ),
    }
    .with_semantic_error(!success);
    Ok(if success {
        tool_result
    } else {
        tool_result.with_failure_message(result.outcome.message())
    })
}

/// Human-readable markdown for a move result: the outcome line, applied
/// imports, the impact report (the centerpiece), and the preview diff.
fn move_result_md(result: &tracedecay_contracts::source_edit::MoveResult) -> String {
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
        out.push_str("Clean move. No references, dependencies, or module concerns detected.\n");
    } else {
        for hint in &result.impact {
            let loc = hint
                .line
                .map_or_else(|| hint.file.clone(), |l| format!("{}:{}", hint.file, l));
            let _ = writeln!(out, "- **{}** ({}), {}", hint.kind, loc, hint.detail);
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tempfile::tempdir;
    use tracedecay_domain::{ManifestDigest, UtcMicros};

    use super::*;
    use serde_json::json;
    use tracedecay_contracts::source_edit::EditResult;
    use tracedecay_contracts::source_edit::{
        SourceEditSurfaceOutcomeV1, SourceEditSurfaceResultV1,
    };
    use tracedecay_contracts::{
        ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
        ApplicationProblem, ApplicationResponse, InvocationError, RetryDirective, SafeDiagnostic,
    };
    use tracedecay_contracts::{SourceEditKind, SourceEditRequest};
    use tracedecay_daemon_protocol::{
        DaemonInvocationError, DaemonInvocationExecutorFuture, DaemonInvocationOutcome,
        DaemonInvocationPayload, DaemonInvocationProblem, DaemonInvocationRequest,
        DaemonInvocationResponse, InvocationCancellationPolicy,
    };

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
        cancellation: tracedecay_contracts::CancellationContext,
    }

    fn digest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).unwrap()
    }

    fn invocation_context(
        executor: Option<&dyn DaemonInvocationExecutor>,
    ) -> SourceEditInvocationContext<'_> {
        SourceEditInvocationContext {
            executor,
            target: tracedecay_contracts::InvocationTarget::CurrentProject,
            request_id: Some(RequestId::new("request.mcp.source-edit.fixture").unwrap()),
            deadline: Some(Deadline::new(UtcMicros(i64::MAX)).unwrap()),
            cancellation: Some(
                CancellationSignal::active("cancel.mcp.source-edit.fixture").unwrap(),
            ),
        }
    }

    struct RecordingSourceEditExecutor {
        seen: Mutex<Vec<SeenInvocation>>,
        routed: Mutex<Vec<RoutedInvocation>>,
    }

    impl RecordingSourceEditExecutor {
        fn new() -> Self {
            Self {
                seen: Mutex::new(Vec::new()),
                routed: Mutex::new(Vec::new()),
            }
        }
    }

    impl ApplicationInvocationExecutor for RecordingSourceEditExecutor {
        fn invoke(
            &self,
            invocation: ApplicationInvocation,
        ) -> ApplicationInvocationFuture<
            '_,
            std::result::Result<ApplicationResponse, InvocationError>,
        > {
            Box::pin(async move {
                let (context, request) = invocation.into_parts();
                let tracedecay_contracts::ApplicationRequest::Surface { binding, payload } =
                    request
                else {
                    return Err(InvocationError::Unavailable);
                };
                tracedecay_daemon_protocol::invoke_application_surface(
                    self, context, binding, payload,
                )
                .await
            })
        }
    }

    impl DaemonInvocationExecutor for RecordingSourceEditExecutor {
        fn invoke_controlled(
            &self,
            request: DaemonInvocationRequest,
            deadline: Deadline,
            cancellation: CancellationSignal,
            _policy: InvocationCancellationPolicy,
        ) -> DaemonInvocationExecutorFuture<
            '_,
            std::result::Result<DaemonInvocationResponse, DaemonInvocationError>,
        > {
            let request_id = RequestId::new(request.request_id.clone())
                .unwrap_or_else(|_| RequestId::new("request.mcp.source-edit.fixture").unwrap());
            let DaemonInvocationPayload::SourceEdit {
                request: invocation,
                ..
            } = request.payload
            else {
                return Box::pin(async { Err(DaemonInvocationError::Unavailable) });
            };
            self.seen.lock().unwrap().push(SeenInvocation {
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
            self.routed.lock().unwrap().push(RoutedInvocation {
                edit: invocation.edit,
                request_id,
                deadline,
                cancellation: cancellation.context(),
            });
            Box::pin(async move {
                Ok(DaemonInvocationResponse::with_outcome(
                    "request.mcp.source-edit.fixture".to_owned(),
                    DaemonInvocationOutcome::SourceEdit {
                        scope: tracedecay_contracts::ResolvedScope::new(
                            tracedecay_domain::ProjectId::new("project.source-edit.fixture")
                                .unwrap(),
                            tracedecay_domain::RepositoryId::new("repository.source-edit.fixture")
                                .unwrap(),
                            tracedecay_domain::WorktreeId::new("worktree.source-edit.fixture")
                                .unwrap(),
                            None,
                        )
                        .unwrap(),
                        result: SourceEditSurfaceResultV1 {
                            outcome: SourceEditSurfaceOutcomeV1::Edit(EditResult {
                                success: true,
                                file_path: "src/lib.rs".to_owned(),
                                matched_str: "old".to_owned(),
                                new_str: "new".to_owned(),
                                dry_run,
                                message: "source edit fixture completed".to_owned(),
                                ..EditResult::default()
                            }),
                            expected_state: digest(EXPECTED_STATE),
                            predicted_state: Some(digest(PREDICTED_STATE)),
                            verification: None,
                            effect: None,
                            replayed: false,
                        },
                    },
                ))
            })
        }

        fn observe_feedback(
            &self,
            _subject_digest: ManifestDigest,
            _observed_at: UtcMicros,
            _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
        ) -> DaemonInvocationExecutorFuture<'_, tracedecay_domain::errors::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct RefusingSourceEditExecutor {
        outcome: DaemonInvocationOutcome,
    }

    impl ApplicationInvocationExecutor for RefusingSourceEditExecutor {
        fn invoke(
            &self,
            invocation: ApplicationInvocation,
        ) -> ApplicationInvocationFuture<
            '_,
            std::result::Result<ApplicationResponse, InvocationError>,
        > {
            Box::pin(async move {
                let (context, request) = invocation.into_parts();
                let tracedecay_contracts::ApplicationRequest::Surface { binding, payload } =
                    request
                else {
                    return Err(InvocationError::Unavailable);
                };
                tracedecay_daemon_protocol::invoke_application_surface(
                    self, context, binding, payload,
                )
                .await
            })
        }
    }

    impl DaemonInvocationExecutor for RefusingSourceEditExecutor {
        fn invoke_controlled(
            &self,
            _request: DaemonInvocationRequest,
            _deadline: Deadline,
            _cancellation: CancellationSignal,
            _policy: InvocationCancellationPolicy,
        ) -> DaemonInvocationExecutorFuture<
            '_,
            std::result::Result<DaemonInvocationResponse, DaemonInvocationError>,
        > {
            let outcome = self.outcome.clone();
            Box::pin(async move {
                Ok(DaemonInvocationResponse::with_outcome(
                    "request.mcp.source-edit.fixture".to_owned(),
                    outcome,
                ))
            })
        }

        fn observe_feedback(
            &self,
            _subject_digest: ManifestDigest,
            _observed_at: UtcMicros,
            _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
        ) -> DaemonInvocationExecutorFuture<'_, tracedecay_domain::errors::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    async fn source_edit_refusal(
        outcome: DaemonInvocationOutcome,
    ) -> tracedecay_domain::errors::TraceDecayError {
        let project = tempdir().unwrap();
        let executor = RefusingSourceEditExecutor { outcome };
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::StrReplace,
            json!({"path":"src/lib.rs","old_str":"old","new_str":"new","dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .expect_err("refused source edit must stay a typed failure")
    }

    #[tokio::test]
    async fn denied_source_edit_preserves_reason_code_and_is_not_retryable() {
        let error = source_edit_refusal(DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        })
        .await;
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("denial must stay a typed project-route error");
        assert_eq!(reason_code, "not_found_or_not_authorized");
        assert!(!retryable);
    }

    #[tokio::test]
    async fn warming_source_edit_gate_is_retryable_unavailable() {
        let error = source_edit_refusal(DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::unavailable(
                SafeDiagnostic::new(
                    tracedecay_contracts::RUNTIME_MOUNTING_REASON_CODE,
                    "The project runtime for this operation is still mounting",
                )
                .unwrap(),
            ),
        })
        .await;
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("warming must stay a typed project-route error");
        assert_eq!(
            reason_code,
            tracedecay_contracts::RUNTIME_MOUNTING_REASON_CODE
        );
        assert!(retryable);
    }

    #[tokio::test]
    async fn kernel_digest_mismatch_reaches_mcp_with_reason_code_and_retryability() {
        let error = source_edit_refusal(DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::stale(
                SafeDiagnostic::new(
                    "source_edit.expected_state_mismatch",
                    "source edit candidate state changed while its exact preview was captured",
                )
                .unwrap(),
            ),
        })
        .await;
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("digest mismatch must stay a typed project-route error");
        assert_eq!(reason_code, "source_edit.expected_state_mismatch");
        assert!(retryable);
        assert_ne!(reason_code, "not_found_or_not_authorized");
    }

    #[tokio::test]
    async fn kernel_conflict_reaches_mcp_with_reason_code_and_retryability() {
        let error = source_edit_refusal(DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::conflict(
                "source_edit.idempotency_conflict",
                "source edit idempotency key conflicts with a prior input",
            ),
        })
        .await;
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("idempotency conflict must stay a typed project-route error");
        assert_eq!(reason_code, "source_edit.idempotency_conflict");
        assert!(retryable);
        assert_ne!(reason_code, "not_found_or_not_authorized");
    }

    #[tokio::test]
    async fn source_edit_protocol_problem_stays_typed_without_debug_formatting() {
        let error = source_edit_refusal(DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::NotFoundOrNotAuthorized,
        })
        .await;
        let (reason_code, retryable, _) = error
            .project_route_context()
            .expect("protocol refusal must stay a typed project-route error");
        assert_eq!(reason_code, "not_found_or_not_authorized");
        assert!(!retryable);
        assert!(!error.to_string().contains("NotFoundOrNotAuthorized"));
    }

    #[tokio::test]
    async fn source_edit_handlers_forward_exact_variants_defaults_and_controls() {
        let project = tempdir().unwrap();
        let executor = RecordingSourceEditExecutor::new();

        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::StrReplace,
            json!({"path":"src/lib.rs","old_str":"old","new_str":"new","dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::MultiStrReplace,
            json!({"path":"src/lib.rs","replacements":[["old","new"]],"dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::InsertAt,
            json!({"path":"src/lib.rs","anchor":"1","content":"new","dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::AstGrepRewrite,
            json!({"path":"src/lib.rs","pattern":"old","rewrite":"new","dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::ReplaceSymbol,
            json!({"symbol":"old","new_source":"fn new() {}","dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::InsertAtSymbol,
            json!({"symbol":"old","content":"fn new() {}","dry_run":true}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::MoveSymbol,
            json!({"symbol":"old","dest_file":"src/new.rs"}),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::RenameSymbol,
            json!({
                "node_id": "node.fixture",
                "qualified_name": "old",
                "kind": "function",
                "file": "src/lib.rs",
                "old_name": "old",
                "new_name": "renamed",
                "__mcp_request_id": "request.mcp.source-edit.fixture"
            }),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();

        let seen = executor.routed.lock().unwrap();
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
                SourceEditKind::RenameSymbol,
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
            SourceEditRequest::RenameSymbol {
                dry_run: true,
                verify: true,
                binding,
                ..
            } if binding.accepted_preview.is_none()
        ));
        for invocation in seen.iter() {
            assert_eq!(
                invocation.request_id.as_str(),
                "request.mcp.source-edit.fixture"
            );
            // The catalog's 30 s source-edit deadline bounds the caller's.
            assert!(invocation.deadline.expires_at < UtcMicros(i64::MAX));
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
        let executor = RecordingSourceEditExecutor::new();
        let result = source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::StrReplace,
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "dry_run": true,
                "format": "json"
            }),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();

        assert_eq!(
            *executor.seen.lock().unwrap(),
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
        let error = source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::StrReplace,
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
            let error = source_edit_tool(
                Some(project.path()),
                BindingSurface::Mcp,
                ApplicationSurfaceOperation::StrReplace,
                args,
                invocation_context(None),
            )
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
        let executor = RecordingSourceEditExecutor::new();
        source_edit_tool(
            Some(project.path()),
            BindingSurface::Mcp,
            ApplicationSurfaceOperation::StrReplace,
            json!({
                "path": "src/lib.rs",
                "old_str": "old",
                "new_str": "new",
                "idempotency_key": "edit.mcp-exact",
                "expected_state": EXPECTED_STATE,
                "format": "json"
            }),
            invocation_context(Some(&executor)),
        )
        .await
        .unwrap();

        assert_eq!(
            *executor.seen.lock().unwrap(),
            vec![SeenInvocation {
                dry_run: false,
                idempotency_key: Some("edit.mcp-exact".to_owned()),
                expected_state: Some(EXPECTED_STATE.to_owned()),
            }]
        );
    }
}
