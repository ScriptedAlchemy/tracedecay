use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::remote::auth::{
    EnrollmentIssueRequestV1, OpaqueRemoteCredential, RemoteAuthenticationError,
    authenticate_caller, issue_enrollment,
};
use tracedecay_application::remote::composition::{
    AuthenticityClaimV1, AuthorizationClaimV1, IntegrityClaimV1, PendingLocalObservationsV1,
    QueryManifestBindingV1, RemoteCompletenessV1, RemoteFreshnessV1, RemoteQueryCompositionV1,
    ShardCoverageStateV1, ShardQueryContributionV1,
};
use tracedecay_application::remote::recovery::{
    AuthorityRejoinStateV1, PromotionCasReceiptV1 as ApplicationPromotionReceiptV1,
    PromotionPreviewV1 as ApplicationPromotionPreviewV1, RecoveryAuthorityExpectationV1,
    StagedRestoreProgressV1,
};
use tracedecay_domain::remote::{
    EnrollmentCredentialRecordV1, EnrollmentCredentialStateV1, RemoteCapabilityV1,
    RemoteCredentialFingerprintV1, RemoteRepositoryScopeV1,
};
use tracedecay_domain::{
    BrainId, BrainNodeId, EntityId, RefId, RepositoryId, RepositoryStateSnapshotId, UserProfileId,
    UtcMicros, WorktreeId,
};
use tracedecay_rusqlite_runtime::remote_spool::{
    RemoteCaptureSpool, RemoteSpoolConfig, RemoteSpoolEncryption, RemoteSpoolEncryptionError,
    RemoteSpoolError,
};
use tracedecay_store::remote_capture::RemoteCaptureStateV1;
use tracedecay_store::remote_recovery::{
    AuthenticatedManifestContextV1, AuthorityCasV1, BackupArtifactKindV1, BackupArtifactV1,
    BackupCoverageV1, BackupManifestV1, CurrentPolicyReplayV1, PromotionPreviewV1,
    PromotionReceiptV1, PromotionRecoveryStateV1, RemoteRecoveryContractErrorV1,
    ReplicaCacheManifestV1, StagedRestoreStateV1,
};
use tracedecay_store::{
    CommitSequenceV1, ShardWatermarkV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn binding(brain: &str, profile: &str, generation: u64, epoch: u64) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile(id::<BrainId>(brain), id::<UserProfileId>(profile)),
        StoreIncarnationV1::new(generation).unwrap(),
        StoreAuthorityEpochV1::new(epoch).unwrap(),
    )
}

fn watermark(binding: &StoreRuntimeBindingV1, sequence: u64) -> ShardWatermarkV1 {
    ShardWatermarkV1 {
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        commit_sequence: CommitSequenceV1(sequence),
    }
}

fn authentication() -> AuthenticatedManifestContextV1 {
    AuthenticatedManifestContextV1 {
        authenticated_node_id: "node.standby".into(),
        enrollment_revision: 4,
        authorization_revision: 7,
    }
}

fn replica_manifest(binding: StoreRuntimeBindingV1) -> ReplicaCacheManifestV1 {
    ReplicaCacheManifestV1 {
        authentication: authentication(),
        watermark: watermark(&binding, 41),
        binding,
        placement_revision: 9,
        schema_digest: [1; 32],
        material_digest: [2; 32],
        material_bytes: 1_024,
        observed_at_micros: 100,
        expires_at_micros: 200,
    }
}

fn backup_manifest(binding: StoreRuntimeBindingV1) -> BackupManifestV1 {
    BackupManifestV1 {
        backup_id: "backup.remote.1".into(),
        authentication: authentication(),
        source_frontier: watermark(&binding, 41),
        binding,
        placement_revision: 9,
        schema_digest: [3; 32],
        parent_backup_id: None,
        lineage_digest: [4; 32],
        created_at_micros: 100,
        expires_at_micros: 300,
        coverage: BackupCoverageV1::Complete,
        artifacts: vec![BackupArtifactV1 {
            artifact_id: "profile.sqlite".into(),
            family: "profile".into(),
            kind: BackupArtifactKindV1::SqliteDatabase,
            bytes: 4_096,
            sha256: [5; 32],
            references: Vec::new(),
        }],
        total_bytes: 4_096,
        artifact_count: 1,
    }
}

