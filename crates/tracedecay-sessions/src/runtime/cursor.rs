use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::SessionMessageRecord;
use crate::runtime::shared::{
    StoredCursor, TranscriptLocation, TranscriptLocationMetadataKeys, append_location_metadata,
    append_tool_calls_metadata, append_tool_event_metadata, append_usage_metadata,
    content_storage_text_and_tools, paths_equal, title_from_messages,
};
use crate::runtime::source::{
    ParsedTranscript, SessionDraft, TranscriptIngestStore, TranscriptSource,
    collect_files_with_ext, ingest_source, stream_new_jsonl,
};
use tracedecay_runtime_core::{config, timeutil};
const CURSOR_EVENT_LOCATION_KEYS: TranscriptLocationMetadataKeys =
    TranscriptLocationMetadataKeys::new(
        "cursor_event_cwd",
        "cursor_event_worktree",
        "cursor_event_location_provenance",
    );

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CursorTranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
}

/// A Cursor hook event scoped to one transcript file.
pub struct CursorEventSource {
    event: Value,
    transcript_path: PathBuf,
    include_subagents: bool,
    user_scope: bool,
}

impl TranscriptSource for CursorEventSource {
    fn provider(&self) -> &'static str {
        "cursor"
    }

    fn transcript_paths(&self, _project_root: &Path) -> Vec<PathBuf> {
        let mut paths = vec![self.transcript_path.clone()];
        if self.include_subagents {
            let parent_session_id = event_session_id(&self.event, &self.transcript_path);
            paths.extend(cursor_subagent_paths(
                &self.transcript_path,
                &parent_session_id,
            ));
        }
        paths
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        _project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let parent_session_id = event_session_id(&self.event, &self.transcript_path);
        parse_cursor_jsonl(
            &self.event,
            &parent_session_id,
            path,
            prev,
            max_new_bytes,
            self.user_scope,
        )
    }
}

