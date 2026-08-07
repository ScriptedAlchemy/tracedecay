//! Shared session-ingest abstractions and provider-neutral transcript helpers.
//!
//! These types and helpers sit below any particular session source adapter:
//! file-backed [`crate::runtime::source`] drivers and the Hermes `SQLite` sweep
//! both depend on them so they do not need to import from each other.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tracedecay_runtime_core::worktree::GitRepoIdentityOutcome;

use crate::runtime::SessionMessageRecord;
pub use crate::{NewRows, StoredCursor, TranscriptIngestStats};

/// Shareable handle to a read-only rusqlite connection over a foreign
/// (non-TraceDecay-owned) `SQLite` store.
///
/// S11: foreign session readers run on the bundled rusqlite engine. The mutex
/// makes the handle `Sync`, so async ingest futures may hold it across await
/// points and stay `Send`; every SQL call runs on a blocking thread via
/// [`SqliteReadConn::with`], keeping the async executor unblocked.
#[derive(Clone)]
pub struct SqliteReadConn {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

impl SqliteReadConn {
    pub fn new(conn: rusqlite::Connection) -> Self {
        Self {
            inner: Arc::new(Mutex::new(conn)),
        }
    }

    /// Runs `body` against the connection on a blocking thread. Returns `None`
    /// only if the blocking task itself fails (cancellation/panic), which
    /// callers degrade to the same outcome as any SQL error.
    pub async fn with<T, F>(&self, body: F) -> Option<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> T + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock().unwrap_or_else(PoisonError::into_inner);
            body(&guard)
        })
        .await
        .ok()
    }
}

/// Generic per-transcript backlog threshold for warning that automatic
/// session transcript catch-up may not drain recall transcripts quickly enough.
pub const SESSION_TRANSCRIPT_STALLED_INGEST_WARNING_BYTES: u64 = 2 * 1024 * 1024;

/// **`RowCursor`** reader for SQLite-backed transcript stores (Zed, Copilot CLI
/// `session-store.db`).
///
/// Selects rows whose rowid is greater than `prev.position` (the last-seen
/// rowid), ordered ascending, mapping each through `map_row` *during* iteration
/// (rows must not outlive the statement cursor) and advancing the stored cursor
/// to the maximum rowid seen. `select_sql` must select the rowid as its first
/// column and accept a single `?` bound to the previous rowid, e.g.
/// `"SELECT rowid, role, text FROM turns WHERE rowid > ? ORDER BY rowid"`.
/// Fail-open: any query error yields `None`; `map_row` returning `None` skips
/// that row while still advancing the cursor. The whole read runs as one
/// blocking call on the connection's thread.
pub async fn read_new_rows<T, F>(
    conn: &SqliteReadConn,
    select_sql: &str,
    prev: StoredCursor,
    map_row: F,
) -> Option<NewRows<T>>
where
    T: Send + 'static,
    F: FnMut(i64, &rusqlite::Row<'_>) -> Option<T> + Send + 'static,
{
    let select_sql = select_sql.to_string();
    conn.with(move |conn| read_new_rows_sync(conn, &select_sql, prev, map_row))
        .await
        .flatten()
}

fn read_new_rows_sync<T>(
    conn: &rusqlite::Connection,
    select_sql: &str,
    prev: StoredCursor,
    mut map_row: impl FnMut(i64, &rusqlite::Row<'_>) -> Option<T>,
) -> Option<NewRows<T>> {
    let mut statement = match conn.prepare(select_sql) {
        Ok(statement) => statement,
        Err(error) => {
            tracing::debug!(
                select_sql,
                previous_rowid = prev.position,
                error = %error,
                "skipping transcript row source query"
            );
            return None;
        }
    };
    let mut result_rows = match statement.query(rusqlite::params![prev.position as i64]) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(
                select_sql,
                previous_rowid = prev.position,
                error = %error,
                "skipping transcript row source query"
            );
            return None;
        }
    };

    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    while let Ok(Some(row)) = result_rows.next() {
        let Ok(rowid) = row.get::<_, i64>(0) else {
            tracing::debug!(
                select_sql,
                "skipping transcript row without rowid in column 0"
            );
            continue;
        };
        if rowid as u64 > max_rowid {
            max_rowid = rowid as u64;
        }
        if let Some(item) = map_row(rowid, row) {
            items.push(item);
        }
    }

    Some(NewRows {
        items,
        new_cursor: StoredCursor {
            position: max_rowid,
            // Row stores have no single file mtime; the rowid alone is the
            // monotonic cursor, so mtime is left as a sentinel.
            mtime: 0,
            file_id: 0,
        },
    })
}

