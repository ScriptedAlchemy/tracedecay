use tracedecay_runtime_core::db::engine::{Error, QueryExecutor, params};
use tracedecay_store::{
    CodexGoalContextSource, SessionMessageRecord, codex_goal_context_correlation,
};

const CORRELATION_LOOKBACK_MESSAGES: i64 = 32;

/// Find the nearest preceding, still-live response representation of a current
/// Codex goal. The lookup reads at most 32 projected metadata rows and compares
/// only SHA-256 semantic identities; transcript text never enters the query.
pub async fn find_preceding_codex_goal_response(
    conn: &impl QueryExecutor,
    current: &SessionMessageRecord,
) -> Result<Option<String>, Error> {
    let Some(current_correlation) =
        codex_goal_context_correlation(current.kind.as_deref(), current.metadata_json.as_deref())
    else {
        return Ok(None);
    };
    if current.provider != "codex"
        || current_correlation.source() != CodexGoalContextSource::ItemCompleted
        || current_correlation.admission_paired()
    {
        return Ok(None);
    }
    let mut rows = conn
        .query(
            "SELECT message_id, kind, metadata_json
             FROM (
                SELECT message_id, kind, metadata_json, ordinal
                FROM session_messages
                WHERE provider = ?1 AND session_id = ?2
                  AND ((?3 IS NULL AND source_path IS NULL) OR source_path = ?3)
                  AND ordinal < ?4
                ORDER BY ordinal DESC
                LIMIT ?5
             )
             ORDER BY ordinal DESC",
            params![
                current.provider.as_str(),
                current.session_id.as_str(),
                current.source_path.as_deref(),
                current.ordinal,
                CORRELATION_LOOKBACK_MESSAGES,
            ],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let message_id = row.get::<String>(0)?;
        let kind = row.get::<Option<String>>(1)?;
        let metadata_json = row.get::<Option<String>>(2)?;
        let Some(candidate) =
            codex_goal_context_correlation(kind.as_deref(), metadata_json.as_deref())
        else {
            continue;
        };
        if candidate.source() == CodexGoalContextSource::ResponseItem
            && candidate.identity() == current_correlation.identity()
        {
            return Ok(Some(message_id));
        }
    }
    Ok(None)
}
