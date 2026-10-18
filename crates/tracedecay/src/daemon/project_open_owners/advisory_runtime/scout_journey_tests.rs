use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::*;

use tracedecay_agent_hosts::agents::context_scout::decision_engine::{
    DecisionAnswerV1, DecisionAnswersV1, DecisionEngineIdentityV1 as EngineIdentityV1,
    DecisionEngineOutcomeV1, DecisionEnginePortV1, DecisionRequestV1,
};
use tracedecay_agent_hosts::agents::context_scout::intent_pass::intent_hint_emitted_event;
use tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1;
use tracedecay_agent_hosts::agents::context_scout::{
    ContextScoutDurableClaimOutcomeV1, ContextScoutDurableStoreOutcomeV1,
    ContextScoutEvidenceEnvelopeExt, ContextScoutMutationBindingV1,
    ContextScoutMutationOperationV1, ContextScoutMutationResultV1,
    ContextScoutMutationSettlementOutcomeV1, ContextScoutPublicMutationV1,
    context_scout_delivery_receipt_id,
};
use tracedecay_contracts::context_scout::{
    ContextScoutAddressV1, ContextScoutCandidateV1, ContextScoutCategoryV1,
    ContextScoutClaimRequestV1, ContextScoutClaimWindowV1, ContextScoutDeliveryOutcomeV1,
    ContextScoutDeliveryReceiptV1, ContextScoutDeliveryWindowV1, ContextScoutEvidenceEnvelopeV1,
    ContextScoutEvidenceSourceKindV1, ContextScoutEvidenceSourceReceiptV1,
    ContextScoutFeedbackKindV1, ContextScoutFeedbackV1, ContextScoutLeaseV1,
    ContextScoutRedactionReceiptV1,
};
use tracedecay_contracts::request_identity::{
    LogicalEffectIdempotencyDomain, derive_logical_effect_idempotency,
};
use tracedecay_contracts::{
    AuthorityReceipt, CoverageCompleteness, CoverageDomainState, DisclosureClass, EvidenceCoverage,
    EvidenceDomain, FreshnessState, IdempotencyKey, PolicyDecisionRef, ResolvedScope,
    RetrieverContributionState, TemporalState,
};
use tracedecay_domain::configuration::{
    ConfigurationValueV1, ContextScoutIntentKindV1, ContextScoutIntentSettingsV1,
    DecisionEngineSettingsV1, IntentWindowSettingsV1,
};
use tracedecay_domain::feedback::FeedbackContentIdentityV1;
use tracedecay_domain::{
    ActorId, CodeGenerationId, ComponentVersion, ProjectId, RefId, RetrievalAnchorId,
    TemporalModeV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

#[test]
fn scout_observation_requires_the_publication_authority_to_exist_first() {
    let publication_revalidated_at = UtcMicros(105);

    assert_eq!(
        admit_context_scout_observed_at(UtcMicros(104), Some(publication_revalidated_at)),
        None,
    );
    assert_eq!(
        admit_context_scout_observed_at(UtcMicros(106), Some(publication_revalidated_at)),
        Some(UtcMicros(106)),
    );
}

fn typed_id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

use tracedecay_domain::test_fixtures::digest;

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
                code_graph_freshness: None,
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
            evidence: configured_model_evidence(marker).into(),
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
        intent: None,
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
    tracedecay_global_db::register_registered_schema_installer();
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

/// Commits one public Scout mutation exactly as the daemon's Context Scout
/// tools do: an idempotency-keyed binding over the mutation's input digest.
async fn commit_public(
    owner: &ProjectContextScoutOwnerV1,
    operation: ContextScoutMutationOperationV1,
    mutation: ContextScoutPublicMutationV1,
    key: &str,
) -> ContextScoutMutationResultV1 {
    let actor = ActorId::new("actor.context-scout.journey").expect("actor");
    let scope = ResolvedScope::new(
        typed_id("project.scout.configured-model"),
        typed_id("repository.scout.configured-model"),
        typed_id("worktree.scout.configured-model"),
        Some(typed_id::<RefId>("refs/heads/main")),
    )
    .expect("resolved scope");
    let idempotency_key = IdempotencyKey::new(key).expect("idempotency key");
    let binding = ContextScoutMutationBindingV1 {
        effect_identity: derive_logical_effect_idempotency(
            LogicalEffectIdempotencyDomain::ContextScoutEffect,
            &(&actor, &scope, operation.as_str(), &idempotency_key),
        )
        .expect("effect identity"),
        actor,
        scope,
        operation,
        idempotency_key,
        input_digest: mutation.input_digest().expect("mutation input digest"),
    };
    match owner.commit_public_mutation(binding, mutation).await {
        ContextScoutMutationSettlementOutcomeV1::Reconciled(settlement) => settlement.result,
        other => panic!("public Scout mutation {key} did not settle: {other:?}"),
    }
}

async fn claim_public(
    owner: &ProjectContextScoutOwnerV1,
    request: &ContextScoutClaimRequestV1,
    now: UtcMicros,
    expires_at: UtcMicros,
) -> ContextScoutDurableClaimOutcomeV1 {
    let mutation = owner
        .public_claim_mutation(request, now, expires_at)
        .await
        .expect("configured claim mutation");
    match commit_public(
        owner,
        ContextScoutMutationOperationV1::Claim,
        mutation,
        request.idempotency_key.as_str(),
    )
    .await
    {
        ContextScoutMutationResultV1::Claim(outcome) => *outcome,
        other => panic!("claim settled as {other:?}"),
    }
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
    install_project_open_context_scout_configuration(
        owner.as_ref(),
        pin,
        tracedecay_agent_hosts::agents::context_scout::model::ContextScoutModelConfig {
            automation: &model_config,
            codex: &tracedecay_domain::configuration::LcmSummarizerExecutableV1::Unconfigured,
        },
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
    let claim = match claim_public(
        &owner,
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
        claim_public(
            &owner,
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
        claim_public(
            &owner,
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
        claim.entry.envelope.candidate.evidence.redaction(),
        &ContextScoutRedactionReceiptV1::MetadataOnly {
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
    let delivery = ContextScoutPublicMutationV1::Delivery {
        work: claim.entry.work,
        envelope_id: claim.entry.envelope.envelope_id,
        lease: claim.lease,
        configuration_revision: control.configuration_revision,
        receipt: receipt.clone(),
    };
    assert_eq!(
        commit_public(
            &owner,
            ContextScoutMutationOperationV1::Delivery,
            delivery,
            "context-scout.delivery.stop",
        )
        .await,
        ContextScoutMutationResultV1::Delivery {
            outcome: ContextScoutDurableStoreOutcomeV1::Stored,
            receipt: receipt.clone(),
        }
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
    let explicit_feedback = ContextScoutPublicMutationV1::Feedback {
        address: stop.address,
        receipt: receipt.clone(),
        feedback,
    };
    assert_eq!(
        commit_public(
            &owner,
            ContextScoutMutationOperationV1::Feedback,
            explicit_feedback,
            "context-scout.feedback.stop",
        )
        .await,
        ContextScoutMutationResultV1::Feedback(ContextScoutDurableStoreOutcomeV1::Stored)
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
    assert_eq!(
        owner
            .recent_exact(cancelled_input.address, 8)
            .await
            .expect("recent state after cancellation")
            .pending
            .len(),
        0
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
    install_project_open_context_scout_configuration(
        owner.as_ref(),
        first_pin,
        tracedecay_agent_hosts::agents::context_scout::model::ContextScoutModelConfig {
            automation: &model_config,
            codex: &tracedecay_domain::configuration::LcmSummarizerExecutableV1::Unconfigured,
        },
    )
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
        tracedecay_agent_hosts::agents::context_scout::model::ContextScoutModelConfig {
            automation: &model_config,
            codex: &tracedecay_domain::configuration::LcmSummarizerExecutableV1::Unconfigured,
        },
    )
    .await
    .expect("install replacement Scout configuration");

    let request = ContextScoutClaimRequestV1 {
        address: input.address,
        window: ContextScoutClaimWindowV1::IdleWindow,
        idempotency_key: IdempotencyKey::new("context-scout.claim.displaced").expect("claim key"),
    };
    assert_eq!(
        claim_public(
            &owner,
            &request,
            UtcMicros(now.0 + 1),
            UtcMicros(now.0 + 30_000_000),
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
        tracedecay_agent_hosts::agents::context_scout::model::ContextScoutModelConfig {
            automation:
                &tracedecay_automation_runtime::automation::config::AutomationConfig::default(),
            codex: &tracedecay_domain::configuration::LcmSummarizerExecutableV1::Unconfigured,
        },
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
                    producer: tracedecay_domain::NativeHostIdentityV1::Codex,
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

/// In-process decision engine: answers every configured question, recalling
/// the earlier session with `recall` and everything else with 0.05, and
/// records each request it receives.
struct RecordingDecisionEngine {
    recall: f64,
    requests: std::sync::atomic::AtomicUsize,
    states: std::sync::Mutex<Vec<String>>,
}

impl RecordingDecisionEngine {
    fn new(recall: f64) -> Arc<Self> {
        Arc::new(Self {
            recall,
            requests: std::sync::atomic::AtomicUsize::new(0),
            states: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl DecisionEnginePortV1 for RecordingDecisionEngine {
    fn ask_blocking(&self, request: &DecisionRequestV1) -> DecisionEngineOutcomeV1 {
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.states
            .lock()
            .expect("recorded states")
            .push(request.state.clone());
        DecisionEngineOutcomeV1::Answered(DecisionAnswersV1 {
            answers: request
                .questions
                .iter()
                .map(|question| {
                    let probability = if question.key == "session_recall" {
                        self.recall
                    } else {
                        0.05
                    };
                    (question.key.clone(), DecisionAnswerV1::Noul(probability))
                })
                .collect(),
            model: Some("kev-4b".to_owned()),
            latency_micros: 1,
        })
    }

    fn identity(&self) -> EngineIdentityV1 {
        EngineIdentityV1 {
            base_url_digest: [7; 32],
            model: Some("kev-4b".to_owned()),
        }
    }
}

fn intent_settings() -> ContextScoutIntentSettingsV1 {
    ContextScoutIntentSettingsV1 {
        enabled: true,
        engine: DecisionEngineSettingsV1 {
            base_url: "http://127.0.0.1:9".to_owned(),
            model: Some("kev-4b".to_owned()),
            timeout_ms: 1_000,
            max_concurrent: 1,
            max_state_bytes: 8_192,
        },
        window: IntentWindowSettingsV1 {
            max_user_turns: 3,
            max_messages: 12,
            max_bytes: 8_192,
            max_age_secs: 600,
            freshen_budget_ms: 0,
        },
        questions: ContextScoutIntentSettingsV1::reference_questions(),
        action_fact_score_permille: 700,
        ttl_secs: 600,
        min_delivery_gap_secs: 30,
        prompt_lookup_deadline_ms: 200,
    }
}

fn intent_pin(intent: Option<ContextScoutIntentSettingsV1>) -> ContextScoutConfigurationPinV1 {
    let setting_key = tracedecay_domain::configuration::SettingKey::new(
        tracedecay_domain::configuration::CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
    )
    .expect("Scout setting key");
    let revision =
        tracedecay_domain::configuration::ConfigurationRevisionId::new("revision.scout.intent")
            .expect("configuration revision");
    let settings = tracedecay_domain::configuration::ContextScoutSettingsV1 {
        state: tracedecay_domain::configuration::ContextScoutConfigurationStateV1::Active,
        intent,
        ..tracedecay_domain::configuration::ContextScoutSettingsV1::disabled()
    };
    settings.validate().expect("intent settings");
    let snapshot = tracedecay_domain::configuration::ConfigurationSnapshotV1::new(
        BTreeMap::from([(
            setting_key.clone(),
            ConfigurationValueV1::ContextScoutSettings(settings),
        )]),
        BTreeMap::from([(
            setting_key,
            vec![tracedecay_domain::configuration::ConfigurationCandidateV1 {
                layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Project {
                    project_id: ProjectId::new("project.scout.intent").expect("project id"),
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
    .expect("intent pin")
}

const INTENT_SESSION: &str = "session-intent-journey";

/// Everything one intent pass needs besides its sessions store and engine:
/// a configured owner, the address registry over its database, an admitted
/// prompt hook, and the authorized request context and pin.
struct IntentJourneyFixture {
    _temporary: tempfile::TempDir,
    owner: Arc<tracedecay_agent_hosts::agents::context_scout::owner::ProjectContextScoutOwnerV1>,
    registry: Arc<ProjectContextScoutAddressRegistryV1>,
    binding: tracedecay_hooks::HookScopeBindingV1,
    pin: ContextScoutAuthorityPinV1,
    context: RequestContext,
    lifecycle: tracedecay_agent_hosts::agents::context_scout::address_registry::ContextScoutLifecycleAddressV1,
}

impl IntentJourneyFixture {
    async fn open(intent: Option<ContextScoutIntentSettingsV1>) -> Self {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let owner = test_scout_owner(&temporary).await;
        let configuration = intent_pin(intent);
        owner
            .install_configuration(configuration.clone(), None)
            .await
            .expect("install intent configuration");
        let scope = ResolvedScope::new(
            typed_id("project.scout.intent"),
            typed_id("repository.scout.intent"),
            typed_id("worktree.scout.intent"),
            Some(typed_id::<RefId>("refs/heads/main")),
        )
        .expect("scope");
        let issued_at = now_utc_micros();
        let grant = CapabilityGrantSnapshot::new(
            typed_id("grant.scout.intent"),
            1,
            digest('a'),
            typed_id("actor.scout.issuer"),
            issued_at,
            UtcMicros(issued_at.0 + 3_600_000_000),
            scope.clone(),
            BTreeSet::from([
                tracedecay_tool_catalog::CapabilityId::new("capability.scout.intent")
                    .expect("capability"),
            ]),
            BTreeSet::from([
                tracedecay_tool_catalog::UseCaseId::new("use-case.scout.intent").expect("use case"),
            ]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            typed_id("actor.scout.requester"),
            scope.clone(),
            grant,
            typed_id("request.scout.intent"),
            Deadline::new(UtcMicros(issued_at.0 + 3_600_000_000)).expect("deadline"),
            CancellationContext::active("cancel.scout.intent").expect("cancellation"),
        )
        .expect("request context");
        let pin = ContextScoutAuthorityPinV1::new(
            &context,
            FeedbackScopeV1 {
                project_id: scope.project_id.clone(),
                repository_id: scope.repository_id.clone(),
                worktree_id: scope.worktree_id.clone(),
                branch_ref: "refs/heads/main".to_owned(),
                head_commit_id: typed_id("commit.scout.intent"),
            },
            configuration,
            issued_at,
        )
        .expect("authority pin");
        let registry = ProjectContextScoutAddressRegistryV1::new(
            owner.store().database().clone(),
            scope.project_id.clone(),
        )
        .expect("address registry");
        let binding = tracedecay_hooks::HookScopeBindingV1 {
            host: tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
            project_id: [8; 16],
            repository_id: [61; 16],
            worktree_id: [62; 16],
            worktree_epoch: 1,
            binding_token: [63; 32],
            capabilities: [
                tracedecay_hooks::HookEventFamily::SessionBoundary,
                tracedecay_hooks::HookEventFamily::ToolLifecycle,
            ]
            .into_iter()
            .map(|family| tracedecay_hooks::HookCapabilityV1 {
                family,
                support: tracedecay_hooks::stock_event_support(
                    tracedecay_domain::NativeHostIdentityV1::ClaudeCode,
                    family,
                ),
            })
            .collect(),
        };
        let lifecycle =
            tracedecay_agent_hosts::agents::context_scout::address_registry::ContextScoutLifecycleAddressV1 {
                profile_id: typed_id("profile.scout.intent"),
                provider_id: typed_id("claude"),
                project_id: scope.project_id.clone(),
                worktree_id: scope.worktree_id.clone(),
                session_id: typed_id(INTENT_SESSION),
                thread_id: typed_id("thread.scout.intent"),
                turn_id: typed_id("turn.scout.intent"),
                agent_id: typed_id("agent.scout.intent"),
                logical_message_id: typed_id("message.scout.intent"),
            };
        Self {
            _temporary: temporary,
            owner,
            registry,
            binding,
            pin,
            context,
            lifecycle,
        }
    }

    fn hook(
        &self,
        event_id: u8,
        event: tracedecay_hooks::HookEventV2,
    ) -> tracedecay_agent_hosts::agents::context_scout::address_registry::AdmittedContextScoutHookV1
    {
        tracedecay_agent_hosts::agents::context_scout::address_registry::AdmittedContextScoutHookV1::new(
            tracedecay_hooks::HookEventEnvelopeV2 {
                schema_version: tracedecay_hooks::HOOK_EVENT_SCHEMA_VERSION,
                event_id: [event_id; 16],
                producer: self.binding.host,
                protected_session_id: tracedecay_agent_hosts::hooks::protected_native_session_id(
                    INTENT_SESSION,
                ),
                project_id: self.binding.project_id,
                repository_id: self.binding.repository_id,
                worktree_id: self.binding.worktree_id,
                worktree_epoch: self.binding.worktree_epoch,
                binding_token: self.binding.binding_token,
                ordering: tracedecay_hooks::HookOrderingV1::Unknown,
                observed_at: now_utc_micros(),
                event,
            },
            &self.binding,
        )
        .expect("admitted hook")
    }

    async fn pass(
        &self,
        hook: &tracedecay_agent_hosts::agents::context_scout::address_registry::AdmittedContextScoutHookV1,
        sessions: &tracedecay_global_db::RegisteredGlobalDb,
        project_key: &str,
        engine: &Arc<RecordingDecisionEngine>,
    ) -> ContextScoutIntentPassOutcomeV1 {
        run_context_scout_intent_pass(ContextScoutIntentPassV1 {
            owner: self.owner.as_ref(),
            registry: self.registry.as_ref(),
            sessions,
            analytics: Some(ContextScoutIntentAnalyticsV1 {
                analytics: sessions,
                project_key,
            }),
            hook,
            pin: &self.pin,
            context: &self.context,
            lifecycle: &self.lifecycle,
            code_generation_id: Some(typed_id("generation.scout.intent")),
            engine: Some(Arc::clone(engine) as Arc<dyn DecisionEnginePortV1>),
            observed_at: now_utc_micros(),
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            cancellation: CancellationToken::new(),
        })
        .await
    }
}

/// Claude's admitted boundary until its prompt hook is registered: a
/// completed tool call.
fn completed_tool() -> tracedecay_hooks::HookEventV2 {
    tracedecay_hooks::HookEventV2::ToolLifecycle {
        tool_id: [70; 16],
        phase: tracedecay_hooks::HookLifecyclePhaseV1::Completed,
        effect_receipt_id: None,
    }
}

fn now_utc_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_micros(),
        )
        .expect("microsecond clock"),
    )
}

async fn seed_transcript_message(
    sessions: &tracedecay_global_db::RegisteredGlobalDb,
    ordinal: i64,
    role: &str,
    text: &str,
    tool_names: Option<&str>,
    timestamp_secs: i64,
) {
    let root = sessions
        .db_path()
        .parent()
        .expect("sessions root")
        .to_path_buf();
    sessions
        .lcm_ingest_raw_message(
            &root,
            &tracedecay_sessions::runtime::SessionMessageRecord {
                provider: "claude".to_owned(),
                message_id: format!("claude-{INTENT_SESSION}-{ordinal:04}"),
                session_id: INTENT_SESSION.to_owned(),
                role: role.to_owned(),
                timestamp: Some(timestamp_secs),
                ordinal,
                text: text.to_owned(),
                kind: Some("message".to_owned()),
                model: None,
                tool_names: tool_names.map(str::to_owned),
                source_path: None,
                source_offset: None,
                metadata_json: None,
            },
        )
        .await
        .expect("ingest transcript message");
}

/// The daemon intent pass at a completed tool call over a seeded session: one batched engine request
/// over the window, exactly one session-evidence suggestion at the session's
/// intent address, a claim at the next stop boundary, a delivery that writes
/// the `hint_emitted` row hint settlement resolves into `hint_outcome`, and a
/// later pass that withholds the delivered intent as a duplicate.
#[tokio::test]
async fn intent_pass_enqueues_one_session_suggestion_claimed_at_stop_and_settled() {
    let fixture = IntentJourneyFixture::open(Some(intent_settings())).await;
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("intent-journey")
            .await;
    let sessions = harness.registered.as_ref();
    let project_key = "/project/intent-journey";
    assert!(
        sessions
            .upsert_session(&tracedecay_sessions::runtime::SessionRecord {
                provider: "claude".to_owned(),
                session_id: INTENT_SESSION.to_owned(),
                project_key: project_key.to_owned(),
                project_path: project_key.to_owned(),
                title: None,
                started_at: None,
                ended_at: None,
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    let now_secs = now_utc_micros().0 / 1_000_000;
    seed_transcript_message(
        sessions,
        0,
        "user",
        "Tighten the uploader retry loop.",
        None,
        now_secs - 40,
    )
    .await;
    seed_transcript_message(
        sessions,
        1,
        "assistant",
        "Updated the retry loop with jittered backoff.",
        None,
        now_secs - 30,
    )
    .await;
    seed_transcript_message(
        sessions,
        2,
        "user",
        "Use the same backoff numbers we settled on for the downloader.",
        None,
        now_secs - 20,
    )
    .await;

    let engine = RecordingDecisionEngine::new(0.9);
    let boundary = fixture.hook(71, completed_tool());
    let ContextScoutIntentPassOutcomeV1::Enqueued { entry, lifecycle } = fixture
        .pass(&boundary, sessions, project_key, &engine)
        .await
    else {
        panic!("a recalled earlier session must enqueue one suggestion");
    };
    let lifecycle = *lifecycle;
    assert_eq!(
        engine.requests(),
        1,
        "all questions share one engine request"
    );
    let states = engine.states.lock().expect("states").clone();
    assert!(states[0].contains("User: Use the same backoff numbers"));
    assert!(states[0].contains("Assistant: Updated the retry loop"));

    assert_eq!(Some(lifecycle.clone()), fixture.lifecycle.intent_scoped());
    assert_eq!(
        entry.route,
        tracedecay_contracts::context_scout::ContextScoutRouteV1::DecisionEngine
    );
    assert_eq!(
        entry.model_outcome,
        tracedecay_contracts::context_scout::ContextScoutModelOutcomeV1::Succeeded
    );
    assert_eq!(entry.model_receipt, None);
    let candidate = &entry.envelope.candidate;
    assert_eq!(candidate.category, ContextScoutCategoryV1::Retrieval);
    assert_eq!(
        candidate.suggestion_text,
        tracedecay_policy::intent_catalog::suggestion_text(ContextScoutIntentKindV1::SessionRecall)
    );
    let evidence = candidate.evidence.as_session().expect("session evidence");
    assert_eq!(
        evidence.decision.selected,
        ContextScoutIntentKindV1::SessionRecall
    );
    assert_ne!(
        evidence.decision.source,
        tracedecay_domain::configuration::IntentDecisionSourceV1::ActionFact,
        "the engine answer carried the selection"
    );
    assert!(evidence.decision.answers.contains(
        &tracedecay_contracts::context_scout::ContextScoutIntentAnswerV1 {
            intent: ContextScoutIntentKindV1::SessionRecall,
            probability_permille: 900,
            threshold_permille: 600,
        }
    ));
    assert_eq!(
        evidence
            .decision
            .engine
            .as_ref()
            .and_then(|engine| engine.model.as_deref()),
        Some("kev-4b")
    );
    assert_eq!(evidence.window.message_count, 3);
    assert_eq!(evidence.provider_id.as_str(), "claude");
    assert_eq!(evidence.session_id.as_str(), INTENT_SESSION);
    assert_eq!(
        evidence.code_generation_id,
        Some(typed_id::<CodeGenerationId>("generation.scout.intent"))
    );
    assert_eq!(
        candidate.evidence.redaction(),
        &ContextScoutRedactionReceiptV1::MetadataOnly {
            disclosure: DisclosureClass::Evidence,
        }
    );
    let dedupe = canonical_sha256(&(
        "tracedecay.context-scout.intent-candidate.v1",
        ContextScoutIntentKindV1::SessionRecall,
        boundary.envelope().protected_session_id,
    ))
    .expect("dedupe digest");
    assert_eq!(
        hex::encode(candidate.dedupe_key),
        dedupe.hex_suffix().expect("sha256 digest")
    );
    let serialized = serde_json::to_string(&entry).expect("serialize entry");
    assert!(
        !serialized.contains("backoff numbers"),
        "the durable entry never carries window text"
    );
    let pending = fixture
        .owner
        .recent_exact(entry.work.address, 8)
        .await
        .expect("intent address state");
    assert_eq!(pending.pending, vec![*entry.clone()]);

    assert_eq!(
        fixture
            .owner
            .mount_current_claim_authority(
                Arc::clone(&fixture.registry),
                &boundary,
                1,
                fixture.pin.clone(),
                fixture.context.clone(),
                lifecycle.clone(),
                entry.work.address,
                entry.work.input_watermark,
                now_utc_micros(),
                true,
            )
            .await,
        tracedecay_agent_hosts::agents::context_scout::owner::ContextScoutClaimAdmissionV1::Mounted
    );
    // The stop boundary carries the session's native lifecycle; admission
    // falls back to the session's intent-scoped mount.
    let stop = fixture.hook(
        72,
        tracedecay_hooks::HookEventV2::SessionBoundary {
            boundary: tracedecay_hooks::HookBoundaryV1::TurnComplete,
        },
    );
    let claimed_at = now_utc_micros();
    let (address, watermark) = fixture
        .owner
        .resolve_current_claim_authority(&stop, &lifecycle, claimed_at, true)
        .await
        .expect("the intent mount resolves at the stop boundary");
    let (guidance, claim) = fixture
        .owner
        .claim_ready_guidance_exact(stop.envelope(), address, watermark, 1, claimed_at)
        .await
        .expect("the stop boundary claims the intent suggestion");
    assert_eq!(guidance.text, candidate.suggestion_text);
    let receipt = ContextScoutDeliveryReceiptV1 {
        receipt_id: context_scout_delivery_receipt_id(
            stop.envelope().event_id,
            guidance.guidance_id,
        ),
        envelope_id: guidance.guidance_id,
        delivered_at: UtcMicros(claimed_at.0 + 1),
        outcome: ContextScoutDeliveryOutcomeV1::Displayed,
    };
    assert_eq!(
        fixture.owner.record_delivery(&claim, &receipt).await,
        ContextScoutDurableStoreOutcomeV1::Stored
    );
    let emitted = intent_hint_emitted_event(project_key, &claim.entry, &receipt)
        .expect("session evidence emits a hint");
    assert_eq!(emitted.provider, "hook_claude");
    assert_eq!(emitted.event_kind, "hint_emitted");
    assert_eq!(emitted.session_id.as_deref(), Some(INTENT_SESSION));
    assert_eq!(emitted.hint_category.as_deref(), Some("session_recall"));
    assert_eq!(
        emitted.hint_id.as_deref(),
        Some(hex::encode(guidance.guidance_id).as_str())
    );
    let metadata: serde_json::Value =
        serde_json::from_str(emitted.metadata_json.as_deref().expect("metadata"))
            .expect("metadata json");
    assert_eq!(metadata["route"], "decision_engine");
    assert_eq!(metadata["engine"]["model"], "kev-4b");
    assert_eq!(metadata["answers"].as_array().map(Vec::len), Some(10));
    assert!(metadata["window_digest"].is_string());
    sessions
        .append_analytics_events(&[emitted])
        .await
        .expect("append hint_emitted");

    // The model then searches earlier sessions: settlement credits the hint.
    let delivered_secs = receipt.delivered_at.0 / 1_000_000;
    seed_transcript_message(
        sessions,
        3,
        "assistant",
        "Searching the earlier session.",
        Some("mcp__tracedecay__tracedecay_message_search"),
        delivered_secs + 5,
    )
    .await;
    let stats = tracedecay_agent_hosts::hooks::hint_outcomes::settlement::correlate_registered_hint_outcomes(
        sessions,
        sessions,
        project_key,
        delivered_secs + 10,
    )
    .await
    .expect("settle hint outcomes");
    assert_eq!(stats.acted, 1, "{stats:?}");

    // A later pass in the session withholds the delivered intent.
    let next_boundary = fixture.hook(73, completed_tool());
    assert_eq!(
        fixture
            .pass(&next_boundary, sessions, project_key, &engine)
            .await,
        ContextScoutIntentPassOutcomeV1::Suppressed {
            reason:
                tracedecay_agent_hosts::agents::context_scout::ContextScoutSuppressionV1::Duplicate,
            model_outcome:
                tracedecay_contracts::context_scout::ContextScoutModelOutcomeV1::Succeeded,
        }
    );
    let withheld = sessions
        .query_analytics_events(&tracedecay_global_db::AnalyticsEventQuery {
            project_id: Some(project_key.to_owned()),
            event_kind: Some("suppressed_duplicate".to_owned()),
            limit: 8,
            ..Default::default()
        })
        .await
        .expect("query withheld hints");
    assert_eq!(withheld.len(), 1);
    assert_eq!(withheld[0].hint_category.as_deref(), Some("session_recall"));
    assert_eq!(
        fixture
            .owner
            .recent_exact(entry.work.address, 8)
            .await
            .expect("intent address state")
            .pending
            .len(),
        0
    );
    unregister_intent_owner(&fixture);
}

fn unregister_intent_owner(fixture: &IntentJourneyFixture) {
    tracedecay_agent_hosts::agents::context_scout::owner::unregister_registered_context_scout_owner(
        [8; 16],
        fixture.owner.store().database().canonical_database_path(),
    );
}

/// Without a configured, enabled intent pass, nothing is read or asked.
#[tokio::test]
async fn absent_or_disabled_intent_configuration_never_asks_the_engine() {
    let harness =
        tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open("intent-disabled")
            .await;
    let sessions = harness.registered.as_ref();
    let engine = RecordingDecisionEngine::new(0.9);
    let disabled = ContextScoutIntentSettingsV1 {
        enabled: false,
        ..intent_settings()
    };
    for intent in [None, Some(disabled)] {
        let fixture = IntentJourneyFixture::open(intent).await;
        assert!(fixture.owner.intent_engine().await.is_none());
        let prompt = fixture.hook(81, completed_tool());
        assert_eq!(
            fixture
                .pass(&prompt, sessions, "/project/intent-disabled", &engine)
                .await,
            ContextScoutIntentPassOutcomeV1::Disabled
        );
        unregister_intent_owner(&fixture);
    }
    assert_eq!(engine.requests(), 0);
}