/// Compare two paths for equality, canonicalizing when possible so that
/// symlinks/`..`/trailing differences do not cause false mismatches. Falls back
/// to a literal comparison when canonicalization fails (e.g. a path that no
/// longer exists).
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => normalized_paths_equal(&a, &b),
        _ => normalized_paths_equal(a, b),
    }
}

pub fn path_belongs_to_project(path: &Path, project_root: &Path) -> bool {
    ProjectRootMatcher::new(project_root).contains(path)
}

/// Tri-state project membership for one transcript working directory.
///
/// `Unknown` is reserved for bounded git-identity timeouts: the path could not
/// be attributed to or excluded from the project without blocking. Callers that
/// persist ingest cursors must defer instead of treating it as `NoMatch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectMembership {
    Match,
    NoMatch,
    Unknown,
}

impl ProjectMembership {
    fn from_bool(value: bool) -> Self {
        if value { Self::Match } else { Self::NoMatch }
    }

    pub fn definitive(self) -> Option<bool> {
        match self {
            Self::Match => Some(true),
            Self::NoMatch => Some(false),
            Self::Unknown => None,
        }
    }
}

/// Resolver for a path's git repository identity, injectable so tests can fake
/// bounded-timeout (`Unknown`) outcomes without a real blocking repository.
pub type GitIdentityResolver = fn(&Path) -> GitRepoIdentityOutcome;

/// How long an `Unknown` (timed-out) identity resolution is served from cache
/// before the next lookup retries the underlying git discovery.
const LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
struct LocationWorktreeCacheEntry {
    outcome: OnceLock<GitRepoIdentityOutcome>,
    unknown_retry_after: Mutex<Option<Instant>>,
}

#[derive(Debug)]
struct ProjectRootMatcherCacheEntry {
    matcher: Arc<ProjectRootMatcher>,
    unknown_retry_after: Mutex<Option<Instant>>,
}

/// A project root with its git worktree/common-dir resolutions computed once,
/// so repeated membership tests (e.g. one per discovered workflow run) do not
/// re-resolve the fixed project side. A single [`ProjectRootMatcher::contains`]
/// call is exactly equivalent to [`path_belongs_to_project`], which is a thin
/// wrapper over it.
#[derive(Debug)]
pub struct ProjectRootMatcher {
    root: PathBuf,
    identity: GitRepoIdentityOutcome,
    identity_resolver: GitIdentityResolver,
    path_membership: Mutex<HashMap<PathBuf, bool>>,
}

impl ProjectRootMatcher {
    /// Resolve the fixed project-side git identity once.
    pub fn new(project_root: &Path) -> Self {
        Self::new_with_identity_resolver(
            project_root,
            tracedecay_runtime_core::worktree::git_repo_identity_outcome,
        )
    }

    pub fn new_with_identity_resolver(
        project_root: &Path,
        identity_resolver: GitIdentityResolver,
    ) -> Self {
        Self {
            root: project_root.to_path_buf(),
            identity: identity_resolver(project_root),
            identity_resolver,
            path_membership: Mutex::new(HashMap::new()),
        }
    }

    /// True when `path` belongs to this project: it is the root, shares the
    /// project's git worktree or common dir, or discovers back to the root.
    /// Each distinct path is resolved once for this matcher, so repeated
    /// transcript rows with the same cwd do not repeatedly discover/open git.
    pub fn contains(&self, path: &Path) -> bool {
        self.contains_status(path) == ProjectMembership::Match
    }

    pub fn contains_status(&self, path: &Path) -> ProjectMembership {
        if let Some(belongs) = self
            .path_membership
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .copied()
        {
            return ProjectMembership::from_bool(belongs);
        }

        let belongs = self.contains_uncached(path);
        if let Some(definitive) = belongs.definitive() {
            self.path_membership
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(path.to_path_buf(), definitive);
        }
        belongs
    }

    fn contains_uncached(&self, path: &Path) -> ProjectMembership {
        self.contains_uncached_with(
            path,
            self.identity_resolver,
            tracedecay_runtime_core::config::discover_project_root,
        )
    }

