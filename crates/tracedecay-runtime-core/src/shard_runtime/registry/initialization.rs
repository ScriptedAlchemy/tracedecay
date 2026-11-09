//! Prospective stores install their schema outside the SQLite basename used by
//! clients. A process death before the no-replace rename leaves only disposable
//! schema in the private work directory. Hard links identify the empty sidecar
//! reservations if the process dies partway through claiming the destination.
use std::fs::{self, File};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::{ffi::CString, os::fd::AsRawFd};

use rusqlite::{Connection, OpenFlags};
use tracedecay_private_fs::framed_log::{
    DirectorySyncPolicy, rename_noreplace, sync_directory, sync_parent_directory,
};
use tracedecay_private_fs::{
    FileLease, create_private_directory, create_private_file, open_private_directory,
    open_private_file,
};

use crate::sqlite_read_snapshot::{fold_staging_to_standalone, with_suffix};

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
const OWNER: &str = "schema-installation.owner";
const SYNC: DirectorySyncPolicy = DirectorySyncPolicy::TolerateUnsupported;

/// Coordinates an authorized reset of an offline store with the existing
/// initializer. A staged or ambiguous family must finish reconciliation
/// before reset can discard any canonical members.
pub fn lock_offline_sqlite_family_for_reset(destination: &Path) -> io::Result<Option<FileLease>> {
    let staging = InitializationDirectory::path(destination);
    let lease = crate::storage::try_lock_existing_durable_directory(&staging)?;
    if present(&staging)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "SQLite initialization requires reconciliation before reset",
        ));
    }
    Ok(lease)
}

pub(super) struct InitializationDirectory {
    destination: PathBuf,
    directory: PathBuf,
    directory_pin: Option<File>,
    _lease: FileLease,
    owns_schema: bool,
}

impl InitializationDirectory {
    fn path(destination: &Path) -> PathBuf {
        with_suffix(destination, ".tracedecay-init")
    }

