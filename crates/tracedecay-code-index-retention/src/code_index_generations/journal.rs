//! One bounded, schema-validated crash journal shared by every retention
//! transaction family.
//!
//! Each family (generation, text artifact, scope, binding-cleanup intent)
//! describes itself with a [`BoundedJournalSpec`]; the persist/load/clear
//! machinery is written once so a hardening fix can never drift between
//! copies again.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, atomic_write};

use super::{CodeGenerationRetentionErrorV1, storage, sync_directory};

/// Static description of one journal family. `label` prefixes every error so
/// a failure names its exact journal; `validate` is the family's schema and
/// invariant authority, enforced on both persist and load so a journal that
/// round-trips is always internally consistent.
pub(super) struct BoundedJournalSpec<T> {
    pub(super) file_name: &'static str,
    pub(super) max_bytes: u64,
    pub(super) label: &'static str,
    pub(super) write_context: &'static str,
    pub(super) validate: fn(&T) -> Result<(), CodeGenerationRetentionErrorV1>,
}

pub(super) fn journal_path<T>(store_root: &Path, spec: &BoundedJournalSpec<T>) -> PathBuf {
    store_root.join(spec.file_name)
}

#[hotpath::measure(label = "code_index_retention.persist_journal")]
pub(super) fn persist_journal<T: Serialize>(
    store_root: &Path,
    spec: &BoundedJournalSpec<T>,
    value: &T,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    (spec.validate)(value)?;
    let bytes = serde_json::to_vec(value).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} serialization failed: {error}",
            spec.label
        ))
    })?;
    atomic_write(
        &journal_path(store_root, spec),
        spec.write_context,
        &bytes,
        DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(storage)
}

/// Load a journal without trusting the path by name.
///
/// On unix the file is opened once with `O_NOFOLLOW`, type and size are taken
/// from that handle (`fstat`), and bytes are read from the same handle. A
/// planted symlink at the journal path is refused at open (`ELOOP`) instead of
/// being followed. Intermediate path components can still be symlinks — this
/// is the same final-component guarantee as `O_NOFOLLOW`, not a full
/// no-follow walk. `tracedecay_private_fs::open_private_file` is not reused:
/// it requires a writable owner-private `0o600` handle, which would reject a
/// readable journal.
///
/// On non-unix builds there is no `O_NOFOLLOW` on `OpenOptions`. Those paths
/// still refuse a symlink or directory observed via `symlink_metadata`, then
/// `std::fs::read` the path. A symlink planted in the window between those
/// two calls can still be followed.
///
/// The byte bound is enforced from the observed size before the body is
/// materialized, and the length is re-verified after the read so a file that
/// changes mid-read fails closed rather than parsing a hybrid.
#[hotpath::measure(label = "code_index_retention.load_journal")]
pub(super) fn load_journal<T: DeserializeOwned>(
    store_root: &Path,
    spec: &BoundedJournalSpec<T>,
) -> Result<Option<T>, CodeGenerationRetentionErrorV1> {
    let path = journal_path(store_root, spec);
    let Some(bytes) = read_journal_bytes(&path, spec.max_bytes, spec.label)? else {
        return Ok(None);
    };
    let value = serde_json::from_slice::<T>(&bytes).map_err(|error| {
        CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{} '{}' is unreadable: {error}",
            spec.label,
            path.display()
        ))
    })?;
    (spec.validate)(&value)?;
    Ok(Some(value))
}

fn journal_not_regular(label: &str, path: &Path) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::UnsafeState(format!(
        "{label} '{}' is not a bounded regular file",
        path.display()
    ))
}

fn journal_oversized(label: &str, path: &Path) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::UnsafeState(format!(
        "{label} '{}' exceeds the bounded journal size",
        path.display()
    ))
}

fn journal_changed(label: &str, path: &Path) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::UnsafeState(format!(
        "{label} '{}' changed during read",
        path.display()
    ))
}

