//! Walk hydrated LCM session history and measure unused returned tool context.

use tracedecay_contracts::retrieval::AdminCliUnusedContextReportV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_lcm::LcmLoadSessionRequest;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_sessions::unused_context::{
    TimelineEvent, UnusedContextOptions, measure_unused_context,
};

const LOAD_PAGE_LIMIT: usize = 100;
const ANALYSIS_SESSION_CAP: usize = 10_000;

pub(super) async fn sessions_unused_context(
    db: &RegisteredGlobalDbLeaseV1,
    example_limit: usize,
    session_limit: usize,
) -> Result<AdminCliUnusedContextReportV1> {
    let session_limit = session_limit.clamp(1, ANALYSIS_SESSION_CAP);
    let sessions = list_sessions(db, session_limit).await?;
    let mut events = Vec::new();
    for (provider, session_id) in sessions {
        load_session_events(db, &provider, &session_id, &mut events).await?;
    }
    Ok(measure_unused_context(
        &events,
        UnusedContextOptions {
            example_limit: example_limit.max(1),
        },
    ))
}

async fn list_sessions(
    db: &RegisteredGlobalDbLeaseV1,
    limit: usize,
) -> Result<Vec<(String, String)>> {
    let mut rows = db
        .query(
            "SELECT provider, session_id
             FROM lcm_raw_messages
             GROUP BY provider, session_id
             ORDER BY MAX(store_id) DESC
             LIMIT ?1",
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
        .map_err(|error| {
            TraceDecayError::database_operation("list unused-context sessions", error)
        })?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await.map_err(|error| {
        TraceDecayError::database_operation("read unused-context session row", error)
    })? {
        let provider: String = row.get(0).map_err(|error| {
            TraceDecayError::database_operation("decode unused-context session provider", error)
        })?;
        let session_id: String = row.get(1).map_err(|error| {
            TraceDecayError::database_operation("decode unused-context session id", error)
        })?;
        sessions.push((provider, session_id));
    }
    Ok(sessions)
}

async fn load_session_events(
    db: &RegisteredGlobalDbLeaseV1,
    provider: &str,
    session_id: &str,
    events: &mut Vec<TimelineEvent>,
) -> Result<()> {
    let labels = message_labels(db, provider, session_id).await?;
    let mut after_store_id = None;
    loop {
        let page = db
            .lcm_load_session(LcmLoadSessionRequest {
                provider: provider.to_owned(),
                session_id: session_id.to_owned(),
                after_store_id,
                limit: LOAD_PAGE_LIMIT,
                roles: Vec::new(),
                start_time: None,
                end_time: None,
                content_slice: None,
            })
            .await
            .map_err(|error| TraceDecayError::Config {
                message: format!("LCM load failed for {provider}/{session_id}: {error}"),
            })?;
        if page.messages.is_empty() {
            break;
        }
        for message in page.messages {
            let (kind, tool_names) = labels
                .get(&message.store_id)
                .cloned()
                .unwrap_or((None, None));
            after_store_id = Some(message.store_id);
            events.push(TimelineEvent {
                provider: message.provider,
                session_id: message.session_id,
                message_id: message.message_id,
                store_id: message.store_id,
                role: message.role,
                kind,
                tool_names,
                content: message.content,
                metadata_json: message.metadata_json,
            });
        }
        if page.next_cursor.is_none() {
            break;
        }
    }
    Ok(())
}

async fn message_labels(
    db: &RegisteredGlobalDbLeaseV1,
    provider: &str,
    session_id: &str,
) -> Result<std::collections::BTreeMap<i64, (Option<String>, Option<String>)>> {
    let mut rows = db
        .query(
            "SELECT store_id, kind, tool_names
             FROM lcm_raw_messages
             WHERE provider = ?1 AND session_id = ?2
             ORDER BY store_id",
            params![provider, session_id],
        )
        .await
        .map_err(|error| {
            TraceDecayError::database_operation("list unused-context message labels", error)
        })?;
    let mut labels = std::collections::BTreeMap::new();
    while let Some(row) = rows.next().await.map_err(|error| {
        TraceDecayError::database_operation("read unused-context message label", error)
    })? {
        let store_id: i64 = row.get(0).map_err(|error| {
            TraceDecayError::database_operation("decode unused-context store_id", error)
        })?;
        let kind: Option<String> = row.get(1).map_err(|error| {
            TraceDecayError::database_operation("decode unused-context kind", error)
        })?;
        let tool_names: Option<String> = row.get(2).map_err(|error| {
            TraceDecayError::database_operation("decode unused-context tool_names", error)
        })?;
        labels.insert(store_id, (kind, tool_names));
    }
    Ok(labels)
}
