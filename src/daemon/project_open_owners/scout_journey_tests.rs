use super::*;

use crate::agents::context_scout_v2::{
    ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutEvidenceEnvelopeV1, ContextScoutEvidenceSourceKindV1,
    ContextScoutEvidenceSourceReceiptV1, ContextScoutFeedbackKindV1, ContextScoutFeedbackV1,
    ContextScoutOutcomeV1, ContextScoutRedactionReceiptV1, ContextScoutRuntimeOutcomeV1,
    context_scout_delivery_receipt_id,
};
use tracedecay_application::{
    AuthorityReceipt, CoverageCompleteness, CoverageDomainState, DisclosureClass, EvidenceCoverage,
    EvidenceDomain, FreshnessState, PolicyDecisionRef, RetrieverContributionState, TemporalState,
};
use tracedecay_domain::feedback::{FeedbackContentIdentityV1, FeedbackScopeV1};
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ManifestDigest, RetrievalAnchorId, TemporalModeV1,
};

fn typed_id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(character: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", character.to_string().repeat(64))).unwrap()
}

pub(super) fn configured_model_evidence(marker: u8) -> ContextScoutEvidenceEnvelopeV1 {
    let scope = ResolvedScope::new(
        typed_id("project.scout.configured-model"),
        typed_id("repository.scout.configured-model"),
        typed_id("worktree.scout.configured-model"),
        Some(typed_id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    let generation =
        typed_id::<CodeGenerationId>(&format!("generation.scout.configured-model.{marker}"));
    ContextScoutEvidenceEnvelopeV1::claim(
        FeedbackScopeV1 {
            project_id: scope.project_id.clone(),
            repository_id: scope.repository_id.clone(),
            worktree_id: scope.worktree_id.clone(),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: typed_id("commit.scout.configured-model"),
        },
        scope.clone(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        },
        generation.clone(),
        AuthorityReceipt {
            grant_id: typed_id("grant.scout.configured-model"),
            grant_revision: 1,
            grant_digest: digest('a'),
            authorized_scope_digest: scope.scope_digest.clone(),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.scout.configured-model",
                1,
                digest('b'),
                ComponentVersion::new("policy.scout.configured-model.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(1),
        },
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        },
        vec![ContextScoutEvidenceSourceReceiptV1 {
            source: ContextScoutEvidenceSourceKindV1::Query,
            contribution_state: RetrieverContributionState::Completed,
            temporal: TemporalState {
                requested_mode: TemporalModeV1::Current,
                requested_at: UtcMicros(1),
                resolved_at: UtcMicros(2),
                source_generation: Some(generation),
                watermark_digest: Some(digest('e')),
                freshness: FreshnessState::Current,
            },
            coverage: EvidenceCoverage {
                requested_domains: vec![EvidenceDomain::Anchor],
                visited: Some(1),
                eligible: Some(1),
                returned: 1,
                completeness: CoverageCompleteness::Complete,
                domains: vec![CoverageDomainState {
                    domain: EvidenceDomain::Anchor,
                    completeness: CoverageCompleteness::Complete,
                }],
            },
            anchors: vec![typed_id::<RetrievalAnchorId>(&format!(
                "anchor.scout.configured-model.{marker}"
            ))],
        }],
        UtcMicros(2),
    )
    .unwrap()
}

fn configured_model_input_at(
    configuration_revision: [u8; 32],
    marker: u8,
    now: UtcMicros,
    delivery_window: ContextScoutDeliveryWindowV1,
) -> crate::agents::context_scout_v2::ContextScoutSelectionInputV1 {
    crate::agents::context_scout_v2::ContextScoutSelectionInputV1 {
        address: crate::agents::context_scout_v2::ContextScoutAddressV1 {
            profile_id: [1; 16],
            provider_id: [2; 16],
            protected_session_id: [3; 32],
            thread_id: [4; 16],
            turn_id: [5; 16],
            agent_id: [6; 16],
            logical_message_id: [7; 16],
            project_id: [8; 16],
        },
        input_watermark: [marker; 32],
        configuration_revision,
        envelope_id: [marker; 16],
        now,
        delivery_window,
        delivered_dedupe_keys: BTreeSet::new(),
        candidates: vec![crate::agents::context_scout_v2::ContextScoutCandidateV1 {
            dedupe_key: [marker; 32],
            category: crate::agents::context_scout_v2::ContextScoutCategoryV1::Retrieval,
            relevance_score: 10,
            suggestion_text: "Use the admitted evidence.".to_owned(),
            evidence: configured_model_evidence(marker),
            expires_at: UtcMicros(now.0.saturating_add(60 * 1_000_000)),
        }],
    }
}

fn configured_model_pin() -> ContextScoutConfigurationPinV1 {
    let setting_key = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    )
    .expect("Scout setting key");
    let revision =
        tracedecay_domain::configuration::ConfigurationRevisionId::new("revision.scout.model")
            .expect("configuration revision");
    let settings = tracedecay_domain::configuration::ContextScoutSettingsV1 {
        schema_version: tracedecay_domain::configuration::ContextScoutSettingsV1::SCHEMA_VERSION,
        state: tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active,
        mode: tracedecay_domain::configuration::ContextScoutConfigurationModeV1::ConfiguredModel,
        limits:
            tracedecay_domain::configuration::ContextScoutConfigurationLimitsV1::bounded_defaults(),
        model_path: Some(
            tracedecay_domain::configuration::ContextScoutConfiguredModelPathV1::CodexAppServer,
        ),
    };
    let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
        BTreeMap::from([(
            setting_key.clone(),
            ConfigurationValueV1::ContextScoutSettings(settings),
        )]),
        BTreeMap::from([(
            setting_key,
            vec![tracedecay_domain::configuration::ConfigurationCandidateV1 {
                layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                    project_id: ProjectId::new("project.scout.model").expect("project id"),
                },
                revision_id: revision.clone(),
                disposition: tracedecay_domain::configuration::CandidateDispositionV1::Winning,
                safe_reason: None,
            }],
        )]),
    )
    .expect("configuration snapshot");
    ContextScoutConfigurationPinV1::from_current(
        &crate::application::configuration::ConfigurationCurrentStateV1 {
            revision_id: revision,
            snapshot,
        },
    )
    .expect("configured-model pin")
}

