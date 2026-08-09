#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "stack_coordinator/preflight_tests.rs"]
mod preflight_tests;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1;
use tracedecay_domain::{
    AnchorOwnerBindingV1, CommitId, GitHubPullRequestIdV1, GitTopologySourceRoleV1, ManifestDigest,
    OrderedGitTopologySourceV1, PrivacyDomainId, ProjectId, ProviderId,
    PullRequestSnapshotAnchorRefV1, RefId, RepositoryId, RetrievalAnchorId, UserProfileId,
    UtcMicros, WorktreeId,
};

use crate::stack_coordinator::*;

fn actor(index: usize) -> tracedecay_domain::ActorId {
    tracedecay_domain::ActorId::new(format!("actor.stack.{index}")).unwrap()
}

fn signal(exact_scope: &ResolvedScope, index: usize, kind: StackSignalKindV1) -> StackSignalV1 {
    signal_at(
        exact_scope,
        kind,
        digest(if index.is_multiple_of(2) { 'b' } else { 'c' }),
        UtcMicros(index as i64),
    )
}

fn signal_at(
    exact_scope: &ResolvedScope,
    kind: StackSignalKindV1,
    state_digest: ManifestDigest,
    observed_at: UtcMicros,
) -> StackSignalV1 {
    StackSignalV1::seal(
        exact_scope,
        StackSignalDraftV1 {
            stack_revision_id: tracedecay_domain::BranchStackRevisionId::new("revision.stack")
                .unwrap(),
            stack_revision_digest: digest('a'),
            kind,
            state_digest,
            github_stack_digest: None,
            observed_at,
        },
    )
    .unwrap()
}

#[derive(Default)]
struct MemoryStore {
    signals: Mutex<BTreeMap<tracedecay_domain::StackSignalId, StackSignalV1>>,
    pending: Mutex<Vec<(StackPendingDeliveryV1, StackSignalV1)>>,
    authorization_losses:
        Mutex<Vec<(tracedecay_domain::StackSignalId, tracedecay_domain::ActorId)>>,
    saturate_after_persist: Mutex<bool>,
}

impl StackCoordinatorStore for MemoryStore {
    fn append_signal(
        &self,
        signal: StackSignalV1,
        recipients: Vec<tracedecay_domain::ActorId>,
    ) -> Result<(), StackCoordinatorErrorV1> {
        let mut signals = self.signals.lock().unwrap();
        if let Some(existing) = signals.get(&signal.signal_id) {
            return (existing == &signal).then_some(()).ok_or_else(|| {
                StackCoordinatorErrorV1::Invalid("signal identity conflict".to_owned())
            });
        }
        signals.insert(signal.signal_id.clone(), signal.clone());
        self.pending
            .lock()
            .unwrap()
            .extend(recipients.into_iter().map(|recipient| {
                (
                    StackPendingDeliveryV1 {
                        recipient,
                        signal_id: signal.signal_id.clone(),
                    },
                    signal.clone(),
                )
            }));
        if *self.saturate_after_persist.lock().unwrap() {
            Err(StackCoordinatorErrorV1::Saturated)
        } else {
            Ok(())
        }
    }

    fn pending_deliveries(
        &self,
    ) -> Result<Vec<(StackPendingDeliveryV1, StackSignalV1)>, StackCoordinatorErrorV1> {
        Ok(self.pending.lock().unwrap().clone())
    }

    fn acknowledge(
        &self,
        watermark_id: &tracedecay_domain::StackDeliveryWatermarkId,
        deliveries: &[StackPendingDeliveryV1],
    ) -> Result<(), StackCoordinatorErrorV1> {
        let acknowledged = deliveries.iter().cloned().collect::<BTreeSet<_>>();
        self.pending.lock().unwrap().retain(|(delivery, signal)| {
            signal.watermark_id != *watermark_id || !acknowledged.contains(delivery)
        });
        Ok(())
    }

    fn signal(
        &self,
        signal_id: &tracedecay_domain::StackSignalId,
    ) -> Result<Option<StackSignalV1>, StackCoordinatorErrorV1> {
        Ok(self.signals.lock().unwrap().get(signal_id).cloned())
    }

    fn record_authorization_loss(
        &self,
        signal: &StackSignalV1,
        recipient: &tracedecay_domain::ActorId,
        _outcome: StackDeliveryAuthorizationV1,
    ) -> Result<(), StackCoordinatorErrorV1> {
        self.authorization_losses
            .lock()
            .unwrap()
            .push((signal.signal_id.clone(), recipient.clone()));
        Ok(())
    }
}

