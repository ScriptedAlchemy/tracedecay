use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::*;

use tracedecay_agent_hosts::agents::context_scout::{
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutDurableStoreV1, ContextScoutEvidenceEnvelopeExt, context_scout_delivery_receipt_id,
};
use tracedecay_contracts::context_scout::{
    ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1,
    ContextScoutClaimHandleV1, ContextScoutClaimRequestV1, ContextScoutClaimWindowV1,
    ContextScoutDeliveryOutcomeV1, ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1,
    ContextScoutEvidenceEnvelopeV1, ContextScoutEvidenceSourceKindV1,
    ContextScoutEvidenceSourceReceiptV1, ContextScoutFeedbackKindV1, ContextScoutFeedbackV1,
    ContextScoutLeaseV1, ContextScoutRedactionReceiptV1,
};
use tracedecay_contracts::{
    AuthorityReceipt, CoverageCompleteness, CoverageDomainState, DisclosureClass, EvidenceCoverage,
    EvidenceDomain, FreshnessState, IdempotencyKey, PolicyDecisionRef, ResolvedScope,
    RetrieverContributionState, TemporalState,
};
use tracedecay_domain::configuration::ConfigurationValueV1;
use tracedecay_domain::feedback::FeedbackContentIdentityV1;
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ManifestDigest, ProjectId, RefId, RetrievalAnchorId,
    TemporalModeV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

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

fn configured_model_evidence(marker: u8) -> ContextScoutEvidenceEnvelopeV1 {
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
) -> tracedecay_agent_hosts::agents::context_scout::ContextScoutSelectionInputV1 {
    tracedecay_agent_hosts::agents::context_scout::ContextScoutSelectionInputV1 {
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
        input_watermark: [marker; 32],
        configuration_revision,
        envelope_id: [marker; 16],
        now,
        delivery_window,
        delivered_dedupe_keys: BTreeSet::new(),
        candidates: vec![ContextScoutCandidateV1 {
            dedupe_key: [marker; 32],
            category: ContextScoutCategoryV1::Retrieval,
            relevance_score: 10,
            suggestion_text: "Use the admitted evidence.".to_owned(),
            evidence: configured_model_evidence(marker),
            expires_at: UtcMicros(now.0.saturating_add(60 * 1_000_000)),
        }],
    }
}

fn configured_model_pin_with_timeout(
    revision: &str,
    model_timeout_secs: u64,
) -> ContextScoutConfigurationPinV1 {
    let setting_key = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    )
    .expect("Scout setting key");
    let revision = tracedecay_domain::configuration::ConfigurationRevisionId::new(revision)
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
        model_id: Some("gpt-5.6-mini".to_owned()),
        model_timeout_secs: Some(model_timeout_secs),
    };
    settings.validate().expect("configured-model settings");
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
        &tracedecay_global_db::configuration::contracts::ports::ConfigurationCurrentStateV1 {
            revision_id: revision,
            snapshot,
        },
    )
    .expect("configured-model pin")
}

fn configured_model_pin() -> ContextScoutConfigurationPinV1 {
    configured_model_pin_with_timeout("revision.scout.model", 30)
}

