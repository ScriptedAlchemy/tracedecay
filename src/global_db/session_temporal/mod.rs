mod cursor_keys;
mod direct;
mod doctor_health;
mod expand;
mod hydration;
mod lcm_render;
pub(crate) mod operations;
mod projection;
mod query;
mod rebuild;
mod refresh;
mod retrieval;
mod schema;

use libsql::params;
use serde::Deserialize;
use tracedecay_domain::{RetrievalAnchorId, SessionId, SignedCursorKeyRefV1};

use crate::application::session::{
    AuthorizedTemporalExecutionRequest, SessionTemporalExecutionError,
    SessionTemporalExecutionPort, SessionTemporalExecutionReport, TemporalExecutionFuture,
};
use crate::global_db::{GlobalDb, GlobalDbReadSnapshot};
use crate::query::temporal::context::VersionedTokenEstimator;
use crate::query::temporal::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use crate::query::temporal::execute_temporal_kernel;
use crate::query::temporal::ports::{
    BindingDigest, KernelVersions, MAX_TEMPORAL_PARTICIPANTS, TemporalExecutionSnapshot,
    TemporalParticipantGeneration, TemporalParticipantManifest, TemporalRetrievalScope,
    TemporalSourceAccess, TemporalWatermarks,
};
use crate::query::temporal::resolution::ValidatedAuthorization;
use crate::sessions::SessionMessageRecord;
use crate::sessions::lcm::{
    LcmDescribeRequest, LcmDescribeResponse, LcmDescribeTarget, LcmError, LcmExpandRequest,
    LcmExpandResponse, LcmExpandTarget, LcmExpandedSummarySource, LcmSourceRef,
};

pub(crate) use self::cursor_keys::GlobalDbCursorKeyProvider;
pub(crate) use self::direct::ResolvedDirectAnchor;
use self::hydration::GlobalDbTemporalHydrationPort;
use self::retrieval::GlobalDbTemporalReadPort;

// Consumed by the pr8/transport cold-Doctor route when that branch is integrated.
#[allow(unused_imports)]
pub(crate) use doctor_health::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
    session_temporal_doctor_health_at,
};
pub(in crate::global_db) use projection::record_canonical_observation_effect;
pub use refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
pub(crate) use schema::{ensure_session_temporal_schema, repair_session_temporal_state};

/// Production temporal executor over one already-open authoritative global DB.
pub struct GlobalDbSessionTemporalExecution<'db> {
    db: &'db GlobalDb,
}

impl<'db> GlobalDbSessionTemporalExecution<'db> {
    pub const fn new(db: &'db GlobalDb) -> Self {
        Self { db }
    }

