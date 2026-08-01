use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Rows, Value, params,
};

use super::{
    CodeProjectRecord, GraphScopeRecord, GraphScopeUpsert, ProjectAliasRecord,
    ProjectRegistryContext, ProjectStoreResolution, RegisteredGlobalDb,
    RegisteredGlobalDbWriteTransaction, StoreArtifactRecord, StoreArtifactUpsert,
    StoreInstanceRecord, StoreInstanceUpsert, global_db_operation_error,
    global_db_operation_message,
};

// ---------------------------------------------------------------------------
// Registry reap contract
// ---------------------------------------------------------------------------
//
// `plan_registry_reap` below is the only producer of these values, so the
// contract lives beside its producer rather than in the composition root that
// merely prints it. Moving it down is what keeps this crate from needing an
// upward `crate::project_registry::…` edge; the root re-exports these names
// through its `tracedecay_global_db::*` shim.

/// Prefix marking a `project_aliases` row that keys a repository's git common
/// directory rather than a checkout path.
pub const GIT_COMMON_DIR_ALIAS_PREFIX: &str = "git-common-dir:";

/// Which registry table a reap candidate belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReapEntryKind {
    /// A `projects` row: the cross-project savings ledger, which despite the
    /// table name is an accounting record and not a project registry.
    SavingsLedgerPath,
    /// A `project_aliases` row keyed by a filesystem path.
    ProjectAlias,
    /// A `code_projects` row: the V2 canonical identity authority.
    CodeProject,
}

impl ReapEntryKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SavingsLedgerPath => "savings-ledger path",
            Self::ProjectAlias => "project alias",
            Self::CodeProject => "project authority",
        }
    }
}

/// One registry row whose referenced path is gone from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryReapEntry {
    pub kind: ReapEntryKind,
    /// Primary key of the row: ledger path, alias key, or project id.
    pub key: String,
    /// The filesystem path that no longer exists.
    pub missing_path: String,
    pub project_id: Option<String>,
}

/// A dead-looking row that reaping deliberately leaves alone, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedRegistryEntry {
    pub entry: RegistryReapEntry,
    pub reason: String,
}

/// The outcome of classifying the registry: what may be removed and what is
/// deliberately kept. Reaping only ever deletes rows; no store directory,
/// database, or session artifact is touched by any part of this plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryReapPlan {
    pub reapable: Vec<RegistryReapEntry>,
    pub retained: Vec<RetainedRegistryEntry>,
}

impl RegistryReapPlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reapable.is_empty()
    }

    /// One line per row, for a dry-run report.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} reapable, {} retained\n",
            self.reapable.len(),
            self.retained.len()
        );
        for entry in &self.reapable {
            let _ = writeln!(
                out,
                "  reap    {} {} (missing {})",
                entry.kind.label(),
                entry.key,
                entry.missing_path
            );
        }
        for retained in &self.retained {
            let _ = writeln!(
                out,
                "  retain  {} {} — {}",
                retained.entry.kind.label(),
                retained.entry.key,
                retained.reason
            );
        }
        out
    }
}

/// The path a `project_aliases` key refers to, or `None` when the key is not
/// path-shaped (a `git-remote-name:` search alias, say) and therefore can
/// never be judged dead by checking the filesystem.
#[must_use]
pub fn alias_key_path(alias: &str) -> Option<&Path> {
    let candidate = alias
        .strip_prefix(GIT_COMMON_DIR_ALIAS_PREFIX)
        .unwrap_or(alias);
    let path = Path::new(candidate);
    path.is_absolute().then_some(path)
}

/// Whether `path` lives under the OS temporary directory.
///
/// Canonicalizes both sides where possible so a `/tmp` symlinked to
/// `/private/tmp` (macOS) still matches.
#[must_use]
pub fn is_ephemeral_path(path: &Path) -> bool {
    let temp_root = std::env::temp_dir();
    let temp_root = temp_root.canonicalize().unwrap_or(temp_root);
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(&temp_root)
}

/// Registry admission policy for a project root, returning the refusal reason
/// when the root must not become a durable project authority.
///
/// A checkout under the OS temporary directory is throwaway by construction —
/// `mktemp -d` fixtures, extracted archives, scratch clones — yet registering
/// one writes a `code_projects` row and a shard that outlive it by years.
/// The comparison is against the *profile*, not absolute: a hermetic profile
/// that itself lives under the temp directory is equally throwaway, so test
/// fixtures and sandboxed runs keep working. Only a durable profile refuses an
/// ephemeral root.
#[must_use]
pub fn ephemeral_root_rejection(project_root: &Path, profile_root: &Path) -> Option<String> {
    (is_ephemeral_path(project_root) && !is_ephemeral_path(profile_root)).then(|| {
        format!(
            "project root '{}' is under the OS temporary directory and cannot be \
             registered as a durable authority in profile '{}'",
            project_root.display(),
            profile_root.display()
        )
    })
}

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
    tracedecay_runtime_core::lifecycle_lease::canonical_or_original(project_path)
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

