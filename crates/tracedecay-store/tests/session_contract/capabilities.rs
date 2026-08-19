use super::common::*;
use super::*;
use tracedecay_temporal_query::ports::ExecutionControl;

struct CapabilityDeniedSessionPorts {
    capabilities: SessionTemporalCapabilitiesV1,
}

impl CapabilityDeniedSessionPorts {
    fn new() -> Self {
        Self {
            capabilities: capabilities([]),
        }
    }
}

impl SessionTemporalCapabilityProvider for CapabilityDeniedSessionPorts {
    fn session_temporal_capabilities(&self) -> &SessionTemporalCapabilitiesV1 {
        &self.capabilities
    }
}

impl SessionRetrievalStore for CapabilityDeniedSessionPorts {
    async fn freeze_session_temporal_snapshot_supported(
        &self,
        _permit: SessionSnapshotFreezePermit,
        _request: SessionTemporalSnapshotRequestV1,
    ) -> SessionStoreResult<SessionTemporalSnapshotV1> {
        panic!("capability guard was bypassed")
    }

    async fn retrieve_session_temporal_page_supported(
        &self,
        _permit: SessionTemporalPageRetrievePermit,
        _request: SessionTemporalRetrievalRequestV1,
    ) -> SessionStoreResult<SessionRetrievalPageV1> {
        panic!("capability guard was bypassed")
    }
}

impl SessionTemporalProjectionStore for CapabilityDeniedSessionPorts {
    async fn begin_session_generation_rebuild_supported(
        &self,
        _permit: SessionGenerationRebuildBeginPermit,
        _request: SessionGenerationRebuildRequestV1,
    ) -> SessionStoreResult<SessionGenerationRebuildReceiptV1> {
        panic!("capability guard was bypassed")
    }

    async fn persist_session_temporal_projection_batch_supported(
        &self,
        _permit: SessionProjectionBatchPersistPermit,
        _batch: SessionTemporalProjectionBatchV1,
    ) -> SessionStoreResult<SessionTemporalProjectionBatchReceiptV1> {
        panic!("capability guard was bypassed")
    }

    async fn activate_session_temporal_generation_supported(
        &self,
        _permit: SessionGenerationActivatePermit,
        _request: SessionGenerationActivationRequestV1,
    ) -> SessionStoreResult<SessionGenerationActivationReceiptV1> {
        panic!("capability guard was bypassed")
    }
}

impl SessionRefreshStore for CapabilityDeniedSessionPorts {
    async fn begin_or_join_session_refresh_supported(
        &self,
        _permit: SessionRefreshBeginOrJoinPermit,
        _request: SessionRefreshBeginOrJoinRequestV1,
    ) -> SessionStoreResult<SessionRefreshBeginOrJoinReceiptV1> {
        panic!("capability guard was bypassed")
    }

    async fn persist_session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressPersistPermit,
        _progress: SessionRefreshProgressV1,
    ) -> SessionStoreResult<SessionRefreshProgressV1> {
        panic!("capability guard was bypassed")
    }

    async fn session_refresh_progress_supported(
        &self,
        _permit: SessionRefreshProgressReadPermit,
        _request: SessionRefreshProgressRequestV1,
    ) -> SessionStoreResult<Option<SessionRefreshProgressV1>> {
        panic!("capability guard was bypassed")
    }

    async fn complete_session_refresh_supported(
        &self,
        _permit: SessionRefreshCompletePermit,
        _request: SessionRefreshCompletionRequestV1,
        _execution_control: ExecutionControl,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        panic!("capability guard was bypassed")
    }

    async fn fail_session_refresh_supported(
        &self,
        _permit: SessionRefreshFailPermit,
        _request: SessionRefreshFailureRequestV1,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        panic!("capability guard was bypassed")
    }

    async fn cancel_session_refresh_supported(
        &self,
        _permit: SessionRefreshCancelPermit,
        _request: SessionRefreshCancellationRequestV1,
    ) -> SessionStoreResult<SessionRefreshReceiptV1> {
        panic!("capability guard was bypassed")
    }

    async fn session_refresh_receipt_supported(
        &self,
        _permit: SessionRefreshReceiptReadPermit,
        _request: SessionRefreshReceiptRequestV1,
    ) -> SessionStoreResult<Option<SessionRefreshReceiptV1>> {
        panic!("capability guard was bypassed")
    }
}

