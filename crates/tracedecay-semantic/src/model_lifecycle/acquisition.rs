fn current_unix_seconds() -> Result<u64, ModelLifecycleErrorV1> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)
}
/// The model one acquisition run installs and the root it installs into.
#[derive(Clone, Copy)]
struct AcquisitionTargetV1<'a> {
    root: &'a Path,
    catalog: &'a FastEmbedModelCatalogV1,
    source: &'a dyn ModelMemberSourceV1,
    model_id: &'a str,
}

#[hotpath::measure(label = "semantic.model_lifecycle.acquire")]
fn run_acquisition(
    target: AcquisitionTargetV1<'_>,
    epoch: &AcquisitionEpochV1,
    inner: &LifecyclePublicationGateV1,
    verified_ready: &watch::Sender<SemanticLifecycleVerifiedReadyEventV1>,
    shared_store: Option<(&ModelArtifactStore, &str, &str)>,
) -> Result<(), ModelLifecycleErrorV1> {
    let AcquisitionTargetV1 {
        root,
        catalog,
        model_id,
        ..
    } = target;
    let result = run_acquisition_inner(target, epoch, inner, verified_ready, shared_store);
    match &result {
        Ok(()) => crate::hotpath_observe::record_model_state("installed"),
        Err(error) => {
            crate::hotpath_observe::record_lifecycle_error(error);
        }
    }
    if let Err(error) = &result
        && !matches!(error, ModelLifecycleErrorV1::Cancelled)
        && let Some(model) = catalog.get(model_id)
    {
        let already_failed = {
            let guard = inner.read();
            matches!(
                guard.durable.state.as_ref(),
                Some(SemanticModelLifecycleStateV1::Failed { .. })
            )
        };
        if !already_failed {
            let retryable = matches!(
                error,
                ModelLifecycleErrorV1::StoreUnavailable
                    | ModelLifecycleErrorV1::DownloadFailed
                    | ModelLifecycleErrorV1::DownloadFailedWithReason(_)
                    | ModelLifecycleErrorV1::InstallFailed
                    | ModelLifecycleErrorV1::ArtifactImport(
                        ArtifactImportErrorV1::StagingUnavailable
                    )
            );
            let _ = epoch.while_current(|| {
                set_failed_state(
                    root,
                    inner,
                    model,
                    &catalog_package_digest(model),
                    &error.to_string(),
                    retryable,
                )
            });
        }
    }
    result
}
fn run_acquisition_inner(
    target: AcquisitionTargetV1<'_>,
    epoch: &AcquisitionEpochV1,
    inner: &LifecyclePublicationGateV1,
    verified_ready: &watch::Sender<SemanticLifecycleVerifiedReadyEventV1>,
    shared_store: Option<(&ModelArtifactStore, &str, &str)>,
) -> Result<(), ModelLifecycleErrorV1> {
    let AcquisitionTargetV1 {
        root,
        catalog,
        source,
        model_id,
    } = target;
    let model = catalog
        .get(model_id)
        .ok_or(CatalogErrorV1::UnknownModel)?
        .clone();
    let digest = catalog_package_digest(&model);
    let bytes_total: u64 = model.members.values().map(|member| member.length).sum();

    epoch.while_active(|| {
        let mut guard = inner.writer();
        guard.durable.selected_model = Some(model.model_id.clone());
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Downloading {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest.clone(),
            bytes_received: 0,
            bytes_total,
        });
        persist_durable(root, &guard.durable)
    })?;
    crate::hotpath_observe::record_model_state("downloading");

    let staging = root.join("staging").join(format!(
        "{}-{}",
        model.model_id,
        &digest[..16.min(digest.len())]
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
    }
    fs::create_dir_all(&staging).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;

    let mut bytes_received = 0_u64;
    hotpath::gauge!("semantic_acquire_bytes_total").set(bytes_total);
    hotpath::gauge!("semantic_acquire_bytes_received").set(bytes_received);
    // Network + staging-copy phase. Early cancellation/failure returns drop
    // the span guard, so aborted downloads still record their duration.
    hotpath::measure_block!("semantic.acquire.download", {
        for member in model.members.values() {
            if epoch.ensure_active().is_err() {
                cleanup_cancelled_path(root, &staging, epoch)?;
                return Err(ModelLifecycleErrorV1::Cancelled);
            }
            let destination = staging.join(&member.path);
            let fetch = source.fetch_member(&model, &member.upstream_path, &destination);
            if epoch.ensure_active().is_err() {
                cleanup_cancelled_path(root, &staging, epoch)?;
                return Err(ModelLifecycleErrorV1::Cancelled);
            }
            if let Err(error) = fetch {
                return fail_state(root, inner, &model, &digest, &error.to_string(), true);
            }
            bytes_received = bytes_received.saturating_add(member.length);
            hotpath::gauge!("semantic_acquire_bytes_received").set(bytes_received);
            let progress = epoch.while_active(|| {
                let mut guard = inner.writer();
                guard.durable.state = Some(SemanticModelLifecycleStateV1::Downloading {
                    model_id: model.model_id.clone(),
                    revision: model.source.revision.clone(),
                    artifact_digest: digest.clone(),
                    bytes_received,
                    bytes_total,
                });
                persist_durable(root, &guard.durable)
            });
            if matches!(&progress, Err(ModelLifecycleErrorV1::Cancelled)) {
                cleanup_cancelled_path(root, &staging, epoch)?;
            }
            progress?;
        }
    });

    if epoch.ensure_active().is_err() {
        cleanup_cancelled_path(root, &staging, epoch)?;
        return Err(ModelLifecycleErrorV1::Cancelled);
    }
    let verifying = epoch.while_active(|| {
        let mut guard = inner.writer();
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Verifying {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest.clone(),
        });
        persist_durable(root, &guard.durable)
    });
    if matches!(&verifying, Err(ModelLifecycleErrorV1::Cancelled)) {
        cleanup_cancelled_path(root, &staging, epoch)?;
    }
    verifying?;
    crate::hotpath_observe::record_model_state("verifying");

    // Disk read + SHA-256 digest verification of every staged member,
    // separate from the download above and the install rename below.
    hotpath::measure_block!("semantic.acquire.verify", {
        for member in model.members.values() {
            let path = staging.join(&member.path);
            if !verify_member_file(&path, member.length, &member.sha256) {
                fs::remove_dir_all(&staging)
                    .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
                return fail_state(
                    root,
                    inner,
                    &model,
                    &digest,
                    "member length or sha256 mismatch",
                    true,
                );
            }
            if epoch.ensure_active().is_err() {
                cleanup_cancelled_path(root, &staging, epoch)?;
                return Err(ModelLifecycleErrorV1::Cancelled);
            }
        }
    });

    if epoch.ensure_active().is_err() {
        cleanup_cancelled_path(root, &staging, epoch)?;
        return Err(ModelLifecycleErrorV1::Cancelled);
    }
    if let Some((store, active_lease, rollback_lease)) = shared_store {
        let resources = SemanticResourceCeilings {
            max_sequence_length: model.max_length,
            // Acquisition writes a durable artifact manifest, and its resource ceiling
        // records the shipped capability — not this host's share. Neither
        // operator configuration nor the process memory authority is in scope
        // here; the composition root resolves the live ceiling against the host
        // when it opens a session over the installed artifact.
        max_resident_bytes: Some(
            tracedecay_semantic_contracts::DEFAULT_SEMANTIC_RESIDENT_BYTES,
        ),
            ..SemanticResourceCeilings::default()
        };
        let manifest = catalog_artifact_manifest(&model, resources)?;
        let now_unix = current_unix_seconds()?;
        let record = store.import_local_directory(&manifest, &staging, now_unix)?;
        // Imported bytes belong to inventory even when cancellation races the
        // import. Only owner-private staging may be removed by this worker.
        fs::remove_dir_all(&staging).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
        return epoch.while_active(|| {
            let mut guard = inner.writer();
            let prior = guard.durable.clone();
            store.activate_artifact_with_rollback(
                &record.artifact_digest,
                active_lease,
                rollback_lease,
                now_unix,
            )?;
            // The lifecycle names the catalog package it installed, exactly as
            // the download/verify states before it and the private-root path
            // below do: that digest is the projection identity every vector
            // generation and compatibility pin carries. The inventory's
            // content address (which also hashes host-derived resource
            // ceilings) stays private to the store and is recovered from the
            // install directory when a lease or rollback needs it.
            guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
                model_id: model.model_id.clone(),
                revision: model.source.revision.clone(),
                artifact_digest: digest.clone(),
                install_path: store.installed_directory(&record.artifact_digest),
            });
            if let Err(error) = persist_durable(root, &guard.durable) {
                guard.durable = prior;
                reconcile_embedding_artifact_leases(
                    store,
                    active_lease,
                    rollback_lease,
                    &guard.durable,
                    now_unix,
                )?;
                return Err(error);
            }
            publish_verified_ready_event(verified_ready, &guard);
            Ok(())
        });
    }
    let install_path = install_path_for(root, &model.model_id, &model.source.revision, &digest);
    // Install-publication disk phase: prior-install removal, atomic rename,
    // and the durable install.json write.
    hotpath::measure_block!("semantic.acquire.install", {
        if let Some(parent) = install_path.parent() {
            fs::create_dir_all(parent).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
        }
        if install_path.exists() {
            fs::remove_dir_all(&install_path).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
        }
        // Atomic publish: rename fully verified staging directory into place.
        fs::rename(&staging, &install_path).map_err(|_| ModelLifecycleErrorV1::InstallFailed)?;
        if epoch.ensure_active().is_err() {
            cleanup_cancelled_path(root, &install_path, epoch)?;
            return Err(ModelLifecycleErrorV1::Cancelled);
        }
        let meta = InstallMetaV1 {
            schema: INSTALL_META_SCHEMA_V1.to_owned(),
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest.clone(),
        };
        let metadata = write_json_atomic(&install_path.join("install.json"), &meta)
            .map_err(|_| ModelLifecycleErrorV1::InstallFailed);
        if epoch.ensure_active().is_err() {
            cleanup_cancelled_path(root, &install_path, epoch)?;
            return Err(ModelLifecycleErrorV1::Cancelled);
        }
        metadata?;
    });

    let publication = epoch.while_active(|| {
        let mut guard = inner.writer();
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest,
            install_path: install_path.clone(),
        });
        persist_durable(root, &guard.durable)?;
        publish_verified_ready_event(verified_ready, &guard);
        Ok(())
    });
    if matches!(&publication, Err(ModelLifecycleErrorV1::Cancelled)) {
        cleanup_cancelled_path(root, &install_path, epoch)?;
    }
    publication
}

