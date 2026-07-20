use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::configuration::{
    AnalyzerExecutableId, AnalyzerExecutableReferenceV1, AnalyzerLanguageId,
    AnalyzerLanguageSelectionV1, AnalyzerPrivacyClassV1, AnalyzerResourceLimitsV1,
    AnalyzerRestartPolicyV1, AnalyzerSettingsV1,
};
use tracedecay_domain::{CapabilityId, ManifestDigest, UtcMicros};
use tracedecay_policy::analyzer::{
    AnalyzerAdmissionDispositionV1, AnalyzerAdmissionEvaluator, AnalyzerAdmissionEvaluatorV1,
    AnalyzerAdmissionInputV1, AnalyzerAdmissionSnapshotV1, AnalyzerAvailabilityV1,
    AnalyzerCandidateV1, AnalyzerExecutionLocationV1,
};
use tracedecay_policy::authorization::PrivacyConstraintV1;
use tracedecay_policy::git::{
    GitConflictRiskV1, GitEffectAuthorizationV1, GitEffectClassificationInputV1,
    GitEffectClassifier, GitEffectClassifierV1, GitEffectDispositionV1, GitIndexEffectV1,
    GitPreviewPreconditionV1, GitRepositoryStateFactV1,
};
use tracedecay_policy::routing::{
    CapabilityAvailabilityV1, CapabilityEffectClassV1, CapabilityRouteCandidateV1,
    CapabilityRoutingDispositionV1, CapabilityRoutingEvaluator, CapabilityRoutingEvaluatorV1,
    CapabilityRoutingRequestV1, ScopeMatchV1, TruthFreshnessRequirementV1, TruthSourceStateV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identifier")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("valid fixture digest")
}

fn analyzer_settings() -> AnalyzerSettingsV1 {
    AnalyzerSettingsV1 {
        schema_version: AnalyzerSettingsV1::SCHEMA_VERSION,
        selections: vec![AnalyzerLanguageSelectionV1 {
            language_id: id::<AnalyzerLanguageId>("rust"),
            enabled: true,
            executable: AnalyzerExecutableReferenceV1::BuiltIn {
                executable_id: id::<AnalyzerExecutableId>("analyzer.rust"),
            },
            arguments: Vec::new(),
            initialization_options: BTreeMap::new(),
            settings: BTreeMap::new(),
            environment_allowlist: BTreeSet::new(),
            privacy_class: AnalyzerPrivacyClassV1::NonSensitive,
            resource_limits: AnalyzerResourceLimitsV1 {
                maximum_memory_mib: 256,
                startup_timeout_millis: 1_000,
                request_timeout_millis: 1_000,
            },
            restart_policy: AnalyzerRestartPolicyV1::RestartOnConfigurationChange,
        }],
    }
}

fn analyzer_input() -> AnalyzerAdmissionInputV1 {
    let capability = id::<CapabilityId>("capability.diagnostics.rust");
    AnalyzerAdmissionInputV1 {
        settings: analyzer_settings(),
        language_id: id::<AnalyzerLanguageId>("rust"),
        requested_capability: capability.clone(),
        candidates: vec![AnalyzerCandidateV1 {
            executable_id: id::<AnalyzerExecutableId>("analyzer.rust"),
            approved_external_digest: None,
            language_id: id::<AnalyzerLanguageId>("rust"),
            capability_id: capability,
            availability: AnalyzerAvailabilityV1::Available,
            execution_location: AnalyzerExecutionLocationV1::Local,
            scope_authorized: true,
            available_memory_mib: 512,
            catalog_digest: digest('a'),
        }],
        privacy_constraints: BTreeSet::new(),
        configuration_digest: digest('b'),
        policy_revision: 1,
        policy_digest: digest('c'),
        evaluated_at: UtcMicros(1),
    }
}

#[test]
fn analyzer_admission_requires_configured_available_scoped_local_candidate() {
    let evaluator = AnalyzerAdmissionEvaluatorV1::default();
    let input = analyzer_input();

    let decision = evaluator.evaluate(&input);

    assert_eq!(decision.disposition, AnalyzerAdmissionDispositionV1::Allow);
    assert_eq!(
        decision.selected_executable_id,
        Some(id::<AnalyzerExecutableId>("analyzer.rust"))
    );

    let mut approved_external = input.clone();
    let external_digest = digest('9');
    approved_external.settings.selections[0].executable =
        AnalyzerExecutableReferenceV1::ApprovedExternal {
            executable_digest: external_digest.clone(),
        };
    approved_external.candidates[0].approved_external_digest = Some(external_digest);
    assert_eq!(
        evaluator.evaluate(&approved_external).disposition,
        AnalyzerAdmissionDispositionV1::Allow
    );

    let mut privacy_restricted = input;
    privacy_restricted.privacy_constraints = BTreeSet::from([PrivacyConstraintV1::LocalOnly]);
    privacy_restricted.candidates[0].execution_location = AnalyzerExecutionLocationV1::External;
    let denied = evaluator.evaluate(&privacy_restricted);

    assert_eq!(denied.disposition, AnalyzerAdmissionDispositionV1::Deny);
    assert!(denied.selected_executable_id.is_none());
}