/// Parse the newly-appended portion of one Cursor transcript file into a
/// provider-neutral [`ParsedTranscript`]. Shared by the hook path
/// ([`CursorEventSource`]) and the startup catch-up sweep
/// ([`CursorSweepSource`]); both derive identical session/message ids for the
/// same file (the hook event's `session_id` always equals the transcript file
/// stem), so whichever runs second is an idempotent no-op.
pub fn parse_cursor_jsonl(
    event: &Value,
    parent_session_id: &str,
    path: &Path,
    prev: StoredCursor,
    max_new_bytes: Option<u64>,
    user_scope: bool,
) -> Option<ParsedTranscript> {
    let new = stream_new_jsonl(path, prev, max_new_bytes)?;
    let subagent = cursor_subagent_identity(path, parent_session_id);
    let session_id = subagent.as_ref().map_or_else(
        || parent_session_id.to_string(),
        |(session_id, _agent_id)| session_id.clone(),
    );
    let subagent_model = subagent.as_ref().and_then(|(_, agent_id)| {
        parent_dispatch_model_for_subagent(path, parent_session_id, agent_id)
    });
    let event_cwd = event_cwd(event);
    let event_location_provenance = event_location_provenance(event);
    let mut carry = TimestampCarry::new(i64::try_from(new.new_cursor.mtime).ok());
    let mut messages = Vec::new();
    for line in &new.lines {
        let derived_timestamp = carry.observe(&line.value);
        let context = CursorMessageContext {
            transcript_path: path,
            source_offset: line.offset,
            derived_timestamp,
            model_fallback: subagent_model.as_deref(),
            event_cwd: event_cwd.as_deref(),
            event_location_provenance,
        };
        // The byte offset doubles as the message ordinal and source_offset,
        // matching the original Cursor ingestion.
        if let Some(message) = event_message(&line.value, event, &session_id, line.offset, context)
        {
            messages.push(message);
        }
        messages.extend(event_dispatch_messages(
            &line.value,
            event,
            &session_id,
            context,
        ));
    }

    // Defer the (filesystem-walking) project/title/metadata derivation until
    // we actually have new messages; the driver ignores the draft otherwise.
    let draft = if messages.is_empty() {
        SessionDraft {
            session_id,
            project_key: String::new(),
            project_path: String::new(),
            title: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    } else {
        let (project_key, project_path) = if user_scope {
            ("user".to_string(), "user".to_string())
        } else {
            event_project(event)
        };
        let (draft_parent_session_id, agent_id) = subagent
            .map_or((None, None), |(_session_id, agent_id)| {
                (Some(parent_session_id.to_string()), Some(agent_id))
            });
        let is_subagent = draft_parent_session_id.is_some();
        SessionDraft {
            session_id,
            project_key,
            project_path,
            title: title_from_messages(&messages),
            metadata_json: serde_json::to_string(&session_metadata(
                event,
                event_cwd.as_deref(),
                event_location_provenance,
            ))
            .ok(),
            parent_session_id: draft_parent_session_id,
            is_subagent,
            agent_id,
            parent_tool_use_id: None,
        }
    };

    Some(ParsedTranscript {
        draft,
        messages,
        new_cursor: new.new_cursor,
    })
}

/// Ingest the Cursor transcript referenced by a hook payload into the
/// provider-neutral session/message tables for the provided database. Project
/// hooks should pass the resolved project DB from [`open_project_session_db`].
///
/// Ingestion is **incremental**: it resumes from the byte offset recorded in the
/// DB's `parse_offsets` table (via the shared [`crate::sessions::source`]
/// driver), so each call only parses and upserts transcript lines appended since
/// the last run rather than re-reading the whole file. Repeated calls on an
/// unchanged file are a no-op.
pub async fn ingest_cursor_transcript_event<S>(
    event_json: &str,
    db: &S,
) -> CursorTranscriptIngestStats
where
    S: TranscriptIngestStore,
{
    ingest_cursor_transcript_event_capped(event_json, db, None).await
}

/// Like [`ingest_cursor_transcript_event`], but bounds how many newly-appended
/// bytes a single call will read. Cursor hooks pass byte caps to stay within hook
/// budgets; capped reads still discover subagent transcript files, with each file
/// independently subject to the same cap.
pub async fn ingest_cursor_transcript_event_capped<S>(
    event_json: &str,
    db: &S,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats
where
    S: TranscriptIngestStore,
{
    let Ok(event) = serde_json::from_str::<Value>(event_json) else {
        return CursorTranscriptIngestStats::default();
    };
    let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    else {
        return CursorTranscriptIngestStats::default();
    };

    // Cursor derives its project from the event, so the driver's project_root
    // argument is unused by `CursorEventSource`; the transcript path's parent is
    // a cheap, side-effect-free placeholder.
    let project_root = transcript_path
        .parent()
        .map_or_else(|| transcript_path.clone(), Path::to_path_buf);
    let source = CursorEventSource {
        event,
        transcript_path,
        include_subagents: true,
        user_scope: false,
    };
    let stats = ingest_source(db, &source, &project_root, max_new_bytes).await;
    CursorTranscriptIngestStats {
        sessions_upserted: stats.sessions_upserted,
        messages_upserted: stats.messages_upserted,
    }
}

pub async fn ingest_cursor_user_transcript_event_capped<S>(
    event_json: &str,
    db: &S,
    max_new_bytes: Option<u64>,
) -> CursorTranscriptIngestStats
where
    S: TranscriptIngestStore,
{
    ingest_cursor_user_transcript_event_capped_with_registered_roots(
        event_json,
        db,
        max_new_bytes,
        &[],
    )
    .await
}

/// User-scope live ingest guarded by a registry snapshot. The unguarded
/// wrapper remains useful for isolated parsing without a profile registry.
pub async fn ingest_cursor_user_transcript_event_capped_with_registered_roots<S>(
    event_json: &str,
    db: &S,
    max_new_bytes: Option<u64>,
    registered_roots: &[PathBuf],
) -> CursorTranscriptIngestStats
where
    S: TranscriptIngestStore,
{
    let Ok(event) = serde_json::from_str::<Value>(event_json) else {
        return CursorTranscriptIngestStats::default();
    };
    let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    else {
        return CursorTranscriptIngestStats::default();
    };
    let event_workspaces = cursor_event_workspace_roots(&event);
    let belongs_to_registered_project = if event_workspaces.is_empty() {
        // Without event workspace identity, Cursor's transcript directory is
        // the only attribution available. Its slash-to-hyphen encoding is
        // lossy, so a registered-slug collision must fail closed rather than
        // risk copying project evidence into user memory.
        cursor_transcript_project_slug(&transcript_path).is_some_and(|slug| {
            registered_roots
                .iter()
                .filter_map(|root| cursor_project_slug(root))
                .any(|registered_slug| registered_slug == slug)
        })
    } else {
        // A hook-provided cwd/file/workspace root is stronger than the lossy
        // transcript slug. This keeps distinct slash-vs-hyphen workspaces,
        // linked worktrees, and renamed checkouts from excluding one another.
        event_workspaces.iter().any(|workspace| {
            registered_roots
                .iter()
                .any(|registered| paths_equal(workspace, registered))
        })
    };
    if belongs_to_registered_project {
        return CursorTranscriptIngestStats::default();
    }
    let placeholder = transcript_path
        .parent()
        .map_or_else(|| transcript_path.clone(), Path::to_path_buf);
    let source = CursorEventSource {
        event,
        transcript_path,
        include_subagents: true,
        user_scope: true,
    };
    let stats = ingest_source(db, &source, &placeholder, max_new_bytes).await;
    CursorTranscriptIngestStats {
        sessions_upserted: stats.sessions_upserted,
        messages_upserted: stats.messages_upserted,
    }
}

pub fn cursor_event_workspace_roots(event: &Value) -> Vec<PathBuf> {
    let candidates = if let Some(cwd) = event_cwd(event) {
        vec![cwd]
    } else if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(file_path);
        vec![path.parent().unwrap_or(path).to_path_buf()]
    } else {
        event
            .get("workspace_roots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .collect()
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        let root = config::discover_project_root(&candidate).unwrap_or(candidate);
        if !roots.iter().any(|seen| paths_equal(seen, &root)) {
            roots.push(root);
        }
    }
    roots
}

fn cursor_transcript_project_slug(path: &Path) -> Option<&str> {
    let components = path.components().collect::<Vec<_>>();
    let transcripts = components
        .iter()
        .position(|component| component.as_os_str() == "agent-transcripts")?;
    components
        .get(transcripts.checked_sub(1)?)?
        .as_os_str()
        .to_str()
}

/// `agent-transcripts/<session>/subagents/<child>.jsonl` is the deepest layout
/// Cursor writes; a little headroom tolerates future nesting.
const MAX_SWEEP_SCAN_DEPTH: u8 = 4;
/// Upper bound on directory-existence probes while checking a slug for decode
/// ambiguity; exhausting it treats the slug as ambiguous (skip, never guess).
const SLUG_DECODE_PROBE_BUDGET: u32 = 4096;

/// Startup catch-up source for Cursor transcripts.
///
/// The live hook path ([`ingest_cursor_transcript_event`]) only sees turns
/// that fire while the tracedecay hooks are installed, so transcripts written
/// before a project was indexed could never ingest. This source sweeps
/// `~/.cursor/projects/<slug>/agent-transcripts/**.jsonl` for the slug that
/// encodes `project_root`, feeding every file through the same
/// [`parse_cursor_jsonl`] parser and (path-keyed) `parse_offsets` cursors as
/// the hook path — files either path has already ingested are byte-offset
/// no-ops for the other, so sweep and hooks never double-ingest.
pub struct CursorSweepSource {
    cursor_projects_dir: PathBuf,
    /// Session ids already owned by the richer composer store
    /// ([`crate::sessions::cursor_composer`]). Transcript files whose stem is
    /// one of these are skipped so the two Cursor sources never double-ingest.
    skip_session_ids: std::collections::HashSet<String>,
    user_registered_slugs: Option<std::collections::HashSet<String>>,
}

impl CursorSweepSource {
    /// Source rooted at the real `~/.cursor/projects`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = super::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.cursor/projects` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        Self {
            cursor_projects_dir: home.join(".cursor").join("projects"),
            skip_session_ids: std::collections::HashSet::new(),
            user_registered_slugs: None,
        }
    }

    /// Skip transcript files whose stem (the Cursor session id) is owned by the
    /// composer store, so the composer rows win without duplication.
    #[must_use]
    pub fn with_skip_session_ids(mut self, ids: std::collections::HashSet<String>) -> Self {
        self.skip_session_ids = ids;
        self
    }

    #[must_use]
    pub fn for_user_scope(mut self, registered_roots: &[PathBuf]) -> Self {
        self.user_registered_slugs = Some(
            registered_roots
                .iter()
                .filter_map(|root| cursor_project_slug(root))
                .collect(),
        );
        self
    }
}

