use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use tempfile::TempDir;
use tracedecay_contracts::remote::capture::{
    AdmittedRemoteCaptureV1, RemoteCaptureSequenceV1, RemoteWriterAuthorityV1,
};
use tracedecay_contracts::remote::replay::{
    RemoteReplayFrameV1, RemoteReplayTransactionErrorV1, RemoteReplayTransactionOutcomeV1,
    RemoteReplayTransactionPortV1, canonical_remote_event_id_v1,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::{
    AuthorityEpoch, BrainNodeId, ComponentVersion, DurableObservationV1, EntityId, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProviderId, RemotePlacementRevisionV1, RemoteWriterFenceV1,
    RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_rusqlite_runtime::native_runtime::{self, NativeTask, NativeTaskOutcome};
use tracedecay_store::{
    RemoteWriterFenceInstallV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1,
};

use super::{DaemonRemoteReplayTransactionAuthorityV1, ReplayRequestProbeV1};
use crate::session_registry::DaemonSessionRuntimeRegistryV1;

struct Fixture {
    authority: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
    project_id: ProjectId,
    _registry: DaemonSessionRuntimeRegistryV1,
    _sessions: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    _scope: tracedecay_runtime_core::db::DaemonDatabaseScope,
    _temp: TempDir,
}

/// One mounted project whose sessions store is the registered replay target.
async fn fixture() -> Fixture {
    let temp = TempDir::new().expect("fixture root");
    let profile_root = temp.path().join("profile");
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        43,
        "remote replay transaction",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_id = ProjectId::new("project.replay-test").expect("project id");
    let project_root = temp.path().join("replay-test");
    std::fs::create_dir_all(&project_root).expect("project root");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let sessions = registry
        .project_sessions(project_id.clone(), [project_root])
        .await
        .expect("project sessions mount registers the replay target");
    Fixture {
        authority: registry.remote_replay_transaction(),
        project_id,
        _registry: registry,
        _sessions: sessions,
        _scope: scope,
        _temp: temp,
    }
}

fn writer(project_id: &ProjectId, epoch: u64) -> RemoteWriterAuthorityV1 {
    serde_json::from_value(json!({
        "project_id": project_id.as_str(),
        "scope": {
            "project_id": project_id.as_str(),
            "repository_id": "repository.replay-test",
            "worktree_id": "worktree.replay-test",
            "reference": "refs/heads/main",
            "snapshot_id": "snapshot.replay-test"
        },
        "authority": {
            "fence": {
                "brain_id": "brain.replay-test",
                "shard_id": "shard.replay-test",
                "generation_id": "generation.replay-test",
                "placement_revision": epoch,
                "authority_epoch": epoch,
                "authority_node_id": "node.replay-test"
            },
            "credential_revision": 1,
            "observed_at": 1
        }
    }))
    .expect("remote writer authority")
}

fn observation(project_id: &ProjectId, sequence: u64) -> DurableObservationV1 {
    let payload = json!({"kind": "assistant_message", "body": format!("frame {sequence}")});
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.replay-test-{sequence}"))
                .expect("receipt id"),
            ComponentVersion::new("sanitizer.replay-test.v1").expect("sanitizer version"),
        )
        .expect("receipt ref"),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).expect("payload reference")),
    )
    .expect("sanitization receipt");
    DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            ObservationSourceIdentityV1::for_provider(
                ProviderId::new("provider.replay-test").expect("provider"),
                SessionId::new("session.replay-test").expect("session"),
            )
            .expect("source identity"),
            ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            ObservationSourceGenerationV1::new(1).expect("generation"),
            ObservationSourceRangeV1::new(sequence - 1, sequence).expect("range"),
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new(format!("observation.replay-test-{sequence}"))
                .expect("observation id"),
        )
        .expect("identity material"),
        receipt,
        RetentionClass::new("retention.replay-test").expect("retention"),
        payload,
    )
    .expect("durable observation")
}

/// `count` chained frames captured under `writer`, lowest sequence first.
fn frames(writer: &RemoteWriterAuthorityV1, count: u64) -> Vec<RemoteReplayFrameV1> {
    let mut frames: Vec<RemoteReplayFrameV1> = Vec::new();
    for sequence in 1..=count {
        let capture = AdmittedRemoteCaptureV1 {
            enrollment_id: EntityId::new("enrollment.replay-test").expect("enrollment"),
            enrollment_revision: 1,
            node_id: BrainNodeId::new("node.replay-test").expect("node"),
            writer: writer.clone(),
            policy_revision: 1,
            sequence: RemoteCaptureSequenceV1 {
                sequence,
                previous_event_id: frames.last().map(|frame| frame.event_id.clone()),
            },
            observation: observation(&writer.project_id, sequence),
            captured_at: UtcMicros(10),
        };
        frames.push(RemoteReplayFrameV1 {
            event_id: canonical_remote_event_id_v1(&capture).expect("event id"),
            capture,
        });
    }
    frames
}

fn uninterrupted_probe() -> Arc<ReplayRequestProbeV1> {
    Arc::new(ReplayRequestProbeV1 {
        cancellation: RuntimeCancellationIdentityV1 {
            cancellation_id: RuntimeCancellationIdV1::new("cancellation.replay-test")
                .expect("cancellation id"),
            generation: 1,
        },
        deadline: RuntimeDeadlineV1 {
            deadline_id: RuntimeDeadlineIdV1::new("deadline.replay-test").expect("deadline id"),
        },
        accepting: Arc::new(AtomicBool::new(true)),
        commit_started: AtomicBool::new(false),
    })
}

fn admitted(
    outcome: Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1>,
) -> bool {
    matches!(outcome, Ok(RemoteReplayTransactionOutcomeV1::Admitted(_)))
}

