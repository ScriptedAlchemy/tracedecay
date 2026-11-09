//! A prospective SQLite store publishes its completed main last. It moves staged
//! files by no-replace rename and retires only empty directories.
//! A failed schema install therefore cannot delete a file at a reused basename.
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
mod interrupted;

use interrupted::InterruptedInstall;
use tracedecay_private_fs::capability_dir::{self, Dir};
use tracedecay_private_fs::framed_log::{DirectorySyncPolicy, sync_parent_directory};
use tracedecay_private_fs::{FileLease, create_private_directory, open_private_directory};

use crate::sqlite_read_snapshot::with_suffix;

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
const STAGED_MEMBERS: [&str; 4] = [
    "schema.sqlite",
    "schema.sqlite-wal",
    "schema.sqlite-shm",
    "schema.sqlite-journal",
];
pub(super) type InitializationFiles = (File, [File; 3]);

const OWNER: &str = "schema-installation.owner";
const SYNC: DirectorySyncPolicy = DirectorySyncPolicy::TolerateUnsupported;

/// Coordinates an authorized reset of an offline store with the existing
/// initializer. A staged or ambiguous family must finish reconciliation
/// before reset can discard any canonical members.
pub fn lock_offline_sqlite_family_for_reset(destination: &Path) -> io::Result<Option<FileLease>> {
    let staging = InitializationDirectory::path(destination);
    let lease = crate::storage::try_lock_existing_durable_directory(&staging)?;
    if present(&staging)?
        || lease
            .as_ref()
            .map(|file| file.metadata())
            .transpose()?
            .is_some_and(|meta| meta.len() != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "SQLite initialization requires reconciliation before reset",
        ));
    }
    Ok(lease)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PreparedFamily {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry: Option<InterruptedInstall>,
    runtime_identity: u64,
    main: LeafIdentity,
    // Allocation identity alone cannot detect in-place content replacement.
    main_sha256: [u8; 32],
    sidecars: [LeafIdentity; 3],
}

fn surviving_sidecars(sidecars: [File; 3]) -> io::Result<[Option<File>; 3]> {
    let retained = sidecars.map(Some);
    #[cfg(windows)]
    let mut retained = retained;
    #[cfg(windows)]
    for slot in &mut retained {
        if let Some(file) = slot.as_ref()
            && tracedecay_private_fs::windows_file::deletion_pending(file)?
        {
            drop(slot.take());
        }
    }
    Ok(retained)
}

impl PreparedFamily {
    fn has_durable_generations(&self) -> bool {
        self.main.allocation.is_some()
            && self
                .sidecars
                .iter()
                .all(|sidecar| sidecar.allocation.is_some())
    }

    fn pin_main(&self, directory: &Dir, name: &OsStr) -> io::Result<File> {
        let file = self.main.pin_at(directory, name, false)?;
        if sha256_file(&file)? != self.main_sha256 {
            return Err(ambiguous("prepared SQLite main file content changed"));
        }
        Ok(file)
    }

    fn load(file: &mut File) -> io::Result<Option<Self>> {
        let length = file.metadata()?.len();
        Ok(if length == 0 {
            None
        } else {
            if length > 2048 {
                return Err(ambiguous("invalid SQLite preparation receipt"));
            }
            let mut bytes = Vec::new();
            file.take(length).read_to_end(&mut bytes)?;
            Some(
                serde_json::from_slice(&bytes)
                    .map_err(|_| ambiguous("invalid SQLite preparation receipt"))?,
            )
        })
    }
}

#[derive(Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LeafIdentity {
    device: u64,
    file_id: u64,
    created: Option<SystemTime>,
    allocation: Option<u64>,
}

