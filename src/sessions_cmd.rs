use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use crate::{
    cli::{
        SessionRefreshBeginArgs, SessionRefreshOperationArgs, SessionRefreshSelectors,
        SessionsAction, SessionsRefreshAction, SessionsSearchArgs,
    },
    resolve_cli_project_root,
};
use serde_json::{Map, Value, json};

const SESSION_REFRESH_TOOL: &str = "tracedecay_session_refresh";
const PROJECT_CONTEXT_TOOL: &str = "tracedecay_project_context";
const ACTIVE_PROJECT_TOOL: &str = "tracedecay_active_project";
const DEFAULT_REFRESH_PROFILE_ID: &str = "profile.primary";
const DEFAULT_REFRESH_TEMPORAL_MODE: &str = "current";
const DEFAULT_REFRESH_GRAIN: &str = "logical_message";

fn message_search_rpc_args(args: SessionsSearchArgs) -> Value {
    let SessionsSearchArgs {
        query,
        provider,
        scope,
        message_type,
        parent_session_id,
        limit,
        since,
        until,
        project_id: _,
        project_path: _,
        branch,
        worktree,
        commit,
    } = args;
    let mut arguments = Map::from_iter([
        ("query".to_string(), Value::String(query)),
        ("scope".to_string(), Value::String(scope)),
        ("message_type".to_string(), Value::String(message_type)),
        ("limit".to_string(), json!(limit)),
        ("format".to_string(), Value::String("json".to_string())),
    ]);
    for (key, value) in [
        ("provider", provider),
        ("parent_session_id", parent_session_id),
        ("since", since),
        ("until", until),
        ("branch", branch),
        ("worktree", worktree),
        ("commit", commit),
    ] {
        if let Some(value) = value {
            arguments.insert(key.to_string(), Value::String(value));
        }
    }
    Value::Object(arguments)
}

