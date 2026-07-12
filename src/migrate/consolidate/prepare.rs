use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::files::{remove_runtime_files, sqlite_sidecar};
use super::*;
use crate::branch_meta::BranchEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PrepareStop {
    TargetCopy,
    SourceBranch(usize),
    BranchMetaWrite,
    Publish,
}

pub(super) fn prepare_destination(resolved: &ResolvedPlan) -> Result<()> {
    prepare_destination_with_stop(resolved, None)
}

pub(super) fn prepare_destination_with_stop(
    resolved: &ResolvedPlan,
    stop: Option<PrepareStop>,
) -> Result<()> {
    let destination = &resolved.report.destination_data_root;
    if destination.exists() {
        validate_prepared_root(resolved, destination)?;
        return Ok(());
    }

    let staging = staging_root(resolved)?;
    create_private_directory(&staging)?;
    copy_target_tree(resolved, &staging)?;
    remove_runtime_files(&staging)?;
    maybe_stop(stop, PrepareStop::TargetCopy)?;

    let mut merged_meta = resolved.target_meta.clone();
    preserve_source_branch_graphs(resolved, &staging, &mut merged_meta, stop)?;
    branch_meta::save_branch_meta(&staging, &merged_meta).map_err(io_error)?;
    maybe_stop(stop, PrepareStop::BranchMetaWrite)?;
    validate_prepared_root(resolved, &staging)?;
    sync_directory(&staging)?;

    fs::rename(&staging, destination).map_err(io_error)?;
    files::sync_parent_directory(
        destination
            .parent()
            .ok_or_else(|| config_error("destination shard has no parent"))?,
    )?;
    maybe_stop(stop, PrepareStop::Publish)?;
    Ok(())
}

fn copy_target_tree(resolved: &ResolvedPlan, staging: &Path) -> Result<()> {
    for (relative, source) in relative_file_map(&resolved.target_layout.data_root)? {
        if relative == Path::new(storage::BRANCH_META_FILENAME) || is_runtime_lock(&relative) {
            continue;
        }
        files::copy_file_exact(&source, &staging.join(relative))?;
    }
    Ok(())
}

fn preserve_source_branch_graphs(
    resolved: &ResolvedPlan,
    staging: &Path,
    merged: &mut BranchMeta,
    stop: Option<PrepareStop>,
) -> Result<()> {
    let prefix = format!("consolidated/{}/", resolved.report.source.project_id);
    let mut branches = resolved.source_meta.branches.iter().collect::<Vec<_>>();
    branches.sort_by_key(|(name, _)| *name);
    for (index, (branch_name, entry)) in branches.into_iter().enumerate() {
        let source_db = resolved.source_layout.data_root.join(&entry.db_file);
        let merged_name = format!("{prefix}{branch_name}");
        let stem = source_branch_stem(&merged_name);
        if stem.is_empty() {
            return Err(config_error(format!(
                "source branch '{branch_name}' cannot be represented safely"
            )));
        }
        let relative = PathBuf::from("branches").join(format!("{stem}.db"));
        copy_sqlite_family_exact(&source_db, &staging.join(&relative))?;
        merged.branches.insert(
            merged_name,
            BranchEntry {
                db_file: relative.to_string_lossy().replace('\\', "/"),
                parent: entry
                    .parent
                    .as_deref()
                    .map(|parent| format!("{prefix}{parent}")),
                created_at: entry.created_at.clone(),
                last_synced_at: entry.last_synced_at.clone(),
                gc_protected: true,
            },
        );
        maybe_stop(stop, PrepareStop::SourceBranch(index + 1))?;
    }
    Ok(())
}

