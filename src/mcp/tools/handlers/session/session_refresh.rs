use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    RequestContext, RequestId,
};
use tracedecay_domain::{
    ActorId, ProjectId, RepositoryId, RetrievalGrainV1, SessionId, SessionSourceCoverageV1,
    TemporalModeV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::super::support::tool_json_with_md;
use crate::application::context::{
    BranchId, CancellationToken, CapabilityDigest, ConfigurationDigest, PolicyDigest, ProfileId,
    RequestBudgets, ResolvedGitRoute, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    application_observed_at, session_application_grant_digest,
};
use crate::application::session::{SessionRefreshTarget, SessionRequestBinding};
use crate::errors::Result;
use crate::mcp::tools::ToolResult;
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_store::SessionRefreshFrontierV1;

const TOOL_NAME: &str = "tracedecay_session_refresh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_MAX_RESULTS: u64 = 64;
const REQUEST_MAX_BYTES: u64 = 64 * 1024 * 1024;
const REQUEST_MAX_WORK_UNITS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionRefreshAction {
    Begin,
    Status,
    Cancel,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SessionRefreshRequestAction {
    Start,
    Status,
    Join,
    Resume,
    Cancel,
    Begin,
}

impl SessionRefreshRequestAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Status => "status",
            Self::Join => "join",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
            Self::Begin => "begin",
        }
    }

    const fn command_action(self) -> SessionRefreshAction {
        match self {
            Self::Start | Self::Join | Self::Resume | Self::Begin => SessionRefreshAction::Begin,
            Self::Status => SessionRefreshAction::Status,
            Self::Cancel => SessionRefreshAction::Cancel,
        }
    }

    const fn begins_or_joins(self) -> bool {
        matches!(self, Self::Start | Self::Join | Self::Resume | Self::Begin)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionRefreshScope {
    Project,
    Profile,
}

impl SessionRefreshScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Profile => "profile",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRefreshCommand {
    pub(crate) action: SessionRefreshAction,
    pub(crate) context: RequestContext,
    pub(crate) binding: SessionRequestBinding,
    pub(crate) target: SessionRefreshTarget,
    pub(crate) handle: Option<String>,
}

pub(crate) type SessionRefreshServiceFuture<'a> =
    Pin<Box<dyn Future<Output = SessionRefreshServiceOutcome> + Send + 'a>>;

pub(crate) trait SessionRefreshServicePort: Send + Sync {
    fn execute(&self, command: SessionRefreshCommand) -> SessionRefreshServiceFuture<'_>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshProgressView {
    pub(crate) operation_id: String,
    pub(crate) session_id: String,
    pub(crate) frontier: SessionRefreshFrontierView,
    pub(crate) coverage: SessionRefreshCoverageView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) source_coverage: Vec<SessionSourceCoverageV1>,
    pub(crate) committed_batches: u64,
    pub(crate) committed_records: u64,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshReceiptView {
    pub(crate) operation_id: String,
    pub(crate) session_id: String,
    pub(crate) frontier: SessionRefreshFrontierView,
    pub(crate) coverage: SessionRefreshCoverageView,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) source_coverage: Vec<SessionSourceCoverageV1>,
    pub(crate) state: String,
    pub(crate) failure_code: Option<String>,
    pub(crate) terminal_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshFrontierView {
    pub(crate) observed_through: u64,
    pub(crate) committed_through: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionRefreshCoverageView {
    pub(crate) visible: u64,
    pub(crate) hidden: u64,
    pub(crate) unknown: u64,
    pub(crate) redacted: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionRefreshServiceOutcome {
    Started {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    Joined {
        operation_id: String,
        handle: String,
        accepted_at: i64,
    },
    Busy,
    Running(Option<SessionRefreshProgressView>),
    Complete(SessionRefreshReceiptView),
    Failed(SessionRefreshReceiptView),
    Cancelled(SessionRefreshReceiptView),
    Denied,
    WrongScope,
    Stale,
    NotFound,
    Aborted,
    DeadlineExceeded,
    Unavailable,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct SessionRefreshServices<'a> {
    project: Option<&'a dyn SessionRefreshServicePort>,
    profile: Option<&'a dyn SessionRefreshServicePort>,
}

impl<'a> SessionRefreshServices<'a> {
    pub(crate) const fn new(
        project: Option<&'a dyn SessionRefreshServicePort>,
        profile: Option<&'a dyn SessionRefreshServicePort>,
    ) -> Self {
        Self { project, profile }
    }

    fn for_scope(self, scope: SessionRefreshScope) -> Option<&'a dyn SessionRefreshServicePort> {
        match scope {
            SessionRefreshScope::Project => self.project,
            SessionRefreshScope::Profile => self.profile,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionRefreshRequest {
    action: SessionRefreshRequestAction,
    scope: SessionRefreshScope,
    project: Option<ProjectSelector>,
    profile: ProfileSelector,
    session: SessionSelector,
    source: SourceSelector,
    target: TargetSelector,
    handle: Option<String>,
    format: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectSelector {
    id: String,
    repository_id: String,
    worktree_id: String,
    branch_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileSelector {
    id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionSelector {
    id: String,
    store_id: String,
    root_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceSelector {
    scope: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TargetSelector {
    temporal_mode: TemporalModeV1,
    grain: RetrievalGrainV1,
    frontier: FrontierSelector,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FrontierSelector {
    observed_through: u64,
    committed_through: u64,
}

pub(crate) async fn handle_session_refresh(
    args: Value,
    services: SessionRefreshServices<'_>,
) -> Result<ToolResult> {
    let request = match parse_request(args.clone()) {
        Ok(request) => request,
        Err(message) => {
            return Ok(render_error(&args, None, None, "invalid_request", &message));
        }
    };
    let action = request.action;
    let scope = request.scope;
    let command = match command_from_request(request) {
        Ok(command) => command,
        Err(message) => {
            return Ok(render_error(
                &args,
                Some(action),
                Some(scope),
                "invalid_request",
                &message,
            ));
        }
    };
    let outcome = match services.for_scope(scope) {
        Some(service) => service.execute(command).await,
        None => SessionRefreshServiceOutcome::Unavailable,
    };
    Ok(render_outcome(&args, action, scope, outcome))
}

fn parse_request(args: Value) -> std::result::Result<SessionRefreshRequest, String> {
    let request: SessionRefreshRequest = serde_json::from_value(args)
        .map_err(|error| format!("invalid session refresh request: {error}"))?;
    if request
        .format
        .as_deref()
        .is_some_and(|format| !matches!(format, "markdown" | "json"))
    {
        return Err("format must be one of markdown, json".to_string());
    }
    if request
        .handle
        .as_deref()
        .is_some_and(|handle| handle.trim().is_empty())
    {
        return Err("refresh handle must be non-empty".to_string());
    }
    Ok(request)
}

fn command_from_request(
    request: SessionRefreshRequest,
) -> std::result::Result<SessionRefreshCommand, String> {
    match (request.scope, request.project.as_ref()) {
        (SessionRefreshScope::Project, None) => {
            return Err("project scope requires the project selector".to_string());
        }
        (SessionRefreshScope::Profile, Some(_)) => {
            return Err("profile scope does not accept the project selector".to_string());
        }
        _ => {}
    }
    match (
        request.action.begins_or_joins(),
        request.action,
        request.handle.as_deref(),
    ) {
        (true, action, Some(_)) => {
            return Err(format!(
                "{} does not accept a refresh handle",
                action.as_str()
            ));
        }
        (
            false,
            SessionRefreshRequestAction::Status | SessionRefreshRequestAction::Cancel,
            None,
        ) => {
            return Err(format!(
                "{} requires the refresh handle returned by start or begin",
                request.action.as_str()
            ));
        }
        _ => {}
    }
    let identity = resolved_identity(&request)?;
    let digest_material = stable_digest_material(&request)?;
    let request_id = mint_global_request_id(GlobalRequestSurface::SessionRefresh)
        .map_err(|error| error.to_string())?;
    let request_id = RequestId::new(request_id.as_str()).map_err(|error| error.to_string())?;
    let actor = ActorId::new("mcp.session-refresh").map_err(|error| error.to_string())?;
    let scope = identity
        .session_request_scope()
        .map_err(|error| error.to_string())?;
    let capability = CapabilityDigest::new(stable_digest(
        b"tracedecay.mcp.session-refresh.capability.v1\0",
        &digest_material,
    ));
    let policy = PolicyDigest::new(stable_digest(
        b"tracedecay.mcp.session-refresh.policy.v1\0",
        &digest_material,
    ));
    let configuration = ConfigurationDigest::new(stable_digest(
        b"tracedecay.mcp.session-refresh.configuration.v1\0",
        &digest_material,
    ));
    let cancellation = CancellationToken::for_application_request(request_id.as_str());
    let budgets = RequestBudgets::new(
        REQUEST_MAX_RESULTS,
        REQUEST_MAX_BYTES,
        REQUEST_MAX_WORK_UNITS,
    )
    .map_err(|error| error.to_string())?;
    let observed_at = application_observed_at();
    let timeout_micros = i64::try_from(REQUEST_TIMEOUT.as_micros()).unwrap_or(i64::MAX);
    let expires_at = UtcMicros(observed_at.0.saturating_add(timeout_micros));
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.mcp.session-refresh").map_err(|error| error.to_string())?,
        1,
        session_application_grant_digest(capability, policy, configuration, &cancellation, budgets)
            .map_err(|error| error.to_string())?,
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        std::collections::BTreeSet::from([
            CapabilityId::new("capability.session.refresh").map_err(|error| error.to_string())?
        ]),
        std::collections::BTreeSet::from([
            UseCaseId::new("use-case.mcp.session-refresh").map_err(|error| error.to_string())?
        ]),
        DisclosureClass::Evidence,
    )
    .map_err(|error| error.to_string())?;
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        request_id,
        Deadline::new(expires_at).map_err(|error| error.to_string())?,
        CancellationContext::active(
            cancellation
                .application_token_id()
                .ok_or_else(|| "session refresh cancellation identity is missing".to_string())?,
        )
        .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let binding = SessionRequestBinding::new(
        identity,
        capability,
        policy,
        configuration,
        cancellation,
        budgets,
    );
    let frontier = SessionRefreshFrontierV1::new(
        request.target.frontier.observed_through,
        request.target.frontier.committed_through,
    )
    .map_err(|error| error.to_string())?;
    let target = SessionRefreshTarget::new(
        SessionId::new(request.session.id).map_err(|error| error.to_string())?,
        Some(request.source.scope),
        request.target.temporal_mode,
        request.target.grain,
        frontier,
    )
    .map_err(|error| error.to_string())?;
    Ok(SessionRefreshCommand {
        action: request.action.command_action(),
        context,
        binding,
        target,
        handle: request.handle,
    })
}

fn resolved_identity(
    request: &SessionRefreshRequest,
) -> std::result::Result<ResolvedSessionIdentity, String> {
    let profile_id =
        ProfileId::new(request.profile.id.clone()).map_err(|error| error.to_string())?;
    let store_id =
        SessionStoreId::new(request.session.store_id.clone()).map_err(|error| error.to_string())?;
    let root_id =
        SessionRootId::new(request.session.root_id.clone()).map_err(|error| error.to_string())?;
    match request.scope {
        SessionRefreshScope::Profile => Ok(ResolvedSessionIdentity::for_profile(
            profile_id, store_id, root_id,
        )),
        SessionRefreshScope::Project => {
            let project = request
                .project
                .as_ref()
                .ok_or_else(|| "project scope requires the project selector".to_string())?;
            Ok(ResolvedSessionIdentity::for_project(
                profile_id,
                ProjectId::new(project.id.clone()).map_err(|error| error.to_string())?,
                store_id,
                root_id,
                ResolvedGitRoute::new(
                    RepositoryId::new(project.repository_id.clone())
                        .map_err(|error| error.to_string())?,
                    WorktreeId::new(project.worktree_id.clone())
                        .map_err(|error| error.to_string())?,
                    BranchId::new(project.branch_id.clone()).map_err(|error| error.to_string())?,
                ),
            ))
        }
    }
}

fn stable_digest_material(request: &SessionRefreshRequest) -> std::result::Result<Vec<u8>, String> {
    serde_json::to_vec(&json!({
        "scope": request.scope,
        "project": &request.project,
        "profile": &request.profile,
        "session": &request.session,
        "source": &request.source,
        "target": &request.target,
    }))
    .map_err(|error| format!("could not bind session refresh selectors: {error}"))
}

fn stable_digest(domain: &[u8], material: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(material);
    digest.finalize().into()
}

fn render_outcome(
    args: &Value,
    action: SessionRefreshRequestAction,
    scope: SessionRefreshScope,
    outcome: SessionRefreshServiceOutcome,
) -> ToolResult {
    let (outcome_name, fields, semantic_error) = outcome_fields(outcome);
    let mut value = json!({
        "tool": TOOL_NAME,
        "action": action.as_str(),
        "scope": scope.as_str(),
        "outcome": outcome_name,
    });
    if let (Some(object), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        object.extend(fields.clone());
    }
    let result = tool_json_with_md(None, args, &value, || refresh_markdown(&value))
        .with_semantic_error(semantic_error);
    if semantic_error {
        result.with_failure_message(outcome_failure_message(outcome_name))
    } else {
        result
    }
}

fn outcome_fields(outcome: SessionRefreshServiceOutcome) -> (&'static str, Value, bool) {
    match outcome {
        SessionRefreshServiceOutcome::Started {
            operation_id,
            handle,
            accepted_at,
        } => (
            "started",
            json!({
                "operation_id": operation_id,
                "handle": handle,
                "accepted_at": accepted_at,
            }),
            false,
        ),
        SessionRefreshServiceOutcome::Joined {
            operation_id,
            handle,
            accepted_at,
        } => (
            "joined",
            json!({
                "operation_id": operation_id,
                "handle": handle,
                "accepted_at": accepted_at,
            }),
            false,
        ),
        SessionRefreshServiceOutcome::Busy => (
            "busy",
            error_field(
                "refresh_busy",
                "a conflicting refresh target is already running",
            ),
            true,
        ),
        SessionRefreshServiceOutcome::Running(progress) => {
            ("running", json!({ "progress": progress }), false)
        }
        SessionRefreshServiceOutcome::Complete(receipt) => {
            ("complete", json!({ "receipt": receipt }), false)
        }
        SessionRefreshServiceOutcome::Failed(receipt) => (
            "failed",
            json!({
                "receipt": receipt,
                "error": {
                    "code": "refresh_failed",
                    "message": "the durable session refresh failed"
                }
            }),
            true,
        ),
        SessionRefreshServiceOutcome::Cancelled(receipt) => {
            ("cancelled", json!({ "receipt": receipt }), false)
        }
        SessionRefreshServiceOutcome::Denied => (
            "denied",
            error_field(
                "refresh_denied",
                "the caller is not authorized for this session refresh",
            ),
            true,
        ),
        SessionRefreshServiceOutcome::WrongScope => (
            "wrong_scope",
            error_field(
                "refresh_wrong_scope",
                "the refresh handle does not belong to the requested scope",
            ),
            true,
        ),
        SessionRefreshServiceOutcome::Stale => (
            "stale",
            error_field(
                "refresh_handle_stale",
                "the refresh handle is no longer current",
            ),
            true,
        ),
        SessionRefreshServiceOutcome::NotFound => (
            "not_found",
            error_field(
                "refresh_handle_not_found",
                "the refresh handle was not found",
            ),
            true,
        ),
        SessionRefreshServiceOutcome::Aborted => (
            "aborted",
            error_field("refresh_aborted", "the session refresh request was aborted"),
            true,
        ),
        SessionRefreshServiceOutcome::DeadlineExceeded => (
            "deadline_exceeded",
            error_field(
                "refresh_deadline_exceeded",
                "the session refresh request deadline was exceeded",
            ),
            true,
        ),
        SessionRefreshServiceOutcome::Unavailable => (
            "unavailable",
            error_field(
                "refresh_service_unavailable",
                "the daemon-owned session refresh service is unavailable",
            ),
            true,
        ),
    }
}

fn render_error(
    args: &Value,
    action: Option<SessionRefreshRequestAction>,
    scope: Option<SessionRefreshScope>,
    code: &str,
    message: &str,
) -> ToolResult {
    let value = json!({
        "tool": TOOL_NAME,
        "action": action.map(SessionRefreshRequestAction::as_str),
        "scope": scope.map(SessionRefreshScope::as_str),
        "outcome": "error",
        "error": {
            "code": code,
            "message": message,
        }
    });
    let mut render_args = args.clone();
    if render_args
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|format| !matches!(format, "markdown" | "json"))
        && let Some(object) = render_args.as_object_mut()
    {
        object.insert("format".to_string(), json!("json"));
    }
    tool_json_with_md(None, &render_args, &value, || refresh_markdown(&value))
        .with_semantic_error(true)
        .with_failure_message(message)
}

fn error_field(code: &str, message: &str) -> Value {
    json!({
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn refresh_markdown(value: &Value) -> String {
    let mut output = String::from("# Session Refresh\n");
    for (label, key) in [
        ("Action", "action"),
        ("Scope", "scope"),
        ("Outcome", "outcome"),
        ("Operation", "operation_id"),
        ("Handle", "handle"),
    ] {
        if let Some(value) = value.get(key).and_then(Value::as_str) {
            let _ = writeln!(output, "- {label}: `{value}`");
        }
    }
    if let Some(progress) = value.get("progress").and_then(Value::as_object) {
        append_refresh_record_markdown(&mut output, progress);
        if let Some(committed_batches) = progress.get("committed_batches").and_then(Value::as_u64) {
            let _ = writeln!(output, "- Committed batches: {committed_batches}");
        }
        if let Some(committed_records) = progress.get("committed_records").and_then(Value::as_u64) {
            let _ = writeln!(output, "- Committed records: {committed_records}");
        }
    }
    if let Some(receipt) = value.get("receipt").and_then(Value::as_object) {
        append_refresh_record_markdown(&mut output, receipt);
        if let Some(state) = receipt.get("state").and_then(Value::as_str) {
            let _ = writeln!(output, "- Receipt state: `{state}`");
        }
        if let Some(failure_code) = receipt.get("failure_code").and_then(Value::as_str) {
            let _ = writeln!(output, "- Failure code: `{failure_code}`");
        }
    }
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        let _ = writeln!(output, "- Error: {message}");
    }
    output
}

fn append_refresh_record_markdown(output: &mut String, record: &serde_json::Map<String, Value>) {
    if let Some(operation_id) = record.get("operation_id").and_then(Value::as_str) {
        let _ = writeln!(output, "- Durable operation: `{operation_id}`");
    }
    if let Some(session_id) = record.get("session_id").and_then(Value::as_str) {
        let _ = writeln!(output, "- Session: `{session_id}`");
    }
    if let Some(frontier) = record.get("frontier").and_then(Value::as_object)
        && let (Some(observed), Some(committed)) = (
            frontier.get("observed_through").and_then(Value::as_u64),
            frontier.get("committed_through").and_then(Value::as_u64),
        )
    {
        let _ = writeln!(output, "- Frontier: {committed}/{observed} committed");
    }
    if let Some(coverage) = record.get("coverage").and_then(Value::as_object) {
        let visible = coverage.get("visible").and_then(Value::as_u64).unwrap_or(0);
        let hidden = coverage.get("hidden").and_then(Value::as_u64).unwrap_or(0);
        let unknown = coverage.get("unknown").and_then(Value::as_u64).unwrap_or(0);
        let redacted = coverage
            .get("redacted")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let _ = writeln!(
            output,
            "- Coverage: visible {visible}, hidden {hidden}, unknown {unknown}, redacted {redacted}"
        );
    }
}

fn outcome_failure_message(outcome: &str) -> &'static str {
    match outcome {
        "busy" => "a conflicting session refresh is already running",
        "failed" => "the durable session refresh failed",
        "denied" => "session refresh authorization denied",
        "wrong_scope" => "session refresh handle scope mismatch",
        "stale" => "session refresh handle stale",
        "not_found" => "session refresh handle not found",
        "aborted" => "session refresh request aborted",
        "deadline_exceeded" => "session refresh request deadline exceeded",
        "unavailable" => "daemon-owned session refresh service unavailable",
        _ => "session refresh failed",
    }
}

pub(crate) fn utc_micros_value(value: UtcMicros) -> i64 {
    value.0
}