    fn contains_uncached_with(
        &self,
        path: &Path,
        identity_resolver: impl FnOnce(&Path) -> GitRepoIdentityOutcome,
        discover_project_root: impl FnOnce(&Path) -> Option<PathBuf>,
    ) -> ProjectMembership {
        if paths_equal(path, &self.root) {
            return ProjectMembership::Match;
        }
        if self.identity == GitRepoIdentityOutcome::Unknown {
            return ProjectMembership::Unknown;
        }

        let path_identity = identity_resolver(path);
        match (&self.identity, path_identity) {
            (
                GitRepoIdentityOutcome::Resolved(project_identity),
                GitRepoIdentityOutcome::Resolved(path_identity),
            ) => {
                if paths_equal(
                    &path_identity.worktree_root,
                    &project_identity.worktree_root,
                ) {
                    return ProjectMembership::Match;
                }
                return ProjectMembership::from_bool(paths_equal(
                    &path_identity.common_dir,
                    &project_identity.common_dir,
                ));
            }
            (GitRepoIdentityOutcome::Unknown, _) | (_, GitRepoIdentityOutcome::Unknown) => {
                return ProjectMembership::Unknown;
            }
            _ => {}
        }

        ProjectMembership::from_bool(
            discover_project_root(path)
                .as_ref()
                .is_some_and(|discovered| paths_equal(discovered, &self.root)),
        )
    }
}

/// Source-lifetime cache of project matchers keyed by canonical project root.
///
/// A source parses many transcript files for the same project. Keeping the
/// matcher here avoids reopening the same git repository once per file while
/// retaining per-path membership caching inside [`ProjectRootMatcher`]. An
/// `Unknown` (timed-out) identity is served from cache only for a cooldown,
/// after which the next lookup re-resolves it.
#[derive(Clone, Debug)]
pub struct ProjectRootMatcherCache {
    matchers: Arc<Mutex<HashMap<PathBuf, Arc<ProjectRootMatcherCacheEntry>>>>,
    location_worktrees: Arc<Mutex<HashMap<PathBuf, Arc<LocationWorktreeCacheEntry>>>>,
    identity_resolver: GitIdentityResolver,
}

impl Default for ProjectRootMatcherCache {
    fn default() -> Self {
        Self {
            matchers: Arc::default(),
            location_worktrees: Arc::default(),
            identity_resolver: tracedecay_runtime_core::worktree::git_repo_identity_outcome,
        }
    }
}

impl ProjectRootMatcherCache {
    #[cfg(test)]
    pub(crate) fn with_identity_resolver(identity_resolver: GitIdentityResolver) -> Self {
        Self {
            identity_resolver,
            ..Self::default()
        }
    }

    pub fn get(&self, project_root: &Path) -> Arc<ProjectRootMatcher> {
        self.get_at(project_root, Instant::now())
    }

    fn get_at(&self, project_root: &Path, now: Instant) -> Arc<ProjectRootMatcher> {
        let key = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        loop {
            let entry = self
                .matchers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.clone())
                .or_insert_with(|| {
                    Arc::new(ProjectRootMatcherCacheEntry {
                        matcher: Arc::new(ProjectRootMatcher::new_with_identity_resolver(
                            project_root,
                            self.identity_resolver,
                        )),
                        unknown_retry_after: Mutex::new(None),
                    })
                })
                .clone();
            if entry.matcher.identity != GitRepoIdentityOutcome::Unknown {
                return entry.matcher.clone();
            }

            let should_retry = {
                let mut retry_after = entry
                    .unknown_retry_after
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let retry_after =
                    retry_after.get_or_insert(now + LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN);
                now >= *retry_after
            };
            if !should_retry {
                return entry.matcher.clone();
            }

            let mut matchers = self
                .matchers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if matchers
                .get(&key)
                .is_some_and(|cached| Arc::ptr_eq(cached, &entry))
            {
                matchers.remove(&key);
            }
        }
    }

    pub fn membership(&self, path: &Path, project_root: &Path) -> ProjectMembership {
        self.get(project_root).contains_status(path)
    }

    pub fn membership_against_roots(
        &self,
        path: &Path,
        project_roots: &[PathBuf],
    ) -> ProjectMembership {
        let mut unknown = false;
        for root in project_roots {
            match self.membership(path, root) {
                ProjectMembership::Match => return ProjectMembership::Match,
                ProjectMembership::NoMatch => {}
                ProjectMembership::Unknown => unknown = true,
            }
        }
        if unknown {
            ProjectMembership::Unknown
        } else {
            ProjectMembership::NoMatch
        }
    }

    /// Resolve a transcript cwd's worktree once for this ingest source.
    ///
    /// Location metadata is added per message, so one transcript can otherwise
    /// repeat git discovery thousands of times for the same cwd. Keep this
    /// source-lifetime like the project matchers and use the bounded CLI-first
    /// identity path instead of opening the repository object database.
    pub fn git_worktree_root(&self, cwd: &Path) -> Option<PathBuf> {
        self.git_worktree_root_at(
            cwd,
            Instant::now(),
            &tracedecay_runtime_core::worktree::git_repo_identity_outcome,
        )
    }

    fn git_worktree_root_at(
        &self,
        cwd: &Path,
        now: Instant,
        identity_resolver: &impl Fn(&Path) -> GitRepoIdentityOutcome,
    ) -> Option<PathBuf> {
        loop {
            let resolution = self
                .location_worktrees
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(cwd.to_path_buf())
                .or_insert_with(|| Arc::new(LocationWorktreeCacheEntry::default()))
                .clone();
            match resolution
                .outcome
                .get_or_init(|| identity_resolver(cwd))
                .clone()
            {
                GitRepoIdentityOutcome::Resolved(identity) => {
                    return Some(identity.worktree_root);
                }
                GitRepoIdentityOutcome::NotFound => return None,
                GitRepoIdentityOutcome::Unknown => {
                    let should_retry = {
                        let mut retry_after = resolution
                            .unknown_retry_after
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner);
                        let retry_after = retry_after
                            .get_or_insert(now + LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN);
                        now >= *retry_after
                    };
                    if !should_retry {
                        return None;
                    }

                    let mut worktrees = self
                        .location_worktrees
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if worktrees
                        .get(cwd)
                        .is_some_and(|cached| Arc::ptr_eq(cached, &resolution))
                    {
                        worktrees.remove(cwd);
                    }
                }
            }
        }
    }
}

