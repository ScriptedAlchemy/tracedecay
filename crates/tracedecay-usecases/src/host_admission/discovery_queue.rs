use std::path::PathBuf;

use tracedecay_domain::ObservationScopeV1;
use tracedecay_sessions::admission::HostDiscoveryQueueEntry;

use super::{HostAdmissionFacade, HostAdmissionOutcome, host_scope};

impl HostAdmissionFacade<'_> {
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