#[derive(Default)]
struct Authorization {
    denied: Mutex<BTreeSet<tracedecay_domain::ActorId>>,
    recipients: Mutex<Vec<tracedecay_domain::ActorId>>,
}

impl StackDeliveryAuthorizationPort for Authorization {
    fn select_recipients(
        &self,
        _scope: &ResolvedScope,
        _signal: &StackSignalV1,
    ) -> Result<Vec<tracedecay_domain::ActorId>, StackCoordinatorErrorV1> {
        Ok(self.recipients.lock().unwrap().clone())
    }

    fn authorize(
        &self,
        recipient: &tracedecay_domain::ActorId,
        _signal: &StackSignalV1,
    ) -> StackDeliveryAuthorizationV1 {
        if self.denied.lock().unwrap().contains(recipient) {
            StackDeliveryAuthorizationV1::Denied
        } else {
            StackDeliveryAuthorizationV1::Authorized
        }
    }
}

#[derive(Default)]
struct RecordingDelivery {
    batches: Mutex<Vec<StackDeliveryBatchV1>>,
    fail_next: Mutex<bool>,
}

impl StackDeliveryPort for RecordingDelivery {
    fn deliver(&self, batch: &StackDeliveryBatchV1) -> Result<(), StackCoordinatorErrorV1> {
        let mut fail_next = self.fail_next.lock().unwrap();
        if *fail_next {
            *fail_next = false;
            return Err(StackCoordinatorErrorV1::Unavailable);
        }
        self.batches.lock().unwrap().push(batch.clone());
        Ok(())
    }
}

fn digest(seed: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", seed.to_string().repeat(64))).unwrap()
}

fn scope(index: usize) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(format!("project.stack.{index}")).unwrap(),
        RepositoryId::new(format!("repository.stack.{index}")).unwrap(),
        WorktreeId::new(format!("worktree.stack.{index}")).unwrap(),
        Some(RefId::new(format!("refs/heads/stack-{index}")).unwrap()),
    )
    .unwrap()
}

fn provider_source_binding(
    exact_scope: &ResolvedScope,
    with_snapshot: bool,
) -> GitHubStackProviderSourceBindingV1 {
    GitHubStackProviderSourceBindingV1 {
        owner: AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.github-stack").unwrap(),
            exact_scope.project_id.clone(),
            PrivacyDomainId::new("privacy.github-stack").unwrap(),
        )
        .unwrap(),
        capability_source_anchor_id: RetrievalAnchorId::new("anchor.github.capability").unwrap(),
        snapshot_source_anchor_id: with_snapshot
            .then(|| RetrievalAnchorId::new("anchor.github.stack-snapshot").unwrap()),
    }
}

fn enabled_snapshot(exact_scope: &ResolvedScope) -> GitHubStackProviderSnapshotV1 {
    let source_anchor_id = RetrievalAnchorId::new("anchor.github.pull-request.41").unwrap();
    GitHubStackProviderSnapshotV1 {
        response_digest: digest('a'),
        provider_stack_id_digest: tracedecay_domain::PrivacyDomainBoundLocatorDigest::new(
            digest('b').as_str(),
        )
        .unwrap(),
        final_target_ref_id: RefId::new("refs/heads/main").unwrap(),
        final_target_commit_id: CommitId::new("commit.main").unwrap(),
        layers: vec![GitHubStackProviderLayerV1 {
            provider_position: 1,
            pull_request: PullRequestSnapshotAnchorRefV1 {
                provider: ProviderId::new("provider.github").unwrap(),
                project_id: exact_scope.project_id.clone(),
                repository_id: exact_scope.repository_id.clone(),
                worktree_id: exact_scope.worktree_id.clone(),
                pull_request_id: GitHubPullRequestIdV1::new("41").unwrap(),
                base_commit_id: CommitId::new("commit.main").unwrap(),
                head_commit_id: CommitId::new("commit.feature").unwrap(),
                merge_base_commit_id: CommitId::new("commit.merge-base").unwrap(),
                source_anchor_id: source_anchor_id.clone(),
                snapshot_digest: digest('9'),
                sources: vec![OrderedGitTopologySourceV1 {
                    source_ordinal: 0,
                    role: GitTopologySourceRoleV1::PullRequestObservation,
                    anchor_id: source_anchor_id,
                }],
            },
            base_ref_id: RefId::new("refs/heads/main").unwrap(),
            head_ref_id: RefId::new("refs/heads/feature").unwrap(),
            protection_digest: digest('c'),
            ci_digest: digest('d'),
            merge_queue_digest: digest('e'),
        }],
    }
}