pub(super) fn encode_native_project_path(path: &Path) -> Vec<u8> {
    tracedecay_runtime_core::os_str_bytes::native_os_str_bytes(path.as_os_str())
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
#[allow(
    clippy::needless_pass_by_value,
    reason = "all platform implementations share the owned database-value contract"
)]
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
    if !bytes.len().is_multiple_of(2) {
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

/// Row batch size for the canonical-key migration's upsert/delete statements.
/// Each upserted row binds 2 params and each deleted row binds 1, so this
/// stays well under SQLite's default `SQLITE_LIMIT_VARIABLE_NUMBER` (999).
const CANONICAL_KEY_MIGRATION_CHUNK: usize = 400;

pub(super) async fn migrate_project_rows_to_canonical_keys(
    conn: &impl Executor,
) -> tracedecay_runtime_core::db::engine::Result<()> {
    let mut rows = conn
        .query("SELECT path, tokens_saved FROM projects", ())
        .await?;
    let mut replacements = Vec::new();
    while let Some(row) = rows.next().await? {
        let old_path = row.get::<String>(0)?;
        let tokens_saved = row.get::<i64>(1)?;
        let canonical_path = canonical_project_path(Path::new(&old_path))
            .to_string_lossy()
            .into_owned();
        if old_path != canonical_path {
            replacements.push((old_path, canonical_path, tokens_saved));
        }
    }
    drop(rows);
    if replacements.is_empty() {
        return Ok(());
    }

    // Multiple drifted paths can canonicalize to the same target (e.g. two
    // differently-cased aliases of one project), and that target may already
    // have its own row. The old per-row loop merged these one at a time via
    // `INSERT ... ON CONFLICT DO UPDATE SET tokens_saved = MAX(...)`, so each
    // upsert's MAX ran against whatever the target held after the previous
    // one. MAX is associative and commutative, so pre-merging every drifted
    // row's `tokens_saved` per canonical target here (before the batched
    // upsert) reaches the identical final value in one pass.
    let mut merged_by_canonical: BTreeMap<String, i64> = BTreeMap::new();
    let mut old_paths = Vec::with_capacity(replacements.len());
    for (old_path, canonical_path, tokens_saved) in replacements {
        merged_by_canonical
            .entry(canonical_path)
            .and_modify(|existing| *existing = (*existing).max(tokens_saved))
            .or_insert(tokens_saved);
        old_paths.push(old_path);
    }
    let merged = merged_by_canonical.into_iter().collect::<Vec<_>>();

    for chunk in merged.chunks(CANONICAL_KEY_MIGRATION_CHUNK) {
        let placeholders = vec!["(?, ?)"; chunk.len()].join(",");
        let sql = format!(
            "INSERT INTO projects (path, tokens_saved) VALUES {placeholders}
             ON CONFLICT(path) DO UPDATE SET
                tokens_saved = MAX(tokens_saved, excluded.tokens_saved)"
        );
        let mut values = Vec::with_capacity(chunk.len() * 2);
        for (canonical_path, tokens_saved) in chunk {
            values.push(Value::Text(canonical_path.clone()));
            values.push(Value::Integer(*tokens_saved));
        }
        conn.execute(&sql, values).await?;
    }

    for chunk in old_paths.chunks(CANONICAL_KEY_MIGRATION_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!("DELETE FROM projects WHERE path IN ({placeholders})");
        let values = chunk
            .iter()
            .map(|old_path| Value::Text(old_path.clone()))
            .collect::<Vec<_>>();
        conn.execute(&sql, values).await?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ProjectRegistryDatabase<'db>(&'db RegisteredGlobalDb);

struct ProjectRegistryReadSnapshot(tracedecay_runtime_core::db::engine::ReadSnapshot);

struct ProjectRegistryWriteTransaction<'db>(RegisteredGlobalDbWriteTransaction<'db>);

impl<'db> ProjectRegistryDatabase<'db> {
    async fn read_snapshot(
        self,
        operation: &'static str,
    ) -> tracedecay_runtime_core::errors::Result<ProjectRegistryReadSnapshot> {
        self.0
            .read_snapshot()
            .await
            .map(ProjectRegistryReadSnapshot)
            .map_err(|error| global_db_operation_error(operation, error))
    }

    async fn begin_write_transaction(
        self,
        operation: &'static str,
    ) -> tracedecay_runtime_core::errors::Result<ProjectRegistryWriteTransaction<'db>> {
        self.0
            .begin_write_transaction()
            .await
            .map(ProjectRegistryWriteTransaction)
            .map_err(|error| global_db_operation_error(operation, error))
    }
}

impl QueryExecutor for ProjectRegistryReadSnapshot {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        QueryExecutor::query(&self.0, sql, params).await
    }
}

impl QueryExecutor for ProjectRegistryWriteTransaction<'_> {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        QueryExecutor::query(&self.0, sql, params).await
    }
}

impl Executor for ProjectRegistryWriteTransaction<'_> {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        Executor::execute(&self.0, sql, params).await
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        Executor::execute_batch(&self.0, sql).await
    }
}

impl ProjectRegistryWriteTransaction<'_> {
    async fn commit(self, operation: &'static str) -> tracedecay_runtime_core::errors::Result<()> {
        self.0
            .commit()
            .await
            .map_err(|error| global_db_operation_error(operation, error))
    }

    async fn rollback(
        self,
        operation: &'static str,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        self.0
            .rollback()
            .await
            .map_err(|error| global_db_operation_error(operation, error))
    }
}

pub(super) async fn list_registered_code_project_paths(
    db: &RegisteredGlobalDb,
    limit: usize,
) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
    list_code_project_paths_from(ProjectRegistryDatabase(db), limit).await
}

