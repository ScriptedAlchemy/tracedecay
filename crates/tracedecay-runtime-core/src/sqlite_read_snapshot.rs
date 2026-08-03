//! Side-effect-free logical inspection of `SQLite` database families.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use fs2::FileExt;
use libsql::{Builder, Connection, OpenFlags};
use sha2::{Digest, Sha256};

static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(0);
const SQLITE_OPEN_URI: i32 = 0x0000_0040;

pub struct SnapshotDatabase {
    connection: Connection,
    _database: libsql::Database,
    source: PathBuf,
    source_state: Vec<FileState>,
    path: PathBuf,
    _scratch: Option<Arc<ScratchDirectory>>,
    _authority: crate::db::DatabaseAuthority,
    #[cfg(test)]
    copied_bytes: u64,
}

impl SnapshotDatabase {
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn validate_source(&self) -> io::Result<()> {
        if family_state(&self.source)? == self.source_state {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "SQLite database family '{}' changed after its read snapshot",
            self.source.display()
        )))
    }

    pub fn source_generation(&self) -> SourceGeneration {
        SourceGeneration {
            source: self.source.clone(),
            states: self.source_state.clone(),
        }
    }

    #[cfg(test)]
    pub fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }
}

#[derive(Debug, Clone)]
pub struct SourceGeneration {
    source: PathBuf,
    states: Vec<FileState>,
}

impl SourceGeneration {
    pub fn validate(&self) -> io::Result<()> {
        if family_state(&self.source)? == self.states {
            return Ok(());
        }
        Err(io::Error::other(format!(
            "SQLite database family '{}' changed after inspection",
            self.source.display()
        )))
    }
}

pub struct SnapshotSet {
    databases: BTreeMap<PathBuf, SnapshotDatabase>,
    copied_bytes: u64,
    #[allow(dead_code)]
    scratch: Arc<ScratchDirectory>,
}

impl SnapshotSet {
    pub async fn capture(paths: &[PathBuf]) -> io::Result<Self> {
        let root = default_scratch_root(paths)?;
        Self::capture_in(paths, &root).await
    }

    pub async fn capture_in(paths: &[PathBuf], root: &Path) -> io::Result<Self> {
        let scratch = Arc::new(create_scratch_directory(root, expected_owner(paths)?)?);
        let mut unique = paths.to_vec();
        unique.sort();
        unique.dedup();
        let mut prepared = Vec::new();
        let mut copied_bytes = 0_u64;
        for (index, path) in unique.into_iter().enumerate() {
            let snapshot = prepare_one(&path, &scratch, index)?;
            copied_bytes = copied_bytes.saturating_add(snapshot.copy_bytes);
            prepared.push(snapshot);
        }
        let available = fs2::available_space(&scratch.path)?;
        if copied_bytes > available {
            return Err(io::Error::other(format!(
                "insufficient scratch space for SQLite read snapshots: required {copied_bytes} bytes, available {available} bytes at '{}'",
                scratch.path.display()
            )));
        }
        let mut databases = BTreeMap::new();
        for snapshot in prepared {
            let source = snapshot.source.clone();
            let database = finish_one(snapshot, Arc::clone(&scratch)).await?;
            databases.insert(source, database);
        }
        Ok(Self {
            databases,
            copied_bytes,
            scratch,
        })
    }

    pub fn get(&self, path: &Path) -> io::Result<&SnapshotDatabase> {
        self.databases.get(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no frozen SQLite snapshot for '{}'", path.display()),
            )
        })
    }

    pub fn validate_sources_unchanged(&self) -> io::Result<()> {
        for database in self.databases.values() {
            database.validate_source()?;
        }
        Ok(())
    }

    pub fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    #[cfg(test)]
    pub fn database_count(&self) -> usize {
        self.databases.len()
    }
}

struct PreparedSnapshot {
    source: PathBuf,
    source_state: Vec<FileState>,
    target: PathBuf,
    mode: SnapshotMode,
    copy_bytes: u64,
    authority: crate::db::DatabaseAuthority,
}

#[derive(Clone, Copy)]
enum SnapshotMode {
    DirectImmutable,
    Reflink,
    Copy,
}

struct ScratchDirectory {
    path: PathBuf,
    owner_lock: Option<File>,
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        drop(self.owner_lock.take());
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileState {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(unix)]
    links: u64,
}

