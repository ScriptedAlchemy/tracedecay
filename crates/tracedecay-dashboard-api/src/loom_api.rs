//! Authorized Loom temporal projection over the retained project session store.
//!
//! The endpoint composes existing authorities; it does not collect new data:
//! `sessions`/`session_messages` provide thread bounds, `commit_sessions`
//! provides persisted commit attribution, `session_git_spans` provides
//! coalesced branch/worktree activity, and `sessions.metadata_json` provides
//! provider-native edited-file rollups. PR/review/CI/release rows belong to the
//! shared Delivery projection and remain explicitly unsupported here until that
//! route exposes session-linked rows.

use std::collections::{BTreeMap, BTreeSet};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardWatermarkV1, scope_from_state,
};
use super::util::{JsonQuery, query_rows};
use tracedecay_runtime_core::db::engine::{IntoParams, QueryExecutor, params};

const DEFAULT_LIMIT: i64 = 200;
const MAX_LIMIT: i64 = 500;
const DELIVERY_AUTHORITY: &str = "GET /api/delivery/overview with session-linked pull_requests, \
review_comments, ci_checks, failure_localization, and releases rows";
const DELIVERY_REASON: &str = "the shared Delivery overview is mounted, but its outcome \
projections are unavailable or unsupported and do not expose session-linked rows; Loom does not \
duplicate them";

const PAGE_CTE: &str = "
    WITH page AS (
        SELECT provider, session_id
        FROM sessions
        ORDER BY (started_at IS NULL), started_at DESC, rowid DESC
        LIMIT ?1 OFFSET ?2
    )";

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

fn decode_rows<T: DeserializeOwned>(rows: Vec<Value>, label: &str) -> Result<Vec<T>, String> {
    serde_json::from_value(Value::Array(rows))
        .map_err(|error| format!("{label} did not match its response contract: {error}"))
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
    JsonQuery(params): JsonQuery<LoomTemporalParamsV1>,
) -> Response {
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0).max(0);
    let Some(database) = state.lcm_db.as_deref() else {
        let payload = unavailable_payload("the resolved project session authority is unavailable");
        return Json(DashboardEnvelopeV1::new(
            scope_from_state(&state),
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            payload,
        ))
        .into_response();
    };

    let snapshot = match database.read_snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => return query_error(format!("open Loom session snapshot: {error}")),
    };
    let read = match read_temporal(&snapshot, limit, offset).await {
        Ok(read) => read,
        Err(error) => return query_error(error),
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
}

