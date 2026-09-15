// Compiled twice on purpose: `build.rs` mounts this file as a `#[path]`
// module to stage the dashboard bundle it embeds, and
// `tests/core_cli_suite/dashboard_bundle_test.rs` mounts the same file so the tests exercise
// the code the build script runs rather than a copy that can drift. Both hosts
// also mount `dashboard_manifest.rs` beside it under that name. Items are
// otherwise fully qualified so the file stays self-contained.

/// Fixed cross-tool bundle digest contract (`scripts/check-dashboard-bundle.py`
/// implements the same algorithm): sha256 over this prefix, then for each
/// manifest-validated relative path in sorted order the UTF-8 path bytes, one
/// 0x00 byte, the file byte length as u64 little-endian, then the file bytes.
pub const BUNDLE_DIGEST_PREFIX: &[u8] = b"tracedecay-dashboard-bundle-v1\0";

/// Transient producer output inside the bundle store. A bundle lives here only
/// until it validates; a build killed mid-way leaves at most this directory
/// behind, never a half-written digest directory.
pub const STAGING_DIR: &str = "staging";

/// Record of the last frontend build in a store: the inputs fingerprint it was
/// built from and the digest of the bundle it produced.
pub const BUILD_RECORD_FILE: &str = "built-from";

/// One validated bundle, renamed to its own digest inside the store. Nothing
/// writes to a digest directory after promotion: a new bundle is staged and
/// promoted beside it, and the compiler reads only the promoted bytes.
#[derive(Debug, PartialEq, Eq)]
pub struct StagedBundle {
    /// `<store>/<digest_hex>`.
    pub root: std::path::PathBuf,
    /// Manifest-validated relative asset paths, sorted.
    pub asset_paths: Vec<String>,
    pub digest_hex: String,
}

/// Which frontend inputs the last staged bundle was built from.
#[derive(Debug, PartialEq, Eq)]
pub struct BuildRecord {
    pub inputs_fingerprint: String,
    pub bundle_digest: String,
}

pub fn bundle_digest(
    root: &std::path::Path,
    sorted_relative_paths: &[String],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, BUNDLE_DIGEST_PREFIX);
    for relative in sorted_relative_paths {
        let bytes = std::fs::read(root.join(relative)).map_err(|error| {
            format!("failed to read dashboard asset {relative} for the bundle digest: {error}")
        })?;
        hash_entry(&mut hasher, relative, &bytes);
    }
    Ok(hex(sha2::Digest::finalize(hasher).as_slice()))
}

/// Content fingerprint of the frontend inputs a bundle is built from. Files are
/// hashed by repository-relative path; directories are walked recursively in
/// sorted order; an absent input contributes nothing, so adding or deleting a
/// file changes the fingerprint exactly like editing one does.
pub fn inputs_fingerprint(
    repository_root: &std::path::Path,
    inputs: &[&str],
) -> Result<String, Box<dyn std::error::Error>> {
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    sha2::Digest::update(&mut hasher, b"tracedecay-dashboard-inputs-v1\0");
    for input in inputs {
        let path = repository_root.join(input);
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            hash_tree(&mut hasher, &path, input)?;
        } else {
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            hash_entry(&mut hasher, input, &bytes);
        }
    }
    Ok(hex(sha2::Digest::finalize(hasher).as_slice()))
}

fn hash_tree(
    hasher: &mut sha2::Sha256,
    dir: &std::path::Path,
    relative: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|error| format!("failed to list {}: {error}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to list {}: {error}", dir.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(format!(
                "dashboard input {} has a non-UTF-8 file name {name:?}",
                dir.display()
            )
            .into());
        };
        let path = entry.path();
        let child = format!("{relative}/{name}");
        let metadata = std::fs::metadata(&path)
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
        if metadata.is_dir() {
            hash_tree(hasher, &path, &child)?;
        } else {
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            hash_entry(hasher, &child, &bytes);
        }
    }
    Ok(())
}

