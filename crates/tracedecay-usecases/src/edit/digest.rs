use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::{
    DirectorySyncPolicy, EffectId, read_bounded, with_owned_temp_publish,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use tracedecay_runtime_core::errors::Result;

use super::verify::{application_contract_error, config_error, domain_error, io_error};
use super::{
    MAX_DURABLE_RECORD_BYTES, SOURCE_EDIT_RECOVERY_DIGEST_DOMAIN_V1,
    SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1,
};

pub(super) fn normalize_candidate_files(root: &Path, files: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = Vec::with_capacity(files.len());
    for file in files {
        let path = Path::new(&file);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
        {
            return Err(config_error(
                "source edit candidate path is outside the authorized worktree",
            ));
        }
        let value = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value),
                _ => None,
            })
            .collect::<PathBuf>();
        crate::tracedecay::validate_source_edit_candidate_parent(root, &value)?;
        normalized.push(value.to_string_lossy().into_owned());
    }
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() {
        return Err(config_error(
            "source edit preview resolved no candidate files",
        ));
    }
    Ok(normalized)
}

pub(super) fn source_edit_state_digest(root: &Path, files: &[String]) -> Result<ManifestDigest> {
    let mut states = Vec::with_capacity(files.len());
    for relative in files {
        let state = match crate::tracedecay::read_source_edit_candidate(root, Path::new(relative))?
        {
            Some(bytes) => Some(hash_source_edit_content(&bytes)?),
            None => None,
        };
        states.push((relative, state));
    }
    canonical_sha256(&(SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1, states)).map_err(domain_error)
}

pub(super) fn source_edit_recovery_digest(
    files: &[crate::tracedecay::PlannedSourceEditFile],
) -> Result<ManifestDigest> {
    canonical_sha256(&(SOURCE_EDIT_RECOVERY_DIGEST_DOMAIN_V1, files)).map_err(domain_error)
}

pub(super) fn planned_source_edit_state_digest(
    files: &[String],
    planned_files: &[crate::tracedecay::PlannedSourceEditFile],
    intended: bool,
) -> Result<ManifestDigest> {
    let mut states = Vec::with_capacity(files.len());
    for relative in files {
        let mut matches = planned_files
            .iter()
            .filter(|planned| &planned.relative_path == relative);
        let planned = matches.next().ok_or_else(|| {
            config_error("source edit candidate is missing from its exact preview plan")
        })?;
        if matches.next().is_some() {
            return Err(config_error(
                "source edit candidate appears more than once in its exact preview plan",
            ));
        }
        let content = if intended {
            planned.intended.as_deref()
        } else {
            planned.expected.as_deref()
        };
        states.push((
            relative,
            content
                .map(|content| hash_source_edit_content(content.as_bytes()))
                .transpose()?,
        ));
    }
    canonical_sha256(&(SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1, states)).map_err(domain_error)
}

fn hash_source_edit_content(content: &[u8]) -> Result<ManifestDigest> {
    ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(content))))
        .map_err(domain_error)
}

pub(super) fn effect_id(
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    let digest = canonical_sha256(&("tracedecay.source-edit-effect-id.v1", key, input_digest))
        .map_err(domain_error)?;
    EffectId::new(format!(
        "effect.source-edit.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(application_contract_error)
}

pub(super) fn reconciliation_attempt_effect_id(
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    let digest = canonical_sha256(&(
        "tracedecay.source-edit-reconciliation-attempt-effect-id.v1",
        key,
        input_digest,
    ))
    .map_err(domain_error)?;
    EffectId::new(format!(
        "effect.source-edit-reconciliation.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(application_contract_error)
}

pub(super) fn persist_record<T: Serialize>(path: &Path, kind: &str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|error| config_error(error.to_string()))?;
    if bytes.len() > MAX_DURABLE_RECORD_BYTES {
        return Err(config_error("source edit durable record exceeds its bound"));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| io_error("create source edit durable directory", error))?;
    }
    with_owned_temp_publish(
        path,
        kind,
        |temporary, destination| {
            tracedecay_runtime_core::db::DatabaseAuthority::replace_file_atomically(
                temporary,
                destination,
                "source edit durable record",
            )
            .map_err(|error| std::io::Error::other(error.to_string()))
        },
        |output| output.write_all(&bytes),
        DirectorySyncPolicy::Strict,
    )
    .map_err(|error| io_error("persist source edit durable record", error))
}

pub(super) fn load_record<T>(path: &Path, kind: &'static str) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(bytes) =
        read_bounded(path, MAX_DURABLE_RECORD_BYTES).map_err(|error| io_error(kind, error))?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| config_error(format!("{kind} is malformed: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn expected_state_digest_covers_content_and_missing_files() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("present.rs"), b"one").unwrap();
        let files = vec!["missing.rs".to_owned(), "present.rs".to_owned()];
        let before = source_edit_state_digest(directory.path(), &files).unwrap();

        fs::write(directory.path().join("present.rs"), b"two").unwrap();
        let after = source_edit_state_digest(directory.path(), &files).unwrap();

        assert_ne!(before, after);
    }

    #[cfg(unix)]
    #[test]
    fn expected_state_digest_rejects_symlinked_candidate_parent() {
        use std::os::unix::fs::symlink;

        let project = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("lib.rs"), b"outside").unwrap();
        symlink(outside.path(), project.path().join("src")).unwrap();

        assert!(source_edit_state_digest(project.path(), &["src/lib.rs".to_owned()]).is_err());
        assert_eq!(fs::read(outside.path().join("lib.rs")).unwrap(), b"outside");
    }

    /// A canonicalized parent is not enough: the final component itself must
    /// never be followed, or a symlink planted inside the worktree hands the
    /// reader arbitrary bytes from outside it.
    #[cfg(unix)]
    #[test]
    fn expected_state_digest_rejects_symlinked_candidate_file() {
        use std::os::unix::fs::symlink;

        let project = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.rs");
        fs::write(&secret, b"outside").unwrap();
        fs::create_dir(project.path().join("src")).unwrap();
        symlink(&secret, project.path().join("src/lib.rs")).unwrap();

        assert!(source_edit_state_digest(project.path(), &["src/lib.rs".to_owned()]).is_err());
        assert_eq!(fs::read(&secret).unwrap(), b"outside");
    }
}