#[test]
fn production_coordinator_materializes_all_four_states_and_exact_stack_anchors() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let provider = ProviderId::new("provider.github").unwrap();
    let disabled = scope(0);
    coordinator
        .register_scope(&disabled, GitHubStackedPullRequestPolicyV1::Disabled)
        .unwrap();
    let disabled = coordinator
        .observe_policy(
            disabled.clone(),
            provider.clone(),
            provider_source_binding(&disabled, false),
            UtcMicros(1),
        )
        .unwrap();
    assert_eq!(
        disabled.capability.state,
        tracedecay_domain::GitHubStackCapabilityStateV1::PrivatePreviewDisabled
    );
    assert!(disabled.snapshot.is_none());

    let probed = scope(1);
    coordinator
        .register_scope(
            &probed,
            GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
        )
        .unwrap();
    let unavailable = coordinator
        .observe_provider(
            probed.clone(),
            provider.clone(),
            GitHubStackProviderOutcomeV1::Unavailable,
            provider_source_binding(&probed, false),
            UtcMicros(2),
        )
        .unwrap();
    assert_eq!(
        unavailable.capability.state,
        tracedecay_domain::GitHubStackCapabilityStateV1::Unavailable
    );
    let degraded = coordinator
        .observe_provider(
            probed.clone(),
            provider.clone(),
            GitHubStackProviderOutcomeV1::Degraded {
                response_digest: digest('f'),
            },
            provider_source_binding(&probed, false),
            UtcMicros(3),
        )
        .unwrap();
    assert_eq!(
        degraded.capability.state,
        tracedecay_domain::GitHubStackCapabilityStateV1::Degraded
    );
    let enabled = coordinator
        .observe_provider(
            probed.clone(),
            provider,
            GitHubStackProviderOutcomeV1::Enabled(enabled_snapshot(&probed)),
            provider_source_binding(&probed, true),
            UtcMicros(4),
        )
        .unwrap();
    assert_eq!(
        enabled.capability.state,
        tracedecay_domain::GitHubStackCapabilityStateV1::Enabled
    );
    let snapshot = enabled.snapshot.as_ref().expect("enabled snapshot");
    assert_eq!(snapshot.capability.repository_id, probed.repository_id);
    assert_eq!(snapshot.final_target_ref_id.as_str(), "refs/heads/main");
    assert_eq!(
        snapshot.layers[0].pull_request.pull_request_id.as_str(),
        "41"
    );
    assert_eq!(snapshot.layers[0].base_ref_id.as_str(), "refs/heads/main");
    assert_eq!(
        snapshot.layers[0].head_ref_id.as_str(),
        "refs/heads/feature"
    );
    assert!(enabled.snapshot_anchor_id.is_some());
    assert_eq!(
        coordinator
            .observe_policy(
                probed,
                ProviderId::new("provider.github").unwrap(),
                provider_source_binding(&scope(1), false),
                UtcMicros(5),
            )
            .unwrap(),
        enabled
    );
}