    pub(crate) async fn session_message_from_hydrated_occurrence(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
        content: &[u8],
    ) -> Result<SessionMessageRecord, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        hydration::session_message_from_hydrated_bytes(&read, snapshot, anchor_id, content)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)
    }

    pub(crate) async fn resolve_lcm_describe_target(
        &self,
        provider: &str,
        session_id: &SessionId,
        target: &LcmDescribeTarget,
    ) -> Result<Option<ResolvedDirectAnchor>, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        direct::resolve_describe_target(&read, provider, session_id, target).await
    }

    pub(crate) async fn resolve_lcm_expand_target(
        &self,
        provider: &str,
        session_id: &SessionId,
        target: &LcmExpandTarget,
    ) -> Result<ResolvedDirectAnchor, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        direct::resolve_expand_target(&read, provider, session_id, target).await
    }

    pub(crate) async fn render_lcm_describe(
        &self,
        request: LcmDescribeRequest,
    ) -> Result<LcmDescribeResponse, SessionTemporalExecutionError> {
        lcm_render::describe(self.db, request)
            .await
            .map_err(map_lcm_error)
    }

    pub(crate) async fn render_lcm_expand(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        request: LcmExpandRequest,
        canonical_content: &str,
    ) -> Result<LcmExpandResponse, SessionTemporalExecutionError> {
        let provider = request.provider.clone();
        let session_id = SessionId::new(request.session_id.clone())
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let mut expansion = lcm_render::expand(self.db, request, canonical_content)
            .await
            .map_err(map_lcm_error)?;
        self.canonicalize_lcm_summary_sources(snapshot, &provider, &session_id, &mut expansion)
            .await?;
        Ok(expansion)
    }

    pub(crate) async fn encode_lcm_source_cursor(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        binding: &str,
        next_source_offset: usize,
    ) -> Result<String, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let authenticator = GlobalDbCursorKeyProvider::from_snapshot(&read, snapshot)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        encode_cursor(
            snapshot,
            &lcm_source_cursor_sort_key(binding, next_source_offset),
            &authenticator,
        )
        .map_err(map_lcm_cursor_error)
    }

    pub(crate) async fn decode_lcm_source_cursor(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        binding: &str,
        encoded: &str,
    ) -> Result<usize, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let authenticator = GlobalDbCursorKeyProvider::from_snapshot(&read, snapshot)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let sort_key =
            verify_cursor(encoded, snapshot, &authenticator).map_err(map_lcm_cursor_error)?;
        parse_lcm_source_cursor_offset(binding, &sort_key)
    }

    async fn canonicalize_lcm_summary_sources(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        provider: &str,
        session_id: &SessionId,
        expansion: &mut LcmExpandResponse,
    ) -> Result<(), SessionTemporalExecutionError> {
        if expansion.summary_sources.is_empty() {
            return Ok(());
        }
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        for source in &mut expansion.summary_sources {
            let target = match &source.source_ref {
                LcmSourceRef::RawMessage { store_id } => LcmExpandTarget::RawMessage {
                    store_id: *store_id,
                },
                LcmSourceRef::SummaryNode { node_id } => LcmExpandTarget::SummaryNode {
                    node_id: node_id.clone(),
                },
            };
            let direct =
                direct::resolve_expand_target(&read, provider, session_id, &target).await?;
            let bytes = hydration::hydrate_authorized_anchor_bytes(
                &read,
                &self.db.storage_root,
                snapshot,
                &direct.anchor_id,
            )
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let canonical_content = String::from_utf8(bytes.to_vec())
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            replace_summary_source_content(source, &canonical_content)?;
        }
        Ok(())
    }

    async fn freeze(
        &self,
        request: &AuthorizedTemporalExecutionRequest,
    ) -> Result<(GlobalDbReadSnapshot, TemporalExecutionSnapshot), SessionTemporalExecutionError>
    {
        let control = request.snapshot_request().execution_control();
        control.checkpoint().map_err(map_control_error)?;
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let (participants, watermarks, cursor_key) = freeze_participants(&read, request).await?;
        control.checkpoint().map_err(map_control_error)?;
        let snapshot = TemporalExecutionSnapshot::new_authorized(
            request.snapshot_request().clone(),
            watermarks,
            KernelVersions {
                schema: request.schema_version(),
                ranking: request.ranking_version(),
                configuration_digest: BindingDigest::new(
                    "configuration_digest",
                    request.configuration_digest(),
                )
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
            },
            cursor_key,
            ValidatedAuthorization::Authorized,
        )
        .and_then(|snapshot| snapshot.with_participant_manifest(participants))
        .map_err(map_control_error)?;
        Ok((read, snapshot))
    }
}

async fn freeze_participants(
    read: &GlobalDbReadSnapshot,
    request: &AuthorizedTemporalExecutionRequest,
) -> Result<
    (
        TemporalParticipantManifest,
        TemporalWatermarks,
        Option<SignedCursorKeyRefV1>,
    ),
    SessionTemporalExecutionError,
