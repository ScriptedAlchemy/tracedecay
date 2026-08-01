use tracedecay_runtime_core::db::engine::{Executor, params};

use tracedecay_sessions::lcm::contracts::{LcmError, LcmSourceRef};

use super::CanonicalPublicationManifest;

/// Materializes canonical authority into the legacy LCM projection.
///
/// This module intentionally has no legacy reads. Existing projection rows
/// conflict at the database boundary; they are never consulted for identity,
/// replay, authorization, or publication decisions.
pub(super) async fn project_canonical_summary(
    conn: &impl Executor,
    summary_id: &str,
    manifest: &CanonicalPublicationManifest,
    created_at: i64,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_summary_nodes (
            node_id, provider, conversation_id, session_id, depth, summary_text,
            summary_hash, summary_token_count, source_token_count, source_time_start,
            source_time_end, expand_hint, metadata_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            summary_id,
            manifest.provider.as_str(),
            manifest.conversation_id.as_str(),
            manifest.session_id.as_str(),
            manifest.depth,
            manifest.summary_text.as_str(),
            manifest.summary_hash.as_str(),
            manifest.summary_token_count,
            manifest.source_token_count,
            manifest.source_time_start,
            manifest.source_time_end,
            manifest.expand_hint.as_deref(),
            manifest.metadata_json.as_deref(),
            created_at,
        ],
    )
    .await?;
    for (ordinal, source) in manifest.source_refs.iter().enumerate() {
        let (kind, id) = match source {
            LcmSourceRef::RawMessage { store_id } => ("raw_message", store_id.to_string()),
            LcmSourceRef::SummaryNode { node_id } => ("summary_node", node_id.clone()),
        };
        conn.execute(
            "INSERT INTO lcm_summary_sources (node_id, source_kind, source_id, ordinal)
             VALUES (?1, ?2, ?3, ?4)",
            params![summary_id, kind, id.as_str(), ordinal as i64],
        )
        .await?;
    }
    Ok(())
}
