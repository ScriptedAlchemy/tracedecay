use serde::Serialize;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceIdentityV1,
};
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};
use tracedecay_store::{ObservationPersistOutcome, ObservationStore, ObservationStoreError};

use crate::application::observation::{
    AdvanceNonDurableSourceCursorRequest, CaptureObservationOutcome, CaptureObservationRequest,
    ObservationApplication, ObservationApplicationError, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::RecordSanitizerV1;
use crate::store::observation::GlobalDbObservationStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostAdmissionStatus {
    Supported,
    Degraded,
    Unavailable,
    Unknown,
    Backpressured,
    AcceptedForReplay,
    Committed,
    ExactDuplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct HostAdmissionOutcome {
    pub status: HostAdmissionStatus,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<&'static str>,
}

impl HostAdmissionOutcome {
    const fn new(
        status: HostAdmissionStatus,
        retryable: bool,
        reason_code: Option<&'static str>,
    ) -> Self {
        Self {
            status,
            retryable,
            reason_code,
        }
    }

    pub const fn supported() -> Self {
        Self::new(HostAdmissionStatus::Supported, false, None)
    }

    pub const fn accepted_for_replay() -> Self {
        Self::new(HostAdmissionStatus::AcceptedForReplay, false, None)
    }

    pub const fn replay_completed(changed: bool, exact_duplicate: bool) -> Self {
        if changed {
            Self::new(HostAdmissionStatus::Committed, false, None)
        } else if exact_duplicate {
            Self::new(HostAdmissionStatus::ExactDuplicate, false, None)
        } else {
            Self::accepted_for_replay()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostAdmissionScope {
    Project,
    Profile,
}

#[derive(Clone, Copy, Default)]
pub struct HostAdmissionAuthorities<'a> {
    project: Option<&'a GlobalDb>,
    profile: Option<&'a GlobalDb>,
}

impl<'a> HostAdmissionAuthorities<'a> {
    pub const fn new(project: Option<&'a GlobalDb>, profile: Option<&'a GlobalDb>) -> Self {
        Self { project, profile }
    }

    fn get(self, scope: HostAdmissionScope) -> Option<&'a GlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project,
            HostAdmissionScope::Profile => self.profile,
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

    pub fn probe(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        if !supported_provider(provider) {
            return HostAdmissionOutcome::new(
                HostAdmissionStatus::Unknown,
                false,
                Some("unknown_provider"),
            );
        }
        if self.authorities.get(scope).is_none() {
            return HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_unavailable"),
            );
        }
        HostAdmissionOutcome::supported()
    }

    pub fn accept_replay(&self, provider: &str, scope: HostAdmissionScope) -> HostAdmissionOutcome {
        let probe = self.probe(provider, scope);
        if probe.status == HostAdmissionStatus::Supported {
            HostAdmissionOutcome::accepted_for_replay()
        } else {
            probe
        }
    }

    pub async fn get_source_cursor(
        &self,
        source: &ObservationSourceIdentityV1,
        scope: &ObservationScopeV1,
    ) -> Result<Option<ObservationSourceCursorV1>, HostAdmissionOutcome> {
        let store = self.store(source.provider().as_str(), scope)?;
        store
            .get_source_cursor(source, scope)
            .await
            .map_err(|error| classify_error(&ObservationApplicationError::Store(error)))
    }

    pub async fn capture_observation(
        &self,
        request: CaptureObservationRequest,
    ) -> Result<CaptureObservationOutcome, HostAdmissionOutcome> {
        let application = self.application(request.provider(), request.scope())?;
        application
            .capture_observation(request)
            .await
            .map_err(|error| classify_error(&error))
    }

    pub async fn capture(&self, request: CaptureObservationRequest) -> HostAdmissionOutcome {
        match self.capture_observation(request).await {
            Ok(outcome) => classify_capture(outcome),
            Err(outcome) => outcome,
        }
    }

    pub async fn advance_non_durable_source_cursor(
        &self,
        advance: ObservationCursorAdvance,
        cancellation: ObservationCancellation,
    ) -> Result<CursorAdvanceOutcome, HostAdmissionOutcome> {
        let cursor = advance.next_cursor();
        let application = self.application(cursor.source().provider().as_str(), cursor.scope())?;
        application
            .advance_non_durable_source_cursor(AdvanceNonDurableSourceCursorRequest::new(
                advance,
                cancellation,
            ))
            .await
            .map_err(|error| classify_error(&error))
    }

    fn application(
        &self,
        provider: &str,
        scope: &ObservationScopeV1,
    ) -> Result<ObservationApplication<GlobalDbObservationStore<'a>>, HostAdmissionOutcome> {
        let store = self.store(provider, scope)?;
        let sanitizer = RecordSanitizerV1::observation_v1().map_err(|_| {
            HostAdmissionOutcome::new(
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
        let scope = host_scope(scope);
        let probe = self.probe(provider, scope);
        if probe.status != HostAdmissionStatus::Supported {
            return Err(probe);
        }
        let db = self.authorities.get(scope).ok_or_else(|| {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_unavailable"),
            )
        })?;
        Ok(GlobalDbObservationStore::new(db))
    }
}

fn host_scope(scope: &ObservationScopeV1) -> HostAdmissionScope {
    match scope {
        ObservationScopeV1::Profile => HostAdmissionScope::Profile,
        ObservationScopeV1::Project { .. } => HostAdmissionScope::Project,
    }
}

fn supported_provider(provider: &str) -> bool {
    matches!(
        provider,
        "claude" | "codex" | "cursor" | "hermes" | "kiro" | "cline" | "roo-code" | "kilo"
    )
}

fn classify_capture(outcome: CaptureObservationOutcome) -> HostAdmissionOutcome {
    match outcome {
        CaptureObservationOutcome::Persisted { outcome, .. } => match outcome {
            ObservationPersistOutcome::Committed(_) => {
                HostAdmissionOutcome::new(HostAdmissionStatus::Committed, false, None)
            }
            ObservationPersistOutcome::ExactDuplicate(_) => {
                HostAdmissionOutcome::new(HostAdmissionStatus::ExactDuplicate, false, None)
            }
            ObservationPersistOutcome::CoveredDuplicate(_) => HostAdmissionOutcome::new(
                HostAdmissionStatus::Committed,
                false,
                Some("duplicate_coverage_committed"),
            ),
        },
        CaptureObservationOutcome::Rejected { .. } => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_rejected"),
        ),
        CaptureObservationOutcome::Quarantined { .. } => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("sanitizer_quarantined"),
        ),
    }
}

fn classify_error(error: &ObservationApplicationError) -> HostAdmissionOutcome {
    match error {
        ObservationApplicationError::Cancelled => HostAdmissionOutcome::new(
            HostAdmissionStatus::Backpressured,
            true,
            Some("admission_cancelled"),
        ),
        ObservationApplicationError::Store(ObservationStoreError::CursorConflict { .. }) => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Backpressured,
                true,
                Some("cursor_conflict"),
            )
        }
        ObservationApplicationError::Store(ObservationStoreError::Storage { .. }) => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            )
        }
        ObservationApplicationError::Contract(_) => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("invalid_observation_contract"),
        ),
        ObservationApplicationError::Privacy(_) => HostAdmissionOutcome::new(
            HostAdmissionStatus::Degraded,
            false,
            Some("privacy_boundary_failed"),
        ),
        ObservationApplicationError::Store(_)
        | ObservationApplicationError::PersistedObservationUnavailable => {
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Degraded,
                false,
                Some("observation_commit_failed"),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_distinguishes_unknown_provider_and_missing_authority() {
        let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::default());
        assert_eq!(
            facade.probe("other", HostAdmissionScope::Project).status,
            HostAdmissionStatus::Unknown
        );
        assert_eq!(
            facade.probe("claude", HostAdmissionScope::Project).status,
            HostAdmissionStatus::Unavailable
        );
    }

    #[test]
    fn all_production_provider_ids_are_supported() {
        for provider in [
            "claude", "codex", "cursor", "hermes", "kiro", "cline", "roo-code", "kilo",
        ] {
            assert!(
                supported_provider(provider),
                "unsupported provider {provider}"
            );
        }
        assert!(!supported_provider("roo"));
    }

    #[test]
    fn replay_statuses_serialize_without_provider_content() {
        for (outcome, expected_status) in [
            (
                HostAdmissionOutcome::replay_completed(false, false),
                "accepted_for_replay",
            ),
            (
                HostAdmissionOutcome::replay_completed(false, true),
                "exact_duplicate",
            ),
            (
                HostAdmissionOutcome::replay_completed(true, false),
                "committed",
            ),
        ] {
            assert_eq!(
                serde_json::to_value(outcome).unwrap(),
                serde_json::json!({
                    "status": expected_status,
                    "retryable": false,
                })
            );
        }
    }

    #[test]
    fn application_errors_map_to_bounded_static_outcomes() {
        assert_eq!(
            classify_error(&ObservationApplicationError::Cancelled),
            HostAdmissionOutcome::new(
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
            HostAdmissionOutcome::new(
                HostAdmissionStatus::Unavailable,
                true,
                Some("authority_write_failed"),
            )
        );
    }
}
