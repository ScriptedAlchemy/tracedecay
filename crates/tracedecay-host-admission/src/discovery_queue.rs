use std::path::PathBuf;

use tracedecay_domain::ObservationScopeV1;
use tracedecay_sessions::admission::{HostAdmissionOutcome, HostDiscoveryQueueEntry};

use super::{HostAdmissionFacade, host_scope};

impl HostAdmissionFacade<'_> {
    #[hotpath::measure(label = "usecases.admission.has_session_message", future = true)]
    pub(super) async fn has_session_message(
        &self,
        scope: &ObservationScopeV1,
        provider: &str,
        message_id: &str,
    ) -> Result<bool, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        database
            .has_session_message(provider, message_id)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "registered host session-message lookup failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    #[hotpath::measure(
        label = "usecases.admission.existing_session_message_ids",
        future = true
    )]
    pub(super) async fn existing_session_message_ids(
        &self,
        scope: &ObservationScopeV1,
        provider: &str,
        message_ids: Vec<String>,
    ) -> Result<Vec<String>, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        database
            .existing_session_message_ids(provider, &message_ids)
            .await
            .map_err(|error| unavailable("read message identity batch", error))
    }

    #[hotpath::skip]
    pub(super) async fn read_session_backfill_state(
        &self,
        scope: &ObservationScopeV1,
        key: &str,
    ) -> Result<Option<String>, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        database
            .read_session_sync_journal(key)
            .await
            .map_err(|error| {
                tracing::warn!(%error, "registered host backfill-state read failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    #[hotpath::skip]
    pub(super) async fn compare_and_swap_session_backfill_state(
        &self,
        scope: &ObservationScopeV1,
        key: &str,
        expected: Option<&str>,
        replacement: &str,
    ) -> Result<bool, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        let result = match expected {
            Some(expected) => {
                database
                    .compare_and_swap_session_sync_journal(key, expected, replacement)
                    .await
            }
            None => database.insert_session_sync_journal(key, replacement).await,
        };
        result.map_err(|error| {
            tracing::warn!(%error, "registered host backfill-state CAS failed");
            HostAdmissionOutcome::registered_authority_unavailable()
        })
    }

    #[hotpath::measure(label = "usecases.admission.get_parse_offset", future = true)]
    pub(super) async fn get_parse_offset(
        &self,
        scope: &ObservationScopeV1,
        path: &str,
    ) -> Result<Option<tracedecay_global_db::ParseOffset>, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .get_parse_offset_result(path)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "registered host parse-offset read failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    #[hotpath::measure(label = "usecases.admission.advance_parse_offset", future = true)]
    pub(super) async fn advance_parse_offset(
        &self,
        scope: &ObservationScopeV1,
        path: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> Result<(), HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .advance_parse_offset_result(path, offset)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "registered host parse-offset advance failed");
                HostAdmissionOutcome::registered_authority_unavailable()
            })
    }

    pub(super) async fn replace_registered_parse_offset(
        &self,
        scope: &ObservationScopeV1,
        path: &str,
        expected: tracedecay_global_db::ParseOffset,
        next: tracedecay_global_db::ParseOffset,
    ) -> Result<(), HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .replace_parse_offset_result(path, expected, next)
            .await
            .map_err(|error| {
                tracing::warn!(?error, "registered host parse-offset replacement failed");
                if matches!(
                    error,
                    tracedecay_global_db::TranscriptPersistenceError::Conflict { .. }
                ) {
                    HostAdmissionOutcome::parse_offset_conflict()
                } else {
                    HostAdmissionOutcome::registered_authority_unavailable()
                }
            })
    }

    pub(super) async fn replace_registered_parse_offset_pair(
        &self,
        scope: &ObservationScopeV1,
        first: (
            &str,
            tracedecay_global_db::ParseOffset,
            tracedecay_global_db::ParseOffset,
        ),
        second: (
            &str,
            tracedecay_global_db::ParseOffset,
            tracedecay_global_db::ParseOffset,
        ),
    ) -> Result<(), HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let database = self
            .authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        database
            .replace_parse_offset_pair_result(first, second)
            .await
            .map_err(|error| {
                tracing::warn!(
                    ?error,
                    "registered host parse-offset pair replacement failed"
                );
                if matches!(
                    error,
                    tracedecay_global_db::TranscriptPersistenceError::PairConflict { .. }
                ) {
                    HostAdmissionOutcome::parse_offset_conflict()
                } else {
                    HostAdmissionOutcome::registered_authority_unavailable()
                }
            })
    }

    #[hotpath::measure(label = "usecases.admission.enqueue_discovery", future = true)]
    pub(super) async fn enqueue_discovery_paths(
        &self,
        scope: &ObservationScopeV1,
        provider: &str,
        paths: Vec<PathBuf>,
    ) -> Result<Option<HostDiscoveryQueueEntry>, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        database
            .enqueue_host_discovery_paths(provider, paths)
            .await
            .map(|entry| entry.map(canonical_discovery_entry))
            .map_err(|error| unavailable("enqueue", error))
    }

    #[hotpath::measure(label = "usecases.admission.discovery_paths_after", future = true)]
    pub(super) async fn discovery_paths_after(
        &self,
        scope: &ObservationScopeV1,
        provider: &str,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<HostDiscoveryQueueEntry>, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        database
            .host_discovery_paths_after(provider, after_sequence, limit)
            .await
            .map(|entries| entries.into_iter().map(canonical_discovery_entry).collect())
            .map_err(|error| unavailable("read", error))
    }

    #[hotpath::measure(label = "usecases.admission.discovery_path", future = true)]
    pub(super) async fn discovery_path(
        &self,
        scope: &ObservationScopeV1,
        provider: &str,
        sequence: u64,
    ) -> Result<Option<HostDiscoveryQueueEntry>, HostAdmissionOutcome> {
        let database = self.discovery_database(scope)?;
        database
            .host_discovery_path(provider, sequence)
            .await
            .map(|entry| entry.map(canonical_discovery_entry))
            .map_err(|error| unavailable("resolve", error))
    }

    fn discovery_database(
        &self,
        scope: &ObservationScopeV1,
    ) -> Result<&tracedecay_global_db::RegisteredGlobalDb, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        self.authorities
            .registered_database(host_scope(scope))?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)
    }
}

fn canonical_discovery_entry(
    entry: tracedecay_global_db::HostDiscoveryQueueEntry,
) -> HostDiscoveryQueueEntry {
    HostDiscoveryQueueEntry {
        sequence: entry.sequence,
        path: entry.path,
    }
}

fn unavailable(operation: &'static str, error: String) -> HostAdmissionOutcome {
    tracing::warn!(operation, %error, "registered host discovery queue access failed");
    HostAdmissionOutcome::registered_authority_unavailable()
}