> {
    let snapshot_request = request.snapshot_request();
    let provider = snapshot_request.provider_scope();
    let mut rows = match snapshot_request.retrieval_scope() {
        TemporalRetrievalScope::Session(session_id) => {
            read.query(
                "SELECT generation.session_id, source.provider, generation.generation,
                        generation.frozen_watermarks_json
                 FROM session_temporal_generations AS generation
                 JOIN sessions AS source ON source.session_id = generation.session_id
                 WHERE generation.session_id = ?1
                   AND generation.state = 'active'
                   AND (?2 IS NULL OR source.provider = ?2)
                 ORDER BY generation.session_id, source.provider
                 LIMIT ?3",
                params![
                    session_id.as_str(),
                    provider,
                    i64::try_from(MAX_TEMPORAL_PARTICIPANTS + 1).unwrap_or(i64::MAX)
                ],
            )
            .await
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            let project_key = snapshot_request
                .authorized_root()
                .ok_or(SessionTemporalExecutionError::WrongScope)?
                .project_key();
            read.query(
                "SELECT generation.session_id, source.provider, generation.generation,
                        generation.frozen_watermarks_json
                 FROM sessions AS source
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = source.session_id
                  AND generation.state = 'active'
                 WHERE source.project_key = ?1
                   AND (?2 IS NULL OR source.provider = ?2)
                 ORDER BY generation.session_id, source.provider
                 LIMIT ?3",
                params![
                    project_key,
                    provider,
                    i64::try_from(MAX_TEMPORAL_PARTICIPANTS + 1).unwrap_or(i64::MAX)
                ],
            )
            .await
        }
    }
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;

    let configuration_digest =
        BindingDigest::new("configuration_digest", request.configuration_digest())
            .map_err(map_control_error)?;
    let mut entries = Vec::new();
    let mut aggregate = TemporalWatermarks {
        generation: 0,
        source: 0,
        projection: 0,
        index: 0,
        summary: 0,
    };
    let mut shared_cursor_key = None::<Option<SignedCursorKeyRefV1>>;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?
    {
        snapshot_request
            .execution_control()
            .checkpoint()
            .map_err(map_control_error)?;
        let session_id = row
            .get::<String>(0)
            .ok()
            .and_then(|value| tracedecay_domain::SessionId::new(value).ok())
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let source_id = row
            .get::<String>(1)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let generation = row
            .get::<i64>(2)
            .ok()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let encoded = row
            .get::<String>(3)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        if frozen.active_generation > generation {
            return Err(SessionTemporalExecutionError::Unavailable);
        }
        let watermarks = TemporalWatermarks {
            generation,
            source: frozen.source_frontier,
            projection: frozen.projection_frontier,
            index: frozen.projection_frontier,
            summary: frozen.summary_frontier,
        };
        aggregate.generation = aggregate.generation.max(watermarks.generation);
        aggregate.source = aggregate.source.max(watermarks.source);
        aggregate.projection = aggregate.projection.max(watermarks.projection);
        aggregate.index = aggregate.index.max(watermarks.index);
        aggregate.summary = aggregate.summary.max(watermarks.summary);
        match &shared_cursor_key {
            Some(expected) if expected != &frozen.cursor_key => {
                return Err(SessionTemporalExecutionError::Unavailable);
            }
            None => shared_cursor_key = Some(frozen.cursor_key.clone()),
            Some(_) => {}
        }
        entries.push(
            TemporalParticipantGeneration::new(
                session_id,
                source_id,
                watermarks,
                watermarks.projection,
                &configuration_digest,
                snapshot_request.access_digest(),
                TemporalSourceAccess::Authorized,
            )
            .map_err(map_control_error)?,
        );
    }
    let participants = TemporalParticipantManifest::new(entries).map_err(map_control_error)?;
    Ok((participants, aggregate, shared_cursor_key.flatten()))
}

impl SessionTemporalExecutionPort for GlobalDbSessionTemporalExecution<'_> {
    fn execute<'a, E>(
        &'a self,
        request: AuthorizedTemporalExecutionRequest,
        estimator: &'a E,
    ) -> TemporalExecutionFuture<'a>
    where
        E: VersionedTokenEstimator + Sync + 'a,
    {
        Box::pin(async move {
            let (read_snapshot, snapshot) = self.freeze(&request).await?;
            let authenticator = GlobalDbCursorKeyProvider::from_snapshot(&read_snapshot, &snapshot)
                .await
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let kernel_request = request.into_kernel_request(snapshot);
            let read = GlobalDbTemporalReadPort::new(&read_snapshot);
            let hydration =
                GlobalDbTemporalHydrationPort::for_snapshot(&read_snapshot, &self.db.storage_root);
            let result = execute_temporal_kernel(
                &kernel_request,
                &read,
                &hydration,
                &authenticator,
                estimator,
            )
            .await
            .map_err(SessionTemporalExecutionError::Kernel)?;
            let source_coverage = result
                .snapshot
                .source_coverage()
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            Ok(SessionTemporalExecutionReport::from_source_coverage(
                result,
                source_coverage,
            ))
        })
    }
}

#[derive(Deserialize)]
struct FrozenWatermarksWire {
    active_generation: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
    projection_frontier: u64,
    source_frontier: u64,
    summary_frontier: u64,
}

fn map_control_error(
    error: crate::query::temporal::ports::TemporalPortError,
) -> SessionTemporalExecutionError {
    match error {
        crate::query::temporal::ports::TemporalPortError::Cancelled
        | crate::query::temporal::ports::TemporalPortError::DeadlineExceeded => {
            SessionTemporalExecutionError::Cancelled
        }
        crate::query::temporal::ports::TemporalPortError::BudgetExceeded { .. } => {
            SessionTemporalExecutionError::BudgetExhausted
        }
        crate::query::temporal::ports::TemporalPortError::ParticipantLimitExceeded { .. }
        | crate::query::temporal::ports::TemporalPortError::ParticipantManifestBytesExceeded {
            ..
        } => SessionTemporalExecutionError::BudgetExhausted,
        // An authorized root with no active generations is empty, not broken:
        // freshly ingested sessions become searchable only after an explicit
        // refresh, and reads must stay side-effect free.
        crate::query::temporal::ports::TemporalPortError::EmptyParticipantManifest => {
            SessionTemporalExecutionError::Empty
        }
        _ => SessionTemporalExecutionError::Unavailable,
    }
}

