//! I/O and validation primitives for the model artifact store.
//! Split out of artifact_store.rs to keep the facade under the 1000-line
//! hygiene ceiling; pure structural move, no behavior change.

use super::*;

pub(super) fn check_compatibility(
    required: &super::super::manifest::RuntimeCompatibilityV1,
    env: &RuntimeEnvironmentV1,
) -> Result<(), SemanticCapabilityDisabledV1> {
    if required.runtime != env.runtime || required.build_revision != env.build_revision {
        return Err(SemanticCapabilityDisabledV1::IncompatibleRuntime);
    }
    if !required
        .platforms
        .iter()
        .any(|p| p.os == env.os && p.arch == env.arch)
    {
        return Err(SemanticCapabilityDisabledV1::IncompatiblePlatform);
    }
    Ok(())
}

pub(super) fn check_resource_ceiling(
    ceiling: &ResourceCeilingV1,
    env: &RuntimeEnvironmentV1,
) -> Result<(), SemanticCapabilityDisabledV1> {
    if env.available_resident_bytes < ceiling.max_resident_bytes {
        return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded);
    }
    if env.available_threads < ceiling.max_threads {
        return Err(SemanticCapabilityDisabledV1::ResourceCeilingExceeded);
    }
    Ok(())
}

pub(super) fn quarantine_reason_for_import_error(
    error: &ArtifactImportErrorV1,
) -> QuarantineReasonV1 {
    match error {
        ArtifactImportErrorV1::SizeExpansionBeyondDeclared => QuarantineReasonV1::SizeExpansion,
        ArtifactImportErrorV1::LengthMismatch => QuarantineReasonV1::MemberLengthMismatch,
        ArtifactImportErrorV1::DigestMismatch => QuarantineReasonV1::MemberDigestMismatch,
        ArtifactImportErrorV1::UndeclaredMember => QuarantineReasonV1::UndeclaredMember,
        ArtifactImportErrorV1::UnsafePackageEntry | ArtifactImportErrorV1::UnsafeStorePath => {
            QuarantineReasonV1::UnsafePackage
        }
        ArtifactImportErrorV1::SourceInterrupted => QuarantineReasonV1::SourceInterrupted,
        _ => QuarantineReasonV1::IdentityMismatch,
    }
}

pub(super) fn inspect_local_package(
    source: &Path,
) -> Result<BTreeMap<String, PathBuf>, ArtifactImportErrorV1> {
    let source_meta =
        fs::symlink_metadata(source).map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
    if !source_meta.is_dir() || source_meta.file_type().is_symlink() {
        return Err(ArtifactImportErrorV1::UnsafePackageEntry);
    }
    let mut files = BTreeMap::new();
    let mut pending = vec![(source.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in
            fs::read_dir(&directory).map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?
        {
            let entry = entry.map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
            if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
            if metadata.file_type().is_symlink() {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
            if metadata.is_dir() {
                pending.push((entry.path(), relative));
                continue;
            }
            if !metadata.is_file() || metadata_has_multiple_links(&metadata) {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
            if files.insert(relative, entry.path()).is_some() {
                return Err(ArtifactImportErrorV1::UnsafePackageEntry);
            }
        }
    }
    Ok(files)
}

#[cfg(unix)]
pub(super) fn metadata_has_multiple_links(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() != 1
}

#[cfg(not(unix))]
pub(super) fn metadata_has_multiple_links(_metadata: &fs::Metadata) -> bool {
    false
}

pub(super) fn stream_local_member(
    store: &ModelArtifactStore,
    session: &mut ImportSession,
    member: &ArtifactPackageMemberV1,
    path: &Path,
    now_unix: u64,
) -> Result<(), ArtifactImportErrorV1> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ArtifactImportErrorV1::UnsafePackageEntry)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata_has_multiple_links(&metadata)
    {
        return Err(ArtifactImportErrorV1::UnsafePackageEntry);
    }
    if metadata.len() > member.byte_length {
        return Err(ArtifactImportErrorV1::SizeExpansionBeyondDeclared);
    }
    if metadata.len() != member.byte_length {
        return Err(ArtifactImportErrorV1::LengthMismatch);
    }
    let mut file = File::open(path).map_err(|_| ArtifactImportErrorV1::SourceInterrupted)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArtifactImportErrorV1::SourceInterrupted)?;
        if read == 0 {
            break;
        }
        store.stage_member_chunk(session, member.role, &buffer[..read], now_unix)?;
    }
    Ok(())
}

pub(super) fn sha256_open_file(
    mut file: impl Read,
) -> Result<Sha256DigestHex, ArtifactImportErrorV1> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Sha256DigestHex::new(hex::encode(hasher.finalize()))
        .map_err(|_| ArtifactImportErrorV1::StorageFailure)
}

pub(super) fn write_staging_meta(
    dir: &Dir,
    ambient_path: &Path,
    meta: &StagingMetaV1,
) -> Result<(), ArtifactImportErrorV1> {
    let bytes = serde_json::to_vec(meta).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
    atomic_write_cap_file(dir, ambient_path, "import.meta.json", &bytes)
}

pub(super) fn read_staging_meta(dir: &Dir) -> Result<StagingMetaV1, ArtifactImportErrorV1> {
    let bytes = read_optional_cap_file(dir, "import.meta.json")?
        .ok_or(ArtifactImportErrorV1::StagingUnavailable)?;
    serde_json::from_slice(&bytes).map_err(|_| ArtifactImportErrorV1::StorageFailure)
}

pub(super) fn read_receipt_frames(bytes: &[u8]) -> Result<Vec<GcReceiptV1>, ArtifactImportErrorV1> {
    let mut receipts = Vec::new();
    for frame in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !frame.ends_with(b"\n") {
            break;
        }
        let payload = &frame[..frame.len() - 1];
        if payload.is_empty() {
            continue;
        }
        match serde_json::from_slice(payload) {
            Ok(receipt) => receipts.push(receipt),
            Err(_) => break,
        }
    }
    Ok(receipts)
}