pub(crate) async fn handle_sessions_action(
    action: SessionsAction,
) -> tracedecay::errors::Result<()> {
    match action {
        SessionsAction::Ingest {
            provider,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            if let Some(provider) = provider.as_deref() {
                tracedecay::sessions::ProviderScope::parse_optional(Some(provider))
                    .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
            }
            let stats = call_daemon_tool(
                &project_path,
                "tracedecay_admin_cli",
                json!({ "action": "sessions_ingest" }),
            )
            .await?;
            println!(
                "ingested {} session(s), {} message(s)",
                stats["sessions_upserted"].as_u64().unwrap_or(0),
                stats["messages_upserted"].as_u64().unwrap_or(0)
            );
        }
        SessionsAction::Search(args) => {
            let project_id = args.project_id.clone();
            let project_path = args.project_path.clone();
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let payload = call_daemon_tool(
                &project_path,
                "tracedecay_message_search",
                message_search_rpc_args(*args),
            )
            .await?;
            for result in payload["results"].as_array().into_iter().flatten() {
                println!(
                    "[{}] {} {}: {}",
                    result
                        .pointer("/session/provider")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    result
                        .pointer("/session/project_key")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    result
                        .pointer("/message/role")
                        .and_then(Value::as_str)
                        .unwrap_or("-"),
                    result
                        .pointer("/message/text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .replace('\n', " ")
                );
            }
        }
        SessionsAction::Refresh { action } => {
            handle_session_refresh_action(action).await?;
        }
        SessionsAction::GitBackfill {
            project_id,
            project_path,
            since,
            limit_sessions,
            dry_run,
        } => {
            run_git_backfill(project_id, project_path, since, limit_sessions, dry_run).await?;
        }
        SessionsAction::Unfinished {
            limit,
            json,
            project_id,
            project_path,
        } => {
            let project_path = resolve_cli_project_root(None, project_id, project_path).await?;
            let payload = call_daemon_tool(
                &project_path,
                "tracedecay_admin_cli",
                json!({ "action": "sessions_unfinished", "limit": limit }),
            )
            .await?;
            let items = payload["items"].as_array().cloned().unwrap_or_default();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&items).map_err(|e| {
                        tracedecay::errors::TraceDecayError::Config {
                            message: e.to_string(),
                        }
                    })?
                );
            } else {
                for item in items {
                    let task_id = item["task_id"].as_str().unwrap_or("-");
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        item["status"].as_str().unwrap_or("-"),
                        item["provider"].as_str().unwrap_or("-"),
                        item["session_id"].as_str().unwrap_or("-"),
                        task_id,
                        item["message_id"].as_str().unwrap_or("-"),
                        item["evidence"].as_str().unwrap_or("")
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionRefreshMode {
    Start,
    Begin,
    Status,
    Join,
    Resume,
    Cancel,
}

impl SessionRefreshMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Begin => "begin",
            Self::Status => "status",
            Self::Join => "join",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }

    const fn begins_or_joins(self) -> bool {
        matches!(self, Self::Start | Self::Begin | Self::Join | Self::Resume)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionRefreshOutcome {
    Started,
    Joined,
    Busy,
    Running,
    Complete,
    Failed,
    Cancelled,
    Denied,
    WrongScope,
    Stale,
    NotFound,
    Aborted,
    DeadlineExceeded,
    Unavailable,
    Error,
}

impl SessionRefreshOutcome {
    fn parse(payload: &Value) -> tracedecay::errors::Result<Self> {
        let outcome = payload
            .get("outcome")
            .and_then(Value::as_str)
            .ok_or_else(|| refresh_response_error("omitted typed outcome"))?;
        match outcome {
            "started" => Ok(Self::Started),
            "joined" => Ok(Self::Joined),
            "busy" => Ok(Self::Busy),
            "running" => Ok(Self::Running),
            "complete" => Ok(Self::Complete),
            "failed" => Ok(Self::Failed),
            "cancelled" => payload
                .get("receipt")
                .filter(|receipt| receipt.is_object())
                .map(|_| Self::Cancelled)
                .ok_or_else(|| refresh_response_error("omitted durable cancellation receipt")),
            "denied" => Ok(Self::Denied),
            "wrong_scope" => Ok(Self::WrongScope),
            "stale" => Ok(Self::Stale),
            "not_found" => Ok(Self::NotFound),
            "aborted" => Ok(Self::Aborted),
            "deadline_exceeded" => Ok(Self::DeadlineExceeded),
            "unavailable" => Ok(Self::Unavailable),
            "error" => Ok(Self::Error),
            _ => Err(refresh_response_error("returned an unknown typed outcome")),
        }
    }

    const fn is_failure(self) -> bool {
        matches!(
            self,
            Self::Busy
                | Self::Failed
                | Self::Denied
                | Self::WrongScope
                | Self::Stale
                | Self::NotFound
                | Self::Aborted
                | Self::DeadlineExceeded
                | Self::Unavailable
                | Self::Error
        )
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Joined => "joined",
            Self::Busy => "busy",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Denied => "denied",
            Self::WrongScope => "wrong scope",
            Self::Stale => "stale",
            Self::NotFound => "not found",
            Self::Aborted => "aborted",
            Self::DeadlineExceeded => "deadline exceeded",
            Self::Unavailable => "unavailable",
            Self::Error => "error",
        }
    }
}

async fn handle_session_refresh_action(
    action: SessionsRefreshAction,
) -> tracedecay::errors::Result<()> {
    let transport = LiveSessionRefreshDaemonTransport;
    handle_session_refresh_action_with_transport(&transport, action).await
}

async fn handle_session_refresh_action_with_transport<T>(
    transport: &T,
    action: SessionsRefreshAction,
) -> tracedecay::errors::Result<()>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    match action {
        SessionsRefreshAction::Start(SessionRefreshBeginArgs { selectors, json }) => {
            dispatch_session_refresh(transport, SessionRefreshMode::Start, &selectors, None, json)
                .await
        }
        SessionsRefreshAction::Begin(SessionRefreshBeginArgs { selectors, json }) => {
            dispatch_session_refresh(transport, SessionRefreshMode::Begin, &selectors, None, json)
                .await
        }
        SessionsRefreshAction::Status(SessionRefreshOperationArgs {
            selectors,
            handle,
            json,
        }) => {
            dispatch_session_refresh(
                transport,
                SessionRefreshMode::Status,
                &selectors,
                Some(&handle),
                json,
            )
            .await
        }
        SessionsRefreshAction::Join(SessionRefreshBeginArgs { selectors, json }) => {
            dispatch_session_refresh(transport, SessionRefreshMode::Join, &selectors, None, json)
                .await
        }
        SessionsRefreshAction::Resume(SessionRefreshBeginArgs { selectors, json }) => {
            dispatch_session_refresh(
                transport,
                SessionRefreshMode::Resume,
                &selectors,
                None,
                json,
            )
            .await
        }
        SessionsRefreshAction::Cancel(SessionRefreshOperationArgs {
            selectors,
            handle,
            json,
        }) => {
            dispatch_session_refresh(
                transport,
                SessionRefreshMode::Cancel,
                &selectors,
                Some(&handle),
                json,
            )
            .await
        }
    }
}

async fn dispatch_session_refresh<T>(
    transport: &T,
    mode: SessionRefreshMode,
    selectors: &SessionRefreshSelectors,
    handle: Option<&str>,
    json_output: bool,
) -> tracedecay::errors::Result<()>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    let payload = execute_session_refresh(transport, mode, selectors, handle).await?;
    emit_session_refresh_outcome(payload, json_output)
}

async fn execute_session_refresh<T>(
    transport: &T,
    mode: SessionRefreshMode,
    selectors: &SessionRefreshSelectors,
    handle: Option<&str>,
) -> tracedecay::errors::Result<Value>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    if selectors.source > selectors.target {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "--source must not exceed --target for a session refresh".to_string(),
        });
    }
    tracedecay::sessions::ProviderScope::parse_optional(Some(&selectors.provider))
        .map_err(|message| tracedecay::errors::TraceDecayError::Config { message })?;
    validate_refresh_handle(mode, handle)?;

    let scope = resolve_session_refresh_scope(transport, selectors).await?;
    transport
        .call(
            scope.project_root.as_deref(),
            SESSION_REFRESH_TOOL,
            session_refresh_payload(mode, selectors, &scope, handle),
        )
        .await
}

