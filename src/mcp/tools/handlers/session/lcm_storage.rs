use super::message_search::{SessionRetrievalServicePort, SessionRetrievalStoreScope};
use super::*;

#[derive(Clone, Copy)]
pub(in super::super) struct LcmHandlerContext<'a> {
    pub(super) project_root: Option<&'a Path>,
    project_session_db_path: Option<&'a Path>,
    retained_session_db: Option<&'a Arc<GlobalDb>>,
    pub(super) retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    pub(super) retrieval_store_scope: SessionRetrievalStoreScope,
}

impl<'a> LcmHandlerContext<'a> {
    pub(in super::super) fn active(
        cg: &'a TraceDecay,
        retained_session_db: Option<&'a Arc<GlobalDb>>,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: Some(cg.project_root()),
            project_session_db_path: Some(cg.store_layout().sessions_db_path.as_path()),
            retained_session_db,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Project,
        }
    }

    pub(in super::super) fn user(
        sessions_db_path: &'a Path,
        retained_session_db: Option<&'a Arc<GlobalDb>>,
        retrieval_service: Option<&'a dyn SessionRetrievalServicePort>,
    ) -> Self {
        Self {
            project_root: None,
            project_session_db_path: Some(sessions_db_path),
            retained_session_db,
            retrieval_service,
            retrieval_store_scope: SessionRetrievalStoreScope::Profile,
        }
    }
}

fn lcm_unavailable(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "message": "could not open active project tracedecay session database",
        }),
    )
}

/// Returned by pure-read tools when the sessions.db file has not been
/// created yet (nothing has been ingested). Distinct from "unavailable"
/// so callers can tell "no data yet" apart from "open failed".
/// The `store_exists: false` field is the machine-readable discriminator;
/// other fields are backward-compatible additions.
fn lcm_not_yet_ingested(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "not_ingested",
            "store_exists": false,
            "message": "session store does not exist yet — nothing has been ingested",
        }),
    )
}

fn project_local_storage_without_project(args: &Value) -> ToolResult {
    tool_json(
        None,
        args,
        &json!({
            "status": "unavailable",
            "message": "LCM storage requires an initialized TraceDecay project root",
        }),
    )
}

pub(super) struct LcmStorage {
    pub(super) db: Arc<GlobalDb>,
}

fn available_lcm_storage(db: GlobalDb) -> LcmStorageResolution {
    LcmStorageResolution::Available(Box::new(LcmStorage { db: Arc::new(db) }))
}

/// Database paths whose schema (sessions DDL + LCM migrations) has already
/// been ensured by this process. In `tracedecay serve`, every LCM tool call
/// re-opens the project session DB; once `GlobalDb::open_at` has ensured the
/// schema for a path, later opens skip the DDL batch and the LCM version
/// gate entirely via `open_at_assuming_schema`. The schema only ever grows
/// and lives in the file itself, so a concurrent process cannot invalidate
/// the flag; the `is_file` check below covers the file being deleted
/// underneath a long-lived server. One-shot CLI invocations open each path
/// once, so their behavior is unchanged.
///
/// Connections opened through this fallback path are deliberately not cached:
/// each call opens a fresh libsql local connection. Daemon-owned session stores
/// bypass this path and supply their retained writer authority directly.
static ENSURED_SCHEMA_DB_PATHS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn schema_already_ensured(db_path: &Path) -> bool {
    db_path.is_file()
        && ENSURED_SCHEMA_DB_PATHS
            .lock()
            .is_ok_and(|paths| paths.contains(db_path))
}

fn mark_schema_ensured(db_path: &Path) {
    if let Ok(mut paths) = ENSURED_SCHEMA_DB_PATHS.lock() {
        paths.insert(db_path.to_path_buf());
    }
}

/// Opens a writable session DB, ensuring the schema at most once per
/// process per path (see [`ENSURED_SCHEMA_DB_PATHS`]).
async fn open_session_db_with_cached_ensure(db_path: &Path) -> Option<GlobalDb> {
    if schema_already_ensured(db_path)
        && let Some(db) = GlobalDb::open_at_assuming_schema(db_path).await
    {
        return Some(db);
    }
    // Fast path failed (e.g. file replaced mid-session): fall through to
    // a full ensure rather than failing the tool call.
    let db = GlobalDb::open_at(db_path).await?;
    mark_schema_ensured(db_path);
    Some(db)
}

pub(super) enum LcmStorageResolution {
    Available(Box<LcmStorage>),
    Unavailable(ToolResult),
}

/// How an LCM storage open treats the backing sessions.db.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LcmOpenMode {
    /// Writable open: creates the store and ensures schema as needed.
    Writable,
    /// Read-only: a missing store is a hard error.
    ReadOnlyExisting,
    /// Read-only: a missing store is a distinguishable `not_ingested`
    /// result, without creating the file. Use this for every `readOnlyHint`
    /// LCM handler so "nothing ingested yet" never looks like "ok, 0 rows"
    /// (and the tool never ghost-creates an empty sessions.db).
    ReadOnlyOrMissing,
}

async fn open_lcm_db_at(db_path: &Path, mode: LcmOpenMode) -> Option<GlobalDb> {
    match mode {
        LcmOpenMode::Writable => open_session_db_with_cached_ensure(db_path).await,
        LcmOpenMode::ReadOnlyExisting | LcmOpenMode::ReadOnlyOrMissing => {
            GlobalDb::open_read_only_at(db_path).await
        }
    }
}

pub(super) async fn open_lcm_storage(
    context: LcmHandlerContext<'_>,
    args: &Value,
    mode: LcmOpenMode,
) -> LcmStorageResolution {
    let Some(db_path) = context.project_session_db_path else {
        return LcmStorageResolution::Unavailable(project_local_storage_without_project(args));
    };
    let db_path = db_path.to_path_buf();
    if mode == LcmOpenMode::ReadOnlyOrMissing && !db_path.is_file() {
        return LcmStorageResolution::Unavailable(lcm_not_yet_ingested(args));
    }
    if let Some(db) = context.retained_session_db {
        return LcmStorageResolution::Available(Box::new(LcmStorage { db: Arc::clone(db) }));
    }
    let Some(db) = open_lcm_db_at(&db_path, mode).await else {
        return LcmStorageResolution::Unavailable(lcm_unavailable(args));
    };
    available_lcm_storage(db)
}
