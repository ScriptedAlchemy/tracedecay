//! Once-per-project-open adoption-eligibility census over the composed
//! application capability catalog.
//!
//! `application_catalog_contributions` is the one closed composition
//! authority the daemon serves, so enumerating it is a complete census
//! (`Known` coverage). Per family: `eligible` = every composed capability,
//! `enabled` = default-profile eligible, `available` = enabled and callable,
//! the exact filter stages of `catalog_composition::application_profile` in
//! funnel order. Families with no composed capability are not emitted: a
//! `Known`-zero census would falsely claim their population is empty.

use std::collections::BTreeMap;
use std::path::Path;

use tracedecay_application::observability::record_adoption_eligibility;
use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, application_catalog_contributions,
};
use tracedecay_domain::{AdoptionEligibilityObservedV1, CoverageStateV1};
use tracedecay_tool_catalog::{CatalogContributionV1, ProfileId};

use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_runtime_core::logging::log_daemon_event;

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
    // and cost read models, the analytics family's one composed capability.
    ("capability.application.observatory-read", "analytics"),
];

fn adoption_family(capability_id: &str) -> Option<&'static str> {
    FAMILY_NAMESPACES
        .iter()
        .find_map(|(namespace, family)| capability_id.starts_with(namespace).then_some(*family))
}

