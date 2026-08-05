fn current_unix_seconds() -> Result<u64, ModelLifecycleErrorV1> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)
}
fn run_acquisition(
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
    source: &dyn ModelMemberSourceV1,
    model_id: &str,
    epoch: &AcquisitionEpochV1,
    inner: &Mutex<LifecycleInner>,
) -> Result<(), ModelLifecycleErrorV1> {
    let result = run_acquisition_inner(root, catalog, source, model_id, epoch, inner);
    if let Err(error) = &result
        && !matches!(error, ModelLifecycleErrorV1::Cancelled)
        && let Some(model) = catalog.get(model_id)
    {
        let already_failed = {
            let guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
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
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
    source: &dyn ModelMemberSourceV1,
    model_id: &str,
    epoch: &AcquisitionEpochV1,
    inner: &Mutex<LifecycleInner>,
) -> Result<(), ModelLifecycleErrorV1> {
    let model = catalog
        .get(model_id)
        .ok_or(CatalogErrorV1::UnknownModel)?
        .clone();
    let digest = catalog_package_digest(&model);
    let bytes_total: u64 = model.members.values().map(|member| member.length).sum();

    epoch.while_active(|| {
        let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
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
        let progress = epoch.while_active(|| {
            let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
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

    if epoch.ensure_active().is_err() {
        cleanup_cancelled_path(root, &staging, epoch)?;
        return Err(ModelLifecycleErrorV1::Cancelled);
    }
    let verifying = epoch.while_active(|| {
        let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
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

    if epoch.ensure_active().is_err() {
        cleanup_cancelled_path(root, &staging, epoch)?;
        return Err(ModelLifecycleErrorV1::Cancelled);
    }
    let install_path = install_path_for(root, &model.model_id, &model.source.revision, &digest);
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

    let publication = epoch.while_active(|| {
        let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
        guard.durable.state = Some(SemanticModelLifecycleStateV1::Installed {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest,
            install_path: install_path.clone(),
        });
        persist_durable(root, &guard.durable)
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
        let quarantine_path =
            quarantine_root.join(format!("acquisition-{}-{leaf}", epoch.epoch));
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
    inner: &Mutex<LifecycleInner>,
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