fn remote_replay_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .expect("thread listing")
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
        .filter(|name| name.trim_end().starts_with("tracedecay-remo"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_and_fence_read_awaited_on_a_native_worker_never_park_it() {
    let fixture = fixture().await;
    let writer = writer(&fixture.project_id, 1);
    let frame = frames(&writer, 1).remove(0);
    let authority_key = writer
        .authority
        .fence
        .recovery_authority_key()
        .expect("authority key");
    let authority = Arc::clone(&fixture.authority);
    let project_id = fixture.project_id.clone();
    let task_writer = writer.clone();
    let writer_waits = native_runtime::native_thread_writer_waits();
    let reader_waits = native_runtime::native_thread_reader_waits();

    let task = NativeTask::spawn(async move {
        assert!(
            native_runtime::Runtime::current_handle().is_some(),
            "the replay must be awaited on a native worker"
        );
        let replay = authority.commit(&frame, &task_writer).await;
        let fence = authority
            .current_writer_fence(project_id, authority_key)
            .await;
        (replay, fence)
    })
    .expect("native task admitted");
    let NativeTaskOutcome::Completed((replay, fence)) = task.wait().await else {
        panic!("replay task did not complete");
    };

    assert!(
        admitted(replay.clone()),
        "the replay commits on a native worker: {replay:?}"
    );
    assert_eq!(
        fence,
        Ok(Some((writer.authority.fence.clone(), 1))),
        "the fence read settles on a native worker and sees the replay's frontier"
    );
    assert_eq!(
        native_runtime::native_thread_writer_waits(),
        writer_waits,
        "no writer wait parked a native worker"
    );
    assert_eq!(
        native_runtime::native_thread_reader_waits(),
        reader_waits,
        "no reader wait parked a native worker"
    );
    assert_eq!(
        remote_replay_threads(),
        0,
        "the replay authority owns no thread of its own"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_ahead_of_its_predecessor_commits_nothing() {
    let fixture = fixture().await;
    let writer = writer(&fixture.project_id, 1);
    let frames = frames(&writer, 2);

    assert_eq!(
        fixture.authority.commit(&frames[1], &writer).await,
        Ok(RemoteReplayTransactionOutcomeV1::Refused),
        "a frame whose predecessor has not committed is refused by the store cursor"
    );
    assert!(admitted(
        fixture.authority.commit(&frames[0], &writer).await
    ));
    let second = fixture.authority.commit(&frames[1], &writer).await;
    assert!(
        admitted(second.clone()),
        "the successor commits once its predecessor has: {second:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_under_a_fenced_writer_commits_nothing_after_the_fence_moves() {
    let fixture = fixture().await;
    let old_writer = writer(&fixture.project_id, 1);
    let frames = frames(&old_writer, 2);
    assert!(admitted(
        fixture.authority.commit(&frames[0], &old_writer).await
    ));
    let (target_binding, _) = fixture
        .authority
        .target_descriptor(&fixture.project_id)
        .expect("registered replay target");
    let authority_key = old_writer
        .authority
        .fence
        .recovery_authority_key()
        .expect("authority key");
    let replacement = RemoteWriterFenceV1 {
        placement_revision: RemotePlacementRevisionV1::new(2).expect("placement revision"),
        authority_epoch: AuthorityEpoch(2),
        ..old_writer.authority.fence.clone()
    };
    fixture
        .authority
        .install_writer_fence(
            fixture.project_id.clone(),
            RemoteWriterFenceInstallV1 {
                project_id: fixture.project_id.clone(),
                target_binding,
                authority_key: authority_key.clone(),
                expected: Some(old_writer.authority.fence.clone()),
                replacement: replacement.clone(),
                installed_at: UtcMicros(20),
            },
            uninterrupted_probe(),
        )
        .await
        .expect("promotion installs the replacement fence");

    let stale = fixture.authority.commit(&frames[1], &old_writer).await;
    assert!(
        !admitted(stale.clone()),
        "a replay under the fenced writer must not commit: {stale:?}"
    );
    assert_eq!(
        fixture
            .authority
            .current_writer_fence(fixture.project_id.clone(), authority_key)
            .await,
        Ok(Some((replacement, 1))),
        "the fence read sees the install and no frontier from the fenced replay"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replay_admitted_after_the_authority_stops_accepting_commits_nothing() {
    let fixture = fixture().await;
    let writer = writer(&fixture.project_id, 1);
    let frame = frames(&writer, 1).remove(0);
    let closing = DaemonRemoteReplayTransactionAuthorityV1::new();
    let target = fixture
        .authority
        .targets
        .read()
        .expect("target registry")
        .get(&fixture.project_id)
        .cloned()
        .expect("registered replay target");
    closing
        .targets
        .write()
        .expect("target registry")
        .insert(fixture.project_id.clone(), target);
    let accepting = Arc::clone(&closing.accepting);

    // What dropping the authority does, while a caller still holds it.
    accepting.store(false, Ordering::Release);
    assert_eq!(
        closing.commit(&frame, &writer).await,
        Err(RemoteReplayTransactionErrorV1::Unavailable),
        "a replay dispatched after shutdown is cancelled before commit"
    );
    assert!(
        admitted(fixture.authority.commit(&frame, &writer).await),
        "the cancelled replay committed nothing, so the frame is still new"
    );

    let dropped = DaemonRemoteReplayTransactionAuthorityV1::new();
    let accepting = Arc::clone(&dropped.accepting);
    drop(dropped);
    assert!(
        !accepting.load(Ordering::Acquire),
        "dropping the authority stops it accepting"
    );
}
