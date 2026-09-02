use std::{sync::Arc, time::Instant};

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

pub(super) struct AcceptedRequest {
    pub(super) request: Arc<RuntimeSubmitRequestV1>,
    pub(super) probe: Arc<dyn RuntimeRequestProbeV1>,
    pub(super) authority: Arc<dyn RuntimeWriteAuthority>,
    reply: oneshot::Sender<RequestResult>,
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
        Self {
            request,
            probe,
            authority,
            reply,
            enqueued_at: Instant::now(),
            _permit: permit,
        }
    }

    pub(super) fn settle(self, result: RequestResult) {
        let _ = self.reply.send(result);
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