fn promotion_preview() -> PromotionPreviewV1 {
    let old = binding("brain.remote", "profile.remote", 12, 21);
    let replacement = binding("brain.remote", "profile.remote", 12, 22);
    PromotionPreviewV1 {
        preview_id: "promotion.remote.1".into(),
        cas: AuthorityCasV1 {
            shard_id: old.shard_id.clone(),
            expected_binding: old.clone(),
            replacement_binding: replacement,
            expected_placement_revision: 9,
            replacement_placement_revision: 10,
        },
        required_frontier: watermark(&old, 41),
        required_sink_ids: vec![
            "canonical_mutation".into(),
            "receipt".into(),
            "publication".into(),
        ],
    }
}

fn promotion_receipt(preview: &PromotionPreviewV1, sequence: u64) -> PromotionReceiptV1 {
    let epoch = preview.cas.replacement_binding.authority_epoch.get();
    PromotionReceiptV1 {
        receipt_id: "promotion.receipt.1".into(),
        preview_id: preview.preview_id.clone(),
        replacement_binding: preview.cas.replacement_binding.clone(),
        replacement_placement_revision: preview.cas.replacement_placement_revision,
        installed_sink_epochs: preview
            .required_sink_ids
            .iter()
            .cloned()
            .map(|sink| (sink, epoch))
            .collect::<BTreeMap<_, _>>(),
        published_frontier: watermark(&preview.cas.replacement_binding, sequence),
        old_authority_read_only: true,
        state: PromotionRecoveryStateV1::Serving,
    }
}

fn repository_scope(worktree: &str) -> RemoteRepositoryScopeV1 {
    RemoteRepositoryScopeV1 {
        repository_id: id::<RepositoryId>("repository.remote"),
        worktree_id: id::<WorktreeId>(worktree),
        reference: Some(id::<RefId>("refs/heads/main")),
        snapshot_id: id::<RepositoryStateSnapshotId>("snapshot.remote.1"),
    }
}

fn enrollment(secret: &[u8]) -> EnrollmentCredentialRecordV1 {
    EnrollmentCredentialRecordV1 {
        enrollment_id: id::<EntityId>("enrollment.remote.1"),
        brain_id: id::<BrainId>("brain.remote"),
        node_id: id::<BrainNodeId>("node.remote"),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(secret).unwrap(),
        revision: 4,
        issued_at: UtcMicros(100),
        expires_at: UtcMicros(200),
        revoked_at: None,
        capabilities: BTreeSet::from([
            RemoteCapabilityV1::CaptureOffline,
            RemoteCapabilityV1::Replay,
            RemoteCapabilityV1::Query,
        ]),
        scope: repository_scope("worktree.remote"),
    }
}

fn query_manifest() -> QueryManifestBindingV1 {
    QueryManifestBindingV1 {
        brain_id: "brain.remote".into(),
        shard_id: "shard.profile".into(),
        generation_id: "generation.12".into(),
        schema_digest: [7; 32],
        watermark_sequence: 41,
        placement_revision: 9,
        authority_epoch: 21,
        cache_age_millis: 20,
        cache_lag_commits: 1,
    }
}

fn unavailable_contribution(reason_code: &str) -> ShardQueryContributionV1<String> {
    ShardQueryContributionV1 {
        manifest: query_manifest(),
        integrity: IntegrityClaimV1::Unknown,
        authenticity: AuthenticityClaimV1::Unknown,
        freshness: RemoteFreshnessV1::Unknown,
        completeness: RemoteCompletenessV1::Unknown,
        authorization: AuthorizationClaimV1::Unknown,
        coverage: ShardCoverageStateV1::Unavailable,
        authority_receipt: None,
        value: None,
        reason_code: Some(reason_code.into()),
    }
}