/// Decides whether one transcript record belongs to the scope currently being
/// ingested.
///
/// Every file-backed provider draws the same line, because the two ingest
/// scopes partition the same records between them:
///
/// * **Project** scope keeps a record when its working directory belongs to
///   the project being ingested.
/// * **Profile** (user-global) scope keeps a record when its working directory
///   belongs to *no* registered project — records with no working directory at
///   all are user-global by definition. That is exactly the complement of the
///   project scopes, so each record lands in one store and not both.
///
/// Resolving the fixed root side once is the point: the equivalent per-record
/// [`path_belongs_to_project`] call re-resolves the same unchanging root for
/// every record.
pub enum TranscriptScopeMatcher {
    Project(Arc<ProjectRootMatcher>),
    Profile(Vec<Arc<ProjectRootMatcher>>),
}

impl TranscriptScopeMatcher {
    /// Project scope over a single root.
    pub fn project(project_root: &Path) -> Self {
        Self::Project(Arc::new(ProjectRootMatcher::new(project_root)))
    }

    /// Profile scope over every registered project root.
    pub fn profile(registered_roots: &[PathBuf]) -> Self {
        Self::Profile(
            registered_roots
                .iter()
                .map(|root| Arc::new(ProjectRootMatcher::new(root)))
                .collect(),
        )
    }

    /// Profile scope when `registered_roots` is present, project scope
    /// otherwise — the shape every provider source carries as an
    /// `Option<Vec<PathBuf>>` user scope beside its project root.
    pub fn for_scope(project_root: &Path, registered_roots: Option<&[PathBuf]>) -> Self {
        registered_roots.map_or_else(|| Self::project(project_root), Self::profile)
    }

    /// [`Self::for_scope`] resolved through a source-lifetime matcher cache,
    /// so a source parsing many transcripts reuses one git identity resolution
    /// per root instead of re-discovering it per file.
    pub fn for_scope_cached(
        project_root: &Path,
        registered_roots: Option<&[PathBuf]>,
        cache: &ProjectRootMatcherCache,
    ) -> Self {
        match registered_roots {
            None => Self::Project(cache.get(project_root)),
            Some(roots) => Self::Profile(roots.iter().map(|root| cache.get(root)).collect()),
        }
    }

    /// True when a record with this working directory belongs to the scope.
    /// An `Unknown` membership counts as "does not belong"; callers that can
    /// defer ingestion should use [`Self::membership`] instead.
    pub fn accepts(&self, cwd: Option<&Path>) -> bool {
        match self {
            Self::Project(project) => cwd.is_some_and(|cwd| project.contains(cwd)),
            Self::Profile(registered) => {
                cwd.is_none_or(|cwd| !registered.iter().any(|root| root.contains(cwd)))
            }
        }
    }

    /// Tri-state scope acceptance: `Match`/`NoMatch` mirror [`Self::accepts`],
    /// while `Unknown` reports that a bounded git timeout left the record's
    /// scope undecided — deferring callers must not persist their cursor.
    pub fn membership(&self, cwd: Option<&Path>) -> ProjectMembership {
        match self {
            Self::Project(project) => {
                cwd.map_or(ProjectMembership::NoMatch, |cwd| project.contains_status(cwd))
            }
            Self::Profile(registered) => {
                let Some(cwd) = cwd else {
                    return ProjectMembership::Match;
                };
                let mut unknown = false;
                for matcher in registered {
                    match matcher.contains_status(cwd) {
                        ProjectMembership::Match => return ProjectMembership::NoMatch,
                        ProjectMembership::NoMatch => {}
                        ProjectMembership::Unknown => unknown = true,
                    }
                }
                if unknown {
                    ProjectMembership::Unknown
                } else {
                    ProjectMembership::Match
                }
            }
        }
    }
}

