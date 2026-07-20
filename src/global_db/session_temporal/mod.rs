mod compatibility;
mod compatibility_cursor;
mod cursor_keys;
mod doctor_health;
mod hydration;
pub(crate) mod operations;
mod projection;
mod query;
mod rebuild;
mod refresh;
mod retrieval;
mod schema;

use libsql::params;
use serde::Deserialize;
use tracedecay_domain::{RetrievalAnchorId, SignedCursorKeyRefV1};

use crate::application::session::{
    AuthorizedTemporalExecutionRequest, SessionDataFreshness, SessionTemporalExecutionError,
    SessionTemporalExecutionPort, SessionTemporalExecutionReport, TemporalExecutionFuture,
};
use crate::global_db::{GlobalDb, GlobalDbReadSnapshot};
use crate::query::temporal::context::VersionedTokenEstimator;
use crate::query::temporal::execute_temporal_kernel;
use crate::query::temporal::ports::{
    BindingDigest, KernelVersions, MAX_TEMPORAL_PARTICIPANTS, TemporalExecutionSnapshot,
    TemporalParticipantGeneration, TemporalParticipantManifest, TemporalRetrievalScope,
    TemporalSourceAccess, TemporalWatermarks,
};
use crate::query::temporal::resolution::ValidatedAuthorization;
use crate::sessions::SessionMessageRecord;

use self::cursor_keys::GlobalDbCursorKeyProvider;
use self::hydration::GlobalDbTemporalHydrationPort;
use self::retrieval::GlobalDbTemporalReadPort;

pub(crate) use compatibility::{
    AuthorizedSessionDescribeRequest, AuthorizedSessionDescribeResult,
    AuthorizedSessionExpandRequest, AuthorizedSessionExpandResult, CompatibilityReadError,
    CompatibilityTemporalMetadata,
};
pub(crate) use compatibility_cursor::{
    AuthorizedSessionExpandCursorBinding, CompatibilityCursorError,
};

// Consumed by the pr8/transport cold-Doctor route when that branch is integrated.
#[allow(unused_imports)]
pub(crate) use doctor_health::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
    session_temporal_doctor_health_at,
};
pub(in crate::global_db) use projection::record_canonical_observation_effect;
pub use refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
pub(crate) use schema::ensure_session_temporal_schema;

/// Production temporal executor over one already-open authoritative global DB.
pub struct GlobalDbSessionTemporalExecution<'db> {
    db: &'db GlobalDb,
}

impl<'db> GlobalDbSessionTemporalExecution<'db> {
    pub const fn new(db: &'db GlobalDb) -> Self {
        Self { db }
    }

    pub(crate) async fn hydrate_authorized_occurrence(
        &self,
        snapshot: &TemporalExecutionSnapshot,
        anchor_id: &RetrievalAnchorId,
    ) -> Result<SessionMessageRecord, SessionTemporalExecutionError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        hydration::hydrate_authorized_occurrence(&read, &self.db.storage_root, snapshot, anchor_id)
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)
    }

    pub(crate) async fn encode_expand_cursor(
        &self,
        binding: AuthorizedSessionExpandCursorBinding,
        source_offset: usize,
    ) -> Result<String, CompatibilityCursorError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| CompatibilityCursorError::Unavailable)?;
        compatibility_cursor::encode_expand_cursor(&read, binding, source_offset).await
    }

    pub(crate) async fn decode_expand_cursor(
        &self,
        binding: &AuthorizedSessionExpandCursorBinding,
        encoded: &str,
    ) -> Result<usize, CompatibilityCursorError> {
        let read = self
            .db
            .read_snapshot()
            .await
            .map_err(|_| CompatibilityCursorError::Unavailable)?;
        compatibility_cursor::decode_expand_cursor(&read, binding, encoded).await
    }

    pub(crate) async fn describe_compatible(
        &self,
        request: AuthorizedSessionDescribeRequest,
    ) -> Result<AuthorizedSessionDescribeResult, CompatibilityReadError> {
        compatibility::describe_authorized(self.db, request).await
    }

    pub(crate) async fn expand_compatible(
        &self,
        request: AuthorizedSessionExpandRequest,
    ) -> Result<AuthorizedSessionExpandResult, CompatibilityReadError> {
        compatibility::expand_authorized(self.db, request).await
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
            Ok(SessionTemporalExecutionReport::new(
                result,
                SessionDataFreshness::Fresh,
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
        _ => SessionTemporalExecutionError::Unavailable,
    }
}
