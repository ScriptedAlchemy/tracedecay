use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_sha256;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write};

use super::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionGenerationV1,
    CodeGenerationRetentionReceiptMaterialV1, CodeGenerationRetentionReceiptV1,
    GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY, GRAPH_REPLAY_RELEASE_SCHEMA,
    MAX_CODE_GENERATION_RETENTION_BATCH_V1, MAX_TRANSACTION_BYTES, RECEIPT_SCHEMA,
    RECEIPTS_DIRECTORY, storage, sync_directory,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeGenerationGraphReplayReleaseV1 {
    pub schema: String,
    pub receipt_digest: String,
    pub generation: CodeGenerationRetentionGenerationV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeGenerationGraphReplayReleasePageV1 {
    pub releases: Vec<CodeGenerationGraphReplayReleaseV1>,
    pub continuation: Option<String>,
}

fn release_path(
    store_root: &Path,
    release: &CodeGenerationGraphReplayReleaseV1,
) -> Result<PathBuf, CodeGenerationRetentionErrorV1> {
    let digest = canonical_sha256(&(
        "tracedecay.graph-replay-release-file.v1",
        &release.receipt_digest,
        &release.generation.generation_id,
        &release.generation.generation_file,
    ))
    .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    Ok(store_root
        .join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY)
        .join(format!(
            "release-{}.json",
            digest
                .as_str()
                .strip_prefix("sha256:")
                .unwrap_or(digest.as_str())
        )))
}

pub(super) fn write_events(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let root = store_root.join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY);
    std::fs::create_dir_all(&root).map_err(storage)?;
    for generation in &receipt.deleted_generations {
        let release = CodeGenerationGraphReplayReleaseV1 {
            schema: GRAPH_REPLAY_RELEASE_SCHEMA.to_owned(),
            receipt_digest: receipt.receipt_digest.clone(),
            generation: generation.clone(),
        };
        let bytes = serde_json::to_vec(&release).map_err(|error| {
            CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "graph replay release serialization failed: {error}"
            ))
        })?;
        atomic_write(
            &release_path(store_root, &release)?,
            "graph-replay-release",
            &bytes,
            DirectorySyncPolicy::TolerateUnsupported,
        )
        .map_err(storage)?;
    }
    sync_directory(&root)
}

/// Whether the durable release event for one retired generation is still
/// queued. A consumed event is the graph reconciler's typed retirement
/// confirmation: the replay pool copy has been released and must never be
/// re-exposed for this receipt.
pub(super) fn release_event_exists(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
    generation: &CodeGenerationRetentionGenerationV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    let release = CodeGenerationGraphReplayReleaseV1 {
        schema: GRAPH_REPLAY_RELEASE_SCHEMA.to_owned(),
        receipt_digest: receipt.receipt_digest.clone(),
        generation: generation.clone(),
    };
    match std::fs::symlink_metadata(release_path(store_root, &release)?) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "graph replay release for '{}' is not a regular file",
            generation.generation_file
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage(error)),
    }
}

pub(super) fn remove_events(
    store_root: &Path,
    receipt: &CodeGenerationRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    for generation in &receipt.deleted_generations {
        let release = CodeGenerationGraphReplayReleaseV1 {
            schema: GRAPH_REPLAY_RELEASE_SCHEMA.to_owned(),
            receipt_digest: receipt.receipt_digest.clone(),
            generation: generation.clone(),
        };
        match std::fs::remove_file(release_path(store_root, &release)?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
    }
    let root = store_root.join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY);
    if root.is_dir() {
        sync_directory(&root)?;
    }
    Ok(())
}