#[test]
fn analyzer_snapshot_pins_exact_policy_and_configuration_inputs() {
    let evaluator = AnalyzerAdmissionEvaluatorV1::default();
    let input = analyzer_input();
    let snapshot = evaluator.snapshot(&input);

    assert!(snapshot.is_bound_to(&input));
    assert_eq!(
        snapshot,
        AnalyzerAdmissionSnapshotV1::compose(&evaluator, &input)
    );

    let mut policy_drift = input;
    policy_drift.policy_digest = digest('f');
    assert!(!snapshot.is_bound_to(&policy_drift));
}

fn route_candidate(
    capability_id: CapabilityId,
    availability: CapabilityAvailabilityV1,
) -> CapabilityRouteCandidateV1 {
    CapabilityRouteCandidateV1 {
        capability_id,
        availability,
        scope_match: ScopeMatchV1::Match,
        effect_class: CapabilityEffectClassV1::Read,
        truth_source_state: TruthSourceStateV1::Fresh,
        catalog_digest: digest('d'),
        priority: 0,
    }
}

#[test]
fn routing_uses_only_explicitly_declared_capability_order() {
    let unavailable = id::<CapabilityId>("capability.exact");
    let declared_fallback = id::<CapabilityId>("capability.declared-fallback");
    let evaluator = CapabilityRoutingEvaluatorV1::default();

    let no_fallback = evaluator.evaluate(&CapabilityRoutingRequestV1 {
        declared_capability_order: vec![unavailable.clone()],
        candidates: vec![
            route_candidate(unavailable.clone(), CapabilityAvailabilityV1::Unavailable),
            route_candidate(
                declared_fallback.clone(),
                CapabilityAvailabilityV1::Available,
            ),
        ],
        authorized_capabilities: BTreeSet::from([unavailable.clone(), declared_fallback.clone()]),
        required_effect_class: CapabilityEffectClassV1::Read,
        required_freshness: TruthFreshnessRequirementV1::Fresh,
        policy_revision: 1,
        policy_digest: digest('e'),
        configuration_digest: digest('f'),
        evaluated_at: UtcMicros(1),
    });
    assert_eq!(
        no_fallback.disposition,
        CapabilityRoutingDispositionV1::Indeterminate
    );
    assert!(no_fallback.selected_capability_id.is_none());

    let explicit_fallback = evaluator.evaluate(&CapabilityRoutingRequestV1 {
        declared_capability_order: vec![unavailable, declared_fallback.clone()],
        candidates: vec![
            route_candidate(
                id::<CapabilityId>("capability.exact"),
                CapabilityAvailabilityV1::Unavailable,
            ),
            route_candidate(
                declared_fallback.clone(),
                CapabilityAvailabilityV1::Available,
            ),
        ],
        authorized_capabilities: BTreeSet::from([
            id::<CapabilityId>("capability.exact"),
            declared_fallback.clone(),
        ]),
        required_effect_class: CapabilityEffectClassV1::Read,
        required_freshness: TruthFreshnessRequirementV1::Fresh,
        policy_revision: 1,
        policy_digest: digest('e'),
        configuration_digest: digest('f'),
        evaluated_at: UtcMicros(1),
    });
    assert_eq!(
        explicit_fallback.disposition,
        CapabilityRoutingDispositionV1::Allow
    );
    assert_eq!(
        explicit_fallback.selected_capability_id,
        Some(declared_fallback)
    );
}

#[test]
fn git_effect_classifier_allows_only_typed_index_effects_with_current_preview() {
    let classifier = GitEffectClassifierV1::default();
    let snapshot = GitRepositoryStateFactV1::new("repository.state.v1.fixture", digest('1'), true)
        .expect("valid repository state fact");
    let input = GitEffectClassificationInputV1 {
        effect: GitIndexEffectV1::StageHunks,
        authorization: GitEffectAuthorizationV1 {
            capability_granted: true,
            owner_scope_matches: true,
        },
        repository_state: snapshot.clone(),
        expected_preview_digest: Some(digest('2')),
        preview: Some(GitPreviewPreconditionV1 {
            preview_digest: digest('2'),
            repository_state_id: snapshot.snapshot_id.clone(),
        }),
        conflict_risk: GitConflictRiskV1::NoneKnown,
        policy_revision: 1,
        policy_digest: digest('3'),
        configuration_digest: digest('4'),
        evaluated_at: UtcMicros(1),
    };

    assert_eq!(
        classifier.evaluate(&input).disposition,
        GitEffectDispositionV1::Allow
    );

    let mut stale_preview = input.clone();
    stale_preview.preview = None;
    assert_eq!(
        classifier.evaluate(&stale_preview).disposition,
        GitEffectDispositionV1::Deny
    );

    let mut mismatched_preview = input;
    mismatched_preview.expected_preview_digest = Some(digest('5'));
    assert_eq!(
        classifier.evaluate(&mismatched_preview).disposition,
        GitEffectDispositionV1::Deny
    );

    assert_eq!(
        serde_json::to_value(GitIndexEffectV1::StageHunks).expect("effect serializes"),
        serde_json::json!("stage_hunks")
    );
    assert!(serde_json::from_value::<GitIndexEffectV1>(serde_json::json!("merge")).is_err());
}
