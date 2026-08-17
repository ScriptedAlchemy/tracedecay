/// Owns selection, background acquisition, and remediation for one data root.
pub struct SemanticModelLifecycleOwnerV1 {
    root: PathBuf,
    catalog: FastEmbedModelCatalogV1,
    source: Arc<dyn ModelMemberSourceV1>,
    artifact_store: ModelArtifactStore,
    inner: Arc<LifecyclePublicationGateV1>,
    worker: Mutex<AcquisitionWorkerStateV1>,
    acquisition: Arc<AcquisitionControlV1>,
    verified_ready: watch::Sender<SemanticLifecycleVerifiedReadyEventV1>,
}
struct LifecycleInner {
    durable: DurableLifecycleV1,
    evaluation_publication_readers: usize,
    evaluation_publication_writers_waiting: usize,
}

/// Exact lifecycle identity that a semantic evaluation may pin through its
/// durable publication. The owner alone mints it from the lifecycle state and
/// verified-ready event, preventing callers from constructing a lookalike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticModelLifecyclePublicationIdentityV1 {
    state: SemanticModelLifecycleStateV1,
    verified_ready_epoch: u64,
    verified_ready_artifact_digest: String,
}

impl SemanticModelLifecyclePublicationIdentityV1 {
    pub fn state(&self) -> &SemanticModelLifecycleStateV1 {
        &self.state
    }
}

/// A Send-safe read lease over the canonical lifecycle state. Dropping the
/// lease releases state-changing selection, install, and remediation work.
pub struct SemanticModelLifecycleEvaluationPublicationLeaseV1 {
    gate: Arc<LifecyclePublicationGateV1>,
}

impl Drop for SemanticModelLifecycleEvaluationPublicationLeaseV1 {
    fn drop(&mut self) {
        let mut guard = self.gate.read();
        let Some(readers) = guard.evaluation_publication_readers.checked_sub(1) else {
            return;
        };
        guard.evaluation_publication_readers = readers;
        if guard.evaluation_publication_readers == 0 {
            self.gate.readers_released.notify_all();
        }
    }
}

/// One lifecycle mutex also serves as the reader/writer publication gate. A
/// lease owns only an `Arc`, never a thread-affine mutex guard, so it remains
/// safe to carry through asynchronous daemon publication.
struct LifecyclePublicationGateV1 {
    inner: Mutex<LifecycleInner>,
    readers_released: Condvar,
}

impl LifecyclePublicationGateV1 {
    fn new(durable: DurableLifecycleV1) -> Self {
        Self {
            inner: Mutex::new(LifecycleInner {
                durable,
                evaluation_publication_readers: 0,
                evaluation_publication_writers_waiting: 0,
            }),
            readers_released: Condvar::new(),
        }
    }

    fn read(&self) -> MutexGuard<'_, LifecycleInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn writer(&self) -> LifecycleWriteGuardV1<'_> {
        let mut guard = self.read();
        guard.evaluation_publication_writers_waiting = guard
            .evaluation_publication_writers_waiting
            .saturating_add(1);
        while guard.evaluation_publication_readers != 0 {
            guard = self
                .readers_released
                .wait(guard)
                .unwrap_or_else(PoisonError::into_inner);
        }
        guard.evaluation_publication_writers_waiting = guard
            .evaluation_publication_writers_waiting
            .saturating_sub(1);
        LifecycleWriteGuardV1 { guard }
    }

    fn try_acquire_reader(
        self: &Arc<Self>,
        expected: &SemanticModelLifecyclePublicationIdentityV1,
        verified_ready: &watch::Sender<SemanticLifecycleVerifiedReadyEventV1>,
    ) -> Result<SemanticModelLifecycleEvaluationPublicationLeaseV1, ModelLifecycleErrorV1> {
        let mut guard = self.read();
        if guard.evaluation_publication_writers_waiting != 0
            || lifecycle_publication_identity(&guard, verified_ready)? != expected.clone()
        {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        guard.evaluation_publication_readers = guard
            .evaluation_publication_readers
            .checked_add(1)
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        Ok(SemanticModelLifecycleEvaluationPublicationLeaseV1 {
            gate: Arc::clone(self),
        })
    }
}

struct LifecycleWriteGuardV1<'a> {
    guard: MutexGuard<'a, LifecycleInner>,
}

