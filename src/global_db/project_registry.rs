use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use libsql::params;

use super::{GlobalDb, global_db_operation_error, global_db_operation_message};

pub(super) const NATIVE_PROJECT_PATH_ALIAS_PREFIX: &str = "tracedecay-project-path-v1";

#[derive(Clone, Copy)]
pub(super) enum LegacyPathAliasKind {
    ProjectRoot,
    GitCommonDir,
}

impl LegacyPathAliasKind {
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::ProjectRoot => "",
            Self::GitCommonDir => "git-common-dir:",
        }
    }

    pub(super) fn owner_query(self) -> &'static str {
        match self {
            Self::ProjectRoot => {
                "SELECT project_id FROM code_projects WHERE canonical_root = ?1 ORDER BY project_id"
            }
            Self::GitCommonDir => {
                "SELECT project_id FROM code_projects WHERE git_common_dir = ?1 ORDER BY project_id"
            }
        }
    }
}

pub(super) fn canonical_project_path(project_path: &Path) -> PathBuf {
    std::fs::canonicalize(project_path).unwrap_or_else(|_| project_path.to_path_buf())
}

pub(super) fn project_path_alias_key(project_path: &Path) -> String {
    let canonical = canonical_project_path(project_path);
    if let Some(path) = canonical.to_str() {
        return path.to_string();
    }
    native_project_path_alias_key(&canonical)
}

fn native_project_path_alias_key(path: &Path) -> String {
    encode_native_project_path_alias(
        native_project_path_platform(),
        &encode_native_project_path(path),
    )
}

#[cfg(unix)]
pub(super) fn native_project_path_platform() -> &'static str {
    "unix-bytes"
}

#[cfg(windows)]
pub(super) fn native_project_path_platform() -> &'static str {
    "windows-utf16le"
}

#[cfg(not(any(unix, windows)))]
pub(super) fn native_project_path_platform() -> &'static str {
    "rust-os-str"
}

#[cfg(unix)]
pub(super) fn encode_native_project_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
pub(super) fn encode_native_project_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(not(any(unix, windows)))]
pub(super) fn encode_native_project_path(path: &Path) -> Vec<u8> {
    path.as_os_str().as_encoded_bytes().to_vec()
}

#[cfg(unix)]
pub(super) fn decode_native_project_path(
    platform: &str,
    bytes: Vec<u8>,
) -> Result<PathBuf, String> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    if platform != native_project_path_platform() {
        return Err(format!(
            "native project path belongs to platform '{platform}'"
        ));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(windows)]
pub(super) fn decode_native_project_path(
    platform: &str,
    bytes: Vec<u8>,
) -> Result<PathBuf, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    if platform != native_project_path_platform() {
        return Err(format!(
            "native project path belongs to platform '{platform}'"
        ));
    }
    if bytes.len() % 2 != 0 {
        return Err("native Windows project path has odd byte length".to_string());
    }
    let wide = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn decode_native_project_path(
    _platform: &str,
    _bytes: Vec<u8>,
) -> Result<PathBuf, String> {
    Err("native project paths are unsupported on this platform".to_string())
}

pub(super) fn encode_native_project_path_alias(platform: &str, native_path: &[u8]) -> String {
    format!(
        "{NATIVE_PROJECT_PATH_ALIAS_PREFIX}-{platform}-{}",
        hex::encode(native_path)
    )
}

pub(super) fn decode_native_project_path_alias(alias: &str) -> Result<Option<PathBuf>, String> {
    if !alias.starts_with(NATIVE_PROJECT_PATH_ALIAS_PREFIX) {
        return Ok(None);
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err("native project path aliases are unsupported on this platform".to_string())
    }

    #[cfg(any(unix, windows))]
    {
        let prefix = format!(
            "{NATIVE_PROJECT_PATH_ALIAS_PREFIX}-{}-",
            native_project_path_platform()
        );
        let Some(encoded) = alias.strip_prefix(&prefix) else {
            return Err("native project path alias belongs to another platform".to_string());
        };
        let bytes = hex::decode(encoded).map_err(|error| error.to_string())?;
        let path = decode_native_project_path(native_project_path_platform(), bytes);
        #[cfg(windows)]
        let path = path.map_err(native_project_path_alias_decode_error);
        path.map(Some)
    }
}