async fn list_code_project_paths_from(
    db: ProjectRegistryDatabase<'_>,
    limit: usize,
) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
    const OPERATION: &str = "list native code project paths";

    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let read = db.read_snapshot(OPERATION).await?;
    let mut rows = read
        .query(
            "SELECT project_id, canonical_root, display_root, primary_root_platform,
                    primary_root_bytes, primary_root_last_seen_at, last_seen_at
             FROM code_projects
             ORDER BY last_seen_at DESC, project_id
             LIMIT ?1",
            tracedecay_runtime_core::db::engine::params![limit],
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
        // One project's stale or malformed evidence must not make every
        // registered root unlistable — a listing consumer (transcript sweeps,
        // storage inventory) degrades to skipping that project, while doctor
        // still surfaces the row through its own registry checks. Genuine
        // storage failures (query/transaction errors) are a different class
        // of problem and must not be downgraded to a per-row skip: they
        // propagate as `Err` and abort the whole listing, matching the
        // existing contract of `project_alias_is_current` below.
        let skip = |detail: &str| {
            tracing::warn!(
                project_id = project_id.as_str(),
                %detail,
                "skipping unlistable code project root"
            );
        };
        let path = match (platform, bytes, primary_root_last_seen_at) {
            (Some(platform), Some(bytes), Some(primary_last_seen)) => {
                let path = match decode_native_project_path(&platform, bytes) {
                    Ok(path) => path,
                    Err(error) => {
                        skip(&format!("invalid primary root: {error}"));
                        continue;
                    }
                };
                let display_evidence = path.to_string_lossy();
                if primary_last_seen != last_seen_at
                    || (display_evidence != canonical_root && display_evidence != display_root)
                    || !project_alias_is_current(&read, &project_id, &path, last_seen_at).await?
                {
                    skip("stale primary root");
                    continue;
                }
                path
            }
            (None, None, None) => {
                match legacy_code_project_path(
                    db,
                    &read,
                    &project_id,
                    &canonical_root,
                    &display_root,
                    last_seen_at,
                )
                .await?
                {
                    PathEvidenceVerdict::Accepted(path) => path,
                    PathEvidenceVerdict::Rejected(detail) => {
                        skip(&format!("legacy root evidence rejected: {detail}"));
                        continue;
                    }
                }
            }
            _ => {
                skip("incomplete primary root");
                continue;
            }
        };
        if !path.is_absolute() {
            skip("non-absolute root");
            continue;
        }
        paths.push(path);
    }
    Ok(paths)
}

