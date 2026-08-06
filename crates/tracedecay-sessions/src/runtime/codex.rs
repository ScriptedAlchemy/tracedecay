//! Codex CLI transcript source.
//!
//! Codex appends one JSON object per line to
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` (sessions archived from the
//! picker move to a flat `~/.codex/archived_sessions/rollout-*.jsonl`). Each
//! line is `{"timestamp": "<iso8601>", "type": "<kind>", "payload": {…}}`. The
//! relevant kinds for conversation text are:
//!
//! * `session_meta` — first line; `payload.cwd`, session `id`. Real rollouts
//!   carry no `model` here (only `model_provider`); the active model is on
//!   `turn_context` lines and can change mid-session.
//! * `event_msg` with `payload.type == "user_message"` — a real user prompt
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "agent_message"` — a real assistant reply
//!   (`payload.message`).
//! * `event_msg` with `payload.type == "token_count"` — provider usage captured
//!   by the canonical observation path, not conversational message metadata.
//! * `event_msg` with `payload.type == "thread_goal_updated"` — the structured
//!   session goal and its lifecycle (`payload.goal.{objective,status,tokensUsed,
//!   timeUsedSeconds,createdAt,updatedAt}`). `TraceDecay` records each state as a
//!   compact `goal` row (objective as text, the rest in `metadata_json`) so the
//!   session's goal and whether it is still active is searchable. `status` is
//!   stored verbatim — real rollouts emit `active`/`paused`, but any future
//!   value (e.g. `completed`) is carried through unchanged rather than mapped to
//!   a fixed enum. Consecutive events that repeat the same `(objective, status)`
//!   within one parse pass are deduped; each genuine transition keeps its row.
//! * `compacted` — Codex context-compression boundary. The rollout stores the
//!   replacement history and an encrypted compaction body, so `TraceDecay` records
//!   the boundary/provenance as a summary record without claiming plaintext
//!   access to Codex's private summary.
//! * `response_item` goal context — Codex replays active thread goals as
//!   synthetic user context. `TraceDecay` indexes those as compact goal-context
//!   records so LCM can catalog the objective and budget without treating the
//!   instruction boilerplate as normal conversation.
//! * subagent rollouts — separate `rollout-*.jsonl` files whose leading
//!   `session_meta` has `thread_source == "subagent"` and parent ids in
//!   `forked_from_id` / `source.subagent.thread_spawn.parent_thread_id`.
//!
//! `response_item` entries are intentionally skipped except for Codex goal
//! context blocks: they usually carry auto-injected synthetic context and
//! duplicate the `agent_message`/`user_message` turns, so ingesting them would
//! double-count the conversation. Goal context blocks are cataloged as compact
//! `goal_context` rows because real rollouts often record them only in
//! `response_item` form. This append-only JSONL is read with the shared
//! byte-offset machinery and scoped per turn by the latest Codex cwd context.

mod context;
mod events;
mod goals;
mod meta;
mod observation;
mod records;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use context::CodexContextState;
use goals::{codex_goal_event_from_line, goal_context_from_line, goal_event_message};
use meta::session_meta;
use records::{
    compacted_summary_from_line, message_from_line, response_item_goal_context_from_line,
    response_item_tool_event_from_line, timestamp_from_record,
};

use crate::runtime::jsonl_observation_admission::{
    namespace_replacement_message_ids, preflight_and_parse_new,
};
use crate::runtime::shared::{StoredCursor, TranscriptScopeMatcher, title_from_messages};
use crate::runtime::source::{
    FileDiscoveryReport, ParsedTranscript, SessionDraft, TranscriptDiscoveryBounds,
    TranscriptIngestResult, TranscriptSource, collect_files_with_ext_bounded, stream_new_jsonl,
};
pub use meta::{CodexMeta, session_meta_from_record, turn_context_from_record};
pub use observation::{
    CodexJsonlAdmissionProgress, try_admit_codex_jsonl_observations_for_profile,
    try_admit_codex_jsonl_observations_for_profile_with_admission,
    try_admit_codex_jsonl_observations_for_profile_with_admission_and_cancellation,
    try_admit_codex_jsonl_observations_for_project,
    try_admit_codex_jsonl_observations_for_project_with_admission,
    try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation,
};

