use std::collections::BTreeSet;

use tracedecay::catalog_composition::build_application_catalog_snapshot;
use tracedecay_application::{application_catalog_contributions, application_handler_descriptors};
use tracedecay_tool_catalog::{CapabilityId, ProfileId};

#[test]
fn root_snapshot_validates_every_application_contribution_against_real_handlers() {
    let contributions = application_catalog_contributions().unwrap();
    let handlers = application_handler_descriptors().unwrap();
    let snapshot = build_application_catalog_snapshot().unwrap();

    let contributed_capabilities = contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
        .count();
    assert_eq!(contributed_capabilities, handlers.iter().count());
    assert_eq!(snapshot.capabilities().count(), contributed_capabilities);

    for contribution in &contributions {
        assert!(contribution.bindings().is_empty());
        for capability in contribution.capabilities() {
            let handler = handlers
                .get(capability.use_case_id())
                .expect("every shipped capability has one real application handler");
            assert_eq!(handler.request_schema(), capability.request_schema());
            assert_eq!(handler.result_schema(), capability.result_schema());
        }
    }

    let symbol_search = CapabilityId::new("capability.retrieval.symbol-search").unwrap();
    assert!(snapshot.capability(&symbol_search).is_some());

    let default_profile = ProfileId::new("profile.default").unwrap();
    assert!(snapshot.profile(&default_profile).is_some());
    assert_eq!(
        snapshot
            .visible_capabilities(&default_profile, &BTreeSet::new())
            .into_iter()
            .map(|capability| capability.capability_id())
            .collect::<Vec<_>>(),
        vec![&symbol_search]
    );
}
