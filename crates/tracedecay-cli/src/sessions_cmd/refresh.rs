//! `tracedecay sessions refresh begin|status|cancel`.
//!
//! The CLI is one more transport for the canonical
//! `tracedecay_session_refresh_{begin,status,cancel}` operations: it resolves
//! the exact session-store owner (a registered project route or the profile's
//! own store), builds the canonical [`SessionRefreshActionRequestV1`], and
//! decodes the typed retained result. It never invents identity from the
//! current directory and never falls back to a project for a profile refresh.

use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};
use tracedecay_contracts::retained_surfaces::{
    RetainedErrorV1, RetainedOutcomeStatusV1, RetainedOutputFormatV1,
    SessionRefreshActionRequestV1, SessionRefreshBeginResultV1, SessionRefreshCancelResultV1,
    SessionRefreshFrontierV1, SessionRefreshGrainV1, SessionRefreshProgressV1,
    SessionRefreshProjectV1, SessionRefreshReceiptV1, SessionRefreshScopeV1,
    SessionRefreshSessionV1, SessionRefreshSourceV1, SessionRefreshStatusResultV1,
    SessionRefreshTargetV1, SessionRefreshTemporalModeV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::cli::{
    SessionRefreshBeginArgs, SessionRefreshOperationArgs, SessionRefreshSelectors,
    SessionsRefreshAction,
};
use crate::commands::{daemon_tool_json, retained_effect_payload, retained_tool_payload};

const REGISTRY_ADMIN_TOOL: &str = "tracedecay_admin_cli";
const ACTIVE_PROJECT_TOOL: &str = "tracedecay_active_project";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionRefreshOperation {
    Begin,
    Status,
    Cancel,
}

impl SessionRefreshOperation {
    #[hotpath::skip]
    const fn tool_name(self) -> &'static str {
        match self {
            Self::Begin => "tracedecay_session_refresh_begin",
            Self::Status => "tracedecay_session_refresh_status",
            Self::Cancel => "tracedecay_session_refresh_cancel",
        }
    }
}

/// The typed refresh result fields the CLI reports, common to the three
/// operations' result contracts.
#[derive(Clone, Debug, PartialEq)]
struct SessionRefreshOutcomeView {
    outcome: RetainedOutcomeStatusV1,
    handle: Option<String>,
    operation_id: Option<String>,
    progress: Option<SessionRefreshProgressV1>,
    receipt: Option<SessionRefreshReceiptV1>,
    error: Option<RetainedErrorV1>,
}

impl SessionRefreshOutcomeView {
    fn decode(operation: SessionRefreshOperation, reply: Value) -> Result<(Self, Value)> {
        let tool_name = operation.tool_name();
        let view = match operation {
            SessionRefreshOperation::Begin => {
                let result: SessionRefreshBeginResultV1 =
                    retained_effect_payload(tool_name, reply)?;
                Self {
                    outcome: result.outcome,
                    handle: result.handle.clone(),
                    operation_id: result.operation_id.clone(),
                    progress: result.progress.clone(),
                    receipt: result.receipt.clone(),
                    error: result.error.clone(),
                }
                .validated(serde_json::to_value(result)?)?
            }
            SessionRefreshOperation::Cancel => {
                let result: SessionRefreshCancelResultV1 =
                    retained_effect_payload(tool_name, reply)?;
                Self {
                    outcome: result.outcome,
                    handle: result.handle.clone(),
                    operation_id: result.operation_id.clone(),
                    progress: result.progress.clone(),
                    receipt: result.receipt.clone(),
                    error: result.error.clone(),
                }
                .validated(serde_json::to_value(result)?)?
            }
            SessionRefreshOperation::Status => {
                let result: SessionRefreshStatusResultV1 = retained_tool_payload(tool_name, reply)?;
                Self {
                    outcome: result.outcome,
                    handle: None,
                    operation_id: None,
                    progress: result.progress.clone(),
                    receipt: result.receipt.clone(),
                    error: result.error.clone(),
                }
                .validated(serde_json::to_value(result)?)?
            }
        };
        Ok(view)
    }

    /// A cancelled outcome is only durable when the daemon returned the
    /// terminal receipt; a bare `cancelled` word proves nothing.
    fn validated(self, payload: Value) -> Result<(Self, Value)> {
        if self.outcome == RetainedOutcomeStatusV1::Cancelled && self.receipt.is_none() {
            return Err(refresh_response_error(
                "omitted durable cancellation receipt",
            ));
        }
        Ok((self, payload))
    }