impl std::ops::Deref for LifecycleWriteGuardV1<'_> {
    type Target = LifecycleInner;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl std::ops::DerefMut for LifecycleWriteGuardV1<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

fn lifecycle_publication_identity(
    guard: &LifecycleInner,
    verified_ready: &watch::Sender<SemanticLifecycleVerifiedReadyEventV1>,
) -> Result<SemanticModelLifecyclePublicationIdentityV1, ModelLifecycleErrorV1> {
    let state = guard
        .durable
        .state
        .clone()
        .ok_or(ModelLifecycleErrorV1::Rejected)?;
    let ready = verified_ready.borrow().clone();
    let artifact_digest = ready
        .artifact_digest
        .ok_or(ModelLifecycleErrorV1::Rejected)?;
    if artifact_digest != state.artifact_digest() {
        return Err(ModelLifecycleErrorV1::Rejected);
    }
    Ok(SemanticModelLifecyclePublicationIdentityV1 {
        state,
        verified_ready_epoch: ready.epoch,
        verified_ready_artifact_digest: artifact_digest,
    })
}

fn verified_ready_artifact_digest(state: &SemanticModelLifecycleStateV1) -> Option<String> {
    match state {
        SemanticModelLifecycleStateV1::Installed {
            artifact_digest, ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            artifact_digest, ..
        } => Some(artifact_digest.clone()),
        _ => None,
    }
}

fn publish_verified_ready_event(
    events: &watch::Sender<SemanticLifecycleVerifiedReadyEventV1>,
    guard: &LifecycleInner,
) {
    let artifact_digest = guard
        .durable
        .state
        .as_ref()
        .and_then(verified_ready_artifact_digest);
    let Some(artifact_digest) = artifact_digest else {
        return;
    };
    events.send_modify(|current| {
        current.epoch = current.epoch.saturating_add(1);
        current.artifact_digest = Some(artifact_digest);
    });
}

#[derive(Default)]
struct AcquisitionWorkerStateV1 {
    handle: Option<JoinHandle<Result<(), ModelLifecycleErrorV1>>>,
    outcome: Option<ModelLifecycleErrorV1>,
}

impl AcquisitionWorkerStateV1 {
    fn join_and_retain(
        &mut self,
        worker: JoinHandle<Result<(), ModelLifecycleErrorV1>>,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let result = join_acquisition_worker(worker);
        if let Err(error) = &result {
            self.outcome = Some(error.clone());
        }
        result
    }

    fn reap_finished(&mut self) -> Result<(), ModelLifecycleErrorV1> {
        let worker = match self.handle.as_ref() {
            Some(worker) if worker.is_finished() => self.handle.take(),
            _ => None,
        };
        match worker {
            Some(worker) => self.join_and_retain(worker),
            None => Ok(()),
        }
    }
}

impl SemanticModelLifecycleOwnerV1 {
    pub fn open(
        root: impl Into<PathBuf>,
        catalog: FastEmbedModelCatalogV1,
        source: Arc<dyn ModelMemberSourceV1>,
    ) -> Result<Self, ModelLifecycleErrorV1> {
        catalog.validate()?;
        let root = root.into();
        fs::create_dir_all(root.join("staging"))
            .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        fs::create_dir_all(root.join("installs"))
            .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        let artifact_store = ModelArtifactStore::open(
            root.join("verified-artifacts"),
            RetentionPolicyV1 {
                grace_seconds: 7 * 24 * 60 * 60,
            },
        )?;
        let durable = load_or_default_durable(&root, &catalog)?;
        let initial_ready = SemanticLifecycleVerifiedReadyEventV1 {
            epoch: 0,
            artifact_digest: durable
                .state
                .as_ref()
                .and_then(verified_ready_artifact_digest),
        };
        let (verified_ready, _) = watch::channel(initial_ready);
        let owner = Self {
            root,
            catalog,
            source,
            artifact_store,
            inner: Arc::new(LifecyclePublicationGateV1::new(durable)),
            worker: Mutex::new(AcquisitionWorkerStateV1::default()),
            acquisition: Arc::new(AcquisitionControlV1::default()),
            verified_ready,
        };
        let durable = owner.inner.read().durable.clone();
        owner.reconcile_embedding_artifact_leases(&durable, current_unix_seconds()?)?;
        Ok(owner)
    }

