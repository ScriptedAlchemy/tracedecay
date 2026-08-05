    #[test]
    fn background_acquisition_does_not_block_startup_or_status_reads() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let source = Arc::new(BlockingFixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
            entered: entered_tx,
            release: Mutex::new(release_rx),
        });
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();

        assert!(owner.enqueue_startup_acquisition_if_needed());
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("background acquisition must start");
        let acquiring = owner.status();
        assert!(matches!(
            acquiring.state,
            Some(SemanticModelLifecycleStateV1::Downloading { .. })
        ));
        assert!(acquiring.semantics_omitted);

        release_tx.send(()).unwrap();
        join_background_acquisition(&owner).expect("fixture acquisition must complete");
        assert!(matches!(
            owner.status().state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
}
    #[test]
    fn explicit_local_import_is_verified_before_lifecycle_installation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let status = owner
            .import_local_artifact(&model_id, &tiny_manifest(&model), fixture.path(), 10)
            .unwrap();
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
    }

    #[test]
    fn restart_re_admits_explicit_import_without_legacy_acquisition() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let imported = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog.clone(),
            scoped_hub_source(root.path()),
        )
        .unwrap()
        .import_local_artifact(&model_id, &tiny_manifest(&model), fixture.path(), 10)
        .unwrap()
        .state
        .unwrap();

        let restarted = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let status = restarted.select_model(Some(&model_id), true).unwrap();

        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
        assert_eq!(
            status.state.as_ref().unwrap().artifact_digest(),
            imported.artifact_digest()
        );
        assert!(!restarted.enqueue_startup_acquisition_if_needed());

        restarted.mark_ready().unwrap();
        let ready_restart = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            tiny_catalog(fixture.path()).0,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let ready = ready_restart.select_model(Some(&model_id), true).unwrap();
        assert!(matches!(
            ready.state,
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ));
        assert!(!ready_restart.enqueue_startup_acquisition_if_needed());
    }

    #[test]
    fn daemon_artifact_gc_collects_only_unreferenced_installs_with_a_receipt() {
        const IMPORTED_AT: u64 = 10;
        const COLLECTED_AT: u64 = IMPORTED_AT + 7 * 24 * 60 * 60;

        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();

        let active_manifest = tiny_manifest(&model);
        let active = owner
            .import_local_artifact(&model_id, &active_manifest, fixture.path(), IMPORTED_AT)
            .unwrap()
            .state
            .unwrap();

        let mut rollback_manifest = active_manifest.clone();
        rollback_manifest.payload.artifact_id = "rollback-fixture".to_owned();
        let rollback = owner
            .artifact_store
            .import_local_directory(&rollback_manifest, fixture.path(), IMPORTED_AT)
            .unwrap();
        owner
            .artifact_store
            .acquire_artifact_lease(
                &rollback.artifact_digest,
                ArtifactLeaseV1 {
                    lease_id: "rollback-fixture".to_owned(),
                    kind: ArtifactLeaseKindV1::Rollback,
                    expires_at_unix: u64::MAX,
                },
                IMPORTED_AT,
            )
            .unwrap();

        let mut orphan_manifest = active_manifest;
        orphan_manifest.payload.artifact_id = "orphan-fixture".to_owned();
        let orphan = owner
            .artifact_store
            .import_local_directory(&orphan_manifest, fixture.path(), IMPORTED_AT)
            .unwrap();

        let receipts = owner.run_daemon_artifact_gc(COLLECTED_AT).unwrap();

        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.artifact_digest.clone())
                .collect::<Vec<_>>(),
            vec![orphan.artifact_digest.clone()]
        );
        let inventory = owner.artifact_store.inventory().unwrap();
        assert!(inventory.records.contains_key(active.artifact_digest()));
        assert!(
            inventory
                .records
                .contains_key(&rollback.artifact_digest.to_string())
        );
        assert!(
            !inventory
                .records
                .contains_key(&orphan.artifact_digest.to_string())
        );
        let receipt_log =
            fs::read_to_string(root.path().join("verified-artifacts/receipts/gc.jsonl")).unwrap();
        assert_eq!(receipt_log.lines().count(), 1);
    }

    fn reranker_manifest(
        model: &CatalogedFastEmbedModelV1,
        artifact_id: &str,
    ) -> ModelArtifactManifestV1 {
        let mut manifest = tiny_manifest(model);
        manifest.payload.artifact_id = artifact_id.to_owned();
        manifest.payload.profile_kind = ArtifactProfileKindV1::Reranker;
        manifest.payload.runtime.runtime =
            super::super::artifact_store::FASTEMBED_RUNTIME_FAMILY_V1.to_owned();
        manifest.payload.runtime.build_revision =
            super::super::artifact_store::FASTEMBED_RUNTIME_BUILD_REVISION_V1.to_owned();
        manifest.payload.runtime.platforms = vec![PlatformTargetV1 {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }];
        manifest
    }

    fn reranker_pins(manifest: &ModelArtifactManifestV1) -> RerankCompatibilityPinsV1 {
        use tracedecay_domain::{ComponentRevision, ManifestDigest, canonical_sha256};

        RerankCompatibilityPinsV1 {
            implementation_revision: ComponentRevision::new(
                super::super::rerank_adapter::RERANK_IMPLEMENTATION_REVISION_V1,
            )
            .unwrap(),
            artifact_manifest_digest: ManifestDigest::new(format!(
                "sha256:{}",
                manifest.artifact_identity_digest()
            ))
            .unwrap(),
            runtime_compatibility_digest: canonical_sha256(&(
                super::super::rerank_adapter::RERANK_RUNTIME_DIGEST_DOMAIN_V1,
                &manifest.payload.runtime.runtime,
                &manifest.payload.runtime.build_revision,
                manifest.payload.device,
                manifest.payload.precision,
            ))
            .unwrap(),
        }
    }

    #[test]
    fn independent_reranker_import_rotates_active_and_rollback_leases() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let first = reranker_manifest(&model, "BAAI/bge-reranker-base");
        let first_pins = reranker_pins(&first);
        let first_digest = first.artifact_identity_digest();

        let first_status = owner
            .import_local_reranker_artifact(first_pins.clone(), &first, fixture.path(), 10)
            .unwrap();
        assert_eq!(
            first_status.active_artifact_digest,
            Some(first_digest.clone())
        );
        assert_eq!(first_status.rollback_artifact_digest, None);
        assert!(owner.mount_reranker(first_pins.clone()).is_ok());

        let second = reranker_manifest(&model, "jinaai/jina-reranker-v1-turbo-en");
        let second_pins = reranker_pins(&second);
        let second_digest = second.artifact_identity_digest();
        let second_status = owner
            .import_local_reranker_artifact(second_pins.clone(), &second, fixture.path(), 11)
            .unwrap();
        assert_eq!(
            second_status.active_artifact_digest,
            Some(second_digest.clone())
        );
        assert_eq!(
            second_status.rollback_artifact_digest,
            Some(first_digest.clone())
        );
        assert!(owner.mount_reranker(first_pins).is_err());
        assert!(owner.mount_reranker(second_pins).is_ok());

        let rolled_back = owner.rollback_reranker_artifact(12).unwrap();
        assert_eq!(rolled_back.active_artifact_digest, Some(first_digest));
        assert_eq!(rolled_back.rollback_artifact_digest, Some(second_digest));
    }

    struct FixtureRerankerHttpsTransport {
        members: BTreeMap<String, Vec<u8>>,
        revision: String,
    }

    impl ExplicitHttpsArtifactTransportV1 for FixtureRerankerHttpsTransport {
        fn fetch_range(
            &self,
            request: &super::super::artifact_store::HttpsArtifactRangeRequestV1,
        ) -> Result<super::super::artifact_store::HttpsArtifactRangeResponseV1, ArtifactImportErrorV1>
        {
            let bytes = self
                .members
                .iter()
                .find_map(|(path, bytes)| {
                    request.url.ends_with(&format!("/{path}")).then_some(bytes)
                })
                .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
            let start = usize::try_from(request.offset)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let count = usize::try_from(request.max_bytes)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let end = start.saturating_add(count).min(bytes.len());
            Ok(super::super::artifact_store::HttpsArtifactRangeResponseV1 {
                offset: request.offset,
                total_length: bytes.len() as u64,
                immutable_revision: self.revision.clone(),
                bytes: bytes[start..end].to_vec(),
            })
        }
    }

    #[test]
    fn configured_https_reranker_acquisition_uses_immutable_member_pins() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let manifest = reranker_manifest(&model, "BAAI/bge-reranker-base");
        let pins = reranker_pins(&manifest);
        let transport = FixtureRerankerHttpsTransport {
            members: manifest
                .payload
                .members
                .iter()
                .map(|member| {
                    (
                        member.path.clone(),
                        fs::read(fixture.path().join(&member.path)).unwrap(),
                    )
                })
                .collect(),
            revision: "immutable-reranker-revision".to_owned(),
        };
        let source = ConfiguredHttpsArtifactSourceV1::new(
            "https://models.example.test/reranker",
            transport.revision.clone(),
        )
        .unwrap();

        let status = owner
            .import_configured_https_reranker_artifact(
                pins.clone(),
                &manifest,
                &source,
                &transport,
                None,
                20,
            )
            .unwrap();

        assert_eq!(
            status.active_artifact_digest,
            Some(manifest.artifact_identity_digest())
        );
        assert!(owner.mount_reranker(pins).is_ok());
    }

    #[test]
    fn reranker_import_rejects_unevaluated_pins_before_installation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let manifest = reranker_manifest(&model, "BAAI/bge-reranker-base");
        let mut pins = reranker_pins(&manifest);
        pins.runtime_compatibility_digest =
            tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "f".repeat(64))).unwrap();

        assert_eq!(
            owner
                .import_local_reranker_artifact(pins, &manifest, fixture.path(), 30)
                .unwrap_err(),
            ModelLifecycleErrorV1::VerificationFailed
        );
        assert_eq!(
            owner.reranker_artifact_status().unwrap(),
            RerankerArtifactLifecycleStatusV1 {
                active_artifact_digest: None,
                rollback_artifact_digest: None,
            }
        );
        assert!(
            !owner
                .artifact_store
                .inventory()
                .unwrap()
                .records
                .contains_key(&manifest.artifact_identity_digest().to_string())
        );
    }

    #[test]
    fn settings_change_schedules_acquire_to_installed_without_blocking_semantics_flag() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source.clone()).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.status().semantics_omitted);
        owner.acquire_blocking_for_tests().unwrap();
        let status = owner.status();
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
        assert!(status.semantics_omitted);
        assert!(source.calls.load(Ordering::SeqCst) >= 5);
        owner.mark_loading().unwrap();
        owner.mark_indexing(1, 2).unwrap();
        owner.mark_ready().unwrap();
        let ready = owner.status();
        assert!(matches!(
            ready.state,
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ));
        assert!(!ready.semantics_omitted);
    }

    #[test]
    fn runtime_failure_retains_ready_rollback_across_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog.clone(), source.clone())
                .unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();
        owner.mark_loading().unwrap();
        owner.mark_indexing(1, 2).unwrap();
        owner
            .mark_runtime_failed("projection failed", true)
            .unwrap();
        assert!(owner.status().remediation.rollback);
        drop(owner);

        let restarted = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        assert!(restarted.status().remediation.rollback);
        let rolled_back = restarted.rollback_to_previous().unwrap();
        assert!(matches!(
            rolled_back.state,
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ));
    }

    #[test]
    fn retry_remove_and_rollback_remediation() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();
        let removed = owner.remove_install().unwrap();
        assert!(matches!(
            removed.state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();
        // Corrupt to Failed then retry.
        {
            let mut guard = owner.inner.lock().unwrap();
            if let Some(SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                ..
            }) = guard.durable.state.clone()
            {
                guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
                    model_id,
                    revision,
                    artifact_digest,
                    detail: "injected".to_owned(),
                    retryable: true,
                });
                persist_durable(&owner.root, &guard.durable).unwrap();
            }
        }
        let retried = owner.retry().unwrap();
        assert!(retried.remediation.retry || retried.state.is_some());
    }

    #[test]
    fn disabling_semantics_skips_startup_queue() {
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap();
        owner.select_model(None, false).unwrap();
        assert!(!owner.enqueue_startup_acquisition_if_needed());
        assert!(owner.status().selected_model.is_none());
    }

    #[test]
    fn verified_background_install_reopens_offline_without_acquisition() {
        struct OfflineSource {
            calls: AtomicUsize,
        }

        impl ModelMemberSourceV1 for OfflineSource {
            fn fetch_member(
                &self,
                _model: &CatalogedFastEmbedModelV1,
                _upstream_path: &str,
                _destination: &Path,
            ) -> Result<(), ModelLifecycleErrorV1> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(ModelLifecycleErrorV1::DownloadFailedWithReason(
                    "offline source must not be called for a verified install".to_owned(),
                ))
            }
        }

        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let online_source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let online = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog.clone(),
            online_source,
        )
        .unwrap();
        online.select_model(Some(&model_id), true).unwrap();
        online.acquire_blocking_for_tests().unwrap();
        let installed = online.status().state.unwrap();
        assert!(matches!(
            installed,
            SemanticModelLifecycleStateV1::Installed { .. }
        ));
        drop(online);

        let offline_source = Arc::new(OfflineSource {
            calls: AtomicUsize::new(0),
        });
        let reopened = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            offline_source.clone(),
        )
        .unwrap();
        let reopened_status = reopened
            .select_model(Some(&model_id), true)
            .expect("verified installed bytes must re-admit offline");

        assert_eq!(reopened_status.state, Some(installed));
        assert!(!reopened.enqueue_startup_acquisition_if_needed());
        assert_eq!(offline_source.calls.load(Ordering::SeqCst), 0);
    }