fn validate_refresh_handle(
    mode: SessionRefreshMode,
    handle: Option<&str>,
) -> tracedecay::errors::Result<()> {
    let handle = handle.map(str::trim);
    if mode.begins_or_joins() {
        return match handle {
            Some(_) => Err(refresh_config_error(&format!(
                "sessions refresh {} does not accept a handle",
                mode.as_str()
            ))),
            None => Ok(()),
        };
    }
    match handle {
        None | Some("") => Err(refresh_config_error(
            "sessions refresh status/cancel requires the handle from start or begin",
        )),
        Some(_) => Ok(()),
    }
}

#[derive(Debug)]
struct ResolvedSessionRefreshScope {
    project_root: Option<PathBuf>,
    scope: &'static str,
    project_id: Option<String>,
    repository_id: Option<String>,
    worktree_id: Option<String>,
    branch_id: Option<String>,
    profile_id: String,
    store_id: String,
    root_id: String,
}

async fn resolve_session_refresh_scope<T>(
    transport: &T,
    selectors: &SessionRefreshSelectors,
) -> tracedecay::errors::Result<ResolvedSessionRefreshScope>
where
    T: SessionRefreshDaemonTransport + ?Sized,
{
    if let Some(profile_id) = selectors.profile_id.as_deref() {
        let suffix = profile_id.strip_prefix("profile.").unwrap_or(profile_id);
        if suffix.is_empty() {
            return Err(refresh_config_error("--profile-id must be non-empty"));
        }
        return Ok(ResolvedSessionRefreshScope {
            project_root: None,
            scope: "profile",
            project_id: None,
            repository_id: None,
            worktree_id: None,
            branch_id: None,
            profile_id: profile_id.to_string(),
            store_id: format!("store.profile.{suffix}"),
            root_id: format!("root.profile.{suffix}"),
        });
    }

    let context_args = match (
        selectors.project_id.as_deref(),
        selectors.project_path.as_deref(),
    ) {
        (Some(project_id), None) => json!({
            "project_id": project_id,
            "format": "json",
        }),
        (None, Some(project_path)) => json!({
            "path": project_path,
            "format": "json",
        }),
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
    let context = transport
        .call(None, PROJECT_CONTEXT_TOOL, context_args)
        .await?;
    if context.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(refresh_config_error(
            "registered project context was not found for the refresh selector",
        ));
    }
    let registry_project = context
        .get("project")
        .and_then(Value::as_object)
        .ok_or_else(|| refresh_response_error("omitted registered project context"))?;
    let project_root = PathBuf::from(required_context_string(registry_project, "display_root")?);
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

fn resolve_project_refresh_scope(
    context: &Value,
    active: &Value,
) -> tracedecay::errors::Result<ResolvedSessionRefreshScope> {
    if context.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(refresh_config_error(
            "registered project context was not found for the refresh selector",
        ));
    }
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
            store_id.to_string(),
            graph_scope.get("graph_scope_id")?.as_str()?.to_string(),
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
        scope: "project",
        project_id: Some(project_id),
        repository_id: Some(repository_id),
        worktree_id: Some(worktree_id),
        root_id: branch_id.clone(),
        branch_id: Some(branch_id),
        profile_id: DEFAULT_REFRESH_PROFILE_ID.to_string(),
        store_id,
    })
}

fn required_context_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> tracedecay::errors::Result<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            refresh_response_error(&format!(
                "omitted authoritative registered project `{field}`"
            ))
        })
}

fn session_refresh_payload(
    mode: SessionRefreshMode,
    selectors: &SessionRefreshSelectors,
    scope: &ResolvedSessionRefreshScope,
    handle: Option<&str>,
) -> Value {
    let mut payload = json!({
        "action": mode.as_str(),
        "scope": scope.scope,
        "profile": { "id": scope.profile_id },
        "session": {
            "id": selectors.session_id,
            "store_id": scope.store_id,
            "root_id": scope.root_id,
        },
        "source": { "scope": selectors.provider },
        "target": {
            "temporal_mode": { "kind": DEFAULT_REFRESH_TEMPORAL_MODE },
            "grain": DEFAULT_REFRESH_GRAIN,
            "frontier": {
                "observed_through": selectors.target,
                "committed_through": selectors.source,
            }
        },
        "format": "json",
    });
    let object = payload
        .as_object_mut()
        .expect("session refresh payload is an object");
    if scope.scope == "project" {
        object.insert(
            "project".to_string(),
            json!({
                "id": scope.project_id,
                "repository_id": scope.repository_id,
                "worktree_id": scope.worktree_id,
                "branch_id": scope.branch_id,
            }),
        );
    }
    if let Some(handle) = handle {
        object.insert("handle".to_string(), Value::String(handle.to_string()));
    }
    payload
}