const PROVIDER: &str = "codex";
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` → date dirs add depth.
const MAX_SCAN_DEPTH: u8 = 6;

/// Codex CLI transcript locator + parser.
pub struct CodexSource {
    sessions_dir: PathBuf,
    archived_sessions_dir: PathBuf,
    user_scope: Option<UserCodexScope>,
}

struct UserCodexScope {
    session_id: Option<String>,
    registered_roots: Vec<PathBuf>,
}

impl CodexSource {
    /// Source rooted at the real `~/.codex`. Returns `None` when the
    /// home directory cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>/.codex` (used by tests).
    pub fn with_home(home: &Path) -> Self {
        let codex_home = home.join(".codex");
        Self {
            sessions_dir: codex_home.join("sessions"),
            archived_sessions_dir: codex_home.join("archived_sessions"),
            user_scope: None,
        }
    }

    /// Restricts ingestion to sessions that cannot be attributed to a registered project.
    #[must_use]
    pub fn for_user_scope(
        mut self,
        session_id: Option<String>,
        registered_roots: Vec<PathBuf>,
    ) -> Self {
        self.user_scope = Some(UserCodexScope {
            session_id,
            registered_roots,
        });
        self
    }
}

impl TranscriptSource for CodexSource {
    fn provider(&self) -> &'static str {
        PROVIDER
    }

    fn transcript_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.discover_transcript_paths(project_root, TranscriptDiscoveryBounds::default_walk())
            .paths
    }

    fn discover_transcript_paths(
        &self,
        _project_root: &Path,
        bounds: TranscriptDiscoveryBounds,
    ) -> FileDiscoveryReport {
        // Archiving a session moves its rollout out of the dated tree; both
        // locations are real transcripts and must be ingested. Share one
        // discovery budget across both trees so the second walk cannot
        // allocate past the same host-admission byte caps.
        let mut live =
            collect_files_with_ext_bounded(&self.sessions_dir, "jsonl", MAX_SCAN_DEPTH, bounds);
        if live.is_truncated() {
            return live;
        }
        let remaining_files = bounds.max_files.saturating_sub(live.paths.len());
        let remaining_bytes = bounds
            .max_discovery_bytes
            .saturating_sub(live.bytes_charged);
        if remaining_files == 0 || remaining_bytes == 0 {
            live.truncated = Some(crate::runtime::source::FileDiscoveryLimit::FileCount);
            return live;
        }
        let archived_bounds = TranscriptDiscoveryBounds {
            max_files: remaining_files,
            max_discovery_bytes: remaining_bytes,
            ..bounds
        };
        let archived = collect_files_with_ext_bounded(
            &self.archived_sessions_dir,
            "jsonl",
            MAX_SCAN_DEPTH,
            archived_bounds,
        );
        live.bytes_charged = live.bytes_charged.saturating_add(archived.bytes_charged);
        live.skipped_oversized_entries = live
            .skipped_oversized_entries
            .saturating_add(archived.skipped_oversized_entries);
        live.paths.extend(archived.paths);
        live.truncated = archived.truncated;
        live
    }

    fn parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> Option<ParsedTranscript> {
        // `session_meta` (line 1) is authoritative for session identity and the
        // initial cwd. Later context records can move one rollout between scopes.
        let meta = session_meta(path)?;
        if self
            .user_scope
            .as_ref()
            .and_then(|scope| scope.session_id.as_deref())
            .is_some_and(|session_id| session_id != meta.session_id)
        {
            return None;
        }

        let new = stream_new_jsonl(path, prev, max_new_bytes)?;
        let mut messages = Vec::new();
        // Collapses identical consecutive goal states within this parse pass:
        // `thread_goal_updated` fires on every token/time tick, so only an
        // objective- or status-change opens a new `goal` row.
        let mut last_goal_key: Option<(String, Option<String>)> = None;
        let mut structured = events::CodexStructuredState::new();
        let replayed_from_start =
            prev.position > 0 && new.lines.first().is_some_and(|line| line.offset == 0);
        let mut context_state = if prev.position > 0 && !replayed_from_start {
            CodexContextState::scan_prior(path, prev.position, &meta)
        } else {
            CodexContextState::from_meta(&meta)
        };
        let scope_matcher = TranscriptScopeMatcher::for_scope(
            project_root,
            self.user_scope
                .as_ref()
                .map(|scope| scope.registered_roots.as_slice()),
        );
        let mut last_in_scope_cwd = None;
        let mut last_in_scope_git = None;
        for line in &new.lines {
            let is_context_record = context_state.observe_context_record(&line.value, path, &meta);
            let in_scope = scope_matcher.accepts(context_state.cwd.as_deref());
            if !in_scope {
                if compacted_summary_from_line(
                    &line.value,
                    &meta,
                    context_state.model.as_deref(),
                    path,
                    line.offset,
                    context_state.compaction_depth + 1,
                )
                .is_some()
                {
                    context_state.compaction_depth += 1;
                }
                continue;
            }
            last_in_scope_cwd.clone_from(&context_state.cwd);
            last_in_scope_git.clone_from(&context_state.git);
            // Non-consuming: harvest session-level policy/effort/rate-limit
            // summary before the line is routed to its owning handler below.
            structured.observe_summary(&line.value);
            if is_context_record {
                continue;
            }
            if let Some(rows) = structured.event_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                for mut message in rows {
                    context::annotate_message(
                        &mut message,
                        context_state.cwd.as_deref(),
                        context_state.git.as_ref(),
                    );
                    messages.push(message);
                }
                continue;
            }
            if let Some(event) = codex_goal_event_from_line(&line.value) {
                let key = event.dedup_key();
                if last_goal_key.as_ref() == Some(&key) {
                    continue;
                }
                last_goal_key = Some(key);
                let mut message = goal_event_message(
                    &meta,
                    context_state.model.as_deref(),
                    path,
                    line.offset,
                    timestamp_from_record(&line.value),
                    &event,
                );
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
                continue;
            }
            if let Some(mut message) = response_item_goal_context_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
                continue;
            }
            if let Some(mut message) = response_item_tool_event_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
                continue;
            }
            if let Some(mut message) = compacted_summary_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
                context_state.compaction_depth + 1,
            ) {
                context_state.compaction_depth += 1;
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
                continue;
            }
            if let Some(mut message) = goal_context_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
                continue;
            }
            if let Some(mut message) = message_from_line(
                &line.value,
                &meta,
                context_state.model.as_deref(),
                path,
                line.offset,
            ) {
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
            }
        }
        // Emit any `exec_command` calls whose paired output never arrived in
        // this pass so the tool call is not silently dropped.
        for mut message in structured.flush_pending(&meta, path) {
            context::annotate_message(
                &mut message,
                last_in_scope_cwd.as_deref(),
                last_in_scope_git.as_ref(),
            );
            messages.push(message);
        }

        // A truncate-and-rewrite can reuse every byte offset from the previous
        // file generation. Legacy projection keys are offset-based, so keep
        // replacement rows distinct instead of overwriting retained history.
        if replayed_from_start {
            namespace_replacement_message_ids(&mut messages, new.new_cursor.file_id);
        }

        let project = self.user_scope.as_ref().map_or_else(
            || project_root.to_string_lossy().to_string(),
            |_| "user".to_string(),
        );
        let draft = SessionDraft {
            session_id: meta.session_id.clone(),
            project_key: project.clone(),
            project_path: project,
            title: title_from_messages(&messages),
            // The summary is session-wide and may include evidence observed
            // after Codex changed cwd into a registered project. User scope
            // stores only the filtered message rows, never that mixed summary.
            metadata_json: context::session_metadata_json(
                &meta,
                self.user_scope.is_none().then_some(&structured.summary),
            ),
            parent_session_id: meta.parent_session_id.clone(),
            is_subagent: meta.is_subagent,
            agent_id: meta.agent_id.clone(),
            parent_tool_use_id: None,
        };

        Some(ParsedTranscript {
            draft,
            messages,
            new_cursor: new.new_cursor,
        })
    }

    fn try_parse_new(
        &self,
        path: &Path,
        prev: StoredCursor,
        project_root: &Path,
        max_new_bytes: Option<u64>,
    ) -> TranscriptIngestResult<Option<ParsedTranscript>> {
        preflight_and_parse_new(PROVIDER, path, prev, max_new_bytes, || {
            self.parse_new(path, prev, project_root, max_new_bytes)
        })
    }
}