fn opaque(byte: u8) -> OpaqueRemoteCredential {
    OpaqueRemoteCredential::new(vec![byte; 32].into_boxed_slice()).unwrap()
}

struct UnavailableEncryption;

impl RemoteSpoolEncryption for UnavailableEncryption {
    fn is_available(&self) -> bool {
        false
    }

    fn seal(&self, _plaintext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
        Err(RemoteSpoolEncryptionError { operation: "seal" })
    }

    fn open(&self, _ciphertext: &[u8]) -> Result<Vec<u8>, RemoteSpoolEncryptionError> {
        Err(RemoteSpoolEncryptionError { operation: "open" })
    }
}

#[test]
fn replica_manifest_rejects_wrong_brain_project_generation_placement_epoch_schema_and_watermark() {
    let expected = binding("brain.remote", "profile.remote", 12, 21);
    let exact = replica_manifest(expected.clone());
    assert_eq!(exact.validate_for(&expected, 9, [1; 32], 150), Ok(()));

    for wrong_binding in [
        binding("brain.other", "profile.remote", 12, 21),
        binding("brain.remote", "profile.other", 12, 21),
        binding("brain.remote", "profile.remote", 13, 21),
        binding("brain.remote", "profile.remote", 12, 22),
    ] {
        assert_eq!(
            exact.validate_for(&wrong_binding, 9, [1; 32], 150),
            Err(RemoteRecoveryContractErrorV1::BindingMismatch)
        );
    }
    assert_eq!(
        exact.validate_for(&expected, 10, [1; 32], 150),
        Err(RemoteRecoveryContractErrorV1::PlacementMismatch)
    );
    assert_eq!(
        exact.validate_for(&expected, 9, [9; 32], 150),
        Err(RemoteRecoveryContractErrorV1::SchemaMismatch)
    );

    let mut wrong_watermark = exact;
    wrong_watermark.watermark = watermark(&binding("brain.remote", "profile.remote", 12, 22), 41);
    assert_eq!(
        wrong_watermark.validate_for(&expected, 9, [1; 32], 150),
        Err(RemoteRecoveryContractErrorV1::BindingMismatch)
    );
}

#[test]
fn expired_or_unauthenticated_replica_never_appears_available() {
    let expected = binding("brain.remote", "profile.remote", 12, 21);
    let mut manifest = replica_manifest(expected.clone());
    manifest.authentication.enrollment_revision = 0;
    assert_eq!(
        manifest.validate_for(&expected, 9, [1; 32], 150),
        Err(RemoteRecoveryContractErrorV1::AuthenticationInvalid)
    );

    let manifest = replica_manifest(expected.clone());
    assert_eq!(
        manifest.validate_for(&expected, 9, [1; 32], 200),
        Err(RemoteRecoveryContractErrorV1::Expired)
    );
}

#[test]
fn stolen_expired_and_revoked_enrollment_grants_fail_closed() {
    let legitimate = opaque(b'a');
    let stolen = opaque(b'b');
    let mut grant = issue_enrollment(
        EnrollmentIssueRequestV1 {
            enrollment_id: id::<EntityId>("enrollment.remote.1"),
            brain_id: id::<BrainId>("brain.remote"),
            node_id: id::<BrainNodeId>("node.remote"),
            issued_at: UtcMicros(100),
            expires_at: UtcMicros(200),
            capabilities: BTreeSet::from([RemoteCapabilityV1::Replay]),
            scope: repository_scope("worktree.remote"),
        },
        &legitimate,
    )
    .unwrap();

    assert_eq!(
        authenticate_caller(
            &grant,
            &stolen,
            &grant.brain_id,
            RemoteCapabilityV1::Replay,
            &repository_scope("worktree.remote"),
            UtcMicros(150),
        ),
        Err(RemoteAuthenticationError::InvalidCredential)
    );
    assert_eq!(
        authenticate_caller(
            &grant,
            &legitimate,
            &grant.brain_id,
            RemoteCapabilityV1::Replay,
            &repository_scope("worktree.other"),
            UtcMicros(150),
        ),
        Err(RemoteAuthenticationError::ScopeMismatch)
    );
    assert_eq!(
        grant.state_at(UtcMicros(200)),
        EnrollmentCredentialStateV1::Expired
    );
    assert_eq!(
        authenticate_caller(
            &grant,
            &legitimate,
            &grant.brain_id,
            RemoteCapabilityV1::Replay,
            &repository_scope("worktree.remote"),
            UtcMicros(200),
        ),
        Err(RemoteAuthenticationError::Expired)
    );

    grant.revoked_at = Some(UtcMicros(140));
    assert_eq!(
        grant.state_at(UtcMicros(140)),
        EnrollmentCredentialStateV1::Revoked
    );
    assert_eq!(
        authenticate_caller(
            &grant,
            &legitimate,
            &grant.brain_id,
            RemoteCapabilityV1::Replay,
            &repository_scope("worktree.remote"),
            UtcMicros(150),
        ),
        Err(RemoteAuthenticationError::Revoked)
    );
}

