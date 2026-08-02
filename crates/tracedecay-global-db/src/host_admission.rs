//! Canonical host admission over one or more registered session stores.

use std::collections::BTreeSet;

use tracedecay_domain::{
    BrainId, ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1, ProjectId,
    UserProfileId,
};
use tracedecay_runtime_core::privacy::RecordSanitizerV1;
use tracedecay_sessions::admission::{
    AdmissionFuture, HostAdmission, HostAdmissionOutcome, HostAdmissionScope, HostAdmissionStatus,
    HostProjectionDrainOutcome,
};
use tracedecay_sessions::observation::{
    AdvanceNonDurableSourceCursorRequest, CaptureObservationOutcome, CaptureObservationRequest,
    ObservationApplication, ObservationApplicationError, ObservationCancellation,
};
use tracedecay_sessions::repository_provenance::RepositoryProvenanceAdmissionContext;
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{
    ObservationProjectionStore, ObservationStore, ObservationStoreError, ParseOffset,
    ProjectionPersistOutcome, StoreShardScopeV1,
};

use crate::external_source_store::RuntimeExternalSourceStore;
use crate::{GlobalDbObservationStore, RegisteredGlobalDb};

#[derive(Clone, Default)]
pub struct HostAdmissionAuthorities<'a> {
    project_id: Option<ProjectId>,
    project_registered: Option<&'a RegisteredGlobalDb>,
    brain_id: Option<BrainId>,
    profile_id: Option<UserProfileId>,
    profile_registered: Option<&'a RegisteredGlobalDb>,
    repository_provenance: Option<RepositoryProvenanceAdmissionContext>,
}

impl<'a> HostAdmissionAuthorities<'a> {
    pub fn registered_for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            project_id: Some(project_id),
            project_registered: Some(registered),
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: None,
            repository_provenance: None,
        }
    }

    pub fn registered_for_profile(
        brain_id: BrainId,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self {
            project_id: None,
            project_registered: None,
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            profile_registered: Some(registered),
            repository_provenance: None,
        }
    }

    pub fn for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self::registered_for_project(brain_id, profile_id, project_id, registered)
    }

    pub fn for_profile(
        brain_id: BrainId,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        Self::registered_for_profile(brain_id, profile_id, registered)
    }

    #[must_use]
    pub fn with_profile_registered(
        mut self,
        profile_id: UserProfileId,
        registered: &'a RegisteredGlobalDb,
    ) -> Self {
        self.profile_id = Some(profile_id);
        self.profile_registered = Some(registered);
        self
    }

    pub fn unregistered_for_project(project_id: ProjectId) -> Self {
        Self {
            project_id: Some(project_id),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn unregistered_for_profile() -> Self {
        Self::default()
    }

    pub fn unavailable_for_project(
        brain_id: BrainId,
        profile_id: UserProfileId,
        project_id: ProjectId,
    ) -> Self {
        Self {
            project_id: Some(project_id),
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            ..Self::default()
        }
    }

    pub fn unavailable_for_profile(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self {
            brain_id: Some(brain_id),
            profile_id: Some(profile_id),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_repository_provenance(
        mut self,
        repository_provenance: RepositoryProvenanceAdmissionContext,
    ) -> Self {
        self.repository_provenance = Some(repository_provenance);
        self
    }

    pub fn registered_database(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<Option<&'a RegisteredGlobalDb>, HostAdmissionOutcome> {
        let database = match scope {
            HostAdmissionScope::Project => self.project_registered,
            HostAdmissionScope::Profile => self.profile_registered,
        };
        let Some(database) = database else {
            return Ok(None);
        };
        let shard = &database.binding().shard_id;
        let profile_matches = self.brain_id.as_ref() == Some(&shard.brain_id)
            && self.profile_id.as_ref() == Some(&shard.profile_id);
        let valid = profile_matches
            && match (scope, &shard.scope) {
                (
                    HostAdmissionScope::Project,
                    StoreShardScopeV1::ProjectSessions { project_id },
                ) => self.project_id.as_ref() == Some(project_id),
                (HostAdmissionScope::Profile, StoreShardScopeV1::ProfileSessions) => true,
                _ => false,
            };
        if valid {
            Ok(Some(database))
        } else {
            Err(HostAdmissionOutcome::project_authority_mismatch())
        }
    }

    pub fn validate_scope(&self, scope: &ObservationScopeV1) -> Result<(), HostAdmissionOutcome> {
        let ObservationScopeV1::Project { project_id } = scope else {
            return Ok(());
        };
        match self.project_id.as_ref() {
            Some(expected) if expected == project_id => Ok(()),
            Some(_) => Err(HostAdmissionOutcome::project_authority_mismatch()),
            None => Err(HostAdmissionOutcome::project_authority_unbound()),
        }
    }
}

pub struct HostAdmissionFacade<'a> {
    authorities: HostAdmissionAuthorities<'a>,
}

impl<'a> HostAdmissionFacade<'a> {
    pub const fn new(authorities: HostAdmissionAuthorities<'a>) -> Self {
        Self { authorities }
    }

    pub const fn authorities(&self) -> &HostAdmissionAuthorities<'a> {
        &self.authorities
    }

    pub fn probe(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        if !supported_provider(provider) {
            return outcome(
                HostAdmissionStatus::Unknown,
                false,
                Some("unknown_provider"),
            );
        }
        if scope == HostAdmissionScope::Project && self.authorities.project_id.is_none() {
            return HostAdmissionOutcome::project_authority_unbound();
        }
        match self.authorities.registered_database(scope) {
            Ok(Some(_)) => HostAdmissionOutcome::supported(),
            Ok(None) => HostAdmissionOutcome::registered_authority_unavailable(),
            Err(outcome) => outcome,
        }
    }

    pub fn accept_replay(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        let probe = self.probe(provider, scope);
        if probe.status == HostAdmissionStatus::Supported {
            HostAdmissionOutcome::accepted_for_replay()
        } else {
            probe
        }
    }

    fn application(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<ObservationApplication<GlobalDbObservationStore<'a>>, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let sanitizer = RecordSanitizerV1::observation_v1().map_err(|_| {
            outcome(
                HostAdmissionStatus::Unavailable,
                false,
                Some("sanitizer_unavailable"),
            )
        })?;
        Ok(ObservationApplication::new(store, sanitizer))
    }

    fn store(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<GlobalDbObservationStore<'a>, HostAdmissionOutcome> {
        self.authorities.validate_scope(scope)?;
        let admission_scope = host_scope(scope);
        let probe = self.probe(provider, admission_scope);
        if probe.status != HostAdmissionStatus::Supported {
            return Err(probe);
        }
        let database = self
            .authorities
            .registered_database(admission_scope)?
            .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
        Ok(GlobalDbObservationStore::with_runtime(
            database.runtime(),
            database.authority(),
        ))
    }
}