impl TranscriptSource for CursorSweepSource {
    fn provider(&self) -> &'static str {
        "cursor"
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        if let Some(registered_slugs) = &self.user_registered_slugs {
            let Ok(entries) = std::fs::read_dir(&self.cursor_projects_dir) else {
                return Vec::new();
            };
            return entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|slug| !registered_slugs.contains(slug))
                })
                .flat_map(|entry| {
                    collect_files_with_ext(
                        &entry.path().join("agent-transcripts"),
                        "jsonl",
                        MAX_SWEEP_SCAN_DEPTH,
                    )
                })
                .collect();
        }
        let Some(slug) = cursor_project_slug(project_root) else {
            return Vec::new();
        };
        let transcripts_dir = self
            .cursor_projects_dir
            .join(&slug)
            .join("agent-transcripts");
        if !transcripts_dir.is_dir() {
            return Vec::new();
        }
        // The slug encoding is lossy (`/` becomes `-`, and real directory
        // names may themselves contain `-`). When another *existing* directory
        // also encodes to this slug, the transcripts in it cannot be
        // attributed safely, so skip with a note rather than guess.
        match decode_slug_candidates(project_root, &slug) {
            Some(candidates)
                if candidates
                    .iter()
                    .all(|candidate| paths_equal(candidate, project_root)) => {}
            _ => {
                eprintln!(
                    "Skipping Cursor transcript sweep for {}: project slug '{slug}' is ambiguous \
                     (another existing directory also encodes to it).",
                    project_root.display()
                );
                return Vec::new();
            }
        }
        let files = collect_files_with_ext(&transcripts_dir, "jsonl", MAX_SWEEP_SCAN_DEPTH);
        // Cursor materializes some subagent sessions twice: under their
        // parent's `subagents/` dir and again as a top-level
        // `<id>/<id>.jsonl` copy whose content drifts slightly (so byte
        // offsets — and therefore message ids — diverge). Ingesting both
        // would duplicate messages and overwrite the parent linkage; keep
        // the subagent copy (it carries parentage, and it is the copy the
        // live hook path ingests) and skip the top-level duplicate.
        let subagent_stems: std::collections::HashSet<std::ffi::OsString> = files
            .iter()
            .filter(|path| is_subagent_transcript(path))
            .filter_map(|path| path.file_stem().map(std::ffi::OsStr::to_os_string))
            .collect();
        files
            .into_iter()
            .filter(|path| {
                is_subagent_transcript(path)
                    || path
                        .file_stem()
                        .is_none_or(|stem| !subagent_stems.contains(stem))
            })
            .filter(|path| {
                // Composer-owned sessions are ingested (richer) by the composer
                // sweep; skip the JSONL copy so neither path double-ingests.
                self.skip_session_ids.is_empty()
                    || path
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .is_none_or(|stem| !self.skip_session_ids.contains(stem))
            })
            .collect()
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        let parent_session_id = sweep_parent_session_id(path)?;
        // Synthesize the minimal hook-shaped event the shared parser expects:
        // the same session id a live hook would carry (Cursor names parent
        // transcripts `<session-id>.jsonl`) and the project root as `cwd` so
        // `event_project` scopes the session exactly like the hook path.
        let user_scope = self.user_registered_slugs.is_some();
        let event = if user_scope {
            serde_json::json!({
                "session_id": parent_session_id,
                "tracedecay_location_provenance": "user_sweep",
            })
        } else {
            serde_json::json!({
                "session_id": parent_session_id,
                "cwd": project_root.to_string_lossy(),
                "tracedecay_location_provenance": "sweep_project_root",
            })
        };
        parse_cursor_jsonl(
            &event,
            &parent_session_id,
            path,
            prev,
            max_new_bytes,
            user_scope,
        )
    }
}