#[test]
fn credentials_overlays_analyzer_state_and_raw_json_rpc_cannot_deserialize_into_durable_state() {
    let grant = enrollment(b"legitimate-enrollment-secret-0001");
    let serialized = serde_json::to_string(&grant).unwrap();
    assert!(!serialized.contains("legitimate-enrollment-secret"));

    for (field, value) in [
        ("credential", serde_json::json!("plaintext-secret")),
        ("overlay", serde_json::json!("unsaved document")),
        ("analyzer_state", serde_json::json!({"dirty": true})),
        (
            "json_rpc",
            serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/didChange"}),
        ),
    ] {
        let mut object = serde_json::to_value(&grant)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
        object.insert(field.into(), value);
        assert!(
            serde_json::from_value::<EnrollmentCredentialRecordV1>(serde_json::Value::Object(
                object
            ))
            .is_err(),
            "{field} entered a deny-unknown durable remote record"
        );
    }
}

#[test]
fn reordered_gapped_and_unacknowledged_capture_cannot_be_collected() {
    assert!(
        !RemoteCaptureStateV1::Pending
            .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
    );
    assert!(
        !RemoteCaptureStateV1::Captured.permits_transition_to(RemoteCaptureStateV1::Acknowledged)
    );
    assert!(
        !RemoteCaptureStateV1::Acknowledged.permits_transition_to(RemoteCaptureStateV1::Pending)
    );
    assert!(
        RemoteCaptureStateV1::Acknowledged
            .permits_transition_to(RemoteCaptureStateV1::GarbageCollectionEligible)
    );
}

#[test]
fn offline_spool_fails_closed_without_admitted_at_rest_encryption() {
    let root = tempfile::tempdir().unwrap();
    let result = RemoteCaptureSpool::open(
        root.path().join("remote-spool.bin"),
        RemoteSpoolConfig {
            maximum_file_bytes: 4_096,
            maximum_record_bytes: 2_048,
            maximum_events: 4,
        },
        Box::new(UnavailableEncryption),
    );
    assert!(matches!(
        result,
        Err(RemoteSpoolError::AtRestEncryptionUnavailable)
    ));
    assert!(!root.path().join("remote-spool.bin").exists());
}

#[test]
fn unavailable_replica_or_shard_is_never_reported_as_complete_or_empty_success() {
    let pending = PendingLocalObservationsV1 {
        count: 0,
        oldest_age_millis: None,
        has_sequence_gap: false,
        has_quarantined: false,
    };
    assert!(RemoteQueryCompositionV1::<String>::compose(Vec::new(), pending.clone()).is_err());

    let result = RemoteQueryCompositionV1::compose(
        vec![unavailable_contribution("replica_unavailable")],
        pending,
    )
    .unwrap();
    assert_eq!(result.coverage, ShardCoverageStateV1::Unavailable);
    assert!(!result.is_complete());
}

