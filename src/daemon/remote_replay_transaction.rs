use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, mpsc};

use tracedecay_application::OperationBudgetUsage;
use tracedecay_application::remote::capture::RemoteWriterAuthorityV1;
use tracedecay_application::remote::replay::{
    RemoteReplayCommitReceiptV1, RemoteReplayFrameV1, RemoteReplayTransactionErrorV1,
    RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionPortV1,
};
use tracedecay_domain::{
    ManifestDigest, ObservationSourceCursorV1, ProjectionGenerationId, UtcMicros, canonical_sha256,
};
use tracedecay_runtime_core::db::DatabaseAuthority;
use tracedecay_runtime_core::store_runtime::registry::StoreRuntimeHandle;
use tracedecay_rusqlite_runtime::exact_sql::{ExactSqlStatement, ExactSqlValue};
use tracedecay_store::{
    AnchoredObservationWrite, CommandDigestV1, DurabilityClassV1, IdempotencyIdentityV1,
    ObservationWrite, OperationPriorityV1, ProjectId, RemoteObservationReplayWriteV1,
    RemoteWriterFenceInstallV1, RepositoryOperationEnvelopeV1, RepositoryWritePayloadV1,
    RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1, RuntimeTransactionIdV1,
    RuntimeTransactionScopeV1, StoreClientIdV1, StoreCommitReceiptV1, StoreIdempotencyKeyV1,
    StoreOperationIdV1, StoreOperationMetadataV1, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

const CHANNEL_CAPACITY: usize = 128;
const PROJECTION_GENERATION: &str = "remote-replay.v1";

#[derive(Clone)]
struct ReplayTargetV1 {
    runtime: StoreRuntimeHandle,
    authority: DatabaseAuthority,
}

enum ReplayCommandV1 {
    Replay {
        frame: Box<RemoteReplayFrameV1>,
        current_writer: RemoteWriterAuthorityV1,
        reply: mpsc::SyncSender<
            Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1>,
        >,
    },
    Snapshot {
        project_id: ProjectId,
        destination: std::path::PathBuf,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        reply: mpsc::SyncSender<Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, String>>,
    },
    InstallFence {
        project_id: ProjectId,
        install: Box<RemoteWriterFenceInstallV1>,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        reply: mpsc::SyncSender<Result<StoreCommitReceiptV1, String>>,
    },
    ReadFence {
        project_id: ProjectId,
        authority_key: ManifestDigest,
        reply:
            mpsc::SyncSender<Result<Option<(tracedecay_domain::RemoteWriterFenceV1, u64)>, String>>,
    },
}

pub(crate) struct DaemonRemoteReplayTransactionAuthorityV1 {
    targets: Arc<RwLock<BTreeMap<ProjectId, ReplayTargetV1>>>,
    accepting: Arc<AtomicBool>,
    sender: mpsc::SyncSender<ReplayCommandV1>,
}

impl DaemonRemoteReplayTransactionAuthorityV1 {
    pub(crate) fn new(runtime: tokio::runtime::Handle) -> Result<Self, String> {
        let targets = Arc::new(RwLock::new(BTreeMap::new()));
        let accepting = Arc::new(AtomicBool::new(true));
        let (sender, receiver) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let worker_targets = Arc::clone(&targets);
        let worker_accepting = Arc::clone(&accepting);
        std::thread::Builder::new()
            .name("tracedecay-remote-replay".to_owned())
            .spawn(move || {
                while let Ok(command) = receiver.recv() {
                    match command {
                        ReplayCommandV1::Replay {
                            frame,
                            current_writer,
                            reply,
                        } => {
                            let outcome = execute_replay(
                                &runtime,
                                &worker_targets,
                                &worker_accepting,
                                &frame,
                                &current_writer,
                            );
                            if reply.send(outcome).is_err() {
                                tracing::debug!(
                                    "remote replay caller ended before transaction receipt delivery"
                                );
                            }
                        }
                        ReplayCommandV1::Snapshot {
                            project_id,
                            destination,
                            probe,
                            reply,
                        } => {
                            let outcome = execute_snapshot(
                                &runtime,
                                &worker_targets,
                                &project_id,
                                destination,
                                probe,
                            );
                            if reply.send(outcome).is_err() {
                                tracing::debug!(
                                    "remote backup caller ended before snapshot receipt delivery"
                                );
                            }
                        }
                        ReplayCommandV1::InstallFence {
                            project_id,
                            install,
                            probe,
                            reply,
                        } => {
                            let outcome = execute_fence_install(
                                &runtime,
                                &worker_targets,
                                &project_id,
                                *install,
                                probe,
                            );
                            if reply.send(outcome).is_err() {
                                tracing::debug!(
                                    "remote promotion caller ended before fence receipt delivery"
                                );
                            }
                        }
                        ReplayCommandV1::ReadFence {
                            project_id,
                            authority_key,
                            reply,
                        } => {
                            let outcome =
                                read_writer_fence(&worker_targets, &project_id, &authority_key);
                            if reply.send(outcome).is_err() {
                                tracing::debug!(
                                    "remote recovery caller ended before fence read delivery"
                                );
                            }
                        }
                    }
                }
            })
            .map_err(|error| format!("spawn remote replay transaction authority: {error}"))?;
        Ok(Self {
            targets,
            accepting,
            sender,
        })
    }

    pub(crate) fn register_target(
        &self,
        project_id: ProjectId,
        runtime: StoreRuntimeHandle,
        authority: DatabaseAuthority,
    ) -> Result<(), String> {
        let mut targets = self
            .targets
            .write()
            .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?;
        if let Some(existing) = targets.get(&project_id)
            && (existing.runtime.binding() != runtime.binding()
                || existing.authority.canonical_database_path()
                    != authority.canonical_database_path())
        {
            return Err(
                "remote replay target registration conflicts with mounted authority".into(),
            );
        }
        targets.insert(project_id, ReplayTargetV1 { runtime, authority });
        Ok(())
    }

    pub(crate) fn unregister_target(
        &self,
        project_id: &ProjectId,
        expected_binding: &tracedecay_store::StoreRuntimeBindingV1,
    ) -> Result<(), String> {
        let mut targets = self
            .targets
            .write()
            .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?;
        let target = targets
            .get(project_id)
            .ok_or_else(|| "remote replay target is not registered".to_owned())?;
        if target.runtime.binding() != expected_binding {
            return Err("remote replay target binding changed before retirement".to_owned());
        }
        targets.remove(project_id);
        Ok(())
    }

    pub(crate) fn target_descriptor(
        &self,
        project_id: &ProjectId,
    ) -> Result<(tracedecay_store::StoreRuntimeBindingV1, std::path::PathBuf), String> {
        let targets = self
            .targets
            .read()
            .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?;
        let target = targets
            .get(project_id)
            .ok_or_else(|| "remote replay target is not registered".to_owned())?;
        Ok((
            target.runtime.binding().clone(),
            target.authority.canonical_database_path().to_path_buf(),
        ))
    }

    pub(crate) fn snapshot_target(
        &self,
        project_id: ProjectId,
        destination: std::path::PathBuf,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, String> {
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .try_send(ReplayCommandV1::Snapshot {
                project_id,
                destination,
                probe,
                reply,
            })
            .map_err(|_| "remote backup worker is saturated".to_owned())?;
        response
            .recv()
            .map_err(|_| "remote backup worker ended before replying".to_owned())?
    }

    pub(crate) fn install_writer_fence(
        &self,
        project_id: ProjectId,
        install: RemoteWriterFenceInstallV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> Result<StoreCommitReceiptV1, String> {
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .try_send(ReplayCommandV1::InstallFence {
                project_id,
                install: Box::new(install),
                probe,
                reply,
            })
            .map_err(|_| "remote promotion worker is saturated".to_owned())?;
        response
            .recv()
            .map_err(|_| "remote promotion worker ended before replying".to_owned())?
    }

    pub(crate) fn current_writer_fence(
        &self,
        project_id: ProjectId,
        authority_key: ManifestDigest,
    ) -> Result<Option<(tracedecay_domain::RemoteWriterFenceV1, u64)>, String> {
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .try_send(ReplayCommandV1::ReadFence {
                project_id,
                authority_key,
                reply,
            })
            .map_err(|_| "remote recovery fence reader is saturated".to_owned())?;
        response
            .recv()
            .map_err(|_| "remote recovery fence reader ended before replying".to_owned())?
    }

    /// Returns the already-published canonical project runtime used by replay,
    /// backup, restore, and remote reads. Query callers receive no locator or
    /// physical attachment and cannot open a caller-selected store.
    pub(crate) fn registered_query_target(
        &self,
        project_id: &ProjectId,
    ) -> Result<StoreRuntimeHandle, String> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err("remote replay target registry is unavailable".to_owned());
        }
        self.targets
            .read()
            .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?
            .get(project_id)
            .map(|target| target.runtime.clone())
            .ok_or_else(|| "remote query target is not registered".to_owned())
    }
}

impl RemoteReplayTransactionPortV1 for DaemonRemoteReplayTransactionAuthorityV1 {
    fn commit(
        &self,
        frame: &RemoteReplayFrameV1,
        current_writer: &RemoteWriterAuthorityV1,
    ) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RemoteReplayTransactionErrorV1::Unavailable);
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .try_send(ReplayCommandV1::Replay {
                frame: Box::new(frame.clone()),
                current_writer: current_writer.clone(),
                reply,
            })
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
        response
            .recv()
            .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?
    }
}