/// Compute the `~/.cursor/projects` directory slug Cursor derives from a
/// workspace path: every normal path component joined with `-`, case
/// preserved (verified against real `~/.cursor/projects` entries).
/// Returns `None` for non-UTF-8, relative, or traversal-containing paths.
pub fn cursor_project_slug(project_root: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in project_root.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
            std::path::Component::CurDir | std::path::Component::ParentDir => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("-"))
}

/// Enumerate every *existing* directory that [`cursor_project_slug`] would
/// encode to `slug`, by walking the filesystem from `project_root`'s root and
/// re-grouping dash-separated tokens into path components (pruned to
/// directories that actually exist). Returns `None` when the probe budget is
/// exhausted, which callers must treat as "ambiguous".
fn decode_slug_candidates(project_root: &Path, slug: &str) -> Option<Vec<PathBuf>> {
    let mut base = PathBuf::new();
    for component in project_root.components() {
        match component {
            std::path::Component::Normal(_) => break,
            other => base.push(other.as_os_str()),
        }
    }
    let tokens: Vec<&str> = slug.split('-').collect();
    let mut candidates = Vec::new();
    let mut budget = SLUG_DECODE_PROBE_BUDGET;
    let exhausted = decode_slug_inner(&base, &tokens, &mut candidates, &mut budget);
    (!exhausted).then_some(candidates)
}

