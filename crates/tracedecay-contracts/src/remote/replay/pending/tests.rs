use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tracedecay_domain::{
    ActorId, AuthorityEpoch, BrainId, BrainNodeId, ComponentVersion, CurrentRemoteAuthorityStateV1,
    CurrentRemoteAuthorityV1, DurableObservationV1, EnrollmentCredentialRecordV1,
    EnrollmentGrantV1, EntityId, ManifestDigest, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, ProjectId,
    ProjectionGenerationId, ProviderId, RefId, RemoteCapabilityV1, RemoteCredentialFingerprintV1,
    RemotePlacementRevisionV1, RemoteRepositoryScopeV1, RemoteWriterFenceV1, RepositoryId,
    RepositoryStateSnapshotId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, ShardId, UtcMicros,
    WorktreeId, canonical_sha256,
};

use crate::remote::auth::{
    OpaqueRemoteCredential, RemoteAuthenticationError, RemoteAuthorityAuthenticationPort,
    RemoteEnrollmentAdmissionEvidenceV1, RemoteEnrollmentAuthorityErrorV1,
    RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentCredentialLookupPortV1,
};
use crate::remote::capture::{
    AdmittedRemoteCaptureV1, RemoteCapturePersistenceErrorV1, RemoteCaptureSequenceV1,
    RemoteWriterAuthorityV1,
};
use crate::remote::protocol::{
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
};
use crate::remote::replay::{
    RemoteReplayApplicationErrorV1, RemoteReplayBacklogEntryV1, RemoteReplayClockPortV1,
    RemoteReplayCommitReceiptV1, RemoteReplayCurrentWriterPortV1, RemoteReplayCurrentWriterV1,
    RemoteReplayFindingV1, RemoteReplayFrameLookupPortV1, RemoteReplayFrameV1,
    RemoteReplayPendingFrameV1, RemoteReplayPendingReceiptV1, RemoteReplayPendingRequestV1,
    RemoteReplayPendingStopV1, RemoteReplayPolicyDecisionV1, RemoteReplayPolicyEvidencePortV1,
    RemoteReplayPolicyEvidenceV1, RemoteReplayPolicyPortV1, RemoteReplayProtocolAdapterV1,
    RemoteReplayRefusalV1, RemoteReplayRequestV1, RemoteReplayServiceErrorV1,
    RemoteReplayServiceV1, RemoteReplaySpoolPortV1, RemoteReplaySpoolStateV1, RemoteReplayStateV1,
    RemoteReplayTransactionErrorV1, RemoteReplayTransactionOutcomeV1,
    RemoteReplayTransactionPortV1, RemoteReplayTransitionReceiptV1, RemoteReplayTransitionV1,
    canonical_remote_event_id_v1, replay_protocol_failure,
};
use crate::{
    ApplicationOutcome, AuthorityReceipt, CapabilityGrantId, Deadline, DisclosureClass,
    OperationBudgetUsage, PolicyDecisionRef, RequestId, ResolvedScope,
};

const SECRET: [u8; 32] = [b'n'; 32];

fn digest(fill: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
}

fn budget() -> OperationBudgetUsage {
    OperationBudgetUsage {
        units_consumed: 1,
        bytes_consumed: 1,
        elapsed_micros: 0,
    }
}

fn scope() -> RemoteRepositoryScopeV1 {
    RemoteRepositoryScopeV1 {
        project_id: ProjectId::new("project.remote").unwrap(),
        repository_id: RepositoryId::new("repository.remote").unwrap(),
        worktree_id: WorktreeId::new("worktree.remote").unwrap(),
        reference: Some(RefId::new("refs/heads/main").unwrap()),
        snapshot_id: RepositoryStateSnapshotId::new("snapshot.remote").unwrap(),
    }
}

fn resolved_scope() -> ResolvedScope {
    let scope = scope();
    ResolvedScope::new(
        scope.project_id,
        scope.repository_id,
        scope.worktree_id,
        scope.reference,
    )
    .unwrap()
}

