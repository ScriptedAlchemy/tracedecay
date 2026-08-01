mod cursor_keys;
mod direct;
mod doctor_health;
pub mod execution;
mod expand;
mod hydration;
pub mod operations;
mod projection;
mod query;
mod rebuild;
mod refresh;
/// LCM compatibility rendering over one frozen registered-store snapshot. The
/// DB-free shaping it applies is owned by [`self::render`].
mod registered_lcm_render;
pub mod render;
mod retrieval;
mod schema;
mod sql;
pub mod store;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use tracedecay_domain::{HydrationStateV1, RetrievalAnchorId, SessionId, SignedCursorKeyRefV1};
use tracedecay_runtime_core::db::engine::params;

use self::execution::{
    AuthorizedTemporalExecutionRequest, SessionDataFreshness, SessionTemporalExecutionError,
    SessionTemporalExecutionPort, SessionTemporalExecutionReport, TemporalExecutionFuture,
};
use self::render::{CanonicalLcmSourceHydration, apply_canonical_summary_source_content};
use crate::RegisteredGlobalDb;
use tracedecay_sessions::lcm::contracts::{
    LcmContentSlice, LcmDescribeRequest, LcmDescribeResponse, LcmDescribeTarget, LcmError,
    LcmExpandRequest, LcmExpandResponse, LcmExpandTarget, LcmSourceRef,
};
use tracedecay_store::SessionMessageRecord;
use tracedecay_temporal_query::context::VersionedTokenEstimator;
use tracedecay_temporal_query::cursor::{CursorError, StableSortKey, encode_cursor, verify_cursor};
use tracedecay_temporal_query::execute_temporal_kernel;
use tracedecay_temporal_query::hydration::hydrate_selected;
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, MAX_TEMPORAL_PARTICIPANTS, TemporalAuthorizedRoot,
    TemporalExecutionSnapshot, TemporalParticipantAuthorization, TemporalParticipantGeneration,
    TemporalParticipantManifest, TemporalRetrievalScope, TemporalSourceAccess, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

pub use self::cursor_keys::GlobalDbCursorKeyProvider;
pub use self::direct::ResolvedDirectAnchor;
use self::hydration::GlobalDbTemporalHydrationPort;
use self::retrieval::GlobalDbTemporalReadPort;
use self::sql::TemporalSqlRead;

// Consumed by the pr8/transport cold-Doctor route when that branch is integrated.
#[allow(unused_imports)]
pub use doctor_health::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
    session_temporal_doctor_health_at,
};
pub use projection::record_canonical_observation_effect;
pub use refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
pub use schema::{ensure_session_temporal_schema, repair_session_temporal_state};
pub use store::GlobalDbSessionTemporalStore;

impl RegisteredGlobalDb {
    pub async fn ensure_active_session_cursor_key_result(
        &self,
    ) -> tracedecay_store::SessionStoreResult<SignedCursorKeyRefV1> {
        const OPERATION: &str = "provision registered session cursor authentication key";
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| query::storage(OPERATION, error))?;
        let key =
            cursor_keys::ensure_active_session_cursor_key_in_transaction(&transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|error| query::storage(OPERATION, error))?;
        Ok(key)
    }

    pub async fn load_session_cursor_key_provider_result(
        &self,
    ) -> Result<GlobalDbCursorKeyProvider, cursor_keys::GlobalDbCursorKeyProviderError> {
        let key = self
            .ensure_active_session_cursor_key_result()
            .await
            .map_err(|source| cursor_keys::GlobalDbCursorKeyProviderError::Provision { source })?;
        let read = self.read_snapshot().await.map_err(|source| {
            cursor_keys::GlobalDbCursorKeyProviderError::Storage {
                operation: "load registered session cursor authentication key",
                source,
            }
        })?;
        GlobalDbCursorKeyProvider::from_registered_key_ref(&read, key).await
    }
}

/// Transitional PR8 rendering adapter over one registry-owned session shard.
pub struct RegisteredGlobalDbSessionTemporalExecution<'db> {
    db: &'db RegisteredGlobalDb,
}

impl<'db> RegisteredGlobalDbSessionTemporalExecution<'db> {
    pub const fn new(db: &'db RegisteredGlobalDb) -> Self {
        Self { db }
    }

