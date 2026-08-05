fn set_failed_state(
    root: &Path,
    inner: &Mutex<LifecycleInner>,
    model: &CatalogedFastEmbedModelV1,
    digest: &str,
    detail: &str,
    retryable: bool,
) -> Result<(), ModelLifecycleErrorV1> {
    let mut guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
    guard.durable.state = Some(SemanticModelLifecycleStateV1::Failed {
        model_id: model.model_id.clone(),
        revision: model.source.revision.clone(),
        artifact_digest: digest.to_owned(),
        detail: detail.to_owned(),
        retryable,
    });
    persist_durable(root, &guard.durable)
}
fn verify_catalog_manifest(
    model: &CatalogedFastEmbedModelV1,
    manifest: &ModelArtifactManifestV1,
) -> Result<(), ModelLifecycleErrorV1> {
    manifest
        .validate()
        .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
    if manifest.payload.artifact_id != model.model_id
        || manifest.payload.dimensions != model.expected_dimensions
        || manifest.payload.truncation.max_length != model.max_length
        || manifest.payload.spdx_license != model.source.license
        || manifest.payload.upstream.revision != model.source.revision
    {
        return Err(ModelLifecycleErrorV1::VerificationFailed);
    }
    for (role_name, catalog_member) in &model.members {
        let role = match role_name.as_str() {
            "model" => ArtifactMemberRoleV1::Model,
            "tokenizer" => ArtifactMemberRoleV1::Tokenizer,
            "config" => ArtifactMemberRoleV1::Config,
            "special_tokens_map" => ArtifactMemberRoleV1::SpecialTokensMap,
            "tokenizer_config" => ArtifactMemberRoleV1::TokenizerConfig,
            _ => return Err(ModelLifecycleErrorV1::VerificationFailed),
        };
        let member = manifest
            .package_member(role)
            .ok_or(ModelLifecycleErrorV1::VerificationFailed)?;
        if member.path != catalog_member.path
            || member.byte_length != catalog_member.length
            || member.digest.as_str() != catalog_member.sha256
        {
            return Err(ModelLifecycleErrorV1::VerificationFailed);
        }
    }
    Ok(())
}
fn load_or_default_durable(
    root: &Path,
    catalog: &FastEmbedModelCatalogV1,
) -> Result<DurableLifecycleV1, ModelLifecycleErrorV1> {
    let path = root.join("lifecycle.json");
    if path.is_file() {
        let bytes = fs::read(&path).map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)?;
        let durable = serde_json::from_slice::<DurableLifecycleV1>(&bytes)
            .map_err(|_| ModelLifecycleErrorV1::VerificationFailed)?;
        if durable.schema != LIFECYCLE_SCHEMA_V1 {
            return Err(ModelLifecycleErrorV1::VerificationFailed);
        }
        return Ok(durable);
    }
    let model = catalog
        .get(DEFAULT_FASTEMBED_MODEL_ID)
        .ok_or(CatalogErrorV1::MissingDefault)?;
    let digest = catalog_package_digest(model);
    let state = if let Some(path) = existing_install_path(root, model, &digest) {
        Some(SemanticModelLifecycleStateV1::Installed {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest,
            install_path: path,
        })
    } else {
        Some(SemanticModelLifecycleStateV1::SelectedNotDownloaded {
            model_id: model.model_id.clone(),
            revision: model.source.revision.clone(),
            artifact_digest: digest,
        })
    };
    let durable = DurableLifecycleV1 {
        schema: LIFECYCLE_SCHEMA_V1.to_owned(),
        selected_model: Some(DEFAULT_FASTEMBED_MODEL_ID.to_owned()),
        auto_download: false,
        state,
        previous_ready: None,
    };
    persist_durable(root, &durable)?;
    Ok(durable)
}

fn persist_durable(root: &Path, durable: &DurableLifecycleV1) -> Result<(), ModelLifecycleErrorV1> {
    write_json_atomic(&root.join("lifecycle.json"), durable)
        .map_err(|_| ModelLifecycleErrorV1::StoreUnavailable)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut file, value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        file.flush()?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn install_path_for(root: &Path, model_id: &str, revision: &str, digest: &str) -> PathBuf {
    root.join("installs")
        .join(model_id)
        .join(revision)
        .join(&digest[..16.min(digest.len())])
}

fn existing_install_path(
    root: &Path,
    model: &CatalogedFastEmbedModelV1,
    digest: &str,
) -> Option<PathBuf> {
    let path = install_path_for(root, &model.model_id, &model.source.revision, digest);
    let meta_path = path.join("install.json");
    if !meta_path.is_file() {
        return None;
    }
    let bytes = fs::read(&meta_path).ok()?;
    let meta: InstallMetaV1 = serde_json::from_slice(&bytes).ok()?;
    if meta.schema != INSTALL_META_SCHEMA_V1
        || meta.model_id != model.model_id
        || meta.revision != model.source.revision
        || meta.artifact_digest != digest
    {
        return None;
    }
    for member in model.members.values() {
        if !verify_member_file(&path.join(&member.path), member.length, &member.sha256) {
            return None;
        }
    }
    Some(path)
}

fn install_path_of(state: &SemanticModelLifecycleStateV1) -> Option<&Path> {
    match state {
        SemanticModelLifecycleStateV1::Installed { install_path, .. }
        | SemanticModelLifecycleStateV1::Loading { install_path, .. }
        | SemanticModelLifecycleStateV1::Indexing { install_path, .. }
        | SemanticModelLifecycleStateV1::Ready { install_path, .. } => Some(install_path),
        _ => None,
    }
}

fn state_revision(state: &SemanticModelLifecycleStateV1) -> &str {
    match state {
        SemanticModelLifecycleStateV1::SelectedNotDownloaded { revision, .. }
        | SemanticModelLifecycleStateV1::Downloading { revision, .. }
        | SemanticModelLifecycleStateV1::Verifying { revision, .. }
        | SemanticModelLifecycleStateV1::Installed { revision, .. }
        | SemanticModelLifecycleStateV1::Loading { revision, .. }
        | SemanticModelLifecycleStateV1::Indexing { revision, .. }
        | SemanticModelLifecycleStateV1::Ready { revision, .. }
        | SemanticModelLifecycleStateV1::Failed { revision, .. } => revision,
    }
}

fn verify_member_file(path: &Path, length: u64, sha256: &str) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.len() != length {
        return false;
    }
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let Ok(read) = file.read(&mut buffer) else {
            return false;
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hex::encode(hasher.finalize()) == sha256
}
