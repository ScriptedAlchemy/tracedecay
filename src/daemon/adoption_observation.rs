//! Once-per-project-open adoption-eligibility census over the composed
//! application capability catalog.
//!
//! `application_catalog_contributions` is the one closed composition
//! authority the daemon serves, so enumerating it is a complete census
//! (`Known` coverage). Per family: `eligible` = every composed capability,
//! `enabled` = default-profile eligible, `available` = enabled and callable —
//! the exact filter stages of `catalog_composition::application_profile` in
//! funnel order. Families with no composed capability are not emitted: a
//! `Known`-zero census would falsely claim their population is empty.

use std::collections::BTreeMap;
use std::path::Path;

use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, application_catalog_contributions,
};
use tracedecay_domain::{AdoptionEligibilityObservedV1, CoverageStateV1};
use tracedecay_tool_catalog::{CatalogContributionV1, ProfileId};
use tracedecay_usecases::observability::record_adoption_eligibility;

use super::log_daemon_event;
use crate::global_db::RegisteredGlobalDb;

/// Composed capability namespaces mapped onto the closed adoption capability
/// families (`AdoptionEligibilityObservedV1::validate`). Prefix, not equality:
/// each namespace is owned by exactly one catalog contribution.
const FAMILY_NAMESPACES: &[(&str, &str)] = &[
    ("capability.application.symbol-search", "retrieval"),
    ("capability.application.primitive.", "retrieval"),
    ("capability.application.code-query.", "retrieval"),
    ("capability.application.context-scout-", "context_scout"),
    ("capability.application.feedback.", "feedback"),
    ("capability.application.git.", "git"),
    ("capability.application.github-stack.", "git"),
    ("capability.application.native-integration.", "git"),
    ("capability.git.", "git"),
    ("capability.application.lsp.", "lsp"),
    // The Observatory read surface exposes only the canonical observability
    // and cost read models — the analytics family's one composed capability.
    ("capability.application.observatory-read", "analytics"),
];

fn adoption_family(capability_id: &str) -> Option<&'static str> {
    FAMILY_NAMESPACES
        .iter()
        .find_map(|(namespace, family)| capability_id.starts_with(namespace).then_some(*family))
}

/// Enumerates the complete composed catalog into per-family eligibility
/// observations. Only families with a non-zero eligible population appear.
pub(in crate::daemon) fn adoption_eligibility_census()
-> Result<Vec<AdoptionEligibilityObservedV1>, ApplicationContractError> {
    let contributions = application_catalog_contributions()?;
    let default_profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?;
    let mut families: BTreeMap<&'static str, AdoptionEligibilityObservedV1> = BTreeMap::new();
    for capability in contributions
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
    {
        let Some(family) = adoption_family(capability.capability_id().as_str()) else {
            continue;
        };
        let observation = families
            .entry(family)
            .or_insert_with(|| AdoptionEligibilityObservedV1 {
                capability: family.to_owned(),
                eligible: 0,
                enabled: 0,
                available: 0,
            });
        observation.eligible = observation.eligible.saturating_add(1);
        if capability.profile_eligibility().contains(&default_profile) {
            observation.enabled = observation.enabled.saturating_add(1);
            if capability.availability().is_callable() {
                observation.available = observation.available.saturating_add(1);
            }
        }
    }
    Ok(families.into_values().collect())
}

