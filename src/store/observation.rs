use tracedecay_domain::{
    CanonicalObservationIdV1, ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ObservationScopeV1,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationPersistOutcome, ObservationProjectionStore, ObservationReplayRequest,
    ObservationStore, ObservationStoreResult, ObservationWrite, ProjectionCheckpoint,
    ProjectionPersistOutcome, ProjectionRebuildOutcome, ProjectionStoreResult, StoredObservation,
};

use crate::global_db::GlobalDb;

/// Observation-store adapter over an already-open authoritative [`GlobalDb`].
pub struct GlobalDbObservationStore<'a> {
    db: &'a GlobalDb,
}

impl<'a> GlobalDbObservationStore<'a> {
    pub const fn new(db: &'a GlobalDb) -> Self {
        Self { db }
    }
}

impl ObservationStore for GlobalDbObservationStore<'_> {
    async fn persist_observation(
        &self,
        write: ObservationWrite,
    ) -> ObservationStoreResult<ObservationPersistOutcome> {
        self.db.persist_observation_result(write).await
    }

    async fn get_source_cursor(
        &self,
        source: &ClaudeSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> ObservationStoreResult<Option<ClaudeSourceCursorV1>> {
        self.db
            .get_observation_source_cursor_result(source, scope)
            .await
    }

    async fn advance_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
    ) -> ObservationStoreResult<CursorAdvanceOutcome> {
        self.db
            .advance_observation_source_cursor_result(advance)
            .await
    }

    async fn get_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ObservationStoreResult<Option<StoredObservation>> {
        self.db.get_observation_result(observation_id).await
    }

    async fn replay_observations(
        &self,
        request: ObservationReplayRequest,
    ) -> ObservationStoreResult<Vec<StoredObservation>> {
        self.db.replay_observations_result(request).await
    }
}

impl ObservationProjectionStore for GlobalDbObservationStore<'_> {
    async fn project_observation(
        &self,
        observation_id: &CanonicalObservationIdV1,
    ) -> ProjectionStoreResult<ProjectionPersistOutcome> {
        self.db.project_observation_result(observation_id).await
    }

    async fn projection_checkpoint(&self) -> ProjectionStoreResult<ProjectionCheckpoint> {
        self.db.projection_checkpoint_result().await
    }

    async fn rebuild_projection(
        &self,
        frontier_sequence: u64,
    ) -> ProjectionStoreResult<ProjectionRebuildOutcome> {
        self.db.rebuild_projection_result(frontier_sequence).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_contains_only_the_borrowed_global_db_handle() {
        fn assert_exact_fields(store: &GlobalDbObservationStore<'_>) {
            let GlobalDbObservationStore { db: _ } = store;
        }

        let _ = assert_exact_fields;
        assert_eq!(
            std::mem::size_of::<GlobalDbObservationStore<'static>>(),
            std::mem::size_of::<&'static GlobalDb>()
        );
    }
}