    fn acquire(destination: PathBuf, create: bool) -> io::Result<Self> {
        let directory = Self::path(&destination);
        let lease = crate::storage::try_lock_durable_directory(&directory)?;
        if create {
            match create_private_directory(&directory) {
                Ok(()) => sync_parent_directory(&directory, SYNC)?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let directory_pin = open_private_directory(&directory)?;
        let directory = directory.canonicalize()?;
        let marker = directory.join(OWNER);
        match open_private_file(&marker) {
            Ok(file) if file.metadata()?.len() == 0 => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SQLite initialization owner",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if fs::read_dir(&directory)?.next().transpose()?.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "unclaimed SQLite initialization directory is occupied",
                    ));
                }
                if create {
                    create_private_file(&marker)?.sync_all()?;
                    sync_directory(&directory, SYNC)?;
                }
            }
            Err(error) => return Err(error),
        }
        Ok(Self {
            destination,
            directory,
            directory_pin: Some(directory_pin),
            owns_schema: false,
            _lease: lease,
        })
    }

    pub(super) fn begin(destination: PathBuf) -> io::Result<Self> {
        let mut stage = Self::acquire(destination, true)?;
        if present(&stage.destination)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "SQLite destination exists",
            ));
        }
        // Check every occupied canonical member before discarding even our
        // private schema. An interrupted pre-protocol family has no witness.
        stage.validate_sidecars(false)?;
        if present(&stage.transferred())?
            || (present(&stage.prepared())?
                && (!present(&stage.database())?
                    || !same_regular_file(&stage.database(), &stage.prepared())?))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "published SQLite file is missing or ambiguous",
            ));
        }
        stage.discard_schema()?;
        stage.owns_schema = true;
        Ok(stage)
    }

    pub(super) fn database(&self) -> PathBuf {
        self.directory.join("schema.sqlite")
    }

    fn prepared(&self) -> PathBuf {
        self.directory.join("prepared.sqlite")
    }

    fn transferred(&self) -> PathBuf {
        self.directory.join("transferred.sqlite")
    }

    /// Teardown is relative to the directory capability acquired with the
    /// initialization lease. A noncooperating process can rename the directory
    /// and replace its pathname with a symlink; unlinking through that pathname
    /// would otherwise delete unrelated database files outside our staging area.
    fn remove_private_member(&self, member: &str) -> io::Result<()> {
        if member.is_empty()
            || member == "."
            || member == ".."
            || member.contains('/')
            || member.contains('\\')
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid SQLite staging member",
            ));
        }
        #[cfg(unix)]
        {
            let name = CString::new(member)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid staging name"))?;
            let directory = self
                .directory_pin
                .as_ref()
                .ok_or_else(|| io::Error::other("SQLite staging directory is no longer pinned"))?;
            // SAFETY: the directory descriptor remains open for this call,
            // and name is a NUL-terminated, single-component filename.
            let rc = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
            if rc == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            Err(error)
        }
        #[cfg(not(unix))]
        {
            remove_if_present(&self.directory.join(member))
        }
    }

    fn discard_schema(&self) -> io::Result<()> {
        self.remove_private_member("prepared.sqlite")?;
        for suffix in ["-wal", "-shm", "-journal", ""] {
            self.remove_private_member(&format!("schema.sqlite{suffix}"))?;
        }
        Ok(())
    }

    fn witness(&self, suffix: &str) -> PathBuf {
        self.directory.join(format!("reservation{suffix}"))
    }

    fn validate_sidecars(&self, required: bool) -> io::Result<()> {
        for suffix in SIDECARS {
            let name = with_suffix(&self.destination, suffix);
            if !required && !present(&name)? {
                continue;
            }
            if !empty_regular(&name)?
                || !present(&self.witness(suffix))?
                || !same_regular_file(&name, &self.witness(suffix))?
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "occupied SQLite sidecar has no matching initialization reservation",
                ));
            }
        }
        Ok(())
    }

    fn reserve_sidecars(&self) -> io::Result<()> {
        self.validate_sidecars(false)?;
        for suffix in SIDECARS {
            let witness = self.witness(suffix);
            if !present(&witness)? {
                create_private_file(&witness)?.sync_all()?;
            }
            if !empty_regular(&witness)? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid SQLite reservation",
                ));
            }
        }
        sync_directory(&self.directory, SYNC)?;
        for suffix in SIDECARS {
            let name = with_suffix(&self.destination, suffix);
            if !present(&name)? {
                fs::hard_link(self.witness(suffix), name)?;
            }
        }
        sync_parent_directory(&self.destination, SYNC)?;
        self.validate_sidecars(true)
    }

    pub(super) fn publish(mut self) -> io::Result<()> {
        let path = self.database();
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(io::Error::other)?;
        connection
            .pragma_update(
                None,
                "application_id",
                tracedecay_rusqlite_runtime::schema_version::REGISTERED_STORE_APPLICATION_ID,
            )
            .map_err(io::Error::other)?;
        fold_staging_to_standalone(connection, &path, || Ok(()))?;
        File::options()
            .read(true)
            .write(true)
            .open(&path)?
            .sync_all()?;
        self.claim_destination()?;
        rename_noreplace(&path, &self.destination)?;
        sync_parent_directory(&self.destination, SYNC)?;
        self.finish_publication()
    }

    fn claim_destination(&self) -> io::Result<()> {
        // This alias witnesses the exact main inode across the no-replace
        // rename. Remove it last, before the first runtime is returned.
        fs::hard_link(self.database(), self.prepared())?;
        self.reserve_sidecars()
    }

    fn finish_publication(&mut self) -> io::Result<()> {
        if present(&self.database())? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "SQLite destination coexists with an unpublished initialization",
            ));
        }
        if present(&self.prepared())? {
            if !same_regular_file(&self.destination, &self.prepared())? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SQLite destination identity changed before handoff",
                ));
            }
            self.validate_sidecars(true)?;
            // All witnesses remain until this single namespace transition.
            // Ordinary SQLite clients own the WAL, SHM and journal afterward.
            rename_noreplace(&self.prepared(), &self.transferred())?;
            sync_directory(&self.directory, SYNC)?;
        }
        if !present(&self.transferred())?
            || !same_regular_file(&self.destination, &self.transferred())?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SQLite destination has no matching handoff",
            ));
        }
        for suffix in SIDECARS {
            self.remove_private_member(&format!("reservation{suffix}"))?;
        }
        self.remove_private_member("transferred.sqlite")?;
        sync_directory(&self.directory, SYNC)?;
        self.retire()
    }

    fn verify_directory_name(&self) -> io::Result<()> {
        let pinned = self
            .directory_pin
            .as_ref()
            .ok_or_else(|| io::Error::other("SQLite staging directory is no longer pinned"))?;
        let named = open_private_directory(&self.directory)?;
        #[cfg(unix)]
        let matches = same_unix_inode(&pinned.metadata()?, &named.metadata()?);
        #[cfg(windows)]
        let matches = same_windows_object(pinned, &named)?;
        #[cfg(not(any(unix, windows)))]
        let matches = false;
        if !matches {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SQLite staging directory no longer names the pinned directory",
            ));
        }
        Ok(())
    }

    fn retire(&mut self) -> io::Result<()> {
        self.verify_directory_name()?;
        for entry in fs::read_dir(&self.directory)? {
            if entry?.file_name() != OWNER {
                return Err(io::Error::new(
                    io::ErrorKind::DirectoryNotEmpty,
                    "SQLite initialization directory still contains files",
                ));
            }
        }
        self.remove_private_member(OWNER)?;
        // rmdir is name-based. Refuse an already changed name as late as
        // possible, while holding the lease and the original directory pin.
        self.verify_directory_name()?;
        drop(self.directory_pin.take());
        fs::remove_dir(&self.directory)?;
        sync_parent_directory(&self.directory, SYNC)
    }

    /// A completed handoff can still have its empty directory and owner marker
    /// after a process exit. Read-only callers need neither cleanup nor a lease.
    /// Any other staged member requires writable reconciliation before attach.
    pub(super) fn verify_read_only_handoff(destination: &Path) -> io::Result<()> {
        let directory = Self::path(destination);
        let _pin = match open_private_directory(&directory) {
            Ok(pin) => pin,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in fs::read_dir(&directory)? {
            let member = entry?;
            if member.file_name() != OWNER || !empty_regular(&member.path())? {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "SQLite staging requires writable reconciliation before read-only attachment",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn recover_published(destination: &Path) -> io::Result<()> {
        let directory = Self::path(destination);
        if !present(&directory)? {
            return Ok(());
        }
        let mut stage = match Self::acquire(destination.to_owned(), false) {
            Ok(stage) => stage,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !present(&directory)? => {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if Self::pending(&stage.directory)? {
            stage.finish_publication()
        } else {
            stage.retire()
        }
    }

    fn pending(directory: &Path) -> io::Result<bool> {
        for name in [
            "schema.sqlite",
            "prepared.sqlite",
            "transferred.sqlite",
            "reservation-wal",
            "reservation-shm",
            "reservation-journal",
        ] {
            if present(&directory.join(name))? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Drop for InitializationDirectory {
    fn drop(&mut self) {
        if self.directory_pin.is_none() {
            return;
        }
        if !self.owns_schema {
            // Early refusals can leave our empty work directory. If a private
            // schema or a reservation still exists, retain it for recovery.
            if matches!(present(&self.directory.join(OWNER)), Ok(true))
                && matches!(Self::pending(&self.directory), Ok(false))
                && let Err(error) = self.retire()
            {
                tracing::warn!(%error, "could not retire SQLite initialization directory");
            }
            return;
        }
        // A destination that appeared during this attempt is ambiguous until
        // a new opener verifies the prepared-main witness.
        match present(&self.destination) {
            Ok(false) => {
                if let Err(error) = self.discard_schema().and_then(|()| {
                    if !Self::pending(&self.directory)? {
                        self.retire()?;
                    }
                    Ok(())
                }) {
                    tracing::warn!(%error, "could not retire unpublished SQLite schema");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not inspect initialized SQLite destination");
            }
            Ok(true) => {}
        }
    }
}

fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(any(test, not(unix)))]
fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn empty_regular(path: &Path) -> io::Result<bool> {
    let meta = fs::symlink_metadata(path)?;
    Ok(meta.is_file() && meta.len() == 0)
}

#[cfg(unix)]
fn same_unix_inode(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_windows_object(left: &File, right: &File) -> io::Result<bool> {
    let a = tracedecay_private_fs::windows_file::information(left)?;
    let b = tracedecay_private_fs::windows_file::information(right)?;
    Ok(a.volume_serial_number == b.volume_serial_number && a.file_index == b.file_index)
}

fn same_regular_file(left: &Path, right: &Path) -> io::Result<bool> {
    if !fs::symlink_metadata(left)?.is_file() || !fs::symlink_metadata(right)?.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        Ok(same_unix_inode(
            &fs::symlink_metadata(left)?,
            &fs::symlink_metadata(right)?,
        ))
    }
    #[cfg(windows)]
    {
        same_windows_object(&File::open(left)?, &File::open(right)?)
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "SQLite file identity unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use tempfile::TempDir;

    fn seed(path: &Path, value: i64) {
        let db = Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE data(value INTEGER)")
            .unwrap();
        db.execute("INSERT INTO data VALUES (?)", [value]).unwrap();
    }

    #[test]
    fn process_exit_during_initialization_recovers_without_publishing_partial_schema() {
        const CHILD: &str = "TRACEDECAY_TEST_EXIT_DURING_INITIALIZATION";
        if let Some(destination) = std::env::var_os(CHILD) {
            let stage = InitializationDirectory::begin(destination.into()).unwrap();
            seed(&stage.database(), 1);
            let state = std::env::var("TRACEDECAY_TEST_INITIALIZATION_STATE").unwrap();
            match state.as_str() {
                "installing" => {}
                "reserved" => stage.claim_destination().unwrap(),
                "published" | "transferred" | "unproven" => {
                    stage.claim_destination().unwrap();
                    rename_noreplace(&stage.database(), &stage.destination).unwrap();
                    if state == "transferred" {
                        rename_noreplace(&stage.prepared(), &stage.transferred()).unwrap();
                    }
                    if state != "published" {
                        remove_if_present(&stage.witness("-wal")).unwrap();
                    }
                }
                state => panic!("unknown initialization state {state}"),
            }
            // Simulate ordinary process death with no Rust or SQLite destructors.
            std::process::exit(73);
        }

        let name = format!(
            "{}::process_exit_during_initialization_recovers_without_publishing_partial_schema",
            module_path!().split_once("::").unwrap().1
        );
        for state in [
            "installing",
            "reserved",
            "published",
            "transferred",
            "unproven",
        ] {
            let root = TempDir::new().unwrap();
            let destination = root.path().join("sessions.db");
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name])
                .env(CHILD, &destination)
                .env("TRACEDECAY_TEST_INITIALIZATION_STATE", state)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(73), "{output:?}");
            if state == "unproven" {
                assert!(InitializationDirectory::recover_published(&destination).is_err());
                for suffix in SIDECARS {
                    assert!(empty_regular(&with_suffix(&destination, suffix)).unwrap());
                }
                continue;
            }
            let expected = if matches!(state, "published" | "transferred") {
                InitializationDirectory::recover_published(&destination).unwrap();
                1
            } else {
                assert!(!destination.exists());
                let retry = InitializationDirectory::begin(destination.clone()).unwrap();
                assert!(!retry.database().exists());
                seed(&retry.database(), 2);
                retry.publish().unwrap();
                2
            };
            let db = Connection::open(&destination).unwrap();
            assert_eq!(
                db.query_row("SELECT value FROM data", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                expected
            );
            assert_eq!(
                db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            assert!(!InitializationDirectory::path(&destination).exists());
        }
    }

    /// A competing process may rename the private directory despite the
    /// cooperative lease and install a symlink at the old path. Failed-start
    /// cleanup must only remove entries from our *pinned* directory and never
    /// the distinct valid SQLite database beneath that replacement symlink.
    #[cfg(unix)]
    #[test]
    fn failed_initialization_cleanup_uses_pinned_directory_after_swap() {
        const SWAP_ROOT: &str = "TRACEDECAY_SQLITE_STAGE_SWAP_ROOT";
        const COMPLETE: &str = "tracedecay-sqlite-stage-swap-complete";
        if let Some(root) = std::env::var_os(SWAP_ROOT) {
            let root = PathBuf::from(root);
            let stage = root.join("sessions.db.tracedecay-init");
            fs::rename(&stage, root.join("retired-stage")).unwrap();
            symlink(root.join("foreign"), &stage).unwrap();
            println!("{COMPLETE}");
            return;
        }

        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let stage = InitializationDirectory::begin(destination).unwrap();
        seed(&stage.database(), 7);
        let foreign = root.path().join("foreign");
        fs::create_dir(&foreign).unwrap();
        let foreign_database = foreign.join("schema.sqlite");
        seed(&foreign_database, 99);
        let foreign_main = fs::read(&foreign_database).unwrap();
        fs::write(foreign.join("prepared.sqlite"), b"foreign prepared").unwrap();
        for suffix in ["-wal", "-shm", "-journal"] {
            fs::write(
                with_suffix(&stage.database(), suffix),
                b"owned staging sidecar",
            )
            .unwrap();
            fs::write(
                with_suffix(&foreign_database, suffix),
                format!("foreign {suffix}"),
            )
            .unwrap();
        }

        let (_, module) = module_path!().split_once("::").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(format!(
                "{module}::failed_initialization_cleanup_uses_pinned_directory_after_swap"
            ))
            .arg("--nocapture")
            .env(SWAP_ROOT, root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains(COMPLETE),
            "cross-process stage swap did not execute: {output:?}"
        );

        // Dropping a failed schema initialization is the production cleanup
        // caller. Its directory descriptor still points to retired-stage.
        drop(stage);
        let retired = root.path().join("retired-stage");
        assert!(!retired.join("schema.sqlite").exists());
        for suffix in ["-wal", "-shm", "-journal"] {
            assert!(!with_suffix(&retired.join("schema.sqlite"), suffix).exists());
            assert_eq!(
                fs::read(with_suffix(&foreign_database, suffix)).unwrap(),
                format!("foreign {suffix}").as_bytes()
            );
        }
        assert_eq!(fs::read(&foreign_database).unwrap(), foreign_main);
        assert_eq!(
            fs::read(foreign.join("prepared.sqlite")).unwrap(),
            b"foreign prepared"
        );
    }

    #[test]
    fn retiring_a_directory_requires_its_own_pin() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let mut stage = InitializationDirectory::begin(destination).unwrap();
        let other = root.path().join("another-empty-directory");
        create_private_directory(&other).unwrap();
        let original = std::mem::replace(&mut stage.directory, other.clone());
        let error = stage.retire().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(other.is_dir());
        assert!(original.join(OWNER).is_file());
        stage.directory = original.clone();
        drop(stage);
        assert!(!original.exists());
        assert!(other.is_dir());
    }

    #[test]
    fn offline_reset_preserves_the_initialization_lease_and_refuses_unresolved_staging() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        assert!(
            lock_offline_sqlite_family_for_reset(&destination)
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let publisher = InitializationDirectory::begin(destination.clone()).unwrap();
        assert!(lock_offline_sqlite_family_for_reset(&destination).is_err());
        drop(publisher);
        let reset = lock_offline_sqlite_family_for_reset(&destination)
            .unwrap()
            .unwrap();
        assert!(InitializationDirectory::begin(destination.clone()).is_err());
        drop(reset);
        let staging = InitializationDirectory::path(&destination);
        create_private_directory(&staging).unwrap();
        let unrelated = staging.join("operator-data");
        fs::write(&unrelated, b"keep").unwrap();
        assert!(InitializationDirectory::verify_read_only_handoff(&destination).is_err());
        assert!(lock_offline_sqlite_family_for_reset(&destination).is_err());
        assert_eq!(fs::read(unrelated).unwrap(), b"keep");
    }

    #[test]
    fn competing_initialization_leaves_owner_and_foreign_sidecars_intact() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let stage = InitializationDirectory::begin(destination.clone()).unwrap();
        seed(&stage.database(), 4);
        assert!(InitializationDirectory::begin(destination.clone()).is_err());
        assert!(stage.database().exists());
        drop(stage);
        for suffix in SIDECARS {
            let foreign = with_suffix(&destination, suffix);
            fs::write(&foreign, b"pre-existing sidecar").unwrap();
            assert!(InitializationDirectory::begin(destination.clone()).is_err());
            assert_eq!(fs::read(&foreign).unwrap(), b"pre-existing sidecar");
            assert!(!destination.exists());
            assert!(!InitializationDirectory::path(&destination).exists());
            fs::remove_file(foreign).unwrap();
        }
    }
}