async fn read_temporal(
    conn: &(impl QueryExecutor + ?Sized),
    limit: i64,
    offset: i64,
) -> Result<LoomReadV1, String> {
    let total = query_count(conn, "SELECT COUNT(*) AS total FROM sessions", (), "total").await?;
    let session_sql = "
        SELECT s.provider, s.session_id, s.title, s.started_at, s.ended_at,
               s.is_subagent, COUNT(m.message_id) AS messages,
               MAX(m.timestamp) AS last_message_at,
               CASE WHEN json_valid(s.metadata_json)
                          AND json_type(s.metadata_json, '$.edited_files') = 'array'
                    THEN 1 ELSE 0 END AS edited_files_recorded
        FROM sessions s
        LEFT JOIN session_messages m
          ON m.provider = s.provider AND m.session_id = s.session_id
        GROUP BY s.provider, s.session_id
        ORDER BY (s.started_at IS NULL), s.started_at DESC, s.rowid DESC
        LIMIT ?1 OFFSET ?2";
    let mut sessions = query_rows(conn, session_sql, params![limit, offset]).await?;
    let examined_sessions = sessions.len() as u64;

    let model_sql = format!(
        "{PAGE_CTE}
         SELECT m.provider, m.session_id, m.model
         FROM session_messages m
         JOIN page p ON p.provider = m.provider AND p.session_id = m.session_id
         WHERE m.model IS NOT NULL AND TRIM(m.model) != ''
         GROUP BY m.provider, m.session_id, m.model
         ORDER BY m.provider, m.session_id, m.model"
    );
    let model_rows = query_rows(conn, &model_sql, params![limit, offset]).await?;
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

    let commit_sql = format!(
        "{PAGE_CTE}
         SELECT c.provider,
                c.session_id, c.commit_sha, c.committed_at, c.branch, c.worktree,
                c.relation, c.evidence, c.confidence, c.span_overlap_kind
         FROM commit_sessions c
         JOIN page p ON p.session_id = c.session_id AND p.provider = c.provider
         ORDER BY c.committed_at, c.commit_sha"
    );
    let commits = query_rows(conn, &commit_sql, params![limit, offset]).await?;
    let commit_eligible_sql = format!(
        "{PAGE_CTE}
         SELECT COUNT(*) AS eligible
         FROM commit_sessions c
         WHERE EXISTS (
             SELECT 1 FROM page p WHERE p.session_id = c.session_id
         )"
    );
    let commit_eligible = query_count(
        conn,
        &commit_eligible_sql,
        params![limit, offset],
        "eligible",
    )
    .await?;

    let edited_file_sql = format!(
        "{PAGE_CTE}
         SELECT p.provider, p.session_id,
                json_extract(file.value, '$.path') AS path,
                json_extract(file.value, '$.change_type') AS change_type,
                json_extract(file.value, '$.hunks') AS hunks
         FROM page p
         JOIN sessions s ON s.provider = p.provider AND s.session_id = p.session_id
         JOIN json_each(
             CASE WHEN json_valid(s.metadata_json) THEN s.metadata_json ELSE '{{}}' END,
             '$.edited_files'
         ) AS file
         WHERE json_type(file.value, '$.path') = 'text'
         ORDER BY p.provider, p.session_id, path"
    );
    let edited_files = query_rows(conn, &edited_file_sql, params![limit, offset]).await?;
    let edited_examined_sql = format!(
        "{PAGE_CTE}
         SELECT COUNT(*) AS examined
         FROM page p
         JOIN sessions s ON s.provider = p.provider AND s.session_id = p.session_id
         WHERE json_valid(s.metadata_json)
           AND json_type(s.metadata_json, '$.edited_files') = 'array'"
    );
    let edited_examined = query_count(
        conn,
        &edited_examined_sql,
        params![limit, offset],
        "examined",
    )
    .await?;

    let branch_sql = format!(
        "{PAGE_CTE}
         SELECT span.provider,
                span.session_id, span.branch, span.worktree,
                span.first_ts AS first_at, span.last_ts AS last_at,
                span.event_count, span.source
         FROM session_git_spans span
         JOIN page p ON p.session_id = span.session_id AND p.provider = span.provider
         ORDER BY span.first_ts, span.span_id"
    );
    let branch_spans = query_rows(conn, &branch_sql, params![limit, offset]).await?;
    let branch_eligible_sql = format!(
        "{PAGE_CTE}
         SELECT COUNT(*) AS eligible
         FROM session_git_spans span
         WHERE EXISTS (
             SELECT 1 FROM page p WHERE p.session_id = span.session_id
         )"
    );
    let branch_eligible = query_count(
        conn,
        &branch_eligible_sql,
        params![limit, offset],
        "eligible",
    )
    .await?;

    let generation_sql = format!(
        "{PAGE_CTE}
         SELECT COUNT(*) AS active_generations, MAX(generation.activated_at) AS latest_activated_at
         FROM session_temporal_generations generation
         JOIN page p ON p.session_id = generation.session_id
         WHERE generation.state = 'active'"
    );
    let generation_rows = query_rows(conn, &generation_sql, params![limit, offset]).await?;
    let generation = generation_rows
        .first()
        .cloned()
        .unwrap_or_else(|| json!({}));
    let active_generations = required_u64(&generation, "active_generations")?;
    let latest_activated_at = generation
        .get("latest_activated_at")
        .and_then(Value::as_i64);

    let statuses = vec![
        source_status(SourceStatusInput {
            id: "session_commit",
            label: "Session ↔ commit",
            state: relation_state(commit_eligible, commits.len()),
            authority: Some("commit_sessions"),
            granularity: "commit attribution",
            rows: &commits,
            reason: None,
            required_authority: None,
            coverage: relation_coverage(
                commit_eligible,
                commits.len(),
                "provider-qualified commit_sessions rows for the displayed session page",
            ),
        }),
        source_status(SourceStatusInput {
            id: "session_file",
            label: "Session → edited file",
            state: DashboardDomainStateV1::Partial,
            authority: Some("sessions.metadata_json $.edited_files[]"),
            granularity: "recorded file rollup",
            rows: &edited_files,
            reason: Some(
                "edited-file coverage is provider-native metadata; sessions without an \
                 edited_files array are omitted, never treated as no edits"
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
        source_status(SourceStatusInput {
            id: "branch_worktree",
            label: "Branch & worktree spans",
            state: relation_state(branch_eligible, branch_spans.len()),
            authority: Some("session_git_spans"),
            granularity: "coalesced activity span",
            rows: &branch_spans,
            reason: None,
            required_authority: None,
            coverage: relation_coverage(
                branch_eligible,
                branch_spans.len(),
                "provider-qualified session_git_spans rows for the displayed session page",
            ),
        }),
        LoomSourceStatusV1 {
            id: "delivery_outcomes",
            label: "Pull request, review, CI & release outcomes",
            state: DashboardDomainStateV1::Unsupported,
            authority: None,
            granularity: "Delivery projection row",
            providers: Vec::new(),
            item_count: None,
            reason: Some(DELIVERY_REASON.to_string()),
            required_authority: Some(DELIVERY_AUTHORITY),
            coverage: LoomSourceCoverageV1 {
                completeness: "unsupported",
                eligible: None,
                examined: None,
                matched: None,
                omitted: None,
                unit: None,
                reason:
                    "coverage belongs to the shared Delivery projection once it serves session-linked rows"
                        .to_string(),
            },
        },
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
            commits: decode_rows(commits, "Loom commits")?,
            edited_files: decode_rows(edited_files, "Loom edited files")?,
            branch_spans: decode_rows(branch_spans, "Loom branch spans")?,
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
    .await?;
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
    .await?;
    Ok(LoomFileSessionProjectionV1 {
        granularity: "file",
        authority: "sessions.metadata_json $.edited_files[]",
        providers: providers(&sessions),
        eligible_sessions,
        matched_sessions: matched_sessions(&sessions),
        sessions,
    })
}

fn relation_state(eligible: u64, returned: usize) -> DashboardDomainStateV1 {
    if eligible == returned as u64 {
        DashboardDomainStateV1::Ready
    } else {
        DashboardDomainStateV1::Partial
    }
}

fn relation_coverage(eligible: u64, returned: usize, reason: &str) -> LoomSourceCoverageV1 {
    let returned = returned as u64;
    let omitted = eligible.saturating_sub(returned);
    LoomSourceCoverageV1 {
        completeness: if omitted == 0 { "complete" } else { "partial" },
        eligible: Some(eligible),
        examined: Some(eligible),
        matched: Some(returned),
        omitted: Some(omitted),
        unit: Some("stored relation rows"),
        reason: reason.to_string(),
    }
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

async fn query_count(
    conn: &(impl QueryExecutor + ?Sized),
    sql: &str,
    params: impl IntoParams,
    field: &str,
) -> Result<u64, String> {
    let rows = query_rows(conn, sql, params).await?;
    let row = rows
        .first()
        .ok_or_else(|| format!("count query returned no row for {field}"))?;
    required_u64(row, field)
}

fn required_u64(row: &Value, field: &str) -> Result<u64, String> {
    let value = required_i64(row, field)?;
    u64::try_from(value).map_err(|_| format!("{field} was negative: {value}"))
}

fn required_i64(row: &Value, field: &str) -> Result<i64, String> {
    row.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("required integer field {field} was absent or invalid"))
}

fn required_str<'a>(row: &'a Value, field: &str) -> Result<&'a str, String> {
    row.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("required string field {field} was absent or invalid"))
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
    LoomTemporalPayloadV1 {
        available: false,
        total: 0,
        sessions: Vec::new(),
        source_statuses: vec![
            unavailable(
                "session_commit",
                "Session ↔ commit",
                "commit_sessions",
                "commit attribution",
            ),
            unavailable(
                "session_file",
                "Session → edited file",
                "sessions.metadata_json $.edited_files[]",
                "recorded file rollup",
            ),
            unavailable(
                "branch_worktree",
                "Branch & worktree spans",
                "session_git_spans",
                "coalesced activity span",
            ),
            LoomSourceStatusV1 {
                id: "delivery_outcomes",
                label: "Pull request, review, CI & release outcomes",
                state: DashboardDomainStateV1::Unsupported,
                authority: None,
                granularity: "Delivery projection row",
                providers: Vec::new(),
                item_count: None,
                reason: Some(DELIVERY_REASON.to_string()),
                required_authority: Some(DELIVERY_AUTHORITY),
                coverage: LoomSourceCoverageV1 {
                    completeness: "unsupported",
                    eligible: None,
                    examined: None,
                    matched: None,
                    omitted: None,
                    unit: None,
                    reason:
                        "coverage belongs to the shared Delivery projection once it serves session-linked rows"
                            .to_string(),
                },
            },
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

fn query_error(error: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "detail": format!("Loom temporal read failed: {error}") })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_dependency_names_the_shared_route() {
        let payload = unavailable_payload("session authority unavailable");
        let delivery = payload
            .source_statuses
            .iter()
            .find(|source| source.id == "delivery_outcomes")
            .expect("delivery status");
        assert_eq!(delivery.state, DashboardDomainStateV1::Unsupported);
        assert_eq!(delivery.required_authority, Some(DELIVERY_AUTHORITY));
    }
}