async fn test_scout_owner(
    temporary: &tempfile::TempDir,
) -> Arc<crate::agents::context_scout_owner::ProjectContextScoutOwnerV1> {
    crate::daemon::store_runtime::register_registered_schema_installer();
    let database_path = temporary.path().join("edit-stop-feedback.db");
    let database_authority =
        crate::db::DatabaseAuthority::acquire_test(&database_path, "edit stop feedback")
            .expect("database authority");
    let database = crate::db::Database::publish_test_runtime(
        &database_path,
        &database_authority,
        crate::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("project database")
    .0;
    crate::agents::context_scout_owner::ProjectContextScoutOwnerV1::startup(
        database,
        [8; 16],
        UtcMicros(1),
        None,
    )
    .await
    .expect("Scout owner")
}

#[tokio::test]
async fn project_open_edit_stop_and_explicit_feedback_preserve_privacy_and_supersession() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let profile_root = temporary.path().join("profile");
    let dashboard_root = temporary.path().join("dashboard");
    std::fs::create_dir_all(&profile_root).expect("create profile");
    std::fs::create_dir_all(&dashboard_root).expect("create dashboard root");
    let pin = configured_model_pin();
    let control = pin.control();
    let owner = test_scout_owner(&temporary).await;
    install_project_open_context_scout_configuration(
        owner.as_ref(),
        pin,
        &profile_root,
        &dashboard_root,
    )
    .await
    .expect("install project-open Scout configuration");
    let now = UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_micros(),
        )
        .expect("microsecond clock"),
    );

    let first_edit = configured_model_input_at(
        control.configuration_revision,
        20,
        now,
        ContextScoutDeliveryWindowV1::NextBoundary,
    );
    let ContextScoutRuntimeOutcomeV1::Enqueued {
        entry: first,
        store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
    } = owner
        .prepare_configured(
            &first_edit,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("first saved edit")
    else {
        panic!("first saved edit must enqueue");
    };

    let second_edit = configured_model_input_at(
        control.configuration_revision,
        21,
        UtcMicros(now.0 + 1),
        ContextScoutDeliveryWindowV1::NextBoundary,
    );
    let ContextScoutRuntimeOutcomeV1::Enqueued {
        entry: second,
        store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
    } = owner
        .prepare_configured(
            &second_edit,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("superseding saved edit")
    else {
        panic!("second saved edit must supersede the first");
    };
    assert!(matches!(
        owner.cancel(first.work).await,
        Err(crate::agents::context_scout_v2::ContextScoutErrorV1::StaleWork)
    ));

    let stop = configured_model_input_at(
        control.configuration_revision,
        22,
        UtcMicros(now.0 + 2),
        ContextScoutDeliveryWindowV1::Immediate,
    );
    let ContextScoutRuntimeOutcomeV1::Enqueued {
        entry: stopped,
        store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
    } = owner
        .prepare_configured(
            &stop,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("stop boundary")
    else {
        panic!("stop boundary must supersede delayed edit guidance");
    };
    assert_ne!(second.work, stopped.work);

    let hook = tracedecay_hooks::HookEventEnvelopeV2 {
        schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
        event_id: [60; 16],
        producer: tracedecay_hooks::HookHostV1::Codex,
        protected_session_id: stop.address.protected_session_id,
        project_id: stop.address.project_id,
        repository_id: [61; 16],
        worktree_id: [62; 16],
        worktree_epoch: 1,
        binding_token: [63; 32],
        ordering: tracedecay_hooks::HookOrderingV1::Unknown,
        observed_at: UtcMicros(now.0 + 3),
        event: tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
        },
    };
    let (guidance, claim) = owner
        .claim_ready_guidance_exact(
            &hook,
            stop.address,
            stop.input_watermark,
            1,
            UtcMicros(now.0 + 3),
        )
        .await
        .expect("exact stop guidance claim");
    assert_eq!(claim.entry, *stopped);
    assert_eq!(guidance.text, "Use the admitted evidence.");
    assert_eq!(
        claim.entry.envelope.candidate.evidence.redaction,
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        }
    );

    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(
            hook.event_id,
            claim.entry.envelope.envelope_id,
        ),
        envelope_id: claim.entry.envelope.envelope_id,
        delivered_at: UtcMicros(now.0 + 4),
        outcome: ContextScoutOutcomeV1::Displayed,
    };
    assert_eq!(
        owner.record_delivery(&claim, &receipt).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    let before_feedback = owner
        .recent_exact(stop.address, 8)
        .await
        .expect("recent delivery");
    assert!(before_feedback.deliveries[0].feedback.is_none());

    let feedback = ContextScoutFeedbackV1 {
        receipt_id: receipt.receipt_id,
        kind: ContextScoutFeedbackKindV1::ExplicitlyAccepted,
    };
    assert_eq!(
        owner
            .record_feedback_exact(stop.address, &receipt, feedback)
            .await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    let recent = owner
        .recent_exact(stop.address, 8)
        .await
        .expect("explicit feedback receipt");
    assert_eq!(recent.pending.len(), 0);
    assert_eq!(recent.deliveries.len(), 1);
    assert_eq!(recent.deliveries[0].feedback, Some(feedback));
    let serialized = serde_json::to_string(&recent).expect("serialize bounded recent state");
    assert!(!serialized.contains("raw source"));
    assert!(!serialized.contains("prompt"));
    assert!(!serialized.contains("secret-token"));
}