#[test]
fn mounted_provider_observation_reports_pr_tip_merge_base_and_ci_drift() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let exact_scope = scope(90);
    let provider = ProviderId::new("provider.github").unwrap();
    coordinator
        .register_scope(
            &exact_scope,
            GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
        )
        .unwrap();
    coordinator
        .observe_provider(
            exact_scope.clone(),
            provider.clone(),
            GitHubStackProviderOutcomeV1::Enabled(enabled_snapshot(&exact_scope)),
            provider_source_binding(&exact_scope, true),
            UtcMicros(1),
        )
        .unwrap();

    let mut changed = enabled_snapshot(&exact_scope);
    changed.response_digest = digest('f');
    changed.final_target_commit_id = CommitId::new("commit.main.next").unwrap();
    changed.layers[0].pull_request.head_commit_id = CommitId::new("commit.feature.next").unwrap();
    changed.layers[0].pull_request.merge_base_commit_id =
        CommitId::new("commit.merge-base.next").unwrap();
    changed.layers[0].ci_digest = digest('a');
    let observation = coordinator
        .observe_provider(
            exact_scope.clone(),
            provider.clone(),
            GitHubStackProviderOutcomeV1::Enabled(changed.clone()),
            provider_source_binding(&exact_scope, true),
            UtcMicros(2),
        )
        .unwrap();

    assert_eq!(
        observation.transitions,
        vec![
            StackSignalKindV1::StackTipDrift,
            StackSignalKindV1::PullRequestDrift,
            StackSignalKindV1::CiEvaluatedCommitDrift,
        ]
    );
    assert_eq!(observation.drift_observations.len(), 3);
    assert!(
        observation
            .drift_observations
            .iter()
            .all(|drift| drift.validate(&exact_scope.scope_digest).is_ok())
    );
    assert!(
        observation
            .drift_observations
            .iter()
            .any(|event| { event.drift.kind == tracedecay_domain::StackDriftKindV1::HeadAdvanced })
    );
    assert!(
        observation
            .drift_observations
            .iter()
            .any(|event| { event.drift.kind == tracedecay_domain::StackDriftKindV1::BaseAdvanced })
    );
    assert!(observation.drift_observations.iter().any(|event| {
        event.drift.kind == tracedecay_domain::StackDriftKindV1::MergeBaseChanged
    }));
    let open = observation.drift_observations[0].clone();
    let persisted = GitHubStackDriftObservationV1::from_persisted(
        &exact_scope.scope_digest,
        open.observed_at,
        open.drift.clone(),
    )
    .unwrap();
    assert_eq!(persisted, open);
    assert_eq!(
        GitHubStackDriftObservationV1::from_persisted(
            &exact_scope.scope_digest,
            UtcMicros(open.drift.first_observed_micros - 1),
            open.drift.clone(),
        ),
        Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
    );
    DaemonGitHubStackCoordinatorV1::default()
        .restore_open_drift_interval(&exact_scope, &persisted)
        .unwrap();

    let closed = coordinator
        .observe_provider(
            exact_scope.clone(),
            provider,
            GitHubStackProviderOutcomeV1::Enabled(changed),
            provider_source_binding(&exact_scope, true),
            UtcMicros(3),
        )
        .unwrap();
    assert_eq!(closed.drift_observations.len(), 3);
    assert!(closed.drift_observations.iter().all(|event| {
        event.drift.state == tracedecay_domain::IntervalStateV1::Closed
            && event.drift.terminal_micros == Some(3)
            && event.validate(&exact_scope.scope_digest).is_ok()
    }));
}

#[test]
fn mounted_owner_dedupes_debounced_signals_but_drains_all_material_overflow() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let exact_scope = scope(91);
    let store = MemoryStore::default();
    let authorization = Authorization::default();
    let delivery = RecordingDelivery::default();
    let recipients = (0..65).map(actor).collect::<Vec<_>>();
    *authorization.recipients.lock().unwrap() = recipients;

    let first = signal_at(
        &exact_scope,
        StackSignalKindV1::DependencyReady,
        digest('f'),
        UtcMicros(1),
    );
    let duplicate = signal_at(
        &exact_scope,
        StackSignalKindV1::DependencyReady,
        digest('f'),
        UtcMicros(2),
    );
    coordinator
        .enqueue_transition(&store, &authorization, &exact_scope, first)
        .unwrap();
    coordinator
        .enqueue_transition(&store, &authorization, &exact_scope, duplicate)
        .unwrap();
    let mut expanded_id = None;
    for index in 3..132 {
        let material = signal(
            &exact_scope,
            index,
            StackSignalKindV1::IntegrationNeedsInspection,
        );
        expanded_id.get_or_insert_with(|| material.signal_id.clone());
        coordinator
            .enqueue_transition(&store, &authorization, &exact_scope, material)
            .unwrap();
    }

    authorization.denied.lock().unwrap().insert(actor(64));
    let delivered = coordinator
        .drain_due(&store, &authorization, &delivery, UtcMicros(2_000_000))
        .unwrap();
    assert_eq!(delivered, 64 * 130);
    assert!(store.pending.lock().unwrap().is_empty());
    assert_eq!(store.authorization_losses.lock().unwrap().len(), 130);
    let batches = delivery.batches.lock().unwrap();
    assert!(batches.len() > 1);
    assert!(batches.iter().all(|batch| {
        batch.recipients.len() <= MAX_BATCH_RECIPIENTS && batch.signals.len() <= MAX_BATCH_SIGNALS
    }));
    drop(batches);
    assert_eq!(
        coordinator.expand_transition(
            &store,
            &authorization,
            &actor(64),
            expanded_id.as_ref().unwrap(),
        ),
        Err(StackCoordinatorErrorV1::Denied)
    );
    *authorization.recipients.lock().unwrap() = vec![actor(64)];
    assert_eq!(
        coordinator.enqueue_transition(
            &store,
            &authorization,
            &exact_scope,
            signal(&exact_scope, 200, StackSignalKindV1::ActualConflict),
        ),
        Err(StackCoordinatorErrorV1::Denied)
    );
}

