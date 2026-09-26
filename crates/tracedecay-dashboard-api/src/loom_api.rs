//! Authorized Loom temporal projection over the retained project session store.
//!
//! The endpoint composes existing authorities; it does not collect new data.
//! `sessions`/`lcm_raw_messages` provide thread bounds and
//! `sessions.metadata_json` provides provider-native edited-file rollups. Git
//! correlation is read through [`DashboardGitCorrelationReadPortV1`], the
//! daemon-owned typed read over the verified session-git-evidence graph
//! projection; a state composed without that authority reports the git
//! sources unavailable instead of inferring relationships from session rows.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, DashboardWatermarkV1, scope_from_state,
};
use super::util::{JsonQuery, collect_rows};
use super::{DashboardState, RequestControl};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::db::engine::{
    Error as EngineError, IntoParams, QueryExecutor, params,
};
use tracedecay_session_memory::context::{RequestInterruption, run_deadline_signal_interruptible};
use tracedecay_sessions::runtime::git_correlation::{CommitSessionRecord, SessionGitSpan};

const DEFAULT_LIMIT: i64 = 200;
const MAX_LIMIT: i64 = 500;
const GIT_CORRELATION_AUTHORITY: &str = "typed Git correlation graph read port";
const GIT_CORRELATION_REASON: &str = "Git correlation is owned by the registered graph runtime; \
the retained session snapshot cannot query or infer commit, branch, or worktree relationships";
const GIT_CORRELATION_PROJECTION: &str = "verified session-git-evidence graph projection";

/// Verified git-correlation evidence recovered for one dashboard read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DashboardGitCorrelationReadV1 {
    /// The projection has never published a verified head, the typed empty
    /// start of a project without any recorded Git evidence.
    Unpublished,
    /// The recovered verified projection: every recorded span and commit
    /// attribution, bound to the generation that verified them.
    Published {
        generation: String,
        spans: Vec<SessionGitSpan>,
        commits: Vec<CommitSessionRecord>,
    },
}

/// Typed failure of the git-correlation read authority. The route reports it
/// as the source's error state; it is never collapsed into an empty result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardGitCorrelationReadErrorV1 {
    pub detail: String,
}

pub type DashboardGitCorrelationReadFutureV1<'a> = Pin<
    Box<
        dyn Future<
                Output = Result<DashboardGitCorrelationReadV1, DashboardGitCorrelationReadErrorV1>,
            > + Send
            + 'a,
    >,
>;

/// Daemon-owned read over the verified session-git-evidence projection.
/// HTTP adapters receive complete typed rows for the requested sessions and
/// never a graph store handle.
pub trait DashboardGitCorrelationReadPortV1: Send + Sync {
    fn read(&self, session_ids: BTreeSet<String>) -> DashboardGitCorrelationReadFutureV1<'_>;
}

/// The requested page of sessions, newest start first. Only rowids ride the
/// sort; message counts and bounds are then resolved per page row through the
/// `(provider, session_id, …)` indexes, so the work scales with the page and
/// that page's messages, never with the whole history.
const SESSION_PAGE_SQL: &str = "
    WITH page AS (
        SELECT rowid AS page_rowid, started_at
        FROM sessions
        ORDER BY (started_at IS NULL), started_at DESC, rowid DESC
        LIMIT ?1 OFFSET ?2
    )
    SELECT p.page_rowid, s.provider, s.session_id, s.title, s.started_at, s.ended_at,
           s.is_subagent,
           NULLIF(TRIM(s.parent_session_id), '') AS parent_session_id,
           NULLIF(TRIM(s.parent_tool_use_id), '') AS parent_tool_use_id,
           (SELECT COUNT(*) FROM lcm_raw_messages m
             WHERE m.provider = s.provider AND m.session_id = s.session_id) AS messages,
           (SELECT MAX(m.timestamp) FROM lcm_raw_messages m
             WHERE m.provider = s.provider AND m.session_id = s.session_id) AS last_message_at,
           CASE WHEN json_valid(s.metadata_json)
                      AND json_type(s.metadata_json, '$.edited_files') = 'array'
                THEN 1 ELSE 0 END AS edited_files_recorded
    FROM page p
    JOIN sessions s ON s.rowid = p.page_rowid
    ORDER BY (p.started_at IS NULL), p.started_at DESC, p.page_rowid DESC";

/// Per-page reads bind the page's session rowids as one JSON array (`?1`) and
/// join outward from it, so no statement re-sorts or rescans `sessions`.
const PAGE_MODELS_SQL: &str = "
    SELECT s.provider, s.session_id, m.model
    FROM json_each(?1) page
    CROSS JOIN sessions s ON s.rowid = page.value
    CROSS JOIN lcm_raw_messages m ON m.provider = s.provider AND m.session_id = s.session_id
    WHERE m.model IS NOT NULL AND TRIM(m.model) != ''
    GROUP BY s.provider, s.session_id, m.model
    ORDER BY s.provider, s.session_id, m.model";