#[test]
fn pending_gap_quarantine_and_stale_cache_keep_query_coverage_honest() {
    let mut stale = unavailable_contribution("cache_stale");
    stale.coverage = ShardCoverageStateV1::Stale;
    stale.freshness = RemoteFreshnessV1::Stale;
    stale.completeness = RemoteCompletenessV1::Partial;

    let pending = PendingLocalObservationsV1 {
        count: 2,
        oldest_age_millis: Some(500),
        has_sequence_gap: true,
        has_quarantined: true,
    };
    let result = RemoteQueryCompositionV1::compose(vec![stale], pending).unwrap();
    assert_eq!(result.coverage, ShardCoverageStateV1::Partial);
    assert!(!result.is_complete());
}

#[test]
fn corrupt_interrupted_or_partial_backup_cannot_validate_as_complete() {
    let current = binding("brain.remote", "profile.remote", 12, 21);

    let mut corrupt = backup_manifest(current.clone());
    corrupt.artifacts[0].sha256 = [0; 32];
    assert_eq!(
        corrupt.validate(150),
        Err(RemoteRecoveryContractErrorV1::InvalidArtifact)
    );

    let mut interrupted = backup_manifest(current.clone());
    interrupted.artifact_count = 2;
    assert_eq!(
        interrupted.validate(150),
        Err(RemoteRecoveryContractErrorV1::InventoryMismatch)
    );

    let mut partial = backup_manifest(current);
    partial.coverage = BackupCoverageV1::Partial;
    assert_eq!(partial.validate(150), Ok(()));
    assert_eq!(partial.coverage, BackupCoverageV1::Partial);
}

#[test]
fn restore_staging_never_serves_partial_or_recovery_required_state() {
    for state in [
        StagedRestoreStateV1::Allocated,
        StagedRestoreStateV1::BytesVerified,
        StagedRestoreStateV1::ReferenceClosureVerified,
        StagedRestoreStateV1::ReadyForPublication,
        StagedRestoreStateV1::RolledBack {
            reason_code: "interrupted_restore".into(),
        },
        StagedRestoreStateV1::RecoveryRequired {
            reason_code: "partial_publication".into(),
        },
    ] {
        assert!(!state.may_serve());
    }
}

#[test]
fn restore_requires_current_tombstone_deletion_quarantine_retention_policy_and_scope() {
    let exact = CurrentPolicyReplayV1 {
        tombstone_revision: 11,
        deletion_revision: 12,
        quarantine_revision: 13,
        retention_revision: 14,
        authorization_revision: 15,
        project_scope_digest: [6; 32],
    };
    assert_eq!(exact.validate(), Ok(()));

    let missing_states = [
        CurrentPolicyReplayV1 {
            tombstone_revision: 0,
            ..exact.clone()
        },
        CurrentPolicyReplayV1 {
            deletion_revision: 0,
            ..exact.clone()
        },
        CurrentPolicyReplayV1 {
            quarantine_revision: 0,
            ..exact.clone()
        },
        CurrentPolicyReplayV1 {
            retention_revision: 0,
            ..exact.clone()
        },
        CurrentPolicyReplayV1 {
            authorization_revision: 0,
            ..exact.clone()
        },
        CurrentPolicyReplayV1 {
            project_scope_digest: [0; 32],
            ..exact
        },
    ];
    for missing in missing_states {
        assert_eq!(
            missing.validate(),
            Err(RemoteRecoveryContractErrorV1::PolicyReplayMissing)
        );
    }
}

#[test]
fn promotion_rejects_split_brain_stale_cas_and_generation_change() {
    let preview = promotion_preview();

    let mut no_higher_fence = preview.cas.clone();
    no_higher_fence.replacement_binding.authority_epoch =
        no_higher_fence.expected_binding.authority_epoch;
    assert_eq!(
        no_higher_fence.validate(),
        Err(RemoteRecoveryContractErrorV1::EpochNotAdvanced)
    );

    let mut changed_generation = preview.cas;
    changed_generation.replacement_binding.incarnation = StoreIncarnationV1::new(13).unwrap();
    assert_eq!(
        changed_generation.validate(),
        Err(RemoteRecoveryContractErrorV1::ImmutableGenerationChanged)
    );
}

