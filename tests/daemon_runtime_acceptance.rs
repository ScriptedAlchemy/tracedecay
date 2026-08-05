//! Mandatory daemon journey over production startup authorities.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracedecay::agents::context_scout_v2::{
    ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1, ContextScoutDecisionV1,
    ContextScoutDeliveryWindowV1, ContextScoutEvidenceEnvelopeV1, ContextScoutEvidenceSourceKindV1,
    ContextScoutEvidenceSourceReceiptV1, ContextScoutLimitsV1, ContextScoutRedactionReceiptV1,
    ContextScoutSelectionInputV1, select_deterministic_context_scout,
};
use tracedecay::agents::host_bundle_v2::{
    HostKindV1, HostRegistrationRouteV1, stock_host_registration_evidence,
};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_application::{
    AuthorityReceipt, CoverageCompleteness, CoverageDomainState, DisclosureClass, EvidenceCoverage,
    EvidenceDomain, FreshnessState, PolicyDecisionRef, ResolvedScope, RetrieverContributionState,
    TemporalState, feedback_surface_catalog_contribution,
};
use tracedecay_domain::feedback::{FeedbackContentIdentityV1, FeedbackScopeV1};
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ManifestDigest, RefId, RetrievalAnchorId, TemporalModeV1,
    UtcMicros,
};
use tracedecay_hooks::{
    HookConfigurationFileReaderV1, HookConfigurationReadOutcomeV1, HookConfigurationSubscriberV1,
    HookEventFamily, HookHostV1, HookSpoolConfigV1, HookSpoolV1, NativeEnvelopeMaterialV1,
    decode_bound_native_hook_event, decode_native_hook_event, hook_configuration_path,
};
use tracedecay_tool_catalog::BindingSurface;

mod common;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(character: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", character.to_string().repeat(64))).unwrap()
}