/// Opens one source family without mutating it. Checkpointed DBs are read
/// directly through `SQLite` immutable mode. WAL-backed DBs are reflinked when
/// supported, then fall back to one full copy with WAL/SHM copied alongside.
pub async fn open(path: &Path) -> io::Result<SnapshotDatabase> {
    let mut snapshots = SnapshotSet::capture(&[path.to_path_buf()]).await?;
    snapshots.databases.remove(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no frozen SQLite snapshot for '{}'", path.display()),
        )
    })
}

pub async fn open_in(path: &Path, root: &Path) -> io::Result<SnapshotDatabase> {
    let mut snapshots = SnapshotSet::capture_in(&[path.to_path_buf()], root).await?;
    snapshots.databases.remove(path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no frozen SQLite snapshot for '{}'", path.display()),
        )
    })
}

pub fn family_fingerprint(path: &Path) -> io::Result<String> {
    use std::io::Read;

    let _authority = crate::db::DatabaseAuthority::for_runtime(
        path,
        "fingerprint SQLite family for offline maintenance",
    )
    .map_err(io::Error::other)?;
    let before = family_state(path)?;
    let mut hash = Sha256::new();
    for (label, member) in [
        (b"db".as_slice(), path.to_path_buf()),
        (b"wal", with_suffix(path, "-wal")),
    ] {
        if !member.is_file() {
            continue;
        }
        let bytes = fs::metadata(&member)?.len();
        // BEGIN IMMEDIATE may create an empty WAL while acquiring the apply
        // guard. An empty sidecar contains no logical database state.
        if label == b"wal" && bytes == 0 {
            continue;
        }
        hash.update(label);
        hash.update(bytes.to_be_bytes());
        let mut file = fs::File::open(&member)?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
    }
    if family_state(path)? != before {
        return Err(changed_during_snapshot(path));
    }
    Ok(hex::encode(hash.finalize()))
}

fn prepare_one(
    source: &Path,
    scratch: &ScratchDirectory,
    index: usize,
) -> io::Result<PreparedSnapshot> {
    let authority = crate::db::DatabaseAuthority::for_runtime(
        source,
        "capture SQLite family for offline maintenance",
    )
    .map_err(io::Error::other)?;
    let directory = scratch.path.join(index.to_string());
    create_private_directory(&directory)?;
    let target = directory.join("database.db");
    let source_state = family_state(source)?;
    let main = source_state
        .iter()
        .find(|state| state.path == source)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("SQLite database '{}' does not exist", source.display()),
            )
        })?;
    let has_wal = source_state
        .iter()
        .any(|state| state.path == with_suffix(source, "-wal"));
    let mode = if has_wal {
        if reflink_copy::reflink(source, &target).is_ok() {
            SnapshotMode::Reflink
        } else {
            let _ = fs::remove_file(&target);
            SnapshotMode::Copy
        }
    } else {
        checkpointed_snapshot_mode()
    };
    let mut copy_bytes = if matches!(mode, SnapshotMode::Copy) {
        main.bytes
    } else {
        0
    };
    if !matches!(mode, SnapshotMode::DirectImmutable) {
        for suffix in ["-wal", "-shm"] {
            let source_member = with_suffix(source, suffix);
            if let Some(state) = source_state
                .iter()
                .find(|state| state.path == source_member)
            {
                copy_bytes = copy_bytes.saturating_add(state.bytes);
            }
        }
    }
    if family_state(source)? != source_state {
        return Err(changed_during_snapshot(source));
    }
    Ok(PreparedSnapshot {
        source: source.to_path_buf(),
        source_state,
        target,
        mode,
        copy_bytes,
        authority,
    })
}

fn checkpointed_snapshot_mode() -> SnapshotMode {
    // SQLite's immutable connection still holds a byte-range lock on Windows.
    // Consolidation retains read snapshots while copying the frozen inputs, so
    // opening a private copy keeps those handles off the source database.
    #[cfg(windows)]
    {
        SnapshotMode::Copy
    }
    #[cfg(not(windows))]
    {
        SnapshotMode::DirectImmutable
    }
}