/// Enumerates the complete composed catalog into per-family eligibility
/// observations. Only families with a non-zero eligible population appear.
#[hotpath::measure(label = "daemon.adoption.census")]
pub fn adoption_eligibility_census()
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
#[hotpath::measure(label = "daemon.adoption.record", future = true)]
pub async fn record_project_open_adoption_census(db: &RegisteredGlobalDb, project_root: &Path) {
    let observations = match adoption_eligibility_census() {
        Ok(observations) => observations,
        Err(error) => {
            hotpath::gauge!("daemon.adoption.census_unavailable_total").inc(1_u64);
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
        match record_adoption_eligibility(db, CoverageStateV1::Known, observation).await {
            Ok(_) => {
                hotpath::gauge!("daemon.adoption.recorded_total").inc(1_u64);
            }
            Err(error) => {
                hotpath::gauge!("daemon.adoption.record_failed_total").inc(1_u64);
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_application::observability::RegisteredObservabilityPortV1;
    use tracedecay_contracts::{
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
    };
    use tracedecay_domain::{CoverageStateV1, ObservabilityPayloadV1, ProjectId};

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

    /// Every classified capability by family, with the furthest funnel stage
    /// it reaches for the default profile. An added capability fails here by
    /// id; the census counts below are derived from this list.
    const CLASSIFIED_CAPABILITIES: &[(&str, &[(&str, &str)])] = &[
        (
            "analytics",
            &[("capability.application.observatory-read", "available")],
        ),
        (
            "context_scout",
            &[
                ("capability.application.context-scout-budget", "available"),
                ("capability.application.context-scout-cancel", "available"),
                (
                    "capability.application.context-scout-capability",
                    "available",
                ),
                ("capability.application.context-scout-claim", "available"),
                ("capability.application.context-scout-delivery", "available"),
                ("capability.application.context-scout-explain", "available"),
                ("capability.application.context-scout-feedback", "available"),
                ("capability.application.context-scout-pause", "available"),
                ("capability.application.context-scout-recent", "available"),
                ("capability.application.context-scout-resume", "available"),
                ("capability.application.context-scout-status", "available"),
            ],
        ),
        (
            "feedback",
            &[
                (
                    "capability.application.feedback.advisory-cycle",
                    "available",
                ),
                (
                    "capability.application.feedback.affected-tests",
                    "available",
                ),
                (
                    "capability.application.feedback.ci-failure-localize",
                    "eligible",
                ),
                ("capability.application.feedback.diagnostics", "available"),
                ("capability.application.feedback.expand", "available"),
                ("capability.application.feedback.get", "available"),
                (
                    "capability.application.feedback.github-review-ingest",
                    "eligible",
                ),
                ("capability.application.feedback.impact", "available"),
                ("capability.application.feedback.list", "available"),
                ("capability.application.feedback.proximity", "available"),
                ("capability.application.feedback.test-results", "available"),
            ],
        ),
        (
            "git",
            &[
                ("capability.application.git.apply", "available"),
                ("capability.application.git.blame", "available"),
                ("capability.application.git.diff", "available"),
                ("capability.application.git.history", "available"),
                ("capability.application.git.hunks", "available"),
                ("capability.application.git.preview", "available"),
                ("capability.application.git.status", "available"),
                (
                    "capability.application.github-stack.signal-expand",
                    "available",
                ),
                (
                    "capability.application.native-integration.apply",
                    "available",
                ),
                (
                    "capability.application.native-integration.approve",
                    "available",
                ),
                (
                    "capability.application.native-integration.cancel",
                    "available",
                ),
                (
                    "capability.application.native-integration.preflight",
                    "available",
                ),
                (
                    "capability.application.native-integration.stack-snapshot",
                    "available",
                ),
                (
                    "capability.application.native-integration.status",
                    "available",
                ),
                (
                    "capability.application.native-integration.worktree-cleanup-confirm",
                    "available",
                ),
                (
                    "capability.application.native-integration.worktree-cleanup-inspect",
                    "available",
                ),
                (
                    "capability.application.native-integration.worktree-cleanup-reconcile",
                    "available",
                ),
                (
                    "capability.application.native-integration.worktree-cleanup-remove",
                    "available",
                ),
                (
                    "capability.application.native-integration.worktree-inventory",
                    "available",
                ),
                ("capability.git.commit-index", "eligible"),
                ("capability.git.stage-hunks", "eligible"),
                ("capability.git.unstage-hunks", "eligible"),
            ],
        ),
        (
            "lsp",
            &[
                ("capability.application.lsp.context", "eligible"),
                ("capability.application.lsp.context-expand", "eligible"),
            ],
        ),
        (
            "retrieval",
            &[
                ("capability.application.code-query.callees", "available"),
                ("capability.application.code-query.declaration", "available"),
                (
                    "capability.application.code-query.exact-occurrence",
                    "available",
                ),
                ("capability.application.code-query.facets", "available"),
                (
                    "capability.application.code-query.phrase-search",
                    "available",
                ),
                ("capability.application.code-query.references", "available"),
                ("capability.application.code-query.timeline", "available"),
                (
                    "capability.application.code-query.type-definition",
                    "available",
                ),
                ("capability.application.primitive.call-chain", "available"),
                ("capability.application.primitive.circular", "available"),
                ("capability.application.primitive.code-callers", "available"),
                (
                    "capability.application.primitive.code-implementations",
                    "available",
                ),
                (
                    "capability.application.primitive.code-signature-search",
                    "available",
                ),
                (
                    "capability.application.primitive.code-type-hierarchy",
                    "available",
                ),
                ("capability.application.primitive.complexity", "available"),
                ("capability.application.primitive.constructors", "available"),
                ("capability.application.primitive.context", "available"),
                ("capability.application.primitive.coupling", "available"),
                ("capability.application.primitive.dead-code", "available"),
                (
                    "capability.application.primitive.dependency-depth",
                    "available",
                ),
                ("capability.application.primitive.diagnose", "available"),
                (
                    "capability.application.primitive.diagnostics-read",
                    "available",
                ),
                ("capability.application.primitive.distribution", "available"),
                ("capability.application.primitive.doc-coverage", "available"),
                ("capability.application.primitive.dsm", "available"),
                ("capability.application.primitive.field-sites", "available"),
                (
                    "capability.application.primitive.file-dependents",
                    "available",
                ),
                ("capability.application.primitive.gini", "available"),
                ("capability.application.primitive.god-class", "available"),
                ("capability.application.primitive.health", "available"),
                ("capability.application.primitive.health-delta", "available"),
                ("capability.application.primitive.health-read", "available"),
                ("capability.application.primitive.hotspots", "available"),
                ("capability.application.primitive.impact", "available"),
                (
                    "capability.application.primitive.inheritance-depth",
                    "available",
                ),
                ("capability.application.primitive.largest", "available"),
                ("capability.application.primitive.module-api", "available"),
                ("capability.application.primitive.node", "available"),
                ("capability.application.primitive.port-order", "available"),
                ("capability.application.primitive.port-status", "available"),
                (
                    "capability.application.primitive.qualified-name",
                    "available",
                ),
                ("capability.application.primitive.rank", "available"),
                ("capability.application.primitive.recursion", "available"),
                ("capability.application.primitive.redundancy", "available"),
                (
                    "capability.application.primitive.rename-preview",
                    "available",
                ),
                (
                    "capability.application.primitive.session-lookup",
                    "available",
                ),
                ("capability.application.primitive.similar", "available"),
                ("capability.application.primitive.source-body", "available"),
                ("capability.application.primitive.source-lines", "available"),
                (
                    "capability.application.primitive.source-outline",
                    "available",
                ),
                (
                    "capability.application.primitive.storage-status",
                    "available",
                ),
                ("capability.application.primitive.test-map", "available"),
                ("capability.application.primitive.test-risk", "available"),
                ("capability.application.primitive.todos", "available"),
                (
                    "capability.application.primitive.unmounted-files",
                    "available",
                ),
                (
                    "capability.application.primitive.unsafe-patterns",
                    "available",
                ),
                ("capability.application.symbol-search", "available"),
            ],
        ),
    ];

    #[test]
    fn census_counts_each_composed_family_and_omits_uncomposed_families() {
        let contributions = application_catalog_contributions().expect("composed catalog");
        let default_profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile");
        let mut composed = Vec::new();
        for capability in contributions
            .iter()
            .flat_map(CatalogContributionV1::capabilities)
        {
            let id = capability.capability_id().as_str();
            let Some(family) = adoption_family(id) else {
                continue;
            };
            let stage = if !capability.profile_eligibility().contains(&default_profile) {
                "eligible"
            } else if capability.availability().is_callable() {
                "available"
            } else {
                "enabled"
            };
            composed.push((family, id, stage));
        }
        let listed: Vec<(&str, &str, &str)> = CLASSIFIED_CAPABILITIES
            .iter()
            .flat_map(|(family, ids)| ids.iter().map(|(id, stage)| (*family, *id, *stage)))
            .collect();
        let unlisted: Vec<_> = composed
            .iter()
            .filter(|entry| !listed.contains(entry))
            .collect();
        let retired: Vec<_> = listed
            .iter()
            .filter(|entry| !composed.contains(entry))
            .collect();
        assert!(
            unlisted.is_empty() && retired.is_empty(),
            "composed but not listed: {unlisted:?}; listed but not composed: {retired:?}"
        );

        let census = adoption_eligibility_census().expect("catalog census");
        let counts: Vec<(&str, u64, u64, u64)> = census
            .iter()
            .map(|observation| {
                (
                    observation.capability.as_str(),
                    observation.eligible,
                    observation.enabled,
                    observation.available,
                )
            })
            .collect();
        let count = |ids: &[(&str, &str)], stages: &[&str]| {
            ids.iter()
                .filter(|(_, stage)| stages.contains(stage))
                .count() as u64
        };
        // Families such as automation, work, and workflow compose no catalog
        // capability and so must be absent rather than a Known-zero population.
        let derived: Vec<(&str, u64, u64, u64)> = CLASSIFIED_CAPABILITIES
            .iter()
            .map(|(family, ids)| {
                (
                    *family,
                    ids.len() as u64,
                    count(ids, &["enabled", "available"]),
                    count(ids, &["available"]),
                )
            })
            .collect();
        assert_eq!(counts, derived);
    }

    #[tokio::test]
    async fn project_open_census_persists_known_coverage_family_observations() {
        let _pin = tracedecay_project::config::PinnedUserDataDir::new();
        let project = tempfile::tempdir().expect("project");
        let project_id = ProjectId::new("project.adoption.census").expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
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