#[test]
fn refresh_ports_deny_every_unsupported_capability() {
    let ports = CapabilityDeniedSessionPorts::new();
    let session_id = session("session.fixture");
    let operation_id = operation_id();
    let partial_frontier = SessionRefreshFrontierV1::new(10, 8).unwrap();
    let complete_frontier = SessionRefreshFrontierV1::new(10, 10).unwrap();
    let progress = SessionRefreshProgressV1::new(
        operation_id.clone(),
        session_id.clone(),
        partial_frontier,
        coverage(),
        2,
        8,
        UtcMicros(100),
    );
    let completion = SessionRefreshCompletionRequestV1::new(
        operation_id.clone(),
        session_id.clone(),
        complete_frontier,
        coverage(),
    )
    .unwrap();
    let failure = SessionRefreshFailureRequestV1::new(
        operation_id.clone(),
        session_id.clone(),
        partial_frontier,
        coverage(),
        "source_unavailable",
    )
    .unwrap();
    let cancellation = SessionRefreshCancellationRequestV1::new(
        operation_id.clone(),
        session_id.clone(),
        partial_frontier,
        coverage(),
    );
    let progress_request =
        SessionRefreshProgressRequestV1::new(operation_id.clone(), session_id.clone());
    let receipt_request =
        SessionRefreshReceiptRequestV1::new(operation_id.clone(), session_id.clone());

    // CapabilityDeniedSessionPorts panics if any *_supported dispatch is entered.
    // Returning UnsupportedCapability (instead of panicking) proves the public
    // call surface never reaches unguarded dispatch without a granted permit.
    let refresh_results = [
        ready(
            ports.begin_or_join_session_refresh(SessionRefreshBeginOrJoinRequestV1::new(
                session_id.clone(),
                complete_frontier,
            )),
        )
        .map(|_| ()),
        ready(ports.persist_session_refresh_progress(progress)).map(|_| ()),
        ready(ports.session_refresh_progress(progress_request)).map(|_| ()),
        ready(ports.complete_session_refresh(completion, ExecutionControl::default())).map(|_| ()),
        ready(ports.fail_session_refresh(failure)).map(|_| ()),
        ready(ports.cancel_session_refresh(cancellation)).map(|_| ()),
        ready(ports.session_refresh_receipt(receipt_request)).map(|_| ()),
    ];
    let expected_refresh_capabilities = [
        SessionTemporalCapabilityV1::RefreshJoin,
        SessionTemporalCapabilityV1::RefreshProgressPersistence,
        SessionTemporalCapabilityV1::RefreshProgressPersistence,
        SessionTemporalCapabilityV1::RefreshProgressPersistence,
        SessionTemporalCapabilityV1::RefreshProgressPersistence,
        SessionTemporalCapabilityV1::RefreshCancellation,
        SessionTemporalCapabilityV1::RefreshProgressPersistence,
    ];
    for (result, capability) in refresh_results
        .into_iter()
        .zip(expected_refresh_capabilities)
    {
        assert!(matches!(
            result,
            Err(SessionStoreError::UnsupportedCapability {
                capability: actual
            }) if actual == capability
        ));
    }
}

#[test]
fn adapter_capabilities_override_forged_snapshot_capabilities() {
    let ports = CapabilityDeniedSessionPorts::new();
    let session_id = session("session.fixture");
    let forged_snapshot = snapshot_with_capabilities(
        session_id.clone(),
        [SessionTemporalCapabilityV1::FrozenWatermarks],
    );
    let request = SessionTemporalRetrievalRequestV1::new(
        session_id.clone(),
        TemporalModeV1::Current,
        RetrievalGrainV1::Occurrence,
        forged_snapshot,
        1,
        None,
        ExecutionControl::default(),
    )
    .unwrap();

    assert!(matches!(
        ready(ports.retrieve_session_temporal_page(request)),
        Err(SessionStoreError::UnsupportedCapability {
            capability: SessionTemporalCapabilityV1::FrozenWatermarks
        })
    ));

    let forged = snapshot_for(session_id.clone(), 7);
    let rebuild =
        SessionGenerationRebuildRequestV1::new(session_id.clone(), generation(8), forged.clone())
            .unwrap();
    let activation = SessionGenerationActivationRequestV1::new(
        session_id.clone(),
        generation(8),
        forged.clone(),
        ExecutionControl::default(),
    )
    .unwrap();
    let projection = projection_batch(&session_id);

    let results = [
        ready(
            ports.freeze_session_temporal_snapshot(SessionTemporalSnapshotRequestV1::new(
                session_id,
            )),
        )
        .map(|_| ()),
        ready(ports.begin_session_generation_rebuild(rebuild)).map(|_| ()),
        ready(ports.persist_session_temporal_projection_batch(projection)).map(|_| ()),
        ready(ports.activate_session_temporal_generation(activation)).map(|_| ()),
    ];
    let required = [
        SessionTemporalCapabilityV1::FrozenWatermarks,
        SessionTemporalCapabilityV1::GenerationRebuild,
        SessionTemporalCapabilityV1::GenerationRebuild,
        SessionTemporalCapabilityV1::GenerationRebuild,
    ];
    for (result, expected) in results.into_iter().zip(required) {
        assert!(matches!(
            result,
            Err(SessionStoreError::UnsupportedCapability { capability })
                if capability == expected
        ));
    }
}

