use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tracedecay_application::remote::auth::{
    EnrollmentIssueRequestV1, OpaqueRemoteCredential, authenticate_caller, issue_enrollment,
};
use tracedecay_application::remote::composition::{
    AuthenticityClaimV1, AuthorizationClaimV1, IntegrityClaimV1, PendingLocalObservationsV1,
    QueryManifestBindingV1, RemoteCompletenessV1, RemoteFreshnessV1, RemoteQueryCompositionV1,
    ShardCoverageStateV1, ShardQueryContributionV1,
};
use tracedecay_application::remote::recovery::AuthorityRejoinStateV1;
use tracedecay_application::{
    ApplicationOperation, AuthorityReceipt, CancellationContext, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, PolicyDecisionRef, RequestContext, RequestId, ResolvedScope,
    ResultContractRef,
};
use tracedecay_domain::remote::{RemoteCapabilityV1, RemoteRepositoryScopeV1};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, EntityId, ManifestDigest, ProjectId, RefId,
    RepositoryId, RepositoryStateSnapshotId, UserProfileId, UtcMicros, WorktreeId,
};
use tracedecay_rusqlite_runtime::remote_recovery::{
    CurrentRestorePolicyAuthorityV1, stage_sqlite_restore,
};
use tracedecay_store::remote_recovery::{
    AuthenticatedManifestContextV1, AuthorityCasV1, BackupArtifactKindV1, BackupArtifactV1,
    BackupCoverageV1, BackupManifestV1, CurrentPolicyReplayV1, PromotionPreviewV1,
    PromotionReceiptV1, PromotionRecoveryStateV1,
};
use tracedecay_store::{
    CommitSequenceV1, ShardWatermarkV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1,
};
use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).unwrap()
}

fn repository_scope() -> RemoteRepositoryScopeV1 {
    RemoteRepositoryScopeV1 {
        repository_id: id::<RepositoryId>("repository.remote"),
        worktree_id: id::<WorktreeId>("worktree.remote"),
        reference: Some(id::<RefId>("refs/heads/main")),
        snapshot_id: id::<RepositoryStateSnapshotId>("snapshot.remote.1"),
    }
}

fn request_context() -> RequestContext {
    let contract =
        ResultContractRef::new(SchemaId::new("schema.remote.query.result").unwrap(), 1).unwrap();
    let operation = ApplicationOperation::new(
        CapabilityId::new("capability.remote.query").unwrap(),
        UseCaseId::new("use-case.remote.query").unwrap(),
        contract,
        true,
    );
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.remote"),
        id::<RepositoryId>("repository.remote"),
        id::<WorktreeId>("worktree.remote"),
        Some(id::<RefId>("refs/heads/main")),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.remote.query"),
        1,
        digest(DIGEST_A),
        id::<ActorId>("actor.remote.authority"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.remote.reader"),
        scope,
        grant,
        RequestId::new("request.remote.query").unwrap(),
        Deadline::new(UtcMicros(500)).unwrap(),
        CancellationContext::active("cancel.remote.query").unwrap(),
    )
    .unwrap()
}

fn query_receipt() -> AuthorityReceipt {
    let context = request_context();
    AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.remote.query",
            1,
            digest(DIGEST_B),
            ComponentVersion::new("policy.remote.v1").unwrap(),
        )
        .unwrap(),
        UtcMicros(2),
    )
    .unwrap()
}