impl Drop for DaemonRemoteReplayTransactionAuthorityV1 {
    fn drop(&mut self) {
        self.accepting.store(false, Ordering::Release);
    }
}

fn execute_snapshot(
    tokio_runtime: &tokio::runtime::Handle,
    targets: &RwLock<BTreeMap<ProjectId, ReplayTargetV1>>,
    project_id: &ProjectId,
    destination: std::path::PathBuf,
    probe: Arc<dyn RuntimeRequestProbeV1>,
) -> Result<tracedecay_rusqlite_runtime::OnlineBackupReceipt, String> {
    let target = targets
        .read()
        .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?
        .get(project_id)
        .cloned()
        .ok_or_else(|| "remote backup target is not registered".to_owned())?;
    tokio_runtime
        .block_on(
            target
                .runtime
                .snapshot_to_interruptible(destination, probe, target.authority),
        )
        .map_err(|error| format!("registered online backup failed: {error:?}"))
}

fn execute_fence_install(
    tokio_runtime: &tokio::runtime::Handle,
    targets: &RwLock<BTreeMap<ProjectId, ReplayTargetV1>>,
    project_id: &ProjectId,
    install: RemoteWriterFenceInstallV1,
    interruption: Arc<dyn RuntimeRequestProbeV1>,
) -> Result<StoreCommitReceiptV1, String> {
    let target = targets
        .read()
        .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?
        .get(project_id)
        .cloned()
        .ok_or_else(|| "remote promotion target is not registered".to_owned())?;
    if target.runtime.binding() != &install.target_binding {
        return Err("remote promotion target binding changed".to_owned());
    }
    let request = prepare_fence_request(&target, install)?;
    let probe = Arc::new(ForwardedReplayProbeV1 {
        cancellation: request.control().cancellation.clone(),
        deadline: request.control().deadline.clone(),
        interruption,
        commit_started: AtomicBool::new(false),
    });
    let outcome = tokio_runtime
        .block_on(
            target
                .runtime
                .dispatch_submit_authorized(request, probe, target.authority),
        )
        .map_err(|error| format!("registered remote fence dispatch failed: {error:?}"))?;
    match outcome {
        RuntimeSubmitOutcomeV1::Committed { receipt }
        | RuntimeSubmitOutcomeV1::ExactReplay { receipt }
        | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { receipt, .. } => Ok(receipt),
        RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
            Err("remote fence installation idempotency conflict".to_owned())
        }
        RuntimeSubmitOutcomeV1::Fenced { .. } => {
            Err("remote fence installation target was fenced".to_owned())
        }
        RuntimeSubmitOutcomeV1::Saturated { .. } => {
            Err("remote fence installation target is saturated".to_owned())
        }
        RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit { .. } => {
            Err("remote fence installation timed out before commit".to_owned())
        }
        RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. } => {
            Err("remote fence installation was cancelled before commit".to_owned())
        }
        RuntimeSubmitOutcomeV1::Unavailable { .. } => {
            Err("remote fence installation target is unavailable".to_owned())
        }
    }
}

