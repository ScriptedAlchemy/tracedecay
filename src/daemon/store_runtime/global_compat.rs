use tracedecay_store::{
    ConsistencyModeV1, RuntimeCancellationStageV1, RuntimeInterruptionV1, RuntimeReadCoverageV1,
    RuntimeReadOutcomeV1, RuntimeReadRequestV1, RuntimeRequestProbeV1, RuntimeSubmitOutcomeV1,
    RuntimeSubmitRequestV1, SessionRefreshProgressRequestV1, SessionRefreshProgressV1,
    SessionRefreshStore, SessionStoreResult, StorageRuntimeContractErrorV1,
    StorageRuntimePortErrorV1, StorageRuntimePortFutureV1, StorageRuntimePortResultV1,
    StorageRuntimeReadPort, StorageRuntimeSubmitPort, StoreRuntimeBindingV1, UnavailableReasonV1,
};

use crate::global_db::{GlobalDb, ProjectRegistryContext};
use crate::store::sqlite_runtime::GlobalDbRuntime;

/// S1 runtime-port bridge over an already-open authoritative [`GlobalDb`].
///
/// The bridge borrows the existing `GlobalDb` facade; it never resolves a
/// locator, opens a database, obtains a raw connection, or owns a second
/// writer. S1 has no atomic runtime receipt/idempotency ledger and no canonical
/// commit-sequence authority. This bridge therefore exposes the existing
/// session/profile read facades but reports every runtime write and bounded
/// watermark claim as unavailable instead of fabricating durable evidence.
pub(crate) struct GlobalDbRuntimeCompat<'db> {
    runtime: GlobalDbRuntime<'db>,
    binding: StoreRuntimeBindingV1,
}

impl<'db> GlobalDbRuntimeCompat<'db> {
    pub(crate) fn new(db: &'db GlobalDb, binding: StoreRuntimeBindingV1) -> Self {
        Self {
            runtime: GlobalDbRuntime::new(db),
            binding,
        }
    }

    /// Profile-registry lookup through the existing borrowed `GlobalDb` adapter.
    ///
    /// This is intentionally separate from the runtime read envelope: S1's
    /// closed read variants expose runtime/consistency state, not registry
    /// lookup arguments.
    pub(crate) async fn project_registry_context_by_id(
        &self,
        project_id: &str,
    ) -> Option<ProjectRegistryContext> {
        self.runtime
            .project_registry()
            .project_registry_context_by_id(project_id)
            .await
    }

    /// Representative session read through the existing typed facade. Runtime
    /// compatibility writes remain unavailable until their receipts can be
    /// persisted atomically with the underlying transaction.
    pub(crate) async fn session_refresh_progress(
        &self,
        request: SessionRefreshProgressRequestV1,
    ) -> SessionStoreResult<Option<SessionRefreshProgressV1>> {
        self.runtime
            .session_store()
            .session_refresh_progress(request)
            .await
    }