async fn project_alias_is_current(
    read: &impl QueryExecutor,
    project_id: &str,
    path: &Path,
    last_seen_at: i64,
) -> tracedecay_runtime_core::errors::Result<bool> {
    const OPERATION: &str = "list native code project paths";
    let alias = project_path_alias_key(path);
    let mut rows = read
        .query(
            "SELECT 1 FROM project_aliases
             WHERE project_id = ?1 AND alias_path = ?2 AND last_seen_at = ?3",
            tracedecay_runtime_core::db::engine::params![project_id, alias, last_seen_at],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

/// Outcome of resolving a project's legacy (pre-primary-root) path evidence.
///
/// This only represents the row-local "this project's evidence doesn't
/// support a path" case, which the caller skips and continues past. Genuine
/// storage failures (query/transaction errors) are never wrapped here — they
/// propagate as `Err`, so a real DB failure aborts the whole listing instead
/// of silently degrading it, consistent with `project_alias_is_current`.
enum PathEvidenceVerdict {
    Accepted(PathBuf),
    Rejected(String),
}

async fn legacy_code_project_path(
    db: ProjectRegistryDatabase<'_>,
    read: &impl QueryExecutor,
    project_id: &str,
    canonical_root: &str,
    display_root: &str,
    last_seen_at: i64,
) -> tracedecay_runtime_core::errors::Result<PathEvidenceVerdict> {
    const OPERATION: &str = "list native code project paths";
    let mut rows = read
        .query(
            "SELECT alias_path, last_seen_at FROM project_aliases
             WHERE project_id = ?1 ORDER BY alias_path",
            tracedecay_runtime_core::db::engine::params![project_id],
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
        return Ok(PathEvidenceVerdict::Rejected(format!(
            "project '{project_id}' has no current lossless legacy root evidence"
        )));
    };
    if candidates.next().is_some() {
        return Ok(PathEvidenceVerdict::Rejected(format!(
            "project '{project_id}' has ambiguous legacy current roots"
        )));
    }
    let transaction = db.begin_write_transaction(OPERATION).await?;
    let updated = transaction
        .execute(
            "UPDATE code_projects
             SET primary_root_platform = ?1, primary_root_bytes = ?2,
                 primary_root_last_seen_at = ?3
             WHERE project_id = ?4 AND last_seen_at = ?3
               AND primary_root_platform IS NULL AND primary_root_bytes IS NULL
               AND primary_root_last_seen_at IS NULL",
            tracedecay_runtime_core::db::engine::params![
                native_project_path_platform(),
                encode_native_project_path(&path),
                last_seen_at,
                project_id
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    if updated != 1 {
        // The row changed concurrently (raced with another writer); treat as
        // row-local evidence rejection rather than a hard storage failure —
        // the write itself succeeded, it just no longer applies.
        transaction
            .rollback("rollback raced native code project path resolution")
            .await?;
        return Ok(PathEvidenceVerdict::Rejected(format!(
            "project '{project_id}' changed while resolving its legacy root"
        )));
    }
    transaction
        .commit("commit native code project path resolution")
        .await?;
    Ok(PathEvidenceVerdict::Accepted(path))
}

pub(super) async fn list_registered_lossless_paths(
    db: &RegisteredGlobalDb,
    sql: &str,
    operation: &'static str,
) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
    list_lossless_paths_from(ProjectRegistryDatabase(db), sql, operation).await
}

async fn list_lossless_paths_from(
    db: ProjectRegistryDatabase<'_>,
    sql: &str,
    operation: &'static str,
) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
    let read = db.read_snapshot(operation).await?;
    let mut rows = read
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

pub(super) async fn list_registered_project_paths_compat(db: &RegisteredGlobalDb) -> Vec<String> {
    list_project_paths_compat_from(ProjectRegistryDatabase(db)).await
}

async fn list_project_paths_compat_from(db: ProjectRegistryDatabase<'_>) -> Vec<String> {
    let Ok(read) = db.read_snapshot("list compatibility project paths").await else {
        return Vec::new();
    };
    let Ok(mut rows) = read.query("SELECT path FROM projects", ()).await else {
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

impl RegisteredGlobalDb {
    pub fn is_explicit_project_path_selector(selector: &str) -> bool {
        let selector = selector.trim();
        !selector.is_empty()
            && (Path::new(selector).is_absolute()
                || selector == "."
                || selector == ".."
                || selector.contains('/')
                || selector.contains('\\'))
    }

    /// Applies [`self::ephemeral_root_rejection`] against
    /// the profile this database belongs to (`<profile>/global.db`).
    fn ephemeral_root_rejection(&self, project_root: &Path) -> Option<String> {
        let profile_root = self.db_path().parent()?;
        self::ephemeral_root_rejection(project_root, profile_root)
    }

    pub async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Option<CodeProjectRecord> {
        // The one door through which a project authority is minted. Enforcing
        // admission here rather than at each caller means an ephemeral root
        // cannot become a durable authority even from a call site that has
        // never heard of the policy.
        if let Some(reason) = self.ephemeral_root_rejection(project_root) {
            eprintln!("warning: refusing to register a TraceDecay project — {reason}");
            return None;
        }
        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        let canonical_project_root = canonical_project_path(project_root);
        let canonical_root = canonical_project_root.to_string_lossy().into_owned();
        let current_root_alias = project_path_alias_key(&canonical_project_root);
        let git_common_dir_text = git_common_dir.map(|path| path.to_string_lossy().into_owned());
        let transaction = self.begin_write_transaction().await.ok()?;
        transaction
            .execute(
                "INSERT INTO code_projects
                 (project_id, canonical_root, display_root, primary_root_platform,
                  primary_root_bytes, primary_root_last_seen_at, git_common_dir,
                  git_remote_url, default_branch, created_at, last_seen_at)
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?5, ?5)
                 ON CONFLICT(project_id) DO UPDATE SET
                    canonical_root = excluded.canonical_root,
                    display_root = excluded.display_root,
                    primary_root_platform = excluded.primary_root_platform,
                    primary_root_bytes = excluded.primary_root_bytes,
                    primary_root_last_seen_at = excluded.primary_root_last_seen_at,
                    git_common_dir = excluded.git_common_dir,
                    git_remote_url = excluded.git_remote_url,
                    default_branch = excluded.default_branch,
                    last_seen_at = excluded.last_seen_at",
                params![
                    project_id,
                    canonical_root,
                    native_project_path_platform(),
                    encode_native_project_path(&canonical_project_root),
                    now,
                    git_common_dir_text,
                    git_remote_url,
                    default_branch
                ],
            )
            .await
            .ok()?;
        let mut aliases = vec![current_root_alias];
        aliases.extend(super::repo_identity_aliases(git_common_dir));
        if let Some(alias) = super::git_remote_search_alias(git_remote_url) {
            aliases.push(alias);
        }
        for alias in aliases {
            transaction
                .execute(
                    "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(alias_path) DO UPDATE SET
                        project_id = excluded.project_id,
                        last_seen_at = excluded.last_seen_at",
                    params![alias, project_id, now],
                )
                .await
                .ok()?;
        }
        transaction.commit().await.ok()?;
        self.get_code_project(project_id).await
    }

    pub async fn upsert_project_alias(
        &self,
        alias_path: &Path,
        project_id: &str,
    ) -> Option<ProjectAliasRecord> {
        self.upsert_project_alias_key(&project_path_alias_key(alias_path), project_id)
            .await
    }

    async fn upsert_project_alias_key(
        &self,
        alias: &str,
        project_id: &str,
    ) -> Option<ProjectAliasRecord> {
        let now = tracedecay_runtime_core::tracedecay::current_timestamp();
        let transaction = self.begin_write_transaction().await.ok()?;
        transaction
            .execute(
                "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(alias_path) DO UPDATE SET
                    project_id = excluded.project_id,
                    last_seen_at = excluded.last_seen_at",
                params![alias, project_id, now],
            )
            .await
            .ok()?;
        transaction.commit().await.ok()?;
        let snapshot = self.read_snapshot().await.ok()?;
        let mut rows = snapshot
            .query(
                "SELECT alias_path, project_id, last_seen_at
                 FROM project_aliases WHERE alias_path = ?1",
                params![alias],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(ProjectAliasRecord {
            alias_path: row.get(0).ok()?,
            project_id: row.get(1).ok()?,
            last_seen_at: row.get(2).ok()?,
        })
    }

    pub async fn upsert_store_instance(
        &self,
        upsert: StoreInstanceUpsert,
    ) -> Option<StoreInstanceRecord> {
        let transaction = self.begin_write_transaction().await.ok()?;
        transaction
            .execute(
                "INSERT INTO store_instances
                 (store_id, project_id, store_kind, storage_mode, store_relpath,
                  manifest_relpath, created_at, last_verified_at, last_write_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(store_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    store_kind = excluded.store_kind,
                    storage_mode = excluded.storage_mode,
                    store_relpath = excluded.store_relpath,
                    manifest_relpath = excluded.manifest_relpath,
                    last_verified_at = excluded.last_verified_at,
                    last_write_at = excluded.last_write_at",
                params![
                    upsert.store_id.as_str(),
                    upsert.project_id.as_str(),
                    upsert.store_kind.as_str(),
                    upsert.storage_mode.as_str(),
                    upsert.store_relpath.as_str(),
                    upsert.manifest_relpath.as_deref(),
                    tracedecay_runtime_core::tracedecay::current_timestamp(),
                    upsert.last_verified_at,
                    upsert.last_write_at
                ],
            )
            .await
            .ok()?;
        transaction.commit().await.ok()?;
        self.project_registry_context_by_id(&upsert.project_id)
            .await
            .ok()
            .flatten()?
            .stores
            .into_iter()
            .map(|context| context.store)
            .find(|store| store.store_id == upsert.store_id)
    }

    pub async fn upsert_graph_scope(&self, upsert: GraphScopeUpsert) -> Option<GraphScopeRecord> {
        let transaction = self.begin_write_transaction().await.ok()?;
        transaction
            .execute(
                "INSERT INTO graph_scopes
                 (graph_scope_id, project_id, store_id, branch_name, db_relpath,
                  parent_scope_id, last_synced_at, writable)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(graph_scope_id) DO UPDATE SET
                    project_id = excluded.project_id,
                    store_id = excluded.store_id,
                    branch_name = excluded.branch_name,
                    db_relpath = excluded.db_relpath,
                    parent_scope_id = excluded.parent_scope_id,
                    last_synced_at = excluded.last_synced_at,
                    writable = excluded.writable",
                params![
                    upsert.graph_scope_id.as_str(),
                    upsert.project_id.as_str(),
                    upsert.store_id.as_str(),
                    upsert.branch_name.as_str(),
                    upsert.db_relpath.as_str(),
                    upsert.parent_scope_id.as_deref(),
                    upsert.last_synced_at,
                    i64::from(upsert.writable)
                ],
            )
            .await
            .ok()?;
        transaction.commit().await.ok()?;
        self.project_registry_context_by_id(&upsert.project_id)
            .await
            .ok()
            .flatten()?
            .stores
            .into_iter()
            .flat_map(|context| context.graph_scopes)
            .find(|scope| scope.graph_scope_id == upsert.graph_scope_id)
    }

    pub async fn upsert_store_artifact(
        &self,
        upsert: StoreArtifactUpsert,
    ) -> Option<StoreArtifactRecord> {
        let transaction = self.begin_write_transaction().await.ok()?;
        transaction
            .execute(
                "INSERT INTO store_artifacts
                 (store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(store_id, artifact_kind, relpath) DO UPDATE SET
                    size_bytes = excluded.size_bytes,
                    schema_version = excluded.schema_version,
                    updated_at = excluded.updated_at",
                params![
                    upsert.store_id.as_str(),
                    upsert.artifact_kind.as_str(),
                    upsert.relpath.as_str(),
                    upsert.size_bytes,
                    upsert.schema_version.as_deref(),
                    upsert.updated_at
                ],
            )
            .await
            .ok()?;
        transaction.commit().await.ok()?;
        let snapshot = self.read_snapshot().await.ok()?;
        let mut rows = snapshot
            .query(
                "SELECT store_id, artifact_kind, relpath, size_bytes, schema_version, updated_at
                 FROM store_artifacts
                 WHERE store_id = ?1 AND artifact_kind = ?2 AND relpath = ?3",
                params![
                    upsert.store_id.as_str(),
                    upsert.artifact_kind.as_str(),
                    upsert.relpath.as_str()
                ],
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        Some(StoreArtifactRecord {
            store_id: row.get(0).ok()?,
            artifact_kind: row.get(1).ok()?,
            relpath: row.get(2).ok()?,
            size_bytes: row.get(3).ok()?,
            schema_version: row.get(4).ok()?,
            updated_at: row.get(5).ok()?,
        })
    }

    pub async fn get_code_project(&self, project_id: &str) -> Option<CodeProjectRecord> {
        self.project_registry_context_by_id(project_id)
            .await
            .ok()
            .flatten()
            .map(|context| context.project)
    }

    pub async fn resolve_project_store_by_alias(
        &self,
        alias_path: &Path,
    ) -> Option<ProjectStoreResolution> {
        let project_id = self
            .project_id_by_native_path_alias(alias_path, LegacyPathAliasKind::ProjectRoot)
            .await
            .ok()
            .flatten()?;
        self.resolve_project_store_for_project_id(&project_id).await
    }

    pub async fn resolve_unique_project_store_by_git_remote(
        &self,
        git_remote_url: &str,
    ) -> Option<ProjectStoreResolution> {
        let remote = super::normalize_git_remote_url(git_remote_url)?;
        let projects = self.list_code_projects(usize::MAX).await.ok()?;
        let mut matches = projects.into_iter().filter(|project| {
            project
                .git_remote_url
                .as_deref()
                .and_then(super::normalize_git_remote_url)
                .is_some_and(|stored| stored == remote)
        });
        let project = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        self.resolve_project_store_for_project_id(&project.project_id)
            .await
    }

    async fn resolve_project_store_for_project_id(
        &self,
        project_id: &str,
    ) -> Option<ProjectStoreResolution> {
        let context = self
            .project_registry_context_by_id(project_id)
            .await
            .ok()
            .flatten()?;
        if context.stores.len() != 1 {
            return None;
        }
        let store = context.stores.into_iter().next()?;
        Some(ProjectStoreResolution {
            project: context.project,
            store: store.store,
            graph_scopes: store.graph_scopes,
            artifacts: store.artifacts,
        })
    }

    pub async fn search_code_projects(&self, query: &str, limit: usize) -> Vec<CodeProjectRecord> {
        match self.try_search_code_projects(query, limit).await {
            Ok(projects) => projects,
            Err(error) => {
                tracing::warn!(%error, "optional project search failed");
                Vec::new()
            }
        }
    }

    pub async fn try_search_code_projects(
        &self,
        query: &str,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<Vec<CodeProjectRecord>> {
        const OPERATION: &str = "search registered code projects";
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut patterns = query
            .split_whitespace()
            .map(super::like_pattern)
            .collect::<Vec<_>>();
        if patterns.is_empty() {
            patterns.push(super::like_pattern(query));
        }
        let clauses = (1..=patterns.len())
            .map(|index| {
                format!(
                    "(cp.project_id LIKE ?{index} ESCAPE '\\'
                        OR cp.canonical_root LIKE ?{index} ESCAPE '\\'
                        OR cp.display_root LIKE ?{index} ESCAPE '\\'
                        OR COALESCE(cp.git_common_dir, '') LIKE ?{index} ESCAPE '\\'
                        OR COALESCE(cp.default_branch, '') LIKE ?{index} ESCAPE '\\'
                        OR COALESCE(pa.alias_path, '') LIKE ?{index} ESCAPE '\\')"
                )
            })
            .collect::<Vec<_>>();
        let limit_param = patterns.len() + 1;
        let sql = format!(
            "SELECT DISTINCT cp.project_id, cp.canonical_root, cp.display_root,
                    cp.git_common_dir, cp.git_remote_url, cp.default_branch,
                    cp.created_at, cp.last_seen_at
             FROM code_projects cp
             LEFT JOIN project_aliases pa ON pa.project_id = cp.project_id
             WHERE {}
             ORDER BY cp.last_seen_at DESC, cp.project_id
             LIMIT ?{limit_param}",
            clauses.join(" OR ")
        );
        let mut values = patterns.into_iter().map(Value::Text).collect::<Vec<_>>();
        values.push(Value::Integer(limit));
        let snapshot = self.read_snapshot().await?;
        let mut rows = snapshot
            .query(&sql, values)
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut projects = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let project = CodeProjectRecord {
                project_id: row
                    .get(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                canonical_root: row
                    .get(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                display_root: row
                    .get(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                git_common_dir: row
                    .get(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                git_remote_url: row
                    .get(4)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                default_branch: row
                    .get(5)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                created_at: row
                    .get(6)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                last_seen_at: row
                    .get(7)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            };
            projects.push(project);
        }
        Ok(projects)
    }

    /// Resolves the sole store for a project-path alias without hiding
    /// ambiguity, query, or row-decoding failures.
    pub async fn try_resolve_project_store_record_by_alias(
        &self,
        alias_path: &Path,
    ) -> tracedecay_runtime_core::errors::Result<Option<StoreInstanceRecord>> {
        async fn query(
            db: &RegisteredGlobalDb,
            alias: &str,
            canonical_root: Option<&str>,
        ) -> tracedecay_runtime_core::errors::Result<Option<StoreInstanceRecord>> {
            const OPERATION: &str = "resolve project store by alias";

            let mut sql = String::from(
                "SELECT si.store_id, si.project_id, si.store_kind, si.storage_mode,
                        si.store_relpath, si.manifest_relpath, si.created_at,
                        si.last_verified_at, si.last_write_at
                 FROM project_aliases pa
                 JOIN code_projects cp ON cp.project_id = pa.project_id
                 JOIN store_instances si ON si.project_id = cp.project_id
                 WHERE pa.alias_path = ?1",
            );
            let mut values = vec![Value::Text(alias.to_string())];
            if let Some(canonical_root) = canonical_root {
                sql.push_str(
                    " AND cp.canonical_root = ?2
                      AND NOT EXISTS (
                          SELECT 1 FROM code_projects other
                          WHERE other.canonical_root = ?2
                            AND other.project_id != cp.project_id
                      )",
                );
                values.push(Value::Text(canonical_root.to_string()));
            }
            sql.push_str(" ORDER BY si.store_id");
            let snapshot = db
                .read_snapshot()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let mut rows = snapshot
                .query(&sql, values)
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
            else {
                return Ok(None);
            };
            let store = StoreInstanceRecord {
                store_id: row
                    .get(0)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                project_id: row
                    .get(1)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                store_kind: row
                    .get(2)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                storage_mode: row
                    .get(3)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                store_relpath: row
                    .get(4)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                manifest_relpath: row
                    .get(5)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                created_at: row
                    .get(6)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                last_verified_at: row
                    .get(7)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
                last_write_at: row
                    .get(8)
                    .map_err(|error| global_db_operation_error(OPERATION, error))?,
            };
            if rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                .is_some()
            {
                return Err(global_db_operation_message(
                    OPERATION,
                    "project identity resolves to multiple stores",
                ));
            }
            Ok(Some(store))
        }

        let native_alias = project_path_alias_key(alias_path);
        if let Some(store) = query(self, &native_alias, None).await? {
            return Ok(Some(store));
        }
        let legacy_alias = canonical_project_path(alias_path)
            .to_string_lossy()
            .into_owned();
        if native_alias == legacy_alias {
            return Ok(None);
        }
        query(self, &legacy_alias, Some(&legacy_alias)).await
    }

    /// Resolves the persisted store for a project by repository identity.
    ///
    /// Repository marker conflicts and registry failures propagate so callers
    /// fail closed instead of minting a second project shard.
    pub async fn resolve_project_store_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> tracedecay_runtime_core::errors::Result<Option<ProjectStoreResolution>> {
        let Some(project_id) = self
            .project_id_by_identity(project_root, git_common_dir)
            .await?
        else {
            return Ok(None);
        };
        let Some(context) = self.project_registry_context_by_id(&project_id).await? else {
            return Ok(None);
        };
        if context.stores.len() > 1 {
            return Err(global_db_operation_message(
                "resolve project store by identity",
                format!(
                    "project '{}' resolves to multiple stores",
                    context.project.project_id
                ),
            ));
        }
        let Some(store) = context.stores.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(ProjectStoreResolution {
            project: context.project,
            store: store.store,
            graph_scopes: store.graph_scopes,
            artifacts: store.artifacts,
        }))
    }

    pub async fn project_registry_context_by_alias(
        &self,
        alias_path: &Path,
    ) -> tracedecay_runtime_core::errors::Result<Option<ProjectRegistryContext>> {
        let Some(project_id) = self
            .project_id_by_native_path_alias(alias_path, LegacyPathAliasKind::ProjectRoot)
            .await?
        else {
            return Ok(None);
        };
        self.project_registry_context_by_id(&project_id).await
    }

    pub async fn project_registry_context_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> tracedecay_runtime_core::errors::Result<Option<ProjectRegistryContext>> {
        let Some(project_id) = self
            .project_id_by_identity(project_root, git_common_dir)
            .await?
        else {
            return Ok(None);
        };
        self.project_registry_context_by_id(&project_id).await
    }

    async fn project_id_by_identity(
        &self,
        project_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> tracedecay_runtime_core::errors::Result<Option<String>> {
        Ok(
            match tracedecay_runtime_core::storage::read_repository_identity_marker(project_root)? {
                Some(marker) => Some(marker.project_id),
                None => {
                    if let Some(project_id) = self
                        .project_id_by_native_path_alias(
                            project_root,
                            LegacyPathAliasKind::ProjectRoot,
                        )
                        .await?
                    {
                        Some(project_id)
                    } else if let Some(git_common_dir) = git_common_dir {
                        self.project_id_by_native_path_alias(
                            git_common_dir,
                            LegacyPathAliasKind::GitCommonDir,
                        )
                        .await?
                    } else {
                        None
                    }
                }
            },
        )
    }

    pub(super) async fn project_id_by_native_path_alias(
        &self,
        path: &Path,
        kind: LegacyPathAliasKind,
    ) -> tracedecay_runtime_core::errors::Result<Option<String>> {
        const OPERATION: &str = "resolve project identity alias";

        async fn project_id_by_alias_key(
            db: &RegisteredGlobalDb,
            alias: &str,
        ) -> tracedecay_runtime_core::errors::Result<Option<String>> {
            const OPERATION: &str = "resolve project identity alias";
            let snapshot = db
                .read_snapshot()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let mut rows = snapshot
                .query(
                    "SELECT project_id FROM project_aliases WHERE alias_path = ?1",
                    params![alias],
                )
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
            else {
                return Ok(None);
            };
            row.get(0)
                .map(Some)
                .map_err(|error| global_db_operation_error(OPERATION, error))
        }

        let native_path = project_path_alias_key(path);
        let native_alias = format!("{}{native_path}", kind.prefix());
        if let Some(project_id) = project_id_by_alias_key(self, &native_alias).await? {
            return Ok(Some(project_id));
        }

        let legacy_path = canonical_project_path(path).to_string_lossy().into_owned();
        if native_path == legacy_path {
            return Ok(None);
        }
        let legacy_alias = format!("{}{legacy_path}", kind.prefix());
        let Some(legacy_project_id) = project_id_by_alias_key(self, &legacy_alias).await? else {
            return Ok(None);
        };
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut rows = snapshot
            .query(kind.owner_query(), params![legacy_path.as_str()])
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        else {
            return Ok(None);
        };
        let owner = row
            .get::<String>(0)
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        if owner != legacy_project_id
            || rows
                .next()
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?
                .is_some()
        {
            return Ok(None);
        }
        drop(rows);
        drop(snapshot);

        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        transaction
            .execute(
                "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(alias_path) DO NOTHING",
                params![
                    native_alias.as_str(),
                    legacy_project_id.as_str(),
                    tracedecay_runtime_core::tracedecay::current_timestamp()
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        transaction
            .commit()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let migrated_project_id = project_id_by_alias_key(self, &native_alias).await?;
        Ok(
            (migrated_project_id.as_deref() == Some(legacy_project_id.as_str()))
                .then_some(legacy_project_id),
        )
    }

    pub async fn delete_code_projects(&self, project_ids: &[String]) -> usize {
        const CHUNK: usize = 256;
        if project_ids.is_empty() {
            return 0;
        }
        let Ok(transaction) = self.begin_write_transaction().await else {
            return 0;
        };
        let mut total = 0_usize;
        for chunk in project_ids.chunks(CHUNK) {
            let sql = format!(
                "DELETE FROM code_projects WHERE project_id IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let values = chunk.iter().cloned().map(Value::Text).collect::<Vec<_>>();
            if let Ok(deleted) = transaction.execute(&sql, values).await {
                total = total.saturating_add(deleted as usize);
            }
        }
        if transaction.commit().await.is_ok() {
            total
        } else {
            0
        }
    }

    pub async fn delete_project(&self, project_path: &Path) {
        let Ok(transaction) = self.begin_write_transaction().await else {
            return;
        };
        if transaction
            .execute(
                "DELETE FROM projects WHERE path = ?1",
                params![project_path_alias_key(project_path)],
            )
            .await
            .is_ok()
        {
            let _ = transaction.commit().await;
        }
    }

    pub async fn delete_projects(&self, project_paths: &[String]) -> usize {
        self.delete_project_paths(project_paths).await
    }

    pub async fn delete_project_paths<P: AsRef<Path>>(&self, project_paths: &[P]) -> usize {
        const CHUNK: usize = 256;
        if project_paths.is_empty() {
            return 0;
        }
        let Ok(transaction) = self.begin_write_transaction().await else {
            return 0;
        };
        let mut total = 0_usize;
        for chunk in project_paths.chunks(CHUNK) {
            let sql = format!(
                "DELETE FROM projects WHERE path IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let values = chunk
                .iter()
                .map(|path| Value::Text(project_path_alias_key(path.as_ref())))
                .collect::<Vec<_>>();
            if let Ok(deleted) = transaction.execute(&sql, values).await {
                total = total.saturating_add(deleted as usize);
            }
        }
        if transaction.commit().await.is_ok() {
            total
        } else {
            0
        }
    }

    /// Lists registered code-project roots from one frozen runtime snapshot.
    pub async fn try_list_code_project_paths(
        &self,
        limit: usize,
    ) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
        list_registered_code_project_paths(self, limit).await
    }

    /// Returns legacy project paths with native path bytes preserved.
    pub async fn try_list_project_paths(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
        list_registered_lossless_paths(
            self,
            "SELECT path FROM projects ORDER BY path",
            "list lossless legacy project paths",
        )
        .await
    }

    /// Returns modern registry aliases with native path bytes preserved.
    pub async fn try_list_project_alias_paths(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<Vec<PathBuf>> {
        list_registered_lossless_paths(
            self,
            "SELECT alias_path FROM project_aliases ORDER BY alias_path",
            "list lossless project aliases",
        )
        .await
    }

    pub async fn list_project_paths_compat(&self) -> Vec<String> {
        list_registered_project_paths_compat(self).await
    }

    /// Classifies registry rows whose referenced path no longer exists.
    ///
    /// Dead references accumulate forever: every deleted worktree, renamed
    /// checkout, and throwaway clone leaves ledger, alias, and authority rows
    /// behind that nothing ever removes. This reports them without changing
    /// anything, so the result can be reviewed before [`apply_registry_reap`]
    /// removes the rows.
    ///
    /// A missing path is *not* evidence that data is disposable. An authority
    /// whose store directory still exists on disk is always retained with a
    /// reason: that store may hold facts, sessions, or branch graphs that
    /// exist nowhere else, and reclaiming it is a separate, verified
    /// operation. Nothing in planning or applying a reap deletes a file.
    ///
    /// [`apply_registry_reap`]: Self::apply_registry_reap
    pub async fn plan_registry_reap(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegistryReapPlan> {
        const OPERATION: &str = "plan registry reap";
        let profile_root = self
            .db_path()
            .parent()
            .ok_or_else(|| {
                global_db_operation_message(OPERATION, "global database has no profile directory")
            })?
            .to_path_buf();
        let snapshot = self
            .read_snapshot()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut plan = RegistryReapPlan::default();

        let mut rows = snapshot
            .query("SELECT path FROM projects ORDER BY path", ())
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let path: String = row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            if Path::new(&path).is_absolute() && !Path::new(&path).exists() {
                plan.reapable.push(RegistryReapEntry {
                    kind: ReapEntryKind::SavingsLedgerPath,
                    key: path.clone(),
                    missing_path: path,
                    project_id: None,
                });
            }
        }

        let mut live_roots_by_project: BTreeMap<String, usize> = BTreeMap::new();
        let mut dead_aliases = Vec::new();
        let mut rows = snapshot
            .query(
                "SELECT alias_path, project_id FROM project_aliases ORDER BY alias_path",
                (),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let alias: String = row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let project_id: String = row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            // A non-path alias (a remote-name search key) can never be judged
            // dead by consulting the filesystem, so it is never a candidate.
            let missing_path = match self::alias_key_path(&alias) {
                Some(path) if !path.exists() => path.to_string_lossy().into_owned(),
                // Either a non-path alias, which the filesystem can never
                // judge dead, or a path that is still there.
                _ => {
                    *live_roots_by_project.entry(project_id).or_default() += 1;
                    continue;
                }
            };
            dead_aliases.push(RegistryReapEntry {
                kind: ReapEntryKind::ProjectAlias,
                key: alias,
                missing_path,
                project_id: Some(project_id),
            });
        }
        plan.reapable.extend(dead_aliases);

        let mut rows = snapshot
            .query(
                "SELECT project_id, canonical_root, git_common_dir
                 FROM code_projects ORDER BY project_id",
                (),
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            let project_id: String = row
                .get(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let canonical_root: String = row
                .get(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let git_common_dir: Option<String> = row.get(2).ok();
            if Path::new(&canonical_root).exists()
                || git_common_dir
                    .as_deref()
                    .is_some_and(|dir| Path::new(dir).exists())
                || live_roots_by_project.contains_key(&project_id)
            {
                continue;
            }
            let entry = RegistryReapEntry {
                kind: ReapEntryKind::CodeProject,
                key: project_id.clone(),
                missing_path: canonical_root,
                project_id: Some(project_id.clone()),
            };
            let store_root = tracedecay_runtime_core::storage::profile_sharded_data_root(
                &profile_root,
                &project_id,
            );
            if store_root.exists() {
                plan.retained.push(RetainedRegistryEntry {
                    entry,
                    reason: format!(
                        "store directory '{}' still holds data; reclaiming it is a separate \
                         verified operation",
                        store_root.display()
                    ),
                });
            } else {
                plan.reapable.push(entry);
            }
        }

        Ok(plan)
    }

    /// Removes the rows in `plan.reapable`, and only those rows.
    ///
    /// Every path is re-checked here rather than trusted from planning time,
    /// so a checkout that reappeared between the two calls is skipped instead
    /// of unregistered. Returns the number of rows actually removed. No
    /// filesystem path is deleted, moved, or opened for writing.
    pub async fn apply_registry_reap(
        &self,
        plan: &RegistryReapPlan,
    ) -> tracedecay_runtime_core::errors::Result<usize> {
        const OPERATION: &str = "apply registry reap";
        let transaction = self
            .begin_write_transaction()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut removed = 0;
        for entry in &plan.reapable {
            if Path::new(&entry.missing_path).exists() {
                continue;
            }
            let statement = match entry.kind {
                ReapEntryKind::SavingsLedgerPath => "DELETE FROM projects WHERE path = ?1",
                ReapEntryKind::ProjectAlias => "DELETE FROM project_aliases WHERE alias_path = ?1",
                ReapEntryKind::CodeProject => "DELETE FROM code_projects WHERE project_id = ?1",
            };
            transaction
                .execute(statement, params![entry.key.as_str()])
                .await
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            removed += 1;
        }
        transaction
            .commit()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        Ok(removed)
    }
}