#[test]
fn transition_signal_and_circuit_policy_reject_stale_or_tampered_identity() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let exact_scope = scope(92);
    let store = MemoryStore::default();
    let authorization = Authorization::default();
    *authorization.recipients.lock().unwrap() = vec![actor(1)];
    assert_eq!(
        coordinator.enqueue_transition(
            &store,
            &authorization,
            &exact_scope,
            signal(&scope(93), 1, StackSignalKindV1::ActualConflict),
        ),
        Err(StackCoordinatorErrorV1::Stale)
    );
    let mut tampered_signal = signal(&exact_scope, 2, StackSignalKindV1::ActualConflict);
    tampered_signal.signal_id =
        tracedecay_domain::StackSignalId::new("signal.stack.tampered").unwrap();
    assert!(matches!(
        coordinator.enqueue_transition(&store, &authorization, &exact_scope, tampered_signal,),
        Err(StackCoordinatorErrorV1::Invalid(_))
    ));
    let mut tampered_watermark = signal(&exact_scope, 3, StackSignalKindV1::ActualConflict);
    tampered_watermark.watermark_id =
        tracedecay_domain::StackDeliveryWatermarkId::new("watermark.stack.tampered").unwrap();
    assert!(matches!(
        coordinator.enqueue_transition(&store, &authorization, &exact_scope, tampered_watermark,),
        Err(StackCoordinatorErrorV1::Invalid(_))
    ));

    let policy = StackCircuitPolicyV1 {
        revision: 1,
        policy_digest: digest('0'),
        failure_threshold: 2,
        open_micros: 100,
    }
    .seal()
    .unwrap();
    let mut tampered = policy.clone();
    tampered.failure_threshold = 3;
    assert!(
        coordinator
            .register_circuit_policy(&exact_scope, tampered)
            .is_err()
    );
    coordinator
        .register_circuit_policy(&exact_scope, policy)
        .unwrap();
}

#[test]
fn durable_saturation_and_restart_delivery_preserve_material_work() {
    let exact_scope = scope(93);
    let store = MemoryStore::default();
    let authorization = Authorization::default();
    *authorization.recipients.lock().unwrap() = vec![actor(1)];
    *store.saturate_after_persist.lock().unwrap() = true;
    let signal = signal(&exact_scope, 1, StackSignalKindV1::ActualConflict);
    assert_eq!(
        DaemonGitHubStackCoordinatorV1::default().enqueue_transition(
            &store,
            &authorization,
            &exact_scope,
            signal.clone(),
        ),
        Err(StackCoordinatorErrorV1::Saturated)
    );
    assert_eq!(store.signal(&signal.signal_id).unwrap(), Some(signal));
    assert_eq!(store.pending.lock().unwrap().len(), 1);

    *store.saturate_after_persist.lock().unwrap() = false;
    let failed_delivery = RecordingDelivery::default();
    *failed_delivery.fail_next.lock().unwrap() = true;
    assert_eq!(
        DaemonGitHubStackCoordinatorV1::default().drain_due(
            &store,
            &authorization,
            &failed_delivery,
            UtcMicros(2),
        ),
        Err(StackCoordinatorErrorV1::Unavailable)
    );
    assert_eq!(store.pending.lock().unwrap().len(), 1);
    assert_eq!(
        DaemonGitHubStackCoordinatorV1::default()
            .drain_due(
                &store,
                &authorization,
                &RecordingDelivery::default(),
                UtcMicros(2),
            )
            .unwrap(),
        1
    );
    assert!(store.pending.lock().unwrap().is_empty());
}