fn binding(epoch: u64) -> StoreRuntimeBindingV1 {
    StoreRuntimeBindingV1::new(
        StoreShardIdV1::profile(
            id::<BrainId>("brain.remote"),
            id::<UserProfileId>("profile.remote"),
        ),
        StoreIncarnationV1::new(12).unwrap(),
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

fn sha256_file(path: &Path) -> [u8; 32] {
    let mut file = fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hasher.finalize().into()
}

struct ExactCurrentPolicy;

impl CurrentRestorePolicyAuthorityV1 for ExactCurrentPolicy {
    fn replay_current_policy(
        &self,
        staged_database: &Path,
        expected: &CurrentPolicyReplayV1,
    ) -> Result<CurrentPolicyReplayV1, String> {
        Connection::open(staged_database)
            .and_then(|connection| connection.execute_batch("PRAGMA integrity_check;"))
            .map_err(|error| error.to_string())?;
        Ok(expected.clone())
    }
}

#[test]
fn authenticated_query_verified_restore_and_higher_fence_preserve_exact_watermark() {
    let secret = OpaqueRemoteCredential::new(vec![b'a'; 32].into_boxed_slice()).unwrap();
    let enrollment = issue_enrollment(
        EnrollmentIssueRequestV1 {
            enrollment_id: id::<EntityId>("enrollment.remote.1"),
            brain_id: id::<BrainId>("brain.remote"),
            node_id: id::<BrainNodeId>("node.remote"),
            issued_at: UtcMicros(10),
            expires_at: UtcMicros(1_000),
            capabilities: BTreeSet::from([
                RemoteCapabilityV1::CaptureOffline,
                RemoteCapabilityV1::Replay,
                RemoteCapabilityV1::Query,
                RemoteCapabilityV1::ReadBackup,
                RemoteCapabilityV1::StageRestore,
                RemoteCapabilityV1::Promote,
            ]),
            scope: repository_scope(),
        },
        &secret,
    )
    .unwrap();
    authenticate_caller(
        &enrollment,
        &secret,
        &enrollment.brain_id,
        RemoteCapabilityV1::Query,
        &repository_scope(),
        UtcMicros(20),
    )
    .unwrap();

    let query = RemoteQueryCompositionV1::compose(
        vec![ShardQueryContributionV1 {
            manifest: QueryManifestBindingV1 {
                brain_id: "brain.remote".into(),
                shard_id: "shard.profile".into(),
                generation_id: "generation.12".into(),
                schema_digest: [1; 32],
                watermark_sequence: 41,
                placement_revision: 9,
                authority_epoch: 21,
                cache_age_millis: 0,
                cache_lag_commits: 0,
            },
            integrity: IntegrityClaimV1::Verified,
            authenticity: AuthenticityClaimV1::Authenticated,
            freshness: RemoteFreshnessV1::Current,
            completeness: RemoteCompletenessV1::Complete,
            authorization: AuthorizationClaimV1::Authorized,
            coverage: ShardCoverageStateV1::Complete,
            authority_receipt: Some(query_receipt()),
            value: Some("sanitized durable observation".to_owned()),
            reason_code: None,
        }],
        PendingLocalObservationsV1 {
            count: 0,
            oldest_age_millis: None,
            has_sequence_gap: false,
            has_quarantined: false,
        },
    )
    .unwrap();
    assert!(query.is_complete());
    assert_eq!(
        query.contributions[0].value.as_deref(),
        Some("sanitized durable observation")
    );

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.sqlite3");
    let connection = Connection::open(&source).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE observations(value TEXT NOT NULL);
             INSERT INTO observations VALUES ('sanitized durable observation');",
        )
        .unwrap();
    drop(connection);
    let source_bytes = fs::metadata(&source).unwrap().len();
    let source_digest = sha256_file(&source);
    let old_binding = binding(21);
    let manifest = BackupManifestV1 {
        backup_id: "backup.remote.1".into(),
        authentication: AuthenticatedManifestContextV1 {
            authenticated_node_id: "node.remote".into(),
            enrollment_revision: enrollment.revision,
            authorization_revision: 1,
        },
        binding: old_binding.clone(),
        placement_revision: 9,
        schema_digest: [2; 32],
        source_frontier: watermark(&old_binding, 41),
        parent_backup_id: None,
        lineage_digest: [3; 32],
        created_at_micros: 20,
        expires_at_micros: 1_000,
        coverage: BackupCoverageV1::Complete,
        artifacts: vec![BackupArtifactV1 {
            artifact_id: "profile.sqlite".into(),
            family: "profile".into(),
            kind: BackupArtifactKindV1::SqliteDatabase,
            bytes: source_bytes,
            sha256: source_digest,
            references: Vec::new(),
        }],
        total_bytes: source_bytes,
        artifact_count: 1,
    };
    manifest.validate(30).unwrap();

    let staged_path = root.path().join("staged.sqlite3");
    let mut staged = stage_sqlite_restore(
        &source,
        staged_path,
        &manifest,
        [4; 32],
        "profile.sqlite",
        30,
    )
    .unwrap();
    let current_policy = CurrentPolicyReplayV1 {
        tombstone_revision: 11,
        deletion_revision: 12,
        quarantine_revision: 13,
        retention_revision: 14,
        authorization_revision: 15,
        project_scope_digest: [5; 32],
    };
    staged
        .replay_current_policy(&ExactCurrentPolicy, &current_policy)
        .unwrap();
    let published_path = root.path().join("restored.sqlite3");
    let restore_receipt = staged
        .publish(&published_path, [4; 32], &old_binding)
        .unwrap();
    assert_eq!(restore_receipt.destination_bytes, source_bytes);
    assert_eq!(restore_receipt.destination_sha256, source_digest);

    let replacement = binding(22);
    let preview = PromotionPreviewV1 {
        preview_id: "promotion.remote.1".into(),
        cas: AuthorityCasV1 {
            shard_id: old_binding.shard_id.clone(),
            expected_binding: old_binding.clone(),
            replacement_binding: replacement.clone(),
            expected_placement_revision: 9,
            replacement_placement_revision: 10,
        },
        required_frontier: watermark(&old_binding, 41),
        required_sink_ids: vec!["writer".into(), "receipt".into(), "publication".into()],
    };
    let epoch = replacement.authority_epoch.get();
    let receipt = PromotionReceiptV1 {
        receipt_id: "promotion.receipt.1".into(),
        preview_id: preview.preview_id.clone(),
        replacement_binding: replacement.clone(),
        replacement_placement_revision: 10,
        installed_sink_epochs: preview
            .required_sink_ids
            .iter()
            .cloned()
            .map(|sink| (sink, epoch))
            .collect::<BTreeMap<_, _>>(),
        published_frontier: watermark(&replacement, 41),
        old_authority_read_only: true,
        state: PromotionRecoveryStateV1::Serving,
    };
    receipt.validate_against(&preview).unwrap();
    assert_eq!(
        receipt.published_frontier.commit_sequence,
        CommitSequenceV1(41)
    );
    assert!(!AuthorityRejoinStateV1::RejoinedReadOnly.may_accept_writes());
}