async fn finish_one(
    prepared: PreparedSnapshot,
    scratch: Arc<ScratchDirectory>,
) -> io::Result<SnapshotDatabase> {
    if matches!(prepared.mode, SnapshotMode::Copy) {
        fs::copy(&prepared.source, &prepared.target)?;
    }
    if !matches!(prepared.mode, SnapshotMode::DirectImmutable) {
        for suffix in ["-wal", "-shm"] {
            let source_member = with_suffix(&prepared.source, suffix);
            let Some(_) = prepared
                .source_state
                .iter()
                .find(|state| state.path == source_member)
            else {
                continue;
            };
            fs::copy(&source_member, with_suffix(&prepared.target, suffix))?;
        }
    }
    if family_state(&prepared.source)? != prepared.source_state {
        return Err(changed_during_snapshot(&prepared.source));
    }
    let (open_path, flags, scratch) = if matches!(prepared.mode, SnapshotMode::DirectImmutable) {
        (
            PathBuf::from(immutable_uri(&prepared.source)?),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::from_bits_retain(SQLITE_OPEN_URI),
            None,
        )
    } else {
        (
            prepared.target.clone(),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
            Some(scratch),
        )
    };
    let database = Builder::new_local(&open_path)
        .flags(flags)
        .build()
        .await
        .map_err(io::Error::other)?;
    let connection = database.connect().map_err(io::Error::other)?;
    connection
        .execute_batch("PRAGMA query_only = ON;")
        .await
        .map_err(io::Error::other)?;
    let snapshot = SnapshotDatabase {
        connection,
        _database: database,
        source: prepared.source,
        source_state: prepared.source_state,
        path: open_path,
        _scratch: scratch,
        _authority: prepared.authority,
        #[cfg(test)]
        copied_bytes: prepared.copy_bytes,
    };
    snapshot.validate_source()?;
    Ok(snapshot)
}

fn changed_during_snapshot(source: &Path) -> io::Error {
    io::Error::other(format!(
        "SQLite database family '{}' changed while taking a read snapshot",
        source.display()
    ))
}

fn create_scratch_directory(
    root: &Path,
    expected_uid: Option<u32>,
) -> io::Result<ScratchDirectory> {
    ensure_private_root(root, expected_uid)?;
    let cleanup_lock = open_private_lock(&root.join(".cleanup.lock"), true)?;
    cleanup_lock.lock_exclusive()?;
    cleanup_stale_directories(root)?;
    for _ in 0..100 {
        let id = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("read-{}-{id}", std::process::id()));
        match create_private_directory(&path) {
            Ok(()) => {
                let owner_lock = open_private_lock(&path.join(".owner.lock"), true)?;
                owner_lock.lock_exclusive()?;
                FileExt::unlock(&cleanup_lock)?;
                return Ok(ScratchDirectory {
                    path,
                    owner_lock: Some(owner_lock),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique SQLite read snapshot directory",
    ))
}

fn default_scratch_root(paths: &[PathBuf]) -> io::Result<PathBuf> {
    #[cfg(unix)]
    {
        let uid = expected_owner(paths)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no SQLite input path was supplied")
        })?;
        Ok(std::env::temp_dir().join(format!("tracedecay-sqlite-read-{uid}")))
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        Ok(std::env::temp_dir().join("tracedecay-sqlite-read"))
    }
}

fn expected_owner(paths: &[PathBuf]) -> io::Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let path = paths.first().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no SQLite input path was supplied",
            )
        })?;
        Ok(Some(fs::metadata(path)?.uid()))
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        Ok(None)
    }
}

