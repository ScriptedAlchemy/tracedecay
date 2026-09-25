//! Capability-relative quarantine for retention files: sealed generations and
//! text artifacts.
//!
//! The source and stage directories are acquired once and every rename and
//! unlink is relative to those handles, so a parent path rebound after the
//! collection decision cannot redirect a destructive step. A file is admitted
//! through the handle it is read from and must arrive in the stage with that
//! same identity; a same-name replacement that lands between admission and
//! rename is refused there and left for the journal rollback to restore, never
//! unlinked.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions};
use tracedecay_private_fs::capability_dir::{rename_noreplace, sync_directory};
#[cfg(windows)]
use tracedecay_private_fs::windows_file;

use super::{CodeGenerationRetentionErrorV1, storage};

/// Held stage capability for one retention transaction's quarantine.
pub(super) struct FileQuarantine {
    label: &'static str,
    quarantine: Option<Dir>,
    stage: Option<Dir>,
    stage_name: String,
}

impl FileQuarantine {
    /// Opens the stage for a new transaction, creating it if needed.
    pub(super) fn prepare(
        label: &'static str,
        quarantine_root: &Path,
        stage_name: &str,
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        std::fs::create_dir_all(quarantine_root).map_err(storage)?;
        let quarantine =
            Dir::open_ambient_dir(quarantine_root, ambient_authority()).map_err(storage)?;
        match quarantine.create_dir(stage_name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(storage(error)),
        }
        let stage = quarantine.open_dir_nofollow(stage_name).map_err(storage)?;
        sync_directory(&quarantine).map_err(storage)?;
        Ok(Self {
            label,
            quarantine: Some(quarantine),
            stage: Some(stage),
            stage_name: stage_name.to_owned(),
        })
    }

    /// Opens whatever stage an interrupted transaction left behind.
    pub(super) fn recover(
        label: &'static str,
        quarantine_root: &Path,
        stage_name: &str,
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        let quarantine = open_optional_dir(quarantine_root)?;
        let stage = match quarantine.as_ref() {
            Some(quarantine) => match quarantine.open_dir_nofollow(stage_name) {
                Ok(stage) => Some(stage),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(storage(error)),
            },
            None => None,
        };
        Ok(Self {
            label,
            quarantine,
            stage,
            stage_name: stage_name.to_owned(),
        })
    }

    /// Moves `name` from `source` into the stage. `admit` sees the exact file
    /// that will be moved and refuses it before any mutation.
    pub(super) fn stage(
        &self,
        source: Option<&Dir>,
        name: &str,
        admit: impl FnOnce(&File) -> Result<(), CodeGenerationRetentionErrorV1>,
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        let stage = self
            .stage
            .as_ref()
            .ok_or_else(|| unsafe_state(format!("{} quarantine stage is not open", self.label)))?;
        let label = self.label;
        let source = match (source, regular_entry_exists(stage, name)?) {
            (Some(source), false) if regular_entry_exists(source, name)? => source,
            (Some(source), true) if regular_entry_exists(source, name)? => {
                return Err(unsafe_state(format!(
                    "{label} '{name}' exists in both source and quarantine"
                )));
            }
            (_, true) => {
                return Err(unsafe_state(format!(
                    "{label} '{name}' was already quarantined"
                )));
            }
            (_, false) => {
                return Err(unsafe_state(format!(
                    "{label} '{name}' is missing before quarantine"
                )));
            }
        };
        let admitted = open_regular_nofollow(source, name)?.ok_or_else(|| {
            unsafe_state(format!("{label} '{name}' is missing before quarantine"))
        })?;
        admit(&admitted)?;
        let identity = file_identity(&admitted).map_err(storage)?;
        // Release this operation's own handle before the rename: the move is
        // proven afterwards against the identity just captured, and no open
        // handle of ours can then be the one that refuses it.
        drop(admitted);
        rename_noreplace(source, OsStr::new(name), stage, OsStr::new(name))
            .map_err(|error| mutation_failed("quarantine rename", &target(label, name), &error))?;
        let moved = open_regular_nofollow(stage, name)?.ok_or_else(|| {
            unsafe_state(format!("{label} '{name}' did not arrive in quarantine"))
        })?;
        if file_identity(&moved).map_err(storage)? != identity {
            return Err(unsafe_state(format!(
                "{label} '{name}' was replaced between admission and quarantine"
            )));
        }
        sync_directory(source).map_err(storage)?;
        sync_directory(stage).map_err(storage)
    }

