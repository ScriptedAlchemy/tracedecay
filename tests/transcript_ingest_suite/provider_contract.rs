use std::collections::BTreeSet;

use tracedecay::sessions::SessionProvider;
use tracedecay_domain::HostIntegrationIdV1;

#[test]
fn direct_hosts_and_transcript_only_providers_partition_host_admission() {
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

    let transcript_only = SessionProvider::ALL
        .into_iter()
        .filter(|provider| provider.supports_host_admission())
        .filter(|provider| HostIntegrationIdV1::from_wire(provider.id()).is_none())
        .collect::<Vec<_>>();
    assert_eq!(
        transcript_only,
        [
            SessionProvider::Cline,
            SessionProvider::RooCode,
            SessionProvider::Kilo,
        ]
    );
}