const PAGE_EDITED_FILES_SQL: &str = "
    SELECT s.provider, s.session_id,
           json_extract(file.value, '$.path') AS path,
           json_extract(file.value, '$.change_type') AS change_type,
           json_extract(file.value, '$.hunks') AS hunks,
           CASE WHEN json_type(file.value, '$.edited_at_micros') = 'integer'
                THEN json_extract(file.value, '$.edited_at_micros') END
               AS edited_at_micros
    FROM json_each(?1) page
    CROSS JOIN sessions s ON s.rowid = page.value
    JOIN json_each(
        CASE WHEN json_valid(s.metadata_json) THEN s.metadata_json ELSE '{}' END,
        '$.edited_files'
    ) AS file
    WHERE json_type(file.value, '$.path') = 'text'
    ORDER BY s.provider, s.session_id, path";

const PAGE_GENERATIONS_SQL: &str = "
    SELECT COUNT(*) AS active_generations, MAX(generation.activated_at) AS latest_activated_at
    FROM json_each(?1) page
    CROSS JOIN sessions s ON s.rowid = page.value
    CROSS JOIN session_temporal_generations generation ON generation.session_id = s.session_id
    WHERE generation.state = 'active'";

const LOOM_REFRESH_OPERATION: &str = "use-case.dashboard.loom.temporal.refresh";

/// Why a Loom temporal read produced no page.
#[derive(Debug)]
enum LoomReadFailureV1 {
    /// The session store could not serve the read now (reader lane refused or
    /// broken, the statement was interrupted or found the store busy); the
    /// same read can succeed on retry.
    StoreUnavailable(String),
    /// The store failed the read or answered with data that breaks this
    /// route's contract; retrying the same read does not help.
    Failed(String),
}

impl fmt::Display for LoomReadFailureV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StoreUnavailable(detail) | Self::Failed(detail) => formatter.write_str(detail),
        }
    }
}