impl LeafIdentity {
    fn from_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        #[cfg(unix)]
        let (device, file_id) = (metadata.dev(), metadata.ino());
        #[cfg(windows)]
        let (device, file_id) = {
            let info = tracedecay_private_fs::windows_file::information(file)?;
            (u64::from(info.volume_serial_number), info.file_index)
        };
        #[cfg(target_os = "macos")]
        let allocation = {
            use std::os::macos::fs::MetadataExt;
            let value = metadata.st_gen();
            (value != 0).then_some(u64::from(value))
        };
        #[cfg(not(target_os = "macos"))]
        let allocation = tracedecay_private_fs::inode_generation(file)?;
        #[cfg(any(unix, windows))]
        {
            Ok(Self {
                device,
                file_id,
                created: metadata.created().ok(),
                allocation,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SQLite generation identity is unavailable",
            ))
        }
    }

    fn pin_at(&self, directory: &Dir, name: &OsStr, empty: bool) -> io::Result<File> {
        let file = capability_dir::open_regular_nofollow(directory, name)?;
        if *self != Self::from_file(&file)? || (empty && file.metadata()?.len() != 0) {
            return Err(ambiguous(
                "SQLite initialization member no longer matches its prepared generation",
            ));
        }
        Ok(file)
    }
}

fn sha256_file(mut file: &File) -> io::Result<[u8; 32]> {
    file.rewind()?;
    let length = file.metadata()?.len();
    let mut input = file.take(length);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    if input.limit() != 0 || file.metadata()?.len() != length {
        return Err(ambiguous(
            "SQLite recovery member changed length during verification",
        ));
    }
    Ok(digest.finalize().into())
}

fn ambiguous(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) struct InitializationDirectory {
    destination: PathBuf,
    directory: PathBuf,
    directory_pin: Option<File>,
    parent: Dir,
    lease: FileLease,
    prepared: Option<PreparedFamily>,
}

impl InitializationDirectory {
    fn path(destination: &Path) -> PathBuf {
        with_suffix(destination, ".tracedecay-init")
    }