impl HostAdmission for HostAdmissionFacade<'_> {
    fn capture_observation<'a>(
        &'a self,
        request: CaptureObservationRequest,
    ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
        Box::pin(async move {
            let provider = request.provider().to_owned();
            let scope = request.scope().clone();
            self.authorities.validate_scope(&scope)?;
            let database = self
                .authorities
                .registered_database(host_scope(&scope))?
                .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
            let application = self.application(&provider, &scope)?;
            let captured = application
                .capture_observation(
                    request
                        .with_repository_provenance(self.authorities.repository_provenance.clone()),
                )
                .await
                .map_err(|error| classify_error(&error))?;
            if let CaptureObservationOutcome::Persisted { outcome, .. } = &captured {
                RuntimeExternalSourceStore::new(
                    database.runtime().clone(),
                    database.authority().clone(),
                )
                .map_err(|error| {
                    tracing::warn!(%error, "registered external-source adapter is unavailable");
                    HostAdmissionOutcome::registered_authority_unavailable()
                })?
                .capture_host_observation(outcome.receipt())
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "registered external-source commit failed");
                    HostAdmissionOutcome::retained_unavailable("external_source_commit_failed")
                })?;
            }
            Ok(captured)
        })
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
        Box::pin(async move {
            let cursor = advance.next_cursor();
            let application =
                self.application(cursor.source().provider().as_str(), cursor.scope())?;
            application
                .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                    advance,
                    cancellation,
                ))
                .await
                .map_err(|error| classify_error(&error))
        })
    }

    fn get_source_cursor<'a>(
        &'a self,
        source: &'a ObservationSourceIdentityV1,
        scope: &'a ObservationScopeV1,
    ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
        Box::pin(async move {
            self.store(source.provider().as_str(), scope)?
                .get_source_cursor(source, scope)
                .await
                .map_err(|error| classify_error(&ObservationApplicationError::Store(error)))
        })
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
        Box::pin(async move {
            let store = self.store(provider, scope)?;
            let mut drained = HostProjectionDrainOutcome::default();
            let mut session_ids = BTreeSet::new();
            for _ in 0..max {
                if cancellation.is_cancelled() {
                    return Err(classify_error(&ObservationApplicationError::Cancelled));
                }
                let Some(observation_id) =
                    store.next_queued_observation().await.map_err(|error| {
                        tracing::warn!(%error, "projection store operation failed during host drain");
                        projection_store_unavailable()
                    })?
                else {
                    break;
                };
                match store
                    .project_observation(&observation_id)
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, "projection store operation failed during host drain");
                        projection_store_unavailable()
                    })?
                {
                    ProjectionPersistOutcome::Projected(projected) => {
                        drained.projected = drained.projected.saturating_add(1);
                        drained.projected_outputs = drained.projected_outputs.saturating_add(
                            u64::try_from(projected.output_count()).unwrap_or(u64::MAX),
                        );
                        if let Some(observation) =
                            store.get_observation(&observation_id).await.map_err(|error| {
                                tracing::warn!(%error, "projection store operation failed during host drain");
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
                        drained.skipped = drained.skipped.saturating_add(1);
                    }
                    ProjectionPersistOutcome::ExactDuplicate(_) => {
                        drained.exact_duplicates = drained.exact_duplicates.saturating_add(1);
                    }
                }
            }
            drained.session_ids = session_ids.into_iter().collect();
            Ok(drained)
        })
    }

    fn has_session_message<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        provider: &'a str,
        message_id: &'a str,
    ) -> AdmissionFuture<'a, bool> {
        Box::pin(async move {
            self.authorities.validate_scope(scope)?;
            let database = self
                .authorities
                .registered_database(host_scope(scope))?
                .ok_or_else(HostAdmissionOutcome::registered_authority_unavailable)?;
            database
                .has_session_message(provider, message_id)
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "registered host session-message lookup failed");
                    HostAdmissionOutcome::registered_authority_unavailable()
                })
        })
    }

    fn get_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
    ) -> AdmissionFuture<'a, Option<ParseOffset>> {
        Box::pin(async move {
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
        })
    }

    fn advance_parse_offset<'a>(
        &'a self,
        scope: &'a ObservationScopeV1,
        path: &'a str,
        offset: ParseOffset,
    ) -> AdmissionFuture<'a, ()> {
        Box::pin(async move {
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
        })
    }
}