    pub fn open_default(root: impl Into<PathBuf>) -> Result<Self, ModelLifecycleErrorV1> {
        let root = root.into();
        let source = Arc::new(HfHubModelMemberSourceV1::new(
            root.join(HF_HUB_CACHE_DIRECTORY_V1),
        ));
        Self::open(root, FastEmbedModelCatalogV1::production(), source)
    }

    pub fn catalog(&self) -> &FastEmbedModelCatalogV1 {
        &self.catalog
    }

    pub fn verified_ready_events(&self) -> watch::Receiver<SemanticLifecycleVerifiedReadyEventV1> {
        self.verified_ready.subscribe()
    }

    /// Mint the exact lifecycle identity that a semantic evaluation may retain
    /// through its final configuration publication.
    pub fn verified_evaluation_publication_identity(
        &self,
    ) -> Result<SemanticModelLifecyclePublicationIdentityV1, ModelLifecycleErrorV1> {
        let guard = self.inner.read();
        lifecycle_publication_identity(&guard, &self.verified_ready)
    }

    /// Acquire the canonical lifecycle read side after checking the same
    /// identity that was observed for evaluation. State-changing lifecycle
    /// operations take the matching writer side and therefore cannot pass this
    /// point until the returned lease is dropped.
    pub async fn acquire_verified_evaluation_publication_lease(
        &self,
        expected: &SemanticModelLifecyclePublicationIdentityV1,
        cancellation: Arc<dyn crate::SemanticEvaluationCancellationV1>,
    ) -> Result<SemanticModelLifecycleEvaluationPublicationLeaseV1, ModelLifecycleErrorV1> {
        if crate::SemanticExecutionAuthority::interruption(cancellation.as_ref()).is_some() {
            return Err(ModelLifecycleErrorV1::Cancelled);
        }
        let lease = self
            .inner
            .try_acquire_reader(expected, &self.verified_ready)?;
        if crate::SemanticExecutionAuthority::interruption(cancellation.as_ref()).is_some() {
            drop(lease);
            return Err(ModelLifecycleErrorV1::Cancelled);
        }
        Ok(lease)
    }