#[cfg(windows)]
fn native_project_path_alias_decode_error(error: String) -> String {
    if error == "native Windows project path has odd byte length" {
        "native Windows project path alias has odd byte length".to_string()
    } else {
        error
    }
}

pub(super) async fn migrate_project_rows_to_canonical_keys(
    db: &GlobalDb,
) -> Result<(), libsql::Error> {
    let transaction = db.begin_authoritative_transaction().await?;
    let mut rows = transaction
        .query("SELECT path, tokens_saved FROM projects", ())
        .await?;
    let mut replacements = Vec::new();
    while let Some(row) = rows.next().await? {
        let old_path: String = row.get(0)?;
        let tokens_saved: i64 = row.get(1)?;
        let canonical_path = GlobalDb::canonical_project_key(Path::new(&old_path));
        if old_path != canonical_path {
            replacements.push((old_path, canonical_path, tokens_saved));
        }
    }
    drop(rows);

    for (old_path, canonical_path, tokens_saved) in replacements {
        transaction
            .execute(
                "INSERT INTO projects (path, tokens_saved) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET
                    tokens_saved = MAX(tokens_saved, excluded.tokens_saved)",
                params![canonical_path, tokens_saved],
            )
            .await?;
        transaction
            .execute("DELETE FROM projects WHERE path = ?1", params![old_path])
            .await?;
    }
    transaction.commit().await
}

pub(super) async fn list_code_project_paths(
    db: &GlobalDb,
    limit: usize,
) -> crate::errors::Result<Vec<PathBuf>> {
    const OPERATION: &str = "list native code project paths";

    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut rows = db
        .conn
        .query(
            "SELECT project_id, canonical_root, display_root, primary_root_platform,
                    primary_root_bytes, primary_root_last_seen_at, last_seen_at
             FROM code_projects
             ORDER BY last_seen_at DESC, project_id
             LIMIT ?1",
            params![limit],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut roots = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let project_id = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let canonical_root = row
            .get::<String>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let display_root = row
            .get::<String>(2)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let platform = row
            .get::<Option<String>>(3)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let bytes = row
            .get::<Option<Vec<u8>>>(4)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let primary_root_last_seen_at = row
            .get::<Option<i64>>(5)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let last_seen_at = row
            .get::<i64>(6)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        roots.push((
            project_id,
            canonical_root,
            display_root,
            platform,
            bytes,
            primary_root_last_seen_at,
            last_seen_at,
        ));
    }
    drop(rows);

    let mut paths = Vec::with_capacity(roots.len());
    for (
        project_id,
        canonical_root,
        display_root,
        platform,
        bytes,
        primary_root_last_seen_at,
        last_seen_at,
    ) in roots
    {
        let path = match (platform, bytes, primary_root_last_seen_at) {
            (Some(platform), Some(bytes), Some(primary_last_seen)) => {
                let path = decode_native_project_path(&platform, bytes).map_err(|error| {
                    global_db_operation_message(
                        OPERATION,
                        format!("invalid primary root for project '{project_id}': {error}"),
                    )
                })?;
                let display_evidence = path.to_string_lossy();
                if primary_last_seen != last_seen_at
                    || (display_evidence != canonical_root && display_evidence != display_root)
                    || !project_alias_is_current(db, &project_id, &path, last_seen_at).await?
                {
                    return Err(global_db_operation_message(
                        OPERATION,
                        format!("project '{project_id}' has a stale primary root"),
                    ));
                }
                path
            }
            (None, None, None) => {
                legacy_code_project_path(
                    db,
                    &project_id,
                    &canonical_root,
                    &display_root,
                    last_seen_at,
                )
                .await?
            }
            _ => {
                return Err(global_db_operation_message(
                    OPERATION,
                    format!("project '{project_id}' has an incomplete primary root"),
                ));
            }
        };
        if !path.is_absolute() {
            return Err(global_db_operation_message(
                OPERATION,
                format!("project '{project_id}' has a non-absolute root"),
            ));
        }
        paths.push(path);
    }
    Ok(paths)
}