#[cfg(windows)]
fn normalized_paths_equal(a: &Path, b: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        let path = path.to_string_lossy().replace('/', "\\");
        path.strip_prefix(r"\\?\")
            .unwrap_or(&path)
            .to_ascii_lowercase()
    }

    normalize(a) == normalize(b)
}

#[cfg(not(windows))]
fn normalized_paths_equal(a: &Path, b: &Path) -> bool {
    a == b
}

/// Collapse internal whitespace/newlines to single spaces and clip to at most
/// `max` characters, appending a single-character `…` when truncation occurred.
/// Shared by the workflow surfaces (run/agent summaries, result summaries,
/// unfinished-run evidence) so a multi-line blob never smears a table, bullet,
/// or stored column.
pub fn one_line_truncated(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(max).collect();
    format!("{truncated}…")
}

/// Clip `text` to at most `max_bytes` on a UTF-8 boundary, appending a single
/// `…` only when truncation occurred. Unlike [`one_line_truncated`] this keeps
/// internal newlines, so multi-line derived-row previews retain their structure.
pub fn preview_truncated(text: &str, max_bytes: usize) -> String {
    let prefix = tracedecay_runtime_core::text::utf8_prefix_at_or_before(text, max_bytes);
    if prefix.len() == text.len() {
        prefix.to_string()
    } else {
        format!("{prefix}…")
    }
}

/// Collapse whitespace and clip to a short preview suitable for a session title.
pub fn preview_title(text: &str) -> String {
    const MAX_TITLE_CHARS: usize = 80;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        collapsed
    } else {
        collapsed.chars().take(MAX_TITLE_CHARS).collect()
    }
}

/// Return the storage representation used by LCM raw ingest for provider
/// transcript content. This intentionally matches the active-message path:
/// strings stay strings, structured content is compact JSON.
pub fn message_storage_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    serde_json::to_string(content).unwrap_or_else(|_| content.to_string())
}

/// Return lossless storage text plus tool names discovered in either structured
/// content blocks or a sibling `tool_calls` field.
pub fn content_storage_text_and_tools(
    content: &Value,
    tool_calls: Option<&Value>,
) -> (String, Vec<String>) {
    let mut tools = Vec::new();
    collect_tool_names(content, &mut tools);
    if let Some(tool_calls) = tool_calls {
        collect_tool_names(tool_calls, &mut tools);
    }
    tools.sort();
    tools.dedup();
    (message_storage_text(content), tools)
}

pub fn append_tool_calls_metadata(map: &mut serde_json::Map<String, Value>, message: &Value) {
    if let Some(tool_calls) = message.get("tool_calls") {
        map.insert("tool_calls".to_string(), tool_calls.clone());
    }
}

/// Byte length of `serde_json::to_string(value)`, or 0 when `value` is absent.
fn json_byte_len(value: Option<&Value>) -> u64 {
    let Some(value) = value else {
        return 0;
    };
    let mut sink = ByteCountSink::default();
    if serde_json::to_writer(&mut sink, value).is_ok() {
        sink.count
    } else {
        0
    }
}

/// `io::Write` sink that counts bytes without retaining them, so JSON byte
/// lengths can be measured without allocating an intermediate `String`.
#[derive(Default)]
struct ByteCountSink {
    count: u64,
}

impl io::Write for ByteCountSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.count += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Records bounded per-call tool metadata (byte counts and identifiers only,
/// never content) for `tool_use`/`tool_result` blocks found in `content`.
/// Inserts the `tool_events` key only when at least one entry was collected.
pub fn append_tool_event_metadata(map: &mut serde_json::Map<String, Value>, content: &Value) {
    let Some(items) = content.as_array() else {
        return;
    };
    let mut events = Vec::new();
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        match item_type {
            "tool_use" => {
                let mut event = serde_json::Map::new();
                event.insert("type".to_string(), Value::String("tool_use".to_string()));
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    event.insert("tool_name".to_string(), Value::String(name.to_string()));
                }
                if let Some(id) = item.get("id").and_then(Value::as_str) {
                    event.insert("call_id".to_string(), Value::String(id.to_string()));
                }
                event.insert(
                    "input_bytes".to_string(),
                    Value::from(json_byte_len(item.get("input"))),
                );
                events.push(Value::Object(event));
            }
            "tool_result" => {
                let mut event = serde_json::Map::new();
                event.insert("type".to_string(), Value::String("tool_result".to_string()));
                if let Some(id) = item.get("tool_use_id").and_then(Value::as_str) {
                    event.insert("call_id".to_string(), Value::String(id.to_string()));
                }
                event.insert(
                    "output_bytes".to_string(),
                    Value::from(json_byte_len(item.get("content"))),
                );
                events.push(Value::Object(event));
            }
            _ => {}
        }
    }
    if !events.is_empty() {
        map.insert("tool_events".to_string(), Value::Array(events));
    }
}

