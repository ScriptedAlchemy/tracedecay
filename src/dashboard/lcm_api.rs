//! LCM dashboard API, backed by tracedecay's LCM session store.
//!
//! Serves Hermes-compatible LCM routes from `lcm_raw_messages`,
//! `lcm_summary_nodes`, and `lcm_summary_sources`. The store is selected by
//! [`super::resolve_lcm_store`], and every payload reports it via `path` and
//! `storage_scope`.
//!
//! Schema mapping (hermes-lcm → tracedecay):
//! - `messages`               → `lcm_raw_messages` (`source` ← `provider`,
//!   `token_estimate` ← ~chars/4, `pinned`/`tool_name` not tracked)
//! - `summary_nodes`          → `lcm_summary_nodes` (`summary` ←
//!   `summary_text`, `token_count` ← `summary_token_count`, `latest_at` ←
//!   `source_time_end`; node ids are strings, not ints)
//! - `summary_nodes.source_ids` JSON → `lcm_summary_sources` rows
//! - FTS mirrors → `lcm_raw_messages_fts` / `lcm_summary_nodes_fts`

use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::lcm_service;
use super::util::{coerce_limit, JsonPath, JsonQuery};
use super::DashboardState;
use crate::sessions::lcm::{gc, query, LcmGcConfig};
use crate::tracedecay::current_timestamp;

type LcmResponse = (StatusCode, Json<Value>);
type LcmResult = Result<LcmResponse, LcmResponse>;

#[derive(Debug, Clone)]
struct PayloadGcPreview {
    token: String,
    provider: String,
    session_id: Option<String>,
    created_at: i64,
}

static PAYLOAD_GC_PREVIEW: LazyLock<Mutex<Option<PayloadGcPreview>>> =
    LazyLock::new(|| Mutex::new(None));

fn ok(payload: Map<String, Value>) -> LcmResponse {
    (StatusCode::OK, Json(Value::Object(payload)))
}

fn err(status: StatusCode, message: impl Into<String>) -> LcmResponse {
    (
        status,
        Json(json!({
            "status": "error",
            "error": message.into(),
        })),
    )
}

#[derive(Deserialize)]
pub(crate) struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct PayloadHealthParams {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    session_id: String,
    deep: Option<bool>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct PayloadGcApplyRequest {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    dry_run_token: String,
    confirm: Option<bool>,
}

/// `GET /api/plugins/hermes-lcm/overview`
pub(crate) async fn overview(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<OverviewParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 25, 200);
    let mut payload = lcm_service::overview_payload(&state, &params.q, limit).await?;
    if let Some(conn) = &state.lcm_conn {
        let provider = "cursor";
        let storage_root = lcm_storage_root(&state);
        let detail = query::payload_health_detail(
            conn,
            &storage_root,
            provider,
            None,
            false,
            20,
            &LcmGcConfig::default(),
        )
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("overview payload health failed: {e}"),
            )
        })?;
        payload.insert("payload_health".into(), payload_health_value(&detail));
    } else {
        payload.insert("payload_health".into(), Value::Null);
    }
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct SearchParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    offset: Option<i64>,
    #[serde(default)]
    role: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    since: String,
    #[serde(default)]
    until: String,
}

/// `GET /api/plugins/hermes-lcm/search`
pub(crate) async fn search(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<SearchParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 25, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let since = lcm_service::parse_epoch(&params.since);
    let until = lcm_service::parse_epoch(&params.until);
    let payload = lcm_service::search_payload(
        &state,
        lcm_service::SearchPayloadArgs {
            query: &params.q,
            limit,
            offset,
            role: &params.role,
            source: &params.source,
            session_id: &params.session_id,
            since,
            until,
        },
    )
    .await?;
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct SessionParams {
    limit: Option<i64>,
    offset: Option<i64>,
    #[serde(default)]
    order: String,
}

/// `GET /api/plugins/hermes-lcm/session/{session_id}`
pub(crate) async fn session(
    State(state): State<DashboardState>,
    JsonPath(session_id): JsonPath<String>,
    JsonQuery(params): JsonQuery<SessionParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 200, 1000);
    let offset = params.offset.unwrap_or(0).max(0);
    let descending = params.order.eq_ignore_ascii_case("desc");
    let payload =
        lcm_service::session_payload(&state, &session_id, limit, offset, descending).await?;
    Ok(ok(payload))
}