impl From<EngineError> for LoomReadFailureV1 {
    fn from(error: EngineError) -> Self {
        const SQLITE_BUSY: i32 = 5;
        const SQLITE_LOCKED: i32 = 6;
        const SQLITE_INTERRUPT: i32 = 9;
        let transient = match &error {
            EngineError::Busy | EngineError::Runtime(_) => true,
            EngineError::Sqlite { code, .. } => {
                matches!(code, Some(SQLITE_BUSY | SQLITE_LOCKED | SQLITE_INTERRUPT))
            }
            _ => false,
        };
        if transient {
            Self::StoreUnavailable(error.to_string())
        } else {
            Self::Failed(error.to_string())
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LoomTemporalParamsV1 {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
struct LoomSourceCoverageV1 {
    completeness: &'static str,
    eligible: Option<u64>,
    examined: Option<u64>,
    matched: Option<u64>,
    omitted: Option<u64>,
    unit: Option<&'static str>,
    reason: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
struct LoomSourceStatusV1 {
    id: &'static str,
    label: &'static str,
    state: DashboardDomainStateV1,
    authority: Option<&'static str>,
    granularity: &'static str,
    providers: Vec<String>,
    item_count: Option<u64>,
    reason: Option<String>,
    required_authority: Option<&'static str>,
    coverage: LoomSourceCoverageV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
struct LoomTemporalRefreshV1 {
    state: DashboardDomainStateV1,
    active_generations: u64,
    latest_activated_at_micros: Option<i64>,
    authority: &'static str,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LoomSessionModelV1 {
    model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LoomSessionRowV1 {
    provider: String,
    session_id: String,
    title: Option<String>,
    started_at: Option<i64>,
    ended_at: Option<i64>,
    last_message_at: Option<i64>,
    is_subagent: bool,
    messages: i64,
    edited_files_recorded: bool,
    models: Vec<LoomSessionModelV1>,
    /// The delegating session as the provider recorded it on `sessions`, the
    /// same column the analytics subagent tree reads. Absent means no parent
    /// was recorded; a branch is never inferred from time overlap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_session_id: Option<String>,
    /// The tool invocation that spawned this session, when the provider
    /// recorded one; it binds a fork to a specific call, not just a parent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_tool_use_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LoomCommitV1 {
    provider: String,
    session_id: String,
    commit_sha: String,
    committed_at: i64,
    branch: Option<String>,
    worktree: Option<String>,
    relation: String,
    evidence: String,
    confidence: f64,
    span_overlap_kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LoomEditedFileV1 {
    provider: String,
    session_id: String,
    path: String,
    change_type: Option<String>,
    hunks: Option<i64>,
    /// Unix microseconds of the edit when the provider rollup recorded
    /// `edited_at_micros` as an integer. Absent is unrecorded; it is never
    /// derived from the session bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    edited_at_micros: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
struct LoomBranchSpanV1 {
    provider: String,
    session_id: String,
    branch: Option<String>,
    worktree: String,
    first_at: i64,
    last_at: i64,
    event_count: i64,
    source: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub(super) struct LoomTemporalPayloadV1 {
    available: bool,
    total: u64,
    sessions: Vec<LoomSessionRowV1>,
    source_statuses: Vec<LoomSourceStatusV1>,
    commits: Vec<LoomCommitV1>,
    edited_files: Vec<LoomEditedFileV1>,
    branch_spans: Vec<LoomBranchSpanV1>,
    temporal_refresh: LoomTemporalRefreshV1,
}

struct LoomReadV1 {
    payload: LoomTemporalPayloadV1,
    examined_sessions: u64,
    latest_activated_at: Option<i64>,
}

fn decode_rows<T: DeserializeOwned>(
    rows: Vec<Value>,
    label: &str,
) -> Result<Vec<T>, LoomReadFailureV1> {
    serde_json::from_value(Value::Array(rows)).map_err(|error| {
        LoomReadFailureV1::Failed(format!(
            "{label} did not match its response contract: {error}"
        ))
    })
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct LoomFileSessionProjectionV1 {
    pub granularity: &'static str,
    pub authority: &'static str,
    pub providers: Vec<String>,
    pub eligible_sessions: u64,
    pub matched_sessions: u64,
    pub sessions: Vec<Value>,
}

pub async fn temporal(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    JsonQuery(params): JsonQuery<LoomTemporalParamsV1>,
) -> Response {
    hotpath::future!(
        async move {
            let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
            let offset = params.offset.unwrap_or(0).max(0);
            let Some(database) = state.lcm_db.as_deref() else {
                let payload =
                    unavailable_payload("the resolved project session authority is unavailable");
                return Json(DashboardEnvelopeV1::new(
                    scope_from_state(&state),
                    DashboardDomainStateV1::Unknown,
                    DashboardCoverageV1::unknown(),
                    DashboardFreshnessV1::unknown(),
                    payload,
                ))
                .into_response();
            };

            let read = run_deadline_signal_interruptible(
                &control.deadline(),
                control.cancellation(),
                read_temporal(
                    database,
                    state.git_correlation_read_authority.as_deref(),
                    limit,
                    offset,
                ),
            )
            .await;
            let read = match read {
                Ok(Ok(read)) => read,
                Ok(Err(failure)) => return read_failure_response(&state, failure),
                Err(interruption) => return interrupted_response(&state, interruption),
            };
            let total = read.payload.total;
            let examined = read.examined_sessions;
            let coverage = if offset == 0 && examined == total {
                DashboardCoverageV1::complete(total, "sessions")
            } else {
                DashboardCoverageV1::partial(
                    total,
                    examined,
                    "sessions",
                    vec!["the requested session page does not cover the full store".to_string()],
                )
            };
            let mut envelope = DashboardEnvelopeV1::new(
                scope_from_state(&state),
                DashboardDomainStateV1::Partial,
                coverage,
                DashboardFreshnessV1::fresh_now(),
                read.payload,
            );
            if let Some(activated_at) = read.latest_activated_at {
                envelope = envelope.with_source_watermark(DashboardWatermarkV1 {
                    source: "session_temporal_generations".to_string(),
                    watermark: format!("active-through-micros-{activated_at}"),
                });
            }
            Json(envelope).into_response()
        },
        label = "dashboard_api.loom.temporal"
    )
    .await
}

/// A read the admitted request deadline or cancellation ended. The page is
/// withheld, never truncated into a partial success; retrying is the legal
/// action.
fn interrupted_response(state: &DashboardState, interruption: RequestInterruption) -> Response {
    let (status, domain_state, code, detail) = match interruption {
        RequestInterruption::DeadlineExceeded => (
            StatusCode::GATEWAY_TIMEOUT,
            DashboardDomainStateV1::TimedOut,
            "loom_temporal_read_timed_out",
            "the Loom temporal read did not finish within its admitted request deadline",
        ),
        RequestInterruption::Cancelled => (
            StatusCode::REQUEST_TIMEOUT,
            DashboardDomainStateV1::Cancelled,
            "loom_temporal_read_cancelled",
            "the Loom temporal request was cancelled before the read finished",
        ),
    };
    let mut coverage = DashboardCoverageV1::unknown();
    coverage.omission_reasons = vec![code.to_owned(), detail.to_owned()];
    problem_response(
        status,
        DashboardEnvelopeV1::new(
            scope_from_state(state),
            domain_state,
            coverage,
            DashboardFreshnessV1::unknown(),
            None,
        )
        .with_legal_actions(vec![refresh_action()]),
    )
}

fn read_failure_response(state: &DashboardState, failure: LoomReadFailureV1) -> Response {
    let scope = scope_from_state(state);
    match failure {
        LoomReadFailureV1::StoreUnavailable(detail) => {
            let mut envelope =
                DashboardEnvelopeV1::unavailable(scope, None, "loom_session_store_unavailable")
                    .with_legal_actions(vec![refresh_action()]);
            envelope.coverage.omission_reasons.push(detail);
            problem_response(StatusCode::SERVICE_UNAVAILABLE, envelope)
        }
        LoomReadFailureV1::Failed(detail) => {
            let mut envelope = DashboardEnvelopeV1::error(scope, None, "loom_temporal_read_failed");
            envelope.coverage.omission_reasons.push(detail);
            problem_response(StatusCode::INTERNAL_SERVER_ERROR, envelope)
        }
    }
}

fn problem_response(
    status: StatusCode,
    envelope: DashboardEnvelopeV1<Option<LoomTemporalPayloadV1>>,
) -> Response {
    (status, Json(envelope)).into_response()
}

fn refresh_action() -> DashboardLegalActionRefV1 {
    DashboardLegalActionRefV1::new(DashboardLegalActionKindV1::Refresh, LOOM_REFRESH_OPERATION)
}

/// One resolved git-correlation read for this request: the composed
/// authority's outcome, or the typed absent/failed states.
enum GitCorrelationSourceReadV1 {
    Absent,
    Failed(String),
    Read(DashboardGitCorrelationReadV1),
}

async fn read_temporal(
    database: &RegisteredGlobalDb,
    git_authority: Option<&dyn DashboardGitCorrelationReadPortV1>,
    limit: i64,
    offset: i64,
) -> Result<LoomReadV1, LoomReadFailureV1> {
    let snapshot = database.read_snapshot().await.map_err(|error| {
        LoomReadFailureV1::StoreUnavailable(format!("open Loom session snapshot: {error}"))
    })?;
    let conn = &snapshot;
    let total = query_count(conn, "SELECT COUNT(*) AS total FROM sessions", (), "total").await?;
    let mut sessions = query_rows(conn, SESSION_PAGE_SQL, params![limit, offset]).await?;
    let examined_sessions = sessions.len() as u64;

    let mut page_rowids = Vec::with_capacity(sessions.len());
    let mut page_keys: BTreeSet<(String, String)> = BTreeSet::new();
    let mut edited_examined = 0_u64;
    for session in &sessions {
        page_rowids.push(required_i64(session, "page_rowid")?);
        page_keys.insert((
            required_str(session, "provider")?.to_string(),
            required_str(session, "session_id")?.to_string(),
        ));
        if required_i64(session, "edited_files_recorded")? != 0 {
            edited_examined += 1;
        }
    }
    let page = Value::from(page_rowids).to_string();

    let model_rows = query_rows(conn, PAGE_MODELS_SQL, params![page.as_str()]).await?;
    let mut models: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    for row in model_rows {
        let provider = required_str(&row, "provider")?.to_string();
        let session_id = required_str(&row, "session_id")?.to_string();
        models
            .entry((provider, session_id))
            .or_default()
            .push(json!({ "model": row.get("model").cloned().unwrap_or(Value::Null) }));
    }
    for session in &mut sessions {
        let provider = required_str(session, "provider")?.to_string();
        let session_id = required_str(session, "session_id")?.to_string();
        let is_subagent = required_i64(session, "is_subagent")? != 0;
        let edited_files_recorded = required_i64(session, "edited_files_recorded")? != 0;
        if let Some(object) = session.as_object_mut() {
            object.insert("is_subagent".to_string(), json!(is_subagent));
            object.insert(
                "edited_files_recorded".to_string(),
                json!(edited_files_recorded),
            );
            object.insert(
                "models".to_string(),
                Value::Array(models.remove(&(provider, session_id)).unwrap_or_default()),
            );
        }
    }

    let edited_files = query_rows(conn, PAGE_EDITED_FILES_SQL, params![page.as_str()]).await?;
    let generation_rows = query_rows(conn, PAGE_GENERATIONS_SQL, params![page.as_str()]).await?;
    let generation = generation_rows
        .first()
        .ok_or_else(|| LoomReadFailureV1::Failed("generation count returned no row".to_owned()))?;
    let active_generations = required_u64(generation, "active_generations")?;
    let latest_activated_at = generation
        .get("latest_activated_at")
        .and_then(Value::as_i64);
    drop(snapshot);

    let git_correlation = match git_authority {
        None => GitCorrelationSourceReadV1::Absent,
        Some(authority) => {
            let session_ids = page_keys.iter().map(|(_, id)| id.clone()).collect();
            match authority.read(session_ids).await {
                Ok(read) => GitCorrelationSourceReadV1::Read(read),
                Err(error) => GitCorrelationSourceReadV1::Failed(error.detail),
            }
        }
    };
    let git = resolve_git_sources(git_correlation, &page_keys, examined_sessions)
        .map_err(LoomReadFailureV1::Failed)?;

    let statuses = vec![
        git.session_commit,
        source_status(SourceStatusInput {
            id: "session_file",
            label: "Session → edited file",
            state: DashboardDomainStateV1::Partial,
            authority: Some("sessions.metadata_json $.edited_files[]"),
            granularity: "recorded file rollup",
            rows: &edited_files,
            reason: Some(
                "edited-file coverage is provider-native metadata; sessions without an \
                 edited_files array are omitted, never treated as no edits; \
                 edited_at_micros is served only where the rollup recorded it, \
                 files without it have no known edit time"
                    .to_string(),
            ),
            required_authority: None,
            coverage: LoomSourceCoverageV1 {
                completeness: "partial",
                eligible: Some(examined_sessions),
                examined: Some(edited_examined),
                matched: Some(matched_sessions(&edited_files)),
                omitted: Some(examined_sessions.saturating_sub(edited_examined)),
                unit: Some("displayed sessions"),
                reason: "only sessions carrying a recorded edited_files array are examined"
                    .to_string(),
            },
        }),
        git.branch_worktree,
    ];

    let refresh_state = if examined_sessions == 0 || active_generations == examined_sessions {
        DashboardDomainStateV1::Ready
    } else {
        DashboardDomainStateV1::Partial
    };
    Ok(LoomReadV1 {
        payload: LoomTemporalPayloadV1 {
            available: true,
            total,
            sessions: decode_rows(sessions, "Loom sessions")?,
            source_statuses: statuses,
            commits: git.commits,
            edited_files: decode_rows(edited_files, "Loom edited files")?,
            branch_spans: git.branch_spans,
            temporal_refresh: LoomTemporalRefreshV1 {
                state: refresh_state,
                active_generations,
                latest_activated_at_micros: latest_activated_at,
                authority: "session_temporal_generations maintained by the temporal refresh scheduler",
            },
        },
        examined_sessions,
        latest_activated_at,
    })
}

struct LoomGitSourcesV1 {
    session_commit: LoomSourceStatusV1,
    branch_worktree: LoomSourceStatusV1,
    commits: Vec<LoomCommitV1>,
    branch_spans: Vec<LoomBranchSpanV1>,
}

fn resolve_git_sources(
    read: GitCorrelationSourceReadV1,
    page_keys: &BTreeSet<(String, String)>,
    examined_sessions: u64,
) -> Result<LoomGitSourcesV1, String> {
    match read {
        GitCorrelationSourceReadV1::Absent => Ok(LoomGitSourcesV1 {
            session_commit: unavailable_git_status(
                "session_commit",
                "Session ↔ commit",
                "commit attribution",
                DashboardDomainStateV1::Unknown,
                None,
                GIT_CORRELATION_REASON.to_string(),
                Some(GIT_CORRELATION_AUTHORITY),
            ),
            branch_worktree: unavailable_git_status(
                "branch_worktree",
                "Branch & worktree spans",
                "coalesced activity span",
                DashboardDomainStateV1::Unknown,
                None,
                GIT_CORRELATION_REASON.to_string(),
                Some(GIT_CORRELATION_AUTHORITY),
            ),
            commits: Vec::new(),
            branch_spans: Vec::new(),
        }),
        GitCorrelationSourceReadV1::Failed(detail) => {
            let reason = format!("the verified Git evidence read failed: {detail}");
            Ok(LoomGitSourcesV1 {
                session_commit: unavailable_git_status(
                    "session_commit",
                    "Session ↔ commit",
                    "commit attribution",
                    DashboardDomainStateV1::Error,
                    Some(GIT_CORRELATION_PROJECTION),
                    reason.clone(),
                    None,
                ),
                branch_worktree: unavailable_git_status(
                    "branch_worktree",
                    "Branch & worktree spans",
                    "coalesced activity span",
                    DashboardDomainStateV1::Error,
                    Some(GIT_CORRELATION_PROJECTION),
                    reason,
                    None,
                ),
                commits: Vec::new(),
                branch_spans: Vec::new(),
            })
        }
        GitCorrelationSourceReadV1::Read(DashboardGitCorrelationReadV1::Unpublished) => {
            let reason = "the verified Git evidence projection has never published a head; \
                          no recorded span or commit correlates yet"
                .to_string();
            Ok(LoomGitSourcesV1 {
                session_commit: ready_git_status(
                    "session_commit",
                    "Session ↔ commit",
                    "commit attribution",
                    Vec::new(),
                    0,
                    0,
                    examined_sessions,
                    reason.clone(),
                ),
                branch_worktree: ready_git_status(
                    "branch_worktree",
                    "Branch & worktree spans",
                    "coalesced activity span",
                    Vec::new(),
                    0,
                    0,
                    examined_sessions,
                    reason,
                ),
                commits: Vec::new(),
                branch_spans: Vec::new(),
            })
        }
        GitCorrelationSourceReadV1::Read(DashboardGitCorrelationReadV1::Published {
            generation,
            spans,
            commits,
        }) => {
            let page_spans: Vec<&SessionGitSpan> = spans
                .iter()
                .filter(|span| {
                    page_keys.contains(&(span.provider.clone(), span.session_id.clone()))
                })
                .collect();
            let page_commits: Vec<&CommitSessionRecord> = commits
                .iter()
                .filter(|record| {
                    page_keys.contains(&(record.provider.clone(), record.session_id.clone()))
                })
                .collect();
            let branch_spans = page_spans
                .iter()
                .map(|span| loom_branch_span(span))
                .collect::<Result<Vec<_>, _>>()?;
            let commit_rows = page_commits
                .iter()
                .map(|record| loom_commit(record))
                .collect::<Result<Vec<_>, _>>()?;
            let reason =
                format!("recovered from the verified Git evidence generation {generation}");
            let span_providers = distinct_strings(page_spans.iter().map(|span| &span.provider));
            let commit_providers =
                distinct_strings(page_commits.iter().map(|record| &record.provider));
            let span_matched = page_spans
                .iter()
                .map(|span| (&span.provider, &span.session_id))
                .collect::<BTreeSet<_>>()
                .len() as u64;
            let commit_matched = page_commits
                .iter()
                .map(|record| (&record.provider, &record.session_id))
                .collect::<BTreeSet<_>>()
                .len() as u64;
            Ok(LoomGitSourcesV1 {
                session_commit: ready_git_status(
                    "session_commit",
                    "Session ↔ commit",
                    "commit attribution",
                    commit_providers,
                    commit_matched,
                    commit_rows.len() as u64,
                    examined_sessions,
                    reason.clone(),
                ),
                branch_worktree: ready_git_status(
                    "branch_worktree",
                    "Branch & worktree spans",
                    "coalesced activity span",
                    span_providers,
                    span_matched,
                    branch_spans.len() as u64,
                    examined_sessions,
                    reason,
                ),
                commits: commit_rows,
                branch_spans,
            })
        }
    }
}

fn unavailable_git_status(
    id: &'static str,
    label: &'static str,
    granularity: &'static str,
    state: DashboardDomainStateV1,
    authority: Option<&'static str>,
    reason: String,
    required_authority: Option<&'static str>,
) -> LoomSourceStatusV1 {
    LoomSourceStatusV1 {
        id,
        label,
        state,
        authority,
        granularity,
        providers: Vec::new(),
        item_count: None,
        reason: Some(reason.clone()),
        required_authority,
        coverage: LoomSourceCoverageV1 {
            completeness: "unknown",
            eligible: None,
            examined: None,
            matched: None,
            omitted: None,
            unit: None,
            reason,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn ready_git_status(
    id: &'static str,
    label: &'static str,
    granularity: &'static str,
    providers: Vec<String>,
    matched_sessions: u64,
    item_count: u64,
    examined_sessions: u64,
    reason: String,
) -> LoomSourceStatusV1 {
    LoomSourceStatusV1 {
        id,
        label,
        state: DashboardDomainStateV1::Ready,
        authority: Some(GIT_CORRELATION_PROJECTION),
        granularity,
        providers,
        item_count: Some(item_count),
        reason: Some(reason.clone()),
        required_authority: None,
        coverage: LoomSourceCoverageV1 {
            completeness: "complete",
            eligible: Some(examined_sessions),
            examined: Some(examined_sessions),
            matched: Some(matched_sessions),
            omitted: Some(0),
            unit: Some("displayed sessions"),
            reason,
        },
    }
}

fn distinct_strings<'a>(values: impl Iterator<Item = &'a String>) -> Vec<String> {
    values
        .filter(|value| !value.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn loom_branch_span(span: &SessionGitSpan) -> Result<LoomBranchSpanV1, String> {
    Ok(LoomBranchSpanV1 {
        provider: span.provider.clone(),
        session_id: span.session_id.clone(),
        branch: span.branch.clone(),
        worktree: span.worktree.clone(),
        first_at: span.first_ts,
        last_at: span.last_ts,
        event_count: span.event_count,
        source: serde_token(&span.source, "Git span source")?,
    })
}

fn loom_commit(record: &CommitSessionRecord) -> Result<LoomCommitV1, String> {
    Ok(LoomCommitV1 {
        provider: record.provider.clone(),
        session_id: record.session_id.clone(),
        commit_sha: record.commit_sha.clone(),
        committed_at: record.committed_at,
        branch: record.branch.clone(),
        worktree: record.worktree.clone(),
        relation: serde_token(&record.relation, "Git commit relation")?,
        evidence: serde_token(&record.evidence, "Git commit evidence")?,
        confidence: record.confidence as f64,
        span_overlap_kind: Some(serde_token(
            &record.span_overlap_kind,
            "Git commit span overlap",
        )?),
    })
}

/// Canonical snake_case token of a unit enum, taken from its own serde
/// contract instead of a hand-written duplicate mapping.
fn serde_token<T: Serialize>(value: &T, label: &str) -> Result<String, String> {
    match serde_json::to_value(value) {
        Ok(Value::String(token)) => Ok(token),
        Ok(other) => Err(format!(
            "{label} did not serialize to its canonical token: {other}"
        )),
        Err(error) => Err(format!("{label} failed to serialize: {error}")),
    }
}

pub async fn sessions_for_edited_file(
    conn: &(impl QueryExecutor + ?Sized),
    file_path: &str,
) -> Result<LoomFileSessionProjectionV1, String> {
    let eligible_sessions = query_count(
        conn,
        "SELECT COUNT(*) AS eligible
         FROM sessions
         WHERE json_valid(metadata_json)
           AND json_type(metadata_json, '$.edited_files') = 'array'",
        (),
        "eligible",
    )
    .await
    .map_err(|failure| failure.to_string())?;
    let sessions = query_rows(
        conn,
        "SELECT DISTINCT s.provider, s.session_id, s.title, s.started_at, s.ended_at
         FROM sessions AS s
         JOIN json_each(s.metadata_json, '$.edited_files') AS edited
         WHERE json_valid(s.metadata_json)
           AND json_type(s.metadata_json, '$.edited_files') = 'array'
           AND edited.type = 'text'
           AND edited.value = ?1
         ORDER BY (s.started_at IS NULL), s.started_at DESC, s.rowid DESC",
        params![file_path],
    )
    .await
    .map_err(|failure| failure.to_string())?;
    Ok(LoomFileSessionProjectionV1 {
        granularity: "file",
        authority: "sessions.metadata_json $.edited_files[]",
        providers: providers(&sessions),
        eligible_sessions,
        matched_sessions: matched_sessions(&sessions),
        sessions,
    })
}

struct SourceStatusInput<'a> {
    id: &'static str,
    label: &'static str,
    state: DashboardDomainStateV1,
    authority: Option<&'static str>,
    granularity: &'static str,
    rows: &'a [Value],
    reason: Option<String>,
    required_authority: Option<&'static str>,
    coverage: LoomSourceCoverageV1,
}

fn source_status(input: SourceStatusInput<'_>) -> LoomSourceStatusV1 {
    LoomSourceStatusV1 {
        id: input.id,
        label: input.label,
        state: input.state,
        authority: input.authority,
        granularity: input.granularity,
        providers: providers(input.rows),
        item_count: Some(input.rows.len() as u64),
        reason: input.reason,
        required_authority: input.required_authority,
        coverage: input.coverage,
    }
}

fn providers(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.get("provider").and_then(Value::as_str))
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn matched_sessions(rows: &[Value]) -> u64 {
    rows.iter()
        .filter_map(|row| row.get("session_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .len() as u64
}

async fn query_rows(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
) -> Result<Vec<Value>, LoomReadFailureV1> {
    hotpath::future!(
        async move {
            let rows = conn.query(sql, params).await?;
            Ok(collect_rows(rows).await?)
        },
        label = "dashboard_api.loom.query_rows"
    )
    .await
}

async fn query_count(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
    field: &str,
) -> Result<u64, LoomReadFailureV1> {
    let rows = query_rows(conn, sql, params).await?;
    let row = rows.first().ok_or_else(|| {
        LoomReadFailureV1::Failed(format!("count query returned no row for {field}"))
    })?;
    required_u64(row, field)
}

fn required_u64(row: &Value, field: &str) -> Result<u64, LoomReadFailureV1> {
    let value = required_i64(row, field)?;
    u64::try_from(value)
        .map_err(|_| LoomReadFailureV1::Failed(format!("{field} was negative: {value}")))
}

fn required_i64(row: &Value, field: &str) -> Result<i64, LoomReadFailureV1> {
    row.get(field).and_then(Value::as_i64).ok_or_else(|| {
        LoomReadFailureV1::Failed(format!(
            "required integer field {field} was absent or invalid"
        ))
    })
}

fn required_str<'a>(row: &'a Value, field: &str) -> Result<&'a str, LoomReadFailureV1> {
    row.get(field).and_then(Value::as_str).ok_or_else(|| {
        LoomReadFailureV1::Failed(format!(
            "required string field {field} was absent or invalid"
        ))
    })
}

fn unavailable_payload(reason: &str) -> LoomTemporalPayloadV1 {
    let unavailable = |id, label, authority, granularity| LoomSourceStatusV1 {
        id,
        label,
        state: DashboardDomainStateV1::Unknown,
        authority: Some(authority),
        granularity,
        providers: Vec::new(),
        item_count: None,
        reason: Some(reason.to_string()),
        required_authority: None,
        coverage: LoomSourceCoverageV1 {
            completeness: "unknown",
            eligible: None,
            examined: None,
            matched: None,
            omitted: None,
            unit: None,
            reason: reason.to_string(),
        },
    };
    let unavailable_required = |id, label, required_authority, granularity| LoomSourceStatusV1 {
        id,
        label,
        state: DashboardDomainStateV1::Unknown,
        authority: None,
        granularity,
        providers: Vec::new(),
        item_count: None,
        reason: Some(reason.to_string()),
        required_authority: Some(required_authority),
        coverage: LoomSourceCoverageV1 {
            completeness: "unknown",
            eligible: None,
            examined: None,
            matched: None,
            omitted: None,
            unit: None,
            reason: reason.to_string(),
        },
    };
    LoomTemporalPayloadV1 {
        available: false,
        total: 0,
        sessions: Vec::new(),
        source_statuses: vec![
            unavailable_required(
                "session_commit",
                "Session ↔ commit",
                GIT_CORRELATION_AUTHORITY,
                "commit attribution",
            ),
            unavailable(
                "session_file",
                "Session → edited file",
                "sessions.metadata_json $.edited_files[]",
                "recorded file rollup",
            ),
            unavailable_required(
                "branch_worktree",
                "Branch & worktree spans",
                GIT_CORRELATION_AUTHORITY,
                "coalesced activity span",
            ),
        ],
        commits: Vec::new(),
        edited_files: Vec::new(),
        branch_spans: Vec::new(),
        temporal_refresh: LoomTemporalRefreshV1 {
            state: DashboardDomainStateV1::Unknown,
            active_generations: 0,
            latest_activated_at_micros: None,
            authority: "session_temporal_generations maintained by the temporal refresh scheduler",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(keys: &[(&str, &str)]) -> BTreeSet<(String, String)> {
        keys.iter()
            .map(|(provider, session_id)| ((*provider).to_owned(), (*session_id).to_owned()))
            .collect()
    }

    fn span(provider: &str, session_id: &str) -> SessionGitSpan {
        SessionGitSpan {
            span_id: format!("transcript:{provider}:{session_id}"),
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            thread_id: None,
            branch: Some("main".to_owned()),
            worktree: "/work/tree".to_owned(),
            first_ts: 1_700_001_000,
            last_ts: 1_700_001_020,
            event_count: 2,
            source: tracedecay_sessions::runtime::git_correlation::SpanSource::Ingest,
        }
    }

    #[test]
    fn absent_git_authority_stays_the_typed_unavailable_state() {
        let sources = resolve_git_sources(
            GitCorrelationSourceReadV1::Absent,
            &page(&[("cursor", "sess-1")]),
            1,
        )
        .expect("absent sources");

        for status in [&sources.session_commit, &sources.branch_worktree] {
            assert_eq!(status.state, DashboardDomainStateV1::Unknown);
            assert_eq!(status.required_authority, Some(GIT_CORRELATION_AUTHORITY));
            assert_eq!(status.item_count, None, "absence must not claim a count");
        }
        assert!(sources.commits.is_empty());
        assert!(sources.branch_spans.is_empty());
    }

    #[test]
    fn failed_git_read_is_an_error_state_never_an_empty_result() {
        let sources = resolve_git_sources(
            GitCorrelationSourceReadV1::Failed("graph runtime is not mounted".to_owned()),
            &page(&[("cursor", "sess-1")]),
            1,
        )
        .expect("failed sources");

        for status in [&sources.session_commit, &sources.branch_worktree] {
            assert_eq!(status.state, DashboardDomainStateV1::Error);
            assert_eq!(status.item_count, None);
            assert!(
                status
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("graph runtime is not mounted")),
                "the failure detail must be preserved: {status:?}"
            );
        }
    }

    #[test]
    fn unpublished_projection_is_a_ready_typed_empty_start() {
        let sources = resolve_git_sources(
            GitCorrelationSourceReadV1::Read(DashboardGitCorrelationReadV1::Unpublished),
            &page(&[("cursor", "sess-1")]),
            1,
        )
        .expect("unpublished sources");

        for status in [&sources.session_commit, &sources.branch_worktree] {
            assert_eq!(status.state, DashboardDomainStateV1::Ready);
            assert_eq!(status.item_count, Some(0));
            assert_eq!(status.coverage.completeness, "complete");
        }
        assert!(sources.commits.is_empty());
        assert!(sources.branch_spans.is_empty());
    }

    #[test]
    fn published_projection_serves_page_rows_and_conceals_foreign_sessions() {
        let sources = resolve_git_sources(
            GitCorrelationSourceReadV1::Read(DashboardGitCorrelationReadV1::Published {
                generation: "session-git-evidence:test".to_owned(),
                spans: vec![span("cursor", "sess-1"), span("cursor", "sess-foreign")],
                commits: Vec::new(),
            }),
            &page(&[("cursor", "sess-1")]),
            1,
        )
        .expect("published sources");

        assert_eq!(sources.branch_worktree.state, DashboardDomainStateV1::Ready);
        assert_eq!(sources.branch_worktree.item_count, Some(1));
        assert_eq!(sources.session_commit.state, DashboardDomainStateV1::Ready);
        assert_eq!(sources.session_commit.item_count, Some(0));
        assert_eq!(sources.branch_spans.len(), 1);
        let span = &sources.branch_spans[0];
        assert_eq!(span.session_id, "sess-1");
        assert_eq!(span.branch.as_deref(), Some("main"));
        assert_eq!(span.first_at, 1_700_001_000);
        assert_eq!(span.last_at, 1_700_001_020);
        assert_eq!(span.source, "ingest");
        assert!(
            sources
                .branch_worktree
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("session-git-evidence:test")),
            "the serving generation must be named: {:?}",
            sources.branch_worktree.reason
        );
    }
}