    /// Returns a staged `name` to `source`, the rollback of [`Self::stage`].
    pub(super) fn restore(
        &self,
        source: Option<&Dir>,
        name: &str,
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        let label = self.label;
        let in_source = match source {
            Some(source) => regular_entry_exists(source, name)?,
            None => false,
        };
        let staged = match self.stage.as_ref() {
            Some(stage) if regular_entry_exists(stage, name)? => Some(stage),
            _ => None,
        };
        match (in_source, staged, source) {
            (true, None, _) => Ok(()),
            (false, Some(stage), Some(source)) => {
                rename_noreplace(stage, OsStr::new(name), source, OsStr::new(name)).map_err(
                    |error| mutation_failed("rollback rename", &target(label, name), &error),
                )?;
                sync_directory(source).map_err(storage)?;
                sync_directory(stage).map_err(storage)
            }
            (false, Some(_), None) => Err(unsafe_state(format!(
                "{label} '{name}' cannot be restored: its source directory is gone"
            ))),
            (false, None, _) => Err(unsafe_state(format!(
                "{label} rollback cannot find '{name}'"
            ))),
            (true, Some(_), _) => Err(unsafe_state(format!(
                "{label} rollback found duplicate '{name}'"
            ))),
        }
    }

    /// Unlinks a staged `name` once its receipt is durable.
    pub(super) fn remove_committed(
        &self,
        source: Option<&Dir>,
        name: &str,
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        let label = self.label;
        if let Some(source) = source
            && regular_entry_exists(source, name)?
        {
            return Err(unsafe_state(format!(
                "{label} receipt is durable but '{name}' returned to its source directory"
            )));
        }
        let Some(stage) = self.stage.as_ref() else {
            return Ok(());
        };
        if !regular_entry_exists(stage, name)? {
            return Ok(());
        }
        stage
            .remove_file(name)
            .map_err(|error| mutation_failed("quarantine unlink", &target(label, name), &error))?;
        sync_directory(stage).map_err(storage)
    }

    /// Removes the stage once nothing is left in it.
    pub(super) fn remove_empty_stage(mut self) -> Result<(), CodeGenerationRetentionErrorV1> {
        let (Some(stage), Some(quarantine)) = (self.stage.take(), self.quarantine.as_ref()) else {
            return Ok(());
        };
        if stage.read_dir(".").map_err(storage)?.next().is_some() {
            return Err(unsafe_state(format!(
                "{} quarantine '{}' contains unexpected files",
                self.label, self.stage_name
            )));
        }
        // Our own handle on the stage must not be what refuses its removal.
        drop(stage);
        quarantine.remove_dir(&self.stage_name).map_err(|error| {
            mutation_failed(
                "quarantine stage unlink",
                &format!("{} stage '{}'", self.label, self.stage_name),
                &error,
            )
        })?;
        sync_directory(quarantine).map_err(storage)
    }
}

/// Opens a retention source directory; `None` when it does not exist.
pub(super) fn open_optional_dir(
    path: &Path,
) -> Result<Option<Dir>, CodeGenerationRetentionErrorV1> {
    match Dir::open_ambient_dir(path, ambient_authority()) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

/// Names the refused step, its exact target, and the native error. A refusal
/// caused by another owner's live handle (Windows sharing or lock violation,
/// Unix `EBUSY`) is a typed deferral; the durable journal retries it once that
/// owner lets go.
pub(super) fn mutation_failed(
    operation: &str,
    target: &str,
    error: &io::Error,
) -> CodeGenerationRetentionErrorV1 {
    let native = error
        .raw_os_error()
        .map_or_else(|| "none".to_owned(), |code| code.to_string());
    let detail = format!("{operation} for {target} failed (native error {native}): {error}");
    if held_by_another_owner(error) {
        CodeGenerationRetentionErrorV1::TargetHeld(detail)
    } else {
        storage(detail)
    }
}

fn held_by_another_owner(error: &io::Error) -> bool {
    // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION.
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return true;
    }
    error.kind() == io::ErrorKind::ResourceBusy
}