/// Records the project-open adoption-eligibility census through the
/// project-bound observation authority. Telemetry only: every failure is
/// logged and discarded so project open never blocks or fails on it.
pub(in crate::daemon) async fn record_project_open_adoption_census(
    db: &RegisteredGlobalDb,
    project_root: &Path,
) {
    let observations = match adoption_eligibility_census() {
        Ok(observations) => observations,
        Err(error) => {
            log_daemon_event(
                "adoption_observation",
                &[
                    ("project", project_root.display().to_string()),
                    ("outcome", "unavailable".to_owned()),
                    ("reason", error.to_string()),
                ],
            );
            return;
        }
    };
    for observation in observations {
        let family = observation.capability.clone();
        // The census enumerated the whole composed catalog, so each family
        // observation is a complete count of its eligible population.
        if let Err(error) =
            record_adoption_eligibility(db, CoverageStateV1::Known, observation).await
        {
            log_daemon_event(
                "adoption_observation",
                &[
                    ("project", project_root.display().to_string()),
                    ("family", family),
                    ("outcome", "failed".to_owned()),
                    ("reason", format!("{error:?}")),
                ],
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_application::{
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    };
    use tracedecay_domain::{CoverageStateV1, ObservabilityPayloadV1, ProjectId};
    use tracedecay_usecases::observability::RegisteredObservabilityPortV1;

    use super::*;

    /// Composed namespaces deliberately outside the closed adoption family
    /// vocabulary. Configuration, retained memory/LCM, and source editing
    /// have no adoption family, so their capabilities are excluded from every
    /// census rather than force-fitted into an unrelated family.
    const OUT_OF_SCOPE_NAMESPACES: &[&str] = &[
        "capability.application.configuration.",
        "capability.application.retained.",
        "capability.application.source-edit.",
    ];

    /// Families the composed application catalog can truthfully census today.
    const COMPOSED_FAMILIES: &[&str] = &[
        "retrieval",
        "context_scout",
        "feedback",
        "git",
        "lsp",
        "analytics",
    ];

    #[test]
    fn every_composed_capability_is_classified_or_deliberately_out_of_scope() {
        let contributions = application_catalog_contributions().expect("composed catalog");
        for capability in contributions
            .iter()
            .flat_map(CatalogContributionV1::capabilities)
        {
            let id = capability.capability_id().as_str();
            let classified = adoption_family(id).is_some()
                || OUT_OF_SCOPE_NAMESPACES
                    .iter()
                    .any(|namespace| id.starts_with(namespace));
            assert!(
                classified,
                "{id} joined the composed catalog without an adoption-census decision; \
                 map its namespace to a closed family or record it as out of scope"
            );
        }
    }

    #[test]
    fn census_counts_hold_the_funnel_order_for_every_composed_family() {
        let census = adoption_eligibility_census().expect("catalog census");
        assert!(!census.is_empty(), "the composed catalog census is empty");
        let by_family: BTreeMap<&str, &AdoptionEligibilityObservedV1> = census
            .iter()
            .map(|observation| (observation.capability.as_str(), observation))
            .collect();
        for family in COMPOSED_FAMILIES {
            let observation = by_family
                .get(family)
                .unwrap_or_else(|| panic!("{family} family missing from the catalog census"));
            assert!(
                observation.eligible > 0,
                "{family} must census a non-zero eligible population"
            );
            assert!(observation.enabled <= observation.eligible);
            assert!(observation.available <= observation.enabled);
        }
        // The default profile serves callable retrieval capabilities, so the
        // census must observe them as enabled and available, not merely
        // composed.
        assert!(by_family["retrieval"].available > 0);
        // Families this catalog authority does not compose must be absent
        // instead of claiming a Known-zero eligible population.
        for family in [
            "automation",
            "work",
            "workflow",
            "hooks",
            "mcp",
            "dashboard",
        ] {
            assert!(
                !by_family.contains_key(family),
                "{family} has no composed catalog capability and must not be emitted"
            );
        }
    }

    #[tokio::test]
    async fn project_open_census_persists_known_coverage_family_observations() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let project_id = ProjectId::new("project.adoption.census").expect("project id");
        let runtime = crate::global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        let db = runtime.project_database().expect("project database");

        record_project_open_adoption_census(db, project.path()).await;

        let page = RegisteredObservabilityPortV1::new(db)
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: project_id.as_str().to_owned(),
                event_kinds: vec!["adoption.eligibility_observed.v1".to_owned()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: i64::MAX,
                },
                after_watermark: None,
                limit: 32,
            })
            .await
            .expect("read persisted eligibility census");
        let expected = adoption_eligibility_census().expect("catalog census");
        assert_eq!(
            page.events.len(),
            expected.len(),
            "one observation must persist per composed family"
        );
        let persisted: BTreeMap<String, _> = page
            .events
            .iter()
            .map(|event| {
                assert_eq!(event.coverage, CoverageStateV1::Known);
                let ObservabilityPayloadV1::AdoptionEligibility(observation) = &event.payload
                else {
                    panic!("unexpected payload for {}", event.event_kind);
                };
                (observation.capability.clone(), observation.clone())
            })
            .collect();
        for observation in expected {
            assert_eq!(
                persisted.get(&observation.capability),
                Some(&observation),
                "persisted census must match the composed catalog"
            );
        }
    }
}
