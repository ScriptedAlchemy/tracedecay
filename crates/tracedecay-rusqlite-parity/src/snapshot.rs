use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags, params};
use sha2::{Digest, Sha256};
use tracedecay_sqlite_parity_protocol::{
    CopiedDatabase, ErrorCode, ErrorPayload, SnapshotFileIdentity, SourceHeaderJournalMode,
    VerifiedCopiedSnapshot, validate_copied_snapshot_provenance,
};

use crate::{closed_sql, sqlite_metadata};

const READ_ONLY_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_URI)
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_NOFOLLOW);

pub(crate) struct ReadOnlyDriver {
    pub(crate) canonical_path: PathBuf,
    pub(crate) source_header_journal_mode: SourceHeaderJournalMode,
    pub(crate) connection: Connection,
}

impl ReadOnlyDriver {
    pub(crate) fn open(snapshot: &VerifiedCopiedSnapshot) -> Result<Self, ErrorPayload> {
        let canonical_path = validate_verified_snapshot(snapshot)?;
        let source_header_journal_mode =
            sqlite_metadata::read_source_header_journal_mode(&canonical_path)?;
        let mut uri = url::Url::from_file_path(&canonical_path).map_err(|()| {
            ErrorPayload::new(
                ErrorCode::InvalidPath,
                "copied snapshot path could not be represented as a file URI",
            )
            .with_path(&canonical_path)
        })?;
        uri.query_pairs_mut()
            .append_pair("mode", "ro")
            .append_pair("immutable", "1");

        let connection =
            Connection::open_with_flags(uri.as_str(), READ_ONLY_FLAGS).map_err(|error| {
                let mut payload = sqlite_error(
                    ErrorCode::OpenFailed,
                    "could not open copied snapshot read-only/no-create",
                    error,
                );
                payload.path = Some(canonical_path.clone());
                payload
            })?;
        validate_verified_snapshot(snapshot)?;
        connection
            .execute_batch(closed_sql::SET_QUERY_ONLY)
            .map_err(|error| {
                sqlite_error(
                    ErrorCode::ReadOnlyInvariant,
                    "could not enable SQLite query_only",
                    error,
                )
                .with_path(&canonical_path)
            })?;
        let observed = connection
            .query_row(closed_sql::QUERY_ONLY, [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_query_error)?;
        if observed != 1 {
            return Err(ErrorPayload::new(
                ErrorCode::ReadOnlyInvariant,
                format!("SQLite query_only was not retained (observed {observed})"),
            )
            .with_path(&canonical_path));
        }
        // Match the runtime reader policy so foreign-key pragma state is comparable.
        connection
            .execute_batch(closed_sql::SET_FOREIGN_KEYS)
            .map_err(|error| {
                sqlite_error(
                    ErrorCode::ReadOnlyInvariant,
                    "could not enable SQLite foreign_keys",
                    error,
                )
                .with_path(&canonical_path)
            })?;
        let foreign_keys = connection
            .query_row(closed_sql::FOREIGN_KEYS, [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_query_error)?;
        if foreign_keys != 1 {
            return Err(ErrorPayload::new(
                ErrorCode::ReadOnlyInvariant,
                format!("SQLite foreign_keys was not retained (observed {foreign_keys})"),
            )
            .with_path(&canonical_path));
        }

        Ok(Self {
            canonical_path,
            source_header_journal_mode,
            connection,
        })
    }

    pub(crate) fn table_exists(&self, spec: closed_sql::TableSpec) -> Result<bool, ErrorPayload> {
        self.connection
            .query_row(closed_sql::TABLE_EXISTS, params![spec.identifier], |row| {
                row.get::<_, i64>(0)
            })
            .map(|exists| exists != 0)
            .map_err(sqlite_query_error)
    }

    pub(crate) fn count_rows(&self, spec: closed_sql::TableSpec) -> Result<i64, ErrorPayload> {
        self.connection
            .query_row(spec.count_sql, [], |row| row.get(0))
            .map_err(sqlite_query_error)
    }
}

pub(crate) fn sqlite_query_error(source: rusqlite::Error) -> ErrorPayload {
    sqlite_error(
        ErrorCode::SqliteFailure,
        "read-only SQLite probe failed",
        source,
    )
}

fn sqlite_error(
    code: ErrorCode,
    message: impl Into<String>,
    source: rusqlite::Error,
) -> ErrorPayload {
    let sqlite_code = match &source {
        rusqlite::Error::SqliteFailure(error, _) => Some(format!("{:?}", error.code)),
        _ => None,
    };
    ErrorPayload {
        code,
        message: format!("{}: {source}", message.into()),
        path: None,
        sqlite_code,
    }
}

pub(crate) fn verify_copied_snapshot(
    database: &CopiedDatabase,
) -> Result<VerifiedCopiedSnapshot, ErrorPayload> {
    let provenance = &database.provenance;
    validate_copied_snapshot_provenance(provenance)?;
    let staging_root = validate_staging_root(&provenance.staging_root)?;
    let canonical_path = validate_copied_path(&database.path)?;
    if !canonical_path.starts_with(&staging_root) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot is outside its declared private staging root",
        )
        .with_path(&canonical_path));
    }
    if canonical_path != provenance.canonical_path {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot canonical path does not match its sealed provenance",
        )
        .with_path(&canonical_path));
    }
    let (byte_len, content_digest, file_identity) = sealed_file_metadata(&canonical_path)?;
    if byte_len != provenance.byte_len {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!(
                "copied snapshot byte length changed from {} to {byte_len}",
                provenance.byte_len
            ),
        )
        .with_path(&canonical_path));
    }
    if file_identity != provenance.file_identity {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot file identity does not match its sealed provenance",
        )
        .with_path(&canonical_path));
    }
    if content_digest != provenance.content_digest {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot content digest does not match its sealed provenance",
        )
        .with_path(&canonical_path));
    }
    Ok(VerifiedCopiedSnapshot {
        authority_identity: provenance.authority_identity.clone(),
        canonical_path,
        byte_len,
        content_digest,
        file_identity,
    })
}