fn target(label: &str, name: &str) -> String {
    format!("{label} '{name}'")
}

fn regular_entry_exists(parent: &Dir, name: &str) -> Result<bool, CodeGenerationRetentionErrorV1> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(unsafe_state(format!(
            "retention entry '{name}' is not a regular file"
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(storage(error)),
    }
}

fn open_regular_nofollow(
    parent: &Dir,
    name: &str,
) -> Result<Option<File>, CodeGenerationRetentionErrorV1> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = match parent.open_with(name, &options) {
        Ok(file) => file.into_std(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    if !file.metadata().map_err(storage)?.is_file() {
        return Err(unsafe_state(format!(
            "retention entry '{name}' is not a regular file"
        )));
    }
    Ok(Some(file))
}

/// The durable file-id pair: Unix device and inode, or the Windows volume
/// serial number and by-handle file index.
#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    let information = windows_file::information(file)?;
    Ok((
        u64::from(information.volume_serial_number),
        information.file_index,
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> io::Result<(u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "retention file identity is unavailable on this platform",
    ))
}

fn unsafe_state(message: String) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::UnsafeState(message)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    const STAGE: &str = "stage";

    struct Fixture {
        _temp: TempDir,
        source_path: PathBuf,
        quarantine_path: PathBuf,
        source: Dir,
        quarantine: FileQuarantine,
    }

    fn fixture(files: &[(&str, &[u8])]) -> Fixture {
        let temp = TempDir::new().expect("temporary store");
        let source_path = temp.path().join("source");
        std::fs::create_dir(&source_path).expect("create source");
        for (name, bytes) in files {
            std::fs::write(source_path.join(name), bytes).expect("write source file");
        }
        let quarantine_path = temp.path().join("quarantine");
        let source = open_optional_dir(&source_path)
            .expect("open source")
            .expect("source exists");
        let quarantine =
            FileQuarantine::prepare("fixture file", &quarantine_path, STAGE).expect("prepare");
        Fixture {
            _temp: temp,
            source_path,
            quarantine_path,
            source,
            quarantine,
        }
    }

    #[test]
    fn a_replacement_landing_after_admission_is_refused_and_restored_not_deleted() {
        let fixture = fixture(&[("payload", b"admitted")]);
        let error = fixture
            .quarantine
            .stage(Some(&fixture.source), "payload", |_| {
                // Same name, same size: only the file identity tells them apart.
                std::fs::rename(
                    fixture.source_path.join("payload"),
                    fixture.source_path.join("admitted-original"),
                )
                .expect("move the admitted file aside");
                std::fs::write(fixture.source_path.join("payload"), b"replaced")
                    .expect("plant a same-size replacement");
                Ok(())
            })
            .expect_err("the replacement must not be accepted as the admitted file");
        assert!(
            matches!(&error, CodeGenerationRetentionErrorV1::UnsafeState(message)
                if message.contains("replaced between admission and quarantine")),
            "{error:?}"
        );

        fixture
            .quarantine
            .restore(Some(&fixture.source), "payload")
            .expect("rollback returns whatever was staged");
        assert_eq!(
            std::fs::read(fixture.source_path.join("payload")).expect("restored replacement"),
            b"replaced"
        );
        assert_eq!(
            std::fs::read(fixture.source_path.join("admitted-original")).expect("original"),
            b"admitted"
        );
        fixture
            .quarantine
            .remove_empty_stage()
            .expect("the drained stage is removed");
        assert!(!fixture.quarantine_path.join(STAGE).exists());
    }

    #[test]
    fn a_rebound_source_path_cannot_redirect_the_rename() {
        let fixture = fixture(&[("payload", b"owned")]);
        let moved = fixture.source_path.with_file_name("source-moved");
        let rebind = std::fs::rename(&fixture.source_path, &moved);
        // A held Windows directory capability is opened without
        // `FILE_SHARE_DELETE`, so the platform refuses the rebind itself.
        #[cfg(windows)]
        assert_eq!(
            rebind.expect_err("a held source cannot be moved").raw_os_error(),
            Some(32)
        );
        #[cfg(not(windows))]
        {
            rebind.expect("move the held source");
            std::fs::create_dir(&fixture.source_path).expect("rebind the source path");
            std::fs::write(fixture.source_path.join("payload"), b"foreign")
                .expect("foreign file");
        }

        fixture
            .quarantine
            .stage(Some(&fixture.source), "payload", |_| Ok(()))
            .expect("stage through the held capability");

        #[cfg(not(windows))]
        assert_eq!(
            std::fs::read(fixture.source_path.join("payload")).expect("foreign file survives"),
            b"foreign"
        );
        #[cfg(windows)]
        assert!(!fixture.source_path.join("payload").exists());
        assert!(!moved.join("payload").exists());
        assert_eq!(
            std::fs::read(fixture.quarantine_path.join(STAGE).join("payload"))
                .expect("the held file was staged"),
            b"owned"
        );
    }

    #[test]
    fn committed_removal_unlinks_only_the_staged_file_and_then_the_stage() {
        let Fixture {
            _temp,
            source_path,
            quarantine_path,
            source,
            quarantine,
        } = fixture(&[("payload", b"retire"), ("sibling", b"live")]);
        quarantine
            .stage(Some(&source), "payload", |_| Ok(()))
            .expect("stage");
        // A restart releases the interrupted transaction's stage capability.
        drop(quarantine);
        let recovered = FileQuarantine::recover("fixture file", &quarantine_path, STAGE)
            .expect("reopen after a restart");
        recovered
            .remove_committed(Some(&source), "payload")
            .expect("unlink the committed file");
        recovered
            .remove_committed(Some(&source), "payload")
            .expect("a replayed unlink is idempotent");
        recovered.remove_empty_stage().expect("remove the stage");

        assert!(!quarantine_path.join(STAGE).exists());
        assert!(!source_path.join("payload").exists());
        assert_eq!(
            std::fs::read(source_path.join("sibling")).expect("live sibling"),
            b"live"
        );
    }

    #[test]
    fn a_committed_file_back_in_its_source_is_refused() {
        let fixture = fixture(&[("payload", b"retire")]);
        fixture
            .quarantine
            .stage(Some(&fixture.source), "payload", |_| Ok(()))
            .expect("stage");
        std::fs::write(fixture.source_path.join("payload"), b"republished").expect("republish");

        let error = fixture
            .quarantine
            .remove_committed(Some(&fixture.source), "payload")
            .expect_err("a republished name is not the committed file");
        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
        assert!(
            fixture
                .quarantine_path
                .join(STAGE)
                .join("payload")
                .is_file()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_rename_leaves_the_source_in_place_and_converges_on_retry() {
        let fixture = fixture(&[("payload", b"owned")]);
        let stage = fixture.quarantine_path.join(STAGE);
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o555))
            .expect("refuse writes to the stage");
        let refused = fixture
            .quarantine
            .stage(Some(&fixture.source), "payload", |_| Ok(()));
        std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o755))
            .expect("release the stage");
        let error = refused.expect_err("a refused rename must surface");
        assert!(
            error.to_string().contains("quarantine rename"),
            "the refusal names its operation: {error}"
        );
        assert!(fixture.source_path.join("payload").is_file());

        fixture
            .quarantine
            .stage(Some(&fixture.source), "payload", |_| Ok(()))
            .expect("the retry converges once the refusal is gone");
        assert!(stage.join("payload").is_file());
    }

    #[test]
    fn a_held_target_is_a_typed_deferral_naming_operation_target_and_code() {
        #[cfg(unix)]
        let held_code = libc::EBUSY;
        #[cfg(windows)]
        let held_code = 32;
        let held = mutation_failed(
            "quarantine rename",
            "fixture file 'payload'",
            &io::Error::from_raw_os_error(held_code),
        );
        let CodeGenerationRetentionErrorV1::TargetHeld(message) = &held else {
            panic!("a held target is a typed deferral, got {held:?}");
        };
        assert!(message.contains("quarantine rename"), "{message}");
        assert!(message.contains("fixture file 'payload'"), "{message}");
        assert!(
            message.contains(&format!("native error {held_code}")),
            "{message}"
        );

        let other = mutation_failed(
            "quarantine unlink",
            "fixture file 'payload'",
            &io::Error::other("no native code"),
        );
        let CodeGenerationRetentionErrorV1::Storage(message) = &other else {
            panic!("an unrelated refusal stays a storage failure, got {other:?}");
        };
        assert!(message.contains("native error none"), "{message}");
    }
}
