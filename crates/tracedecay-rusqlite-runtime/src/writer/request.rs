use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use tokio::sync::oneshot;
use tracedecay_store::{
    OperationPriorityV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1, RuntimeSubmitRequestV1,
    StorageRuntimeErrorV1, StoreClientIdV1, StoreOperationIdV1,
};

use crate::{
    RuntimeWriteAuthority, RuntimeWriteAuthorityStage, WriterActorError,
    admission::{Permit, QueueItem},
    checkpoint::{
        CheckpointBlockers, CheckpointError, CheckpointResult, MaintenanceCheckpointMode,
        RusqliteCheckpointError,
    },
    maintenance::ExclusiveMaintenancePermit,
};

pub(super) type RequestResult = Result<RuntimeSubmitOutcomeV1, StorageRuntimeErrorV1>;

struct ReplyState {
    leader: Option<oneshot::Sender<RequestResult>>,
    followers: Vec<oneshot::Sender<RequestResult>>,
    settled: Option<SettledReply>,
}

struct SettledReply {
    at: Instant,
    result: RequestResult,
}

#[derive(Clone)]
pub(super) struct SharedReply {
    inner: Arc<Mutex<ReplyState>>,
    request: Arc<RuntimeSubmitRequestV1>,
}

impl SharedReply {
    fn new(request: Arc<RuntimeSubmitRequestV1>, leader: oneshot::Sender<RequestResult>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ReplyState {
                leader: Some(leader),
                followers: Vec::new(),
                settled: None,
            })),
            request,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReplyState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn attach(&self, follower: oneshot::Sender<RequestResult>) {
        let mut state = self.lock();
        if let Some(settled) = &state.settled {
            let result = settled.result.clone();
            drop(state);
            let _ = follower.send(result);
            return;
        }
        state.followers.push(follower);
    }

    pub(super) fn attach_request(&self, follower: AcceptedRequest) {
        if let Some(sender) = follower.into_leader_sender() {
            self.attach(sender);
        }
    }

    pub(super) fn matches(&self, follower: &AcceptedRequest) -> bool {
        let leader = self.request.as_ref();
        let follower = follower.request.as_ref();
        leader.envelope().metadata.idempotency == follower.envelope().metadata.idempotency
            && leader.transaction_scope().compatibility
                == follower.transaction_scope().compatibility
            && leader.control().deadline == follower.control().deadline
            && leader.control().cancellation == follower.control().cancellation
    }

    pub(super) fn can_attach(&self, follower: &AcceptedRequest) -> bool {
        self.matches(follower)
            && self
                .lock()
                .settled
                .as_ref()
                .is_none_or(|settled| follower.enqueued_at <= settled.at)
    }

    fn take_leader(&self) -> Option<oneshot::Sender<RequestResult>> {
        self.lock().leader.take()
    }

    fn settle(&self, result: RequestResult) {
        let mut state = self.lock();
        state.settled = Some(SettledReply {
            at: Instant::now(),
            result: result.clone(),
        });
        let leader = state.leader.take();
        let followers = std::mem::take(&mut state.followers);
        drop(state);
        for follower in followers {
            let _ = follower.send(result.clone());
        }
        if let Some(leader) = leader {
            let _ = leader.send(result);
        }
    }
}

pub(super) struct AcceptedRequest {
    pub(super) request: Arc<RuntimeSubmitRequestV1>,
    pub(super) probe: Arc<dyn RuntimeRequestProbeV1>,
    pub(super) authority: Arc<dyn RuntimeWriteAuthority>,
    reply: SharedReply,
    pub(super) enqueued_at: Instant,
    _permit: Permit,
}

impl AcceptedRequest {
    pub(super) fn new(
        request: Arc<RuntimeSubmitRequestV1>,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn RuntimeWriteAuthority>,
        reply: oneshot::Sender<RequestResult>,
        permit: Permit,
    ) -> Self {
        let shared_reply = SharedReply::new(Arc::clone(&request), reply);
        Self {
            request,
            probe,
            authority,
            reply: shared_reply,
            enqueued_at: Instant::now(),
            _permit: permit,
        }
    }

    pub(super) fn shared_reply(&self) -> SharedReply {
        self.reply.clone()
    }

    pub(super) fn attach_follower(&mut self, follower: Self) {
        if let Some(sender) = follower.into_leader_sender() {
            self.reply.attach(sender);
        }
    }