/// `GET /api/plugins/hermes-lcm/node/{node_id}` — a summary node plus the
/// exact source items it covers (lossless expand).
pub(crate) async fn node(
    State(state): State<DashboardState>,
    JsonPath(node_id): JsonPath<String>,
) -> LcmResult {
    let payload = lcm_service::node_payload(&state, &node_id).await?;
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct TimelineParams {
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    session_id: String,
    limit: Option<i64>,
}

/// `GET /api/plugins/hermes-lcm/timeline`
pub(crate) async fn timeline(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<TimelineParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 400, 2000);
    let by_hour = params.bucket.eq_ignore_ascii_case("hour");
    let payload = lcm_service::timeline_payload(&state, by_hour, &params.session_id, limit).await?;
    Ok(ok(payload))
}

#[derive(Deserialize)]
pub(crate) struct CompressionParams {
    #[serde(default)]
    by: String,
    limit: Option<i64>,
}

/// `GET /api/plugins/hermes-lcm/compression`
pub(crate) async fn compression(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<CompressionParams>,
) -> LcmResult {
    let limit = coerce_limit(params.limit, 50, 500);
    let by_node = params.by.eq_ignore_ascii_case("node");
    let payload = lcm_service::compression_payload(&state, by_node, limit).await?;
    Ok(ok(payload))
}

/// `GET /api/plugins/hermes-lcm/payloads/health`
pub(crate) async fn payloads_health(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<PayloadHealthParams>,
) -> LcmResult {
    let conn = state
        .lcm_conn
        .as_ref()
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "LCM store unavailable"))?;
    let provider = if params.provider.trim().is_empty() {
        "cursor"
    } else {
        params.provider.trim()
    };
    let session_id = (!params.session_id.trim().is_empty()).then_some(params.session_id.trim());
    let deep = params.deep.unwrap_or(false);
    let sample_limit = coerce_limit(params.limit, 20, 100) as usize;
    let detail = query::payload_health_detail(
        conn,
        &lcm_storage_root(&state),
        provider,
        session_id,
        deep,
        sample_limit,
        &LcmGcConfig::default(),
    )
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("payload health failed: {e}"),
        )
    })?;

    Ok(ok(Map::from_iter([
        ("status".into(), json!("ok")),
        ("provider".into(), json!(provider)),
        (
            "session_id".into(),
            session_id.map_or(Value::Null, |value| json!(value)),
        ),
        ("storage_scope".into(), json!(state.lcm_scope)),
        ("path".into(), json!(state.lcm_db_path)),
        ("deep".into(), json!(deep)),
        ("payload_health".into(), payload_health_value(&detail)),
        (
            "samples".into(),
            json!({
                "missing_payload_refs": detail.missing_payload_refs,
                "orphan_files": detail.orphan_files,
                "unreferenced_refs": detail.unreferenced_refs,
                "missing_placeholder_refs": detail.missing_placeholder_refs,
                "integrity_mismatch_refs": detail.integrity_mismatch_refs,
            }),
        ),
    ])))
}

/// `GET /api/plugins/hermes-lcm/payloads/gc`
pub(crate) async fn payloads_gc_preview(
    State(state): State<DashboardState>,
    JsonQuery(params): JsonQuery<PayloadHealthParams>,
) -> LcmResult {
    let conn = state
        .lcm_conn
        .as_ref()
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "LCM store unavailable"))?;
    let provider = if params.provider.trim().is_empty() {
        "cursor"
    } else {
        params.provider.trim()
    };
    let session_id = (!params.session_id.trim().is_empty()).then_some(params.session_id.trim());
    let report = gc::run_payload_gc_with_apply(
        conn,
        &lcm_storage_root(&state),
        provider,
        session_id,
        &LcmGcConfig::default(),
        false,
        current_timestamp(),
    )
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("payload GC preview failed: {e}"),
        )
    })?;
    let token = make_preview_token(provider, session_id);
    if let Ok(mut preview) = PAYLOAD_GC_PREVIEW.lock() {
        *preview = Some(PayloadGcPreview {
            token: token.clone(),
            provider: provider.to_string(),
            session_id: session_id.map(str::to_string),
            created_at: now_unix(),
        });
    }

    Ok(ok(Map::from_iter([
        ("status".into(), json!("ok")),
        ("provider".into(), json!(provider)),
        (
            "session_id".into(),
            session_id.map_or(Value::Null, |value| json!(value)),
        ),
        ("storage_scope".into(), json!(state.lcm_scope)),
        ("path".into(), json!(state.lcm_db_path)),
        ("dry_run".into(), json!(true)),
        ("dry_run_token".into(), json!(token)),
        (
            "gc_report".into(),
            serde_json::to_value(report).unwrap_or(Value::Null),
        ),
    ])))
}

