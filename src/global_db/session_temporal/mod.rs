mod compatibility;
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
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalWatermarks,
};
use crate::query::temporal::resolution::ValidatedAuthorization;
use crate::sessions::SessionMessageRecord;

use self::cursor_keys::GlobalDbCursorKeyProvider;
use self::hydration::GlobalDbTemporalHydrationPort;
use self::retrieval::GlobalDbTemporalReadPort;

pub(crate) use compatibility::{
    AuthorizedSessionDescribeRequest, AuthorizedSessionDescribeResult,
    AuthorizedSessionExpandCursorBinding, AuthorizedSessionExpandRequest,
    AuthorizedSessionExpandResult, CompatibilityCursorError, CompatibilityReadError,
    CompatibilityTemporalMetadata,
};
pub(crate) use doctor_health::{
    SessionTemporalHealthFindingKind, SessionTemporalHealthReport, SessionTemporalHealthStatus,
};
pub(in crate::global_db) use projection::record_canonical_observation_effect;
pub use refresh::{SessionRefreshRecoveryV1, SessionRefreshRestartStateV1};
pub(super) use schema::ensure_session_temporal_schema;

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
        compatibility::encode_expand_cursor(&read, binding, source_offset).await
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
        compatibility::decode_expand_cursor(&read, binding, encoded).await
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
        let mut rows = read
            .query(
                "SELECT generation, frozen_watermarks_json
                 FROM session_temporal_generations
                 WHERE session_id = ?1 AND state = 'active'
                 ORDER BY generation DESC
                 LIMIT 1",
                [request.snapshot_request().session_id().as_str()],
            )
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let row = rows
            .next()
            .await
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?
            .ok_or(SessionTemporalExecutionError::Unavailable)?;
        let generation_value = row
            .get::<i64>(0)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let generation = u64::try_from(generation_value)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let encoded = row
            .get::<String>(1)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        let frozen: FrozenWatermarksWire = serde_json::from_str(&encoded)
            .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        drop(rows);
        if frozen.active_generation > generation {
            return Err(SessionTemporalExecutionError::Unavailable);
        }
        control.checkpoint().map_err(map_control_error)?;
        let snapshot = TemporalExecutionSnapshot::new_authorized(
            request.snapshot_request().clone(),
            TemporalWatermarks {
                generation,
                source: frozen.source_frontier,
                projection: frozen.projection_frontier,
                // FTS candidates commit atomically with projection batches.
                index: frozen.projection_frontier,
                summary: frozen.summary_frontier,
            },
            KernelVersions {
                schema: request.schema_version(),
                ranking: request.ranking_version(),
                configuration_digest: BindingDigest::new(
                    "configuration_digest",
                    request.configuration_digest(),
                )
                .map_err(|_| SessionTemporalExecutionError::Unavailable)?,
            },
            frozen.cursor_key,
            ValidatedAuthorization::Authorized,
        )
        .map_err(|_| SessionTemporalExecutionError::Unavailable)?;
        Ok((read, snapshot))
    }
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
        _ => SessionTemporalExecutionError::Unavailable,
    }
}
