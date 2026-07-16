use serde_json::{Value, json};
use tracedecay_domain::{
    HostCapabilityAvailabilityV1, HostCapabilityReasonV1, HostIntegrationCatalogV1,
    HostIntegrationIdV1, IntegrationCatalogError, IntegrationDaemonActionV1,
    IntegrationDaemonApiV1, IntegrationEffectClassV1, IntegrationPrivacyClassV1,
    TraceDecayProfileBindingV1, canonical_json_bytes, host_integration_catalog_v1,
};

const GOLDEN_CATALOG: &str = include_str!("fixtures/integration_catalog_v1.json");
const HOST_EVENT_FIXTURES: [(&str, &str); 5] = [
    (
        "claude",
        include_str!("../../../tests/fixtures/host_events/claude/baseline.json"),
    ),
    (
        "codex",
        include_str!("../../../tests/fixtures/host_events/codex/baseline.json"),
    ),
    (
        "cursor",
        include_str!("../../../tests/fixtures/host_events/cursor/baseline.json"),
    ),
    (
        "hermes",
        include_str!("../../../tests/fixtures/host_events/hermes/baseline.json"),
    ),
    (
        "kiro",
        include_str!("../../../tests/fixtures/host_events/kiro/baseline.json"),
    ),
];

#[test]
fn catalog_schema_round_trips_to_the_deterministic_golden() {
    let catalog = host_integration_catalog_v1();
    catalog.validate().expect("built-in catalog is valid");

    let golden_value: Value = serde_json::from_str(GOLDEN_CATALOG).expect("valid golden JSON");
    let expected = canonical_json_bytes(&golden_value).expect("canonical golden");
    let actual = canonical_json_bytes(&catalog).expect("canonical catalog");
    assert_eq!(actual, expected);

    let decoded: HostIntegrationCatalogV1 =
        serde_json::from_str(GOLDEN_CATALOG).expect("catalog schema round trip");
    decoded.validate().expect("decoded catalog is valid");
    assert_eq!(decoded, catalog);
}

#[test]
fn host_matrix_is_backed_by_the_native_event_fixtures() {
    let catalog = host_integration_catalog_v1();
    let [capability] = catalog.capabilities() else {
        panic!("minimal PR6 catalog must contain exactly one capability");
    };

    assert_eq!(
        capability.capability_id().as_str(),
        "capability.integration.observation.capture"
    );
    assert_eq!(
        capability.effect_class(),
        IntegrationEffectClassV1::DaemonWrite
    );
    assert_eq!(
        capability.privacy_class(),
        IntegrationPrivacyClassV1::SensitiveInputSanitizedByDaemon
    );
    assert_eq!(
        capability.required_daemon().api(),
        IntegrationDaemonApiV1::HostAdmission
    );
    assert_eq!(
        capability.required_daemon().action(),
        IntegrationDaemonActionV1::CaptureObservation
    );

    let fixture_hosts: Vec<_> = HOST_EVENT_FIXTURES
        .iter()
        .map(|(provider, fixture)| {
            let document: Value = serde_json::from_str(fixture).expect("valid host fixture");
            assert_eq!(document["provider"], *provider);
            *provider
        })
        .collect();
    let catalog_hosts: Vec<_> = capability
        .hosts()
        .iter()
        .map(|host| host.integration_id().as_str())
        .collect();
    assert_eq!(catalog_hosts, fixture_hosts);

    for host in capability.hosts() {
        assert_eq!(
            host.profile_binding(),
            TraceDecayProfileBindingV1::User,
            "{} must use the single user TraceDecay profile",
            host.integration_id().as_str()
        );
        let (_, fixture) = HOST_EVENT_FIXTURES
            .iter()
            .find(|(provider, _)| *provider == host.integration_id().as_str())
            .expect("catalog host has a native event fixture");
        let document: Value = serde_json::from_str(fixture).expect("valid host fixture");
        let fixture_states: Vec<HostCapabilityAvailabilityV1> = document["cases"]
            .as_array()
            .expect("host fixture cases")
            .iter()
            .filter_map(|case| {
                let admission = &case["admission"];
                let status = admission["status"].as_str()?;
                matches!(status, "supported" | "degraded" | "unavailable").then(|| {
                    let mut state = json!({"status": status});
                    if let Some(reason) = admission["reason_code"].as_str() {
                        state["reason_code"] = Value::from(reason);
                    }
                    serde_json::from_value(state).expect("fixture state is in the typed taxonomy")
                })
            })
            .collect();
        assert_eq!(
            host.availability_states(),
            fixture_states,
            "{} catalog states must be exactly fixture-backed",
            host.integration_id().as_str()
        );
    }
}