fn ensure_private_root(root: &Path, expected_uid: Option<u32>) -> io::Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::other(format!(
                "SQLite scratch root '{}' is not a directory",
                root.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            create_private_directory(root)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::symlink_metadata(root)?;
        if expected_uid.is_some_and(|uid| metadata.uid() != uid) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "SQLite scratch root '{}' has the wrong owner",
                    root.display()
                ),
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn open_private_lock(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn cleanup_stale_directories(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("read-") {
            continue;
        }
        let path = entry.path();
        if !fs::symlink_metadata(&path)?.is_dir() {
            continue;
        }
        let removable = match open_private_lock(&path.join(".owner.lock"), false) {
            Ok(lock) => lock.try_lock_exclusive().is_ok(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        if removable {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn family_state(path: &Path) -> io::Result<Vec<FileState>> {
    let mut states = Vec::new();
    for member in family_paths(path) {
        match fs::metadata(&member) {
            Ok(metadata) if metadata.is_file() => {
                #[cfg(unix)]
                use std::os::unix::fs::MetadataExt;
                states.push(FileState {
                    path: member,
                    bytes: metadata.len(),
                    modified: metadata.modified()?,
                    #[cfg(unix)]
                    device: metadata.dev(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                    #[cfg(unix)]
                    changed_seconds: metadata.ctime(),
                    #[cfg(unix)]
                    changed_nanoseconds: metadata.ctime_nsec(),
                    #[cfg(unix)]
                    links: metadata.nlink(),
                });
            }
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "SQLite family member '{}' is not a file",
                    member.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(states)
}

fn family_paths(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ]
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn immutable_uri(path: &Path) -> io::Result<String> {
    let raw = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite path '{}' is not UTF-8", path.display()),
        )
    })?;
    let mut encoded = String::with_capacity(raw.len() + 24);
    for ch in raw.chars() {
        match ch {
            '?' => encoded.push_str("%3f"),
            '#' => encoded.push_str("%23"),
            '%' => encoded.push_str("%25"),
            other => encoded.push(other),
        }
    }
    Ok(format!("file:{encoded}?immutable=1&mode=ro"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn snapshot_reads_wal_rows_without_touching_source_bytes_or_mtime() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let database = Builder::new_local(&path).build().await.unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE durable(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('wal-resident');",
            )
            .await
            .unwrap();
        assert!(with_suffix(&path, "-wal").metadata().unwrap().len() > 0);
        let before = family_state(&path).unwrap();

        let snapshot = open(&path).await.unwrap();
        let mut rows = snapshot
            .connection()
            .query("SELECT value FROM durable", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "wal-resident"
        );
        assert_eq!(family_state(&path).unwrap(), before);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn checkpointed_database_reads_directly_without_copy_or_metadata_change() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let database = Builder::new_local(&path).build().await.unwrap();
        let connection = database.connect().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE durable(value TEXT NOT NULL);
                 INSERT INTO durable(value) VALUES ('checkpointed');",
            )
            .await
            .unwrap();
        drop(connection);
        drop(database);
        let before = family_state(&path).unwrap();
        let snapshots = SnapshotSet::capture(std::slice::from_ref(&path))
            .await
            .unwrap();
        assert_eq!(snapshots.copied_bytes(), 0);
        let mut rows = snapshots
            .get(&path)
            .unwrap()
            .connection()
            .query("SELECT value FROM durable", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            "checkpointed"
        );
        assert_eq!(family_state(&path).unwrap(), before);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn checkpointed_snapshot_does_not_lock_source_against_copying() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let database = Builder::new_local(&path).build().await.unwrap();
        database
            .connect()
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .await
            .unwrap();
        drop(database);

        let snapshots = SnapshotSet::capture(std::slice::from_ref(&path))
            .await
            .unwrap();
        assert_eq!(snapshots.copied_bytes(), fs::metadata(&path).unwrap().len());
        fs::copy(&path, temp.path().join("backup.db")).unwrap();
    }

    #[test]
    fn empty_wal_does_not_change_the_content_fingerprint() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        fs::write(&path, b"database bytes").unwrap();
        let before = family_fingerprint(&path).unwrap();
        fs::write(with_suffix(&path, "-wal"), b"").unwrap();
        assert_eq!(family_fingerprint(&path).unwrap(), before);
        fs::write(with_suffix(&path, "-wal"), b"logical frame").unwrap();
        assert_ne!(family_fingerprint(&path).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scratch_is_private_and_next_capture_cleans_crash_debris() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let path = temp.path().join("source.db");
        let scratch_root = temp.path().join("private-scratch");
        let database = Builder::new_local(&path).build().await.unwrap();
        database
            .connect()
            .unwrap()
            .execute_batch("CREATE TABLE durable(value TEXT NOT NULL);")
            .await
            .unwrap();
        drop(database);

        ensure_private_root(
            &scratch_root,
            expected_owner(std::slice::from_ref(&path)).unwrap(),
        )
        .unwrap();
        let stale = scratch_root.join("read-999999-0");
        create_private_directory(&stale).unwrap();
        fs::write(stale.join("database.db"), b"private session data").unwrap();
        fs::write(stale.join(".owner.lock"), b"").unwrap();

        let snapshots = SnapshotSet::capture_in(&[path], &scratch_root)
            .await
            .unwrap();
        assert!(
            !stale.exists(),
            "an unlocked crashed snapshot must be cleaned"
        );
        assert_eq!(
            fs::metadata(&scratch_root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&snapshots.scratch.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let live = snapshots.scratch.path.clone();
        drop(snapshots);
        assert!(
            !live.exists(),
            "normal drop must remove copied database data"
        );
        assert!(
            fs::read_dir(&scratch_root)
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == ".cleanup.lock")
        );
    }
}
