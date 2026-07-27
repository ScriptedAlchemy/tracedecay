struct DistributionBlockingAcquisitionSource {
    entered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl crate::semantic_code::ModelMemberSourceV1 for DistributionBlockingAcquisitionSource {
    fn fetch_member(
        &self,
        _model: &crate::semantic_code::CatalogedFastEmbedModelV1,
        _upstream_path: &str,
        _destination: &std::path::Path,
    ) -> Result<(), crate::semantic_code::ModelLifecycleErrorV1> {
        self.entered
            .send(())
            .map_err(|_| crate::semantic_code::ModelLifecycleErrorV1::DownloadFailed)?;
        self.release
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv()
            .map_err(|_| crate::semantic_code::ModelLifecycleErrorV1::DownloadFailed)?;
        Err(crate::semantic_code::ModelLifecycleErrorV1::DownloadFailed)
    }
}

#[test]
fn distribution_missing_or_invalid_artifacts_use_typed_fallback_and_strict_unavailable() {
    let profile = tempfile::tempdir().expect("isolated acquisition profile");
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let acquisition = crate::semantic_code::SemanticModelLifecycleOwnerV1::open(
        profile.path(),
        crate::semantic_code::FastEmbedModelCatalogV1::production(),
        Arc::new(DistributionBlockingAcquisitionSource {
            entered: entered_tx,
            release: std::sync::Mutex::new(release_rx),
        }),
    )
    .expect("open isolated acquisition lifecycle");
    acquisition
        .select_model(Some(crate::config::DEFAULT_FASTEMBED_MODEL_ID), true)
        .expect("select packaged default model");
    assert!(acquisition.enqueue_startup_acquisition_if_needed());
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("background acquisition must start without blocking retrieval");
    assert!(matches!(
        acquisition.status().state,
        Some(crate::semantic_code::SemanticModelLifecycleStateV1::Downloading { .. })
    ));

    for (state, expected) in [
        (
            SemanticIndexStateV1::Unavailable,
            SemanticAbstentionV1::IndexUnavailable,
        ),
        (
            SemanticIndexStateV1::Incompatible,
            SemanticAbstentionV1::IndexIncompatible,
        ),
    ] {
        let query_view = query_view();
        let projection = projection();
        let request = request(&query_view, &projection, 1);
        let embedder = FakeQueryEmbedder::default();
        let vectors = FakeVectorReadPort::new(&request, Vec::new());
        let control = FixedExecutionControl::default();
        let lane = SemanticCodeRetriever::new(&embedder, &vectors, &control);
        let fallback = fallback();
        let fallback_identity = Arc::as_ptr(&fallback);

        let permissive = CalibratedSemanticQueryService::new(&lane)
            .execute(
                SemanticLaneReadinessV1::Unavailable(state),
                SemanticQueryModeV1::FallbackAllowed,
                Arc::clone(&fallback),
            )
            .expect("artifact unavailability must preserve the declared fallback");
        assert!(matches!(
            &permissive,
            SemanticQueryServiceOutcomeV1::Fallback {
                abstention,
                ..
            } if abstention == &expected
        ));
        assert_eq!(Arc::as_ptr(permissive.fallback()), fallback_identity);
        assert_eq!(
            embedder.calls.get(),
            0,
            "unavailable artifacts cannot embed"
        );
        assert_eq!(vectors.scans.get(), 0, "unavailable artifacts cannot scan");

        assert_eq!(
            CalibratedSemanticQueryService::new(&lane)
                .execute(
                    SemanticLaneReadinessV1::Unavailable(state),
                    SemanticQueryModeV1::StrictSemantic,
                    fallback,
                )
                .err(),
            Some(SemanticQueryServiceError::StrictUnavailable(expected))
        );
    }

    release_tx
        .send(())
        .expect("release blocked background acquisition");
    for _ in 0..100 {
        if matches!(
            acquisition.status().state,
            Some(crate::semantic_code::SemanticModelLifecycleStateV1::Failed { .. })
        ) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("released background acquisition did not publish its failed state");
}