    fn validate_probe(
        control: &tracedecay_store::RuntimeRequestControlV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> Result<(), StorageRuntimeContractErrorV1> {
        if probe.cancellation_identity() != &control.cancellation {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "runtime cancellation probe identity",
            });
        }
        if probe.deadline_identity() != &control.deadline {
            return Err(StorageRuntimeContractErrorV1::ReceiptBindingMismatch {
                field: "runtime deadline probe identity",
            });
        }
        Ok(())
    }

    fn validate_submit_dispatch(
        request: &RuntimeSubmitRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortResultV1<()> {
        request
            .validate()
            .and_then(|()| Self::validate_probe(request.control(), probe))
            .map_err(StorageRuntimePortErrorV1::InvalidRequest)
    }

    fn validate_read_dispatch(
        request: &RuntimeReadRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortResultV1<()> {
        request
            .validate()
            .and_then(|()| Self::validate_probe(request.control(), probe))
            .map_err(StorageRuntimePortErrorV1::InvalidRequest)
    }

    fn binding_unavailable_reason(
        &self,
        requested: &StoreRuntimeBindingV1,
    ) -> Option<UnavailableReasonV1> {
        if requested.shard_id != self.binding.shard_id {
            return Some(UnavailableReasonV1::MissingAuthority);
        }
        if requested.incarnation != self.binding.incarnation {
            return Some(UnavailableReasonV1::WrongIncarnation);
        }
        if requested.authority_epoch != self.binding.authority_epoch {
            return Some(UnavailableReasonV1::WrongAuthorityEpoch);
        }
        None
    }

    fn submit_binding_outcome(
        &self,
        requested: &StoreRuntimeBindingV1,
    ) -> Option<RuntimeSubmitOutcomeV1> {
        match self.binding_unavailable_reason(requested) {
            Some(UnavailableReasonV1::WrongAuthorityEpoch) => {
                Some(RuntimeSubmitOutcomeV1::Fenced {
                    expected: requested.authority_epoch,
                    actual: self.binding.authority_epoch,
                })
            }
            Some(reason) => Some(RuntimeSubmitOutcomeV1::Unavailable { reason }),
            None => None,
        }
    }

    fn submit_interruption(
        request: &RuntimeSubmitRequestV1,
        probe: &dyn RuntimeRequestProbeV1,
        stage: RuntimeCancellationStageV1,
    ) -> Option<RuntimeSubmitOutcomeV1> {
        match probe.interruption()? {
            RuntimeInterruptionV1::Cancelled => {
                Some(RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
                    cancellation: request.control().cancellation.clone(),
                    stage,
                })
            }
            RuntimeInterruptionV1::DeadlineExceeded => {
                Some(RuntimeSubmitOutcomeV1::DeadlineExceededBeforeCommit {
                    deadline: request.control().deadline.clone(),
                })
            }
        }
    }

    fn read_interruption(probe: &dyn RuntimeRequestProbeV1) -> Option<UnavailableReasonV1> {
        match probe.interruption()? {
            RuntimeInterruptionV1::Cancelled => Some(UnavailableReasonV1::Cancelled),
            RuntimeInterruptionV1::DeadlineExceeded => Some(UnavailableReasonV1::DeadlineExceeded),
        }
    }

    fn unavailable_read(
        reason: UnavailableReasonV1,
    ) -> StorageRuntimePortResultV1<RuntimeReadOutcomeV1> {
        RuntimeReadOutcomeV1::new(
            None,
            RuntimeReadCoverageV1::Unavailable {
                coverage: None,
                reason,
            },
        )
        .map_err(StorageRuntimePortErrorV1::InvalidResponse)
    }
}

impl StorageRuntimeSubmitPort for GlobalDbRuntimeCompat<'_> {
    fn dispatch_submit<'a>(
        &'a self,
        request: RuntimeSubmitRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeSubmitOutcomeV1> {
        Box::pin(async move {
            Self::validate_submit_dispatch(&request, probe)?;
            if let Some(outcome) = self.submit_binding_outcome(request.binding()) {
                return Ok(outcome);
            }
            if let Some(outcome) =
                Self::submit_interruption(&request, probe, RuntimeCancellationStageV1::BeforeCommit)
            {
                return Ok(outcome);
            }
            Ok(RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::UnsupportedOperation,
            })
        })
    }
}