/// Depth-first regrouping of `tokens` into existing directory components
/// under `base`. Returns `true` when the probe budget ran out (enumeration is
/// incomplete and the result must not be trusted).
fn decode_slug_inner(
    base: &Path,
    tokens: &[&str],
    candidates: &mut Vec<PathBuf>,
    budget: &mut u32,
) -> bool {
    if tokens.is_empty() {
        candidates.push(base.to_path_buf());
        return false;
    }
    for split in 1..=tokens.len() {
        if *budget == 0 {
            return true;
        }
        *budget -= 1;
        let candidate = base.join(tokens[..split].join("-"));
        if candidate.is_dir() && decode_slug_inner(&candidate, &tokens[split..], candidates, budget)
        {
            return true;
        }
    }
    false
}

/// Whether a transcript file lives in a `subagents/` directory.
fn is_subagent_transcript(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("subagents")
}

/// Derive the parent-session id for a swept transcript file from its location:
/// `…/<parent>/subagents/<child>.jsonl` belongs to `<parent>`; anything else
/// is a parent transcript whose file stem *is* the session id (which always
/// equals the `session_id` a live hook event would carry for that file).
fn sweep_parent_session_id(path: &Path) -> Option<String> {
    if is_subagent_transcript(path) {
        return path
            .parent()?
            .parent()?
            .file_name()?
            .to_str()
            .map(str::to_string);
    }
    path.file_stem()?.to_str().map(str::to_string)
}

fn cursor_subagent_paths(transcript_path: &Path, parent_session_id: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent_dir) = transcript_path.parent() {
        if transcript_path.file_stem().and_then(|stem| stem.to_str()) == Some(parent_session_id) {
            candidates.push(parent_dir.join(parent_session_id).join("subagents"));
        }
        if parent_dir.file_name().and_then(|name| name.to_str()) == Some(parent_session_id) {
            candidates.push(parent_dir.join("subagents"));
        }
    }

    let mut paths = Vec::new();
    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn cursor_subagent_identity(path: &Path, parent_session_id: &str) -> Option<(String, String)> {
    let is_subagent_path = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("subagents");
    if !is_subagent_path {
        return None;
    }
    let parent_dir = path.parent()?.parent()?;
    if parent_dir.file_name().and_then(|name| name.to_str()) != Some(parent_session_id) {
        return None;
    }
    let session_id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| !id.is_empty())?
        .to_string();
    Some((session_id.clone(), session_id))
}

fn parent_dispatch_model_for_subagent(
    path: &Path,
    parent_session_id: &str,
    agent_id: &str,
) -> Option<String> {
    let parent_dir = path.parent()?.parent()?;
    let candidates = [
        parent_dir.join(format!("{parent_session_id}.jsonl")),
        parent_dir.with_extension("jsonl"),
    ];
    for candidate in candidates {
        if let Some(model) = dispatch_model_for_agent(&candidate, agent_id) {
            return Some(model);
        }
    }
    None
}

