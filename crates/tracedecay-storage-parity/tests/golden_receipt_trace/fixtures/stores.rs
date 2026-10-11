//! Work placement, workflow definition and Remote Brain spool inputs for the
//! exact-SQL domain stores.

use std::{collections::BTreeSet, sync::Arc};

use super::BRAIN;
use serde_json::json;
use tracedecay_contracts::remote::auth::RemoteEnrollmentAdmissionEvidenceV1;
use tracedecay_contracts::remote::capture::{
    AdmittedRemoteCaptureV1, RemoteCaptureSequenceV1, RemoteWriterAuthorityV1,
};
use tracedecay_contracts::remote::replay::{
    RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidenceV1,
};
use tracedecay_contracts::{
    AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, EffectId, IdempotencyKey, PolicyDecisionRef, RequestContext, RequestId,
    ResolvedScope, WorkflowDefinitionLifecycleCommand, WorkflowEffectIdentityV1,
    WorkflowEffectOperationV1, WorkflowEffectReceiptContextV1, WorkflowLifecycleOperation,
};
use tracedecay_domain::test_fixtures::{digest, fixture_abs_root, id};
use tracedecay_domain::{
    ActorId, BrainId, BrainNodeId, ComponentVersion, DurableObservationV1, EnrollmentGrantV1,
    EntityId, ManifestDigest, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, ProjectId,
    ProviderId, RemoteCapabilityV1, RemoteCredentialFingerprintV1, RepositoryId, RetentionClass,
    RunId, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, TaskId, UtcMicros, WorkAuthority,
    WorkPlacementIdentityV1, WorkPlacementKindV1, WorkPlacementObservationV1,
    WorkPlacementPreflightV1, WorkPlacementTargetV1, WorkPlacementV1, WorkflowDefinition,
    WorkflowOperationRef, WorkflowOutputName, WorkflowStep, WorkflowStepId, WorktreeId,
    canonical_sha256,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteSpoolKeyV1, RemoteSpoolKeyringV1, RemoteSqliteStorageErrorV1,
};

// --- Work -------------------------------------------------------------------

pub fn work_authority() -> WorkAuthority {
    WorkAuthority::new(
        id::<ProjectId>("project.placement.parity"),
        id::<RepositoryId>("repository.placement.parity"),
        id::<WorktreeId>("worktree.placement.parity"),
        id::<ActorId>("actor.placement.parity"),
        digest('a'),
    )
    .unwrap()
}

pub fn admitted_placement() -> WorkPlacementV1 {
    let identity =
        WorkPlacementIdentityV1::new(id::<TaskId>("task.placement.parity"), id::<RunId>("run.a"));
    let target = WorkPlacementTargetV1::new(
        WorkPlacementKindV1::LinkedWorktree,
        Some(fixture_abs_root("/workspace/placement-parity")),
        false,
        true,
    )
    .unwrap();
    let clean = WorkPlacementObservationV1 {
        dirty_tracked_paths: 0,
        untracked_paths: 0,
        unique_commits: Some(0),
        readable: true,
        active_holder: false,
        network_required: false,
        observed_at: UtcMicros(100),
    };
    let preflight = WorkPlacementPreflightV1::evaluate(identity, target, clean);
    WorkPlacementV1::admit(&preflight, Some(UtcMicros(50_000)), UtcMicros(200)).unwrap()
}

// --- Workflow ---------------------------------------------------------------

fn workflow_digest(byte: char) -> ManifestDigest {
    let hex_byte = format!("{:02x}", u32::from(byte) & 0xff);
    ManifestDigest::new(format!("sha256:{}", hex_byte.repeat(32))).unwrap()
}

pub fn workflow_definition(project_id: &ProjectId) -> WorkflowDefinition {
    WorkflowDefinition::new(
        id("workflow.definition.parity"),
        1,
        project_id.clone(),
        vec![WorkflowStep {
            step_id: id::<WorkflowStepId>("prepare"),
            operation: id::<WorkflowOperationRef>("operation.prepare.v1"),
            predecessors: Default::default(),
            inputs: Vec::new(),
            outputs: vec![id::<WorkflowOutputName>("context")],
            fan_out: None,
        }],
        workflow_digest('a'),
        workflow_digest('b'),
        workflow_digest('c'),
    )
    .unwrap()
}