impl StorageRuntimeReadPort for GlobalDbRuntimeCompat<'_> {
    fn dispatch_read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        probe: &'a dyn RuntimeRequestProbeV1,
    ) -> StorageRuntimePortFutureV1<'a, RuntimeReadOutcomeV1> {
        Box::pin(async move {
            Self::validate_read_dispatch(&request, probe)?;
            if let Some(reason) = self.binding_unavailable_reason(request.binding()) {
                return Self::unavailable_read(reason);
            }
            if let Some(reason) = Self::read_interruption(probe) {
                return Self::unavailable_read(reason);
            }
            let reason = match request.consistency() {
                ConsistencyModeV1::LatestAvailable => UnavailableReasonV1::UnsupportedOperation,
                ConsistencyModeV1::AtLeast { .. } => UnavailableReasonV1::WatermarkNotReached,
                ConsistencyModeV1::ExactSnapshot { .. } => UnavailableReasonV1::SnapshotNotRetained,
                ConsistencyModeV1::FrozenWatermarkVector { .. } => {
                    UnavailableReasonV1::MissingAuthority
                }
            };
            Self::unavailable_read(reason)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use tempfile::TempDir;
    use tracedecay_domain::{BrainId, ProjectId, SessionId, UserProfileId, UtcMicros};
    use tracedecay_store::{
        CommandDigestV1, CommitSequenceV1, DurabilityClassV1, FrozenWatermarkVectorV1,
        IdempotencyIdentityV1, OperationPriorityV1, RepositoryOperationEnvelopeV1,
        RepositoryWritePayloadV1, RuntimeBatchCompatibilityV1, RuntimeCancellationIdV1,
        RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeReadOperationV1, RuntimeReadRequestV1,
        RuntimeRequestControlV1, RuntimeTransactionIdV1, RuntimeTransactionScopeV1,
        SessionRefreshBeginOrJoinRequestV1, SessionRefreshFrontierV1,
        SessionRefreshProgressRequestV1, SessionTemporalProjectionBatchV1, ShardWatermarkV1,
        StoreAuthorityEpochV1, StoreClientIdV1, StoreIdempotencyKeyV1, StoreIncarnationV1,
        StoreOperationIdV1, StoreOperationMetadataV1, StoreShardIdV1,
    };

    use crate::global_db::GlobalDb;

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).expect("canonical test identity")
    }

    fn binding() -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project_sessions(
                id::<BrainId>("brain.global-compat"),
                id::<UserProfileId>("profile.global-compat"),
                id::<ProjectId>("project.global-compat"),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(7).unwrap(),
        )
    }

    fn control() -> RuntimeRequestControlV1 {
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(1),
            deadline: RuntimeDeadlineV1 {
                deadline_id: RuntimeDeadlineIdV1::new("deadline.global-compat").unwrap(),
            },
            cancellation: tracedecay_store::RuntimeCancellationIdentityV1 {
                cancellation_id: RuntimeCancellationIdV1::new("cancel.global-compat").unwrap(),
                generation: 1,
            },
        }
    }

    struct Probe {
        control: RuntimeRequestControlV1,
        interruption: Option<RuntimeInterruptionV1>,
    }

    impl Probe {
        fn new(control: &RuntimeRequestControlV1) -> Self {
            Self {
                control: control.clone(),
                interruption: None,
            }
        }
    }

    impl RuntimeRequestProbeV1 for Probe {
        fn cancellation_identity(&self) -> &tracedecay_store::RuntimeCancellationIdentityV1 {
            &self.control.cancellation
        }

        fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
            &self.control.deadline
        }

        fn interruption(&self) -> Option<RuntimeInterruptionV1> {
            self.interruption
        }
    }

    fn submit_request(
        binding: StoreRuntimeBindingV1,
        payload: RepositoryWritePayloadV1,
        operation_id: &str,
    ) -> RuntimeSubmitRequestV1 {
        let metadata = StoreOperationMetadataV1 {
            operation_id: StoreOperationIdV1::new(operation_id).unwrap(),
            client_id: StoreClientIdV1::new("client.global-compat").unwrap(),
            shard_id: binding.shard_id.clone(),
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            idempotency: IdempotencyIdentityV1 {
                key: StoreIdempotencyKeyV1::new("idempotency.global-compat").unwrap(),
                command_digest: CommandDigestV1::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            },
            durability: DurabilityClassV1::Full,
            priority: OperationPriorityV1::Foreground,
            admission_bytes: 128,
            admitted_at: UtcMicros(1),
        };
        RuntimeSubmitRequestV1::new(
            RepositoryOperationEnvelopeV1 {
                metadata: metadata.clone(),
                payload,
            },
            RuntimeTransactionScopeV1 {
                transaction_id: RuntimeTransactionIdV1::new(format!("transaction.{operation_id}"))
                    .unwrap(),
                compatibility: RuntimeBatchCompatibilityV1::from_operation(&metadata).unwrap(),
                opened_at: UtcMicros(1),
            },
            control(),
        )
        .unwrap()
    }

    async fn projection_batch(db: &GlobalDb) -> SessionTemporalProjectionBatchV1 {
        let session_id = SessionId::new("session.global-compat").unwrap();
        db.begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
            session_id.clone(),
            SessionRefreshFrontierV1::new(1, 0).unwrap(),
        ))
        .await
        .unwrap();
        let recovery = db
            .session_refresh_recovery_result(&session_id)
            .await
            .unwrap()
            .expect("new refresh has recovery state");
        SessionTemporalProjectionBatchV1::new(
            session_id,
            recovery.candidate_generation(),
            recovery.frozen_watermarks().clone(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
        .with_checkpoint(0, 1, 1)
        .unwrap()
    }

    #[tokio::test]
    async fn runtime_writes_and_watermarks_are_honestly_unavailable() {
        let temporary = TempDir::new().unwrap();
        let db = GlobalDb::open_at(&temporary.path().join("global.db"))
            .await
            .expect("open temporary GlobalDb authority");
        let binding = binding();
        let runtime = GlobalDbRuntimeCompat::new(&db, binding.clone());
        let batch = projection_batch(&db).await;
        let request = submit_request(
            binding.clone(),
            RepositoryWritePayloadV1::SessionProjection(Box::new(batch.clone())),
            "operation.global-compat.first",
        );
        let probe = Probe::new(request.control());

        assert!(matches!(
            runtime.submit(request, &probe).await.unwrap(),
            RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::UnsupportedOperation
            }
        ));

        let required = ShardWatermarkV1 {
            shard_id: binding.shard_id.clone(),
            incarnation: binding.incarnation,
            authority_epoch: binding.authority_epoch,
            commit_sequence: CommitSequenceV1(1),
        };
        let read = RuntimeReadRequestV1::new(
            binding,
            ConsistencyModeV1::FrozenWatermarkVector {
                vector: FrozenWatermarkVectorV1::new([required]).unwrap(),
            },
            RuntimeReadOperationV1::FrozenCoverage,
            OperationPriorityV1::Foreground,
            64,
            control(),
        )
        .unwrap();
        let read_probe = Probe::new(read.control());
        let outcome = runtime.read(read, &read_probe).await.unwrap();
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                coverage: None,
                reason: UnavailableReasonV1::MissingAuthority,
            }
        ));
        assert!(outcome.value().is_none());
    }

    #[tokio::test]
    async fn interruption_and_fencing_use_typed_decision_channels() {
        let temporary = TempDir::new().unwrap();
        let db = GlobalDb::open_at(&temporary.path().join("global.db"))
            .await
            .expect("open temporary GlobalDb authority");
        let authoritative = binding();
        let runtime = GlobalDbRuntimeCompat::new(&db, authoritative.clone());
        let batch = projection_batch(&db).await;

        let cancelled_request = submit_request(
            authoritative.clone(),
            RepositoryWritePayloadV1::SessionProjection(Box::new(batch.clone())),
            "operation.global-compat.cancelled",
        );
        let mut cancelled = Probe::new(cancelled_request.control());
        cancelled.interruption = Some(RuntimeInterruptionV1::Cancelled);
        assert!(matches!(
            runtime.submit(cancelled_request, &cancelled).await.unwrap(),
            RuntimeSubmitOutcomeV1::CancelledBeforeCommit {
                stage: RuntimeCancellationStageV1::BeforeAdmission,
                ..
            }
        ));

        let read = RuntimeReadRequestV1::new(
            authoritative.clone(),
            ConsistencyModeV1::LatestAvailable,
            RuntimeReadOperationV1::CurrentWatermark,
            OperationPriorityV1::Foreground,
            64,
            control(),
        )
        .unwrap();
        let mut deadline = Probe::new(read.control());
        deadline.interruption = Some(RuntimeInterruptionV1::DeadlineExceeded);
        let outcome = runtime.read(read, &deadline).await.unwrap();
        assert!(matches!(
            outcome.coverage(),
            RuntimeReadCoverageV1::Unavailable {
                coverage: None,
                reason: UnavailableReasonV1::DeadlineExceeded,
            }
        ));

        let mut stale_binding = authoritative;
        stale_binding.authority_epoch = StoreAuthorityEpochV1::new(8).unwrap();
        let fenced_request = submit_request(
            stale_binding,
            RepositoryWritePayloadV1::SessionProjection(Box::new(batch)),
            "operation.global-compat.fenced",
        );
        let probe = Probe::new(fenced_request.control());
        assert!(matches!(
            runtime.submit(fenced_request, &probe).await.unwrap(),
            RuntimeSubmitOutcomeV1::Fenced { expected, actual }
                if expected == StoreAuthorityEpochV1::new(8).unwrap()
                    && actual == StoreAuthorityEpochV1::new(7).unwrap()
        ));
    }

    #[tokio::test]
    async fn profile_registry_delegates_through_the_borrowed_globaldb_facade() {
        let temporary = TempDir::new().unwrap();
        let project_root = temporary.path().join("project");
        std::fs::create_dir_all(&project_root).unwrap();
        let db = GlobalDb::open_at(&temporary.path().join("global.db"))
            .await
            .expect("open temporary GlobalDb authority");
        db.upsert_code_project("project.global-compat", &project_root, None, None, None)
            .await
            .expect("register project");

        let runtime = GlobalDbRuntimeCompat::new(&db, binding());
        let context = runtime
            .project_registry_context_by_id("project.global-compat")
            .await
            .expect("resolve project through existing registry adapter");
        assert_eq!(context.project.project_id, "project.global-compat");
    }

    #[tokio::test]
    async fn session_progress_reads_delegate_through_the_borrowed_facade() {
        let temporary = TempDir::new().unwrap();
        let db = GlobalDb::open_at(&temporary.path().join("global.db"))
            .await
            .expect("open temporary GlobalDb authority");
        let session_id = SessionId::new("session.global-compat.read").unwrap();
        let receipt = db
            .begin_or_join_session_refresh_result(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                SessionRefreshFrontierV1::new(1, 0).unwrap(),
            ))
            .await
            .unwrap();
        let runtime = GlobalDbRuntimeCompat::new(&db, binding());

        let request =
            SessionRefreshProgressRequestV1::new(receipt.operation_id().clone(), session_id);
        let expected = GlobalDbRuntime::new(&db)
            .session_store()
            .session_refresh_progress(request.clone())
            .await
            .unwrap();
        let actual = runtime.session_refresh_progress(request).await.unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn unsupported_payloads_fail_as_typed_runtime_unavailable() {
        let temporary = TempDir::new().unwrap();
        let db = GlobalDb::open_at(&temporary.path().join("global.db"))
            .await
            .expect("open temporary GlobalDb authority");
        let binding = StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                id::<BrainId>("brain.global-compat"),
                id::<UserProfileId>("profile.global-compat"),
                id::<ProjectId>("project.global-compat"),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(7).unwrap(),
        );
        let runtime = GlobalDbRuntimeCompat::new(&db, binding.clone());
        let diagnostics = tracedecay_store::SanitizedCleanDiagnosticSnapshotV1::new(
            id("generation.global-compat"),
            vec![],
        )
        .unwrap();
        let request = submit_request(
            binding,
            RepositoryWritePayloadV1::Diagnostics(Box::new(diagnostics)),
            "operation.global-compat.unsupported",
        );
        let probe = Probe::new(request.control());

        assert!(matches!(
            runtime.submit(request, &probe).await.unwrap(),
            RuntimeSubmitOutcomeV1::Unavailable {
                reason: UnavailableReasonV1::UnsupportedOperation
            }
        ));
    }
}