    #[hotpath::skip]
    const fn is_failure(&self) -> bool {
        !matches!(
            self.outcome,
            RetainedOutcomeStatusV1::Started
                | RetainedOutcomeStatusV1::Joined
                | RetainedOutcomeStatusV1::Running
                | RetainedOutcomeStatusV1::Complete
                | RetainedOutcomeStatusV1::Cancelled
        )
    }

    fn label(&self) -> String {
        match serde_json::to_value(self.outcome) {
            Ok(Value::String(label)) => label.replace('_', " "),
            _ => format!("{:?}", self.outcome),
        }
    }
}

pub(super) async fn handle_session_refresh_action(action: SessionsRefreshAction) -> Result<()> {
    let transport = LiveSessionRefreshDaemonTransport;
    handle_session_refresh_action_with_transport(&transport, action).await
}

async fn handle_session_refresh_action_with_transport<T>(
    transport: &T,
    action: SessionsRefreshAction,
) -> Result<()>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    match action {
        SessionsRefreshAction::Begin(SessionRefreshBeginArgs { selectors, json }) => {
            hotpath::future!(
                dispatch_session_refresh(
                    transport,
                    SessionRefreshOperation::Begin,
                    &selectors,
                    None,
                    json
                ),
                label = "cli.sessions.refresh.begin"
            )
            .await
        }
        SessionsRefreshAction::Status(SessionRefreshOperationArgs {
            selectors,
            handle,
            json,
        }) => {
            hotpath::future!(
                dispatch_session_refresh(
                    transport,
                    SessionRefreshOperation::Status,
                    &selectors,
                    Some(&handle),
                    json,
                ),
                label = "cli.sessions.refresh.status"
            )
            .await
        }
        SessionsRefreshAction::Cancel(SessionRefreshOperationArgs {
            selectors,
            handle,
            json,
        }) => {
            hotpath::future!(
                dispatch_session_refresh(
                    transport,
                    SessionRefreshOperation::Cancel,
                    &selectors,
                    Some(&handle),
                    json,
                ),
                label = "cli.sessions.refresh.cancel"
            )
            .await
        }
    }
}

async fn dispatch_session_refresh<T>(
    transport: &T,
    operation: SessionRefreshOperation,
    selectors: &SessionRefreshSelectors,
    handle: Option<&str>,
    json_output: bool,
) -> Result<()>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    let (view, payload) = execute_session_refresh(transport, operation, selectors, handle).await?;
    emit_session_refresh_outcome(&view, &payload, json_output)
}

async fn execute_session_refresh<T>(
    transport: &T,
    operation: SessionRefreshOperation,
    selectors: &SessionRefreshSelectors,
    handle: Option<&str>,
) -> Result<(SessionRefreshOutcomeView, Value)>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    if selectors.source > selectors.target {
        return Err(refresh_config_error(
            "--source must not exceed --target for a session refresh",
        ));
    }
    tracedecay_sessions::runtime::ProviderScope::parse_optional(Some(&selectors.provider))
        .map_err(|message| TraceDecayError::Config { message })?;
    let handle = validated_refresh_handle(operation, handle)?;

    let scope = resolve_session_refresh_scope(transport, selectors).await?;
    let request = session_refresh_request(selectors, &scope, handle);
    let reply = transport
        .call(
            scope.project_root.as_deref(),
            operation.tool_name(),
            serde_json::to_value(&request)?,
        )
        .await?;
    SessionRefreshOutcomeView::decode(operation, reply)
}

fn validated_refresh_handle(
    operation: SessionRefreshOperation,
    handle: Option<&str>,
) -> Result<Option<&str>> {
    let handle = handle.map(str::trim);
    match (operation, handle) {
        (SessionRefreshOperation::Begin, None) => Ok(None),
        (SessionRefreshOperation::Begin, Some(_)) => Err(refresh_config_error(
            "sessions refresh begin does not accept a handle",
        )),
        (SessionRefreshOperation::Status | SessionRefreshOperation::Cancel, None | Some("")) => {
            Err(refresh_config_error(
                "sessions refresh status/cancel requires the handle returned by begin",
            ))
        }
        (SessionRefreshOperation::Status | SessionRefreshOperation::Cancel, Some(handle)) => {
            Ok(Some(handle))
        }
    }
}

/// The exact session-store owner a refresh binds to, resolved from daemon
/// authorities. `project_root` is `None` for a profile-owned refresh: the call
/// travels on the projectless route and no project is ever consulted.
#[derive(Debug)]
struct ResolvedSessionRefreshScope {
    project_root: Option<PathBuf>,
    scope: SessionRefreshScopeV1,
    store_id: String,
    root_id: String,
}

