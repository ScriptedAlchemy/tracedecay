//! Shared session-ingest abstractions and provider-neutral transcript helpers.
//!
//! These types and helpers sit below any particular session source adapter:
//! file-backed [`crate::runtime::source`] drivers and the Hermes `SQLite` sweep
//! both depend on them so they do not need to import from each other.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

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

pub fn path_belongs_to_project(path: &Path, project_root: &Path) -> ProjectMembership {
    ProjectRootMatcher::new(project_root).contains(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectMembership {
    Match,
    NoMatch,
    Unknown(tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown),
}

impl ProjectMembership {
    pub fn definitive(self) -> Option<bool> {
        match self {
            Self::Match => Some(true),
            Self::NoMatch => Some(false),
            Self::Unknown(_) => None,
        }
    }
}

type GitIdentityResolver =
    fn(&Path) -> tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome;

pub const PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct ProjectRootMatcherCacheEntry {
    matcher: Arc<ProjectRootMatcher>,
    unknown_retry_after: Mutex<Option<Instant>>,
}

/// A project root with its git worktree/common-dir resolutions computed once,
/// so repeated membership tests (e.g. one per discovered workflow run) do not
/// re-run `git_worktree_root`/`git_common_dir` on the fixed project side. A
/// single [`ProjectRootMatcher::contains`] call is exactly equivalent to
/// [`path_belongs_to_project`], which is a thin wrapper over it.
#[derive(Debug)]
pub struct ProjectRootMatcher {
    root: PathBuf,
    identity: tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome,
    identity_resolver: GitIdentityResolver,
    path_membership: Mutex<HashMap<PathBuf, ProjectMembershipCacheEntry>>,
}

#[derive(Clone, Copy, Debug)]
struct ProjectMembershipCacheEntry {
    membership: ProjectMembership,
    unknown_retry_after: Option<Instant>,
}

impl ProjectRootMatcher {
    /// Resolve the fixed project-side git identity once.
    pub fn new(project_root: &Path) -> Self {
        Self::new_with_identity_resolver(
            project_root,
            tracedecay_runtime_core::git_discovery::discover_repository_identity_bounded,
        )
    }

    fn new_with_identity_resolver(
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
    /// Only the varying `path` side is git-resolved here.
    pub fn contains(&self, path: &Path) -> ProjectMembership {
        self.contains_at(path, Instant::now())
    }

    fn contains_at(&self, path: &Path, now: Instant) -> ProjectMembership {
        let cached = self
            .path_membership
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .copied();
        if let Some(cached) = cached
            && cached
                .unknown_retry_after
                .is_none_or(|retry_after| now < retry_after)
        {
            return cached.membership;
        }
        let membership = self.contains_uncached(path);
        self.path_membership
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                path.to_path_buf(),
                ProjectMembershipCacheEntry {
                    membership,
                    unknown_retry_after: matches!(membership, ProjectMembership::Unknown(_))
                        .then_some(now + PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN),
                },
            );
        membership
    }

    fn contains_uncached(&self, path: &Path) -> ProjectMembership {
        if paths_equal(path, &self.root) {
            return ProjectMembership::Match;
        }

        use tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome as Identity;
        let project_identity = &self.identity;
        if let Identity::Unknown(reason) = project_identity {
            return ProjectMembership::Unknown(*reason);
        }
        let path_identity = (self.identity_resolver)(path);
        match (project_identity, path_identity) {
            (Identity::Resolved(project), Identity::Resolved(candidate)) => {
                if paths_equal(&candidate.worktree_root, &project.worktree_root)
                    || paths_equal(&candidate.common_dir, &project.common_dir)
                {
                    ProjectMembership::Match
                } else {
                    ProjectMembership::NoMatch
                }
            }
            (_, Identity::Unknown(reason)) => ProjectMembership::Unknown(reason),
            (Identity::NotRepository, Identity::NotRepository) => {
                if tracedecay_runtime_core::config::discover_project_root(path)
                    .as_ref()
                    .is_some_and(|discovered| paths_equal(discovered, &self.root))
                {
                    ProjectMembership::Match
                } else {
                    ProjectMembership::NoMatch
                }
            }
            (Identity::Resolved(_), Identity::NotRepository)
            | (Identity::NotRepository, Identity::Resolved(_)) => ProjectMembership::NoMatch,
            (Identity::Unknown(reason), _) => ProjectMembership::Unknown(*reason),
        }
    }
}

/// Source-lifetime cache of repository identities and definitive memberships.
#[derive(Clone, Debug)]
pub struct ProjectRootMatcherCache {
    matchers: Arc<Mutex<HashMap<PathBuf, Arc<ProjectRootMatcherCacheEntry>>>>,
    identity_resolver: GitIdentityResolver,
}

impl Default for ProjectRootMatcherCache {
    fn default() -> Self {
        Self {
            matchers: Arc::default(),
            identity_resolver:
                tracedecay_runtime_core::git_discovery::discover_repository_identity_bounded,
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

    pub fn membership(&self, path: &Path, project_root: &Path) -> ProjectMembership {
        self.membership_at(path, project_root, Instant::now())
    }

    fn membership_at(&self, path: &Path, project_root: &Path, now: Instant) -> ProjectMembership {
        self.matcher_at(project_root, now).contains_at(path, now)
    }

    fn matcher_at(&self, project_root: &Path, now: Instant) -> Arc<ProjectRootMatcher> {
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
            if !matches!(
                entry.matcher.identity,
                tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::Unknown(_)
            ) {
                return entry.matcher.clone();
            }

            let should_retry = {
                let mut retry_after = entry
                    .unknown_retry_after
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let retry_after =
                    retry_after.get_or_insert(now + PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN);
                now >= *retry_after
            };
            if !should_retry {
                return entry.matcher.clone();
            }

            let mut matchers = self.matchers.lock().unwrap_or_else(PoisonError::into_inner);
            if matchers
                .get(&key)
                .is_some_and(|cached| Arc::ptr_eq(cached, &entry))
            {
                matchers.remove(&key);
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
/// [`path_belongs_to_project`] call re-runs `git_worktree_root` and
/// `git_common_dir` against the same unchanging root for every record.
pub enum TranscriptScopeMatcher {
    Project {
        project_root: PathBuf,
        cache: ProjectRootMatcherCache,
    },
    Profile {
        registered_roots: Vec<PathBuf>,
        cache: ProjectRootMatcherCache,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptScopeRouting {
    Accepted,
    Rejected,
    Deferred(tracedecay_runtime_core::git_discovery::GitDiscoveryUnknown),
}

impl TranscriptScopeMatcher {
    /// Project scope over a single root.
    pub fn project(project_root: &Path) -> Self {
        Self::project_with_cache(project_root, ProjectRootMatcherCache::default())
    }

    pub fn project_with_cache(project_root: &Path, cache: ProjectRootMatcherCache) -> Self {
        Self::Project {
            project_root: project_root.to_path_buf(),
            cache,
        }
    }

    /// Profile scope over every registered project root.
    pub fn profile(registered_roots: &[PathBuf]) -> Self {
        Self::profile_with_cache(registered_roots, ProjectRootMatcherCache::default())
    }

    pub fn profile_with_cache(
        registered_roots: &[PathBuf],
        cache: ProjectRootMatcherCache,
    ) -> Self {
        Self::Profile {
            registered_roots: registered_roots.to_vec(),
            cache,
        }
    }

    /// Profile scope when `registered_roots` is present, project scope
    /// otherwise — the shape every provider source carries as an
    /// `Option<Vec<PathBuf>>` user scope beside its project root.
    pub fn for_scope(project_root: &Path, registered_roots: Option<&[PathBuf]>) -> Self {
        registered_roots.map_or_else(|| Self::project(project_root), Self::profile)
    }

    pub fn for_scope_with_cache(
        project_root: &Path,
        registered_roots: Option<&[PathBuf]>,
        cache: ProjectRootMatcherCache,
    ) -> Self {
        if let Some(roots) = registered_roots {
            Self::profile_with_cache(roots, cache)
        } else {
            Self::project_with_cache(project_root, cache)
        }
    }

    /// Classify a record without collapsing temporarily unknown membership.
    pub fn route(&self, cwd: Option<&Path>) -> TranscriptScopeRouting {
        match self {
            Self::Project {
                project_root,
                cache,
            } => match cwd.map(|cwd| cache.membership(cwd, project_root)) {
                Some(ProjectMembership::Match) => TranscriptScopeRouting::Accepted,
                Some(ProjectMembership::NoMatch) | None => TranscriptScopeRouting::Rejected,
                Some(ProjectMembership::Unknown(reason)) => {
                    TranscriptScopeRouting::Deferred(reason)
                }
            },
            Self::Profile {
                registered_roots,
                cache,
            } => {
                let Some(cwd) = cwd else {
                    return TranscriptScopeRouting::Accepted;
                };
                let mut unknown = None;
                for root in registered_roots {
                    match cache.membership(cwd, root) {
                        ProjectMembership::Match => return TranscriptScopeRouting::Rejected,
                        ProjectMembership::NoMatch => {}
                        ProjectMembership::Unknown(reason) => {
                            unknown.get_or_insert(reason);
                        }
                    };
                }
                unknown.map_or(
                    TranscriptScopeRouting::Accepted,
                    TranscriptScopeRouting::Deferred,
                )
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
    let Some(cwd) = location.cwd else {
        return;
    };
    map.insert(
        keys.cwd.to_string(),
        Value::String(cwd.to_string_lossy().to_string()),
    );
    if let tracedecay_runtime_core::git_discovery::GitRepositoryIdentityOutcome::Resolved(
        identity,
    ) = tracedecay_runtime_core::git_discovery::discover_repository_identity_bounded(cwd)
    {
        map.insert(
            keys.worktree.to_string(),
            Value::String(identity.worktree_root.to_string_lossy().to_string()),
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
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use serde_json::json;

    use super::one_line_truncated;
    use super::usage_counters_from;
    use super::{
        PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN, ProjectMembership, ProjectRootMatcherCache,
        TranscriptScopeMatcher, TranscriptScopeRouting,
    };
    use tracedecay_runtime_core::git_discovery::{
        GitDiscoveryUnknown, GitRepositoryIdentity, GitRepositoryIdentityOutcome,
    };

    static IDENTITY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static IDENTITY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn unknown_then_resolved(path: &Path) -> GitRepositoryIdentityOutcome {
        match IDENTITY_CALLS.fetch_add(1, Ordering::SeqCst) {
            0 => GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded),
            _ => GitRepositoryIdentityOutcome::Resolved(GitRepositoryIdentity {
                worktree_root: path.to_path_buf(),
                git_dir: path.join(".git"),
                common_dir: path.join(".git"),
            }),
        }
    }

    fn resolved_by_repository(path: &Path) -> GitRepositoryIdentityOutcome {
        let worktree_root = path
            .ancestors()
            .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "member"))
            .unwrap_or(path)
            .to_path_buf();
        let common_dir = if worktree_root.ends_with("member") {
            PathBuf::from("/shared/member.git")
        } else {
            PathBuf::from("/shared/other.git")
        };
        GitRepositoryIdentityOutcome::Resolved(GitRepositoryIdentity {
            git_dir: worktree_root.join(".git"),
            worktree_root,
            common_dir,
        })
    }

    fn resolved_then_unknown_then_resolved(path: &Path) -> GitRepositoryIdentityOutcome {
        let call = IDENTITY_CALLS.fetch_add(1, Ordering::SeqCst);
        if call == 1 {
            return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded);
        }
        GitRepositoryIdentityOutcome::Resolved(GitRepositoryIdentity {
            worktree_root: path.to_path_buf(),
            git_dir: path.join(".git"),
            common_dir: PathBuf::from("/shared/member.git"),
        })
    }

    #[test]
    fn matcher_caches_definitive_membership_but_retries_unknown_after_cooldown() {
        let _guard = IDENTITY_TEST_LOCK.lock().expect("identity test lock");
        let fixture = tempfile::TempDir::new().expect("fixture");
        let root = fixture.path().join("member");
        std::fs::create_dir_all(&root).expect("project root");
        IDENTITY_CALLS.store(0, Ordering::SeqCst);
        let cache = ProjectRootMatcherCache::with_identity_resolver(unknown_then_resolved);
        let now = Instant::now();

        assert_eq!(
            cache.membership_at(&root.join("src"), &root, now),
            ProjectMembership::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        );
        assert_eq!(
            cache.membership_at(
                &root.join("src"),
                &root,
                now + PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN / 2,
            ),
            ProjectMembership::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        );
        assert_eq!(IDENTITY_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            cache.membership_at(
                &root.join("src"),
                &root,
                now + PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN,
            ),
            ProjectMembership::Match
        );
        assert_eq!(IDENTITY_CALLS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn project_and_profile_scopes_defer_the_same_unknown_membership() {
        let _guard = IDENTITY_TEST_LOCK.lock().expect("identity test lock");
        let fixture = tempfile::TempDir::new().expect("fixture");
        let root = fixture.path().join("member");
        let cwd = root.join("src");
        std::fs::create_dir_all(&cwd).expect("cwd");
        IDENTITY_CALLS.store(0, Ordering::SeqCst);
        let cache = ProjectRootMatcherCache::with_identity_resolver(unknown_then_resolved);

        let project = TranscriptScopeMatcher::project_with_cache(&root, cache.clone());
        let profile = TranscriptScopeMatcher::profile_with_cache(&[root], cache);
        assert_eq!(
            project.route(Some(&cwd)),
            TranscriptScopeRouting::Deferred(GitDiscoveryUnknown::DeadlineExceeded)
        );
        assert_eq!(
            profile.route(Some(&cwd)),
            TranscriptScopeRouting::Deferred(GitDiscoveryUnknown::DeadlineExceeded)
        );
    }

    #[test]
    fn matcher_retries_unknown_candidate_identity_after_cooldown() {
        let _guard = IDENTITY_TEST_LOCK.lock().expect("identity test lock");
        let fixture = tempfile::TempDir::new().expect("fixture");
        let root = fixture.path().join("member");
        let cwd = root.join("src");
        std::fs::create_dir_all(&cwd).expect("cwd");
        IDENTITY_CALLS.store(0, Ordering::SeqCst);
        let cache =
            ProjectRootMatcherCache::with_identity_resolver(resolved_then_unknown_then_resolved);
        let now = Instant::now();

        assert_eq!(
            cache.membership_at(&cwd, &root, now),
            ProjectMembership::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        );
        assert_eq!(
            cache.membership_at(
                &cwd,
                &root,
                now + PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN / 2,
            ),
            ProjectMembership::Unknown(GitDiscoveryUnknown::DeadlineExceeded)
        );
        assert_eq!(IDENTITY_CALLS.load(Ordering::SeqCst), 2);
        assert_eq!(
            cache.membership_at(&cwd, &root, now + PROJECT_MEMBERSHIP_UNKNOWN_RETRY_COOLDOWN,),
            ProjectMembership::Match
        );
        assert_eq!(IDENTITY_CALLS.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn repository_identity_distinguishes_member_and_nonmember() {
        let fixture = tempfile::TempDir::new().expect("fixture");
        let root = fixture.path().join("member");
        let member = root.join("src");
        let other = fixture.path().join("other/src");
        std::fs::create_dir_all(&member).expect("member");
        std::fs::create_dir_all(&other).expect("other");
        let cache = ProjectRootMatcherCache::with_identity_resolver(resolved_by_repository);

        assert_eq!(cache.membership(&member, &root), ProjectMembership::Match);
        assert_eq!(cache.membership(&other, &root), ProjectMembership::NoMatch);
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
