use super::*;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    RetrievalGrainV1, SessionId, SymbolOccurrenceId, TemporalModeV1, canonical_sha256,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::session_temporal::GlobalDbCursorKeyProvider;
use tracedecay_temporal_query::cursor::{StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

const PR_CONTEXT_CURSOR_SESSION: &str = "session.daemon.pr-context";

#[derive(Serialize)]
pub(super) struct PrContextCursorBinding<'a> {
    pub protocol: &'static str,
    pub project_root: &'a str,
    pub base_oid: &'a str,
    pub head_oid: &'a str,
    pub merge_base: &'a str,
    pub graph_generation: &'a str,
    pub maximum_symbols: usize,
    pub changes: &'a [GitFileChange],
}

#[derive(Serialize, Deserialize)]
struct PrContextCursorKey<'a> {
    symbol_occurrence_id: &'a str,
    impact_nodes_admitted: usize,
    direct_call_edges_admitted: usize,
    impact_bytes_admitted: usize,
}

pub(super) struct PrContextCursorPosition {
    pub after: SymbolOccurrenceId,
    pub impact_nodes_admitted: usize,
    pub direct_call_edges_admitted: usize,
    pub impact_bytes_admitted: usize,
}

pub(super) async fn pr_context_cursor_authority(
    session_db: &RegisteredGlobalDb,
    binding: &PrContextCursorBinding<'_>,
) -> Result<(TemporalExecutionSnapshot, GlobalDbCursorKeyProvider)> {
    let digest = canonical_sha256(binding).map_err(|error| TraceDecayError::Config {
        message: format!("failed to bind PR context cursor: {error}"),
    })?;
    let graph_digest = canonical_sha256(&(
        "tracedecay.pr-context.graph-generation.v1",
        binding.graph_generation,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to bind PR context graph generation: {error}"),
    })?;
    let graph_generation_hex = graph_digest
        .as_str()
        .strip_prefix("sha256:")
        .and_then(|hex| hex.get(..16))
        .ok_or_else(|| TraceDecayError::Config {
            message: "invalid PR context graph generation digest".to_owned(),
        })?;
    let graph_generation = u64::from_str_radix(graph_generation_hex, 16)
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid PR context graph generation watermark: {error}"),
        })?
        .max(1);
    let authenticator = session_db
        .load_preprovisioned_session_cursor_key_provider_result()
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "pr_context_cursor_authority_unavailable",
                true,
                format!("pre-provisioned PR context cursor key is unavailable: {error}"),
            )
        })?;
    let key = authenticator.active_key_ref().clone();
    let request = TemporalSnapshotRequest::new(
        SessionId::new(PR_CONTEXT_CURSOR_SESSION).map_err(|error| TraceDecayError::Config {
            message: format!("invalid PR context cursor session: {error}"),
        })?,
        digest.as_str(),
        digest.as_str(),
        digest.as_str(),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid PR context cursor binding: {error}"),
    })?;
    let configuration_digest = BindingDigest::new("configuration_digest", digest.as_str())
        .map_err(|error| TraceDecayError::Config {
            message: format!("invalid PR context cursor configuration: {error}"),
        })?;
    let snapshot = TemporalExecutionSnapshot::new_authorized(
        request,
        TemporalWatermarks {
            generation: graph_generation,
            source: 1,
            projection: 1,
            index: 1,
            summary: 1,
        },
        KernelVersions {
            schema: 1,
            ranking: 1,
            configuration_digest,
        },
        Some(key),
        ValidatedAuthorization::Authorized,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("invalid PR context cursor snapshot: {error}"),
    })?;
    Ok((snapshot, authenticator))
}

pub(super) fn decode_pr_context_cursor(
    encoded: &str,
    snapshot: &TemporalExecutionSnapshot,
    authenticator: &GlobalDbCursorKeyProvider,
) -> Result<PrContextCursorPosition> {
    let sort_key = verify_cursor(encoded, snapshot, authenticator).map_err(|error| {
        TraceDecayError::Config {
            message: format!("invalid or stale PR context cursor: {error}"),
        }
    })?;
    let key: PrContextCursorKey<'_> =
        serde_json::from_str(&sort_key.stable_id).map_err(|_| TraceDecayError::Config {
            message: "invalid PR context cursor key".to_owned(),
        })?;
    Ok(PrContextCursorPosition {
        after: SymbolOccurrenceId::new(key.symbol_occurrence_id.to_owned()).map_err(|error| {
            TraceDecayError::Config {
                message: format!("invalid PR context symbol cursor: {error}"),
            }
        })?,
        impact_nodes_admitted: key.impact_nodes_admitted,
        direct_call_edges_admitted: key.direct_call_edges_admitted,
        impact_bytes_admitted: key.impact_bytes_admitted,
    })
}

pub(super) fn encode_pr_context_cursor(
    after: &SymbolOccurrenceId,
    impact_nodes_admitted: usize,
    direct_call_edges_admitted: usize,
    impact_bytes_admitted: usize,
    snapshot: &TemporalExecutionSnapshot,
    authenticator: &GlobalDbCursorKeyProvider,
) -> Result<String> {
    let stable_id = serde_json::to_string(&PrContextCursorKey {
        symbol_occurrence_id: after.as_str(),
        impact_nodes_admitted,
        direct_call_edges_admitted,
        impact_bytes_admitted,
    })
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to encode PR context cursor key: {error}"),
    })?;
    encode_cursor(
        snapshot,
        &StableSortKey {
            normalized_score_micros: 0,
            knowledge_at_micros: 0,
            stable_id,
        },
        authenticator,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to issue PR context cursor: {error}"),
    })
}