#[test]
fn guarded_refresh_dispatch_never_enters_denied_adapters() {
    refresh_ports_deny_every_unsupported_capability();
}

#[test]
fn yielding_in_memory_adapter_exercises_every_guarded_port() {
    let ports = InMemorySessionPorts::default();
    let session_id = session("session.adapter");
    let snapshot = yields_then_ready(ports.freeze_session_temporal_snapshot(
        SessionTemporalSnapshotRequestV1::new(session_id.clone()),
    ))
    .unwrap();

    let rebuild_request =
        SessionGenerationRebuildRequestV1::new(session_id.clone(), generation(8), snapshot.clone())
            .unwrap();
    let started = ready(ports.begin_session_generation_rebuild(rebuild_request.clone())).unwrap();
    let resumed = ready(ports.begin_session_generation_rebuild(rebuild_request)).unwrap();
    assert_eq!(
        started.disposition(),
        SessionGenerationRebuildDispositionV1::Started
    );
    assert_eq!(
        resumed.disposition(),
        SessionGenerationRebuildDispositionV1::Resumed
    );

    let projection = projection_batch(&session_id);
    let applied =
        ready(ports.persist_session_temporal_projection_batch(projection.clone())).unwrap();
    let replayed =
        ready(ports.persist_session_temporal_projection_batch(projection.clone())).unwrap();
    assert_eq!(
        applied.disposition(),
        SessionTemporalProjectionBatchDispositionV1::Applied
    );
    assert_eq!(
        replayed.disposition(),
        SessionTemporalProjectionBatchDispositionV1::ExactReplay
    );

    let activation = SessionGenerationActivationRequestV1::new(
        session_id.clone(),
        generation(8),
        snapshot.clone(),
        ExecutionControl::default(),
    )
    .unwrap();
    assert_eq!(
        ready(ports.activate_session_temporal_generation(activation))
            .unwrap()
            .previous_generation(),
        Some(generation(7))
    );

    let page = ready(
        ports.retrieve_session_temporal_page(
            SessionTemporalRetrievalRequestV1::new(
                session_id.clone(),
                TemporalModeV1::Current,
                RetrievalGrainV1::Summary,
                snapshot,
                10,
                None,
                ExecutionControl::default(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    assert!(page.summaries().is_empty());

    let target = SessionRefreshFrontierV1::new(10, 10).unwrap();
    let join_request = SessionRefreshBeginOrJoinRequestV1::new(session_id.clone(), target);
    let started = ready(ports.begin_or_join_session_refresh(join_request.clone())).unwrap();
    let joined = ready(ports.begin_or_join_session_refresh(join_request)).unwrap();
    assert_eq!(started.disposition(), SessionRefreshDispositionV1::Started);
    assert_eq!(joined.disposition(), SessionRefreshDispositionV1::Joined);

    let first_progress = SessionRefreshProgressV1::new(
        operation_id(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(10, 8).unwrap(),
        coverage(),
        1,
        8,
        UtcMicros(106),
    );
    ready(ports.persist_session_refresh_progress(first_progress)).unwrap();
    let final_progress = SessionRefreshProgressV1::new(
        operation_id(),
        session_id.clone(),
        target,
        coverage(),
        2,
        10,
        UtcMicros(107),
    );
    ready(ports.persist_session_refresh_progress(final_progress)).unwrap();
    let terminal = ready(
        ports.complete_session_refresh(
            SessionRefreshCompletionRequestV1::new(
                operation_id(),
                session_id.clone(),
                target,
                coverage(),
            )
            .unwrap(),
            ExecutionControl::default(),
        ),
    )
    .unwrap();
    assert_eq!(terminal.state(), SessionRefreshTerminalStateV1::Complete);
    assert!(
        ready(
            ports.session_refresh_receipt(SessionRefreshReceiptRequestV1::new(
                operation_id(),
                session_id.clone(),
            ))
        )
        .unwrap()
        .is_some()
    );
}
