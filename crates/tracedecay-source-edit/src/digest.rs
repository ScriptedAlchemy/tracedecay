use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::EffectId;
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, read_bounded, with_owned_temp_publish,
};

use tracedecay_domain::errors::Result;

use super::verify::{application_contract_error, config_error, domain_error, io_error};
use super::{
    MAX_DURABLE_RECORD_BYTES, SOURCE_EDIT_RECOVERY_DIGEST_DOMAIN_V1,
    SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1,
};

/// Canonical spelling of every source-edit candidate: `/`-joined normal
/// components, on every host.
///
/// This string is the candidate's identity — it is matched against the exact
/// preview plan, digested into the expected/predicted state, and written to
/// the durable journal. Rendering it through `PathBuf::to_string_lossy` made
/// that identity platform-dependent: the same edit spelled `src/b.rs` in its
/// plan came back as `src\b.rs` on Windows, so no candidate matched its own
/// plan and every apply failed as a missing candidate. Joining the components
/// explicitly (rather than replacing separators in the rendered string) keeps
/// a Unix filename that genuinely contains a backslash intact.
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
        let components = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>();
        let value = components.iter().collect::<PathBuf>();
        tracedecay_usecases::tracedecay::validate_source_edit_candidate_parent(root, &value)?;
        normalized.push(
            components
                .iter()
                .map(|component| component.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        );
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

#[hotpath::measure(label = "usecases.edit.state_digest")]
pub(super) fn source_edit_state_digest(root: &Path, files: &[String]) -> Result<ManifestDigest> {
    let mut states = Vec::with_capacity(files.len());
    for relative in files {
        let state = match tracedecay_usecases::tracedecay::read_source_edit_candidate(
            root,
            Path::new(relative),
        )? {
            Some(bytes) => {
                hotpath::gauge!("usecases.edit.digest_bytes").inc(bytes.len() as f64);
                Some(hash_source_edit_content(&bytes)?)
            }
            None => None,
        };
        states.push((relative, state));
    }
    canonical_sha256(&(SOURCE_EDIT_STATE_DIGEST_DOMAIN_V1, states)).map_err(domain_error)
}

pub(super) fn source_edit_recovery_digest(
    files: &[tracedecay_usecases::tracedecay::PlannedSourceEditFile],
) -> Result<ManifestDigest> {
    canonical_sha256(&(SOURCE_EDIT_RECOVERY_DIGEST_DOMAIN_V1, files)).map_err(domain_error)
}

#[hotpath::measure(label = "usecases.edit.planned_state_digest")]
pub(super) fn planned_source_edit_state_digest(
    files: &[String],
    planned_files: &[tracedecay_usecases::tracedecay::PlannedSourceEditFile],
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
    ManifestDigest::from_sha256_bytes(&Sha256::digest(content)).map_err(domain_error)
}

fn minted_effect_id(
    domain: &'static str,
    prefix: &'static str,
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    let digest = canonical_sha256(&(domain, key, input_digest)).map_err(domain_error)?;
    EffectId::new(format!(
        "{prefix}{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(application_contract_error)
}

pub(super) fn effect_id(
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    minted_effect_id(
        "tracedecay.source-edit-effect-id.v1",
        "effect.source-edit.",
        key,
        input_digest,
    )
}

pub(super) fn reconciliation_attempt_effect_id(
    key: &tracedecay_application::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    minted_effect_id(
        "tracedecay.source-edit-reconciliation-attempt-effect-id.v1",
        "effect.source-edit-reconciliation.",
        key,
        input_digest,
    )
}

#[hotpath::measure(label = "usecases.edit.persist_record")]
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

#[hotpath::measure(label = "usecases.edit.load_record")]
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
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    /// The candidate identity must be spelled the same way the preview plan
    /// spells it, on every host: the plan uses `/` separators, so normalizing
    /// through a native `PathBuf` rendering would desynchronize the two on
    /// Windows and no candidate would match its own plan.
    #[test]
    fn candidate_identity_is_slash_separated_on_every_host() {
        let project = tempdir().unwrap();
        fs::create_dir_all(project.path().join("src/nested")).unwrap();

        assert_eq!(
            normalize_candidate_files(
                project.path(),
                vec!["./src/nested/deep.rs".to_owned(), "src/b.rs".to_owned()]
            )
            .unwrap(),
            vec!["src/b.rs".to_owned(), "src/nested/deep.rs".to_owned()]
        );
    }

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