pub(crate) fn validate_verified_snapshot(
    snapshot: &VerifiedCopiedSnapshot,
) -> Result<PathBuf, ErrorPayload> {
    let canonical_path = validate_copied_path(&snapshot.canonical_path)?;
    if canonical_path != snapshot.canonical_path {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "verified snapshot path is not canonical",
        )
        .with_path(&canonical_path));
    }
    let (byte_len, content_digest, file_identity) = sealed_file_metadata(&canonical_path)?;
    if byte_len != snapshot.byte_len
        || content_digest != snapshot.content_digest
        || file_identity != snapshot.file_identity
    {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot changed after provenance verification",
        )
        .with_path(&canonical_path));
    }
    Ok(canonical_path)
}

pub(crate) fn validate_copied_path(path: &Path) -> Result<PathBuf, ErrorPayload> {
    if !path.is_absolute() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPath,
            "copied snapshot path must be absolute",
        )
        .with_path(path));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidPath,
            format!("copied snapshot path is not an existing regular file: {error}"),
        )
        .with_path(path)
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidPath,
            "copied snapshot path must be a regular file and not a symlink",
        )
        .with_path(path));
    }
    let canonical = fs::canonicalize(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidPath,
            format!("could not canonicalize copied snapshot path: {error}"),
        )
        .with_path(path)
    })?;
    reject_protected_profile_path(&canonical, &protected_profile_roots())?;
    Ok(canonical)
}

fn validate_staging_root(path: &Path) -> Result<PathBuf, ErrorPayload> {
    if !path.is_absolute() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "snapshot staging root must be absolute",
        )
        .with_path(path));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("snapshot staging root is not an existing directory: {error}"),
        )
        .with_path(path)
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "snapshot staging root must be a directory and not a symlink",
        )
        .with_path(path));
    }
    fs::canonicalize(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not canonicalize snapshot staging root: {error}"),
        )
        .with_path(path)
    })
}

pub(crate) fn sealed_file_metadata(
    path: &Path,
) -> Result<(u64, String, SnapshotFileIdentity), ErrorPayload> {
    let mut file = fs::File::open(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not open copied snapshot for provenance verification: {error}"),
        )
        .with_path(path)
    })?;
    let before = file.metadata().map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not inspect copied snapshot provenance: {error}"),
        )
        .with_path(path)
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ErrorPayload::new(
                ErrorCode::InvalidSnapshotProvenance,
                format!("could not hash copied snapshot provenance: {error}"),
            )
            .with_path(path)
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after = fs::metadata(path).map_err(|error| {
        ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            format!("could not revalidate copied snapshot provenance: {error}"),
        )
        .with_path(path)
    })?;
    let identity = SnapshotFileIdentity::from_metadata(&before);
    if before.len() != after.len() || identity != SnapshotFileIdentity::from_metadata(&after) {
        return Err(ErrorPayload::new(
            ErrorCode::InvalidSnapshotProvenance,
            "copied snapshot changed while its provenance was verified",
        )
        .with_path(path));
    }
    Ok((
        before.len(),
        format!("sha256:{}", hex::encode(hasher.finalize())),
        identity,
    ))
}

fn protected_profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = env::var_os("TRACEDECAY_DATA_DIR").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(root));
    }
    for home_var in ["HOME", "USERPROFILE"] {
        if let Some(home) = env::var_os(home_var).filter(|value| !value.is_empty()) {
            roots.push(PathBuf::from(home).join(".tracedecay"));
        }
    }
    roots
}

pub(crate) fn reject_protected_profile_path(
    canonical: &Path,
    protected_roots: &[PathBuf],
) -> Result<(), ErrorPayload> {
    let has_profile_component = canonical.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".tracedecay")
    });
    let under_protected_root = protected_roots.iter().any(|root| {
        let absolute = if root.is_absolute() {
            root.clone()
        } else {
            env::current_dir()
                .map(|current| current.join(root))
                .unwrap_or_else(|_| root.clone())
        };
        let normalized = fs::canonicalize(&absolute).unwrap_or(absolute);
        canonical.starts_with(normalized)
    });
    if has_profile_component || under_protected_root {
        return Err(ErrorPayload::new(
            ErrorCode::RefusedLiveProfile,
            "path is inside a live/default TraceDecay profile; inspect an explicit copy elsewhere",
        )
        .with_path(canonical));
    }
    Ok(())
}