#[test]
fn typed_status_reasons_have_stable_encoding() {
    assert_eq!(
        serde_json::to_value(HostCapabilityAvailabilityV1::Degraded(
            HostCapabilityReasonV1::SpoolRecordTooLarge,
        ))
        .unwrap(),
        json!({"status": "degraded", "reason_code": "spool_record_too_large"})
    );
    assert_eq!(
        serde_json::to_value(HostCapabilityAvailabilityV1::Unavailable(
            HostCapabilityReasonV1::AuthorityUnavailable,
        ))
        .unwrap(),
        json!({"status": "unavailable", "reason_code": "authority_unavailable"})
    );
}

#[test]
fn schema_rejects_unknown_fields_and_supported_reasons() {
    let mut unknown: Value = serde_json::from_str(GOLDEN_CATALOG).unwrap();
    unknown["future_field"] = json!(true);
    assert!(serde_json::from_value::<HostIntegrationCatalogV1>(unknown).is_err());

    let mut supported_reason: Value = serde_json::from_str(GOLDEN_CATALOG).unwrap();
    supported_reason["capabilities"][0]["hosts"][0]["availability_states"][0]["reason_code"] =
        json!("authority_unavailable");
    assert!(serde_json::from_value::<HostIntegrationCatalogV1>(supported_reason).is_err());
}

#[test]
fn catalog_validation_rejects_an_incomplete_host_matrix() {
    let mut incomplete: Value = serde_json::from_str(GOLDEN_CATALOG).unwrap();
    incomplete["capabilities"][0]["hosts"]
        .as_array_mut()
        .unwrap()
        .pop();
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(incomplete).unwrap();
    assert!(catalog.validate().is_err());
}

#[test]
fn catalog_validation_requires_the_exact_fixture_backed_state_set() {
    let mut duplicate: Value = serde_json::from_str(GOLDEN_CATALOG).unwrap();
    let states = duplicate["capabilities"][0]["hosts"][0]["availability_states"]
        .as_array_mut()
        .unwrap();
    states.push(states[0].clone());
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(duplicate).unwrap();
    assert!(matches!(
        catalog.validate(),
        Err(IntegrationCatalogError::DuplicateHostAvailabilityState { .. })
    ));

    let mut incomplete: Value = serde_json::from_str(GOLDEN_CATALOG).unwrap();
    incomplete["capabilities"][0]["hosts"][0]["availability_states"]
        .as_array_mut()
        .unwrap()
        .pop();
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(incomplete).unwrap();
    assert!(matches!(
        catalog.validate(),
        Err(IntegrationCatalogError::InvalidHostAvailabilityStates { .. })
    ));

    let mut unproven: Value = serde_json::from_str(GOLDEN_CATALOG).unwrap();
    unproven["capabilities"][0]["hosts"][0]["availability_states"][1]["reason_code"] =
        json!("authority_unavailable");
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(unproven).unwrap();
    assert!(matches!(
        catalog.validate(),
        Err(IntegrationCatalogError::InvalidHostAvailabilityStates { .. })
    ));
}

#[test]
fn stable_host_integration_ids_match_provider_ids() {
    let encoded: Vec<_> = HostIntegrationIdV1::ALL
        .iter()
        .map(|id| serde_json::to_value(id).unwrap())
        .collect();
    assert_eq!(
        encoded,
        ["claude", "codex", "cursor", "hermes", "kiro"].map(Value::from)
    );
    for host in HostIntegrationIdV1::ALL {
        assert_eq!(HostIntegrationIdV1::from_wire(host.as_wire()), Some(host));
    }
}