fn cleanup_cancelled_path(
    root: &Path,
    path: &Path,
    epoch: &AcquisitionEpochV1,
) -> Result<(), ModelLifecycleErrorV1> {
    epoch.while_current(|| {
        if !path.exists() {
            return Ok(());
        }
        if fs::remove_dir_all(path).is_ok() || !path.exists() {
            return Ok(());
        }
        let quarantine_root = root.join("quarantine");
        fs::create_dir_all(&quarantine_root)
            .map_err(|_| ModelLifecycleErrorV1::CancellationCleanupFailed(path.to_path_buf()))?;
        let leaf = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let quarantine_path = quarantine_root.join(format!("acquisition-{}-{leaf}", epoch.epoch));
        fs::rename(path, &quarantine_path)
            .map_err(|_| ModelLifecycleErrorV1::CancellationCleanupFailed(path.to_path_buf()))?;
        match fs::remove_dir_all(&quarantine_path) {
            Ok(()) => Ok(()),
            Err(_) => Err(ModelLifecycleErrorV1::CancellationCleanupQuarantined(
                quarantine_path,
            )),
        }
    })
}

fn fail_state(
    root: &Path,
    inner: &LifecyclePublicationGateV1,
    model: &CatalogedFastEmbedModelV1,
    digest: &str,
    detail: &str,
    retryable: bool,
) -> Result<(), ModelLifecycleErrorV1> {
    set_failed_state(root, inner, model, digest, detail, retryable)?;
    Err(if retryable {
        ModelLifecycleErrorV1::DownloadFailed
    } else {
        ModelLifecycleErrorV1::VerificationFailed
    })
}