async fn test_scout_owner(
    temporary: &tempfile::TempDir,
) -> Arc<tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1> {
    tracedecay_store_runtime::register_registered_schema_installer();
    let database_path = temporary.path().join("edit-stop-feedback.db");
    let database_authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        &database_path,
        "edit stop feedback",
    )
    .expect("database authority");
    let database = tracedecay_runtime_core::db::Database::publish_test_runtime(
        &database_path,
        &database_authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .expect("project database")
    .0;
    tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1::startup(
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
    use tracedecay_automation_runtime::automation::config::{AutomationBackend, AutomationConfig};

    let temporary = tempfile::tempdir().expect("temporary directory");
    let model_config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        ..AutomationConfig::default()
    };
    let pin = configured_model_pin();
    let control = pin.control();
    let owner = test_scout_owner(&temporary).await;
    install_project_open_context_scout_configuration(owner.as_ref(), pin, &model_config)
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
        Err(tracedecay_agent_hosts::agents::context_scout::ContextScoutErrorV1::StaleWork)
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

    let claimable_input = configured_model_input_at(
        control.configuration_revision,
        23,
        UtcMicros(now.0 + 3),
        ContextScoutDeliveryWindowV1::IdleWindow,
    );
    let ContextScoutRuntimeOutcomeV1::Enqueued {
        entry: claimable,
        store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
    } = owner
        .prepare_configured(
            &claimable_input,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("claimable idle guidance")
    else {
        panic!("idle guidance must enqueue before claim");
    };
    let delivery_event_id = [60; 16];
    let claim_request = ContextScoutClaimRequestV1 {
        address: stop.address,
        window: ContextScoutClaimWindowV1::IdleWindow,
        idempotency_key: IdempotencyKey::new("context-scout.claim.stop").expect("claim key"),
    };
    let claim = match owner
        .claim_delivery_request(
            &claim_request,
            UtcMicros(now.0 + 3),
            UtcMicros(now.0 + 30_000_000),
        )
        .await
    {
        ContextScoutDurableClaimOutcomeV1::Claimed(claim) => claim,
        other => panic!("exact stop guidance must claim, got {other:?}"),
    };
    assert_eq!(
        owner
            .claim_delivery_request(
                &claim_request,
                UtcMicros(now.0 + 4),
                UtcMicros(now.0 + 31_000_000),
            )
            .await,
        ContextScoutDurableClaimOutcomeV1::Claimed(claim.clone())
    );
    let competing_request = ContextScoutClaimRequestV1 {
        idempotency_key: IdempotencyKey::new("context-scout.claim.competing")
            .expect("competing claim key"),
        ..claim_request.clone()
    };
    assert_eq!(
        owner
            .claim_delivery_request(
                &competing_request,
                UtcMicros(now.0 + 4),
                UtcMicros(now.0 + 31_000_000),
            )
            .await,
        ContextScoutDurableClaimOutcomeV1::Empty
    );
    assert_eq!(claim.entry, *claimable);
    assert_eq!(
        claim.entry.envelope.candidate.suggestion_text,
        "Use the admitted evidence."
    );
    assert_eq!(
        claim.entry.envelope.candidate.evidence.redaction,
        ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        }
    );

    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(
            delivery_event_id,
            claim.entry.envelope.envelope_id,
        ),
        envelope_id: claim.entry.envelope.envelope_id,
        delivered_at: UtcMicros(now.0 + 4),
        outcome: ContextScoutDeliveryOutcomeV1::Displayed,
    };
    let claim_handle = ContextScoutClaimHandleV1 {
        work: claim.entry.work,
        envelope_id: claim.entry.envelope.envelope_id,
        lease_id: claim.lease.lease_id,
        lease_expires_at: claim.lease.expires_at,
    };
    assert_eq!(
        owner
            .record_delivery_by_handle(&claim_handle, &receipt)
            .await,
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

    let cancelled_input = configured_model_input_at(
        control.configuration_revision,
        24,
        UtcMicros(now.0 + 5),
        ContextScoutDeliveryWindowV1::OnRequest,
    );
    let ContextScoutRuntimeOutcomeV1::Enqueued {
        entry: cancelled,
        store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
    } = owner
        .prepare_configured(
            &cancelled_input,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("cancellable request guidance")
    else {
        panic!("request guidance must enqueue before cancellation");
    };
    assert_eq!(
        owner
            .cancel(cancelled.work)
            .await
            .expect("cancel current work"),
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    assert!(
        owner
            .recent_exact(cancelled_input.address, 8)
            .await
            .expect("recent state after cancellation")
            .pending
            .is_empty()
    );
}

#[tokio::test]
async fn claim_refuses_work_from_a_displaced_configuration_revision() {
    use tracedecay_automation_runtime::automation::config::{AutomationBackend, AutomationConfig};

    let temporary = tempfile::tempdir().expect("temporary directory");
    let model_config = AutomationConfig {
        enabled: true,
        backend: AutomationBackend::CodexAppServer,
        ..AutomationConfig::default()
    };
    let first_pin = configured_model_pin_with_timeout("revision.scout.claim.first", 30);
    let first_control = first_pin.control();
    let owner = test_scout_owner(&temporary).await;
    install_project_open_context_scout_configuration(owner.as_ref(), first_pin, &model_config)
        .await
        .expect("install first Scout configuration");
    let now = UtcMicros(1_000_000);
    let input = configured_model_input_at(
        first_control.configuration_revision,
        31,
        now,
        ContextScoutDeliveryWindowV1::IdleWindow,
    );
    let ContextScoutRuntimeOutcomeV1::Enqueued {
        entry,
        store_outcome: ContextScoutDurableStoreOutcomeV1::Stored,
    } = owner
        .prepare_configured(
            &input,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("first-revision guidance")
    else {
        panic!("first-revision guidance must enqueue");
    };
    install_project_open_context_scout_configuration(
        owner.as_ref(),
        configured_model_pin_with_timeout("revision.scout.claim.second", 31),
        &model_config,
    )
    .await
    .expect("install replacement Scout configuration");

    let request = ContextScoutClaimRequestV1 {
        address: input.address,
        window: ContextScoutClaimWindowV1::IdleWindow,
        idempotency_key: IdempotencyKey::new("context-scout.claim.displaced").expect("claim key"),
    };
    assert_eq!(
        owner
            .claim_delivery_request(
                &request,
                UtcMicros(now.0 + 1),
                UtcMicros(now.0 + 30_000_000)
            )
            .await,
        ContextScoutDurableClaimOutcomeV1::Empty
    );
    assert!(matches!(
        owner
            .store()
            .claim(
                input.address,
                UtcMicros(now.0 + 2),
                ContextScoutLeaseV1 {
                    lease_id: [32; 16],
                    expires_at: UtcMicros(now.0 + 30_000_000),
                },
            )
            .await,
        ContextScoutDurableClaimOutcomeV1::Claimed(claim) if claim.entry == *entry
    ));
}

/// Disabled is the only stock state: the registry default renders the flag
/// off and a disabled pin suppresses the producer without enqueueing work.
#[tokio::test]
async fn stock_disabled_configuration_produces_nothing() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let owner = test_scout_owner(&temporary).await;
    let setting_key = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    )
    .expect("Scout setting key");
    let revision =
        tracedecay_domain::configuration::ConfigurationRevisionId::new("revision.scout.disabled")
            .expect("configuration revision");
    let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
        BTreeMap::from([(
            setting_key.clone(),
            ConfigurationValueV1::ContextScoutSettings(
                tracedecay_domain::configuration::ContextScoutSettingsV1::disabled(),
            ),
        )]),
        BTreeMap::from([(
            setting_key,
            vec![tracedecay_domain::configuration::ConfigurationCandidateV1 {
                layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                    project_id: ProjectId::new("project.scout.disabled").expect("project id"),
                },
                revision_id: revision.clone(),
                disposition: tracedecay_domain::configuration::CandidateDispositionV1::Winning,
                safe_reason: None,
            }],
        )]),
    )
    .expect("configuration snapshot");
    let pin = ContextScoutConfigurationPinV1::from_current(
        &tracedecay_global_db::configuration::contracts::ports::ConfigurationCurrentStateV1 {
            revision_id: revision,
            snapshot,
        },
    )
    .expect("disabled pin");
    let control = pin.control();
    install_project_open_context_scout_configuration(
        owner.as_ref(),
        pin,
        &tracedecay_automation_runtime::automation::config::AutomationConfig::default(),
    )
    .await
    .expect("install disabled Scout configuration");

    let input = configured_model_input_at(
        control.configuration_revision,
        30,
        UtcMicros(1_000),
        ContextScoutDeliveryWindowV1::NextBoundary,
    );
    let outcome = owner
        .prepare_configured(
            &input,
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(1)),
            CancellationToken::new(),
        )
        .await
        .expect("disabled prepare");
    assert!(matches!(
        outcome,
        ContextScoutRuntimeOutcomeV1::Suppressed { .. }
    ));
    assert!(
        owner
            .claim_ready_guidance_exact(
                &tracedecay_hooks::HookEventEnvelopeV2 {
                    schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
                    event_id: [64; 16],
                    producer: tracedecay_hooks::HookHostV1::Codex,
                    protected_session_id: input.address.protected_session_id,
                    project_id: input.address.project_id,
                    repository_id: [61; 16],
                    worktree_id: [62; 16],
                    worktree_epoch: 1,
                    binding_token: [63; 32],
                    ordering: tracedecay_hooks::HookOrderingV1::Unknown,
                    observed_at: UtcMicros(1_001),
                    event: tracedecay_hooks::HookEventV2::SessionBoundary {
                        boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
                    },
                },
                input.address,
                input.input_watermark,
                1,
                UtcMicros(1_001),
            )
            .await
            .is_none(),
        "a disabled configuration must never surface guidance"
    );
}
