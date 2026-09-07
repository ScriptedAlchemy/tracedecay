    #[test]
    fn config_selection_waits_for_semantic_demand_without_blocking_status_reads() {
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
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            Arc::clone(&source) as Arc<dyn ModelMemberSourceV1>,
        )
        .unwrap();
        let selected = apply_config_selection_to_owner(&owner, Some(&model_id), true).unwrap();

        assert!(matches!(
            selected.state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            0,
            "configuration must not fetch model bytes before semantic demand"
        );

        assert!(owner.enqueue_demand_acquisition_if_needed());
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
    fn isolated_evaluation_import_rejects_an_incomplete_catalog_package() {
        let lifecycle_root = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();

        let result = open_local_semantic_evaluation_lifecycle(
            lifecycle_root.path(),
            package.path(),
            SemanticResourceCeilings::default(),
            10,
        );

        assert!(matches!(
            result,
            Err(ModelLifecycleErrorV1::VerificationFailed)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn isolated_evaluation_import_never_follows_a_catalog_member_symlink() {
        use std::os::unix::fs::symlink;

        let package = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), package.path().join("model.onnx")).unwrap();
        let package_root =
            cap_std::fs::Dir::open_ambient_dir(package.path(), cap_fs_ext::ambient_authority())
                .unwrap();
        let destination = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(destination.path()).unwrap();

        let result =
            copy_local_evaluation_member(&package_root, "model.onnx", destination.path());

        assert_eq!(result, Err(ModelLifecycleErrorV1::VerificationFailed));
        assert!(!destination.path().exists());
    }

    // Restart re-admission of an explicitly imported artifact (its digest is
    // not the catalog package digest) verifies the manifest against
    // `RuntimeEnvironmentV1::detect_fastembed_process()`, which is a typed
    // capability absence without the bundled runtime. Workspace builds unify
    // `semantic-fastembed` on via the root crate's `production` default; the
    // gate keeps scoped `-p tracedecay-semantic` runs truthful.
    #[cfg(feature = "semantic-fastembed")]
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
        assert!(!restarted.enqueue_demand_acquisition_if_needed());

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
        assert!(!ready_restart.enqueue_demand_acquisition_if_needed());
    }

    #[test]
    fn explicit_import_atomically_retains_and_restores_the_verified_rollback_artifact() {
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
        let first_manifest = tiny_manifest(&model);
        let first = owner
            .import_local_artifact(&model_id, &first_manifest, fixture.path(), 10)
            .unwrap();
        owner.mark_ready().unwrap();
        let mut second_manifest = first_manifest;
        second_manifest.payload.resource_ceiling.max_resident_bytes += 1;
        let second = owner
            .import_local_artifact(&model_id, &second_manifest, fixture.path(), 20)
            .unwrap();

        let rolled_back = owner.rollback_to_previous().unwrap();

        assert_ne!(
            first.state.as_ref().unwrap().artifact_digest(),
            second.state.as_ref().unwrap().artifact_digest()
        );
        assert_eq!(
            rolled_back.state.as_ref().unwrap().artifact_digest(),
            first.state.as_ref().unwrap().artifact_digest()
        );
        let active = owner
            .artifact_store
            .artifact_digest_for_lease(
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                30,
            )
            .unwrap();
        let retained = owner
            .artifact_store
            .artifact_digest_for_lease(
                EMBEDDING_ROLLBACK_LEASE_ID_V1,
                ArtifactLeaseKindV1::Rollback,
                30,
            )
            .unwrap();
        assert_eq!(
            active.unwrap().to_string(),
            first.state.unwrap().artifact_digest()
        );
        assert_eq!(
            retained.unwrap().to_string(),
            second.state.unwrap().artifact_digest()
        );
    }

    #[test]
    fn failed_lifecycle_persist_restores_the_prior_active_artifact_lease() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog.clone(),
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let first_manifest = tiny_manifest(&model);
        let first = owner
            .import_local_artifact(&model_id, &first_manifest, fixture.path(), 10)
            .unwrap();
        owner.mark_ready().unwrap();
        let durable_bytes = fs::read(root.path().join("lifecycle.json")).unwrap();
        fs::remove_file(root.path().join("lifecycle.json")).unwrap();
        fs::create_dir(root.path().join("lifecycle.json")).unwrap();
        let mut second_manifest = first_manifest;
        second_manifest.payload.resource_ceiling.max_resident_bytes += 1;

        assert_eq!(
            owner
                .import_local_artifact(&model_id, &second_manifest, fixture.path(), 20)
                .unwrap_err(),
            ModelLifecycleErrorV1::StoreUnavailable
        );
        let active = owner
            .artifact_store
            .artifact_digest_for_lease(
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                30,
            )
            .unwrap();
        assert_eq!(
            active.unwrap().to_string(),
            first.state.as_ref().unwrap().artifact_digest()
        );
        assert_eq!(
            owner.status().state.as_ref().unwrap().artifact_digest(),
            first.state.as_ref().unwrap().artifact_digest()
        );

        fs::remove_dir(root.path().join("lifecycle.json")).unwrap();
        fs::write(root.path().join("lifecycle.json"), durable_bytes).unwrap();
        let restarted =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, scoped_hub_source(root.path()))
                .unwrap();
        assert_eq!(
            restarted.status().state.as_ref().unwrap().artifact_digest(),
            first.state.unwrap().artifact_digest()
        );
    }

    #[test]
    fn restart_reconciles_an_inventory_rotation_that_never_committed_lifecycle_state() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog.clone(),
            scoped_hub_source(root.path()),
        )
        .unwrap();
        let first_manifest = tiny_manifest(&model);
        let first = owner
            .import_local_artifact(&model_id, &first_manifest, fixture.path(), 10)
            .unwrap();
        owner.mark_ready().unwrap();
        let mut uncommitted_manifest = first_manifest;
        uncommitted_manifest
            .payload
            .resource_ceiling
            .max_resident_bytes += 1;
        let uncommitted = owner
            .artifact_store
            .import_local_directory(&uncommitted_manifest, fixture.path(), 20)
            .unwrap();
        owner
            .artifact_store
            .activate_artifact_with_rollback(
                &uncommitted.artifact_digest,
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                EMBEDDING_ROLLBACK_LEASE_ID_V1,
                20,
            )
            .unwrap();
        drop(owner);

        let restarted =
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, scoped_hub_source(root.path()))
                .unwrap();
        let active = restarted
            .artifact_store
            .artifact_digest_for_lease(
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                30,
            )
            .unwrap();
        assert_eq!(
            active.unwrap().to_string(),
            first.state.unwrap().artifact_digest()
        );
        assert_eq!(
            restarted
                .artifact_store
                .artifact_digest_for_lease(
                    EMBEDDING_ROLLBACK_LEASE_ID_V1,
                    ArtifactLeaseKindV1::Rollback,
                    30,
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn concurrent_publication_and_rollback_leave_one_matching_active_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(
            SemanticModelLifecycleOwnerV1::open(
                root.path(),
                catalog,
                scoped_hub_source(root.path()),
            )
            .unwrap(),
        );
        let first_manifest = tiny_manifest(&model);
        owner
            .import_local_artifact(&model_id, &first_manifest, fixture.path(), 10)
            .unwrap();
        owner.mark_ready().unwrap();
        let mut second_manifest = first_manifest.clone();
        second_manifest.payload.resource_ceiling.max_resident_bytes += 1;
        owner
            .import_local_artifact(&model_id, &second_manifest, fixture.path(), 20)
            .unwrap();
        owner.mark_ready().unwrap();
        let mut third_manifest = first_manifest;
        third_manifest.payload.resource_ceiling.max_resident_bytes += 2;
        let start = Arc::new(Barrier::new(3));
        let publisher = {
            let owner = Arc::clone(&owner);
            let start = Arc::clone(&start);
            let model_id = model_id.clone();
            let source = fixture.path().to_path_buf();
            std::thread::spawn(move || {
                start.wait();
                owner.import_local_artifact(&model_id, &third_manifest, &source, 30)
            })
        };
        let rollback = {
            let owner = Arc::clone(&owner);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                owner.rollback_to_previous()
            })
        };
        start.wait();
        publisher.join().unwrap().unwrap();
        rollback.join().unwrap().unwrap();

        let status = owner.status();
        let active = owner
            .artifact_store
            .artifact_digest_for_lease(
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                40,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            active.to_string(),
            status.state.as_ref().unwrap().artifact_digest()
        );
        let rolled_back = owner.rollback_to_previous().unwrap();
        let active = owner
            .artifact_store
            .artifact_digest_for_lease(
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                40,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            active.to_string(),
            rolled_back.state.unwrap().artifact_digest()
        );
    }

    #[test]
    fn restart_rejects_corrupt_lifecycle_state_instead_of_reconstructing_installation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lifecycle.json"), b"{not-json").unwrap();

        let result = SemanticModelLifecycleOwnerV1::open_default(root.path());

        assert!(matches!(
            result,
            Err(ModelLifecycleErrorV1::VerificationFailed)
        ));
    }

    #[test]
    fn restart_rejects_a_future_lifecycle_schema_instead_of_resetting_state() {
        let root = tempfile::tempdir().unwrap();
        drop(SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap());
        let path = root.path().join("lifecycle.json");
        let mut durable: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        durable["schema"] = serde_json::Value::String("tracedecay.fastembed.model-lifecycle.v2".into());
        std::fs::write(&path, serde_json::to_vec_pretty(&durable).unwrap()).unwrap();

        let result = SemanticModelLifecycleOwnerV1::open_default(root.path());

        assert!(matches!(
            result,
            Err(ModelLifecycleErrorV1::VerificationFailed)
        ));
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

    #[cfg(feature = "semantic-fastembed")]
    fn pinned_reranker_fixture_catalog(fixture: &Path) -> (FastEmbedModelCatalogV1, String) {
        let mut members = BTreeMap::new();
        for (role, name) in [
            ("model", "model.onnx"),
            ("tokenizer", "tokenizer.json"),
            ("config", "config.json"),
            ("special_tokens_map", "special_tokens_map.json"),
            ("tokenizer_config", "tokenizer_config.json"),
        ] {
            let bytes = fs::read(fixture.join(name)).unwrap_or_else(|error| {
                panic!(
                    "cannot read pinned reranker fixture member {}: {error}",
                    fixture.join(name).display()
                )
            });
            members.insert(
                role.to_owned(),
                CatalogMemberPinV1 {
                    path: name.to_owned(),
                    upstream_path: name.to_owned(),
                    length: bytes.len() as u64,
                    sha256: hex::encode(Sha256::digest(&bytes)),
                },
            );
        }
        let model = CatalogedFastEmbedModelV1 {
            model_id: "PinnedJinaRerankerV1TurboEn".to_owned(),
            backend: CatalogedEmbeddingBackendV1::FastEmbedOrt {
                fastembed_enum: "JINARerankerV1TurboEn".to_owned(),
            },
            model_code: "jinaai/jina-reranker-v1-turbo-en".to_owned(),
            source: CatalogSourceV1 {
                upstream: "https://huggingface.co/jinaai/jina-reranker-v1-turbo-en".to_owned(),
                revision: "b8c14f4e723d9e0aab4732a7b7b93741eeeb77c2".to_owned(),
                license: "Apache-2.0".to_owned(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".to_owned(),
                provenance:
                    "https://huggingface.co/jinaai/jina-reranker-v1-turbo-en/tree/b8c14f4e723d9e0aab4732a7b7b93741eeeb77c2"
                        .to_owned(),
            },
            expected_dimensions: 1,
            max_length: 512,
            members,
        };
        let mut catalog = FastEmbedModelCatalogV1::production();
        catalog.models.push(model.clone());
        (catalog, model.model_id)
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    #[ignore = "requires TRACEDECAY_FASTEMBED_FIXTURE_DIR with a pinned local reranker"]
    fn pinned_reranker_cold_first_and_warm_queries_reuse_one_session() {
        let fixture = PathBuf::from(
            std::env::var("TRACEDECAY_FASTEMBED_FIXTURE_DIR").unwrap_or_else(|_| {
                panic!(
                    "this gate requires its pinned reranker in \
                     TRACEDECAY_FASTEMBED_FIXTURE_DIR"
                )
            }),
        );
        let (catalog, model_id) = pinned_reranker_fixture_catalog(&fixture);
        let model = catalog.get(&model_id).unwrap().clone();
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(
            SemanticModelLifecycleOwnerV1::open(
                root.path(),
                catalog,
                scoped_hub_source(root.path()),
            )
            .unwrap(),
        );
        let mut manifest = reranker_manifest(&model, "jinaai/jina-reranker-v1-turbo-en");
        let package_bytes = manifest
            .payload
            .members
            .iter()
            .map(|member| member.byte_length)
            .sum::<u64>();
        manifest.payload.resource_ceiling.max_model_bytes =
            manifest.payload.model_member.byte_length;
        manifest.payload.resource_ceiling.max_tokenizer_bytes =
            package_bytes.saturating_sub(manifest.payload.model_member.byte_length);
        manifest.payload.resource_ceiling.max_resident_bytes = package_bytes.saturating_mul(4);
        manifest.payload.resource_ceiling.max_batch_size = 8;
        manifest.payload.resource_ceiling.max_sequence_length = 512;
        let pins = reranker_pins(&manifest);
        owner
            .import_local_reranker_artifact(pins.clone(), &manifest, &fixture, 10)
            .unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let mounts = (0..2)
            .map(|_| {
                let owner = Arc::clone(&owner);
                let barrier = Arc::clone(&barrier);
                let pins = pins.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    owner.mount_reranker(pins)
                })
            })
            .collect::<Vec<_>>();
        let publication_started = Instant::now();
        barrier.wait();
        let mut authorities = mounts
            .into_iter()
            .map(|mount| mount.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        let publication_micros = publication_started.elapsed().as_micros();
        let shared_authority = authorities.pop().unwrap();
        let authority = authorities.pop().unwrap();
        assert!(
            Arc::ptr_eq(
                authority.executor_handle(),
                shared_authority.executor_handle()
            ),
            "repeated mounts must share one resident model session"
        );

        let candidates = ["session pooling", "unrelated rendering"]
            .into_iter()
            .enumerate()
            .map(|(index, _)| tracedecay_domain::RankedCandidate {
                candidate: tracedecay_domain::FusedCandidate {
                    anchor_id: tracedecay_domain::RetrievalAnchorId::new(format!(
                        "rerank.fixture.{index}"
                    ))
                    .unwrap(),
                    logical_evidence_id: tracedecay_domain::LogicalEvidenceId::new(format!(
                        "rerank.fixture.evidence.{index}"
                    ))
                    .unwrap(),
                    occurrences: Vec::new(),
                    exact_class: tracedecay_domain::ExactClass::Approximate,
                    utility_micros: 0,
                    contributions: Vec::new(),
                    freshness: Vec::new(),
                    decisions: Vec::new(),
                },
                final_ordinal: index as u32,
            })
            .collect::<Vec<_>>();
        let snapshot =
            tracedecay_domain::CandidateSetDigest::new(format!("sha256:{}", "a".repeat(64)))
                .unwrap();
        let privacy = tracedecay_domain::PrivacyDomainId::new("privacy.rerank.fixture").unwrap();
        let query = "reuse one warmed reranker session";
        let views = candidates
            .iter()
            .zip(["session pooling", "unrelated rendering"])
            .map(|(candidate, document)| {
                let mut approved_features = Vec::new();
                approved_features.extend_from_slice(&(query.len() as u32).to_be_bytes());
                approved_features.extend_from_slice(query.as_bytes());
                approved_features.extend_from_slice(document.as_bytes());
                tracedecay_domain::AuthorizedRerankView {
                    anchor_id: candidate.candidate.anchor_id.clone(),
                    snapshot_digest: snapshot.clone(),
                    privacy_domain: privacy.clone(),
                    compatibility: tracedecay_domain::FreshnessCompatibilityV1::Current,
                    approved_features,
                }
            })
            .collect::<Vec<_>>();
        let inputs = candidates
            .iter()
            .zip(&views)
            .map(
                |(candidate, view)| tracedecay_query::retrieval::rerank::LocalRerankInputV1 {
                    candidate,
                    view,
                },
            )
            .collect::<Vec<_>>();
        let permit = tracedecay_query::retrieval::rerank::LocalRerankPermitV1 {
            input_bytes: views
                .iter()
                .map(|view| view.approved_features.len() as u64)
                .sum(),
            input_tokens: 64,
            work_units: 2,
            model_invocations: 1,
            remaining_deadline_micros: None,
        };
        let policy = tracedecay_domain::RerankPolicy {
            policy_id: tracedecay_domain::RerankPolicyId::new("rerank.fixture.policy").unwrap(),
            evaluation_result_anchor: tracedecay_domain::RetrievalAnchorId::new(
                "rerank.fixture.evaluation",
            )
            .unwrap(),
            max_candidates: 2,
            max_input_bytes: permit.input_bytes,
            max_input_tokens: permit.input_tokens,
            max_work_units: permit.work_units,
            max_model_invocations: permit.model_invocations,
            deadline_micros: None,
        };

        let first_query_started = Instant::now();
        let first = authority
            .executor()
            .rerank(&policy, &inputs, permit)
            .unwrap();
        let first_query_micros = first_query_started.elapsed().as_micros();
        assert_eq!(first.len(), inputs.len());

        let mut warm_micros = Vec::with_capacity(20);
        for _ in 0..20 {
            let started = Instant::now();
            let warm = authority
                .executor()
                .rerank(&policy, &inputs, permit)
                .unwrap();
            warm_micros.push(started.elapsed().as_micros());
            assert_eq!(warm, first);
        }
        warm_micros.sort_unstable();
        let warm_p50_micros = warm_micros[warm_micros.len() / 2];
        let warm_p95_micros = warm_micros[(warm_micros.len() * 95).div_ceil(100) - 1];
        println!(
            "reranker publication={publication_micros}us first_query={first_query_micros}us \
             warm_p50={warm_p50_micros}us warm_p95={warm_p95_micros}us samples={}",
            warm_micros.len()
        );
    }

    // Reranker publication admits the artifact for runtime against
    // `detect_fastembed_process()` evidence; see the gate rationale above.
    #[cfg(feature = "semantic-fastembed")]
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
        assert!(matches!(
            owner.mount_reranker(first_pins.clone()),
            Err(ModelLifecycleErrorV1::RerankerUnavailable)
        ));

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
        assert!(matches!(
            owner.mount_reranker(first_pins),
            Err(ModelLifecycleErrorV1::VerificationFailed)
        ));
        assert!(matches!(
            owner.mount_reranker(second_pins.clone()),
            Err(ModelLifecycleErrorV1::RerankerUnavailable)
        ));

        let rolled_back = owner.rollback_reranker_artifact(12).unwrap();
        assert_eq!(rolled_back.active_artifact_digest, Some(first_digest));
        assert_eq!(rolled_back.rollback_artifact_digest, Some(second_digest));
        assert!(matches!(
            owner.mount_reranker(second_pins),
            Err(ModelLifecycleErrorV1::VerificationFailed)
        ));
    }

    #[cfg(feature = "semantic-fastembed")]
    struct FixtureRerankerHttpsTransport {
        members: BTreeMap<String, Vec<u8>>,
        revision: String,
    }

    #[cfg(feature = "semantic-fastembed")]
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

    // Same runtime-evidence gate as the local reranker import above.
    #[cfg(feature = "semantic-fastembed")]
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
        assert!(matches!(
            owner.mount_reranker(pins),
            Err(ModelLifecycleErrorV1::RerankerUnavailable)
        ));
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
            let mut guard = owner.inner.writer();
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
        let mut recovery = owner.verified_ready_events();
        let prior_recovery_epoch = recovery.borrow().epoch;
        let retried = owner.retry().unwrap();
        assert!(retried.remediation.retry || retried.state.is_some());
        assert!(recovery.has_changed().unwrap());
        let recovered = recovery.borrow_and_update().clone();
        assert!(recovered.epoch > prior_recovery_epoch);
        assert!(recovered.artifact_digest.is_some());
    }

    #[test]
    fn disabling_semantics_skips_startup_queue() {
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap();
        // Product default is auto_download: true; None still disables the lane.
        owner.select_model(None, true).unwrap();
        let status = owner.status();
        assert!(!owner.enqueue_demand_acquisition_if_needed());
        assert!(status.selected_model.is_none());
        assert!(status.state.is_none());
        assert!(status.semantics_omitted);
        assert!(status.auto_download);
    }

    #[test]
    fn hermetic_download_failure_reports_failed_not_warming() {
        struct RefusedNetworkSource;

        impl ModelMemberSourceV1 for RefusedNetworkSource {
            fn fetch_member(
                &self,
                _model: &CatalogedFastEmbedModelV1,
                _upstream_path: &str,
                _destination: &Path,
            ) -> Result<(), ModelLifecycleErrorV1> {
                Err(ModelLifecycleErrorV1::DownloadFailedWithReason(
                    "connection refused to unroutable endpoint 192.0.2.1".to_owned(),
                ))
            }
        }

        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            Arc::new(RefusedNetworkSource),
        )
        .unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.enqueue_demand_acquisition_if_needed());
        assert_eq!(
            join_background_acquisition(&owner),
            Err(ModelLifecycleErrorV1::DownloadFailed)
        );

        let status = owner.status();
        let Some(SemanticModelLifecycleStateV1::Failed {
            detail,
            retryable,
            ..
        }) = &status.state
        else {
            panic!("failed download must be Failed, not warming: {status:?}");
        };
        assert!(*retryable);
        assert!(detail.contains("unroutable"));
        assert!(status.semantics_omitted);
    }

    #[test]
    fn publication_lease_blocks_selection_until_the_commit_boundary_releases_it() {
        struct ActiveCancellation;

        impl SemanticExecutionAuthority for ActiveCancellation {
            fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
                None
            }
        }

        impl SemanticEvaluationCancellationV1 for ActiveCancellation {}

        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner = Arc::new(
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap(),
        );
        owner.select_model(Some(&model_id), true).unwrap();
        owner.acquire_blocking_for_tests().unwrap();
        owner.mark_ready().unwrap();

        let identity = owner.verified_evaluation_publication_identity().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let lease = runtime
            .block_on(owner.acquire_verified_evaluation_publication_lease(
                &identity,
                Arc::new(ActiveCancellation),
            ))
            .unwrap();

        let (attempted_tx, attempted_rx) = mpsc::sync_channel(1);
        let (selected_tx, selected_rx) = mpsc::sync_channel(1);
        let selecting_owner = Arc::clone(&owner);
        let selecting = thread::spawn(move || {
            attempted_tx.send(()).unwrap();
            selected_tx
                .send(selecting_owner.select_model(None, false))
                .unwrap();
        });
        attempted_rx.recv().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while owner.inner.read().evaluation_publication_writers_waiting == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "selection never reached the lifecycle writer gate"
            );
            thread::yield_now();
        }
        assert!(
            selected_rx.try_recv().is_err(),
            "selection must remain blocked until the publication lease drops"
        );

        drop(lease);
        let selected = selected_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        selecting.join().unwrap();
        assert!(selected.selected_model.is_none());
    }

    #[test]
    fn cancelled_publication_lease_releases_the_lifecycle_reader() {
        struct CancelAfterAcquisition {
            checks: AtomicUsize,
        }

        impl SemanticExecutionAuthority for CancelAfterAcquisition {
            fn interruption(&self) -> Option<SemanticExecutionInterruptionV1> {
                (self.checks.fetch_add(1, Ordering::SeqCst) != 0)
                    .then_some(SemanticExecutionInterruptionV1::Cancelled)
            }
        }

        impl SemanticEvaluationCancellationV1 for CancelAfterAcquisition {}

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

        let identity = owner.verified_evaluation_publication_identity().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let cancelled = runtime.block_on(owner.acquire_verified_evaluation_publication_lease(
            &identity,
            Arc::new(CancelAfterAcquisition {
                checks: AtomicUsize::new(0),
            }),
        ));
        assert!(matches!(cancelled, Err(ModelLifecycleErrorV1::Cancelled)));

        let selected = owner.select_model(None, false).unwrap();
        assert!(selected.selected_model.is_none());
    }

    #[test]
    fn publication_lease_is_send_for_daemon_owned_commit_work() {
        fn assert_send<T: Send>() {}

        assert_send::<SemanticModelLifecycleEvaluationPublicationLeaseV1>();
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
        assert!(!reopened.enqueue_demand_acquisition_if_needed());
        assert_eq!(offline_source.calls.load(Ordering::SeqCst), 0);
    }