fn read_writer_fence(
    targets: &RwLock<BTreeMap<ProjectId, ReplayTargetV1>>,
    project_id: &ProjectId,
    authority_key: &ManifestDigest,
) -> Result<Option<(tracedecay_domain::RemoteWriterFenceV1, u64)>, String> {
    let target = targets
        .read()
        .map_err(|_| "remote replay target registry lock is poisoned".to_owned())?
        .get(project_id)
        .cloned()
        .ok_or_else(|| "remote recovery target is not registered".to_owned())?;
    let handle = target
        .runtime
        .authorized_exact_sql_handle(target.authority)
        .map_err(|error| format!("authorize remote recovery fence read: {error:?}"))?;
    let rows = handle
        .query(
            ExactSqlStatement::new(
                "SELECT writer_fence_json, frontier_sequence
                 FROM remote_writer_fences WHERE authority_key = ?1"
                    .to_owned(),
                vec![ExactSqlValue::Text(authority_key.as_str().to_owned())],
            )
            .map_err(|error| format!("prepare remote recovery fence read: {error}"))?,
            std::time::Duration::from_secs(5),
        )
        .map_err(|error| format!("read remote recovery fence: {error}"))?;
    let mut rows = rows.rows.into_iter();
    let Some(row) = rows.next() else {
        return Ok(None);
    };
    if rows.next().is_some() {
        return Err("remote recovery fence authority is not unique".to_owned());
    }
    let [
        ExactSqlValue::Text(encoded),
        ExactSqlValue::Integer(frontier),
    ] = row.values.as_slice()
    else {
        return Err("remote recovery fence row is malformed".to_owned());
    };
    let frontier = u64::try_from(*frontier)
        .map_err(|_| "remote recovery fence frontier is invalid".to_owned())?;
    let fence = serde_json::from_str(encoded)
        .map_err(|error| format!("decode remote recovery fence: {error}"))?;
    Ok(Some((fence, frontier)))
}