    fn acquire(destination: PathBuf, create: bool) -> io::Result<Self> {
        let directory = Self::path(&destination);
        let parent = capability_dir::pin_parent(
            destination
                .parent()
                .ok_or_else(|| ambiguous("SQLite destination has no parent"))?,
        )?;
        let mut lease = crate::storage::try_lock_durable_directory(&directory)?;
        let prepared = PreparedFamily::load(&mut lease)?;
        if create {
            if prepared.is_some() && !present(&directory)? {
                return Err(ambiguous(
                    "prepared SQLite initialization directory is missing",
                ));
            }
            match create_private_directory(&directory) {
                Ok(()) => sync_parent_directory(&directory, SYNC)?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        let directory_pin = open_private_directory(&directory)?;
        let directory = directory.canonicalize()?;
        Ok(Self {
            destination,
            directory,
            directory_pin: Some(directory_pin),
            parent,
            lease,
            prepared,
        })
    }

    pub(super) fn begin(destination: PathBuf) -> io::Result<Self> {
        let stage = Self::acquire(destination, true)?;
        if present(&stage.destination)? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "SQLite destination exists",
            ));
        }
        if stage.prepared.is_none() {
            stage.require_empty()?;
            for suffix in SIDECARS {
                if present(&with_suffix(&stage.destination, suffix))? {
                    return Err(ambiguous(
                        "occupied SQLite sidecar has no prepared generation",
                    ));
                }
            }
        }
        Ok(stage)
    }

    pub(super) fn database(&self) -> PathBuf {
        self.directory.join("schema.sqlite")
    }

    #[cfg(test)]
    fn reservation(&self, suffix: &str) -> PathBuf {
        self.directory.join(format!("reservation{suffix}"))
    }

    fn stage_cap(&self) -> io::Result<Dir> {
        let pin = self
            .directory_pin
            .as_ref()
            .ok_or_else(|| ambiguous("SQLite staging directory is no longer pinned"))?;
        Ok(Dir::from_std_file(pin.try_clone()?))
    }

    fn require_empty(&self) -> io::Result<()> {
        self.verify_directory_name()?;
        if self.stage_cap()?.entries()?.next().transpose()?.is_some() {
            return Err(ambiguous(
                "unproven SQLite staging contents require offline reconciliation",
            ));
        }
        Ok(())
    }

    fn prepare(
        &mut self,
        expected: u64,
        (main_file, sidecars): (File, [File; 3]),
    ) -> io::Result<[File; 4]> {
        self.verify_directory_name()?;
        let stage = self.stage_cap()?;
        let sidecars = surviving_sidecars(sidecars)?;
        let main = LeafIdentity::from_file(&main_file)?;
        let main_pin = main.pin_at(&stage, OsStr::new("schema.sqlite"), false)?;
        require_schema_members(&stage)?;
        main_file.sync_all()?;
        let main_sha256 = sha256_file(&main_pin)?;
        // WAL and rollback-journal bytes may be durable data. The original
        // writer has checkpointed and joined; refuse any remaining frames.
        // Only the original, now-unused SHM index may be cleared by descriptor.
        for (index, suffix) in SIDECARS.iter().enumerate() {
            let source = format!("schema.sqlite{suffix}");
            if present_at(&stage, source.as_ref())? {
                let file = sidecars[index].as_ref().ok_or_else(|| {
                    ambiguous("SQLite sidecar has no surviving original reservation")
                })?;
                LeafIdentity::from_file(file)?.pin_at(
                    &stage,
                    source.as_ref(),
                    *suffix != "-shm",
                )?;
            }
        }
        let reserve = |index: usize, suffix: &str| {
            let source = format!("schema.sqlite{suffix}");
            let target = format!("reservation{suffix}");
            if present_at(&stage, source.as_ref())? {
                let file = sidecars[index].as_ref().ok_or_else(|| {
                    ambiguous("SQLite sidecar has no surviving original reservation")
                })?;
                let identity = LeafIdentity::from_file(file)?;
                identity.pin_at(&stage, source.as_ref(), suffix != "-shm")?;
                if suffix == "-shm" {
                    file.set_len(0)?;
                }
                file.sync_all()?;
                capability_dir::rename_noreplace(&stage, source.as_ref(), &stage, target.as_ref())?;
                let pin = identity.pin_at(&stage, target.as_ref(), true)?;
                Ok::<_, io::Error>((identity, pin))
            } else {
                let file = capability_dir::create_private_file(&stage, target.as_ref())?;
                file.sync_all()?;
                let identity = LeafIdentity::from_file(&file)?;
                let pin = identity.pin_at(&stage, target.as_ref(), true)?;
                Ok((identity, pin))
            }
        };
        let (wal, wal_pin) = reserve(0, "-wal")?;
        let (shm, shm_pin) = reserve(1, "-shm")?;
        let (journal, journal_pin) = reserve(2, "-journal")?;
        let prepared = PreparedFamily {
            retry: None,
            runtime_identity: expected,
            main,
            main_sha256,
            sidecars: [wal, shm, journal],
        };
        capability_dir::sync_directory(&stage)?;
        self.record(prepared)?;
        Ok([main_pin, wal_pin, shm_pin, journal_pin])
    }

    fn record(&mut self, prepared: PreparedFamily) -> io::Result<()> {
        let bytes = serde_json::to_vec(&prepared).map_err(io::Error::other)?;
        self.lease.rewind()?;
        self.lease.write_all(&bytes)?;
        self.lease.set_len(bytes.len() as u64)?;
        self.lease.sync_all()?;
        self.prepared = Some(prepared);
        Ok(())
    }

    fn pin_members(&self, stage: &Dir, prepared: &PreparedFamily) -> io::Result<Vec<File>> {
        self.verify_directory_name()?;
        for entry in stage.entries()? {
            let name = entry?.file_name();
            if name != "schema.sqlite"
                && !SIDECARS
                    .iter()
                    .any(|suffix| name == OsStr::new(&format!("reservation{suffix}")))
            {
                return Err(ambiguous(
                    "unexpected SQLite staging member requires offline reconciliation",
                ));
            }
        }
        let mut pins = vec![prepared.pin_main(stage, OsStr::new("schema.sqlite"))?];
        for (suffix, identity) in SIDECARS.into_iter().zip(&prepared.sidecars) {
            let from = format!("reservation{suffix}");
            let to = with_suffix(&self.destination, suffix);
            let to = leaf(&to)?;
            pins.push(
                match (
                    present_at(stage, from.as_ref())?,
                    present_at(&self.parent, to)?,
                ) {
                    (true, false) => identity.pin_at(stage, from.as_ref(), true)?,
                    (false, true) => identity.pin_at(&self.parent, to, true)?,
                    _ => return Err(ambiguous("SQLite reservation is missing or ambiguous")),
                },
            );
        }
        Ok(pins)
    }

    pub(super) fn resume_prepared(&mut self) -> io::Result<Option<u64>> {
        let Some(prepared) = self.prepared.as_ref() else {
            return Ok(None);
        };
        if prepared.retry.is_some() {
            return Ok(None);
        }
        // A persistent inode number alone cannot distinguish inode reuse after
        // process death. A filesystem without allocation generations can publish
        // in the original process, but must preserve interrupted preparation.
        if !prepared.has_durable_generations() {
            return Err(ambiguous(
                "SQLite preparation has no durable file generation for recovery",
            ));
        }
        self.publish_prepared().map(Some)
    }

    fn publish_prepared(&mut self) -> io::Result<u64> {
        let prepared = self
            .prepared
            .as_ref()
            .ok_or_else(|| ambiguous("SQLite preparation receipt is missing"))?;
        let stage = self.stage_cap()?;
        let _pins = self.pin_members(&stage, prepared)?;
        for (suffix, identity) in SIDECARS.into_iter().zip(&prepared.sidecars) {
            let from = format!("reservation{suffix}");
            let to = with_suffix(&self.destination, suffix);
            let to = leaf(&to)?;
            if present_at(&stage, from.as_ref())? {
                capability_dir::rename_noreplace(&stage, from.as_ref(), &self.parent, to)?;
            }
            identity.pin_at(&self.parent, to, true)?;
        }
        capability_dir::sync_directory(&stage)?;
        capability_dir::sync_directory(&self.parent)?;
        self.pin_members(&stage, prepared)?;
        let expected = prepared.runtime_identity;
        capability_dir::rename_noreplace(
            &stage,
            OsStr::new("schema.sqlite"),
            &self.parent,
            leaf(&self.destination)?,
        )?;
        capability_dir::sync_directory(&self.parent)?;
        prepared.pin_main(&self.parent, leaf(&self.destination)?)?;
        drop(stage);
        self.retire()?;
        Ok(expected)
    }

    pub(super) fn publish(mut self, expected: u64, files: (File, [File; 3])) -> io::Result<()> {
        let _creation_pins = self.prepare(expected, files)?;
        self.publish_prepared().map(|_| ())
    }

    fn verify_directory_name(&self) -> io::Result<()> {
        let pinned = self
            .directory_pin
            .as_ref()
            .ok_or_else(|| ambiguous("SQLite staging directory is no longer pinned"))?;
        verify_named_directory(pinned, &self.directory)
    }

    fn retire(&mut self) -> io::Result<()> {
        self.require_empty()?;
        if self.prepared.is_some() {
            // If we crash after clearing the receipt, an empty directory is all
            // that remains of the already-published preparation.
            self.lease.set_len(0)?;
            self.lease.sync_all()?;
        }
        drop(self.directory_pin.take());
        // The kernel refuses a file or a nonempty directory even if a new
        // entry appears after the read. No regular staging leaf is unlinked.
        self.parent.remove_dir(leaf(&self.directory)?)?;
        capability_dir::sync_directory(&self.parent)
    }

    fn require_released_receipt(directory: &Path) -> io::Result<()> {
        if crate::storage::durable_directory_lock_has_data(directory)? {
            return Err(ambiguous(
                "SQLite preparation receipt requires writable reconciliation",
            ));
        }
        Ok(())
    }

    pub(super) fn verify_read_only_handoff(destination: &Path) -> io::Result<()> {
        let directory = Self::path(destination);
        Self::require_released_receipt(&directory)?;
        let pin = match open_private_directory(&directory) {
            Ok(pin) => pin,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in Dir::from_std_file(pin).entries()? {
            // An empty owner-only marker from the previous layout is harmless
            // for read-only admission. Never retire it by mutable pathname.
            let entry = entry?;
            if entry.file_name() != OWNER
                || !entry.file_type()?.is_file()
                || entry.metadata()?.len() != 0
            {
                return Err(ambiguous(
                    "SQLite staging requires writable reconciliation before read-only attachment",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn recover_published(destination: &Path) -> io::Result<()> {
        let directory = Self::path(destination);
        if !present(&directory)? {
            return Self::require_released_receipt(&directory);
        }
        let mut stage = match Self::acquire(destination.to_owned(), false) {
            Ok(stage) => stage,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !present(&directory)? => {
                return Self::require_released_receipt(&directory);
            }
            Err(error) => return Err(error),
        };
        let mut pins = Vec::new();
        if let Some(prepared) = &stage.prepared {
            if prepared.retry.is_some() {
                return Err(ambiguous(
                    "canonical SQLite database appeared during an interrupted schema installation",
                ));
            }
            if !prepared.has_durable_generations() {
                return Err(ambiguous(
                    "SQLite preparation has no durable file generation for recovery",
                ));
            }
            pins.push(prepared.pin_main(&stage.parent, leaf(destination)?)?);
            for (suffix, identity) in SIDECARS.into_iter().zip(&prepared.sidecars) {
                pins.push(identity.pin_at(
                    &stage.parent,
                    leaf(&with_suffix(destination, suffix))?,
                    true,
                )?);
            }
        }
        stage.retire()
    }
}

impl Drop for InitializationDirectory {
    fn drop(&mut self) {
        if self.directory_pin.is_some()
            && self.prepared.is_none()
            && self.require_empty().is_ok()
            && let Err(error) = self.retire()
        {
            tracing::warn!(%error, "could not retire empty SQLite initialization directory");
        }
    }
}

fn require_schema_members(directory: &Dir) -> io::Result<()> {
    for entry in directory.entries()? {
        let name = entry?.file_name();
        if !STAGED_MEMBERS
            .iter()
            .any(|member| name == OsStr::new(member))
        {
            return Err(ambiguous("unproven member in SQLite schema directory"));
        }
    }
    Ok(())
}

fn verify_named_directory(pinned: &File, path: &Path) -> io::Result<()> {
    let named = open_private_directory(path)?;
    #[cfg(unix)]
    let matches = {
        let left = pinned.metadata()?;
        let right = named.metadata()?;
        left.dev() == right.dev() && left.ino() == right.ino()
    };
    #[cfg(windows)]
    let matches = {
        let left = tracedecay_private_fs::windows_file::information(pinned)?;
        let right = tracedecay_private_fs::windows_file::information(&named)?;
        left.volume_serial_number == right.volume_serial_number
            && left.file_index == right.file_index
    };
    #[cfg(not(any(unix, windows)))]
    let matches = false;
    if !matches {
        return Err(ambiguous(
            "SQLite staging directory no longer names the pinned directory",
        ));
    }
    Ok(())
}

fn leaf(path: &Path) -> io::Result<&OsStr> {
    path.file_name()
        .ok_or_else(|| ambiguous("SQLite member has no basename"))
}

fn present_at(directory: &Dir, name: &OsStr) -> io::Result<bool> {
    match directory.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use tempfile::TempDir;
    use tracedecay_private_fs::create_private_file;
    use tracedecay_private_fs::framed_log::rename_noreplace;

    fn seed(path: &Path, value: i64) {
        let db = Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE data(value INTEGER)")
            .unwrap();
        db.execute("INSERT INTO data VALUES (?)", [value]).unwrap();
    }

    fn owned_files(stage: &InitializationDirectory) -> (File, [File; 3]) {
        let path = stage.database();
        let main = File::options().read(true).write(true).open(&path).unwrap();
        (
            main,
            SIDECARS.map(|suffix| create_private_file(&with_suffix(&path, suffix)).unwrap()),
        )
    }

    #[test]
    fn changed_prepared_sqlite_bytes_are_preserved_and_never_published() {
        const CHILD: &str = "TRACEDECAY_SQLITE_PREPARED_SWAP";
        const FOREIGN: &str = "TRACEDECAY_SQLITE_PREPARED_FOREIGN";
        if let Some(target) = std::env::var_os(CHILD) {
            let bytes = fs::read(std::env::var_os(FOREIGN).unwrap()).unwrap();
            let mut file = fs::OpenOptions::new().write(true).open(target).unwrap();
            assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
            file.rewind().unwrap();
            file.write_all(&bytes).unwrap();
            file.sync_all().unwrap();
            return;
        }

        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
        seed(&stage.database(), 7);
        let foreign = root.path().join("foreign.sqlite");
        seed(&foreign, 99);
        let before = fs::read(stage.database()).unwrap();
        assert_eq!(before.len(), fs::metadata(&foreign).unwrap().len() as usize);
        let identity = crate::db::sqlite_generation_identity(&stage.database()).unwrap();
        let files = owned_files(&stage);
        stage.prepare(identity, files).unwrap();

        let (_, module) = module_path!().split_once("::").unwrap();
        let test =
            format!("{module}::changed_prepared_sqlite_bytes_are_preserved_and_never_published");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test])
            .env(CHILD, stage.database())
            .env(FOREIGN, &foreign)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "cross-process mutation failed: {child:?}"
        );
        assert!(
            LeafIdentity::from_file(&File::open(stage.database()).unwrap()).unwrap()
                == stage.prepared.as_ref().unwrap().main
        );
        assert_ne!(fs::read(stage.database()).unwrap(), before);
        assert_eq!(read_value(&stage.database()), 99);
        assert_eq!(
            stage.publish_prepared().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!destination.exists());
        for suffix in SIDECARS {
            assert!(!with_suffix(&destination, suffix).exists());
            assert!(stage.reservation(suffix).exists());
        }
        assert_eq!(read_value(&stage.database()), 99);
        assert_eq!(read_value(&foreign), 99);
    }

    #[test]
    fn process_exit_during_initialization_recovers_without_publishing_partial_schema() {
        const CHILD: &str = "TRACEDECAY_TEST_EXIT_DURING_INITIALIZATION";
        if let Some(destination) = std::env::var_os(CHILD) {
            let mut stage = InitializationDirectory::begin(destination.into()).unwrap();
            seed(&stage.database(), 1);
            let state = std::env::var("TRACEDECAY_TEST_INITIALIZATION_STATE").unwrap();
            if state != "installing" {
                let identity = crate::db::sqlite_generation_identity(&stage.database()).unwrap();
                let files = owned_files(&stage);
                stage.prepare(identity, files).unwrap();
                if state != "prepared" {
                    let count = if state == "reserved" || state == "unproven" {
                        1
                    } else {
                        3
                    };
                    for suffix in SIDECARS.into_iter().take(count) {
                        rename_noreplace(
                            &stage.reservation(suffix),
                            &with_suffix(&stage.destination, suffix),
                        )
                        .unwrap();
                    }
                    if state == "published" {
                        rename_noreplace(&stage.database(), &stage.destination).unwrap();
                    }
                }
            }
            std::process::exit(73);
        }
        let name = format!(
            "{}::process_exit_during_initialization_recovers_without_publishing_partial_schema",
            module_path!().split_once("::").unwrap().1
        );
        for state in [
            "installing",
            "prepared",
            "reserved",
            "published",
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
            let directory = InitializationDirectory::path(&destination);
            if state == "installing" {
                assert!(InitializationDirectory::begin(destination.clone()).is_err());
                assert_eq!(read_value(&directory.join("schema.sqlite")), 1);
                assert!(!destination.exists());
                continue;
            }
            if state == "unproven" {
                let wal = with_suffix(&destination, "-wal");
                fs::write(&wal, b"unproven WAL").unwrap();
                let mut retry = InitializationDirectory::begin(destination.clone()).unwrap();
                assert!(retry.resume_prepared().is_err());
                assert_eq!(fs::read(wal).unwrap(), b"unproven WAL");
                assert!(!destination.exists());
                continue;
            }
            let durable = {
                let stage = InitializationDirectory::acquire(destination.clone(), false).unwrap();
                stage.prepared.as_ref().unwrap().has_durable_generations()
            };
            if !durable {
                let published = state == "published";
                if published {
                    assert!(InitializationDirectory::recover_published(&destination).is_err());
                } else {
                    let mut retry = InitializationDirectory::begin(destination.clone()).unwrap();
                    assert!(retry.resume_prepared().is_err());
                }
                assert_eq!(destination.exists(), published);
                assert_eq!(
                    read_value(
                        if published {
                            destination.clone()
                        } else {
                            directory.join("schema.sqlite")
                        }
                        .as_path()
                    ),
                    1
                );
                assert!(directory.is_dir());
                continue;
            }
            if state == "published" {
                InitializationDirectory::recover_published(&destination).unwrap();
            } else {
                let mut retry = InitializationDirectory::begin(destination.clone()).unwrap();
                assert!(retry.resume_prepared().unwrap().is_some());
            }
            assert_eq!(read_value(&destination), 1);
            let db = Connection::open(&destination).unwrap();
            assert_eq!(
                db.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            assert!(!directory.exists());
        }
    }

    fn read_value(path: &Path) -> i64 {
        Connection::open(path)
            .unwrap()
            .query_row("SELECT value FROM data", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn unproven_private_members_survive_drop_and_retry() {
        let root = TempDir::new().unwrap();
        for member in [
            "schema.sqlite",
            "schema.sqlite-wal",
            "schema.sqlite-shm",
            "schema.sqlite-journal",
            "prepared.sqlite",
        ] {
            let destination = root.path().join(member);
            let stage = InitializationDirectory::begin(destination.clone()).unwrap();
            let path = stage.directory.join(member);
            fs::write(&path, b"unclaimed data").unwrap();
            drop(stage);
            assert!(InitializationDirectory::begin(destination.clone()).is_err());
            assert!(!destination.exists());
            assert_eq!(fs::read(path).unwrap(), b"unclaimed data");
        }
    }

    #[test]
    fn publication_requires_its_original_directory_and_schema() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
        let schema = stage.database();
        seed(&schema, 7);
        let identity = crate::db::sqlite_generation_identity(&schema).unwrap();
        let files = owned_files(&stage);
        let other = root.path().join("independent-directory");
        create_private_directory(&other).unwrap();
        let independent = other.join("schema.sqlite");
        seed(&independent, 99);
        let bytes = fs::read(&independent).unwrap();
        stage.directory = other;
        assert_eq!(
            stage.publish(identity, files).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!destination.exists());
        for suffix in SIDECARS {
            assert!(!with_suffix(&destination, suffix).exists());
        }
        assert_eq!(fs::read(&independent).unwrap(), bytes);
        assert_eq!(read_value(&schema), 7);
    }

    #[test]
    fn missing_stage_with_an_outstanding_receipt_denies_handoff_and_reset() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        seed(&destination, 7);
        let directory = InitializationDirectory::path(&destination);
        let mut lease = crate::storage::try_lock_durable_directory(&directory).unwrap();
        lease.write_all(b"unresolved preparation").unwrap();
        drop(lease);
        assert!(InitializationDirectory::verify_read_only_handoff(&destination).is_err());
        assert!(InitializationDirectory::recover_published(&destination).is_err());
        assert!(lock_offline_sqlite_family_for_reset(&destination).is_err());
        assert!(!directory.exists());
        let mut lease = crate::storage::try_lock_existing_durable_directory(&directory)
            .unwrap()
            .unwrap();
        let mut receipt = String::new();
        lease.read_to_string(&mut receipt).unwrap();
        assert_eq!(receipt, "unresolved preparation");
        assert_eq!(read_value(&destination), 7);
    }

    #[test]
    fn process_exit_during_interrupted_schema_transfer_preserves_or_restores_every_member() {
        const DATABASE: &str = "TRACEDECAY_TEST_EXIT_DURING_RETRY_TRANSFER";
        const MOVES: &str = "TRACEDECAY_TEST_RETRY_TRANSFER_MOVES";
        if let Some(path) = std::env::var_os(DATABASE) {
            let destination = PathBuf::from(path);
            let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
            seed(&stage.database(), 17);
            let identity = crate::db::sqlite_generation_identity(&stage.database()).unwrap();
            let files = owned_files(&stage);
            stage.hold_interrupted(identity, [9; 32], files).unwrap();
            let family = stage.prepared.as_ref().unwrap();
            let recovery = family.retry.as_ref().unwrap().directory(&destination);
            let moves: usize = std::env::var(MOVES).unwrap().parse().unwrap();
            for name in STAGED_MEMBERS.iter().take(moves) {
                rename_noreplace(&recovery.join(name), &stage.directory.join(name)).unwrap();
            }
            if moves > STAGED_MEMBERS.len() {
                fs::remove_dir(&recovery).unwrap();
            }
            std::process::exit(73);
        }
        let test = format!(
            "{}::process_exit_during_interrupted_schema_transfer_preserves_or_restores_every_member",
            module_path!().split_once("::").unwrap().1
        );
        for moves in [1, 3, 4, 5] {
            let root = TempDir::new().unwrap();
            let destination = root.path().join("sessions.db");
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &test])
                .env(DATABASE, &destination)
                .env(MOVES, moves.to_string())
                .output()
                .unwrap();
            assert_eq!(child.status.code(), Some(73), "{child:?}");
            let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
            let family = stage.prepared.as_ref().unwrap();
            let supported = std::iter::once(&family.main)
                .chain(&family.sidecars)
                .take(moves)
                .all(|leaf| leaf.allocation.is_some());
            let recovery = family.retry.as_ref().unwrap().directory(&destination);
            let before = STAGED_MEMBERS.map(|name| {
                let in_stage = stage.directory.join(name);
                let parked = recovery.join(name);
                assert_ne!(in_stage.exists(), parked.exists());
                if in_stage.exists() { in_stage } else { parked }
            });
            let bytes = before.each_ref().map(|file| fs::read(file).unwrap());
            let result = stage.resume_interrupted([9; 32]);
            if supported {
                let pins = result.unwrap().unwrap();
                for (name, contents) in STAGED_MEMBERS.into_iter().zip(bytes) {
                    assert_eq!(fs::read(stage.directory.join(name)).unwrap(), contents);
                }
                drop(pins);
                assert!(!recovery.exists());
            } else {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
                for (file, contents) in before.iter().zip(bytes) {
                    assert_eq!(fs::read(file).unwrap(), contents);
                }
            }
            assert!(!destination.exists());
        }
    }

    #[test]
    fn interrupted_install_requires_its_entire_recorded_family_before_restoring_any_member() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
        seed(&stage.database(), 7);
        let files = owned_files(&stage);
        let identity = crate::db::sqlite_generation_identity(&stage.database()).unwrap();
        stage.hold_interrupted(identity, [1; 32], files).unwrap();
        let recovery = stage
            .prepared
            .as_ref()
            .unwrap()
            .retry
            .as_ref()
            .unwrap()
            .directory(&destination);
        drop(stage);
        let mut retry = InitializationDirectory::begin(destination.clone()).unwrap();
        assert_eq!(
            retry.resume_interrupted([2; 32]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(retry);
        let wal = recovery.join("schema.sqlite-wal");
        fs::write(&wal, b"independent WAL bytes").unwrap();
        let mut retry = InitializationDirectory::begin(destination.clone()).unwrap();
        assert_eq!(
            retry.resume_interrupted([1; 32]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&wal).unwrap(), b"independent WAL bytes");
        assert!(recovery.join("schema.sqlite").is_file());
        assert!(
            retry
                .stage_cap()
                .unwrap()
                .entries()
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!destination.exists());
        assert!(lock_offline_sqlite_family_for_reset(&destination).is_err());
    }

    #[test]
    fn preparation_refuses_unclaimed_sidecar_handles_without_modifying_the_family() {
        let root = TempDir::new().unwrap();
        let destination = root.path().join("sessions.db");
        let mut stage = InitializationDirectory::begin(destination.clone()).unwrap();
        seed(&stage.database(), 9);
        let identity = crate::db::sqlite_generation_identity(&stage.database()).unwrap();
        let (main, mut files) = owned_files(&stage);
        let separate = root.path().join("unrelated-empty-reservation");
        files[1] = create_private_file(&separate).unwrap();
        let shm = with_suffix(&stage.database(), "-shm");
        fs::write(&shm, b"preserve existing index").unwrap();
        let error = stage.prepare(identity, (main, files)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(shm).unwrap(), b"preserve existing index");
        assert_eq!(fs::metadata(separate).unwrap().len(), 0);
        assert!(!destination.exists());
        for suffix in SIDECARS {
            assert!(!stage.reservation(suffix).exists());
        }
    }

    /// A competing process may rename the private directory despite the
    /// cooperative lease and install a symlink at the old path. Failed-start
    /// cleanup must preserve occupied entries in the pinned directory and never
    /// touch the distinct SQLite database beneath that replacement symlink.
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
        assert!(retired.join("schema.sqlite").is_file());
        for suffix in ["-wal", "-shm", "-journal"] {
            assert_eq!(
                fs::read(with_suffix(&retired.join("schema.sqlite"), suffix)).unwrap(),
                b"owned staging sidecar"
            );
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
        assert!(original.is_dir());
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
        let destination = root.path().join("other.db");
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
