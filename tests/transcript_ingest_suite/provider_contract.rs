use std::collections::BTreeSet;

use tracedecay_domain::HostIntegrationIdV1;
use tracedecay_sessions::runtime::SessionProvider;

#[test]
fn provider_capture_and_direct_host_admission_remain_distinct() {
    let catalogued = HostIntegrationIdV1::ALL
        .into_iter()
        .map(HostIntegrationIdV1::as_str)
        .collect::<BTreeSet<_>>();
    let direct_from_providers = SessionProvider::ALL
        .into_iter()
        .filter_map(|provider| HostIntegrationIdV1::from_wire(provider.id()))
        .map(HostIntegrationIdV1::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(catalogued, direct_from_providers);

    // The Cline family is admitted directly without owning a catalogued host
    // integration. Membership is the durable claim; the set is free to grow.
    let admitted_without_host_integration = SessionProvider::ALL
        .into_iter()
        .filter(|provider| provider.supports_host_admission())
        .filter(|provider| HostIntegrationIdV1::from_wire(provider.id()).is_none())
        .collect::<Vec<_>>();
    for provider in [
        SessionProvider::Cline,
        SessionProvider::RooCode,
        SessionProvider::Kilo,
    ] {
        assert!(
            admitted_without_host_integration.contains(&provider),
            "{provider:?} must be admitted directly without a host integration"
        );
    }
    assert!(
        !SessionProvider::Vibe.supports_host_admission(),
        "Vibe canonical capture runs through provider ingestion, not direct host admission"
    );
}