pub fn code_generation_graph_replay_release_page(
    store_root: &Path,
    after: Option<&str>,
) -> Result<CodeGenerationGraphReplayReleasePageV1, CodeGenerationRetentionErrorV1> {
    let root = store_root.join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CodeGenerationGraphReplayReleasePageV1 {
                releases: Vec::new(),
                continuation: None,
            });
        }
        Err(error) => return Err(storage(error)),
    };
    let mut selected = BTreeMap::new();
    let mut has_more = false;
    for entry in entries {
        let entry = entry.map_err(storage)?;
        let name = entry.file_name().into_string().map_err(|_| {
            CodeGenerationRetentionErrorV1::UnsafeState(
                "graph replay release filename is not UTF-8".to_owned(),
            )
        })?;
        if after.is_some_and(|cursor| name.as_str() <= cursor) {
            continue;
        }
        selected.insert(name, entry.path());
        if selected.len() > MAX_CODE_GENERATION_RETENTION_BATCH_V1 {
            selected.pop_last();
            has_more = true;
        }
    }
    let continuation = if has_more {
        Some(
            selected
                .last_key_value()
                .map(|(name, _)| name.clone())
                .ok_or_else(|| {
                    CodeGenerationRetentionErrorV1::UnsafeState(
                        "graph replay release pagination lost its bounded selection".to_owned(),
                    )
                })?,
        )
    } else {
        None
    };
    let mut releases = Vec::with_capacity(selected.len());
    for (_, path) in selected {
        let bytes = read_bounded_regular_file(&path, "graph replay release")?;
        let release: CodeGenerationGraphReplayReleaseV1 =
            serde_json::from_slice(&bytes).map_err(|error| {
                CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "graph replay release is unreadable: {error}"
                ))
            })?;
        if release.schema != GRAPH_REPLAY_RELEASE_SCHEMA
            || release_path(store_root, &release)? != path
        {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "graph replay release identity is invalid".to_owned(),
            ));
        }
        validate_receipt(store_root, &release)?;
        releases.push(release);
    }
    releases.sort_by(|left, right| {
        (&left.receipt_digest, &left.generation.generation_id)
            .cmp(&(&right.receipt_digest, &right.generation.generation_id))
    });
    Ok(CodeGenerationGraphReplayReleasePageV1 {
        releases,
        continuation,
    })
}

fn read_bounded_regular_file(
    path: &Path,
    subject: &str,
) -> Result<Vec<u8>, CodeGenerationRetentionErrorV1> {
    let metadata = path.symlink_metadata().map_err(storage)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_TRANSACTION_BYTES {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{subject} is not a bounded regular file"
        )));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{subject} length exceeds addressable memory"
        ))
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)
        .map_err(storage)?
        .take(MAX_TRANSACTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{subject} changed during its bounded read"
        )));
    }
    Ok(bytes)
}

fn validate_receipt(
    store_root: &Path,
    release: &CodeGenerationGraphReplayReleaseV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let receipt_path = store_root
        .join(RECEIPTS_DIRECTORY)
        .join(format!("receipt-{}.json", release.receipt_digest));
    let bytes = read_bounded_regular_file(&receipt_path, "graph replay release receipt")?;
    let receipt: CodeGenerationRetentionReceiptV1 =
        serde_json::from_slice(&bytes).map_err(|error| {
            CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "graph replay release receipt is unreadable: {error}"
            ))
        })?;
    let material = CodeGenerationRetentionReceiptMaterialV1 {
        schema: RECEIPT_SCHEMA,
        active_generation_id: &receipt.active_generation_id,
        vector_readable_sources: &receipt.vector_readable_sources,
        rollback_floor: receipt.rollback_floor,
        deleted_generations: &receipt.deleted_generations,
        reclaimed_bytes: receipt.reclaimed_bytes,
        completed_at_micros: receipt.completed_at_micros,
    };
    let digest = canonical_sha256(&material)
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    if receipt.schema != RECEIPT_SCHEMA
        || digest.as_str().strip_prefix("sha256:") != Some(receipt.receipt_digest.as_str())
        || receipt.receipt_digest != release.receipt_digest
        || !receipt.deleted_generations.contains(&release.generation)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "graph replay release does not match durable deletion evidence".to_owned(),
        ));
    }
    Ok(())
}

pub fn complete_code_generation_graph_replay_release(
    store_root: &Path,
    release: &CodeGenerationGraphReplayReleaseV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    std::fs::remove_file(release_path(store_root, release)?).map_err(storage)?;
    sync_directory(&store_root.join(GRAPH_REPLAY_RELEASE_QUEUE_DIRECTORY))
}