fn dispatch_model_for_agent(path: &Path, agent_id: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let message = record.get("message").unwrap_or(&record);
        let content = message.get("content").unwrap_or(message);
        let Some(items) = content.as_array() else {
            continue;
        };
        for item in items {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            if is_subagent_dispatch_tool(name) && dispatch_targets_agent(item, agent_id) {
                if let Some(model) = cursor_dispatch_model(item) {
                    return Some(model);
                }
            }
        }
    }
    None
}

fn dispatch_targets_agent(item: &Value, agent_id: &str) -> bool {
    let input = item.get("input").unwrap_or(item);
    [
        "agent_id",
        "agentId",
        "subagent_id",
        "subagentId",
        "session_id",
        "sessionId",
        "id",
    ]
    .into_iter()
    .any(|key| {
        input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            == Some(agent_id)
    })
}

/// Per-line timestamp derivation for Cursor transcripts, which carry no
/// structured per-message timestamps. The injected `<timestamp>…</timestamp>`
/// tag in user prompts is parsed and carried forward across subsequent lines
/// (assistant turns happen after the prompt that started them); lines seen
/// before any tag fall back to the transcript file's mtime, which on the
/// incremental hook path approximates "now" for freshly appended lines.
pub struct TimestampCarry {
    carried: Option<i64>,
    fallback: Option<i64>,
}

impl TimestampCarry {
    pub fn new(fallback_mtime: Option<i64>) -> Self {
        Self {
            carried: None,
            fallback: fallback_mtime.filter(|mtime| *mtime > 0),
        }
    }

    /// Folds one transcript line into the carry and returns the timestamp to
    /// use for messages derived from that line.
    pub fn observe(&mut self, record: &Value) -> Option<i64> {
        if let Some(tag) = timestamp_tag_from_record(record) {
            self.carried = Some(tag);
        }
        self.carried.or(self.fallback)
    }
}

/// Extracts and parses the first `<timestamp>…</timestamp>` tag found in a
/// transcript line's text content.
fn timestamp_tag_from_record(record: &Value) -> Option<i64> {
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    match content {
        Value::String(text) => timestamp_tag_from_text(text),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .find_map(timestamp_tag_from_text),
        _ => None,
    }
}

fn timestamp_tag_from_text(text: &str) -> Option<i64> {
    let start = text.find("<timestamp>")? + "<timestamp>".len();
    let end = start + text[start..].find("</timestamp>")?;
    timeutil::parse_cursor_human_timestamp(text[start..end].trim())
}

#[derive(Clone, Copy)]
struct CursorMessageContext<'a> {
    transcript_path: &'a Path,
    source_offset: i64,
    derived_timestamp: Option<i64>,
    model_fallback: Option<&'a str>,
    event_cwd: Option<&'a Path>,
    event_location_provenance: &'a str,
}

fn event_message(
    record: &Value,
    event: &Value,
    session_id: &str,
    ordinal: i64,
    context: CursorMessageContext<'_>,
) -> Option<SessionMessageRecord> {
    let role = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())?;
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    if content_is_only_subagent_dispatch(content) {
        return None;
    }
    let (text, tool_names) = content_storage_text_and_tools(
        content,
        message
            .get("tool_calls")
            .or_else(|| record.get("tool_calls")),
    );
    if text.trim().is_empty() {
        return None;
    }

    let message_id = record
        .get("id")
        .or_else(|| message.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || format!("{session_id}:{ordinal}"),
            std::string::ToString::to_string,
        );
    let model = cursor_record_message_model(record, message)
        .or_else(|| context.model_fallback.map(str::to_string))
        .or_else(|| cursor_model_string(event));

    Some(SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id,
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp: record_timestamp(record)
            .or_else(|| record_timestamp(event))
            .or(context.derived_timestamp),
        ordinal,
        text,
        kind: content_kind(content).map(str::to_string),
        model,
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(context.transcript_path.to_string_lossy().to_string()),
        source_offset: Some(context.source_offset),
        metadata_json: serde_json::to_string(&message_metadata(
            record,
            message,
            content,
            context.event_cwd,
            context.event_location_provenance,
        ))
        .ok(),
    })
}