fn prepare_fence_request(
    target: &ReplayTargetV1,
    install: RemoteWriterFenceInstallV1,
) -> Result<RuntimeSubmitRequestV1, String> {
    install
        .validate()
        .map_err(|error| format!("invalid remote fence installation: {error}"))?;
    let command = serde_json::json!({
        "kind": "remote_writer_fence_install",
        "authority_key": &install.authority_key,
        "project_id": &install.project_id,
        "target_binding": &install.target_binding,
        "expected": &install.expected,
        "replacement": &install.replacement,
        "installed_at": install.installed_at,
    });
    let command_digest = canonical_sha256(&command)
        .map_err(|error| format!("derive remote fence command digest: {error}"))?;
    let digest_suffix = command_digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| "remote fence command digest prefix is invalid".to_owned())?;
    let admitted_at = install.installed_at;
    let admission_bytes = u64::try_from(
        serde_json::to_vec(&command)
            .map_err(|error| format!("encode remote fence command: {error}"))?
            .len(),
    )
    .map_err(|_| "remote fence command size exceeds u64".to_owned())?
    .max(1);
    let binding = target.runtime.binding();
    let metadata = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new(format!("operation.remote-fence.{digest_suffix}"))
            .map_err(|error| error.to_string())?,
        client_id: StoreClientIdV1::new("client.remote-recovery")
            .map_err(|error| error.to_string())?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: IdempotencyIdentityV1 {
            key: StoreIdempotencyKeyV1::new(format!("remote-fence.{digest_suffix}"))
                .map_err(|error| error.to_string())?,
            command_digest: CommandDigestV1::new(command_digest.as_str())
                .map_err(|error| error.to_string())?,
        },
        durability: DurabilityClassV1::Full,
        priority: OperationPriorityV1::Foreground,
        admission_bytes,
        admitted_at,
    };
    let compatibility = RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(|error| error.to_string())?;
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(|error| error.to_string())?,
        compatibility,
        opened_at: admitted_at,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{digest_suffix}"))
            .map_err(|error| error.to_string())?,
    };
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{digest_suffix}"))
            .map_err(|error| error.to_string())?,
        generation: 1,
    };
    RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 {
            metadata,
            payload: RepositoryWritePayloadV1::RemoteWriterFenceInstall(Box::new(install)),
        },
        transaction_scope,
        RuntimeRequestControlV1 {
            requested_at: admitted_at,
            deadline,
            cancellation,
        },
    )
    .map_err(|error| error.to_string())
}