async fn resolve_session_refresh_scope<T>(
    transport: &T,
    selectors: &SessionRefreshSelectors,
) -> Result<ResolvedSessionRefreshScope>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    if let Some(profile_id) = selectors.profile_id.as_deref() {
        return profile_refresh_scope(profile_id);
    }
    let project_arg = match (
        selectors.project_id.as_deref(),
        selectors.project_path.as_deref(),
    ) {
        (Some(project_id), None) => project_id,
        (None, Some(project_path)) => project_path,
        (None, None) => {
            return Err(refresh_config_error(
                "sessions refresh requires --project-id, --project-path, or --profile-id; it never falls back to the current directory",
            ));
        }
        (Some(_), Some(_)) => {
            return Err(refresh_config_error(
                "sessions refresh accepts only one project selector",
            ));
        }
    };
    // The registry context carries the daemon's durable profile id alongside
    // the registered project; the CLI never fabricates either.
    let context = transport
        .call(
            None,
            REGISTRY_ADMIN_TOOL,
            json!({ "action": "registry_context", "project_arg": project_arg }),
        )
        .await?;
    if context.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(refresh_config_error(
            "registered project context was not found for the refresh selector",
        ));
    }
    let project_root = PathBuf::from(required_context_string(
        context
            .get("project")
            .and_then(Value::as_object)
            .ok_or_else(|| refresh_response_error("omitted registered project context"))?,
        "display_root",
    )?);
    if !project_root.is_absolute() {
        return Err(refresh_response_error(
            "returned a non-absolute registered project root",
        ));
    }
    let active = transport
        .call(
            Some(&project_root),
            ACTIVE_PROJECT_TOOL,
            json!({ "format": "json" }),
        )
        .await?;
    resolve_project_refresh_scope(&context, &active)
}

fn profile_refresh_scope(profile_id: &str) -> Result<ResolvedSessionRefreshScope> {
    let suffix = profile_id
        .strip_prefix("profile.")
        .filter(|suffix| !suffix.is_empty())
        .ok_or_else(|| {
            refresh_config_error("--profile-id must be the typed `profile.<id>` identity")
        })?;
    Ok(ResolvedSessionRefreshScope {
        project_root: None,
        scope: SessionRefreshScopeV1::Profile {
            profile_id: profile_id.to_owned(),
        },
        store_id: format!("store.profile.{suffix}"),
        root_id: format!("root.profile.{suffix}"),
    })
}

fn resolve_project_refresh_scope(
    context: &Value,
    active: &Value,
) -> Result<ResolvedSessionRefreshScope> {
    let context_object = context
        .as_object()
        .ok_or_else(|| refresh_response_error("omitted registered project context"))?;
    let profile_id = required_context_string(context_object, "profile_id")?;
    let project = context
        .get("project")
        .and_then(Value::as_object)
        .ok_or_else(|| refresh_response_error("omitted registered project context"))?;
    let project_id = required_context_string(project, "project_id")?;
    let project_root = PathBuf::from(required_context_string(project, "display_root")?);
    if !project_root.is_absolute() {
        return Err(refresh_response_error(
            "returned a non-absolute registered project root",
        ));
    }
    let worktree_id = project_root.to_string_lossy().into_owned();
    let repository_id = required_context_string(project, "git_common_dir")?;
    let branch = active
        .get("branch")
        .and_then(Value::as_object)
        .ok_or_else(|| refresh_response_error("omitted active project branch context"))?;
    let branch_name = required_context_string(branch, "current_branch")?;

    let stores = context
        .get("stores")
        .and_then(Value::as_array)
        .ok_or_else(|| refresh_response_error("omitted registered project stores"))?;
    let mut matches = stores.iter().filter_map(|store_context| {
        let store = store_context.get("store")?.as_object()?;
        let store_id = store.get("store_id")?.as_str()?;
        let graph_scope = store_context
            .get("graph_scopes")?
            .as_array()?
            .iter()
            .find(|scope| {
                scope.get("branch_name").and_then(Value::as_str) == Some(branch_name.as_str())
                    && scope.get("store_id").and_then(Value::as_str) == Some(store_id)
                    && scope.get("writable").and_then(Value::as_bool) != Some(false)
            })?;
        Some((
            store_id.to_owned(),
            graph_scope.get("graph_scope_id")?.as_str()?.to_owned(),
        ))
    });
    let (store_id, branch_id) = matches.next().ok_or_else(|| {
        refresh_response_error("did not identify a writable session store for the project branch")
    })?;
    if matches.next().is_some() {
        return Err(refresh_response_error(
            "returned ambiguous session stores for the project branch",
        ));
    }

    Ok(ResolvedSessionRefreshScope {
        project_root: Some(project_root),
        scope: SessionRefreshScopeV1::Project {
            project: SessionRefreshProjectV1 {
                id: project_id,
                profile_id,
                repository_id,
                worktree_id,
                branch_id: branch_id.clone(),
            },
        },
        store_id,
        root_id: branch_id,
    })
}