fn event_dispatch_messages(
    record: &Value,
    event: &Value,
    session_id: &str,
    context: CursorMessageContext<'_>,
) -> Vec<SessionMessageRecord> {
    let Some(role) = record
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.is_empty())
    else {
        return Vec::new();
    };
    let message = record.get("message").unwrap_or(record);
    let content = message.get("content").unwrap_or(message);
    let Some(items) = content.as_array() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(name) = item.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !is_subagent_dispatch_tool(name) {
            continue;
        }
        let Some(text) = dispatch_text(item) else {
            continue;
        };
        let tool_use_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let message_id = tool_use_id.map_or_else(
            || {
                format!(
                    "{}:tool_dispatch:{}:{index}",
                    session_id, context.source_offset
                )
            },
            |id| format!("{session_id}:tool_dispatch:{id}"),
        );
        out.push(SessionMessageRecord {
            provider: "cursor".to_string(),
            message_id,
            session_id: session_id.to_string(),
            role: role.to_string(),
            timestamp: record_timestamp(record)
                .or_else(|| record_timestamp(event))
                .or(context.derived_timestamp),
            ordinal: context.source_offset.saturating_add(index as i64),
            text,
            kind: Some("tool_dispatch".to_string()),
            model: cursor_dispatch_model(item)
                .or_else(|| cursor_record_message_model(record, message))
                .or_else(|| context.model_fallback.map(str::to_string))
                .or_else(|| cursor_model_string(event)),
            tool_names: Some(name.to_string()),
            source_path: Some(context.transcript_path.to_string_lossy().to_string()),
            source_offset: Some(context.source_offset),
            metadata_json: serde_json::to_string(&dispatch_message_metadata(
                record,
                tool_use_id,
                context.event_cwd,
                context.event_location_provenance,
            ))
            .ok(),
        });
    }
    out
}

fn cursor_model_string(value: &Value) -> Option<String> {
    [
        "model",
        "model_id",
        "modelId",
        "model_name",
        "modelName",
        "model_slug",
        "modelSlug",
        "model_display_name",
        "modelDisplayName",
        "display_model",
        "displayModel",
        "display_model_name",
        "displayModelName",
    ]
    .into_iter()
    .find_map(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|model| !model.trim().is_empty())
            .map(str::to_string)
    })
}

fn cursor_record_message_model(record: &Value, message: &Value) -> Option<String> {
    cursor_model_string(record).or_else(|| cursor_model_string(message))
}

fn cursor_dispatch_model(item: &Value) -> Option<String> {
    item.get("input")
        .and_then(cursor_model_string)
        .or_else(|| cursor_model_string(item))
}

fn is_subagent_dispatch_tool(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "task" | "subagent")
}

fn content_is_only_subagent_dispatch(content: &Value) -> bool {
    let Some(items) = content.as_array() else {
        return false;
    };
    !items.is_empty()
        && items.iter().all(|item| {
            item.get("type").and_then(Value::as_str) == Some("tool_use")
                && item
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(is_subagent_dispatch_tool)
        })
}