#[derive(Clone, Copy)]
pub struct TranscriptLocation<'a> {
    pub cwd: Option<&'a Path>,
    pub provenance: &'a str,
}

impl<'a> TranscriptLocation<'a> {
    pub fn new(cwd: Option<&'a Path>, provenance: &'a str) -> Self {
        Self { cwd, provenance }
    }
}

#[derive(Clone, Copy)]
pub struct TranscriptLocationMetadataKeys {
    pub cwd: &'static str,
    pub worktree: &'static str,
    pub provenance: &'static str,
}

impl TranscriptLocationMetadataKeys {
    pub const fn new(cwd: &'static str, worktree: &'static str, provenance: &'static str) -> Self {
        Self {
            cwd,
            worktree,
            provenance,
        }
    }
}

pub fn append_location_metadata(
    map: &mut serde_json::Map<String, Value>,
    keys: TranscriptLocationMetadataKeys,
    location: TranscriptLocation<'_>,
) {
    append_location_metadata_with_worktree(
        map,
        keys,
        location,
        location
            .cwd
            .and_then(tracedecay_runtime_core::worktree::git_worktree_root),
    );
}

/// [`append_location_metadata`] with the cwd's worktree resolved through a
/// source-lifetime cache, so one transcript's repeated cwd does not re-run git
/// discovery for every message row.
pub fn append_location_metadata_cached(
    map: &mut serde_json::Map<String, Value>,
    keys: TranscriptLocationMetadataKeys,
    location: TranscriptLocation<'_>,
    cache: &ProjectRootMatcherCache,
) {
    append_location_metadata_with_worktree(
        map,
        keys,
        location,
        location.cwd.and_then(|cwd| cache.git_worktree_root(cwd)),
    );
}

fn append_location_metadata_with_worktree(
    map: &mut serde_json::Map<String, Value>,
    keys: TranscriptLocationMetadataKeys,
    location: TranscriptLocation<'_>,
    worktree: Option<PathBuf>,
) {
    let Some(cwd) = location.cwd else {
        return;
    };
    map.insert(
        keys.cwd.to_string(),
        Value::String(cwd.to_string_lossy().to_string()),
    );
    if let Some(worktree) = worktree {
        map.insert(
            keys.worktree.to_string(),
            Value::String(worktree.to_string_lossy().to_string()),
        );
    }
    map.insert(
        keys.provenance.to_string(),
        Value::String(location.provenance.to_string()),
    );
}

/// Token-usage counter keys recognized by the savings dashboard
/// (`dashboard/savings_api.rs` `MESSAGE_TOKENS_CTE`): both the Anthropic
/// (`input_tokens`/`output_tokens`/`cache_*`) and `OpenAI`
/// (`prompt_tokens`/`completion_tokens`) shapes, plus total/reasoning counters
/// for reference.
const USAGE_COUNTER_KEYS: [&str; 9] = [
    "input_tokens",
    "output_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "total_tokens",
    "reasoning_tokens",
    "reasoning_output_tokens",
];

/// Extracts a `usage` counters object from a transcript record/message,
/// keeping only recognized numeric token counters (so arbitrarily large or
/// provider-private payloads never bloat `metadata_json`). Returns `None`
/// when the value has no `usage` object or it carries no recognized counters.
pub fn usage_counters_from(value: &Value) -> Option<Value> {
    let usage = value.get("usage")?.as_object()?;
    let mut counters = serde_json::Map::new();
    for key in USAGE_COUNTER_KEYS {
        if let Some(count) = usage.get(key).and_then(Value::as_i64) {
            counters.insert(key.to_string(), Value::from(count));
        }
    }
    if !counters.contains_key("cache_read_input_tokens")
        && let Some(count) = usage.get("cached_input_tokens").and_then(Value::as_i64)
    {
        counters.insert("cache_read_input_tokens".to_string(), Value::from(count));
    }
    if !counters.is_empty()
        && !counters.contains_key("input_tokens")
        && !counters.contains_key("prompt_tokens")
        && !counters.contains_key("output_tokens")
        && !counters.contains_key("completion_tokens")
    {
        counters.insert("input_tokens".to_string(), Value::from(0));
        counters.insert("output_tokens".to_string(), Value::from(0));
    }
    (!counters.is_empty()).then_some(Value::Object(counters))
}