#[cfg(unix)]
fn read_journal_bytes(
    path: &Path,
    max_bytes: u64,
    label: &str,
) -> Result<Option<Vec<u8>>, CodeGenerationRetentionErrorV1> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if is_unix_symlink_open_error(&error) => {
            return Err(journal_not_regular(label, path));
        }
        Err(error) => return Err(storage(error)),
    };
    let metadata = file.metadata().map_err(storage)?;
    if !metadata.file_type().is_file() {
        return Err(journal_not_regular(label, path));
    }
    if metadata.len() > max_bytes {
        return Err(journal_oversized(label, path));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(journal_changed(label, path));
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn is_unix_symlink_open_error(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn read_journal_bytes(
    path: &Path,
    max_bytes: u64,
    label: &str,
) -> Result<Option<Vec<u8>>, CodeGenerationRetentionErrorV1> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(journal_not_regular(label, path));
    }
    if metadata.len() > max_bytes {
        return Err(journal_oversized(label, path));
    }
    let bytes = std::fs::read(path).map_err(storage)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(journal_changed(label, path));
    }
    Ok(Some(bytes))
}

pub(super) fn clear_journal<T>(
    store_root: &Path,
    spec: &BoundedJournalSpec<T>,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    match std::fs::remove_file(journal_path(store_root, spec)) {
        Ok(()) => sync_directory(store_root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct FixtureJournalV1 {
        value: u32,
    }

    fn fixture_spec() -> BoundedJournalSpec<FixtureJournalV1> {
        BoundedJournalSpec {
            file_name: ".fixture-retention-journal-v1.json",
            max_bytes: 64,
            label: "fixture journal",
            write_context: "fixture-retention-journal",
            validate: |_| Ok(()),
        }
    }

    #[test]
    fn load_journal_round_trips_a_regular_file() {
        let store = tempfile::tempdir().expect("store");
        let spec = fixture_spec();
        persist_journal(store.path(), &spec, &FixtureJournalV1 { value: 7 })
            .expect("persist fixture journal");
        assert_eq!(
            load_journal(store.path(), &spec).expect("load fixture journal"),
            Some(FixtureJournalV1 { value: 7 })
        );
    }

    #[test]
    fn load_journal_returns_none_when_missing() {
        let store = tempfile::tempdir().expect("store");
        assert_eq!(
            load_journal(store.path(), &fixture_spec()).expect("missing journal"),
            None
        );
    }

    #[test]
    fn load_journal_refuses_a_directory() {
        let store = tempfile::tempdir().expect("store");
        let spec = fixture_spec();
        std::fs::create_dir(journal_path(store.path(), &spec)).expect("directory at journal path");
        let error = load_journal(store.path(), &spec).expect_err("directory must fail closed");
        assert!(
            matches!(error, CodeGenerationRetentionErrorV1::UnsafeState(ref message) if message.contains("not a bounded regular file")),
            "{error:?}"
        );
    }

    #[test]
    fn load_journal_refuses_an_oversized_file() {
        let store = tempfile::tempdir().expect("store");
        let spec = fixture_spec();
        std::fs::write(journal_path(store.path(), &spec), vec![b'x'; 65])
            .expect("oversized journal");
        let error =
            load_journal(store.path(), &spec).expect_err("oversized journal must fail closed");
        assert!(
            matches!(error, CodeGenerationRetentionErrorV1::UnsafeState(ref message) if message.contains("exceeds the bounded journal size")),
            "{error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn load_journal_refuses_a_planted_symlink() {
        let store = tempfile::tempdir().expect("store");
        let spec = fixture_spec();
        let target = store.path().join("outside.json");
        std::fs::write(&target, r#"{"value":1}"#).expect("write symlink target");
        std::os::unix::fs::symlink(&target, journal_path(store.path(), &spec))
            .expect("plant journal symlink");
        let error = load_journal(store.path(), &spec).expect_err("symlink must fail closed");
        assert!(
            matches!(error, CodeGenerationRetentionErrorV1::UnsafeState(ref message) if message.contains("not a bounded regular file")),
            "{error:?}"
        );
    }
}