#[test]
fn debounce_classes_and_five_minute_dedupe_ttl_advance_independently() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let exact_scope = scope(94);
    let store = MemoryStore::default();
    let authorization = Authorization::default();
    *authorization.recipients.lock().unwrap() = vec![actor(1)];
    let delivery = RecordingDelivery::default();
    let ready = signal_at(
        &exact_scope,
        StackSignalKindV1::DependencyReady,
        digest('d'),
        UtcMicros(0),
    );
    let drift = signal_at(
        &exact_scope,
        StackSignalKindV1::StackTipDrift,
        digest('c'),
        UtcMicros(0),
    );
    coordinator
        .enqueue_transition(&store, &authorization, &exact_scope, ready.clone())
        .unwrap();
    coordinator
        .enqueue_transition(&store, &authorization, &exact_scope, drift)
        .unwrap();
    assert_eq!(
        coordinator
            .drain_due(&store, &authorization, &delivery, UtcMicros(249_999))
            .unwrap(),
        0
    );
    assert_eq!(
        coordinator
            .drain_due(&store, &authorization, &delivery, UtcMicros(250_000))
            .unwrap(),
        1
    );
    assert_eq!(
        coordinator
            .drain_due(&store, &authorization, &delivery, UtcMicros(999_999))
            .unwrap(),
        0
    );
    assert_eq!(
        coordinator
            .drain_due(&store, &authorization, &delivery, UtcMicros(1_000_000))
            .unwrap(),
        1
    );

    let duplicate = signal_at(
        &exact_scope,
        StackSignalKindV1::DependencyReady,
        digest('d'),
        UtcMicros(300_000_000),
    );
    coordinator
        .enqueue_transition(&store, &authorization, &exact_scope, duplicate)
        .unwrap();
    assert!(store.pending.lock().unwrap().is_empty());
    let after_ttl = signal_at(
        &exact_scope,
        StackSignalKindV1::DependencyReady,
        digest('d'),
        UtcMicros(300_000_001),
    );
    coordinator
        .enqueue_transition(&store, &authorization, &exact_scope, after_ttl)
        .unwrap();
    assert_eq!(store.pending.lock().unwrap().len(), 1);
}

#[test]
fn multi_root_fanout_is_authorized_deterministic_and_bounded() {
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    let provider = ProviderId::new("provider.github").unwrap();
    let mut roots = Vec::new();
    for index in 0..70 {
        let root = scope(index);
        coordinator
            .register_scope(&root, GitHubStackedPullRequestPolicyV1::ProbePrivatePreview)
            .unwrap();
        coordinator
            .observe_provider(
                root.clone(),
                provider.clone(),
                GitHubStackProviderOutcomeV1::EnabledWithoutStack {
                    response_digest: digest(char::from(b'a' + (index % 6) as u8)),
                },
                provider_source_binding(&root, false),
                UtcMicros(index as i64),
            )
            .unwrap();
        roots.push(root);
    }
    roots.reverse();
    let page = coordinator
        .fanout_authorized(roots, |root| root.project_id.as_str() != "project.stack.3")
        .unwrap();
    assert!(page.len() <= MAX_GITHUB_STACK_ROOTS_PER_FANOUT_V1);
    assert!(page.len() <= MAX_GITHUB_STACK_SIGNALS_PER_FANOUT_V1);
    assert!(
        page.windows(2)
            .all(|pair| pair[0].scope.scope_digest < pair[1].scope.scope_digest)
    );
    assert!(
        page.iter()
            .all(|item| item.scope.project_id.as_str() != "project.stack.3")
    );
}

#[test]
fn daemon_restart_returns_unavailable_until_a_delayed_exact_probe_upgrades_state() {
    let exact_scope = scope(80);
    let provider = ProviderId::new("provider.github").unwrap();
    let restarted = DaemonGitHubStackCoordinatorV1::default();
    restarted
        .register_scope(
            &exact_scope,
            GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
        )
        .unwrap();

    let initial = restarted
        .observe_policy(
            exact_scope.clone(),
            provider.clone(),
            provider_source_binding(&exact_scope, false),
            UtcMicros(10),
        )
        .unwrap();
    assert_eq!(
        initial.capability.state,
        tracedecay_domain::GitHubStackCapabilityStateV1::Unavailable
    );
    assert!(initial.snapshot.is_none());

    let upgraded = restarted
        .observe_provider(
            exact_scope,
            provider,
            GitHubStackProviderOutcomeV1::Enabled(enabled_snapshot(&scope(80))),
            provider_source_binding(&scope(80), true),
            UtcMicros(11),
        )
        .unwrap();
    assert_eq!(
        upgraded.capability.state,
        tracedecay_domain::GitHubStackCapabilityStateV1::Enabled
    );
    assert!(upgraded.snapshot.is_some());
}