/// Inserts transcript-recorded token usage into message metadata under the
/// `usage` key the savings dashboard reads. Probes each candidate value in
/// order and keeps the first recognized counters object.
pub fn append_usage_metadata(map: &mut serde_json::Map<String, Value>, candidates: &[&Value]) {
    if map.contains_key("usage") {
        return;
    }
    if let Some(usage) = candidates
        .iter()
        .find_map(|value| usage_counters_from(value))
    {
        map.insert("usage".to_string(), usage);
    }
}

fn collect_tool_names(value: &Value, tools: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_tool_names(item, tools);
            }
        }
        Value::Object(map) => {
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_use" | "tool_call" | "function_call")
            ) && let Some(name) = map.get("name").and_then(Value::as_str)
            {
                tools.push(name.to_string());
            }
            for key in ["tool_call", "functionCall", "function_call", "function"] {
                if let Some(name) = map
                    .get(key)
                    .and_then(Value::as_object)
                    .and_then(|nested| nested.get("name"))
                    .and_then(Value::as_str)
                {
                    tools.push(name.to_string());
                }
            }
            if let Some(tool_calls) = map.get("tool_calls") {
                collect_tool_names(tool_calls, tools);
            }
        }
        _ => {}
    }
}

fn title_text_from_stored_content(text: &str) -> String {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| visible_text_from_content(&value))
        .unwrap_or_else(|| text.to_string())
}