fn required_context_string(object: &serde_json::Map<String, Value>, field: &str) -> Result<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            refresh_response_error(&format!(
                "omitted authoritative registered project `{field}`"
            ))
        })
}

fn session_refresh_request(
    selectors: &SessionRefreshSelectors,
    scope: &ResolvedSessionRefreshScope,
    handle: Option<&str>,
) -> SessionRefreshActionRequestV1 {
    SessionRefreshActionRequestV1 {
        scope: scope.scope.clone(),
        session: SessionRefreshSessionV1 {
            id: selectors.session_id.clone(),
            store_id: scope.store_id.clone(),
            root_id: scope.root_id.clone(),
        },
        source: SessionRefreshSourceV1 {
            scope: selectors.provider.clone(),
        },
        target: SessionRefreshTargetV1 {
            temporal_mode: SessionRefreshTemporalModeV1::Current,
            grain: SessionRefreshGrainV1::LogicalMessage,
            frontier: SessionRefreshFrontierV1 {
                observed_through: selectors.target,
                committed_through: selectors.source,
            },
        },
        handle: handle.map(str::to_owned),
        format: Some(RetainedOutputFormatV1::Json),
    }
}

fn emit_session_refresh_outcome(
    view: &SessionRefreshOutcomeView,
    payload: &Value,
    json_output: bool,
) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(payload)?);
    } else {
        println!("{}", session_refresh_human_outcome(view));
    }
    if view.is_failure() {
        return Err(TraceDecayError::Config {
            message: format!("session refresh {}", view.label()),
        });
    }
    Ok(())
}

fn session_refresh_human_outcome(view: &SessionRefreshOutcomeView) -> String {
    let mut output = format!("session refresh {}", view.label());
    if let Some(handle) = &view.handle {
        let _ = write!(output, " (handle {handle})");
    }
    if let Some(operation_id) = &view.operation_id {
        let _ = write!(output, "; operation {operation_id}");
    }
    if let Some(progress) = &view.progress {
        let _ = write!(
            output,
            "; frontier {}/{}; coverage visible {}, hidden {}, unknown {}, redacted {}; committed batches {}; committed records {}",
            progress.frontier.committed_through,
            progress.frontier.observed_through,
            progress.coverage.visible,
            progress.coverage.hidden,
            progress.coverage.unknown,
            progress.coverage.redacted,
            progress.committed_batches,
            progress.committed_records,
        );
    }
    if let Some(receipt) = &view.receipt {
        let state = match serde_json::to_value(receipt.state) {
            Ok(Value::String(state)) => state,
            _ => format!("{:?}", receipt.state),
        };
        let _ = write!(
            output,
            "; frontier {}/{}; coverage visible {}, hidden {}, unknown {}, redacted {}; receipt {state}",
            receipt.frontier.committed_through,
            receipt.frontier.observed_through,
            receipt.coverage.visible,
            receipt.coverage.hidden,
            receipt.coverage.unknown,
            receipt.coverage.redacted,
        );
        if let Some(failure_code) = &receipt.failure_code {
            let _ = write!(output, "; failure {failure_code}");
        }
    }
    if let Some(error) = &view.error {
        let _ = write!(output, ": {}", error.message);
    }
    output
}

fn refresh_config_error(message: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.to_owned(),
    }
}

fn refresh_response_error(detail: &str) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("daemon sessions refresh response {detail}"),
    }
}

type SessionRefreshDaemonFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

trait SessionRefreshDaemonTransport {
    fn call<'a>(
        &'a self,
        project_root: Option<&'a Path>,
        tool_name: &'a str,
        arguments: Value,
    ) -> SessionRefreshDaemonFuture<'a>;
}

struct LiveSessionRefreshDaemonTransport;

impl SessionRefreshDaemonTransport for LiveSessionRefreshDaemonTransport {
    fn call<'a>(
        &'a self,
        project_root: Option<&'a Path>,
        tool_name: &'a str,
        arguments: Value,
    ) -> SessionRefreshDaemonFuture<'a> {
        Box::pin(async move { daemon_tool_json(project_root, tool_name, arguments).await })
    }
}

#[cfg(test)]
mod tests;
