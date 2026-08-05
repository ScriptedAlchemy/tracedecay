    use super::*;
    use std::collections::BTreeMap;
    #[cfg(feature = "semantic-fastembed")]
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::sync::Barrier;
    use std::time::Duration;
    #[cfg(feature = "semantic-fastembed")]
    use std::time::Instant;

    use super::super::manifest::{
        ArtifactMemberPinV1, ArtifactPackageMemberV1, ArtifactProfileKindV1, DeviceClassV1,
        EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
        MODEL_ARTIFACT_MANIFEST_SCHEMA_V1, ModelArtifactManifestPayloadV1, PlatformTargetV1,
        ResourceCeilingV1, RuntimeCompatibilityV1, SemanticMetricV1, Sha256DigestHex,
        TruncationPolicyV1, TruncationSideV1, UpstreamSourceV1,
    };
    use super::super::model_catalog::{CatalogMemberPinV1, CatalogSourceV1};

    struct FixtureSource {
        root: PathBuf,
        calls: AtomicUsize,
    }
    impl ModelMemberSourceV1 for FixtureSource {
        fn fetch_member(
            &self,
            _model: &CatalogedFastEmbedModelV1,
            upstream_path: &str,
            destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let source = self.root.join(upstream_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            }
            fs::copy(&source, destination).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            Ok(())
        }
}
    struct BlockingFixtureSource {
        root: PathBuf,
        calls: AtomicUsize,
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl ModelMemberSourceV1 for BlockingFixtureSource {
        fn fetch_member(
            &self,
            _model: &CatalogedFastEmbedModelV1,
            upstream_path: &str,
            destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered
                    .send(())
                    .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
                self.release
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv()
                    .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            }
            let source = self.root.join(upstream_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::DownloadFailed)?;
            }
            fs::copy(source, destination)
                .map(|_| ())
                .map_err(|_| ModelLifecycleErrorV1::DownloadFailed)
        }
    }

    struct PanickingFixtureSource;

    impl ModelMemberSourceV1 for PanickingFixtureSource {
        fn fetch_member(
            &self,
            _model: &CatalogedFastEmbedModelV1,
            _upstream_path: &str,
            _destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            panic!("fixture acquisition worker panic")
        }
    }

    struct BlockingPanickingFixtureSource {
        entered: SyncSender<()>,
        release: Mutex<Receiver<()>>,
    }

    impl ModelMemberSourceV1 for BlockingPanickingFixtureSource {
        fn fetch_member(
            &self,
            _model: &CatalogedFastEmbedModelV1,
            _upstream_path: &str,
            _destination: &Path,
        ) -> Result<(), ModelLifecycleErrorV1> {
            self.entered
                .send(())
                .expect("blocking panic entered receiver");
            self.release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv()
                .expect("blocking panic release sender");
            panic!("fixture acquisition worker panic after release")
        }
    }

    #[cfg(feature = "semantic-fastembed")]
    struct FixtureHub {
        endpoint: String,
        requests: Arc<AtomicUsize>,
        worker: Option<JoinHandle<()>>,
    }

    #[cfg(feature = "semantic-fastembed")]
    impl FixtureHub {
        fn start(model: &CatalogedFastEmbedModelV1, fixture: &Path) -> Self {
            let members = model
                .members
                .values()
                .map(|member| {
                    (
                        member.upstream_path.clone(),
                        fs::read(fixture.join(&member.path)).unwrap(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let revision = model.source.revision.clone();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(AtomicUsize::new(0));
            let request_counter = Arc::clone(&requests);
            let worker = thread::spawn(move || {
                let expected_requests = members.len() * 2;
                let deadline = Instant::now() + Duration::from_secs(5);
                while request_counter.load(Ordering::SeqCst) < expected_requests
                    && Instant::now() < deadline
                {
                    match listener.accept() {
                        Ok((mut stream, _)) => serve_fixture_hub_request(
                            &mut stream,
                            &members,
                            &revision,
                            &request_counter,
                        ),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("fixture hub accept failed: {error}"),
                    }
                }
            });
            Self {
                endpoint,
                requests,
                worker: Some(worker),
            }
        }

        fn finish(mut self) -> usize {
            self.worker.take().unwrap().join().unwrap();
            self.requests.load(Ordering::SeqCst)
        }
    }

    #[cfg(feature = "semantic-fastembed")]
    fn serve_fixture_hub_request(
        stream: &mut TcpStream,
        members: &BTreeMap<String, Vec<u8>>,
        revision: &str,
        requests: &AtomicUsize,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::with_capacity(1024);
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request);
        let request_line = request.lines().next().unwrap();
        let path = request_line.split_whitespace().nth(1).unwrap();
        let resolve_marker = format!("/resolve/{revision}/");
        let upstream_path = path.split_once(&resolve_marker).map_or_else(
            || panic!("unexpected fixture hub request path: {path}"),
            |(_, upstream_path)| upstream_path,
        );
        let body = members
            .get(upstream_path)
            .unwrap_or_else(|| panic!("unexpected fixture hub request path: {path}"));
        let metadata_request = request.to_ascii_lowercase().contains("range: bytes=0-0");
        let response_body = if metadata_request { &body[..1] } else { body };
        let end = if metadata_request { 0 } else { body.len() - 1 };
        let etag = hex::encode(Sha256::digest(body));
        write!(
            stream,
            "HTTP/1.1 206 Partial Content\r\n\
             Content-Length: {}\r\n\
             Content-Range: bytes 0-{end}/{}\r\n\
             ETag: \"{etag}\"\r\n\
             X-Repo-Commit: {revision}\r\n\
             Connection: close\r\n\r\n",
            response_body.len(),
            body.len(),
        )
        .unwrap();
        stream.write_all(response_body).unwrap();
        stream.flush().unwrap();
        requests.fetch_add(1, Ordering::SeqCst);
    }

    fn join_background_acquisition(
        owner: &SemanticModelLifecycleOwnerV1,
    ) -> Result<(), ModelLifecycleErrorV1> {
        owner
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handle
            .take()
            .expect("background acquisition worker")
            .join()
            .expect("background acquisition worker must not panic")
    }

    fn tiny_catalog(fixture: &Path) -> (FastEmbedModelCatalogV1, String) {
        let members_dir = fixture;
        fs::create_dir_all(members_dir).unwrap();
        let mut members = BTreeMap::new();
        for (role, name, bytes) in [
            ("model", "model.onnx", b"onnx-bytes".as_slice()),
            ("tokenizer", "tokenizer.json", br#"{"ok":true}"#.as_slice()),
            ("config", "config.json", br#"{"dim":8}"#.as_slice()),
            (
                "special_tokens_map",
                "special_tokens_map.json",
                br"{}".as_slice(),
            ),
            (
                "tokenizer_config",
                "tokenizer_config.json",
                br"{}".as_slice(),
            ),
        ] {
            let path = members_dir.join(name);
            fs::write(&path, bytes).unwrap();
            members.insert(
                role.to_owned(),
                CatalogMemberPinV1 {
                    path: name.to_owned(),
                    upstream_path: name.to_owned(),
                    length: bytes.len() as u64,
                    sha256: hex::encode(Sha256::digest(bytes)),
                },
            );
        }
        let model = CatalogedFastEmbedModelV1 {
            model_id: "TinyFixtureModel".to_owned(),
            fastembed_enum: "TinyFixtureModel".to_owned(),
            model_code: "tracedecay/tiny-fixture".to_owned(),
            source: CatalogSourceV1 {
                upstream: "https://example.invalid/tracedecay/tiny-fixture".to_owned(),
                revision: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                license: "Apache-2.0".to_owned(),
                license_url: "https://www.apache.org/licenses/LICENSE-2.0".to_owned(),
                provenance: "https://example.invalid/tracedecay/tiny-fixture/tree/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
            },
            expected_dimensions: 8,
            max_length: 32,
            members,
        };
        // Production validate requires default Jina; for unit tests build a
        // catalog that includes both the default pin and the tiny fixture.
        let mut catalog = FastEmbedModelCatalogV1::production();
        catalog.models.push(model.clone());
        (catalog, model.model_id)
    }

    fn tiny_manifest(model: &CatalogedFastEmbedModelV1) -> ModelArtifactManifestV1 {
        let role = |name: &str| match name {
            "model" => ArtifactMemberRoleV1::Model,
            "tokenizer" => ArtifactMemberRoleV1::Tokenizer,
            "config" => ArtifactMemberRoleV1::Config,
            "special_tokens_map" => ArtifactMemberRoleV1::SpecialTokensMap,
            "tokenizer_config" => ArtifactMemberRoleV1::TokenizerConfig,
            _ => unreachable!(),
        };
        let members: Vec<_> = model
            .members
            .iter()
            .map(|(name, pin)| ArtifactPackageMemberV1 {
                role: role(name),
                path: pin.path.clone(),
                digest: Sha256DigestHex::new(pin.sha256.clone()).unwrap(),
                byte_length: pin.length,
            })
            .collect();
        let member = |role| members.iter().find(|member| member.role == role).unwrap();
        let model_member = member(ArtifactMemberRoleV1::Model);
        ModelArtifactManifestV1 {
            payload: ModelArtifactManifestPayloadV1 {
                schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_owned(),
                artifact_id: model.model_id.clone(),
                profile_kind: ArtifactProfileKindV1::Embedding,
                spdx_license: model.source.license.clone(),
                model_member: ArtifactMemberPinV1 {
                    digest: model_member.digest.clone(),
                    byte_length: model_member.byte_length,
                },
                tokenizer_digest: member(ArtifactMemberRoleV1::Tokenizer).digest.clone(),
                config_digest: member(ArtifactMemberRoleV1::Config).digest.clone(),
                query_instruction_digest: None,
                document_instruction_digest: None,
                members,
                dimensions: model.expected_dimensions,
                metric: SemanticMetricV1::Cosine,
                normalization: EmbeddingNormalizationV1::L2,
                pooling: EmbeddingPoolingV1::Mean,
                truncation: TruncationPolicyV1 {
                    side: TruncationSideV1::Right,
                    max_length: model.max_length,
                },
                precision: EmbeddingPrecisionV1::Fp32,
                runtime: RuntimeCompatibilityV1 {
                    runtime: super::super::artifact_store::FASTEMBED_RUNTIME_FAMILY_V1.to_owned(),
                    build_revision:
                        super::super::artifact_store::FASTEMBED_RUNTIME_BUILD_REVISION_V1.to_owned(),
                    platforms: vec![PlatformTargetV1 {
                        os: std::env::consts::OS.to_owned(),
                        arch: std::env::consts::ARCH.to_owned(),
                    }],
                },
                device: DeviceClassV1::Cpu,
                resource_ceiling: ResourceCeilingV1 {
                    max_model_bytes: 1_024,
                    max_tokenizer_bytes: 1_024,
                    max_resident_bytes: 4_096,
                    max_threads: 1,
                    max_batch_size: 1,
                    max_sequence_length: model.max_length,
                    load_deadline_ms: 1_000,
                },
                upstream: UpstreamSourceV1 {
                    name: model.model_code.clone(),
                    version: "fixture".to_owned(),
                    revision: model.source.revision.clone(),
                },
            },
        }
    }

    fn scoped_hub_source(root: &Path) -> Arc<dyn ModelMemberSourceV1> {
        Arc::new(HfHubModelMemberSourceV1::new(
            root.join(HF_HUB_CACHE_DIRECTORY_V1),
        ))
    }

    #[test]
    fn default_selection_is_selected_not_downloaded_and_offline_safe() {
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open_default(root.path()).unwrap();
        let status = owner.status();
        assert_eq!(
            status.selected_model.as_deref(),
            Some(DEFAULT_FASTEMBED_MODEL_ID)
        );
        assert!(!status.auto_download);
        assert!(status.semantics_omitted);
        assert!(matches!(
            status.state,
            Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
        ));
        assert!(status.remediation.retry);
        assert!(!owner.enqueue_startup_acquisition_if_needed());
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn fresh_hub_acquisition_downloads_then_reuses_private_cache_offline() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let model = catalog.get(&model_id).unwrap().clone();
        let online_root = tempfile::tempdir().unwrap();
        let hub = FixtureHub::start(&model, fixture.path());
        let cache = online_root.path().join(HF_HUB_CACHE_DIRECTORY_V1);
        let online_source = Arc::new(HfHubModelMemberSourceV1::new_for_tests(
            cache.clone(),
            Some(hub.endpoint.clone()),
            false,
        ));
        let online =
            SemanticModelLifecycleOwnerV1::open(online_root.path(), catalog.clone(), online_source)
                .unwrap();

        online.select_model(Some(&model_id), true).unwrap();
        assert!(online.enqueue_startup_acquisition_if_needed());
        join_background_acquisition(&online).expect("online acquisition must complete");
        assert!(matches!(
            online.status().state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
        assert_eq!(hub.finish(), model.members.len() * 2);

        let offline_root = tempfile::tempdir().unwrap();
        let offline_source = Arc::new(HfHubModelMemberSourceV1::new_for_tests(cache, None, true));
        let offline =
            SemanticModelLifecycleOwnerV1::open(offline_root.path(), catalog, offline_source)
                .unwrap();
        offline.select_model(Some(&model_id), true).unwrap();
        offline.acquire_blocking_for_tests().unwrap();

        assert!(matches!(
            offline.status().state,
            Some(SemanticModelLifecycleStateV1::Installed { .. })
        ));
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn offline_cache_miss_reports_failed_reason_and_omits_semantics() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(HfHubModelMemberSourceV1::new_for_tests(
            root.path().join(HF_HUB_CACHE_DIRECTORY_V1),
            None,
            true,
        ));
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();

        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.enqueue_startup_acquisition_if_needed());
        assert!(matches!(
            join_background_acquisition(&owner),
            Err(
                ModelLifecycleErrorV1::DownloadFailed
                    | ModelLifecycleErrorV1::DownloadFailedWithReason(_)
            )
        ));

        let status = owner.status();
        let Some(SemanticModelLifecycleStateV1::Failed {
            detail, retryable, ..
        }) = status.state
        else {
            panic!("offline cache miss must report failed acquisition: {status:?}");
        };
        assert!(retryable);
        assert!(detail.contains("offline"));
        assert!(detail.contains("config.json"));
        assert!(status.semantics_omitted);
    }

    #[test]
    fn store_failure_does_not_leave_acquisition_stuck_in_progress() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let source = Arc::new(FixtureSource {
            root: fixture.path().to_path_buf(),
            calls: AtomicUsize::new(0),
        });
        let owner = SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        fs::remove_dir(root.path().join("staging")).unwrap();
        fs::write(root.path().join("staging"), b"not a directory").unwrap();

        assert_eq!(
            owner.acquire_blocking_for_tests().unwrap_err(),
            ModelLifecycleErrorV1::StoreUnavailable
        );
        let status = owner.status();
        let Some(SemanticModelLifecycleStateV1::Failed {
            detail, retryable, ..
        }) = status.state
        else {
            panic!("store failure must terminate acquisition state: {status:?}");
        };
        assert!(retryable);
        assert_eq!(detail, ModelLifecycleErrorV1::StoreUnavailable.to_string());
        assert!(status.semantics_omitted);
    }

    #[test]
    fn cancellation_joins_background_acquisition_without_installing() {
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
        let owner = Arc::new(
            SemanticModelLifecycleOwnerV1::open(root.path(), catalog, source).unwrap(),
        );
        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.enqueue_startup_acquisition_if_needed());
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background acquisition entered fixture source");

        let cancel_owner = Arc::clone(&owner);
        let (joined_tx, joined_rx) = mpsc::sync_channel(1);
        let canceller = thread::spawn(move || {
            joined_tx
                .send(
                    cancel_owner
                        .cancel_and_join_background_acquisition_until(
                            std::time::Instant::now() + Duration::from_secs(1),
                        )
                        .and_then(|joined| {
                            joined
                                .then_some(())
                                .ok_or(ModelLifecycleErrorV1::WorkerJoinFailed)
                        }),
                )
                .expect("report cancellation outcome");
        });
        assert!(
            joined_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "cancellation must join the still-running acquisition worker"
        );
        release_tx.send(()).expect("release fixture source");
        joined_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("joined cancellation outcome")
            .expect("cancel and join background acquisition");
        canceller.join().expect("cancellation caller");

        assert!(
            owner
                .worker
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .handle
                .is_none(),
            "joined worker handle must not remain mounted"
        );
        assert!(
            !matches!(
                owner.status().state,
                Some(
                    SemanticModelLifecycleStateV1::Installed { .. }
                        | SemanticModelLifecycleStateV1::Ready { .. }
                )
            ),
            "cancelled acquisition must never install or ready the model"
        );
    }

    #[test]
    fn selection_change_supersedes_blocked_acquisition_before_mutation() {
        for next_model in [Some(DEFAULT_FASTEMBED_MODEL_ID), None] {
            let fixture = tempfile::tempdir().unwrap();
            let (catalog, model_id) = tiny_catalog(fixture.path());
            let root = tempfile::tempdir().unwrap();
            let (entered_tx, entered_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let owner = SemanticModelLifecycleOwnerV1::open(
                root.path(),
                catalog,
                Arc::new(BlockingFixtureSource {
                    root: fixture.path().to_path_buf(),
                    calls: AtomicUsize::new(0),
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap();
            owner.select_model(Some(&model_id), true).unwrap();
            assert!(owner.enqueue_startup_acquisition_if_needed());
            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("background acquisition entered fixture source");

            owner.select_model(next_model, false).unwrap();
            release_tx.send(()).expect("release fixture source");
            let acquisition = owner
                .worker
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .handle
                .take()
                .expect("background acquisition worker")
                .join()
                .expect("background acquisition worker");
            assert_eq!(acquisition, Err(ModelLifecycleErrorV1::Cancelled));

            let status = owner.status();
            assert_eq!(status.selected_model.as_deref(), next_model);
            assert!(
                next_model.is_some_and(|model| {
                    matches!(
                        status.state.as_ref(),
                        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded {
                            model_id,
                            ..
                        }) if model_id == model
                    )
                }) || next_model.is_none() && status.state.is_none(),
                "superseded acquisition must not overwrite the new selection: {status:?}"
            );
        }
    }

    #[test]
    fn automatic_spawn_revalidates_auto_download_under_worker_lock() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            Arc::new(FixtureSource {
                root: fixture.path().to_path_buf(),
                calls: AtomicUsize::new(0),
            }),
        )
        .unwrap();
        owner.select_model(Some(&model_id), false).unwrap();

        assert!(
            !owner.spawn_acquire(true, Some(&model_id)),
            "automatic acquisition must revalidate the selection's auto-download policy"
        );
        assert!(
            !owner.spawn_acquire(false, Some(DEFAULT_FASTEMBED_MODEL_ID)),
            "explicit retry must not acquire a selection other than the requested model"
        );
    }

    #[test]
    fn terminal_worker_join_outcome_is_retained_across_shutdown_retries() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let owner = SemanticModelLifecycleOwnerV1::open(
            root.path(),
            catalog,
            Arc::new(PanickingFixtureSource),
        )
        .unwrap();
        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.enqueue_startup_acquisition_if_needed());
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !owner
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handle
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "panicking acquisition worker did not finish"
            );
            thread::yield_now();
        }

        for _ in 0..2 {
            assert_eq!(
                owner.cancel_and_join_background_acquisition_until(
                    std::time::Instant::now() + Duration::from_secs(1),
                ),
                Err(ModelLifecycleErrorV1::WorkerJoinFailed),
            );
        }
        assert_eq!(
            owner.resolve_background_acquisition_outcome(),
            Some(ModelLifecycleErrorV1::WorkerJoinFailed),
        );
        assert_eq!(
            owner.cancel_and_join_background_acquisition_until(
                std::time::Instant::now() + Duration::from_secs(1),
            ),
            Ok(true),
        );
    }

    #[test]
    fn concurrent_shutdown_retries_cannot_report_clean_while_join_is_pending() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let owner = Arc::new(
            SemanticModelLifecycleOwnerV1::open(
                root.path(),
                catalog,
                Arc::new(BlockingPanickingFixtureSource {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap(),
        );
        owner.select_model(Some(&model_id), true).unwrap();
        assert!(owner.enqueue_startup_acquisition_if_needed());
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background acquisition entered fixture source");

        let start = Arc::new(Barrier::new(3));
        let (result_tx, result_rx) = mpsc::sync_channel(2);
        let callers = (0..2)
            .map(|_| {
                let owner = Arc::clone(&owner);
                let start = Arc::clone(&start);
                let result_tx = result_tx.clone();
                thread::spawn(move || {
                    start.wait();
                    result_tx
                        .send(owner.cancel_and_join_background_acquisition_until(
                            std::time::Instant::now() + Duration::from_secs(1),
                        ))
                        .expect("shutdown result receiver");
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        assert!(
            result_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "no shutdown caller may report clean while its peer is joining the worker"
        );
        release_tx.send(()).expect("release fixture source");
        for _ in 0..2 {
            assert_eq!(
                result_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("shutdown result"),
                Err(ModelLifecycleErrorV1::WorkerJoinFailed),
            );
        }
        for caller in callers {
            caller.join().expect("shutdown caller");
        }
    }

    #[test]
    fn cancellation_returns_typed_cancelled_without_publishing_failed_state() {
        let fixture = tempfile::tempdir().unwrap();
        let (catalog, model_id) = tiny_catalog(fixture.path());
        let root = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let owner = Arc::new(
            SemanticModelLifecycleOwnerV1::open(
                root.path(),
                catalog,
                Arc::new(BlockingFixtureSource {
                    root: fixture.path().to_path_buf(),
                    calls: AtomicUsize::new(0),
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .unwrap(),
        );
        owner.select_model(Some(&model_id), false).unwrap();
        let acquisition_owner = Arc::clone(&owner);
        let acquisition = thread::spawn(move || acquisition_owner.acquire_blocking_for_tests());
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocking acquisition entered fixture source");

        owner.cancel_background_acquisition();
        release_tx.send(()).expect("release fixture source");

        assert_eq!(
            acquisition.join().expect("acquisition worker"),
            Err(ModelLifecycleErrorV1::Cancelled),
        );
        assert!(!matches!(
            owner.status().state,
            Some(SemanticModelLifecycleStateV1::Failed { .. }),
        ));
    }

    #[test]
    fn cancellation_and_installed_publication_are_serialized_by_epoch() {
        let control = Arc::new(AcquisitionControlV1::default());
        let epoch = control.begin_epoch();
        let (publication_entered_tx, publication_entered_rx) = mpsc::sync_channel(1);
        let (release_publication_tx, release_publication_rx) = mpsc::sync_channel(1);
        let installed = Arc::new(AtomicBool::new(false));
        let installed_by_publication = Arc::clone(&installed);
        let publication = thread::spawn(move || {
            epoch.while_active(|| {
                publication_entered_tx
                    .send(())
                    .expect("publication-entered receiver");
                release_publication_rx
                    .recv()
                    .expect("release-publication sender");
                installed_by_publication.store(true, Ordering::SeqCst);
                Ok(())
            })
        });
        publication_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("publication owns epoch");

        let cancellation_control = Arc::clone(&control);
        let (cancelled_tx, cancelled_rx) = mpsc::sync_channel(1);
        let cancellation = thread::spawn(move || {
            cancellation_control.cancel_current();
            cancelled_tx.send(()).expect("cancelled receiver");
        });
        assert!(
            cancelled_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "cancellation must wait while Installed publication owns the epoch"
        );
        release_publication_tx
            .send(())
            .expect("release publication");
        publication
            .join()
            .expect("publication worker")
            .expect("Installed publication");
        cancelled_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation completed");
        cancellation.join().expect("cancellation worker");
        assert!(installed.load(Ordering::SeqCst));

        let cancelled_epoch = control.begin_epoch();
        control.cancel_current();
        assert_eq!(
            cancelled_epoch.while_active(|| {
                installed.store(false, Ordering::SeqCst);
                Ok(())
            }),
            Err(ModelLifecycleErrorV1::Cancelled),
        );
        assert!(installed.load(Ordering::SeqCst));
    }