fn map_lcm_error(error: LcmError) -> SessionTemporalExecutionError {
    match error {
        LcmError::SummaryNodeNotFound
        | LcmError::PayloadNotFound
        | LcmError::PayloadMissing
        | LcmError::PayloadGcd => SessionTemporalExecutionError::Deleted,
        LcmError::PayloadNotOwnedBySession | LcmError::SummarySourceNotOwnedBySession => {
            SessionTemporalExecutionError::Denied
        }
        _ => SessionTemporalExecutionError::Unavailable,
    }
}

fn replace_summary_source_content(
    source: &mut LcmExpandedSummarySource,
    canonical_content: &str,
) -> Result<(), SessionTemporalExecutionError> {
    let total_chars = canonical_content.chars().count();
    let (offset, limit) = source
        .content_range
        .as_ref()
        .map(|range| {
            let offset = usize::try_from(range.offset)
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let limit = usize::try_from(range.limit)
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            Ok((offset, limit))
        })
        .transpose()?
        .unwrap_or((0, total_chars));
    let offset = offset.min(total_chars);
    let content = canonical_content
        .chars()
        .skip(offset)
        .take(limit)
        .collect::<String>();
    let returned_chars = content.chars().count();
    let truncated = offset > 0 || offset.saturating_add(returned_chars) < total_chars;
    source.content.clone_from(&content);
    source.content_truncated = truncated;
    if let Some(range) = source.content_range.as_mut() {
        range.offset =
            u64::try_from(offset).map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        range.limit =
            u64::try_from(limit).map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        range.returned_chars = u64::try_from(returned_chars)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        range.total_chars =
            u64::try_from(total_chars).map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        range.truncated = truncated;
    }
    if let Some(raw) = source.raw_message.as_mut() {
        raw.content.clone_from(&content);
    }
    if let Some(summary) = source.summary_node.as_deref_mut() {
        summary.summary_text = content;
    }
    Ok(())
}

fn lcm_source_cursor_sort_key(binding: &str, next_source_offset: usize) -> StableSortKey {
    StableSortKey {
        normalized_score_micros: 0,
        knowledge_at_micros: 0,
        stable_id: format!("lcm-source:{binding}:{next_source_offset}"),
    }
}

fn parse_lcm_source_cursor_offset(
    binding: &str,
    sort_key: &StableSortKey,
) -> Result<usize, SessionTemporalExecutionError> {
    if sort_key.normalized_score_micros != 0 || sort_key.knowledge_at_micros != 0 {
        return Err(SessionTemporalExecutionError::Denied);
    }
    let prefix = format!("lcm-source:{binding}:");
    let offset = sort_key
        .stable_id
        .strip_prefix(&prefix)
        .ok_or(SessionTemporalExecutionError::Denied)?;
    offset
        .parse()
        .map_err(|_| SessionTemporalExecutionError::Denied)
}

fn map_lcm_cursor_error(error: CursorError) -> SessionTemporalExecutionError {
    match error {
        CursorError::RootMismatch
        | CursorError::SessionMismatch
        | CursorError::WrongAccess
        | CursorError::TemporalModeMismatch
        | CursorError::GrainMismatch => SessionTemporalExecutionError::WrongScope,
        CursorError::Malformed
        | CursorError::Tampered
        | CursorError::FilterMismatch
        | CursorError::SortKeyMismatch => SessionTemporalExecutionError::Denied,
        CursorError::Expired
        | CursorError::UnknownOrExpiredKey
        | CursorError::WrongRequest
        | CursorError::SchemaMismatch
        | CursorError::RankingMismatch
        | CursorError::ConfigurationMismatch
        | CursorError::GenerationMismatch
        | CursorError::ParticipantManifestMismatch
        | CursorError::EpochMismatch
        | CursorError::SourceWatermarkMismatch
        | CursorError::ProjectionWatermarkMismatch
        | CursorError::IndexWatermarkMismatch
        | CursorError::SummaryWatermarkMismatch
        | CursorError::KeyIdMismatch
        | CursorError::KeyVersionMismatch
        | CursorError::KeyUnavailable
        | CursorError::InvalidKeyMaterial => SessionTemporalExecutionError::Unavailable,
    }
}