fn execute_replay(
    tokio_runtime: &tokio::runtime::Handle,
    targets: &RwLock<BTreeMap<ProjectId, ReplayTargetV1>>,
    accepting: &Arc<AtomicBool>,
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
    if frame.capture.writer.project_id != current_writer.project_id
        || frame.capture.writer.scope != current_writer.scope
        || !(current_writer.authority.fence == frame.capture.writer.authority.fence
            || current_writer
                .authority
                .fence
                .fences(&frame.capture.writer.authority.fence))
    {
        return Err(RemoteReplayTransactionErrorV1::FenceMismatch);
    }
    let target = targets
        .read()
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?
        .get(&current_writer.project_id)
        .cloned()
        .ok_or(RemoteReplayTransactionErrorV1::Unavailable)?;
    let prepared = prepare_request(frame, current_writer, &target)?;
    let request = prepared.request;
    let probe = Arc::new(ReplayRequestProbeV1 {
        cancellation: request.control().cancellation.clone(),
        deadline: request.control().deadline.clone(),
        accepting: Arc::clone(accepting),
        commit_started: AtomicBool::new(false),
    });
    let outcome = tokio_runtime
        .block_on(
            target
                .runtime
                .dispatch_submit_authorized(request, probe, target.authority),
        )
        .map_err(|_| RemoteReplayTransactionErrorV1::Unavailable)?;
    map_submit_outcome(frame, current_writer, prepared.admission_bytes, outcome)
}

struct PreparedReplayRequestV1 {
    request: RuntimeSubmitRequestV1,
    admission_bytes: u64,
}

fn prepare_request(
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
    target: &ReplayTargetV1,
) -> Result<PreparedReplayRequestV1, RemoteReplayTransactionErrorV1> {
    let observation = frame.capture.observation.clone();
    let identity = observation.identity();
    let position = identity.position();
    let expected_cursor = (position.start() > 0)
        .then(|| {
            ObservationSourceCursorV1::for_ordering(
                identity.source().clone(),
                identity.scope().clone(),
                identity.generation(),
                identity.ordering_domain(),
                position.start(),
            )
        })
        .transpose()
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        identity.source().clone(),
        identity.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        position.end(),
    )
    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let projection_generation = ProjectionGenerationId::new(PROJECTION_GENERATION)
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let committed_at = tracedecay_application::clock::now_micros();
    let authorization =
        build_observation_resolution_authorization_v1(&observation, PROJECTION_GENERATION)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let anchor = build_observation_retrieval_anchor_v2(
        &observation,
        projection_generation.clone(),
        committed_at,
        authorization,
    )
    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let anchored = AnchoredObservationWrite::new(
        ObservationWrite::new(observation, expected_cursor, next_cursor)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        anchor,
        projection_generation,
    )
    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let authority_key = canonical_sha256(&(
        "tracedecay.remote-recovery-authority.v1",
        &current_writer.authority.fence.brain_id,
        &current_writer.authority.fence.shard_id,
        &current_writer.authority.fence.generation_id,
    ))
    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let frame_digest = canonical_sha256(&(&frame.event_id, &frame.capture))
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let command_value = serde_json::json!({
        "kind": "remote_observation_replay",
        "event_id": frame.event_id,
        "frame_digest": frame_digest,
        "capture": frame.capture,
        "writer_fence": current_writer.authority.fence,
        "target_binding": target.runtime.binding(),
    });
    let command_digest = canonical_sha256(&command_value)
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let admission_bytes = u64::try_from(
        serde_json::to_vec(&command_value)
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?
            .len(),
    )
    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?
    .max(1);
    let digest_suffix = digest_suffix(&command_digest)?;
    let payload = RepositoryWritePayloadV1::RemoteObservationReplay(Box::new(
        RemoteObservationReplayWriteV1 {
            event_id: frame.event_id.clone(),
            authority_key,
            frame_digest,
            enrollment_id: frame.capture.enrollment_id.clone(),
            enrollment_revision: frame.capture.enrollment_revision,
            node_id: frame.capture.node_id.clone(),
            policy_revision: frame.capture.policy_revision,
            capture_sequence: frame.capture.sequence.sequence,
            previous_event_id: frame.capture.sequence.previous_event_id.clone(),
            project_id: current_writer.project_id.clone(),
            writer_fence: current_writer.authority.fence.clone(),
            captured_at: frame.capture.captured_at,
            command_digest: command_digest.clone(),
            observation: anchored,
        },
    ));
    let binding = target.runtime.binding();
    let metadata = StoreOperationMetadataV1 {
        operation_id: StoreOperationIdV1::new(format!("operation.remote-replay.{digest_suffix}"))
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        client_id: StoreClientIdV1::new("client.remote-replay")
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        shard_id: binding.shard_id.clone(),
        incarnation: binding.incarnation,
        authority_epoch: binding.authority_epoch,
        idempotency: IdempotencyIdentityV1 {
            key: StoreIdempotencyKeyV1::new(format!("remote-replay.{}", frame.event_id))
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
            command_digest: CommandDigestV1::new(command_digest.as_str())
                .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        },
        durability: DurabilityClassV1::Full,
        priority: OperationPriorityV1::Foreground,
        admission_bytes,
        admitted_at: committed_at,
    };
    let compatibility = RuntimeBatchCompatibilityV1::from_operation(&metadata)
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    let transaction_scope = RuntimeTransactionScopeV1 {
        transaction_id: RuntimeTransactionIdV1::new(format!(
            "transaction.{}",
            metadata.operation_id.as_str()
        ))
        .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        compatibility,
        opened_at: committed_at,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("deadline.{digest_suffix}"))
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
    };
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("cancellation.{digest_suffix}"))
            .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?,
        generation: 1,
    };
    let control = RuntimeRequestControlV1 {
        requested_at: committed_at,
        deadline,
        cancellation,
    };
    let request = RuntimeSubmitRequestV1::new(
        RepositoryOperationEnvelopeV1 { metadata, payload },
        transaction_scope,
        control,
    )
    .map_err(|_| RemoteReplayTransactionErrorV1::CanonicalEffect)?;
    Ok(PreparedReplayRequestV1 {
        request,
        admission_bytes,
    })
}