pub fn workflow_effect(
    project_id: &ProjectId,
    operation: WorkflowEffectOperationV1,
    input: char,
) -> WorkflowEffectIdentityV1 {
    let scope = ResolvedScope::new(
        project_id.clone(),
        id("repository.workflow.parity"),
        id("worktree.workflow.parity"),
        None,
    )
    .unwrap();
    let actor: ActorId = id("actor.workflow.parity");
    let grant = CapabilityGrantSnapshot::new(
        id::<CapabilityGrantId>("grant.workflow.parity"),
        1,
        workflow_digest('1'),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(90_000_000),
        scope.clone(),
        [id("capability.workflow.handoff_issue")]
            .into_iter()
            .collect(),
        [id("use-case.workflow.handoff_issue")]
            .into_iter()
            .collect(),
        DisclosureClass::Metadata,
    )
    .unwrap();
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        id::<RequestId>("request.workflow.parity"),
        Deadline::new(UtcMicros(80_000_000)).unwrap(),
        CancellationContext::active("cancel.workflow.parity").unwrap(),
    )
    .unwrap();
    let policy = PolicyDecisionRef::new(
        "policy.workflow.parity.v1",
        1,
        workflow_digest('6'),
        ComponentVersion::new("workflow-parity.v1").unwrap(),
    )
    .unwrap();
    let started_at = UtcMicros(10);
    let authority = AuthorityReceipt::from_context(&context, policy, started_at).unwrap();
    let receipt_context = WorkflowEffectReceiptContextV1::new(
        id(&format!("use-case.workflow.{}", operation.as_str())),
        id::<EffectId>(&format!("effect.workflow.parity.{input}")),
        authority,
        workflow_digest('7'),
        workflow_digest('8'),
        workflow_digest('9'),
        workflow_digest('a'),
    );
    WorkflowEffectIdentityV1::new(
        operation,
        id::<IdempotencyKey>(&format!("workflow.effect.{input}")),
        context.request_id().clone(),
        context.actor().clone(),
        context.scope().clone(),
        workflow_digest(input),
        started_at,
        context.deadline().clone(),
        receipt_context,
    )
    .unwrap()
}

pub fn workflow_lifecycle(
    definition: &WorkflowDefinition,
    operation: WorkflowLifecycleOperation,
    expected_revision: u64,
    at: i64,
) -> WorkflowDefinitionLifecycleCommand {
    WorkflowDefinitionLifecycleCommand {
        definition_id: definition.definition_id().clone(),
        definition_version: definition.definition_version(),
        operation,
        expected_revision,
        transitioned_at: UtcMicros(at),
    }
}

// --- Remote Brain spool -----------------------------------------------------

pub struct Keyring(pub Arc<RemoteSpoolKeyV1>);

impl RemoteSpoolKeyringV1 for Keyring {
    fn active_key(&self) -> Result<Arc<RemoteSpoolKeyV1>, RemoteSqliteStorageErrorV1> {
        Ok(Arc::clone(&self.0))
    }

    fn key(
        &self,
        revision: u64,
    ) -> Result<Option<Arc<RemoteSpoolKeyV1>>, RemoteSqliteStorageErrorV1> {
        Ok((revision == self.0.revision()).then(|| Arc::clone(&self.0)))
    }
}

pub fn keyring() -> Arc<dyn RemoteSpoolKeyringV1> {
    Arc::new(Keyring(Arc::new(
        RemoteSpoolKeyV1::from_secret_bytes(7, vec![7; 32]).unwrap(),
    )))
}