async fn project_alias_is_current(
    db: &GlobalDb,
    project_id: &str,
    path: &Path,
    last_seen_at: i64,
) -> crate::errors::Result<bool> {
    const OPERATION: &str = "list native code project paths";
    let alias = project_path_alias_key(path);
    let mut rows = db
        .conn
        .query(
            "SELECT 1 FROM project_aliases
             WHERE project_id = ?1 AND alias_path = ?2 AND last_seen_at = ?3",
            params![project_id, alias, last_seen_at],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn legacy_code_project_path(
    db: &GlobalDb,
    project_id: &str,
    canonical_root: &str,
    display_root: &str,
    last_seen_at: i64,
) -> crate::errors::Result<PathBuf> {
    const OPERATION: &str = "list native code project paths";
    let mut rows = db
        .conn
        .query(
            "SELECT alias_path, last_seen_at FROM project_aliases
             WHERE project_id = ?1 ORDER BY alias_path",
            params![project_id],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let mut candidates = BTreeMap::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
    {
        let alias = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let alias_last_seen = row
            .get::<i64>(1)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if alias_last_seen != last_seen_at {
            continue;
        }
        let path = match decode_native_project_path_alias(&alias) {
            Ok(Some(path)) => path,
            Ok(None) if Path::new(&alias).is_absolute() => PathBuf::from(&alias),
            Ok(None) | Err(_) => continue,
        };
        let display_evidence = path.to_string_lossy();
        if display_evidence != canonical_root && display_evidence != display_root {
            continue;
        }
        let identity = format!(
            "{}:{}",
            native_project_path_platform(),
            hex::encode(encode_native_project_path(&path))
        );
        candidates.insert(identity, path);
    }
    drop(rows);

    let mut candidates = candidates.into_values();
    let Some(path) = candidates.next() else {
        return Err(global_db_operation_message(
            OPERATION,
            format!("project '{project_id}' has no current lossless legacy root evidence"),
        ));
    };
    if candidates.next().is_some() {
        return Err(global_db_operation_message(
            OPERATION,
            format!("project '{project_id}' has ambiguous legacy current roots"),
        ));
    }
    let _writer = db.transaction.lock().await;
    let updated = db
        .conn
        .execute(
            "UPDATE code_projects
             SET primary_root_platform = ?1, primary_root_bytes = ?2,
                 primary_root_last_seen_at = ?3
             WHERE project_id = ?4 AND last_seen_at = ?3
               AND primary_root_platform IS NULL AND primary_root_bytes IS NULL
               AND primary_root_last_seen_at IS NULL",
            params![
                native_project_path_platform(),
                encode_native_project_path(&path),
                last_seen_at,
                project_id
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if updated != 1 {
        return Err(global_db_operation_message(
            OPERATION,
            format!("project '{project_id}' changed while resolving its legacy root"),
        ));
    }
    Ok(path)
}

pub(super) async fn list_lossless_paths(
    db: &GlobalDb,
    sql: &str,
    operation: &'static str,
) -> crate::errors::Result<Vec<PathBuf>> {
    let mut rows = db
        .conn
        .query(sql, ())
        .await
        .map_err(|error| global_db_operation_error(operation, error))?;
    let mut paths = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(operation, error))?
    {
        let encoded = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(operation, error))?;
        let path = match decode_native_project_path_alias(&encoded) {
            Ok(Some(path)) => Some(path),
            Ok(None) if Path::new(&encoded).is_absolute() => Some(PathBuf::from(encoded)),
            Ok(None) => None,
            Err(error) => {
                return Err(global_db_operation_message(
                    operation,
                    format!("invalid native project path alias: {error}"),
                ));
            }
        };
        if let Some(path) = path {
            if !path.is_absolute() {
                return Err(global_db_operation_message(
                    operation,
                    "native project path alias is not absolute",
                ));
            }
            paths.push(path);
        }
    }
    Ok(paths)
}

pub(super) async fn list_project_paths_compat(db: &GlobalDb) -> Vec<String> {
    let Ok(mut rows) = db.conn.query("SELECT path FROM projects", ()).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            paths.push(path);
        }
    }
    paths
}

pub(super) async fn list_project_alias_paths_compat(db: &GlobalDb) -> Vec<String> {
    let Ok(mut rows) = db
        .conn
        .query(
            "SELECT alias_path FROM project_aliases ORDER BY alias_path",
            (),
        )
        .await
    else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0)
            && Path::new(&path).is_absolute()
        {
            paths.push(path);
        }
    }
    paths
}