fn emit_session_refresh_outcome(
    payload: Value,
    json_output: bool,
) -> tracedecay::errors::Result<()> {
    let outcome = SessionRefreshOutcome::parse(&payload)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("{}", session_refresh_human_outcome(outcome, &payload));
    }
    if outcome.is_failure() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("session refresh {}", outcome.label()),
        });
    }
    Ok(())
}

fn session_refresh_human_outcome(outcome: SessionRefreshOutcome, payload: &Value) -> String {
    let handle = payload
        .get("handle")
        .and_then(Value::as_str)
        .map(|handle| format!(" (handle {handle})"))
        .unwrap_or_default();
    let mut output = format!("session refresh {}{handle}", outcome.label());
    if let Some(progress) = payload.get("progress").and_then(Value::as_object) {
        append_session_refresh_record(&mut output, progress);
        if let Some(committed_batches) = progress.get("committed_batches").and_then(Value::as_u64) {
            output.push_str(&format!("; committed batches {committed_batches}"));
        }
        if let Some(committed_records) = progress.get("committed_records").and_then(Value::as_u64) {
            output.push_str(&format!("; committed records {committed_records}"));
        }
    }
    if let Some(receipt) = payload.get("receipt").and_then(Value::as_object) {
        append_session_refresh_record(&mut output, receipt);
        if let Some(state) = receipt.get("state").and_then(Value::as_str) {
            output.push_str(&format!("; receipt {state}"));
        }
        if let Some(failure_code) = receipt.get("failure_code").and_then(Value::as_str) {
            output.push_str(&format!("; failure {failure_code}"));
        }
    }
    if let Some(message) = payload
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        output.push_str(&format!(": {message}"));
    }
    output
}

fn append_session_refresh_record(output: &mut String, record: &serde_json::Map<String, Value>) {
    if let Some(operation_id) = record.get("operation_id").and_then(Value::as_str) {
        output.push_str(&format!("; operation {operation_id}"));
    }
    if let Some(frontier) = record.get("frontier").and_then(Value::as_object)
        && let (Some(observed), Some(committed)) = (
            frontier.get("observed_through").and_then(Value::as_u64),
            frontier.get("committed_through").and_then(Value::as_u64),
        )
    {
        output.push_str(&format!("; frontier {committed}/{observed}"));
    }
    if let Some(coverage) = record.get("coverage").and_then(Value::as_object) {
        let visible = coverage.get("visible").and_then(Value::as_u64).unwrap_or(0);
        let hidden = coverage.get("hidden").and_then(Value::as_u64).unwrap_or(0);
        let unknown = coverage.get("unknown").and_then(Value::as_u64).unwrap_or(0);
        let redacted = coverage
            .get("redacted")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        output.push_str(&format!(
            "; coverage visible {visible}, hidden {hidden}, unknown {unknown}, redacted {redacted}"
        ));
    }
}

fn refresh_config_error(message: &str) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: message.to_string(),
    }
}

fn refresh_response_error(detail: &str) -> tracedecay::errors::TraceDecayError {
    tracedecay::errors::TraceDecayError::Config {
        message: format!("daemon sessions refresh response {detail}"),
    }
}

/// Default lower bound for `git-backfill`: 90 days before now.
const GIT_BACKFILL_DEFAULT_WINDOW_SECS: i64 = 90 * 24 * 60 * 60;

async fn run_git_backfill(
    project_id: Option<String>,
    project_path: Option<String>,
    since: Option<String>,
    limit_sessions: usize,
    dry_run: bool,
) -> tracedecay::errors::Result<()> {
    let project_root = resolve_cli_project_root(None, project_id, project_path).await?;
    let since_ts = resolve_backfill_since(since.as_deref())?;
    let stats = call_daemon_tool(
        &project_root,
        "tracedecay_admin_cli",
        json!({
            "action": "sessions_git_backfill",
            "since": since_ts,
            "limit_sessions": limit_sessions,
            "dry_run": dry_run,
        }),
    )
    .await?;

    if dry_run {
        println!("git-backfill (dry-run): no rows written");
    }
    println!("sessions scanned:    {}", stats["sessions_scanned"]);
    println!("spans written:       {}", stats["spans_written"]);
    println!("commits attributed:  {}", stats["commits_attributed"]);
    println!(
        "skipped:             {} (no-window {}, not-worktree {}, git-error {})",
        stats["skipped_total"],
        stats["skipped_no_window"],
        stats["skipped_not_worktree"],
        stats["skipped_git_error"]
    );
    Ok(())
}