fn hash_entry(hasher: &mut sha2::Sha256, relative: &str, bytes: &[u8]) {
    sha2::Digest::update(hasher, relative.as_bytes());
    sha2::Digest::update(hasher, [0u8]);
    sha2::Digest::update(hasher, (bytes.len() as u64).to_le_bytes());
    sha2::Digest::update(hasher, bytes);
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Reopens the promoted bundle named `digest_hex`, revalidating its manifest
/// and recomputing its digest. `Ok(None)` when no such bundle is staged; an
/// error when a directory exists but no longer matches its own name, because a
/// store nothing else may write to must never silently self-heal.
pub fn open(
    store: &std::path::Path,
    digest_hex: &str,
) -> Result<Option<StagedBundle>, Box<dyn std::error::Error>> {
    let root = store.join(digest_hex);
    if !root.is_dir() {
        return Ok(None);
    }
    let asset_paths = super::dashboard_manifest::dashboard_asset_paths(&root)?;
    let actual = bundle_digest(&root, &asset_paths)?;
    if actual != digest_hex {
        return Err(format!(
            "staged dashboard bundle {} has digest {actual}, not the digest it is named after; \
             the bundle store was modified outside the build — remove it to rebuild",
            root.display()
        )
        .into());
    }
    Ok(Some(StagedBundle {
        root,
        asset_paths,
        digest_hex: digest_hex.to_owned(),
    }))
}

/// Path a producer must write its complete bundle to before [`promote`].
/// Any leftover from an interrupted run is removed so the producer starts from
/// an empty directory.
pub fn prepare_staging(store: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let staging = store.join(STAGING_DIR);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    Ok(staging)
}

/// Validates the finished producer output in the store's staging directory,
/// digests it, and renames it to `<store>/<digest>` in one step. Every other
/// digest directory in the store is pruned: the generated module only ever
/// names the current bundle, and a rerun regenerates it.
pub fn promote(store: &std::path::Path) -> Result<StagedBundle, Box<dyn std::error::Error>> {
    let staging = store.join(STAGING_DIR);
    let asset_paths = super::dashboard_manifest::dashboard_asset_paths(&staging)?;
    let digest_hex = bundle_digest(&staging, &asset_paths)?;
    let root = store.join(&digest_hex);
    if root.exists() {
        std::fs::remove_dir_all(&root).map_err(|error| {
            format!(
                "failed to replace staged dashboard bundle {}: {error}",
                root.display()
            )
        })?;
    }
    std::fs::rename(&staging, &root).map_err(|error| {
        format!(
            "failed to promote dashboard bundle {} to {}: {error}",
            staging.display(),
            root.display()
        )
    })?;
    for entry in std::fs::read_dir(store)
        .map_err(|error| format!("failed to list bundle store {}: {error}", store.display()))?
    {
        let entry = entry
            .map_err(|error| format!("failed to list bundle store {}: {error}", store.display()))?;
        let path = entry.path();
        if path.is_dir() && entry.file_name() != digest_hex.as_str() {
            std::fs::remove_dir_all(&path).map_err(|error| {
                format!(
                    "failed to prune superseded dashboard bundle {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(StagedBundle {
        root,
        asset_paths,
        digest_hex,
    })
}

/// Stages a copy of a producer's already-complete bundle (the manifest and
/// every file it lists) and promotes it. The caller compares the promoted
/// digest against the digest the producer advertised: a producer that is still
/// rewriting `source` while the copy runs yields a mismatch and is refused,
/// never mixed bytes under a claimed digest.
pub fn stage_copy(
    store: &std::path::Path,
    source: &std::path::Path,
) -> Result<StagedBundle, Box<dyn std::error::Error>> {
    let asset_paths = super::dashboard_manifest::dashboard_asset_paths(source)?;
    let staging = prepare_staging(store)
        .map_err(|error| format!("failed to prepare {}: {error}", store.display()))?;
    let manifest = super::dashboard_manifest::DASHBOARD_ASSET_MANIFEST.to_owned();
    for relative in asset_paths.iter().chain(std::iter::once(&manifest)) {
        let target = staging.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        std::fs::copy(source.join(relative), &target).map_err(|error| {
            format!(
                "failed to copy dashboard asset {relative} from {}: {error}",
                source.display()
            )
        })?;
    }
    promote(store)
}

/// `None` when the store has no readable, well-formed record: the only
/// consequence is one frontend rebuild.
pub fn read_build_record(store: &std::path::Path) -> Option<BuildRecord> {
    let text = std::fs::read_to_string(store.join(BUILD_RECORD_FILE)).ok()?;
    let mut lines = text.lines();
    let inputs_fingerprint = lines.next()?.to_owned();
    let bundle_digest = lines.next()?.to_owned();
    if lines.next().is_some() || inputs_fingerprint.len() != 64 || bundle_digest.len() != 64 {
        return None;
    }
    Some(BuildRecord {
        inputs_fingerprint,
        bundle_digest,
    })
}

pub fn write_build_record(store: &std::path::Path, record: &BuildRecord) -> std::io::Result<()> {
    std::fs::write(
        store.join(BUILD_RECORD_FILE),
        format!("{}\n{}\n", record.inputs_fingerprint, record.bundle_digest),
    )
}