fn scout_evidence(now: UtcMicros) -> ContextScoutEvidenceEnvelopeV1 {
    let scope = ResolvedScope::new(
        id("project.scout.acceptance"),
        id("repository.scout.acceptance"),
        id("worktree.scout.acceptance"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    let generation = id::<CodeGenerationId>("generation.scout.acceptance");
    ContextScoutEvidenceEnvelopeV1::claim(
        FeedbackScopeV1 {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
            worktree_id: scope.worktree_id.clone(),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: id("commit.scout.acceptance"),
        },
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        },
        generation.clone(),
        AuthorityReceipt {
            grant_id: id("grant.scout.acceptance"),
            grant_revision: 1,
            grant_digest: digest('a'),
            authorized_scope_digest: scope.scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.scout.acceptance",
                1,
                digest('b'),
                ComponentVersion::new("policy.scout.acceptance.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(now.0 - 1),
        },
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        vec![ContextScoutEvidenceSourceReceiptV1 {
            source: ContextScoutEvidenceSourceKindV1::Code,
            contribution_state: RetrieverContributionState::Completed,
            temporal: TemporalState {
                requested_mode: TemporalModeV1::Current,
                requested_at: UtcMicros(now.0 - 1),
                resolved_at: now,
                source_generation: Some(generation),
                watermark_digest: Some(digest('e')),
                freshness: FreshnessState::Current,
            },
            coverage: EvidenceCoverage {
                requested_domains: vec![EvidenceDomain::Diagnostic],
                visited: Some(1),
                eligible: Some(1),
                returned: 1,
                completeness: CoverageCompleteness::Complete,
                domains: vec![CoverageDomainState {
                    domain: EvidenceDomain::Diagnostic,
                    completeness: CoverageCompleteness::Complete,
                }],
            },
            anchors: vec![id::<RetrievalAnchorId>("anchor.scout.acceptance")],
        }],
        now,
    )
    .unwrap()
}

#[tokio::test]
async fn authentic_callback_to_all_delivery_surfaces() {
    let (_environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn advisory_callback() {}\n",
    )
    .unwrap();
    TraceDecay::init(&project)
        .await
        .expect("production project initialization");
    let project_runtime = TraceDecay::open(&project)
        .await
        .expect("production project-open startup");

    let callback = include_bytes!(
        "../crates/tracedecay-hooks/fixtures/host_events/claude/post_tool_use_write.json"
    );
    let decoded =
        decode_native_hook_event(HookHostV1::ClaudeCode, callback).expect("authentic callback");

    let layout = project_runtime.store_layout();
    let subscriber = HookConfigurationSubscriberV1::new(HookConfigurationFileReaderV1::new(
        hook_configuration_path(&layout.data_root, HookHostV1::ClaudeCode),
    ));
    let now = UtcMicros(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_micros()
            .try_into()
            .expect("microsecond clock"),
    );
    let HookConfigurationReadOutcomeV1::Bound(configuration) =
        subscriber.load_current(HookHostV1::ClaudeCode, now)
    else {
        panic!("project-open must publish the Hook V2 binding");
    };
    let envelope = decode_bound_native_hook_event(
        HookHostV1::ClaudeCode,
        callback,
        &configuration.binding,
        NativeEnvelopeMaterialV1 {
            event_id: [1; 16],
            protected_session_id: [2; 32],
            observed_at: now,
            tool_id: (decoded.family() == HookEventFamily::ToolLifecycle).then_some([3; 16]),
            effect_receipt_id: Some([4; 16]),
            file_id: (decoded.family() == HookEventFamily::SavedEdit).then_some([5; 16]),
            changed_range_count: 1,
        },
    )
    .expect("authentic callback binds to project-open Hook V2 scope");
    assert_eq!(envelope.event.family(), decoded.family());

    let spool_root = layout.data_root.join("advisory-hook-replay");
    let (mut spool, _) = HookSpoolV1::open(
        &spool_root,
        HookSpoolConfigV1::stock(HookHostV1::ClaudeCode),
        now,
    )
    .expect("Hook V2 replay spool");
    let appended = spool
        .append(envelope, &configuration.binding, now)
        .expect("admitted callback persists for replay");
    let replay = spool
        .claim_replay_batches(UtcMicros(now.0 + 1), 1)
        .expect("Hook V2 replay claim");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].records[0].sequence, appended.sequence);

    let scout = select_deterministic_context_scout(
        &ContextScoutSelectionInputV1 {
            address: ContextScoutAddressV1 {
                profile_id: [1; 16],
                provider_id: [2; 16],
                protected_session_id: [3; 32],
                thread_id: [4; 16],
                turn_id: [5; 16],
                agent_id: [6; 16],
                logical_message_id: [7; 16],
                project_id: [8; 16],
            },
            input_watermark: [9; 32],
            configuration_revision: [10; 32],
            envelope_id: [11; 16],
            now,
            delivery_window: ContextScoutDeliveryWindowV1::Immediate,
            delivered_dedupe_keys: BTreeSet::new(),
            candidates: vec![ContextScoutCandidateV1 {
                dedupe_key: [12; 32],
                category: ContextScoutCategoryV1::Coordination,
                relevance_score: 10_000,
                suggestion_text: "Another active agent edited this file.".to_owned(),
                evidence: scout_evidence(now),
                expires_at: UtcMicros(now.0 + 1_000_000),
            }],
        },
        ContextScoutLimitsV1::bounded_defaults(),
    )
    .expect("Scout production selector");
    assert!(matches!(scout, ContextScoutDecisionV1::Ready { .. }));

    let feedback = feedback_surface_catalog_contribution().expect("feedback surface catalog");
    let surfaces = feedback
        .bindings()
        .iter()
        .map(|binding| binding.surface())
        .collect::<BTreeSet<_>>();
    assert!(surfaces.contains(&BindingSurface::Http));
    assert!(surfaces.contains(&BindingSurface::Mcp));
    assert!(surfaces.contains(&BindingSurface::Cli));
    assert!(surfaces.contains(&BindingSurface::Lsp));

    let registrations = stock_host_registration_evidence(HostKindV1::ClaudeCode)
        .into_iter()
        .map(|evidence| evidence.route)
        .collect::<Vec<_>>();
    assert!(registrations.contains(&HostRegistrationRouteV1::Hook));
    assert!(registrations.contains(&HostRegistrationRouteV1::ClaudeConfiguredLanguageLsp));
    assert!(registrations.contains(&HostRegistrationRouteV1::Mcp));
    assert!(registrations.contains(&HostRegistrationRouteV1::Cli));
    for operation in [
        "feedback_diagnostics",
        "feedback_get",
        "feedback_expand",
        "feedback_list",
    ] {
        let bindings = feedback
            .bindings()
            .iter()
            .filter(|binding| binding.operation().as_str() == operation)
            .map(|binding| binding.surface())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            bindings,
            BTreeSet::from([
                BindingSurface::Cli,
                BindingSurface::Mcp,
                BindingSurface::Http,
                BindingSurface::Dashboard,
            ]),
            "{operation} must bind every canonical publication read surface"
        );
    }
}

#[tokio::test]
async fn exact_search_does_not_wait_for_semantic_projection() {
    const STARTUP_DEADLINE: Duration = Duration::from_secs(5);

    let (_environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn semantic_startup_probe() -> &'static str { \"exact\" }\n",
    )
    .unwrap();
    let initialized =
        TraceDecay::init_and_index_with_options(&project, TraceDecayOpenOptions::default())
            .await
            .expect("production project initialization and exact indexing");
    initialized.close();

    let runtime = tokio::time::timeout(STARTUP_DEADLINE, TraceDecay::open(&project))
        .await
        .expect("project-open readiness must not wait for semantic model loading")
        .expect("production project-open startup");
    let exact = tokio::time::timeout(
        STARTUP_DEADLINE,
        runtime.search("semantic_startup_probe", 8),
    )
    .await
    .expect("exact search must not wait for semantic projection")
    .expect("exact search remains healthy");
    assert!(
        exact
            .iter()
            .any(|result| result.node.name == "semantic_startup_probe")
    );
}