/// Resolves the `--since` argument (ISO-8601 or unix seconds) to a unix-second
/// lower bound, defaulting to 90 days before now when unset.
fn resolve_backfill_since(since: Option<&str>) -> tracedecay::errors::Result<i64> {
    let Some(raw) = since.map(str::trim).filter(|value| !value.is_empty()) else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        return Ok((now - GIT_BACKFILL_DEFAULT_WINDOW_SECS).max(0));
    };
    if let Ok(unix) = raw.parse::<i64>() {
        if unix >= 0 {
            return Ok(unix);
        }
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "--since must be >= 0".to_string(),
        });
    }
    tracedecay::timeutil::parse_rfc3339_timestamp(raw).ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "--since must be a non-negative Unix timestamp or ISO/RFC3339 string (got `{raw}`)"
            ),
        }
    })
}

async fn call_daemon_tool(
    project_root: &Path,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<Value> {
    call_daemon_tool_for_scope(Some(project_root), tool_name, arguments).await
}

async fn call_daemon_tool_for_scope(
    project_root: Option<&Path>,
    tool_name: &str,
    arguments: Value,
) -> tracedecay::errors::Result<Value> {
    let handshake = tracedecay::daemon::DaemonHandshake::for_current_client(
        project_root.map(Path::to_path_buf),
        None,
        false,
        false,
    )?;
    let result = tracedecay::daemon::call_default_tool(&handshake, tool_name, arguments).await?;
    tracedecay::daemon::tool_json_payload(&result, tool_name)
}

type SessionRefreshDaemonFuture<'a> =
    Pin<Box<dyn Future<Output = tracedecay::errors::Result<Value>> + Send + 'a>>;

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
        Box::pin(
            async move { call_daemon_tool_for_scope(project_root, tool_name, arguments).await },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use clap::Parser;
    use serde_json::{Value, json};

    use super::{
        SessionRefreshDaemonFuture, SessionRefreshDaemonTransport, SessionRefreshMode,
        SessionRefreshOutcome, SessionRefreshSelectors, dispatch_session_refresh,
        execute_session_refresh, message_search_rpc_args, session_refresh_human_outcome,
    };
    use crate::cli::{Cli, SessionsSearchArgs};

    #[test]
    fn message_search_rpc_args_omit_absent_optional_filters() {
        let args = message_search_rpc_args(SessionsSearchArgs {
            query: "example-query".to_string(),
            provider: None,
            scope: "all".to_string(),
            message_type: "all".to_string(),
            parent_session_id: None,
            limit: 3,
            since: None,
            until: None,
            project_id: None,
            project_path: None,
            branch: None,
            worktree: None,
            commit: None,
        });

        assert_eq!(
            args,
            json!({
                "query": "example-query",
                "scope": "all",
                "message_type": "all",
                "limit": 3,
                "format": "json",
            })
        );
    }

    #[test]
    fn message_search_rpc_args_preserve_explicit_typed_filters() {
        let args = message_search_rpc_args(SessionsSearchArgs {
            query: "example-query".to_string(),
            provider: Some("cursor".to_string()),
            scope: "subagents_only".to_string(),
            message_type: "direct_user".to_string(),
            parent_session_id: Some("parent-1".to_string()),
            limit: 5,
            since: Some("last hour".to_string()),
            until: Some("2026-07-28T00:00:00Z".to_string()),
            project_id: None,
            project_path: None,
            branch: Some("master".to_string()),
            worktree: Some("/repos/worktree".to_string()),
            commit: Some("abc123".to_string()),
        });

        assert_eq!(
            args,
            json!({
                "query": "example-query",
                "provider": "cursor",
                "scope": "subagents_only",
                "message_type": "direct_user",
                "parent_session_id": "parent-1",
                "limit": 5,
                "since": "last hour",
                "until": "2026-07-28T00:00:00Z",
                "branch": "master",
                "worktree": "/repos/worktree",
                "commit": "abc123",
                "format": "json",
            })
        );
    }

    fn project_selectors() -> SessionRefreshSelectors {
        SessionRefreshSelectors {
            project_id: None,
            project_path: Some("registered-alias".to_string()),
            profile_id: None,
            session_id: "session.refresh".to_string(),
            provider: "cursor".to_string(),
            source: 4,
            target: 9,
        }
    }

    fn project_context() -> Value {
        json!({
            "status": "ok",
            "project": {
                "project_id": "project.registered",
                "display_root": "/registered/authoritative-worktree",
                "canonical_root": "/registered/authoritative-worktree",
                "git_common_dir": "/registered/repository/.git",
                "default_branch": "master"
            },
            "aliases": [{
                "alias_path": "registered-alias",
                "project_id": "project.registered",
                "last_seen_at": 42
            }],
            "stores": [{
                "store": {
                    "store_id": "store.authoritative",
                    "project_id": "project.registered",
                    "store_kind": "code_project",
                    "storage_mode": "profile_sharded",
                    "store_relpath": "projects/project.registered"
                },
                "graph_scopes": [
                    {
                        "graph_scope_id": "scope.default",
                        "project_id": "project.registered",
                        "store_id": "store.authoritative",
                        "branch_name": "master",
                        "db_relpath": "codegraph-master.db",
                        "writable": true
                    },
                    {
                        "graph_scope_id": "scope.selected",
                        "project_id": "project.registered",
                        "store_id": "store.authoritative",
                        "branch_name": "feature/selected",
                        "db_relpath": "codegraph-selected.db",
                        "writable": true
                    }
                ],
                "artifacts": []
            }]
        })
    }

    fn active_project_context() -> Value {
        json!({
            "project_root": "/registered/authoritative-worktree",
            "resolution_source": "active_project",
            "branch": {
                "current_branch": "feature/selected",
                "open_active_branch": "feature/selected",
                "serving_branch": "feature/selected",
                "branch_drifted": false,
                "is_fallback": false
            }
        })
    }

    #[derive(Clone, Debug, PartialEq)]
    struct RecordedCall {
        project_root: Option<PathBuf>,
        tool_name: String,
        arguments: Value,
    }

    struct FakeDaemonTransport {
        responses: Mutex<VecDeque<Value>>,
        calls: Mutex<Vec<RecordedCall>>,
    }

    impl FakeDaemonTransport {
        fn new(responses: impl IntoIterator<Item = Value>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl SessionRefreshDaemonTransport for FakeDaemonTransport {
        fn call<'a>(
            &'a self,
            project_root: Option<&'a Path>,
            tool_name: &'a str,
            arguments: Value,
        ) -> SessionRefreshDaemonFuture<'a> {
            self.calls.lock().unwrap().push(RecordedCall {
                project_root: project_root.map(Path::to_path_buf),
                tool_name: tool_name.to_string(),
                arguments,
            });
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("fake daemon response");
            Box::pin(async move { Ok(response) })
        }
    }

    #[tokio::test]
    async fn project_refresh_uses_registered_authorities_and_exact_mcp_payload() {
        let transport = FakeDaemonTransport::new([
            project_context(),
            active_project_context(),
            json!({
                "outcome": "started",
                "operation_id": "internal-operation-17",
                "handle": "opaque-refresh-handle"
            }),
        ]);

        let response = execute_session_refresh(
            &transport,
            SessionRefreshMode::Begin,
            &project_selectors(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(response["handle"], "opaque-refresh-handle");
        let calls = transport.calls();
        assert_eq!(
            calls[0],
            RecordedCall {
                project_root: None,
                tool_name: "tracedecay_project_context".to_string(),
                arguments: json!({
                    "path": "registered-alias",
                    "format": "json"
                }),
            }
        );
        assert_eq!(
            calls[1],
            RecordedCall {
                project_root: Some(PathBuf::from("/registered/authoritative-worktree")),
                tool_name: "tracedecay_active_project".to_string(),
                arguments: json!({ "format": "json" }),
            }
        );
        assert_eq!(
            calls[2],
            RecordedCall {
                project_root: Some(PathBuf::from("/registered/authoritative-worktree")),
                tool_name: "tracedecay_session_refresh".to_string(),
                arguments: json!({
                    "action": "begin",
                    "scope": "project",
                    "project": {
                        "id": "project.registered",
                        "repository_id": "/registered/repository/.git",
                        "worktree_id": "/registered/authoritative-worktree",
                        "branch_id": "scope.selected"
                    },
                    "profile": { "id": "profile.primary" },
                    "session": {
                        "id": "session.refresh",
                        "store_id": "store.authoritative",
                        "root_id": "scope.selected"
                    },
                    "source": { "scope": "cursor" },
                    "target": {
                        "temporal_mode": { "kind": "current" },
                        "grain": "logical_message",
                        "frontier": {
                            "observed_through": 9,
                            "committed_through": 4
                        }
                    },
                    "format": "json"
                }),
            }
        );
        assert!(
            calls
                .iter()
                .all(|call| call.tool_name != "tracedecay_admin_cli")
        );
    }

    #[tokio::test]
    async fn profile_refresh_roundtrips_only_the_opaque_begin_handle() {
        let transport = FakeDaemonTransport::new([
            json!({
                "outcome": "started",
                "operation_id": "internal-operation-23",
                "handle": "opaque-profile-handle"
            }),
            json!({ "outcome": "running", "progress": null }),
        ]);
        let selectors = SessionRefreshSelectors {
            project_id: None,
            project_path: None,
            profile_id: Some("profile.primary".to_string()),
            session_id: "session.profile".to_string(),
            provider: "claude".to_string(),
            source: 2,
            target: 7,
        };

        let begin =
            execute_session_refresh(&transport, SessionRefreshMode::Begin, &selectors, None)
                .await
                .unwrap();
        let handle = begin["handle"].as_str().expect("begin handle");
        execute_session_refresh(
            &transport,
            SessionRefreshMode::Status,
            &selectors,
            Some(handle),
        )
        .await
        .unwrap();

        let calls = transport.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|call| call.project_root.is_none()));
        assert!(calls.iter().all(|call| {
            call.tool_name == "tracedecay_session_refresh"
                && call.arguments["scope"] == "profile"
                && call.arguments.get("project").is_none()
                && call.arguments["profile"]["id"] == "profile.primary"
                && call.arguments["session"]["store_id"] == "store.profile.primary"
                && call.arguments["session"]["root_id"] == "root.profile.primary"
        }));
        assert!(calls[0].arguments.get("handle").is_none());
        assert_eq!(calls[1].arguments["handle"], "opaque-profile-handle");
        assert_ne!(
            calls[1].arguments["handle"],
            Value::String("internal-operation-23".to_string())
        );
    }

    #[tokio::test]
    async fn refresh_without_explicit_scope_never_calls_daemon_or_discovers_cwd() {
        let transport = FakeDaemonTransport::new([]);
        let mut selectors = project_selectors();
        selectors.project_path = None;

        let error =
            execute_session_refresh(&transport, SessionRefreshMode::Begin, &selectors, None)
                .await
                .expect_err("refresh must not use the current directory as implicit scope");

        assert!(error.to_string().contains("never falls back"));
        assert!(transport.calls().is_empty());
    }

    #[tokio::test]
    async fn fake_daemon_semantic_failure_produces_a_nonzero_command_result() {
        let transport = FakeDaemonTransport::new([json!({
            "outcome": "wrong_scope",
            "error": {
                "code": "refresh_wrong_scope",
                "message": "the refresh handle does not belong to this profile"
            }
        })]);
        let selectors = SessionRefreshSelectors {
            project_id: None,
            project_path: None,
            profile_id: Some("profile.primary".to_string()),
            session_id: "session.profile".to_string(),
            provider: "cursor".to_string(),
            source: 2,
            target: 7,
        };

        let error = dispatch_session_refresh(
            &transport,
            SessionRefreshMode::Status,
            &selectors,
            Some("opaque-other-scope"),
            false,
        )
        .await
        .expect_err("semantic daemon failures must produce a nonzero CLI result");

        assert!(error.to_string().contains("wrong scope"));
    }

    #[tokio::test]
    async fn generated_refresh_payload_uses_only_declared_closed_schema_fields() {
        fn assert_declared(payload: &Value, schema: &Value) {
            let Some(payload) = payload.as_object() else {
                return;
            };
            let schema = schema
                .get("oneOf")
                .and_then(Value::as_array)
                .and_then(|variants| {
                    variants.iter().find(|variant| {
                        variant
                            .pointer("/properties/kind/const")
                            .and_then(Value::as_str)
                            == payload.get("kind").and_then(Value::as_str)
                    })
                })
                .unwrap_or(schema);
            let properties = schema["properties"].as_object().expect("schema properties");
            for (key, value) in payload {
                let field_schema = properties
                    .get(key)
                    .unwrap_or_else(|| panic!("payload field `{key}` absent from MCP schema"));
                assert_declared(value, field_schema);
            }
        }

        let transport = FakeDaemonTransport::new([
            project_context(),
            active_project_context(),
            json!({ "outcome": "started", "handle": "opaque" }),
        ]);
        execute_session_refresh(
            &transport,
            SessionRefreshMode::Begin,
            &project_selectors(),
            None,
        )
        .await
        .unwrap();
        let payload = &transport.calls()[2].arguments;
        let definition = tracedecay::mcp::tools::get_tool_definitions()
            .into_iter()
            .find(|definition| definition.name == "tracedecay_session_refresh")
            .expect("session refresh MCP definition");

        assert_eq!(
            definition.input_schema["additionalProperties"],
            Value::Bool(false)
        );
        assert_declared(payload, &definition.input_schema);
    }

    #[test]
    fn refresh_outcomes_have_human_labels_and_failure_semantics() {
        let started = SessionRefreshOutcome::parse(&json!({
            "outcome": "started",
            "operation_id": "internal-operation",
            "handle": "opaque-refresh-handle"
        }))
        .expect("started should be a typed outcome");
        assert_eq!(
            session_refresh_human_outcome(
                started,
                &json!({
                    "operation_id": "internal-operation",
                    "handle": "opaque-refresh-handle"
                })
            ),
            "session refresh started (handle opaque-refresh-handle)"
        );
        assert!(!started.is_failure());

        for (outcome, failure) in [
            ("started", false),
            ("joined", false),
            ("busy", true),
            ("running", false),
            ("complete", false),
            ("failed", true),
            ("denied", true),
            ("wrong_scope", true),
            ("stale", true),
            ("not_found", true),
            ("aborted", true),
            ("deadline_exceeded", true),
            ("unavailable", true),
            ("error", true),
        ] {
            assert_eq!(
                SessionRefreshOutcome::parse(&json!({"outcome": outcome}))
                    .expect("known typed outcome")
                    .is_failure(),
                failure,
                "unexpected CLI failure semantics for {outcome}"
            );
        }
        assert!(
            SessionRefreshOutcome::parse(&json!({"outcome": "cancelled"}))
                .expect_err("durable cancellation requires a receipt")
                .to_string()
                .contains("omitted durable cancellation receipt")
        );
        assert!(
            !SessionRefreshOutcome::parse(&json!({
                "outcome": "cancelled",
                "receipt": { "state": "cancelled" }
            }))
            .expect("receipt-backed cancellation")
            .is_failure()
        );
        assert!(
            SessionRefreshOutcome::parse(&json!({"outcome": "future"}))
                .expect_err("unknown outcomes must fail closed")
                .to_string()
                .contains("unknown typed outcome")
        );

        let running = json!({
            "outcome": "running",
            "progress": {
                "operation_id": "refresh-operation",
                "frontier": { "observed_through": 9, "committed_through": 4 },
                "coverage": { "visible": 3, "hidden": 1, "unknown": 0, "redacted": 0 },
                "committed_batches": 2,
                "committed_records": 4
            }
        });
        assert_eq!(
            session_refresh_human_outcome(
                SessionRefreshOutcome::parse(&running).unwrap(),
                &running,
            ),
            "session refresh running; operation refresh-operation; frontier 4/9; coverage visible 3, hidden 1, unknown 0, redacted 0; committed batches 2; committed records 4"
        );

        let invalid = json!({
            "outcome": "error",
            "error": {
                "code": "invalid_request",
                "message": "status requires a handle"
            }
        });
        let invalid_outcome = SessionRefreshOutcome::parse(&invalid).unwrap();
        assert!(invalid_outcome.is_failure());
        assert_eq!(
            session_refresh_human_outcome(invalid_outcome, &invalid),
            "session refresh error: status requires a handle"
        );
    }

    #[tokio::test]
    async fn start_join_resume_and_begin_preserve_mcp_wire_actions() {
        let transport = FakeDaemonTransport::new([
            json!({"outcome": "started", "handle": "start-handle"}),
            json!({"outcome": "joined", "handle": "join-handle"}),
            json!({"outcome": "joined", "handle": "resume-handle"}),
            json!({"outcome": "joined", "handle": "begin-handle"}),
        ]);
        let selectors = SessionRefreshSelectors {
            project_id: None,
            project_path: None,
            profile_id: Some("profile.primary".to_string()),
            session_id: "session.profile".to_string(),
            provider: "cursor".to_string(),
            source: 2,
            target: 7,
        };

        for mode in [
            SessionRefreshMode::Start,
            SessionRefreshMode::Join,
            SessionRefreshMode::Resume,
            SessionRefreshMode::Begin,
        ] {
            execute_session_refresh(&transport, mode, &selectors, None)
                .await
                .unwrap();
        }

        let calls = transport.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(
            calls
                .iter()
                .map(|call| call.arguments["action"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["start", "join", "resume", "begin"]
        );
        assert!(
            calls
                .iter()
                .all(|call| call.arguments.get("handle").is_none())
        );
    }

    #[test]
    fn refresh_cli_accepts_public_actions_and_begin_compatibility() {
        let selectors = [
            "--profile-id",
            "profile.primary",
            "--session-id",
            "session.profile",
            "--provider",
            "cursor",
            "--source",
            "2",
            "--target",
            "7",
        ];
        for action in ["start", "join", "resume", "begin"] {
            let args = ["tracedecay", "sessions", "refresh", action]
                .into_iter()
                .chain(selectors)
                .collect::<Vec<_>>();
            Cli::try_parse_from(args)
                .unwrap_or_else(|error| panic!("refresh action `{action}` should parse: {error}"));
        }
        for action in ["status", "cancel"] {
            let args = ["tracedecay", "sessions", "refresh", action]
                .into_iter()
                .chain(selectors)
                .chain(["--handle", "opaque-handle"])
                .collect::<Vec<_>>();
            Cli::try_parse_from(args)
                .unwrap_or_else(|error| panic!("refresh action `{action}` should parse: {error}"));
        }
    }

    #[tokio::test]
    async fn joined_complete_and_cancelled_are_successful_cli_outcomes() {
        let transport = FakeDaemonTransport::new([
            json!({"outcome": "joined", "handle": "joined-handle"}),
            json!({"outcome": "complete", "receipt": {"state": "complete"}}),
            json!({"outcome": "cancelled", "receipt": {"state": "cancelled"}}),
        ]);
        let selectors = SessionRefreshSelectors {
            project_id: None,
            project_path: None,
            profile_id: Some("profile.primary".to_string()),
            session_id: "session.profile".to_string(),
            provider: "cursor".to_string(),
            source: 2,
            target: 7,
        };

        dispatch_session_refresh(
            &transport,
            SessionRefreshMode::Join,
            &selectors,
            None,
            false,
        )
        .await
        .unwrap();
        dispatch_session_refresh(
            &transport,
            SessionRefreshMode::Status,
            &selectors,
            Some("joined-handle"),
            false,
        )
        .await
        .unwrap();
        dispatch_session_refresh(
            &transport,
            SessionRefreshMode::Cancel,
            &selectors,
            Some("joined-handle"),
            false,
        )
        .await
        .unwrap();

        let calls = transport.calls();
        assert_eq!(calls[1].arguments["handle"], "joined-handle");
        assert_eq!(calls[2].arguments["handle"], "joined-handle");
    }
}