fn dispatch_text(item: &Value) -> Option<String> {
    let input = item.get("input").unwrap_or(item);
    let mut parts = Vec::new();
    for key in ["description", "prompt", "subagent_type"] {
        if let Some(value) = input
            .get(key)
            .or_else(|| item.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            parts.push(value.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn content_kind(content: &Value) -> Option<&'static str> {
    if content.is_array() {
        Some("message")
    } else if content.is_string() {
        Some("text")
    } else {
        None
    }
}

fn event_session_id(event: &Value, transcript_path: &Path) -> String {
    event
        .get("session_id")
        .or_else(|| event.get("conversation_id"))
        .or_else(|| event.get("chat_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map_or_else(
            || {
                transcript_path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            },
            str::to_string,
        )
}

fn event_project(event: &Value) -> (String, String) {
    let cwd_root = event_cwd(event).and_then(|cwd| config::discover_project_root(&cwd));
    let candidates = event_project_candidates(event);
    let resolved = candidates
        .iter()
        .find_map(|candidate| config::discover_project_root(candidate))
        .or_else(|| candidates.into_iter().next());
    let project_path = match (cwd_root, resolved) {
        (Some(cwd_root), Some(resolved)) if !paths_equal(&cwd_root, &resolved) => cwd_root,
        (Some(cwd_root), None) => cwd_root,
        (_, Some(resolved)) => resolved,
        _ => return ("unknown".to_string(), "unknown".to_string()),
    };
    let project = project_path.to_string_lossy().to_string();
    (project.clone(), project)
}

fn event_cwd(event: &Value) -> Option<PathBuf> {
    event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn event_project_candidates(event: &Value) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push_unique = |candidate: PathBuf| {
        if !candidates.iter().any(|seen| seen == &candidate) {
            candidates.push(candidate);
        }
    };
    if let Some(cwd) = event
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        push_unique(PathBuf::from(cwd));
    }
    if let Some(file_path) = event
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(file_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(transcript_path) = event
        .get("transcript_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    {
        let path = Path::new(transcript_path);
        push_unique(path.parent().unwrap_or(path).to_path_buf());
    }
    if let Some(roots) = event.get("workspace_roots").and_then(Value::as_array) {
        for root in roots {
            if let Some(path) = root.as_str().filter(|path| !path.is_empty()) {
                push_unique(PathBuf::from(path));
            }
        }
    }
    candidates
}

fn record_timestamp(value: &Value) -> Option<i64> {
    value
        .get("timestamp")
        .or_else(|| value.get("created_at"))
        .and_then(|timestamp| {
            timestamp
                .as_i64()
                .or_else(|| timestamp.as_str().and_then(|s| s.parse::<i64>().ok()))
        })
}

fn event_location_provenance(event: &Value) -> &str {
    event
        .get("tracedecay_location_provenance")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("hook_event")
}

fn session_metadata(event: &Value, event_cwd: Option<&Path>, location_provenance: &str) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "conversation_id".to_string(),
        event.get("conversation_id").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "hook_event_name".to_string(),
        event.get("hook_event_name").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "cursor_version".to_string(),
        event.get("cursor_version").cloned().unwrap_or(Value::Null),
    );
    if let Some(roots) = event.get("workspace_roots") {
        metadata.insert("workspace_roots".to_string(), roots.clone());
    }
    append_location_metadata(
        &mut metadata,
        CURSOR_EVENT_LOCATION_KEYS,
        TranscriptLocation::new(event_cwd, location_provenance),
    );
    Value::Object(metadata)
}

fn message_metadata(
    record: &Value,
    message: &Value,
    content: &Value,
    event_cwd: Option<&Path>,
    location_provenance: &str,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "raw_type".to_string(),
        record.get("type").cloned().unwrap_or(Value::Null),
    );
    append_location_metadata(
        &mut metadata,
        CURSOR_EVENT_LOCATION_KEYS,
        TranscriptLocation::new(event_cwd, location_provenance),
    );
    append_tool_calls_metadata(&mut metadata, message);
    append_tool_event_metadata(&mut metadata, content);
    // These JSONL agent-transcript lines carry no token counters (verified
    // across 100k+ real lines). Cursor *does* record per-turn token counts, but
    // only in the composer store (`state.vscdb` bubbles), which the richer
    // `cursor_composer` sweep reads and maps to `usage`. This probe stays as
    // future-proofing in case the JSONL format gains counters too.
    append_usage_metadata(&mut metadata, &[record, message]);
    Value::Object(metadata)
}

fn dispatch_message_metadata(
    record: &Value,
    tool_use_id: Option<&str>,
    event_cwd: Option<&Path>,
    location_provenance: &str,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("cursor_transcript".to_string()),
    );
    metadata.insert(
        "raw_type".to_string(),
        record.get("type").cloned().unwrap_or(Value::Null),
    );
    metadata.insert(
        "tool_use_id".to_string(),
        tool_use_id.map_or(Value::Null, |id| Value::String(id.to_string())),
    );
    append_location_metadata(
        &mut metadata,
        CURSOR_EVENT_LOCATION_KEYS,
        TranscriptLocation::new(event_cwd, location_provenance),
    );
    Value::Object(metadata)
}