    pub(super) fn matches_follower(&self, follower: &Self) -> bool {
        self.reply.matches(follower)
    }

    fn into_leader_sender(self) -> Option<oneshot::Sender<RequestResult>> {
        self.reply.take_leader()
    }

    pub(super) fn settle(self, result: RequestResult) {
        self.reply.settle(result);
        // `_permit` is dropped only after the final reply has been sent.
    }
}

impl QueueItem for AcceptedRequest {
    fn operation_id(&self) -> &StoreOperationIdV1 {
        &self.request.envelope().metadata.operation_id
    }

    fn client_id(&self) -> &StoreClientIdV1 {
        &self.request.envelope().metadata.client_id
    }

    fn priority(&self) -> OperationPriorityV1 {
        self.request.envelope().metadata.priority
    }

    fn admission_bytes(&self) -> u64 {
        self.request.envelope().metadata.admission_bytes
    }
}

pub(super) struct ExecutionBatch {
    pub(super) bytes: u64,
    pub(super) items: Vec<AcceptedRequest>,
}

pub(super) type CheckpointRequestResult =
    Result<CheckpointResult, CheckpointError<RusqliteCheckpointError>>;

pub(super) struct CheckpointCommand {
    pub(super) snapshot_blockers: CheckpointBlockers,
    pub(super) kind: CheckpointCommandKind,
    authority: Arc<dyn RuntimeWriteAuthority>,
    reply: oneshot::Sender<CheckpointRequestResult>,
}

pub(super) enum CheckpointCommandKind {
    Passive {
        probe: Arc<dyn RuntimeRequestProbeV1>,
    },
    Maintenance {
        mode: MaintenanceCheckpointMode,
        permit: Box<ExclusiveMaintenancePermit>,
    },
}

impl CheckpointCommand {
    pub(super) fn new(
        snapshot_blockers: CheckpointBlockers,
        probe: Arc<dyn RuntimeRequestProbeV1>,
        authority: Arc<dyn RuntimeWriteAuthority>,
        reply: oneshot::Sender<CheckpointRequestResult>,
    ) -> Self {
        Self {
            snapshot_blockers,
            kind: CheckpointCommandKind::Passive { probe },
            authority,
            reply,
        }
    }

    pub(super) fn new_maintenance(
        snapshot_blockers: CheckpointBlockers,
        mode: MaintenanceCheckpointMode,
        permit: ExclusiveMaintenancePermit,
        authority: Arc<dyn RuntimeWriteAuthority>,
        reply: oneshot::Sender<CheckpointRequestResult>,
    ) -> Self {
        Self {
            snapshot_blockers,
            kind: CheckpointCommandKind::Maintenance {
                mode,
                permit: Box::new(permit),
            },
            authority,
            reply,
        }
    }

    pub(super) fn verify(
        &self,
        stage: RuntimeWriteAuthorityStage,
    ) -> Result<(), CheckpointError<RusqliteCheckpointError>> {
        self.authority
            .verify(stage)
            .map_err(|_| CheckpointError::AuthorityDenied(stage))
    }

    pub(super) fn settle(self, result: CheckpointRequestResult) {
        let _ = self.reply.send(result);
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        CheckpointBlockers,
        CheckpointCommandKind,
        Arc<dyn RuntimeWriteAuthority>,
        CheckpointReply,
    ) {
        (
            self.snapshot_blockers,
            self.kind,
            self.authority,
            CheckpointReply(self.reply),
        )
    }
}

pub(super) struct CheckpointReply(oneshot::Sender<CheckpointRequestResult>);

impl CheckpointReply {
    pub(super) fn settle(self, result: CheckpointRequestResult) {
        let _ = self.0.send(result);
    }
}

pub(super) struct IncrementalVacuumCommand {
    pub(super) max_pages: u32,
    pub(super) authority: Arc<dyn RuntimeWriteAuthority>,
    reply: oneshot::Sender<Result<(), WriterActorError>>,
}

impl IncrementalVacuumCommand {
    pub(super) fn new(
        max_pages: u32,
        authority: Arc<dyn RuntimeWriteAuthority>,
        reply: oneshot::Sender<Result<(), WriterActorError>>,
    ) -> Self {
        Self {
            max_pages: max_pages.max(1),
            authority,
            reply,
        }
    }

    pub(super) fn settle(self, result: Result<(), WriterActorError>) {
        let _ = self.reply.send(result);
    }
}
