use std::collections::BTreeSet;

use tracedecay_store::ProjectionPersistOutcome;

use super::*;

impl HostAdmissionFacade<'_> {
    pub async fn drain_projection_queue(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
        cancellation: &ObservationCancellation,
        max: usize,
    ) -> Result<HostProjectionDrainOutcome, HostAdmissionOutcome> {
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        let external_source = crate::external_source_store::RuntimeExternalSourceStore::new(
            database.runtime_client(),
        );
        let external_replay = external_source
            .drain_host_projection_replay(max, cancellation)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "external-source projection replay failed during host drain");
                HostAdmissionOutcome::retained_unavailable("external_source_projection_unavailable")
            })?;
        if cancellation.is_cancelled() {
            return Err(classify_error(&ObservationApplicationError::Cancelled));
        }
        let store = self.store(provider, scope)?;
        let mut outcome = HostProjectionDrainOutcome {
            deferred: external_replay.deferred,
            ..HostProjectionDrainOutcome::default()
        };
        let mut session_ids = BTreeSet::new();
        let mut observation_deferred = false;
        let mut observation_queue_exhausted = false;
        for _ in 0..max {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            let Some(observation_id) = store.next_queued_observation().await.map_err(|error| {
                tracing::warn!(%error, "projection store operation failed during host drain");
                projection_store_unavailable()
            })?
            else {
                observation_queue_exhausted = true;
                break;
            };
            let projected = match store.project_observation(&observation_id).await {
                Ok(projected) => projected,
                Err(ProjectionStoreError::RetryDeferred { .. }) => {
                    observation_deferred = true;
                    break;
                }
                Err(error @ ProjectionStoreError::Contract(_)) => {
                    tracing::warn!(
                        %error,
                        observation = observation_id.as_str(),
                        "deterministic projection contract rejection committed"
                    );
                    outcome.skipped = outcome.skipped.saturating_add(1);
                    continue;
                }
                Err(error) => {
                    tracing::warn!(%error, "projection store operation failed during host drain");
                    return Err(projection_error_outcome(&error));
                }
            };
            match projected {
                ProjectionPersistOutcome::Projected(projected) => {
                    outcome.projected = outcome.projected.saturating_add(1);
                    outcome.projected_outputs = outcome.projected_outputs.saturating_add(
                        u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                    );
                    if let Some(observation) = store
                        .get_observation(&observation_id)
                        .await
                        .map_err(|error| {
                            tracing::warn!(
                                %error,
                                "projection store operation failed during host drain"
                            );
                            projection_store_unavailable()
                        })?
                    {
                        session_ids.insert(
                            observation
                                .observation()
                                .source()
                                .session_id()
                                .as_str()
                                .to_owned(),
                        );
                    }
                }
                ProjectionPersistOutcome::Skipped { .. } => {
                    outcome.skipped = outcome.skipped.saturating_add(1);
                }
                ProjectionPersistOutcome::ExactDuplicate(_) => {
                    outcome.exact_duplicates = outcome.exact_duplicates.saturating_add(1);
                }
            }
        }
        if !observation_deferred && !observation_queue_exhausted {
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
            observation_deferred = store
                .next_queued_observation()
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "projection suffix probe failed during host drain");
                    projection_store_unavailable()
                })?
                .is_some();
            if cancellation.is_cancelled() {
                return Err(classify_error(&ObservationApplicationError::Cancelled));
            }
        }
        outcome.deferred |= observation_deferred;
        outcome.session_ids = session_ids.into_iter().collect();
        Ok(outcome)
    }
}
