use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_contracts::EffectId;
use tracedecay_domain::{ManifestDigest, canonical_sha256};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, read_bounded, with_owned_temp_publish,
};

use tracedecay_domain::errors::Result;

use super::file_authority::{SourceEditFileAuthority, read_source_edit_candidate};
use super::plan::PlannedSourceEditFile;
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
        SourceEditFileAuthority::open(root, &value)?;
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
        let state = match read_source_edit_candidate(root, Path::new(relative))? {
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
    files: &[PlannedSourceEditFile],
) -> Result<ManifestDigest> {
    canonical_sha256(&(SOURCE_EDIT_RECOVERY_DIGEST_DOMAIN_V1, files)).map_err(domain_error)
}

#[hotpath::measure(label = "usecases.edit.planned_state_digest")]
pub(super) fn planned_source_edit_state_digest(
    files: &[String],
    planned_files: &[PlannedSourceEditFile],
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
    key: &tracedecay_contracts::IdempotencyKey,
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
    key: &tracedecay_contracts::IdempotencyKey,
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
    key: &tracedecay_contracts::IdempotencyKey,
    input_digest: &ManifestDigest,
) -> Result<EffectId> {
    minted_effect_id(
        "tracedecay.source-edit-reconciliation-attempt-effect-id.v1",
        "effect.source-edit-reconciliation.",
        key,
        input_digest,
    )
}

/// `io::Write` sink that refuses the write which would carry it past `limit`,
/// so `serde_json::to_writer` stops encoding — and the buffer stops growing —
/// before an oversized record has been materialized just to be rejected.
struct BoundedRecordBytes {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedRecordBytes {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedRecordBytes {
    fn write(&mut self, incoming: &[u8]) -> std::io::Result<usize> {
        if incoming.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "source edit durable record exceeds its bound",
            ));
        }
        self.bytes.extend_from_slice(incoming);
        Ok(incoming.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[hotpath::measure(label = "usecases.edit.persist_record")]
pub(super) fn persist_record<T: Serialize>(path: &Path, kind: &str, value: &T) -> Result<()> {
    let mut sink = BoundedRecordBytes::new(MAX_DURABLE_RECORD_BYTES);
    if let Err(error) = serde_json::to_writer(&mut sink, value) {
        if sink.exceeded {
            return Err(config_error("source edit durable record exceeds its bound"));
        }
        return Err(config_error(error.to_string()));
    }
    let bytes = sink.bytes;
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

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct FixtureRecord {
        content: String,
    }

    #[test]
    fn persisted_records_round_trip_with_canonical_bytes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("journal.json");
        let record = FixtureRecord {
            content: "a\"b\\c\n".repeat(512),
        };

        persist_record(&path, "fixture", &record).unwrap();

        assert_eq!(
            fs::read(&path).unwrap(),
            serde_json::to_vec(&record).unwrap()
        );
        assert_eq!(
            load_record::<FixtureRecord>(&path, "fixture").unwrap(),
            Some(record)
        );
    }

    /// The bound applies to the encoded output, so content whose raw length
    /// is under the limit but whose JSON escaping is not must be refused as
    /// well — and the refusal must not touch the journal already on disk.
    #[test]
    fn oversized_records_are_refused_before_publication() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("journal.json");
        let existing = FixtureRecord {
            content: "keep".to_owned(),
        };
        persist_record(&path, "fixture", &existing).unwrap();
        let before = fs::read(&path).unwrap();

        let escaped = FixtureRecord {
            content: "\"".repeat(MAX_DURABLE_RECORD_BYTES * 3 / 4),
        };
        let oversized = FixtureRecord {
            content: "a".repeat(MAX_DURABLE_RECORD_BYTES + 1),
        };
        for record in [&escaped, &oversized] {
            let error = persist_record(&path, "fixture", record).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("source edit durable record exceeds its bound"),
                "{error}"
            );
        }

        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            1,
            "a refused record leaves no scratch file behind"
        );
    }

    /// `load_record` reads through the private-fs bounded primitive, so a
    /// missing journal is a typed absence while an oversized one is refused
    /// before its bytes are deserialized.
    #[test]
    fn load_record_distinguishes_missing_from_oversized_journals() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("journal.json");

        assert_eq!(
            load_record::<FixtureRecord>(&path, "source edit journal").unwrap(),
            None
        );

        fs::write(&path, vec![b'"'; MAX_DURABLE_RECORD_BYTES + 1]).unwrap();
        let error = load_record::<FixtureRecord>(&path, "source edit journal").unwrap_err();
        assert!(error.to_string().contains("source edit journal"), "{error}");
    }

    /// A journal path that has become a symlink hands the reader bytes from
    /// wherever the link points; the read is bound to the opened object and
    /// refuses the link itself instead of following it.
    #[cfg(unix)]
    #[test]
    fn load_record_refuses_a_symlinked_journal() {
        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = outside.path().join("journal.json");
        let record = FixtureRecord {
            content: "outside".to_owned(),
        };
        persist_record(&target, "fixture", &record).unwrap();
        let link = directory.path().join("journal.json");
        symlink(&target, &link).unwrap();

        assert!(load_record::<FixtureRecord>(&link, "source edit journal").is_err());
        assert_eq!(
            load_record::<FixtureRecord>(&target, "source edit journal").unwrap(),
            Some(record)
        );
    }

    #[test]
    fn the_bounded_sink_stops_encoding_at_its_limit() {
        let mut sink = BoundedRecordBytes::new(16);

        assert!(serde_json::to_writer(&mut sink, &"x".repeat(64)).is_err());

        assert!(sink.exceeded);
        assert!(sink.bytes.len() <= 16, "{}", sink.bytes.len());

        let mut sink = BoundedRecordBytes::new(16);
        serde_json::to_writer(&mut sink, &"x".repeat(14)).unwrap();
        assert!(!sink.exceeded);
        assert_eq!(sink.bytes.len(), 16);
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