    /// Re-admit the independently evaluated reranker selected by exact
    /// compatibility pins. No catalog lookup, network access, or ambient
    /// cache participates in this mount.
    pub fn mount_reranker(
        &self,
        pins: RerankCompatibilityPinsV1,
    ) -> Result<super::rerank_adapter::ProductionCodeRerankAuthorityV1, ModelLifecycleErrorV1> {
        let digest = pins
            .artifact_manifest_digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(ModelLifecycleErrorV1::VerificationFailed)
            .and_then(|digest| {
                super::manifest::Sha256DigestHex::new(digest.to_owned())
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)
            })?;
        let artifact = self
            .artifact_store
            .admit_leased_for_runtime_by_digest(
                &digest,
                &RuntimeEnvironmentV1::detect_fastembed_process()
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?,
                RERANKER_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                current_unix_seconds()?,
            )
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        super::rerank_adapter::ProductionCodeRerankAuthorityV1::from_admitted(artifact, pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)
    }

    pub fn import_local_reranker_artifact(
        &self,
        pins: RerankCompatibilityPinsV1,
        manifest: &ModelArtifactManifestV1,
        source: &Path,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        super::rerank_adapter::validate_reranker_manifest_pins(manifest, &pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let record = self
            .artifact_store
            .import_local_directory(manifest, source, now_unix)?;
        self.publish_reranker_artifact(pins, record, now_unix)
    }

    pub fn import_configured_https_reranker_artifact(
        &self,
        pins: RerankCompatibilityPinsV1,
        manifest: &ModelArtifactManifestV1,
        source: &ConfiguredHttpsArtifactSourceV1,
        transport: &dyn ExplicitHttpsArtifactTransportV1,
        resume_staging_id: Option<&str>,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        super::rerank_adapter::validate_reranker_manifest_pins(manifest, &pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let record = self.artifact_store.import_configured_https(
            manifest,
            source,
            transport,
            resume_staging_id,
            now_unix,
        )?;
        self.publish_reranker_artifact(pins, record, now_unix)
    }

    fn publish_reranker_artifact(
        &self,
        pins: RerankCompatibilityPinsV1,
        record: ArtifactInventoryRecordV1,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        let environment = RuntimeEnvironmentV1::detect_fastembed_process()
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let admitted = self
            .artifact_store
            .admit_for_runtime_by_digest(&record.artifact_digest, &environment)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        super::rerank_adapter::ProductionCodeRerankAuthorityV1::from_admitted(admitted, pins)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        self.artifact_store.activate_artifact_with_rollback(
            &record.artifact_digest,
            RERANKER_ACTIVE_LEASE_ID_V1,
            RERANKER_ROLLBACK_LEASE_ID_V1,
            now_unix,
        )?;
        self.reranker_artifact_status()
    }

    pub fn reranker_artifact_status(
        &self,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        let now_unix = current_unix_seconds()?;
        Ok(RerankerArtifactLifecycleStatusV1 {
            active_artifact_digest: self.artifact_store.artifact_digest_for_lease(
                RERANKER_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                now_unix,
            )?,
            rollback_artifact_digest: self.artifact_store.artifact_digest_for_lease(
                RERANKER_ROLLBACK_LEASE_ID_V1,
                ArtifactLeaseKindV1::Rollback,
                now_unix,
            )?,
        })
    }

    pub fn rollback_reranker_artifact(
        &self,
        now_unix: u64,
    ) -> Result<RerankerArtifactLifecycleStatusV1, ModelLifecycleErrorV1> {
        let rollback = self
            .reranker_artifact_status()?
            .rollback_artifact_digest
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        self.artifact_store.activate_artifact_with_rollback(
            &rollback,
            RERANKER_ACTIVE_LEASE_ID_V1,
            RERANKER_ROLLBACK_LEASE_ID_V1,
            now_unix,
        )?;
        self.reranker_artifact_status()
    }

    pub fn mounted_shared() -> Option<Arc<Self>> {
        SHARED_LIFECYCLE_OWNER.get().cloned().flatten()
    }

    pub fn run_daemon_artifact_gc(
        &self,
        now_unix: u64,
    ) -> Result<Vec<GcReceiptV1>, ModelLifecycleErrorV1> {
        let expires_at_unix = now_unix
            .checked_add(ARTIFACT_GC_LEASE_SECONDS)
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        let lease = self.artifact_store.acquire_daemon_gc_lease(
            format!("daemon:{}:{now_unix}", std::process::id()),
            expires_at_unix,
            now_unix,
        )?;
        self.artifact_store
            .gc_with_daemon_lease(&lease, now_unix)
            .map_err(Into::into)
    }

    /// Explicitly import a complete local package through the verified store.
    /// Selection alone never invokes this operation.
    pub fn import_local_artifact(
        &self,
        model_id: &str,
        manifest: &ModelArtifactManifestV1,
        source: &Path,
        now_unix: u64,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let model = self
            .catalog
            .get(model_id)
            .ok_or(CatalogErrorV1::UnknownModel)?;
        verify_catalog_manifest(model, manifest)?;
        let record = self
            .artifact_store
            .import_local_directory(manifest, source, now_unix)?;
        self.publish_explicit_import(model, record, now_unix)
    }

    /// Explicitly import or resume from a configured immutable HTTPS source.
    /// The typed transport is supplied by the user-action boundary and is not
    /// retained by startup, query, or runtime paths.
    pub fn import_configured_https_artifact(
        &self,
        model_id: &str,
        manifest: &ModelArtifactManifestV1,
        source: &ConfiguredHttpsArtifactSourceV1,
        transport: &dyn ExplicitHttpsArtifactTransportV1,
        resume_staging_id: Option<&str>,
        now_unix: u64,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let model = self
            .catalog
            .get(model_id)
            .ok_or(CatalogErrorV1::UnknownModel)?;
        verify_catalog_manifest(model, manifest)?;
        let record = self.artifact_store.import_configured_https(
            manifest,
            source,
            transport,
            resume_staging_id,
            now_unix,
        )?;
        self.publish_explicit_import(model, record, now_unix)
    }

    fn publish_explicit_import(
        &self,
        model: &CatalogedFastEmbedModelV1,
        record: ArtifactInventoryRecordV1,
        now_unix: u64,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let mut guard = self.inner.writer();
        let prior_durable = guard.durable.clone();
        self.artifact_store.activate_artifact_with_rollback(
            &record.artifact_digest,
            EMBEDDING_ACTIVE_LEASE_ID_V1,
            EMBEDDING_ROLLBACK_LEASE_ID_V1,
            now_unix,
        )?;
        let install_path = self
            .artifact_store
            .installed_directory(&record.artifact_digest);
        if let Some(previous @ SemanticModelLifecycleStateV1::Ready { .. }) =
            guard.durable.state.clone()
        {
            guard.durable.previous_ready = Some(previous);
        }
        guard.durable.selected_model = Some(model.model_id.clone());
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: record.artifact_digest.to_string(),
            install_path,
        });
        if let Err(error) = persist_durable(&self.root, &guard.durable) {
            guard.durable = prior_durable.clone();
            self.reconcile_embedding_artifact_leases(&prior_durable, now_unix)?;
            return Err(error);
        }
        publish_verified_ready_event(&self.verified_ready, &guard);
        drop(guard);
        Ok(self.status())
    }

    pub fn status(&self) -> SemanticModelLifecycleStatusV1 {
        let guard = self.inner.read();
        let mut remediation = guard.durable.state.as_ref().map_or(
            SemanticModelRemediationV1 {
                retry: false,
                remove: false,
                rollback: false,
            },
            SemanticModelLifecycleStateV1::remediation,
        );
        if matches!(
            guard.durable.previous_ready.as_ref(),
            Some(SemanticModelLifecycleStateV1::Ready { .. })
        ) {
            remediation.rollback = true;
        }
        let semantics_omitted = guard
            .durable
            .state
            .as_ref()
            .is_none_or(SemanticModelLifecycleStateV1::omits_semantics);
        SemanticModelLifecycleStatusV1 {
            selected_model: guard.durable.selected_model.clone(),
            auto_download: guard.durable.auto_download,
            catalog_model_ids: self.catalog.model_ids().map(str::to_owned).collect(),
            state: guard.durable.state.clone(),
            remediation,
            semantics_omitted,
        }
    }

    /// Apply a settings selection. `None` disables semantics without download.
    pub fn select_model(
        &self,
        model_id: Option<&str>,
        auto_download: bool,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let selected = match model_id {
            Some(model_id) => {
                let model = self
                    .catalog
                    .get(model_id)
                    .ok_or(CatalogErrorV1::UnknownModel)?;
                Some((model, self.re_admit_durable_selection(model)?))
            }
            None => None,
        };
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        self.cancel_background_acquisition();
        let _ = worker.reap_finished();
        let mut guard = self.inner.writer();
        guard.durable.auto_download = auto_download;
        match selected {
            None => {
                guard.durable.selected_model = None;
                guard.durable.state = None;
            }
            Some((model, durable_selection)) => {
                let digest = catalog_package_digest(model);
                guard.durable.selected_model = Some(model.model_id.clone());
                if let Some(state) = durable_selection {
                    guard.durable.state = Some(state);
                } else if let Some(path) = existing_install_path(&self.root, model, &digest) {
                    guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
                        model_id: model.model_id.clone(),
                        revision: model.source.revision.clone(),
                        artifact_digest: digest,
                        install_path: path,
                    });
                } else {
                    guard.durable.state =
                        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded {
                            model_id: model.model_id.clone(),
                            revision: model.source.revision.clone(),
                            artifact_digest: digest,
                        });
                }
            }
        }
        persist_durable(&self.root, &guard.durable)?;
        publish_verified_ready_event(&self.verified_ready, &guard);
        drop(guard);
        drop(worker);
        Ok(self.status())
    }

    fn re_admit_durable_selection(
        &self,
        model: &CatalogedFastEmbedModelV1,
    ) -> Result<Option<SemanticModelLifecycleStateV1>, ModelLifecycleErrorV1> {
        let state = {
            let guard = self.inner.read();
            guard.durable.state.clone()
        };
        let (was_ready, artifact_digest) = match state {
            Some(SemanticModelLifecycleStateV1::Installed {
                model_id,
                revision,
                artifact_digest,
                ..
            }) if model_id == model.model_id && revision == model.source.revision => {
                (false, artifact_digest)
            }
            Some(SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                ..
            }) if model_id == model.model_id && revision == model.source.revision => {
                (true, artifact_digest)
            }
            _ => return Ok(None),
        };
        let catalog_digest = catalog_package_digest(model);
        if artifact_digest == catalog_digest
            && let Some(install_path) = existing_install_path(&self.root, model, &catalog_digest)
        {
            return Ok(Some(if was_ready {
                SemanticModelLifecycleStateV1::Ready {
                    model_id: model.model_id.clone(),
                    revision: model.source.revision.clone(),
                    artifact_digest: catalog_digest,
                    install_path,
                }
            } else {
                SemanticModelLifecycleStateV1::Installed {
                    model_id: model.model_id.clone(),
                    revision: model.source.revision.clone(),
                    artifact_digest: catalog_digest,
                    install_path,
                }
            }));
        }
        let digest = super::manifest::Sha256DigestHex::new(artifact_digest.clone())
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let environment = RuntimeEnvironmentV1::detect_fastembed_process()
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        let admitted = self
            .artifact_store
            .admit_leased_for_runtime_by_digest(
                &digest,
                &environment,
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                ArtifactLeaseKindV1::Active,
                current_unix_seconds()?,
            )
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        verify_catalog_manifest(model, admitted.manifest())?;
        let install_path = self.artifact_store.installed_directory(&digest);
        Ok(Some(if was_ready {
            SemanticModelLifecycleStateV1::Ready {
                model_id: model.model_id.clone(),
                revision: model.source.revision.clone(),
                artifact_digest,
                install_path,
            }
        } else {
            SemanticModelLifecycleStateV1::Installed {
                model_id: model.model_id.clone(),
                revision: model.source.revision.clone(),
                artifact_digest,
                install_path,
            }
        }))
    }

    /// Queue background acquisition when a selected model is not yet installed.
    pub fn enqueue_startup_acquisition_if_needed(&self) -> bool {
        let status = self.status();
        let selected_model = status.selected_model.clone();
        if !status.auto_download {
            return false;
        }
        let Some(state) = status.state else {
            return false;
        };
        if !matches!(
            state,
            SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. }
                | SemanticModelLifecycleStateV1::Failed {
                    retryable: true,
                    ..
                }
        ) {
            return false;
        }
        self.spawn_acquire(true, selected_model.as_deref())
    }

    pub fn retry(&self) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let status = self.status();
        if !status.remediation.retry {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        let model_id = status
            .selected_model
            .clone()
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        let selected = self.select_model(Some(&model_id), status.auto_download)?;
        if selected
            .state
            .as_ref()
            .and_then(verified_ready_artifact_digest)
            .is_none()
        {
            let _ = self.spawn_acquire(false, Some(&model_id));
        }
        Ok(self.status())
    }

    pub fn remove_install(&self) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let status = self.status();
        if !status.remediation.remove {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        self.cancel_and_join_background_acquisition()?;
        {
            let mut guard = self.inner.writer();
            let prior = guard.durable.clone();
            guard.durable.state = None;
            if let Err(error) = persist_durable(&self.root, &guard.durable) {
                guard.durable = prior;
                return Err(error);
            }
            self.reconcile_embedding_artifact_leases(&guard.durable, current_unix_seconds()?)?;
        }
        if let Some(state) = &status.state
            && let Some(path) = install_path_of(state)
        {
            if path.starts_with(self.root.join("verified-artifacts").join("artifacts")) {
                // Store bytes remain inventory-owned and become eligible only
                // under a later daemon GC lease.
            } else {
                fs::remove_dir_all(path).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
            }
        }
        let model_id = status.selected_model.clone();
        self.select_model(model_id.as_deref(), status.auto_download)
    }

    /// Signal the daemon-owned acquisition worker without blocking shutdown.
    pub fn cancel_background_acquisition(&self) {
        self.acquisition.cancel_current();
    }

    /// Clear and return a terminal worker join or cancellation-cleanup outcome
    /// after its quarantine or cleanup state has been explicitly resolved.
    pub fn resolve_background_acquisition_outcome(&self) -> Option<ModelLifecycleErrorV1> {
        self.worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .outcome
            .take()
    }

    /// Cancel and join the daemon-owned acquisition worker before its model
    /// state or staged files are mutated by another lifecycle operation.
    pub fn cancel_and_join_background_acquisition(&self) -> Result<(), ModelLifecycleErrorV1> {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        self.cancel_background_acquisition();
        if let Some(error) = worker.outcome.clone() {
            return Err(error);
        }
        if let Some(handle) = worker.handle.take() {
            worker.join_and_retain(handle)?;
        }
        Ok(())
    }

    /// Cancel acquisition and join only within the caller's shutdown budget.
    ///
    /// A worker that is still blocked in its source remains retained for a
    /// later join; cancellation checkpoints fence verified-install publication.
    pub fn cancel_and_join_background_acquisition_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<bool, ModelLifecycleErrorV1> {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        self.cancel_background_acquisition();
        if let Some(error) = worker.outcome.clone() {
            return Err(error);
        }
        loop {
            let finished = match worker.handle.as_ref() {
                None => return Ok(true),
                Some(handle) if handle.is_finished() => worker.handle.take(),
                Some(_) => None,
            };
            if let Some(handle) = finished {
                worker.join_and_retain(handle)?;
                return Ok(true);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(now)
                    .min(std::time::Duration::from_millis(1)),
            );
        }
    }

    pub fn rollback_to_previous(
        &self,
    ) -> Result<SemanticModelLifecycleStatusV1, ModelLifecycleErrorV1> {
        let mut guard = self.inner.writer();
        let previous = guard
            .durable
            .previous_ready
            .clone()
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        if !matches!(previous, SemanticModelLifecycleStateV1::Ready { .. }) {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        let prior_durable = guard.durable.clone();
        if install_path_of(&previous).is_some_and(|path| {
            path.starts_with(self.root.join("verified-artifacts").join("artifacts"))
        }) {
            let digest =
                super::manifest::Sha256DigestHex::new(previous.artifact_digest().to_owned())
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
            self.artifact_store.activate_artifact_with_rollback(
                &digest,
                EMBEDDING_ACTIVE_LEASE_ID_V1,
                EMBEDDING_ROLLBACK_LEASE_ID_V1,
                current_unix_seconds()?,
            )?;
        }
        if let Some(SemanticModelLifecycleStateV1::Ready { .. }) = &guard.durable.state {
            let ready_state = guard.durable.state.clone();
            guard.durable.previous_ready = ready_state;
        }
        guard.durable.selected_model = Some(previous.model_id().to_owned());
        guard.durable.state = Some(previous);
        if let Err(error) = persist_durable(&self.root, &guard.durable) {
            guard.durable = prior_durable.clone();
            self.reconcile_embedding_artifact_leases(&prior_durable, current_unix_seconds()?)?;
            return Err(error);
        }
        publish_verified_ready_event(&self.verified_ready, &guard);
        drop(guard);
        Ok(self.status())
    }

    pub fn mark_loading(&self) -> Result<(), ModelLifecycleErrorV1> {
        self.transition_installed_like(|model_id, revision, digest, path| {
            SemanticModelLifecycleStateV1::Loading {
                model_id,
                revision,
                artifact_digest: digest,
                install_path: path,
            }
        })
    }

    pub fn mark_indexing(
        &self,
        completed_units: u64,
        total_units: u64,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.writer();
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let (SemanticModelLifecycleStateV1::Installed {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
        }
        | SemanticModelLifecycleStateV1::Loading {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
        }
        | SemanticModelLifecycleStateV1::Indexing {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
        }) = state
        else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        if total_units == 0 || completed_units > total_units {
            return Err(ModelLifecycleErrorV1::Rejected);
        }
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Indexing {
            model_id,
            revision,
            artifact_digest: digest,
            install_path,
            completed_units,
            total_units,
        });
        persist_durable(&self.root, &guard.durable)
    }

    pub fn mark_ready(&self) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.writer();
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let ready = match state {
            SemanticModelLifecycleStateV1::Installed {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Loading {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Indexing {
                model_id,
                revision,
                artifact_digest,
                install_path,
                ..
            } => SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                install_path,
            },
            SemanticModelLifecycleStateV1::Ready { .. } => state,
            _ => return Err(ModelLifecycleErrorV1::Rejected),
        };
        if let Some(previous) = guard.durable.state.clone()
            && matches!(previous, SemanticModelLifecycleStateV1::Ready { .. })
            && previous.artifact_digest() != ready.artifact_digest()
        {
            guard.durable.previous_ready = Some(previous);
        }
        guard.durable.state = Some(ready);
        persist_durable(&self.root, &guard.durable)?;
        publish_verified_ready_event(&self.verified_ready, &guard);
        Ok(())
    }

    pub fn mark_runtime_failed(
        &self,
        detail: impl Into<String>,
        retryable: bool,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.writer();
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        let (SemanticModelLifecycleStateV1::Installed {
            model_id,
            revision,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Loading {
            model_id,
            revision,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Indexing {
            model_id,
            revision,
            artifact_digest,
            ..
        }
        | SemanticModelLifecycleStateV1::Ready {
            model_id,
            revision,
            artifact_digest,
            ..
        }) = state
        else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
            model_id,
            revision,
            artifact_digest,
            detail: detail.into(),
            retryable,
        });
        persist_durable(&self.root, &guard.durable)
    }

    fn transition_installed_like(
        &self,
        build: impl FnOnce(String, String, String, PathBuf) -> SemanticModelLifecycleStateV1,
    ) -> Result<(), ModelLifecycleErrorV1> {
        let mut guard = self.inner.writer();
        let Some(state) = guard.durable.state.clone() else {
            return Err(ModelLifecycleErrorV1::Rejected);
        };
        if matches!(state, SemanticModelLifecycleStateV1::Ready { .. }) {
            guard.durable.previous_ready = Some(state.clone());
        }
        let next = match state {
            SemanticModelLifecycleStateV1::Installed {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Loading {
                model_id,
                revision,
                artifact_digest,
                install_path,
            }
            | SemanticModelLifecycleStateV1::Ready {
                model_id,
                revision,
                artifact_digest,
                install_path,
            } => build(model_id, revision, artifact_digest, install_path),
            _ => return Err(ModelLifecycleErrorV1::Rejected),
        };
        guard.durable.state = Some(next);
        persist_durable(&self.root, &guard.durable)
    }

    fn spawn_acquire(&self, require_auto_download: bool, expected_model_id: Option<&str>) -> bool {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        if worker.outcome.is_some() {
            return false;
        }
        if worker.reap_finished().is_err() {
            return false;
        }
        if worker.handle.is_some() {
            return false;
        }
        let epoch = self.acquisition.begin_epoch();
        let root = self.root.clone();
        let catalog = self.catalog.clone();
        let source = Arc::clone(&self.source);
        let inner = Arc::clone(&self.inner);
        let selected = {
            let guard = inner.read();
            if require_auto_download && !guard.durable.auto_download {
                return false;
            }
            if !matches!(
                guard.durable.state.as_ref(),
                Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded { .. })
                    | Some(SemanticModelLifecycleStateV1::Failed {
                        retryable: true,
                        ..
                    })
            ) {
                return false;
            }
            let selected = guard.durable.selected_model.clone();
            if expected_model_id.is_some_and(|expected| selected.as_deref() != Some(expected)) {
                return false;
            }
            selected
        };
        let Some(model_id) = selected else {
            return false;
        };
        let worker_root = root.clone();
        let worker_catalog = catalog.clone();
        let worker_model_id = model_id.clone();
        let worker_inner = Arc::clone(&inner);
        let verified_ready = self.verified_ready.clone();
        let handle = thread::Builder::new()
            .name("tracedecay-fastembed-acquire".to_owned())
            .spawn(move || {
                let result = run_acquisition(
                    &worker_root,
                    &worker_catalog,
                    source.as_ref(),
                    &worker_model_id,
                    &epoch,
                    &worker_inner,
                    &verified_ready,
                );
                result
            });
        match handle {
            Ok(join) => {
                worker.handle = Some(join);
                true
            }
            Err(error) => {
                if let Some(model) = catalog.get(&model_id) {
                    let _ = set_failed_state(
                        &root,
                        &inner,
                        model,
                        &catalog_package_digest(model),
                        &format!("cannot start background acquisition worker: {error}"),
                        true,
                    );
                }
                false
            }
        }
    }

    /// Synchronously acquire for tests and focused integration.
    pub fn acquire_blocking_for_tests(&self) -> Result<(), ModelLifecycleErrorV1> {
        let model_id = self
            .status()
            .selected_model
            .ok_or(ModelLifecycleErrorV1::Rejected)?;
        let epoch = self.acquisition.begin_epoch();
        run_acquisition(
            &self.root,
            &self.catalog,
            self.source.as_ref(),
            &model_id,
            &epoch,
            &self.inner,
            &self.verified_ready,
        )
    }
}

fn join_acquisition_worker(
    worker: JoinHandle<Result<(), ModelLifecycleErrorV1>>,
) -> Result<(), ModelLifecycleErrorV1> {
    match worker
        .join()
        .map_err(|_| ModelLifecycleErrorV1::WorkerJoinFailed)?
    {
        Ok(()) | Err(ModelLifecycleErrorV1::Cancelled) => Ok(()),
        Err(error @ ModelLifecycleErrorV1::CancellationCleanupQuarantined(_))
        | Err(error @ ModelLifecycleErrorV1::CancellationCleanupFailed(_)) => Err(error),
        Err(_) => Ok(()),
    }
}