fn visible_text_from_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(visible_text_from_content)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        Value::Object(map) => {
            for key in ["text", "content", "message"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    return Some(text.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Build a session title from the first user message, if any.
pub fn title_from_messages(messages: &[SessionMessageRecord]) -> Option<String> {
    messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| preview_title(&title_text_from_stored_content(&message.text)))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tracedecay_runtime_core::worktree::{GitRepoIdentity, GitRepoIdentityOutcome};

    use super::LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN;
    use super::ProjectMembership;
    use super::ProjectRootMatcher;
    use super::ProjectRootMatcherCache;
    use super::TranscriptLocation;
    use super::TranscriptLocationMetadataKeys;
    use super::append_location_metadata_cached;
    use super::one_line_truncated;
    use super::usage_counters_from;

    static MATCHER_CACHE_RESOLVER_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn unknown_then_resolved_identity(path: &Path) -> GitRepoIdentityOutcome {
        if MATCHER_CACHE_RESOLVER_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            GitRepoIdentityOutcome::Unknown
        } else {
            GitRepoIdentityOutcome::Resolved(GitRepoIdentity {
                worktree_root: path.to_path_buf(),
                common_dir: path.join(".git"),
            })
        }
    }

    fn resolved_test_identity(path: &Path) -> GitRepoIdentityOutcome {
        let root = path
            .ancestors()
            .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "repo"))
            .unwrap_or(path);
        GitRepoIdentityOutcome::Resolved(GitRepoIdentity {
            worktree_root: root.to_path_buf(),
            common_dir: root.join(".git"),
        })
    }

    #[test]
    fn matcher_timeout_is_unknown_without_downstream_discovery() {
        let temp = TempDir::new().expect("temp dir");
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
        let matcher =
            ProjectRootMatcher::new_with_identity_resolver(&project_root, resolved_test_identity);

        let membership = matcher.contains_uncached_with(
            &nested_cwd,
            |_| GitRepoIdentityOutcome::Unknown,
            |_| panic!("timeout must not fall through to project discovery"),
        );

        assert_eq!(membership, ProjectMembership::Unknown);
    }

    #[test]
    fn matcher_cache_suppresses_repeated_unknown_identity_lookups() {
        let temp = TempDir::new().expect("temp dir");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("root");
        MATCHER_CACHE_RESOLVER_CALLS.store(0, Ordering::SeqCst);
        let cache = ProjectRootMatcherCache::with_identity_resolver(unknown_then_resolved_identity);
        let now = Instant::now();

        let first = cache.get_at(&root, now);
        let first_path = root.join("first-session");
        let second_path = root.join("second-session");
        std::fs::create_dir_all(&first_path).expect("first session");
        std::fs::create_dir_all(&second_path).expect("second session");
        assert_eq!(
            first.contains_status(&first_path),
            ProjectMembership::Unknown
        );
        assert_eq!(
            first.contains_status(&second_path),
            ProjectMembership::Unknown
        );
        let during_cooldown =
            cache.get_at(&root, now + LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN / 2);

        assert!(Arc::ptr_eq(&first, &during_cooldown));
        assert_eq!(first.identity, GitRepoIdentityOutcome::Unknown);
        assert_eq!(MATCHER_CACHE_RESOLVER_CALLS.load(Ordering::SeqCst), 1);

        let retried = cache.get_at(&root, now + LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN);
        assert!(!Arc::ptr_eq(&first, &retried));
        assert!(matches!(
            retried.identity,
            GitRepoIdentityOutcome::Resolved(_)
        ));
        assert_eq!(MATCHER_CACHE_RESOLVER_CALLS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn location_metadata_unknown_uses_cooldown_then_retries() {
        let temp = TempDir::new().expect("temp dir");
        let cwd = temp.path().join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let cache = ProjectRootMatcherCache::default();
        let calls = AtomicUsize::new(0);
        let now = Instant::now();
        let resolver = |path: &Path| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                GitRepoIdentityOutcome::Unknown
            } else {
                GitRepoIdentityOutcome::Resolved(GitRepoIdentity {
                    worktree_root: path.to_path_buf(),
                    common_dir: path.join(".git"),
                })
            }
        };

        assert!(cache.git_worktree_root_at(&cwd, now, &resolver).is_none());
        assert!(
            cache
                .git_worktree_root_at(
                    &cwd,
                    now + LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN / 2,
                    &resolver,
                )
                .is_none()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert_eq!(
            cache.git_worktree_root_at(
                &cwd,
                now + LOCATION_WORKTREE_UNKNOWN_RETRY_COOLDOWN,
                &resolver,
            ),
            Some(cwd)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn location_metadata_cache_reuses_worktree_root_for_repeated_cwd() {
        let temp = TempDir::new().expect("temp dir");
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project_root)
            .status()
            .expect("git init");
        assert!(status.success());

        let cache = ProjectRootMatcherCache::default();
        let keys = TranscriptLocationMetadataKeys::new("cwd", "worktree", "provenance");
        let location = TranscriptLocation::new(Some(&nested_cwd), "test");
        let mut first = serde_json::Map::new();
        append_location_metadata_cached(&mut first, keys, location, &cache);
        assert_eq!(
            first.get("worktree").and_then(Value::as_str),
            Some(
                project_root
                    .canonicalize()
                    .expect("canonical project root")
                    .to_string_lossy()
                    .as_ref()
            )
        );

        std::fs::rename(project_root.join(".git"), project_root.join(".git.hidden"))
            .expect("hide git metadata after first lookup");

        let mut second = serde_json::Map::new();
        append_location_metadata_cached(&mut second, keys, location, &cache);
        assert_eq!(
            second.get("worktree").and_then(Value::as_str),
            first.get("worktree").and_then(Value::as_str),
            "repeated cwd should reuse the source-lifetime worktree resolution"
        );
    }

    #[test]
    fn project_root_matcher_caches_repeated_path_membership() {
        let temp = TempDir::new().expect("temp dir");
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).expect("nested cwd");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project_root)
            .status()
            .expect("git init");
        assert!(status.success());

        let matcher = ProjectRootMatcher::new(&project_root);
        assert!(matcher.contains(&nested_cwd));

        // A repeated lookup should use the result already resolved for this
        // cwd, rather than discovering/opening the same repository again.
        std::fs::rename(project_root.join(".git"), project_root.join(".git.hidden"))
            .expect("hide git metadata after first lookup");
        assert!(matcher.contains(&nested_cwd));
    }

    #[test]
    fn one_line_truncated_collapses_and_clips() {
        assert_eq!(one_line_truncated("a\n b\t c", 100), "a b c");
        assert_eq!(one_line_truncated("abcdef", 3), "abc…");
    }

    #[test]
    fn usage_counters_keep_cache_only_rows_actual() {
        let Some(usage) = usage_counters_from(&json!({
            "usage": {
                "cache_read_input_tokens": 123,
                "total_tokens": 123
            }
        })) else {
            panic!("cache-only usage should be retained");
        };

        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["cache_read_input_tokens"], 123);
        assert_eq!(usage["total_tokens"], 123);
    }

    #[test]
    fn usage_counters_normalize_openai_cached_input_alias() {
        let Some(usage) = usage_counters_from(&json!({
            "usage": {
                "cached_input_tokens": 456,
                "total_tokens": 456
            }
        })) else {
            panic!("OpenAI cache alias should be retained");
        };

        assert_eq!(usage["input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 0);
        assert_eq!(usage["cache_read_input_tokens"], 456);
        assert_eq!(usage["total_tokens"], 456);
    }
}