fn fence(epoch: u64) -> RemoteWriterFenceV1 {
    RemoteWriterFenceV1 {
        brain_id: BrainId::new("brain.remote").unwrap(),
        shard_id: ShardId::new("shard.remote").unwrap(),
        generation_id: ProjectionGenerationId::new("generation.remote").unwrap(),
        placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
        authority_epoch: AuthorityEpoch(epoch),
        authority_node_id: BrainNodeId::new("node.remote").unwrap(),
    }
}

fn writer() -> RemoteWriterAuthorityV1 {
    RemoteWriterAuthorityV1 {
        project_id: scope().project_id,
        scope: scope(),
        authority: CurrentRemoteAuthorityV1 {
            fence: fence(1),
            credential_revision: 1,
            observed_at: UtcMicros(10),
        },
    }
}

/// The caller is also the published writer, as on a single-node Brain, so
/// one enrollment answers both the caller and the authority lookup.
fn enrollment() -> (
    EnrollmentCredentialRecordV1,
    RemoteEnrollmentCommitReceiptV1,
) {
    let capabilities = BTreeSet::from([
        RemoteCapabilityV1::Replay,
        RemoteCapabilityV1::ServeAuthority,
    ]);
    let grant = EnrollmentGrantV1 {
        grant_id: EntityId::new("grant.remote").unwrap(),
        brain_id: BrainId::new("brain.remote").unwrap(),
        node_id: BrainNodeId::new("node.remote").unwrap(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&[b'g'; 32]).unwrap(),
        revision: 1,
        issued_at: UtcMicros(1),
        expires_at: UtcMicros(1_000_000),
        revoked_at: None,
        capabilities: capabilities.clone(),
        scope: scope(),
    };
    let record = EnrollmentCredentialRecordV1 {
        enrollment_id: EntityId::new("enrollment.remote").unwrap(),
        brain_id: grant.brain_id.clone(),
        node_id: grant.node_id.clone(),
        fingerprint: RemoteCredentialFingerprintV1::from_secret(&SECRET).unwrap(),
        revision: 1,
        issued_at: UtcMicros(10),
        expires_at: UtcMicros(1_000_000),
        revoked_at: None,
        capabilities,
        scope: scope(),
    };
    let grant_digest = canonical_sha256(&grant).unwrap();
    let resolved = resolved_scope();
    let admission = RemoteEnrollmentAdmissionEvidenceV1::new(
        &grant,
        resolved.clone(),
        AuthorityReceipt {
            grant_id: CapabilityGrantId::new(grant.grant_id.as_str()).unwrap(),
            grant_revision: grant.revision,
            grant_digest: grant_digest.clone(),
            authorized_scope_digest: resolved.scope_digest,
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.remote.enrollment",
                1,
                grant_digest.clone(),
                ComponentVersion::new("policy.remote.enrollment.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(2),
        },
        ActorId::new("actor.remote").unwrap(),
        digest('b'),
        digest('c'),
        digest('d'),
        Deadline::new(UtcMicros(1_000_000)).unwrap(),
    )
    .unwrap();
    let receipt = RemoteEnrollmentCommitReceiptV1 {
        admission,
        prior_grant_digest: grant_digest,
        input_digest: digest('e'),
        committed_state_digest: canonical_sha256(&record).unwrap(),
        consumed_at: record.issued_at,
        budget: budget(),
        enrollment: record.clone(),
    };
    receipt.validate().unwrap();
    (record, receipt)
}

fn observation() -> DurableObservationV1 {
    let payload = json!({"kind": "assistant_message", "body": "offline"});
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
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("provider.remote").unwrap(),
                SessionId::new("session.remote").unwrap(),
            )
            .unwrap(),
            ObservationScopeV1::Project {
                project_id: scope().project_id,
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
    .unwrap()
}

/// `count` chained frames of the caller's enrollment, lowest sequence first.
fn frames(count: u64) -> Vec<RemoteReplayFrameV1> {
    let mut frames: Vec<RemoteReplayFrameV1> = Vec::new();
    for sequence in 1..=count {
        let capture = AdmittedRemoteCaptureV1 {
            enrollment_id: EntityId::new("enrollment.remote").unwrap(),
            enrollment_revision: 1,
            node_id: BrainNodeId::new("node.remote").unwrap(),
            writer: writer(),
            policy_revision: 1,
            sequence: RemoteCaptureSequenceV1 {
                sequence,
                previous_event_id: frames.last().map(|frame| frame.event_id.clone()),
            },
            observation: observation(),
            captured_at: UtcMicros(10),
        };
        frames.push(RemoteReplayFrameV1 {
            event_id: canonical_remote_event_id_v1(&capture).unwrap(),
            capture,
        });
    }
    frames
}

struct Row {
    sequence: u64,
    state: RemoteReplayStateV1,
    receipt: Option<RemoteReplayCommitReceiptV1>,
    finding: Option<RemoteReplayFindingV1>,
    last_attempt: u64,
    attempt_open: bool,
}

impl Row {
    fn spool_state(&self) -> RemoteReplaySpoolStateV1 {
        RemoteReplaySpoolStateV1 {
            state: self.state,
            receipt: self.receipt.clone(),
            last_attempt: self.last_attempt,
            finding: self.finding,
        }
    }
}

/// One in-memory authority behind every replay port: the caller's enrollment,
/// the published writer, a project store that may refuse chosen frames, and
/// the spool those frames wait in.
struct FakeAuthority {
    caller: EnrollmentCredentialRecordV1,
    admission: RemoteEnrollmentCommitReceiptV1,
    writer: RemoteWriterAuthorityV1,
    frames: Vec<RemoteReplayFrameV1>,
    refused: BTreeSet<String>,
    rows: Mutex<BTreeMap<String, Row>>,
    commits: AtomicUsize,
    clock: AtomicI64,
}

impl FakeAuthority {
    fn new(frames: Vec<RemoteReplayFrameV1>, refused: BTreeSet<String>) -> Arc<Self> {
        let (caller, admission) = enrollment();
        let rows = frames
            .iter()
            .map(|frame| {
                (
                    frame.event_id.clone(),
                    Row {
                        sequence: frame.capture.sequence.sequence,
                        state: RemoteReplayStateV1::Pending,
                        receipt: None,
                        finding: None,
                        last_attempt: 0,
                        attempt_open: false,
                    },
                )
            })
            .collect();
        Arc::new(Self {
            caller,
            admission,
            writer: writer(),
            frames,
            refused,
            rows: Mutex::new(rows),
            commits: AtomicUsize::new(0),
            clock: AtomicI64::new(100),
        })
    }

    fn tick(&self) -> UtcMicros {
        UtcMicros(self.clock.fetch_add(1, Ordering::SeqCst))
    }

    fn commits(&self) -> usize {
        self.commits.load(Ordering::SeqCst)
    }

    fn state(&self, event_id: &str) -> RemoteReplayStateV1 {
        self.rows.lock().unwrap()[event_id].state
    }

    fn service(self: &Arc<Self>) -> RemoteReplayServiceV1 {
        RemoteReplayServiceV1::new_with_clock(
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
        )
    }
}

impl RemoteAuthorityAuthenticationPort for FakeAuthority {
    fn authenticate_connected_authority(
        &self,
        _expected_authority: &CurrentRemoteAuthorityV1,
        _expected_credential: &EnrollmentCredentialRecordV1,
        _observed_at: UtcMicros,
    ) -> Result<(), RemoteAuthenticationError> {
        Ok(())
    }
}

impl RemoteEnrollmentCredentialLookupPortV1 for FakeAuthority {
    fn enrollment_by_id(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        (enrollment_id == &self.caller.enrollment_id)
            .then(|| self.caller.clone())
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)
    }

    fn authority_enrollment(
        &self,
        brain_id: &BrainId,
        node_id: &BrainNodeId,
        revision: u64,
    ) -> Result<EnrollmentCredentialRecordV1, RemoteEnrollmentAuthorityErrorV1> {
        (brain_id == &self.caller.brain_id
            && node_id == &self.caller.node_id
            && revision == self.caller.revision)
            .then(|| self.caller.clone())
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)
    }

    fn enrollment_commit_receipt(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<RemoteEnrollmentCommitReceiptV1, RemoteEnrollmentAuthorityErrorV1> {
        (enrollment_id == &self.caller.enrollment_id)
            .then(|| self.admission.clone())
            .ok_or(RemoteEnrollmentAuthorityErrorV1::GrantNotFound)
    }
}

