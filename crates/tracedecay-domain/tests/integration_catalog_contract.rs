use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_domain::{
    HostCapabilityStateV1, HostCapabilityV1, HostIntegrationCatalogV1, HostIntegrationIdV1,
    HostKindV1, IntegrationCatalogError, IntegrationDaemonActionV1, IntegrationDaemonApiV1,
    IntegrationEffectClassV1, IntegrationPrivacyClassV1, TraceDecayProfileBindingV1,
    canonical_json_bytes, host_integration_catalog_v1, stock_host_capabilities,
};

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FixtureAdmissionReason {
    SpoolRecordTooLarge,
    ProjectAuthorityUnbound,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "status",
    content = "reason_code",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum FixtureAdmissionState {
    Supported,
    Degraded(FixtureAdmissionReason),
    Unavailable(FixtureAdmissionReason),
}

fn catalog_json() -> Value {
    serde_json::to_value(host_integration_catalog_v1()).expect("catalog JSON")
}

#[test]
fn catalog_wire_round_trips_without_changing_canonical_bytes() {
    let catalog = host_integration_catalog_v1();
    catalog.validate().expect("built-in catalog is valid");

    let encoded = serde_json::to_vec(&catalog).expect("catalog serialization");
    let decoded: HostIntegrationCatalogV1 =
        serde_json::from_slice(&encoded).expect("catalog schema round trip");
    decoded.validate().expect("decoded catalog is valid");
    assert_eq!(decoded, catalog);
    assert_eq!(
        canonical_json_bytes(&decoded).expect("decoded canonical bytes"),
        canonical_json_bytes(&catalog).expect("catalog canonical bytes")
    );
}

#[test]
fn observation_host_matrix_matches_native_event_fixture_providers() {
    let catalog = host_integration_catalog_v1();
    let [capability] = catalog.capabilities() else {
        panic!("observation catalog must contain exactly one capability");
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
    }
}

#[test]
fn host_event_fixture_admission_deserializes_into_fixture_only_taxonomy() {
    for (provider, fixture) in HOST_EVENT_FIXTURES {
        let document: Value = serde_json::from_str(fixture).expect("valid host fixture");
        let fixture_states: Vec<FixtureAdmissionState> = document["cases"]
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
        assert!(
            !fixture_states.is_empty(),
            "{provider} fixture must prove at least one admission taxonomy state"
        );
        assert!(
            fixture_states.contains(&FixtureAdmissionState::Supported),
            "{provider} fixture must prove supported admission"
        );
    }
}

#[test]
fn typed_status_reasons_have_stable_encoding() {
    assert_eq!(
        serde_json::to_value(FixtureAdmissionState::Degraded(
            FixtureAdmissionReason::SpoolRecordTooLarge,
        ))
        .unwrap(),
        json!({"status": "degraded", "reason_code": "spool_record_too_large"})
    );
    assert_eq!(
        serde_json::to_value(FixtureAdmissionState::Unavailable(
            FixtureAdmissionReason::ProjectAuthorityUnbound,
        ))
        .unwrap(),
        json!({"status": "unavailable", "reason_code": "project_authority_unbound"})
    );
}

#[test]
fn schema_rejects_unknown_fields() {
    let mut unknown = catalog_json();
    unknown["future_field"] = json!(true);
    assert!(serde_json::from_value::<HostIntegrationCatalogV1>(unknown).is_err());

    let mut host_unknown = catalog_json();
    host_unknown["capabilities"][0]["hosts"][0]["availability_states"] = json!([]);
    assert!(serde_json::from_value::<HostIntegrationCatalogV1>(host_unknown).is_err());
}

#[test]
fn catalog_validation_rejects_empty_catalog_and_duplicate_hosts() {
    let empty: HostIntegrationCatalogV1 = serde_json::from_value(json!({
        "schema_version": 1,
        "capabilities": []
    }))
    .unwrap();
    assert!(matches!(
        empty.validate(),
        Err(IntegrationCatalogError::EmptyCatalog)
    ));

    let mut duplicate_capability = catalog_json();
    let capability = duplicate_capability["capabilities"][0].clone();
    duplicate_capability["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(capability);
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(duplicate_capability).unwrap();
    assert!(matches!(
        catalog.validate(),
        Err(IntegrationCatalogError::DuplicateCapabilityId(_))
    ));

    let mut duplicate_host = catalog_json();
    let host = duplicate_host["capabilities"][0]["hosts"][0].clone();
    duplicate_host["capabilities"][0]["hosts"]
        .as_array_mut()
        .unwrap()
        .push(host);
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(duplicate_host).unwrap();
    assert!(matches!(
        catalog.validate(),
        Err(IntegrationCatalogError::DuplicateHostIntegration { .. })
    ));
}

#[test]
fn catalog_validation_rejects_an_incomplete_host_matrix() {
    let mut incomplete = catalog_json();
    incomplete["capabilities"][0]["hosts"]
        .as_array_mut()
        .unwrap()
        .pop();
    let catalog: HostIntegrationCatalogV1 = serde_json::from_value(incomplete).unwrap();
    assert!(matches!(
        catalog.validate(),
        Err(IntegrationCatalogError::IncompleteHostMatrix { .. })
    ));
}

#[test]
fn stable_direct_host_integration_ids_match_provider_ids() {
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

#[test]
fn stock_host_kinds_project_only_fixture_backed_observation_integrations() {
    assert_eq!(
        HostKindV1::ALL.map(|host| serde_json::to_value(host).unwrap()),
        [
            "claude_code",
            "cursor_desktop",
            "cursor_cloud",
            "codex",
            "hermes",
            "kiro",
            "cline_family",
            "cline",
            "roo_code",
            "kilo",
            "kimi_code",
            "open_code",
        ]
        .map(Value::from)
    );
    assert_eq!(
        HostKindV1::ClaudeCode.fixture_backed_observation_integration_id(),
        Some(HostIntegrationIdV1::Claude)
    );
    assert_eq!(
        HostKindV1::CursorDesktop.fixture_backed_observation_integration_id(),
        Some(HostIntegrationIdV1::Cursor)
    );
    assert_eq!(
        HostKindV1::Codex.fixture_backed_observation_integration_id(),
        Some(HostIntegrationIdV1::Codex)
    );
    assert_eq!(
        HostKindV1::Hermes.fixture_backed_observation_integration_id(),
        Some(HostIntegrationIdV1::Hermes)
    );
    assert_eq!(
        HostKindV1::Kiro.fixture_backed_observation_integration_id(),
        Some(HostIntegrationIdV1::Kiro)
    );
    for host in [
        HostKindV1::CursorCloud,
        HostKindV1::ClineFamily,
        HostKindV1::Cline,
        HostKindV1::RooCode,
        HostKindV1::Kilo,
        HostKindV1::KimiCode,
        HostKindV1::OpenCode,
    ] {
        assert_eq!(
            host.fixture_backed_observation_integration_id(),
            None,
            "{host:?} must not claim a native observation fixture"
        );
    }
}

#[test]
fn stock_host_capability_matrix_is_sole_capability_authority() {
    let catalog = host_integration_catalog_v1();
    let views = catalog.stock_host_capability_views();
    assert_eq!(
        views.iter().map(|view| view.host()).collect::<Vec<_>>(),
        HostKindV1::ALL
    );
    let mut digests = BTreeSet::new();
    for view in &views {
        assert_eq!(
            view.capabilities()
                .iter()
                .map(|record| record.capability)
                .collect::<Vec<_>>(),
            [
                HostCapabilityV1::Lsp,
                HostCapabilityV1::NativeDiagnostics,
                HostCapabilityV1::Hooks,
                HostCapabilityV1::Mcp,
                HostCapabilityV1::Cli,
            ]
        );
        assert_eq!(
            view.capabilities(),
            catalog.stock_host_capabilities(view.host())
        );
        let digest = catalog.host_capability_digest(view.host()).unwrap();
        assert_ne!(digest, [0; 32]);
        assert!(
            digests.insert(digest),
            "each HostKindV1 digest is stable and unique"
        );
    }
    assert_eq!(digests.len(), HostKindV1::ALL.len());

    let catalog_digest = catalog.canonical_authority_digest().unwrap();
    assert_ne!(catalog_digest, [0; 32]);
    assert!(!digests.contains(&catalog_digest));
}

#[test]
fn cursor_cloud_remains_discoverable_only_as_unsupported() {
    let capabilities = stock_host_capabilities(HostKindV1::CursorCloud);
    assert!(
        capabilities
            .iter()
            .all(|record| { matches!(record.state, HostCapabilityStateV1::Unavailable(_)) })
    );
}