fn validate_prepared_root(resolved: &ResolvedPlan, root: &Path) -> Result<()> {
    let (expected_meta, expected_files) = expected_prepared_files(resolved)?;
    let actual_meta = branch_meta::load_branch_meta(root)
        .ok_or_else(|| config_error("prepared destination branch metadata is invalid"))?;
    if serde_json::to_value(&actual_meta).map_err(|error| config_error(error.to_string()))?
        != serde_json::to_value(&expected_meta).map_err(|error| config_error(error.to_string()))?
    {
        return Err(config_error(
            "prepared destination branch metadata differs from the deterministic plan",
        ));
    }

    let actual = relative_file_map(root)?;
    let actual_paths = actual.keys().cloned().collect::<BTreeSet<_>>();
    let mut expected_paths = expected_files.keys().cloned().collect::<BTreeSet<_>>();
    expected_paths.insert(PathBuf::from(storage::BRANCH_META_FILENAME));
    if actual_paths != expected_paths {
        return Err(config_error(format!(
            "prepared destination file inventory differs from the deterministic plan: expected {expected_paths:?}, found {actual_paths:?}"
        )));
    }
    for (relative, source) in expected_files {
        if file_digest(&source)? != file_digest(&root.join(&relative))? {
            return Err(config_error(format!(
                "prepared destination artifact '{}' differs from '{}'",
                root.join(relative).display(),
                source.display()
            )));
        }
    }
    Ok(())
}

fn expected_prepared_files(
    resolved: &ResolvedPlan,
) -> Result<(BranchMeta, BTreeMap<PathBuf, PathBuf>)> {
    let mut files = BTreeMap::new();
    for (relative, source) in relative_file_map(&resolved.target_layout.data_root)? {
        if relative == Path::new(storage::BRANCH_META_FILENAME) || is_runtime_lock(&relative) {
            continue;
        }
        files.insert(relative, source);
    }

    let prefix = format!("consolidated/{}/", resolved.report.source.project_id);
    let mut meta = resolved.target_meta.clone();
    let mut branches = resolved.source_meta.branches.iter().collect::<Vec<_>>();
    branches.sort_by_key(|(name, _)| *name);
    for (branch_name, entry) in branches {
        let source_db = resolved.source_layout.data_root.join(&entry.db_file);
        let merged_name = format!("{prefix}{branch_name}");
        let relative =
            PathBuf::from("branches").join(format!("{}.db", source_branch_stem(&merged_name)));
        for (suffix, source) in [
            ("", source_db.clone()),
            ("-wal", sqlite_sidecar(&source_db, "-wal")),
            ("-shm", sqlite_sidecar(&source_db, "-shm")),
        ] {
            if source.is_file() {
                let target = if suffix.is_empty() {
                    relative.clone()
                } else {
                    sqlite_sidecar(&relative, suffix)
                };
                files.insert(target, source);
            }
        }
        meta.branches.insert(
            merged_name,
            BranchEntry {
                db_file: relative.to_string_lossy().replace('\\', "/"),
                parent: entry
                    .parent
                    .as_deref()
                    .map(|parent| format!("{prefix}{parent}")),
                created_at: entry.created_at.clone(),
                last_synced_at: entry.last_synced_at.clone(),
                gc_protected: true,
            },
        );
    }
    Ok((meta, files))
}

fn staging_root(resolved: &ResolvedPlan) -> Result<PathBuf> {
    let parent = resolved
        .report
        .destination_data_root
        .parent()
        .ok_or_else(|| config_error("destination shard has no parent"))?;
    Ok(parent.join(format!(".{}.staging", resolved.report.migration_id)))
}

fn source_branch_stem(branch_name: &str) -> String {
    let base = crate::branch::sanitize_branch_name(branch_name)
        .chars()
        .take(180)
        .collect::<String>();
    if base.is_empty() {
        return base;
    }
    let mut hash = Sha256::new();
    hash.update(branch_name.as_bytes());
    format!("{base}_{}", &hex::encode(hash.finalize())[..10])
}

fn maybe_stop(stop: Option<PrepareStop>, point: PrepareStop) -> Result<()> {
    if stop == Some(point) {
        Err(config_error(format!(
            "synthetic interruption inside destination preparation at {point:?}"
        )))
    } else {
        Ok(())
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => return Err(config_error("destination staging path is not a directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(io_error)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}