fn digest_suffix(digest: &ManifestDigest) -> Result<&str, RemoteReplayTransactionErrorV1> {
    digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(RemoteReplayTransactionErrorV1::CanonicalEffect)
}

fn map_submit_outcome(
    frame: &RemoteReplayFrameV1,
    current_writer: &RemoteWriterAuthorityV1,
    admission_bytes: u64,
    outcome: RuntimeSubmitOutcomeV1,
) -> Result<RemoteReplayTransactionOutcomeV1, RemoteReplayTransactionErrorV1> {
    let (receipt, duplicate) = match outcome {
        RuntimeSubmitOutcomeV1::Committed { receipt }
        | RuntimeSubmitOutcomeV1::CommittedAfterCancellation { receipt, .. } => (receipt, false),
        RuntimeSubmitOutcomeV1::ExactReplay { receipt } => (receipt, true),
        RuntimeSubmitOutcomeV1::IdempotencyConflict { .. } => {
            return Err(RemoteReplayTransactionErrorV1::IdempotencyConflict);
        }
        RuntimeSubmitOutcomeV1::Fenced { .. } => {
            return Err(RemoteReplayTransactionErrorV1::FenceMismatch);
        }
        RuntimeSubmitOutcomeV1::Saturated { .. }
        | RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit { .. }
        | RuntimeSubmitOutcomeV1::CancelledBeforeCommit { .. }
        | RuntimeSubmitOutcomeV1::Unavailable { .. } => {
            return Err(RemoteReplayTransactionErrorV1::Unavailable);
        }
    };
    let transaction_receipt = RemoteReplayCommitReceiptV1 {
        event_id: frame.event_id.clone(),
        writer_fence: current_writer.authority.fence.clone(),
        commit_sequence: receipt.commit_sequence.0,
        committed_at: receipt.committed_at,
        budget: OperationBudgetUsage {
            units_consumed: 1,
            bytes_consumed: admission_bytes,
            elapsed_micros: elapsed_micros(frame.capture.captured_at, receipt.committed_at),
        },
    };
    if duplicate {
        Ok(RemoteReplayTransactionOutcomeV1::Duplicate(
            transaction_receipt,
        ))
    } else {
        Ok(RemoteReplayTransactionOutcomeV1::Admitted(
            transaction_receipt,
        ))
    }
}

fn elapsed_micros(started_at: UtcMicros, committed_at: UtcMicros) -> u64 {
    match committed_at.0.checked_sub(started_at.0) {
        Some(elapsed) if elapsed >= 0 => elapsed as u64,
        _ => 0,
    }
}

struct ReplayRequestProbeV1 {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    accepting: Arc<AtomicBool>,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for ReplayRequestProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        (!self.accepting.load(Ordering::Acquire)).then_some(RuntimeInterruptionV1::Cancelled)
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}

struct ForwardedReplayProbeV1 {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
    interruption: Arc<dyn RuntimeRequestProbeV1>,
    commit_started: AtomicBool,
}

impl RuntimeRequestProbeV1 for ForwardedReplayProbeV1 {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        self.interruption.interruption()
    }

    fn try_begin_commit(&self) -> bool {
        self.interruption().is_none()
            && self
                .commit_started
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }
}