pub(super) fn open_cap_file(
    dir: &Dir,
    name: &str,
    read: bool,
    write: bool,
    create: bool,
    create_new: bool,
    append: bool,
) -> Result<CapFile, ArtifactImportErrorV1> {
    if !is_component(name) {
        return Err(ArtifactImportErrorV1::UnsafeStorePath);
    }
    let mut options = CapOpenOptions::new();
    options
        .read(read)
        .write(write)
        .create(create)
        .create_new(create_new)
        .append(append);
    #[cfg(unix)]
    options.mode(0o600);
    options.follow(FollowSymlinks::No);
    if write {
        options.sync(true);
    }
    dir.open_with(name, &options)
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ArtifactImportErrorV1::StagingUnavailable,
            _ => ArtifactImportErrorV1::UnsafeStorePath,
        })
}

pub(super) fn read_optional_cap_file(
    dir: &Dir,
    name: &str,
) -> Result<Option<Vec<u8>>, ArtifactImportErrorV1> {
    match open_cap_file(dir, name, true, false, false, false, false) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            Ok(Some(bytes))
        }
        Err(ArtifactImportErrorV1::StagingUnavailable) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn atomic_write_cap_file(
    dir: &Dir,
    ambient_parent: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(), ArtifactImportErrorV1> {
    if !is_component(name) {
        return Err(ArtifactImportErrorV1::UnsafeStorePath);
    }
    #[cfg(windows)]
    {
        // `Dir` holds the parent without FILE_SHARE_DELETE, so the maintained
        // fsys wrapper can safely perform replace-existing + write-through by
        // ambient path without a parent replacement/reparse race.
        dir.dir_metadata()
            .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
        fsys::quick::write(ambient_parent.join(name), bytes)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        sync_cap_dir(dir)
    }
    #[cfg(not(windows))]
    {
        let temporary = format!(".{name}.{}.tmp", random_staging_id()?);
        {
            let mut file = open_cap_file(dir, &temporary, false, true, false, true, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        replace_cap_file(dir, ambient_parent, &temporary, name)?;
        sync_cap_dir(dir)
    }
}

#[cfg(not(windows))]
pub(super) fn replace_cap_file(
    dir: &Dir,
    _ambient_parent: &Path,
    temporary: &str,
    destination: &str,
) -> Result<(), ArtifactImportErrorV1> {
    dir.rename(temporary, dir, destination)
        .map_err(|_| ArtifactImportErrorV1::StorageFailure)
}

pub(super) fn remove_cap_file_if_exists(
    dir: &Dir,
    name: &str,
) -> Result<(), ArtifactImportErrorV1> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() => dir
            .remove_file(name)
            .map_err(|_| ArtifactImportErrorV1::StorageFailure),
        Ok(_) => Err(ArtifactImportErrorV1::UnsafeStorePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ArtifactImportErrorV1::StorageFailure),
    }
}

pub(super) fn sync_cap_dir(dir: &Dir) -> Result<(), ArtifactImportErrorV1> {
    #[cfg(windows)]
    {
        // MoveFileExW WRITE_THROUGH is the Windows namespace durability
        // authority; directory FlushFileBuffers is not supported reliably.
        dir.dir_metadata()
            .map(|_| ())
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)
    }
    #[cfg(not(windows))]
    {
        let mut options = CapOpenOptions::new();
        options.read(true).maybe_dir(true);
        dir.open_with(".", &options)
            .and_then(|file| file.sync_all())
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)
    }
}

pub(super) fn is_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

pub(super) fn open_root_from_trusted_parent(root: &Path) -> Result<Dir, ArtifactImportErrorV1> {
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| is_component(name))
        .ok_or(ArtifactImportErrorV1::UnsafeStorePath)?;
    let trusted_parent = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = Dir::open_ambient_dir(trusted_parent, ambient_authority())
        .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)?;
    open_or_create_component_dir(&parent, root_name)
}

pub(super) fn open_or_create_component_dir(
    parent: &Dir,
    name: &str,
) -> Result<Dir, ArtifactImportErrorV1> {
    if !is_component(name) {
        return Err(ArtifactImportErrorV1::UnsafeStorePath);
    }
    match parent.open_dir_nofollow(name) {
        Ok(dir) => Ok(dir),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[allow(unused_mut)] // mode() is unix-only
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            parent
                .create_dir_with(name, &builder)
                .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
            parent
                .open_dir_nofollow(name)
                .map_err(|_| ArtifactImportErrorV1::UnsafeStorePath)
        }
        Err(_) => Err(ArtifactImportErrorV1::UnsafeStorePath),
    }
}

pub(super) fn member_file_name(role: ArtifactMemberRoleV1) -> &'static str {
    match role {
        ArtifactMemberRoleV1::Model => "model.onnx",
        ArtifactMemberRoleV1::Tokenizer => "tokenizer.json",
        ArtifactMemberRoleV1::Config => "config.json",
        ArtifactMemberRoleV1::SpecialTokensMap => "special_tokens_map.json",
        ArtifactMemberRoleV1::TokenizerConfig => "tokenizer_config.json",
        ArtifactMemberRoleV1::QueryInstruction => "query_instruction.txt",
        ArtifactMemberRoleV1::DocumentInstruction => "document_instruction.txt",
    }
}

pub(super) fn is_valid_staging_id(staging_id: &str) -> bool {
    staging_id.len() == 32
        && staging_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(super) fn random_staging_id() -> Result<String, ArtifactImportErrorV1> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
    Ok(hex::encode(bytes))
}