fn host_scope(scope: &ObservationScopeV1) -> HostAdmissionScope {
    match scope {
        ObservationScopeV1::Profile => HostAdmissionScope::Profile,
        ObservationScopeV1::Project { .. } => HostAdmissionScope::Project,
    }
}

fn supported_provider(provider: &str) -> bool {
    matches!(provider, "kimi" | "opencode")
        || tracedecay_sessions::runtime::SessionProvider::parse(provider)
            .is_some_and(tracedecay_sessions::runtime::SessionProvider::supports_host_admission)
}

const fn outcome(
    status: HostAdmissionStatus,
    retryable: bool,
    reason_code: Option<&'static str>,
) -> HostAdmissionOutcome {
    HostAdmissionOutcome {
        status,
        retryable,
        reason_code,
    }
}

const fn projection_store_unavailable() -> HostAdmissionOutcome {
    outcome(
        HostAdmissionStatus::Unavailable,
        true,
        Some("projection_store_unavailable"),
    )
}

fn classify_error(error: &ObservationApplicationError) -> HostAdmissionOutcome {
    match error {
        ObservationApplicationError::Cancelled => outcome(
            HostAdmissionStatus::Backpressured,
            true,
            Some("admission_cancelled"),
        ),
        ObservationApplicationError::Store(ObservationStoreError::CursorConflict { .. }) => {
            outcome(
                HostAdmissionStatus::Backpressured,
                true,
                Some("cursor_conflict"),
            )
        }
        ObservationApplicationError::Store(ObservationStoreError::Storage { .. }) => outcome(
            HostAdmissionStatus::Unavailable,
            true,
            Some("authority_write_failed"),
        ),
        ObservationApplicationError::Contract(_) => outcome(
            HostAdmissionStatus::Degraded,
            false,
            Some("invalid_observation_contract"),
        ),
        ObservationApplicationError::Privacy(_) => outcome(
            HostAdmissionStatus::Degraded,
            false,
            Some("privacy_boundary_failed"),
        ),
        ObservationApplicationError::Store(_)
        | ObservationApplicationError::PersistedObservationUnavailable => outcome(
            HostAdmissionStatus::Degraded,
            false,
            Some("observation_commit_failed"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_errors_map_to_bounded_static_outcomes() {
        assert_eq!(
            classify_error(&ObservationApplicationError::Cancelled),
            outcome(
                HostAdmissionStatus::Backpressured,
                true,
                Some("admission_cancelled"),
            )
        );
        assert_eq!(
            classify_error(&ObservationApplicationError::Store(
                ObservationStoreError::Storage {
                    operation: "write",
                    source: Box::new(std::io::Error::other("provider content must not escape",)),
                },
            )),
            outcome(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            )
        );
    }
}