impl RemoteReplayFrameLookupPortV1 for FakeAuthority {
    fn load_replay_frame(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplayFrameV1, RemoteCapturePersistenceErrorV1> {
        self.frames
            .iter()
            .find(|frame| frame.event_id == event_id)
            .cloned()
            .ok_or(RemoteCapturePersistenceErrorV1::Unavailable)
    }
}

impl RemoteReplayCurrentWriterPortV1 for FakeAuthority {
    fn current_writer(
        &self,
        _fence: &RemoteWriterFenceV1,
    ) -> Result<RemoteReplayCurrentWriterV1, RemoteCapturePersistenceErrorV1> {
        Ok(RemoteReplayCurrentWriterV1 {
            writer: Some(self.writer.clone()),
            state: CurrentRemoteAuthorityStateV1::Available(self.writer.authority.clone()),
        })
    }
}

impl RemoteReplayPolicyPortV1 for FakeAuthority {
    fn authorize_current_policy(
        &self,
        _frame: &RemoteReplayFrameV1,
        _observed_at: UtcMicros,
    ) -> Result<RemoteReplayPolicyDecisionV1, RemoteReplayApplicationErrorV1> {
        Ok(RemoteReplayPolicyDecisionV1::Admit)
    }
}

impl RemoteReplayPolicyEvidencePortV1 for FakeAuthority {
    fn current_policy_evidence(
        &self,
        _frame: &RemoteReplayFrameV1,
    ) -> Result<RemoteReplayPolicyEvidenceV1, RemoteReplayApplicationErrorV1> {
        Ok(RemoteReplayPolicyEvidenceV1 {
            scope: resolved_scope(),
            repository_scope: scope(),
            policy_revision: 1,
            decision: RemoteReplayPolicyDecisionV1::Admit,
            policy: PolicyDecisionRef::new(
                "policy.remote.replay",
                1,
                digest('f'),
                ComponentVersion::new("policy.remote.replay.v1").unwrap(),
            )
            .unwrap(),
            configuration_digest: digest('b'),
            catalog_digest: digest('c'),
            privacy_digest: digest('d'),
            revalidated_at: UtcMicros(10),
        })
    }
}

impl RemoteReplayTransactionPortV1 for FakeAuthority {
    fn commit(
        &self,
        frame: &RemoteReplayFrameV1,
        current_writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
        let commit_sequence = self.commits.fetch_add(1, Ordering::SeqCst) + 1;
        if self.refused.contains(&frame.event_id) {
            return Ok(RemoteReplayTransactionOutcomeV1::Refused);
        }
        Ok(RemoteReplayTransactionOutcomeV1::Admitted(
            RemoteReplayCommitReceiptV1 {
                event_id: frame.event_id.clone(),
                writer_fence: current_writer.authority.fence.clone(),
                commit_sequence: u64::try_from(commit_sequence).unwrap(),
                committed_at: self.tick(),
                budget: budget(),
            },
        ))
    }
}

impl RemoteReplaySpoolPortV1 for FakeAuthority {
    fn state(
        &self,
        event_id: &str,
    ) -> Result<RemoteReplaySpoolStateV1, RemoteCapturePersistenceErrorV1> {
        self.rows
            .lock()
            .unwrap()
            .get(event_id)
            .map(Row::spool_state)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)
    }