/// `POST /api/plugins/hermes-lcm/payloads/gc`
pub(crate) async fn payloads_gc_apply(
    State(state): State<DashboardState>,
    Json(body): Json<PayloadGcApplyRequest>,
) -> LcmResult {
    let conn = state
        .lcm_conn
        .as_ref()
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "LCM store unavailable"))?;
    let provider = if body.provider.trim().is_empty() {
        "cursor"
    } else {
        body.provider.trim()
    };
    let session_id = (!body.session_id.trim().is_empty()).then_some(body.session_id.trim());
    if body.confirm != Some(true) || body.dry_run_token.trim().is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "payload GC apply requires confirm=true and a prior dry_run_token",
        ));
    }
    {
        let preview = PAYLOAD_GC_PREVIEW.lock().map_err(|_| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "payload GC preview lock poisoned",
            )
        })?;
        let Some(preview) = preview.as_ref() else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "payload GC apply requires a prior dry-run preview",
            ));
        };
        if preview.token != body.dry_run_token
            || preview.provider != provider
            || preview.session_id.as_deref() != session_id
            || now_unix().saturating_sub(preview.created_at) > 300
        {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "payload GC dry_run_token is missing, expired, or does not match the requested scope",
            ));
        }
    }

    let report = gc::run_payload_gc_with_apply(
        conn,
        &lcm_storage_root(&state),
        provider,
        session_id,
        &LcmGcConfig::default(),
        true,
        current_timestamp(),
    )
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("payload GC apply failed: {e}"),
        )
    })?;
    if let Ok(mut preview) = PAYLOAD_GC_PREVIEW.lock() {
        *preview = None;
    }

    Ok(ok(Map::from_iter([
        ("status".into(), json!("ok")),
        ("provider".into(), json!(provider)),
        (
            "session_id".into(),
            session_id.map_or(Value::Null, |value| json!(value)),
        ),
        ("storage_scope".into(), json!(state.lcm_scope)),
        ("path".into(), json!(state.lcm_db_path)),
        ("dry_run".into(), json!(false)),
        (
            "gc_report".into(),
            serde_json::to_value(report).unwrap_or(Value::Null),
        ),
    ])))
}

fn payload_health_value(detail: &query::PayloadHealthDetail) -> Value {
    let mut object = Map::new();
    object.insert(
        "status".into(),
        json!(query::payload_health_state(
            &detail.payload,
            &detail.payload_gc
        )),
    );
    merge_object(
        &mut object,
        serde_json::to_value(&detail.payload).unwrap_or(Value::Null),
    );
    merge_object(
        &mut object,
        serde_json::to_value(&detail.payload_gc).unwrap_or(Value::Null),
    );
    Value::Object(object)
}

fn merge_object(target: &mut Map<String, Value>, value: Value) {
    if let Value::Object(object) = value {
        target.extend(object);
    }
}

fn lcm_storage_root(state: &DashboardState) -> PathBuf {
    Path::new(&state.lcm_db_path)
        .parent()
        .map_or_else(|| state.store_root.clone(), Path::to_path_buf)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn make_preview_token(provider: &str, session_id: Option<&str>) -> String {
    let session = session_id.unwrap_or("all");
    format!(
        "payload-gc-{}-{}-{}-{}",
        provider,
        session,
        std::process::id(),
        now_unix()
    )
}