/// The remote writer authority; its fence brain and epoch must equal the
/// shard's.
pub fn remote_writer(authority_epoch: u64) -> RemoteWriterAuthorityV1 {
    serde_json::from_value(json!({
        "project_id": "project.remote",
        "scope": {
            "project_id": "project.remote",
            "repository_id": "repository.remote",
            "worktree_id": "worktree.remote",
            "reference": "refs/heads/main",
            "snapshot_id": "snapshot.remote"
        },
        "authority": {
            "fence": {
                "brain_id": BRAIN,
                "shard_id": "shard.remote",
                "generation_id": "generation.remote",
                "placement_revision": 1,
                "authority_epoch": authority_epoch,
                "authority_node_id": "node.authority"
            },
            "credential_revision": 1,
            "observed_at": 10
        }
    }))
    .unwrap()
}

pub fn enrollment_grant(node: &str, authority_epoch: u64) -> EnrollmentGrantV1 {
    EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote.writer-scope").unwrap(),
        brain_id: BrainId::new(BRAIN).unwrap(),
        node_id: BrainNodeId::new(node).unwrap(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&[3_u8; 32]).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(100),
        revoked_at: None,
        capabilities: BTreeSet::from([RemoteCapabilityV1::Replay, RemoteCapabilityV1::Promote]),
        scope: remote_writer(authority_epoch).scope,
    }
}

pub fn enrollment_admission(grant: &EnrollmentGrantV1) -> RemoteEnrollmentAdmissionEvidenceV1 {
    let scope = ResolvedScope::new(
        grant.scope.project_id.clone(),
        grant.scope.repository_id.clone(),
        grant.scope.worktree_id.clone(),
        grant.scope.reference.clone(),
    )
    .unwrap();
    let grant_digest = canonical_sha256(grant).unwrap();
    RemoteEnrollmentAdmissionEvidenceV1::new(
        grant,
        scope.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: scope.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote.enrollment",
                1,
                grant_digest,
                ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(2),
        },
        ActorId::new("actor.remote").unwrap(),
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap(),
        Deadline::new(UtcMicros(100)).unwrap(),
    )
    .unwrap()
}

pub fn replay_policy(authority_epoch: u64) -> RemoteReplayPolicyEvidenceV1 {
    let repository_scope = remote_writer(authority_epoch).scope;
    let scope = ResolvedScope::new(
        repository_scope.project_id.clone(),
        repository_scope.repository_id.clone(),
        repository_scope.worktree_id.clone(),
        repository_scope.reference.clone(),
    )
    .unwrap();
    let policy_digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    RemoteReplayPolicyEvidenceV1 {
        scope,
        repository_scope,
        policy_revision: 1,
        decision: RemoteReplayPolicyDecisionV1::Admit,
        policy: PolicyDecisionRef::new(
            "policy.remote.capture",
            1,
            policy_digest.clone(),
            ComponentVersion::new("policy.remote.capture.v1").unwrap(),
        )
        .unwrap(),
        configuration_digest: policy_digest.clone(),
        catalog_digest: policy_digest.clone(),
        privacy_digest: policy_digest,
        revalidated_at: UtcMicros(10),
    }
}

pub fn admitted_capture(node: &str, authority_epoch: u64) -> AdmittedRemoteCaptureV1 {
    let payload = json!({ "kind": "assistant_message", "body": "spooled parity capture" });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.remote").unwrap(),
            ComponentVersion::new("sanitizer.remote.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("provider.remote").unwrap(),
                SessionId::new("session.remote").unwrap(),
            )
            .unwrap(),
            ObservationScopeV1::Project {
                project_id: ProjectId::new("project.remote").unwrap(),
            },
            ObservationSourceGenerationV1::new(1).unwrap(),
            ObservationSourceRangeV1::new(0, 1).unwrap(),
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new("observation.remote").unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.remote").unwrap(),
        payload,
    )
    .unwrap();
    AdmittedRemoteCaptureV1 {
        enrollment_id: EntityId::new("enrollment.remote").unwrap(),
        enrollment_revision: 1,
        node_id: BrainNodeId::new(node).unwrap(),
        writer: remote_writer(authority_epoch),
        policy_revision: 1,
        sequence: RemoteCaptureSequenceV1 {
            sequence: 1,
            previous_event_id: None,
        },
        observation,
        captured_at: UtcMicros(10),
    }
}
