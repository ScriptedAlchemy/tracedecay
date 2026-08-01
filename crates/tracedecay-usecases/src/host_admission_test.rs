use tracedecay_store::ObservationStoreError;

use super::*;

#[test]
fn probe_distinguishes_unknown_provider_and_unbound_project_authority() {
    let facade = HostAdmissionFacade::new(HostAdmissionAuthorities::default());
    let unknown = facade.probe("other", HostAdmissionScope::Project);
    assert_eq!(unknown.status, HostAdmissionStatus::Unknown);
    assert_eq!(unknown.reason_code, Some("unknown_provider"));
    assert!(!unknown.retryable);

    let unavailable = facade.probe("claude", HostAdmissionScope::Project);
    assert_eq!(unavailable.status, HostAdmissionStatus::Unavailable);
    assert_eq!(unavailable.reason_code, Some("project_authority_unbound"));
    assert!(!unavailable.retryable);
}

#[test]
fn all_production_provider_ids_are_supported() {
    for provider in tracedecay_sessions::runtime::SessionProvider::ALL
        .into_iter()
        .filter(|provider| provider.supports_host_admission())
    {
        assert!(
            supported_provider(provider.id()),
            "unsupported provider {}",
            provider.id()
        );
    }
    assert!(!supported_provider("roo"));
    assert!(!supported_provider("vibe"));
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
fn quarantine_outcomes_serialize_as_static_payload_free_dispositions() {
    for outcome in [
        HostAdmissionOutcome::quarantine_full(),
        HostAdmissionOutcome::quarantine_corrupted(),
        HostAdmissionOutcome::quarantine_recovery_required(),
    ] {
        let rendered = serde_json::to_string(&outcome).unwrap();
        assert!(rendered.contains("spool_quarantine_"));
        assert!(!rendered.contains("provider-private-payload"));
        assert!(!matches!(
            outcome.status,
            HostAdmissionStatus::Committed | HostAdmissionStatus::ExactDuplicate
        ));
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