    fn transition(
        &self,
        transition: RemoteReplayTransitionV1,
    ) -> Result<RemoteReplayTransitionReceiptV1, RemoteCapturePersistenceErrorV1> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .get_mut(&transition.event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        if row.state != transition.from
            || row.last_attempt != transition.replay_attempt
            || !row.attempt_open
        {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        let pre_state_digest = canonical_sha256(&row.spool_state()).unwrap();
        row.state = transition.to;
        row.receipt = transition.receipt.clone();
        row.finding = transition.finding;
        row.attempt_open = !transition.to.is_settled();
        Ok(RemoteReplayTransitionReceiptV1 {
            event_id: transition.event_id,
            replay_attempt: transition.replay_attempt,
            from: transition.from,
            to: transition.to,
            pre_state_digest,
            terminal_state_digest: canonical_sha256(&row.spool_state()).unwrap(),
            committed_at: transition.observed_at,
            budget: budget(),
        })
    }

    fn begin_replay_attempt(
        &self,
        event_id: &str,
        _observed_at: UtcMicros,
    ) -> Result<u64, RemoteCapturePersistenceErrorV1> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .get_mut(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        if row.attempt_open {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        row.last_attempt += 1;
        row.attempt_open = true;
        Ok(row.last_attempt)
    }

    fn abandon_replay_attempt(
        &self,
        event_id: &str,
        replay_attempt: u64,
    ) -> Result<(), RemoteCapturePersistenceErrorV1> {
        let mut rows = self.rows.lock().unwrap();
        let row = rows
            .get_mut(event_id)
            .ok_or(RemoteCapturePersistenceErrorV1::Corruption)?;
        if row.last_attempt != replay_attempt {
            return Err(RemoteCapturePersistenceErrorV1::Corruption);
        }
        row.attempt_open = false;
        Ok(())
    }

    fn replay_backlog(
        &self,
        enrollment_id: &EntityId,
    ) -> Result<Vec<RemoteReplayBacklogEntryV1>, RemoteCapturePersistenceErrorV1> {
        if enrollment_id != &self.caller.enrollment_id {
            return Ok(Vec::new());
        }
        let rows = self.rows.lock().unwrap();
        let mut backlog = rows
            .iter()
            .map(|(event_id, row)| RemoteReplayBacklogEntryV1 {
                event_id: event_id.clone(),
                sequence: row.sequence,
                state: row.state,
                receipt: row.receipt.clone(),
                finding: row.finding,
            })
            .collect::<Vec<_>>();
        backlog.sort_by_key(|entry| entry.sequence);
        Ok(backlog)
    }
}

impl RemoteReplayClockPortV1 for FakeAuthority {
    fn now(&self) -> Result<UtcMicros, RemoteReplayApplicationErrorV1> {
        Ok(self.tick())
    }
}

fn request(
    request_id: &str,
    expected: Option<RemoteWriterFenceV1>,
) -> RemoteProtocolRequestV1<RemoteReplayPendingRequestV1> {
    RemoteProtocolRequestV1::new(
        RequestId::new(request_id).unwrap(),
        BrainId::new("brain.remote").unwrap(),
        BrainNodeId::new("node.remote").unwrap(),
        1,
        expected,
        UtcMicros(50),
        RemoteReplayPendingRequestV1 {},
    )
    .unwrap()
}

fn replay_pending(
    authority: &Arc<FakeAuthority>,
    request_id: &str,
) -> Result<RemoteReplayPendingReceiptV1, RemoteReplayServiceErrorV1> {
    authority
        .service()
        .replay_pending(
            &request(request_id, Some(fence(1))),
            &OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap(),
        )
        .map(|outcome| outcome.receipt)
}

fn committed(receipt: &RemoteReplayPendingReceiptV1) -> Vec<&RemoteReplayCommitReceiptV1> {
    receipt
        .acknowledged
        .iter()
        .map(|frame| &frame.receipt)
        .collect()
}

#[test]
fn a_resend_after_every_frame_is_acknowledged_commits_nothing() {
    let frames = frames(2);
    let authority = FakeAuthority::new(frames.clone(), BTreeSet::new());

    let first = replay_pending(&authority, "request.remote-replay-pending").unwrap();
    assert_eq!(
        first
            .acknowledged
            .iter()
            .map(|frame| (
                frame.sequence,
                frame.receipt.event_id.as_str(),
                frame.replayed.is_some()
            ))
            .collect::<Vec<_>>(),
        vec![
            (1, frames[0].event_id.as_str(), true),
            (2, frames[1].event_id.as_str(), true),
        ],
        "both frames commit in sequence order through this step"
    );
    assert_eq!(first.stopped_at, None);
    assert_eq!(authority.commits(), 2);

    let again = replay_pending(&authority, "request.remote-replay-pending-again").unwrap();
    assert_eq!(committed(&again), committed(&first));
    assert!(
        again
            .acknowledged
            .iter()
            .all(|frame| frame.replayed.is_none()),
        "a resend reports settled frames without replaying them"
    );
    assert_eq!(again.stopped_at, None);
    assert_eq!(authority.commits(), 2, "a resend never commits again");
}

#[test]
fn a_frame_whose_acknowledgement_was_lost_acknowledges_without_a_commit() {
    let frames = frames(2);
    let authority = FakeAuthority::new(frames.clone(), BTreeSet::new());
    let lost = RemoteReplayCommitReceiptV1 {
        event_id: frames[0].event_id.clone(),
        writer_fence: fence(1),
        commit_sequence: 7,
        committed_at: UtcMicros(20),
        budget: budget(),
    };
    {
        let mut rows = authority.rows.lock().unwrap();
        let row = rows.get_mut(&frames[0].event_id).unwrap();
        row.state = RemoteReplayStateV1::Admitted;
        row.receipt = Some(lost.clone());
    }

    let receipt = replay_pending(&authority, "request.remote-replay-pending").unwrap();
    assert_eq!(
        receipt.acknowledged.first(),
        Some(&RemoteReplayPendingFrameV1 {
            sequence: 1,
            receipt: lost,
            replayed: receipt.acknowledged[0].replayed.clone(),
        })
    );
    assert!(receipt.acknowledged[0].replayed.is_some());
    assert_eq!(receipt.acknowledged.len(), 2);
    assert_eq!(
        authority.commits(),
        1,
        "only the frame that never committed reaches the project store"
    );
}

#[test]
fn a_stale_fence_refuses_even_when_every_frame_is_settled() {
    let authority = FakeAuthority::new(frames(2), BTreeSet::new());
    replay_pending(&authority, "request.remote-replay-pending").unwrap();
    let credential = OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap();

    for expected in [Some(fence(2)), None] {
        let error = authority
            .service()
            .replay_pending(
                &request("request.remote-replay-pending-stale", expected.clone()),
                &credential,
            )
            .map(|outcome| outcome.receipt)
            .expect_err("a guided replay checks the fence before it reports anything");
        assert!(
            matches!(
                error,
                RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(_)
            ),
            "{expected:?}: {error:?}"
        );
        assert_eq!(
            replay_protocol_failure(error),
            RemoteProtocolFailureV1::StaleAuthorityFence
        );
    }
    assert_eq!(authority.commits(), 2);
}

#[test]
fn a_store_refusal_stops_the_walk_and_a_resend_repeats_the_stop() {
    let frames = frames(3);
    let authority =
        FakeAuthority::new(frames.clone(), BTreeSet::from([frames[1].event_id.clone()]));

    let first = replay_pending(&authority, "request.remote-replay-pending").unwrap();
    assert_eq!(
        first
            .acknowledged
            .iter()
            .map(|frame| (frame.sequence, frame.replayed.is_some()))
            .collect::<Vec<_>>(),
        vec![(1, true)]
    );
    let stop = first
        .stopped_at
        .clone()
        .expect("the refused frame stops the walk");
    assert_eq!(
        (
            stop.event_id.as_str(),
            stop.sequence,
            stop.refusal,
            stop.finding,
            stop.replayed.is_some()
        ),
        (
            frames[1].event_id.as_str(),
            2,
            RemoteReplayRefusalV1::Rejected,
            RemoteReplayFindingV1::StoreRefused,
            true
        )
    );
    assert_eq!(
        authority.state(&frames[2].event_id),
        RemoteReplayStateV1::Pending,
        "frames after the refusal stay pending"
    );
    assert_eq!(authority.commits(), 2);

    let again = replay_pending(&authority, "request.remote-replay-pending-again").unwrap();
    assert_eq!(committed(&again), committed(&first));
    assert!(again.acknowledged[0].replayed.is_none());
    assert_eq!(
        again.stopped_at,
        Some(RemoteReplayPendingStopV1 {
            replayed: None,
            ..stop
        }),
        "a resend repeats the stop without replaying the refused frame"
    );
    assert_eq!(authority.commits(), 2, "a resend never reaches the store");
    assert_eq!(
        authority.state(&frames[2].event_id),
        RemoteReplayStateV1::Pending
    );
}

fn single_request(
    request_id: &str,
    expected: Option<RemoteWriterFenceV1>,
    event_id: &str,
) -> RemoteProtocolRequestV1<RemoteReplayRequestV1> {
    RemoteProtocolRequestV1::new(
        RequestId::new(request_id).unwrap(),
        BrainId::new("brain.remote").unwrap(),
        BrainNodeId::new("node.remote").unwrap(),
        1,
        expected,
        UtcMicros(50),
        RemoteReplayRequestV1 {
            event_id: event_id.to_owned(),
        },
    )
    .unwrap()
}

#[test]
fn a_single_replay_of_an_acknowledged_frame_reports_its_stored_receipt() {
    let frames = frames(2);
    let authority = FakeAuthority::new(frames.clone(), BTreeSet::new());
    let credential = OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap();
    let first = replay_pending(&authority, "request.remote-replay-pending").unwrap();
    let stored = first.acknowledged[0].receipt.clone();
    let expected = json!({"outcome": "already_acknowledged", "receipt": stored});

    for request_id in [
        "request.remote-replay-single",
        "request.remote-replay-single-again",
    ] {
        let outcome = authority
            .service()
            .replay(
                &single_request(request_id, Some(fence(1)), &frames[0].event_id),
                &credential,
            )
            .expect("a single replay of an acknowledged frame answers with its receipt")
            .outcome;
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            expected,
            "{request_id}"
        );
    }

    let response = RemoteReplayProtocolAdapterV1::new(authority.service())
        .execute(
            single_request(
                "request.remote-replay-single-protocol",
                Some(fence(1)),
                &frames[0].event_id,
            ),
            OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap(),
        )
        .unwrap();
    let envelope = response
        .result
        .expect("the protocol answers a resent replay with success");
    let ApplicationOutcome::Effect(effect) = envelope.outcome else {
        panic!("a replay answers with an effect");
    };
    assert_eq!(
        serde_json::to_value(effect.payload.as_ref().unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        effect.receipt.committed_state.as_ref(),
        Some(
            &first.acknowledged[0]
                .replayed
                .as_ref()
                .unwrap()
                .committed_effect_digest
        ),
        "the resent replay names the state the original replay committed"
    );

    assert_eq!(
        authority.commits(),
        2,
        "a resent replay never commits again"
    );
    assert_eq!(
        authority.state(&frames[0].event_id),
        RemoteReplayStateV1::Acknowledged
    );

    authority
        .rows
        .lock()
        .unwrap()
        .get_mut(&frames[0].event_id)
        .unwrap()
        .state = RemoteReplayStateV1::GarbageCollectionEligible;
    let outcome = authority
        .service()
        .replay(
            &single_request(
                "request.remote-replay-single-collectable",
                Some(fence(1)),
                &frames[0].event_id,
            ),
            &credential,
        )
        .expect("a collectable frame still answers with its receipt")
        .outcome;
    assert_eq!(serde_json::to_value(&outcome).unwrap(), expected);
    assert_eq!(authority.commits(), 2);
}

#[test]
fn a_single_replay_of_an_acknowledged_frame_still_checks_the_fence_and_credential() {
    let frames = frames(1);
    let authority = FakeAuthority::new(frames.clone(), BTreeSet::new());
    replay_pending(&authority, "request.remote-replay-pending").unwrap();
    let credential = OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap();

    let stale = authority
        .service()
        .replay(
            &single_request(
                "request.remote-replay-single-stale",
                Some(fence(2)),
                &frames[0].event_id,
            ),
            &credential,
        )
        .expect_err("a stale fence refuses before the stored receipt is reported");
    assert!(
        matches!(
            stale,
            RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(_)
        ),
        "{stale:?}"
    );

    let impostor = authority
        .service()
        .replay(
            &single_request(
                "request.remote-replay-single-impostor",
                Some(fence(1)),
                &frames[0].event_id,
            ),
            &OpaqueRemoteCredential::new([b'x'; 32].to_vec()).unwrap(),
        )
        .expect_err("an unauthenticated caller never reads the stored receipt");
    assert_eq!(
        impostor,
        RemoteReplayServiceErrorV1::Replay(RemoteReplayApplicationErrorV1::Authentication(
            RemoteAuthenticationError::InvalidCredential
        ))
    );
    assert_eq!(authority.commits(), 1);
    assert_eq!(
        authority.state(&frames[0].event_id),
        RemoteReplayStateV1::Acknowledged
    );
}

#[test]
fn a_single_replay_of_a_refused_frame_reports_its_stored_finding() {
    let frames = frames(2);
    let authority =
        FakeAuthority::new(frames.clone(), BTreeSet::from([frames[0].event_id.clone()]));
    let credential = OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap();
    replay_pending(&authority, "request.remote-replay-pending").unwrap();
    {
        let mut rows = authority.rows.lock().unwrap();
        let row = rows.get_mut(&frames[1].event_id).unwrap();
        row.state = RemoteReplayStateV1::Quarantined;
        row.finding = Some(RemoteReplayFindingV1::PolicyChanged);
    }
    let row = |event_id: &str| {
        let rows = authority.rows.lock().unwrap();
        let row = &rows[event_id];
        (row.state, row.finding, row.last_attempt, row.attempt_open)
    };
    let before = [row(&frames[0].event_id), row(&frames[1].event_id)];

    for (frame, expected) in [
        (
            &frames[0],
            json!({"outcome": "already_refused", "refusal": "rejected", "finding": "store_refused"}),
        ),
        (
            &frames[1],
            json!({"outcome": "already_refused", "refusal": "quarantined", "finding": "policy_changed"}),
        ),
    ] {
        for request_id in [
            "request.remote-replay-single",
            "request.remote-replay-single-again",
        ] {
            let outcome = authority
                .service()
                .replay(
                    &single_request(request_id, Some(fence(1)), &frame.event_id),
                    &credential,
                )
                .expect("a single replay of a refused frame answers with its finding")
                .outcome;
            assert_eq!(
                serde_json::to_value(&outcome).unwrap(),
                expected,
                "{request_id}"
            );
        }
        let response = RemoteReplayProtocolAdapterV1::new(authority.service())
            .execute(
                single_request(
                    "request.remote-replay-single-protocol",
                    Some(fence(1)),
                    &frame.event_id,
                ),
                OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap(),
            )
            .unwrap();
        let envelope = response
            .result
            .expect("the protocol answers a refused frame with a terminal outcome");
        let ApplicationOutcome::Effect(effect) = envelope.outcome else {
            panic!("a replay answers with an effect");
        };
        assert_eq!(
            serde_json::to_value(effect.payload.as_ref().unwrap()).unwrap(),
            expected
        );
        assert_eq!(
            effect.receipt.committed_state.as_ref(),
            Some(&effect.receipt.expected_state),
            "a replay of a refused frame changes no state"
        );
    }

    assert_eq!(authority.commits(), 1, "a refused frame never commits");
    assert_eq!(
        [row(&frames[0].event_id), row(&frames[1].event_id)],
        before,
        "a replay of a refused frame opens no attempt and leaves the row as it was"
    );
}

#[test]
fn a_single_replay_of_a_refused_frame_authenticates_before_it_reports_the_finding() {
    let frames = frames(1);
    let authority =
        FakeAuthority::new(frames.clone(), BTreeSet::from([frames[0].event_id.clone()]));
    replay_pending(&authority, "request.remote-replay-pending").unwrap();

    let impostor = authority
        .service()
        .replay(
            &single_request(
                "request.remote-replay-single-impostor",
                Some(fence(1)),
                &frames[0].event_id,
            ),
            &OpaqueRemoteCredential::new([b'x'; 32].to_vec()).unwrap(),
        )
        .expect_err("an unauthenticated caller never reads the stored finding");
    assert_eq!(
        impostor,
        RemoteReplayServiceErrorV1::Replay(RemoteReplayApplicationErrorV1::Authentication(
            RemoteAuthenticationError::InvalidCredential
        ))
    );
    let stale = authority
        .service()
        .replay(
            &single_request(
                "request.remote-replay-single-stale",
                Some(fence(2)),
                &frames[0].event_id,
            ),
            &OpaqueRemoteCredential::new(SECRET.to_vec()).unwrap(),
        )
        .expect_err("a stale fence refuses before the stored finding is reported");
    assert!(
        matches!(
            stale,
            RemoteReplayServiceErrorV1::ExpectedAuthorityMismatch(_)
        ),
        "{stale:?}"
    );
    assert_eq!(
        authority.state(&frames[0].event_id),
        RemoteReplayStateV1::Rejected
    );
}
