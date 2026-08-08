mod compatibility;
mod generation;
mod message_anchor;
mod publication;
mod sources;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_runtime_core::db::engine::{Executor, params};

use tracedecay_sessions::compatibility::projected_content_hash;
use tracedecay_sessions::runtime::lcm::types::{LcmError, LcmSourceRef, LcmSummaryNodeDraft};

pub(in crate::session_temporal) use publication::load_and_verify_receipt;
pub use publication::{GlobalDbLcmSummaryPublication, publish_immutable_summary};

pub(super) const PUBLICATION_ROUTE: &str = "lcm_summary_lineage_v1";
pub const SANITIZER_VERSION: &str = "tracedecay.lcm-summary-publication.v1";
pub(super) const UNIX_TIMESTAMP_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CanonicalSourceBinding {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PreparedPayload {
    pub payload_ref: String,
    pub digest: String,
    pub manifest_json: String,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedSource {
    pub canonical: CanonicalSourceBinding,
    pub compatibility_anchor: bool,
    pub timestamp: i64,
    pub payload: Option<PreparedPayload>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CanonicalPublicationManifest {
    pub version: u32,
    pub provider: String,
    pub conversation_id: String,
    pub session_id: String,
    pub depth: i64,
    pub summary_text: String,
    pub summary_hash: String,
    pub source_refs: Vec<LcmSourceRef>,
    pub canonical_sources: Vec<CanonicalSourceBinding>,
    pub source_token_count: i64,
    pub summary_token_count: i64,
    pub source_time_start: Option<i64>,
    pub source_time_end: Option<i64>,
    pub expand_hint: Option<String>,
    pub metadata_json: Option<String>,
    pub source_horizon_json: String,
    pub owner_json: String,
    pub summary_anchor_id: String,
    pub receipt_id: String,
    pub predecessor_summary_id: Option<String>,
    pub logical_identity_digest: String,
    pub payloads: Vec<PreparedPayload>,
    pub model_route: String,
    pub configuration_digest: String,
    pub sanitization_receipt: String,
    pub route: Value,
}

impl CanonicalPublicationManifest {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn from_publication(
        draft: &LcmSummaryNodeDraft,
        summary_hash: String,
        sources: &[PreparedSource],
        source_horizon_json: String,
        owner_json: String,
        summary_anchor_id: String,
        receipt_id: String,
        predecessor_summary_id: Option<String>,
        logical_identity_digest: String,
    ) -> Self {
        let metadata = draft
            .metadata_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap_or(Value::Null);
        let model_route = metadata
            .get("codex_auxiliary_model")
            .or_else(|| metadata.get("model"))
            .and_then(Value::as_str)
            .unwrap_or(PUBLICATION_ROUTE)
            .to_string();
        let route = metadata
            .get("tracedecay_summary_source")
            .or_else(|| metadata.get("route"))
            .cloned()
            .unwrap_or_else(|| Value::String(PUBLICATION_ROUTE.to_string()));
        let payloads = sources
            .iter()
            .filter_map(|source| source.payload.clone())
            .map(|payload| (payload.payload_ref.clone(), payload))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect();
        Self {
            version: 1,
            provider: draft.provider.clone(),
            conversation_id: draft.conversation_id.clone(),
            session_id: draft.session_id.clone(),
            depth: draft.depth,
            summary_text: draft.summary_text.clone(),
            summary_hash: summary_hash.clone(),
            source_refs: draft.source_refs.clone(),
            canonical_sources: sources
                .iter()
                .map(|source| source.canonical.clone())
                .collect(),
            source_token_count: draft.source_token_count,
            summary_token_count: draft.summary_token_count,
            source_time_start: draft.source_time_start,
            source_time_end: draft.source_time_end,
            expand_hint: draft.expand_hint.clone(),
            metadata_json: draft.metadata_json.clone(),
            source_horizon_json,
            owner_json,
            summary_anchor_id,
            receipt_id: receipt_id.clone(),
            predecessor_summary_id,
            logical_identity_digest,
            payloads,
            model_route,
            configuration_digest: summary_hash,
            sanitization_receipt: receipt_id,
            route,
        }
    }

    pub(super) fn matches_draft(&self, draft: &LcmSummaryNodeDraft) -> bool {
        self.version == 1
            && self.provider == draft.provider
            && self.conversation_id == draft.conversation_id
            && self.session_id == draft.session_id
            && self.depth == draft.depth
            && self.summary_text == draft.summary_text
            && self.summary_hash == projected_content_hash(&draft.summary_text)
            && self.source_refs == draft.source_refs
            && self.source_token_count == draft.source_token_count
            && self.summary_token_count == draft.summary_token_count
            && self.source_time_start == draft.source_time_start
            && self.source_time_end == draft.source_time_end
            && self.expand_hint == draft.expand_hint
            && self.metadata_json == draft.metadata_json
            && self.configuration_digest == self.summary_hash
            && self.sanitization_receipt == self.receipt_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenPublicationReceipt {
    pub summary_id: String,
    pub disposition: String,
    pub published_at: i64,
    pub generation: i64,
    pub frozen_watermarks_json: String,
    pub source_horizon_json: String,
    pub publication_manifest_digest: String,
}

pub fn receipt_id(summary_id: &str, summary_hash: &str) -> String {
    format!(
        "receipt_summary_{}",
        projected_content_hash(&format!("{summary_id}\0{summary_hash}"))
    )
}

pub(super) fn logical_identity_digest(draft: &LcmSummaryNodeDraft) -> Result<String, LcmError> {
    let identity = serde_json::to_string(&(
        draft.provider.as_str(),
        draft.conversation_id.as_str(),
        draft.session_id.as_str(),
        draft.depth,
        &draft.source_refs,
    ))
    .map_err(|error| LcmError::Db(format!("encode summary logical identity: {error}")))?;
    Ok(projected_content_hash(&identity))
}

pub(super) fn normalize_timestamp(value: i64) -> i64 {
    if value.abs() < UNIX_TIMESTAMP_MILLIS_THRESHOLD {
        value.saturating_mul(1_000_000)
    } else {
        value
    }
}

pub(super) async fn unixepoch(conn: &impl Executor) -> Result<i64, LcmError> {
    let mut rows = conn.query("SELECT unixepoch() * 1000000", ()).await?;
    rows.next()
        .await?
        .ok_or_else(|| LcmError::Db("unixepoch query returned no rows".to_string()))?
        .get(0)
        .map_err(LcmError::from)
}

pub(super) async fn load_manifest(
    conn: &impl Executor,
    summary_id: &str,
) -> Result<Option<(CanonicalPublicationManifest, i64)>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT publication_json, created_at
             FROM session_summary_nodes WHERE summary_id = ?1",
            params![summary_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let raw: String = row.get(0)?;
    let manifest = serde_json::from_str(&raw).map_err(|_| LcmError::ImmutableSummaryConflict {
        summary_id: summary_id.to_string(),
    })?;
    Ok(Some((manifest, row.get(1)?)))
}