#[test]
fn promotion_rejects_insufficient_standby_frontier_and_sink_fence_failure() {
    let preview = promotion_preview();

    let insufficient = promotion_receipt(&preview, 40);
    assert_eq!(
        insufficient.validate_against(&preview),
        Err(RemoteRecoveryContractErrorV1::FrontierMismatch)
    );

    let mut missing_sink = promotion_receipt(&preview, 41);
    missing_sink.installed_sink_epochs.remove("publication");
    assert_eq!(
        missing_sink.validate_against(&preview),
        Err(RemoteRecoveryContractErrorV1::SinkFenceMissing)
    );

    let mut old_writer_not_fenced = promotion_receipt(&preview, 41);
    old_writer_not_fenced.old_authority_read_only = false;
    assert_eq!(
        old_writer_not_fenced.validate_against(&preview),
        Err(RemoteRecoveryContractErrorV1::PromotionReceiptMismatch)
    );
}

#[test]
fn application_promotion_rejects_startup_race_stale_frontier_and_missing_sink_fence() {
    let preview = ApplicationPromotionPreviewV1 {
        preview_id: "promotion.remote.1".into(),
        expected: RecoveryAuthorityExpectationV1 {
            brain_id: "brain.remote".into(),
            shard_id: "shard.profile".into(),
            generation_id: "generation.12".into(),
            placement_revision: 9,
            authority_epoch: 21,
            frontier_sequence: 41,
        },
        replacement_epoch: 22,
        replacement_placement_revision: 10,
        required_sink_ids: vec!["writer".into(), "receipt".into(), "publication".into()],
        expires_at_micros: 200,
    };
    assert_eq!(preview.validate(150), Ok(()));

    let stale_frontier = ApplicationPromotionReceiptV1 {
        receipt_id: "promotion.receipt.1".into(),
        preview_id: preview.preview_id.clone(),
        previous_epoch: 21,
        installed_epoch: 22,
        installed_placement_revision: 10,
        installed_sink_ids: preview.required_sink_ids.clone(),
        published_frontier_sequence: 40,
        old_authority_fenced: true,
    };
    assert!(stale_frontier.validate_against(&preview).is_err());

    let mut missing_sink = stale_frontier;
    missing_sink.published_frontier_sequence = 41;
    missing_sink
        .installed_sink_ids
        .retain(|sink| sink != "publication");
    assert!(missing_sink.validate_against(&preview).is_err());

    let mut split_brain = missing_sink;
    split_brain.installed_sink_ids = preview.required_sink_ids.clone();
    split_brain.old_authority_fenced = false;
    assert!(split_brain.validate_against(&preview).is_err());
}

#[test]
fn old_writer_is_rejected_before_and_after_rejoin_until_explicit_reseed_and_promotion() {
    for state in [
        AuthorityRejoinStateV1::FencedReadOnly {
            observed_higher_epoch: 22,
        },
        AuthorityRejoinStateV1::ReseedRequired {
            observed_higher_epoch: 22,
        },
        AuthorityRejoinStateV1::ReseedPreviewed {
            preview_id: "reseed.remote.1".into(),
            observed_higher_epoch: 22,
        },
        AuthorityRejoinStateV1::Reseeding,
        AuthorityRejoinStateV1::RejoinedReadOnly,
    ] {
        assert!(!state.may_accept_writes());
    }
}

#[test]
fn interrupted_or_partial_restore_never_serves() {
    for state in [
        StagedRestoreProgressV1::Isolated,
        StagedRestoreProgressV1::DestinationBytesVerified,
        StagedRestoreProgressV1::ReferenceClosureVerified,
        StagedRestoreProgressV1::ReplayingCurrentPolicy,
        StagedRestoreProgressV1::ReadyForPublication,
        StagedRestoreProgressV1::RolledBackBeforePublication {
            reason_code: "corrupt_backup".into(),
        },
        StagedRestoreProgressV1::ForwardRecoveryRequired {
            reason_code: "interrupted_publication".into(),
        },
    ] {
        assert!(!state.serving());
    }
}