    pub async fn session_message_from_hydrated_occurrence(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
        provider: &str,
        session_id: &str,
        content: &[u8],
    ) -> Result<SessionMessageRecord, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        hydration::session_message_from_hydrated_bytes(
            &TemporalSqlRead::registered(&read),
            snapshot,
            anchor_id,
            provider,
            session_id,
            content,
        )
        .await
        .map_err(|_| SessionTemporalExecutionError::Unavailable)
    }

    pub async fn resolve_lcm_describe_target(
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
        direct::resolve_describe_target(
            &TemporalSqlRead::registered(&read),
            provider,
            session_id,
            target,
        )
        .await
    }

    pub async fn resolve_lcm_expand_target(
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
        direct::resolve_expand_target(
            &TemporalSqlRead::registered(&read),
            provider,
            session_id,
            target,
        )
        .await
    }

    pub async fn render_lcm_describe(
        &self,
        request: LcmDescribeRequest,
    ) -> Result<LcmDescribeResponse, SessionTemporalExecutionError> {
        let snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        registered_lcm_render::describe(&snapshot, request)
            .await
            .map_err(map_lcm_error)
    }

    pub async fn render_lcm_expand(
        &self,
        request: LcmExpandRequest,
        canonical_content: &str,
    ) -> Result<LcmExpandResponse, SessionTemporalExecutionError> {
        let snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        registered_lcm_render::expand(&snapshot, request, canonical_content)
            .await
            .map_err(map_lcm_error)
    }

    pub async fn hydrate_lcm_summary_sources(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        provider: &str,
        session_id: &SessionId,
        slice: LcmContentSlice,
        expansion: &mut LcmExpandResponse,
    ) -> Result<(), SessionTemporalExecutionError> {
        if expansion.summary_sources.is_empty() {
            return Ok(());
        }
        let read_snapshot = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let read = TemporalSqlRead::registered(&read_snapshot);
        let mut resolutions = Vec::with_capacity(expansion.summary_sources.len());
        let mut anchors = Vec::with_capacity(expansion.summary_sources.len());
        for source in &expansion.summary_sources {
            let target = match &source.source_ref {
                LcmSourceRef::RawMessage { store_id } => LcmExpandTarget::RawMessage {
                    store_id: *store_id,
                },
                LcmSourceRef::SummaryNode { node_id } => LcmExpandTarget::SummaryNode {
                    node_id: node_id.clone(),
                },
            };
            let resolution =
                direct::resolve_expand_target(&read, provider, session_id, &target).await;
            match resolution {
                Ok(resolved) if &resolved.owner_session_id == session_id => {
                    anchors.push(resolved.anchor_id.clone());
                    resolutions.push(Ok(resolved.anchor_id));
                }
                Ok(_)
                | Err(
                    SessionTemporalExecutionError::Denied
                    | SessionTemporalExecutionError::WrongScope,
                ) => {
                    resolutions.push(Err(HydrationStateV1::Unauthorized));
                }
                Err(SessionTemporalExecutionError::Deleted) => {
                    resolutions.push(Err(HydrationStateV1::Deleted));
                }
                Err(SessionTemporalExecutionError::Redacted) => {
                    resolutions.push(Err(HydrationStateV1::Redacted));
                }
                Err(SessionTemporalExecutionError::Locked) => {
                    resolutions.push(Err(HydrationStateV1::Locked));
                }
                Err(SessionTemporalExecutionError::BudgetExhausted) => {
                    return Err(SessionTemporalExecutionError::BudgetExhausted);
                }
                Err(SessionTemporalExecutionError::Cancelled) => {
                    return Err(SessionTemporalExecutionError::Cancelled);
                }
                Err(_) => {
                    resolutions.push(Err(HydrationStateV1::RetainedButUnavailable));
                }
            }
        }
        let storage_root = self
            .db
            .db_path()
            .parent()
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let authority =
            GlobalDbTemporalHydrationPort::for_registered_snapshot(&read_snapshot, storage_root);
        let batch = hydrate_selected(&authority, snapshot, &anchors)
            .await
            .map_err(|error| {
                SessionTemporalExecutionError::Kernel(
                    tracedecay_temporal_query::TemporalKernelError::Hydration(error),
                )
            })?;
        let available = batch
            .available
            .iter()
            .filter_map(|payload| {
                String::from_utf8(payload.bytes().to_vec())
                    .ok()
                    .map(|content| (payload.anchor_id().clone(), content))
            })
            .collect::<BTreeMap<_, _>>();
        let unavailable = batch
            .unavailable
            .iter()
            .map(|denial| (denial.anchor_id().clone(), denial.state()))
            .collect::<BTreeMap<_, _>>();
        let hydration = expansion
            .summary_sources
            .iter()
            .zip(resolutions)
            .map(|(source, resolution)| {
                let (state, content) = match resolution {
                    Ok(anchor_id) => {
                        if let Some(content) = available.get(&anchor_id) {
                            (HydrationStateV1::Available, Some(content.clone()))
                        } else {
                            (
                                unavailable
                                    .get(&anchor_id)
                                    .copied()
                                    .unwrap_or(HydrationStateV1::RetainedButUnavailable),
                                None,
                            )
                        }
                    }
                    Err(state) => (state, None),
                };
                CanonicalLcmSourceHydration {
                    source_ref: source.source_ref.clone(),
                    state,
                    content,
                }
            })
            .collect::<Vec<_>>();
        apply_canonical_summary_source_content(expansion, slice, &hydration)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)
    }

    pub async fn encode_lcm_source_cursor(
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
        let authenticator = GlobalDbCursorKeyProvider::from_registered_snapshot(&read, snapshot)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        encode_cursor(
            snapshot,
            &lcm_source_cursor_sort_key(binding, next_source_offset),
            &authenticator,
        )
        .map_err(map_lcm_cursor_error)
    }

    pub async fn decode_lcm_source_cursor(
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
        let authenticator = GlobalDbCursorKeyProvider::from_registered_snapshot(&read, snapshot)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let sort_key =
            verify_cursor(encoded, snapshot, &authenticator).map_err(map_lcm_cursor_error)?;
        parse_lcm_source_cursor_offset(binding, &sort_key)
    }

    async fn freeze(
        &self,
        request: &AuthorizedTemporalExecutionRequest,
    ) -> Result<
        (
            tracedecay_runtime_core::db::engine::ReadSnapshot,
            TemporalExecutionSnapshot,
        ),
        SessionTemporalExecutionError,
    > {
        let control = request.snapshot_request().execution_control();
        control.checkpoint().map_err(map_control_error)?;
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let (participants, watermarks, cursor_key) =
            freeze_participants(&TemporalSqlRead::registered(&read), request).await?;
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

/// Decides what a request is actually allowed to see of one participant source.
///
/// This used to be the literal `Authorized`, so every source claimed authority
/// regardless of which project owned it and the manifest could not express
/// anything else. The session-scoped query in particular does not filter on
/// `project_key`, so a session belonging to another project reaches this point
/// and must be denied here.
///
/// An absent authorized root is a missing authority, not a permissive one, so
/// it denies rather than reporting the source as merely unavailable.
fn participant_authorization(
    authorized_root: Option<&TemporalAuthorizedRoot>,
    participant_project_key: &str,
) -> TemporalParticipantAuthorization {
    match authorized_root {
        Some(root) if root.project_key() == participant_project_key => {
            TemporalParticipantAuthorization::Authorized
        }
        _ => TemporalParticipantAuthorization::Denied,
    }
}

fn participant_source_access(
    metadata_json: Option<&str>,
    now: i64,
) -> Option<TemporalSourceAccess> {
    let metadata = match metadata_json {
        Some(encoded) => serde_json::from_str::<Value>(encoded).ok()?,
        None => Value::Null,
    };
    if metadata
        .get("retention_expires_at")
        .and_then(Value::as_i64)
        .is_some_and(|expires_at| expires_at <= now)
    {
        return Some(TemporalSourceAccess::RetentionWithheld);
    }
    let state = [
        "source_access",
        "payload_access",
        "hydration_state",
        "availability",
    ]
    .iter()
    .find_map(|key| metadata.get(*key).and_then(Value::as_str));
    match state {
        None | Some("authorized" | "available" | "eligible") => {
            Some(TemporalSourceAccess::Available)
        }
        Some("locked" | "quarantined") => Some(TemporalSourceAccess::Locked),
        Some("retention_withheld" | "retention_expired") => {
            Some(TemporalSourceAccess::RetentionWithheld)
        }
        Some("deleted") => Some(TemporalSourceAccess::Deleted),
        Some("redacted") => Some(TemporalSourceAccess::Redacted),
        Some("unavailable") => Some(TemporalSourceAccess::Unavailable),
        Some(_) => None,
    }
}

async fn freeze_participants(
    read: &TemporalSqlRead<'_>,
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
                        generation.frozen_watermarks_json, source.project_key,
                        source.metadata_json, unixepoch()
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
                        generation.frozen_watermarks_json, source.project_key,
                        source.metadata_json, unixepoch()
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
        let participant_project_key = row
            .get::<String>(4)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let participant_metadata = row
            .get::<Option<String>>(5)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let snapshot_time = row
            .get::<i64>(6)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let mut authorization =
            participant_authorization(snapshot_request.authorized_root(), &participant_project_key);
        let access = participant_source_access(participant_metadata.as_deref(), snapshot_time)
            .unwrap_or_else(|| {
                authorization = TemporalParticipantAuthorization::Denied;
                TemporalSourceAccess::Available
            });
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
                authorization,
                access,
            )
            .map_err(map_control_error)?,
        );
    }
    drop(rows);
    if entries.is_empty() {
        return if authorized_scope_has_sources(read, request).await? {
            Err(SessionTemporalExecutionError::Unavailable)
        } else {
            Err(SessionTemporalExecutionError::Empty {
                freshness: SessionDataFreshness::Fresh,
            })
        };
    }
    let participants = TemporalParticipantManifest::new(entries).map_err(map_control_error)?;
    Ok((participants, aggregate, shared_cursor_key.flatten()))
}

async fn authorized_scope_has_sources(
    read: &TemporalSqlRead<'_>,
    request: &AuthorizedTemporalExecutionRequest,
) -> Result<bool, SessionTemporalExecutionError> {
    let snapshot_request = request.snapshot_request();
    let provider = snapshot_request.provider_scope();
    let project_key = snapshot_request
        .authorized_root()
        .ok_or(SessionTemporalExecutionError::WrongScope)?
        .project_key();
    let mut rows = match snapshot_request.retrieval_scope() {
        TemporalRetrievalScope::Session(session_id) => {
            read.query(
                "SELECT 1
                 FROM sessions
                 WHERE session_id = ?1
                   AND project_key = ?2
                   AND (?3 IS NULL OR provider = ?3)
                 LIMIT 1",
                params![session_id.as_str(), project_key, provider],
            )
            .await
        }
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            read.query(
                "SELECT 1
                 FROM sessions
                 WHERE project_key = ?1
                   AND (?2 IS NULL OR provider = ?2)
                 LIMIT 1",
                params![project_key, provider],
            )
            .await
        }
    }
    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|_| SessionTemporalExecutionError::Unavailable)
}

impl SessionTemporalExecutionPort for RegisteredGlobalDbSessionTemporalExecution<'_> {
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
            let authenticator =
                GlobalDbCursorKeyProvider::from_registered_snapshot(&read_snapshot, &snapshot)
                    .await
                    .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
            let storage_root = self
                .db
                .db_path()
                .parent()
                .ok_or(SessionTemporalExecutionError::Unavailable)?;
            let kernel_request = request.into_kernel_request(snapshot);
            let read = GlobalDbTemporalReadPort::new_registered(&read_snapshot);
            let hydration = GlobalDbTemporalHydrationPort::for_registered_snapshot(
                &read_snapshot,
                storage_root,
            );
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
    error: tracedecay_temporal_query::ports::TemporalPortError,
) -> SessionTemporalExecutionError {
    match error {
        tracedecay_temporal_query::ports::TemporalPortError::Cancelled
        | tracedecay_temporal_query::ports::TemporalPortError::DeadlineExceeded => {
            SessionTemporalExecutionError::Cancelled
        }
        tracedecay_temporal_query::ports::TemporalPortError::BudgetExceeded { .. } => {
            SessionTemporalExecutionError::BudgetExhausted
        }
        error @ (tracedecay_temporal_query::ports::TemporalPortError::ParticipantLimitExceeded {
            ..
        } | tracedecay_temporal_query::ports::TemporalPortError::ParticipantManifestBytesExceeded {
            ..
        }) => SessionTemporalExecutionError::Kernel(
            tracedecay_temporal_query::TemporalKernelError::Port(error),
        ),
        // The caller distinguishes a genuinely source-free root from sources
        // that exist but have not published a searchable generation.
        tracedecay_temporal_query::ports::TemporalPortError::EmptyParticipantManifest => {
            SessionTemporalExecutionError::Unavailable
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
        | CursorError::WrongRequest
        | CursorError::FilterMismatch
        | CursorError::SortKeyMismatch => SessionTemporalExecutionError::Denied,
        CursorError::Expired
        | CursorError::UnknownOrExpiredKey
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

#[cfg(test)]
mod participant_access_tests {
    use super::*;

    fn root(project_id: Option<&str>) -> TemporalAuthorizedRoot {
        match project_id {
            Some(project_id) => {
                TemporalAuthorizedRoot::project("profile", project_id, "store", "root")
            }
            None => TemporalAuthorizedRoot::profile("profile", "store", "root"),
        }
        .expect("valid authorized root")
    }

    #[test]
    fn a_source_owned_by_the_authorized_project_is_authorized() {
        assert_eq!(
            participant_authorization(Some(&root(Some("proj_a"))), "proj_a"),
            TemporalParticipantAuthorization::Authorized
        );
    }

    #[test]
    fn a_source_owned_by_another_project_is_denied() {
        // The session-scoped participant query does not filter on project_key,
        // so this row really does reach the manifest builder.
        assert_eq!(
            participant_authorization(Some(&root(Some("proj_a"))), "proj_b"),
            TemporalParticipantAuthorization::Denied
        );
    }

    #[test]
    fn a_profile_root_does_not_authorize_project_owned_sources() {
        assert_eq!(
            participant_authorization(Some(&root(None)), "proj_a"),
            TemporalParticipantAuthorization::Denied
        );
        assert_eq!(
            participant_authorization(Some(&root(None)), "user"),
            TemporalParticipantAuthorization::Authorized
        );
    }

    #[test]
    fn a_missing_authorized_root_denies_rather_than_permits() {
        assert_eq!(
            participant_authorization(None, "proj_a"),
            TemporalParticipantAuthorization::Denied
        );
    }

    #[test]
    fn persisted_source_lifecycle_states_are_preserved() {
        for (metadata, expected) in [
            (
                r#"{"payload_access":"quarantined"}"#,
                TemporalSourceAccess::Locked,
            ),
            (
                r#"{"payload_access":"retention_expired"}"#,
                TemporalSourceAccess::RetentionWithheld,
            ),
            (
                r#"{"payload_access":"deleted"}"#,
                TemporalSourceAccess::Deleted,
            ),
            (
                r#"{"payload_access":"redacted"}"#,
                TemporalSourceAccess::Redacted,
            ),
            (
                r#"{"payload_access":"unavailable"}"#,
                TemporalSourceAccess::Unavailable,
            ),
        ] {
            assert_eq!(
                participant_source_access(Some(metadata), 100),
                Some(expected)
            );
        }
    }

    #[test]
    fn expired_source_retention_is_withheld_at_snapshot_time() {
        assert_eq!(
            participant_source_access(Some(r#"{"retention_expires_at":99}"#), 100),
            Some(TemporalSourceAccess::RetentionWithheld)
        );
    }

    #[test]
    fn invalid_or_ambiguous_source_access_never_becomes_unavailable() {
        assert_eq!(
            participant_source_access(Some(r#"{"payload_access":"ambiguous"}"#), 100),
            None
        );
        assert_eq!(participant_source_access(Some("{"), 100), None);
    }
}

#[cfg(test)]
mod cursor_access_tests {
    use super::*;

    #[test]
    fn request_rebinding_is_denied_while_missing_key_authority_is_unavailable() {
        assert!(matches!(
            map_lcm_cursor_error(CursorError::WrongRequest),
            SessionTemporalExecutionError::Denied
        ));
        assert!(matches!(
            map_lcm_cursor_error(CursorError::KeyUnavailable),
            SessionTemporalExecutionError::Unavailable
        ));
    }
}
