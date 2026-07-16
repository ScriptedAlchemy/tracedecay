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
//! * `event_msg` with `payload.type == "token_count"` — per-API-call usage; a
//!   turn's tool loop emits one per call, so a turn's true cost is the *sum*
//!   (see [`CodexTurnUsage`]).
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

use std::fmt::Write as _;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1,
    CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1,
    CanonicalWorkflowEvidenceKindV1, CanonicalWorkflowSemanticKindV1, ObservationId,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceIdentityV1, ProjectId,
    ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::accounting::parser::parse_timestamp;
use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::global_db::GlobalDb;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::SessionMessageRecord;
use crate::sessions::jsonl_observation_admission::{
    JsonlFrameAdmission, JsonlObservationAdmissionRequest, PersistedCursorUpdate,
    admit_jsonl_observations,
};
use crate::sessions::shared::{
    StoredCursor, append_tool_calls_metadata, content_storage_text_and_tools,
    path_belongs_to_project, title_from_messages,
};
use crate::sessions::source::{
    FileDiscoveryReport, MAX_JSONL_RECORD_BYTES, ParsedTranscript, RawJsonlFrame,
    RawJsonlFrameReader, SessionDraft, TranscriptDiscoveryBounds, TranscriptIngestError,
    TranscriptIngestResult, TranscriptSource, collect_files_with_ext_bounded,
    preflight_strict_jsonl, stream_new_jsonl,
};
use context::CodexContextState;

const PROVIDER: &str = "codex";
/// `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` → date dirs add depth.
const MAX_SCAN_DEPTH: u8 = 6;
/// Threshold above which a tool call's arguments / a tool output is flagged as
/// truncated in metadata. Raw tool-call arguments and tool outputs are never
/// embedded in the FTS-searchable message text (they can carry secrets); only
/// byte counts and this truncation flag are recorded. The lossless body already
/// lives in the Codex rollout itself, recoverable via `source_path`/
/// `source_offset`.
const TOOL_EVENT_PREVIEW_BYTES: usize = 2000;

/// Session metadata read from a rollout's leading `session_meta` line.
struct CodexMeta {
    cwd: PathBuf,
    session_id: String,
    model: Option<String>,
    git: Option<Value>,
    parent_session_id: Option<String>,
    is_subagent: bool,
    agent_id: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    thread_source: Option<String>,
}

struct CodexMetaWithProvenance {
    meta: CodexMeta,
    native_thread_id: Option<String>,
}

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
        let home = crate::sessions::home_dir()?;
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
        // allocate past the same PR6 caps.
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
            live.truncated = Some(crate::sessions::source::FileDiscoveryLimit::FileCount);
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
        let mut turn_usage = CodexTurnUsage::default();
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
        let mut last_in_scope_cwd = None;
        let mut last_in_scope_git = None;
        for line in &new.lines {
            let is_context_record = context_state.observe_context_record(&line.value, path, &meta);
            let in_scope = self.user_scope.as_ref().map_or_else(
                || {
                    context_state
                        .cwd
                        .as_deref()
                        .is_some_and(|cwd| path_belongs_to_project(cwd, project_root))
                },
                |scope| {
                    context_state.cwd.as_deref().is_none_or(|cwd| {
                        !scope
                            .registered_roots
                            .iter()
                            .any(|root| path_belongs_to_project(cwd, root))
                    })
                },
            );
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
            if turn_usage.observe(&line.value) {
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
                flush_turn_usage(&mut messages, &mut turn_usage);
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
                // A new user prompt closes the previous turn: attach that
                // turn's summed API-call usage to its assistant reply.
                if message.role == "user" {
                    flush_turn_usage(&mut messages, &mut turn_usage);
                }
                context::annotate_message(
                    &mut message,
                    context_state.cwd.as_deref(),
                    context_state.git.as_ref(),
                );
                messages.push(message);
            }
        }
        // The final turn's trailing token_count(s) arrive after its
        // agent_message; flush them onto it.
        flush_turn_usage(&mut messages, &mut turn_usage);
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
        preflight_strict_jsonl(PROVIDER, path, prev, max_new_bytes)?;
        Ok(self.parse_new(path, prev, project_root, max_new_bytes))
    }
}

fn namespace_replacement_message_ids(messages: &mut [SessionMessageRecord], generation: u64) {
    for message in messages {
        message.message_id = format!("{}:generation:{generation}", message.message_id);
    }
}

const CODEX_OBSERVATION_RETENTION: &str = "retention.provider-observation";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CodexJsonlAdmissionProgress {
    pub bytes_consumed: u64,
    pub source_deferred: bool,
}

/// Admit a Codex rollout for one exact project identity.
///
/// The scheduler supplies the already-resolved project id; each complete record
/// is routed by the rollout's current Codex cwd, including context reconstructed
/// before a resumed byte cursor.
pub async fn try_admit_codex_jsonl_observations_for_project(
    path: &Path,
    db: &GlobalDb,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        db,
        CodexObservationAdmission::Project {
            root: project_root,
            project_id,
        },
        max_new_bytes,
    )
    .await
}

/// Admit Codex records that are not attributable to any registered project.
///
/// A scheduler may constrain this pass to one session while it catches up a
/// profile-owned rollout.
pub async fn try_admit_codex_jsonl_observations_for_profile(
    path: &Path,
    db: &GlobalDb,
    session_id: Option<&str>,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    try_admit_codex_jsonl_observations(
        path,
        db,
        CodexObservationAdmission::Profile {
            session_id,
            registered_roots,
        },
        max_new_bytes,
    )
    .await
}

enum CodexObservationAdmission<'a> {
    Project {
        root: &'a Path,
        project_id: ProjectId,
    },
    Profile {
        session_id: Option<&'a str>,
        registered_roots: &'a [PathBuf],
    },
}

impl CodexObservationAdmission<'_> {
    fn scope(&self) -> ObservationScopeV1 {
        match self {
            Self::Project { project_id, .. } => ObservationScopeV1::Project {
                project_id: project_id.clone(),
            },
            Self::Profile { .. } => ObservationScopeV1::Profile,
        }
    }

    fn accepts(&self, cwd: Option<&Path>) -> bool {
        match self {
            Self::Project { root, .. } => cwd.is_some_and(|cwd| path_belongs_to_project(cwd, root)),
            Self::Profile {
                registered_roots, ..
            } => cwd.is_none_or(|cwd| {
                !registered_roots
                    .iter()
                    .any(|root| path_belongs_to_project(cwd, root))
            }),
        }
    }

    fn accepts_session(&self, session_id: &str) -> bool {
        !matches!(self, Self::Profile { session_id: Some(expected), .. } if *expected != session_id)
    }
}

async fn try_admit_codex_jsonl_observations(
    path: &Path,
    db: &GlobalDb,
    admission_scope: CodexObservationAdmission<'_>,
    max_new_bytes: Option<u64>,
) -> TranscriptIngestResult<CodexJsonlAdmissionProgress> {
    let parsed_meta = session_meta_with_provenance(path).ok_or_else(|| {
        TranscriptIngestError::InvalidSourceIdentity {
            provider: PROVIDER,
            path: path.to_path_buf(),
        }
    })?;
    let native_thread_id = parsed_meta.native_thread_id;
    let meta = parsed_meta.meta;
    if !admission_scope.accepts_session(&meta.session_id) {
        return Ok(CodexJsonlAdmissionProgress::default());
    }
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)?,
        SessionId::new(meta.session_id.clone())?,
    )?;
    let scope = admission_scope.scope();
    let authorities = match &scope {
        ObservationScopeV1::Profile => HostAdmissionAuthorities::for_profile(db),
        ObservationScopeV1::Project { project_id } => {
            HostAdmissionAuthorities::for_project(db, project_id.clone())
        }
    };
    let admission = HostAdmissionFacade::new(authorities);
    let request = JsonlObservationAdmissionRequest::new(
        PROVIDER,
        path,
        &admission,
        source,
        scope,
        RetentionClass::new(CODEX_OBSERVATION_RETENTION)?,
    )
    .with_max_new_bytes(max_new_bytes)
    .with_persisted_cursor_update(PersistedCursorUpdate::Replace);
    let progress = admit_jsonl_observations(
        request,
        |scan| {
            if scan.resumed {
                CodexContextState::scan_prior(path, scan.start_offset, &meta)
            } else {
                CodexContextState::from_meta(&meta)
            }
        },
        |context_state, bytes, range, _| {
            let mut stable_record_id = None;
            let mut non_durable_reason = None;
            let parsed = parse_normalized_observation_record_v1(
                bytes,
                range,
                ObservationOrderingDomainV1::FileBytes,
                |native| {
                    context_state.observe_context_record(&native, path, &meta);
                    if !admission_scope.accepts(context_state.cwd.as_deref()) {
                        non_durable_reason = Some(ObservationCoverageReason::OutOfScope);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                    if !codex_observation_record_supported(&native) {
                        non_durable_reason = Some(ObservationCoverageReason::UnsupportedFact);
                        return Err(ObservationRecordParseErrorV1::NormalizationFailed);
                    }
                    let record_id = codex_native_record_id(&meta.session_id, &native)
                        .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?;
                    let envelope = normalize_codex_observation(
                        &native,
                        &meta.session_id,
                        native_thread_id.as_deref(),
                        record_id.clone(),
                        range,
                    )?;
                    stable_record_id = Some(record_id);
                    Ok(envelope)
                },
            );
            match parsed {
                Ok(parsed) => Ok(JsonlFrameAdmission::durable(
                    parsed,
                    stable_record_id
                        .ok_or(TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?,
                )),
                Err(_) => Ok(JsonlFrameAdmission::non_durable(
                    non_durable_reason.unwrap_or(ObservationCoverageReason::MalformedFrame),
                )),
            }
        },
    )
    .await?;
    Ok(CodexJsonlAdmissionProgress {
        bytes_consumed: progress.bytes_consumed,
        source_deferred: progress.source_deferred,
    })
}

fn codex_observation_record_supported(value: &Value) -> bool {
    matches!(
        value.get("type").and_then(Value::as_str),
        Some(
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "compacted"
                | "inter_agent_communication"
        )
    )
}

fn normalize_codex_observation(
    native: &Value,
    session_id: &str,
    native_thread_id: Option<&str>,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native_kind = native
        .get("type")
        .and_then(Value::as_str)
        .ok_or(ObservationRecordParseErrorV1::NormalizationFailed)?;
    let payload = native.get("payload").unwrap_or(native);
    let timestamp = timestamp_from_record(native);
    let mut relations = CanonicalObservationRelationsV1::new(
        SessionId::new(session_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    );
    // `session_meta.payload.id` is the native thread id. The session parser may
    // fall back to a filename for legacy source identity, which is not enough
    // evidence to populate this relation.
    if let Some(thread_id) = native_thread_id.and_then(observation_id_from_native) {
        relations = relations.with_thread_id(thread_id);
    }
    if let Some(turn_id) = codex_native_turn_id(payload) {
        relations = relations.with_turn_id(turn_id);
    }
    if matches!(
        native_kind,
        "event_msg" | "response_item" | "compacted" | "inter_agent_communication"
    ) {
        relations = relations.with_message_id(stable_record_id.clone());
    }
    if native_kind == "session_meta" {
        relations = append_codex_session_meta_agent_relations(relations, payload, native_thread_id);
    }

    let mut facts = Vec::new();
    match native_kind {
        "session_meta" => {
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::SessionStart,
            });
            append_codex_git_facts(payload, &mut facts);
            if payload.pointer("/source/subagent").is_some()
                || payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
            {
                facts.push(CanonicalObservationFactV1::Workflow {
                    evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                    reference: None,
                    content: None,
                });
            }
        }
        "turn_context" => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "turn_context".to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        "event_msg" => append_codex_event_facts(payload, timestamp, &mut facts),
        "response_item" => {
            append_codex_response_item_facts(payload, timestamp, &stable_record_id, &mut facts);
        }
        "compacted" => {
            facts.push(CanonicalObservationFactV1::Compaction {
                summary: payload.get("message").cloned(),
                input_tokens: canonical_u64(payload.get("input_tokens")),
                output_tokens: canonical_u64(payload.get("output_tokens")),
            });
            facts.push(CanonicalObservationFactV1::Boundary {
                boundary_kind: CanonicalBoundaryKindV1::CompactionBoundary,
            });
        }
        "inter_agent_communication" => {
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Subagent,
                reference: None,
                content: payload
                    .get("message")
                    .or_else(|| payload.get("content"))
                    .cloned(),
            });
        }
        _ => {}
    }
    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: native_kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::FileBytes, range);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        native_kind,
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn observation_id_from_native(value: &str) -> Option<ObservationId> {
    ObservationId::new(value).ok()
}

fn codex_native_turn_id(payload: &Value) -> Option<ObservationId> {
    string_field(payload, "turn_id")
        .or_else(|| {
            nested_string_field(
                payload,
                "/internal_chat_message_metadata_passthrough/turn_id",
            )
        })
        .and_then(|turn_id| observation_id_from_native(&turn_id))
}

fn append_codex_session_meta_agent_relations(
    mut relations: CanonicalObservationRelationsV1,
    payload: &Value,
    native_thread_id: Option<&str>,
) -> CanonicalObservationRelationsV1 {
    let parent_session_id = string_field(payload, "forked_from_id")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/parent_thread_id"));
    let thread_source = string_field(payload, "thread_source");
    let is_subagent = thread_source.as_deref() == Some("subagent")
        || parent_session_id.is_some()
        || payload.pointer("/source/subagent").is_some();
    if !is_subagent {
        return relations;
    }
    // Codex does not expose a separate stable agent id in session_meta. The
    // subagent's native session/thread id is stable; nickname and role are
    // mutable descriptive labels and must never participate in identity.
    if let Some(agent_id) = native_thread_id.and_then(observation_id_from_native) {
        relations = relations.with_agent_id(agent_id);
    }
    if let Some(parent_agent_id) = parent_session_id.and_then(|id| observation_id_from_native(&id))
    {
        relations = relations.with_parent_agent_id(parent_agent_id);
    }
    relations
}

fn append_codex_event_facts(
    payload: &Value,
    timestamp: Option<i64>,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    match payload.get("type").and_then(Value::as_str) {
        Some("user_message" | "agent_message") => {
            let role = if payload.get("type").and_then(Value::as_str) == Some("user_message") {
                CanonicalMessageRoleV1::User
            } else {
                CanonicalMessageRoleV1::Assistant
            };
            if let Some(content) = payload.get("message").cloned() {
                facts.push(CanonicalObservationFactV1::Message {
                    role,
                    content,
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp,
                });
            }
        }
        Some("token_count") => {
            let usage = payload
                .get("info")
                .and_then(|info| {
                    info.get("last_token_usage")
                        .or_else(|| info.get("total_token_usage"))
                })
                .unwrap_or(payload);
            let input = canonical_u64(usage.get("input_tokens"));
            let cache_read = canonical_u64(
                usage
                    .get("cached_input_tokens")
                    .or_else(|| usage.get("cache_read_input_tokens")),
            );
            facts.push(CanonicalObservationFactV1::Usage {
                input_tokens: input.map(|input| input.saturating_sub(cache_read.unwrap_or(0))),
                output_tokens: canonical_u64(
                    usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens")),
                ),
                cache_read_tokens: cache_read,
                cache_write_tokens: canonical_u64(usage.get("cache_write_input_tokens")),
                reasoning_tokens: canonical_u64(
                    usage
                        .get("reasoning_output_tokens")
                        .or_else(|| usage.get("reasoning_tokens")),
                ),
            });
        }
        Some("thread_goal_updated") => {
            if !append_codex_thread_goal_lifecycle_fact(payload, facts) {
                facts.push(CanonicalObservationFactV1::Unknown {
                    native_kind: "thread_goal_updated".to_string(),
                    state: CanonicalUnknownStateV1::Malformed,
                });
            }
        }
        Some(event @ ("task_started" | "task_complete" | "turn_aborted")) => {
            append_codex_turn_lifecycle_fact(payload, event, facts);
        }
        Some(kind) => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
        None => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "event_msg".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        }),
    }
}

fn append_codex_thread_goal_lifecycle_fact(
    payload: &Value,
    facts: &mut Vec<CanonicalObservationFactV1>,
) -> bool {
    // Real Codex shape (see goal_event_line / write_codex_rollout_with_goal_events):
    // payload.goal.{objective,status,threadId,tokensUsed,timeUsedSeconds,createdAt,updatedAt}
    // plus optional payload.threadId. Never invent fields absent from that nest.
    let Some(goal) = payload.get("goal") else {
        return false;
    };
    let Some(objective) = goal
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
    else {
        return false;
    };
    let _ = objective;
    let status = goal
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(str::to_string);
    let provider_reference = goal
        .get("threadId")
        .and_then(Value::as_str)
        .or_else(|| payload.get("threadId").and_then(Value::as_str))
        .filter(|thread_id| !thread_id.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(goal.clone()),
    });
    true
}

fn append_codex_turn_lifecycle_fact(
    payload: &Value,
    event: &str,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    // Exact singular task_complete / task_started / turn_aborted only
    // (write_codex_rollout_with_structured_events, task_events_become_turn_boundary_rows).
    // Do not index last_agent_message as content — classic turn rows exclude it.
    let provider_reference = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .filter(|turn_id| !turn_id.is_empty())
        .map(str::to_string);
    // Keep native `reason` in content only — do not promote it to status
    // (no fixture evidence that abort reason is a workflow status vocabulary).
    let mut content = serde_json::Map::new();
    content.insert("type".to_string(), Value::String(event.to_string()));
    for key in [
        "turn_id",
        "started_at",
        "completed_at",
        "duration_ms",
        "time_to_first_token_ms",
        "model_context_window",
        "reason",
    ] {
        if let Some(value) = payload.get(key) {
            content.insert(key.to_string(), value.clone());
        }
    }
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: Some(event.to_string()),
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(Value::Object(content)),
    });
}

fn append_codex_update_plan_lifecycle_fact(
    payload: &Value,
    facts: &mut Vec<CanonicalObservationFactV1>,
) -> bool {
    // Real Codex shape: response_item function_call name=update_plan with
    // arguments JSON string/object {explanation?, plan:[{step,status}]} + call_id.
    let Some(arguments) = events::parse_arguments(payload.get("arguments")) else {
        return false;
    };
    if arguments.get("plan").and_then(Value::as_array).is_none() {
        return false;
    }
    let provider_reference = payload
        .get("call_id")
        .and_then(Value::as_str)
        .filter(|call_id| !call_id.is_empty())
        .map(str::to_string);
    facts.push(CanonicalObservationFactV1::WorkflowLifecycle {
        semantic_kind: CanonicalWorkflowSemanticKindV1::Plan,
        provider_reference,
        item_id: None,
        parent_reference: None,
        list_reference: None,
        state: None,
        status: None,
        item_order: None,
        revision: None,
        event_sequence: None,
        content: Some(arguments),
    });
    true
}

fn append_codex_response_item_facts(
    payload: &Value,
    timestamp: Option<i64>,
    stable_record_id: &ObservationId,
    facts: &mut Vec<CanonicalObservationFactV1>,
) {
    let Some(item_kind) = payload.get("type").and_then(Value::as_str) else {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "response_item".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
        return;
    };
    match item_kind {
        "message" => {
            if let Some(content) = payload.get("content").cloned() {
                facts.push(CanonicalObservationFactV1::Message {
                    role: canonical_message_role(payload.get("role").and_then(Value::as_str)),
                    content,
                    model: payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    timestamp,
                });
                // Goal-context response_item messages stay Message-only.
                // WorkflowLifecycle Goal is reserved for nested thread_goal_updated
                // (write_codex_rollout_with_goal_events), not synthetic goal-context text.
            }
        }
        "function_call" | "custom_tool_call" | "tool_search_call" | "web_search_call" => {
            let invocation_id = canonical_native_observation_id(
                payload
                    .get("call_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str),
                stable_record_id,
            );
            let name = response_item_tool_name(payload, item_kind)
                .unwrap_or_else(|| item_kind.to_string());
            facts.push(CanonicalObservationFactV1::ToolInvocation {
                invocation_id,
                name: name.clone(),
                arguments: Value::Null,
            });
            if item_kind == "function_call" && name == "update_plan" {
                let _ = append_codex_update_plan_lifecycle_fact(payload, facts);
            }
        }
        "function_call_output" | "custom_tool_call_output" => {
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: Some(canonical_native_observation_id(
                    payload.get("call_id").and_then(Value::as_str),
                    stable_record_id,
                )),
                content: Value::Null,
                success: payload
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|status| matches!(status, "completed" | "success" | "succeeded")),
            });
        }
        "reasoning" => {
            let summary = payload.get("summary").filter(|summary| !summary.is_null());
            let encrypted = payload
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty());
            let (visibility, content) = if let Some(summary) = summary {
                (
                    CanonicalReasoningVisibilityV1::Visible,
                    Some(summary.clone()),
                )
            } else if encrypted {
                (CanonicalReasoningVisibilityV1::Redacted, None)
            } else {
                (CanonicalReasoningVisibilityV1::Unavailable, None)
            };
            facts.push(CanonicalObservationFactV1::Reasoning {
                visibility,
                content,
            });
        }
        kind => facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: kind.to_string(),
            state: CanonicalUnknownStateV1::Unsupported,
        }),
    }
}

fn append_codex_git_facts(payload: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    let Some(git) = payload.get("git") else {
        return;
    };
    if let Some(branch) = git
        .get("branch")
        .or_else(|| git.get("current_branch"))
        .and_then(Value::as_str)
        .filter(|branch| !branch.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Branch,
            reference: Some(branch.to_string()),
            content: None,
        });
    }
    if let Some(commit) = git
        .get("commit_hash")
        .or_else(|| git.get("commit"))
        .or_else(|| git.get("head"))
        .and_then(Value::as_str)
        .filter(|commit| !commit.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Commit,
            reference: Some(commit.to_string()),
            content: None,
        });
    }
}

fn canonical_message_role(role: Option<&str>) -> CanonicalMessageRoleV1 {
    match role {
        Some("user") => CanonicalMessageRoleV1::User,
        Some("assistant") => CanonicalMessageRoleV1::Assistant,
        Some("system" | "developer") => CanonicalMessageRoleV1::System,
        Some("tool") => CanonicalMessageRoleV1::Tool,
        _ => CanonicalMessageRoleV1::Unknown,
    }
}

fn canonical_native_observation_id(
    native_id: Option<&str>,
    fallback: &ObservationId,
) -> ObservationId {
    native_id
        .and_then(|native_id| ObservationId::new(native_id).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

fn codex_native_record_id(
    session_id: &str,
    value: &Value,
) -> TranscriptIngestResult<ObservationId> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.provider-native-record.v1\0codex\0");
    hasher.update(session_id.as_bytes());
    hasher.update([0]);
    hasher.update(
        serde_json::to_vec(value)
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?,
    );
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    }
    Ok(ObservationId::new(format!(
        "codex.native.sha256:{encoded}"
    ))?)
}

/// Read the leading `session_meta` line of a rollout for cwd/session-id/model.
fn session_meta(path: &Path) -> Option<CodexMeta> {
    session_meta_with_provenance(path).map(|parsed| parsed.meta)
}

fn session_meta_with_provenance(path: &Path) -> Option<CodexMetaWithProvenance> {
    let file = std::fs::File::open(path).ok()?;
    let mut frames = RawJsonlFrameReader::new(BufReader::new(file), MAX_JSONL_RECORD_BYTES);
    for _ in 0..4 {
        match frames.next_frame().ok()? {
            RawJsonlFrame::Eof => break,
            RawJsonlFrame::Complete { .. } | RawJsonlFrame::Partial { .. } => {
                let Ok(value) = serde_json::from_slice::<Value>(frames.record()) else {
                    continue;
                };
                if let Some(meta) = session_meta_with_provenance_from_record(&value, path) {
                    return Some(meta);
                }
            }
            RawJsonlFrame::Oversized { .. } | RawJsonlFrame::BudgetExhausted { .. } => {}
        }
    }
    None
}

fn session_meta_from_record(record: &Value, path: &Path) -> Option<CodexMeta> {
    session_meta_with_provenance_from_record(record, path).map(|parsed| parsed.meta)
}

fn session_meta_with_provenance_from_record(
    record: &Value,
    path: &Path,
) -> Option<CodexMetaWithProvenance> {
    if record.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = record.get("payload").unwrap_or(record);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)?;
    let native_thread_id = payload
        .get("id")
        .or_else(|| payload.get("session_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let session_id = native_thread_id.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    // Note: real rollouts have no `model` in session_meta — only
    // `model_provider` (e.g. "openai"), which is *not* a model and must
    // not be stored as one; `turn_context` lines carry the actual model.
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string);
    let git = payload.get("git").filter(|git| git.is_object()).cloned();
    let parent_session_id = string_field(payload, "forked_from_id")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/parent_thread_id"));
    let thread_source = string_field(payload, "thread_source");
    let agent_nickname = string_field(payload, "agent_nickname")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/agent_nickname"));
    let agent_role = string_field(payload, "agent_role")
        .or_else(|| nested_string_field(payload, "/source/subagent/thread_spawn/agent_role"));
    let is_subagent = thread_source.as_deref() == Some("subagent")
        || parent_session_id.is_some()
        || payload.pointer("/source/subagent").is_some();
    let agent_id = is_subagent.then(|| {
        agent_nickname
            .clone()
            .or_else(|| agent_role.clone())
            .unwrap_or_else(|| session_id.clone())
    });
    Some(CodexMetaWithProvenance {
        meta: CodexMeta {
            cwd,
            session_id,
            model,
            git,
            parent_session_id,
            is_subagent,
            agent_id,
            agent_nickname,
            agent_role,
            thread_source,
        },
        native_thread_id,
    })
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn nested_string_field(payload: &Value, pointer: &str) -> Option<String> {
    payload
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

struct CodexTurnContext {
    model: Option<String>,
    cwd: Option<PathBuf>,
}

/// Context recorded on a `turn_context` line. Real rollouts use this for the
/// active model and current cwd; both can change mid-session.
fn turn_context_from_record(record: &Value) -> Option<CodexTurnContext> {
    if record.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    let payload = record.get("payload").unwrap_or(record);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(str::to_string);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from);
    Some(CodexTurnContext { model, cwd })
}

/// Map one rollout line to a provider-neutral message, or `None` for non-message
/// events (`response_item`, tool calls, token counts, …).
fn message_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = record.get("payload")?;
    let role = match payload.get("type").and_then(Value::as_str)? {
        "user_message" => "user",
        "agent_message" => "assistant",
        _ => return None,
    };
    let content = payload.get("message")?;
    let (text, tool_names) = content_storage_text_and_tools(content, payload.get("tool_calls"));
    if text.trim().is_empty() {
        return None;
    }

    let timestamp = timestamp_from_record(record);
    if let Some(goal_context) = codex_goal_context_from_text(&text) {
        return Some(goal_context_message(
            meta,
            model,
            path,
            offset,
            timestamp,
            &goal_context,
            &message_metadata(payload, Some(&goal_context)),
        ));
    }

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: role.to_string(),
        timestamp,
        ordinal: offset,
        text,
        kind: Some("message".to_string()),
        model: model.map(str::to_string),
        tool_names: (!tool_names.is_empty()).then(|| tool_names.join(",")),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&message_metadata(payload, None)).ok(),
    })
}

fn response_item_goal_context_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let text = collect_response_item_text(payload.get("content").unwrap_or(payload));
    let goal_context = codex_goal_context_from_text(&text)?;
    let mut metadata = message_metadata(payload, Some(&goal_context));
    if let Value::Object(map) = &mut metadata {
        map.insert(
            "source_event".to_string(),
            Value::String("response_item".to_string()),
        );
        if let Some(role) = payload.get("role").and_then(Value::as_str) {
            map.insert("source_role".to_string(), Value::String(role.to_string()));
        }
    }

    Some(goal_context_message(
        meta,
        model,
        path,
        offset,
        timestamp_from_record(record),
        &goal_context,
        &metadata,
    ))
}

fn response_item_tool_event_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = record.get("payload")?;
    let response_item_type = payload.get("type").and_then(Value::as_str)?;
    // Serialize the output payload once and share it with both helpers below.
    let output = payload.get("output").map(compact_response_item_value);
    let (role, text, metadata) = match response_item_type {
        "function_call" | "custom_tool_call" | "tool_search_call" | "web_search_call" => {
            let tool_name = response_item_tool_name(payload, response_item_type);
            let text =
                response_item_tool_call_text(response_item_type, tool_name.as_deref(), payload);
            (
                "tool",
                text,
                response_item_tool_metadata(
                    response_item_type,
                    payload,
                    tool_name,
                    output.as_deref(),
                ),
            )
        }
        "function_call_output" | "custom_tool_call_output" => {
            let text = response_item_tool_output_text(payload, output.as_deref())?;
            (
                "tool",
                text,
                response_item_tool_metadata(response_item_type, payload, None, output.as_deref()),
            )
        }
        "reasoning" => {
            let text = response_item_reasoning_summary_text(payload)?;
            (
                "assistant",
                text,
                response_item_tool_metadata(response_item_type, payload, None, output.as_deref()),
            )
        }
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: role.to_string(),
        timestamp: timestamp_from_record(record),
        ordinal: offset,
        text,
        kind: Some(if response_item_type == "reasoning" {
            "reasoning".to_string()
        } else {
            "tool_event".to_string()
        }),
        model: model.map(str::to_string),
        tool_names: response_item_tool_name(payload, response_item_type),
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&metadata).ok(),
    })
}

fn response_item_tool_name(payload: &Value, response_item_type: &str) -> Option<String> {
    payload
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| match response_item_type {
            "tool_search_call" => Some("tool_search".to_string()),
            "web_search_call" => Some("web_search".to_string()),
            _ => None,
        })
}

fn response_item_tool_call_text(
    response_item_type: &str,
    tool_name: Option<&str>,
    payload: &Value,
) -> String {
    let label = tool_name.unwrap_or(response_item_type);
    let mut parts = vec![format!("Codex tool call: {label}")];
    if let Some(namespace) = payload.get("namespace").and_then(Value::as_str) {
        parts.push(format!("namespace: {namespace}"));
    }
    if let Some(call_id) = payload.get("call_id").and_then(Value::as_str) {
        parts.push(format!("call_id: {call_id}"));
    }
    // Never embed raw arguments in the FTS-searchable text — they can carry
    // secrets (tokens, credentials, private paths). Record only the byte count;
    // the lossless arguments remain in the rollout at `source_offset`.
    if let Some(arguments_bytes) = response_item_arguments_bytes(payload) {
        parts.push(format!("arguments_bytes: {arguments_bytes}"));
    }
    parts.join("\n")
}

/// Byte length of a tool call's arguments payload (`arguments`/`input`/`action`,
/// whichever is present) after compact serialization. Returns `None` when the
/// item carries no argument payload.
fn response_item_arguments_bytes(payload: &Value) -> Option<usize> {
    payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .map(compact_response_item_value)
        .map(|arguments| arguments.len())
}

fn response_item_tool_output_text(payload: &Value, output: Option<&str>) -> Option<String> {
    let call_id = payload
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let output = output?;
    let output_bytes = output.len();
    // Record only the byte count — the raw tool output can carry secrets and
    // must not land in the FTS-searchable text. The full body stays in the
    // rollout, recoverable via `source_path`/`source_offset`.
    Some(format!(
        "Codex tool output: {call_id}\noutput_bytes: {output_bytes}"
    ))
}

fn response_item_reasoning_summary_text(payload: &Value) -> Option<String> {
    let summary = payload.get("summary")?;
    let text = collect_response_item_text(summary);
    (!text.trim().is_empty()).then(|| format!("Codex reasoning summary:\n{text}"))
}

fn compact_response_item_value(value: &Value) -> String {
    value.as_str().map_or_else(
        || serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
        str::to_string,
    )
}

fn response_item_tool_metadata(
    response_item_type: &str,
    payload: &Value,
    tool_name: Option<String>,
    output: Option<&str>,
) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_response_item".to_string()),
    );
    metadata.insert(
        "response_item_type".to_string(),
        Value::String(response_item_type.to_string()),
    );
    for key in ["call_id", "id", "status", "namespace"] {
        if let Some(value) = payload.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    if let Some(tool_name) = tool_name {
        metadata.insert("tool_name".to_string(), Value::String(tool_name));
    }
    if response_item_type == "reasoning" {
        metadata.insert(
            "reasoning_visibility".to_string(),
            Value::String("provider_exposed".to_string()),
        );
        metadata.insert(
            "reasoning_retention".to_string(),
            Value::String("provider_exposed".to_string()),
        );
    }
    // Byte counts + truncation flags only — never the raw argument/output bytes.
    if let Some(arguments_bytes) = response_item_arguments_bytes(payload) {
        metadata.insert(
            "arguments_bytes".to_string(),
            Value::from(arguments_bytes as i64),
        );
        metadata.insert(
            "arguments_truncated".to_string(),
            Value::Bool(arguments_bytes > TOOL_EVENT_PREVIEW_BYTES),
        );
    }
    if let Some(output) = output {
        metadata.insert("output_bytes".to_string(), Value::from(output.len() as i64));
        metadata.insert(
            "output_truncated".to_string(),
            Value::Bool(output.len() > TOOL_EVENT_PREVIEW_BYTES),
        );
    }
    Value::Object(metadata)
}

fn goal_context_message(
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    timestamp: Option<i64>,
    goal_context: &CodexGoalContext,
    metadata: &Value,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "system".to_string(),
        timestamp,
        ordinal: offset,
        text: goal_context.storage_text(),
        kind: Some("goal_context".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&metadata).ok(),
    }
}

/// Codex's structured session goal, parsed from a `thread_goal_updated`
/// `event_msg`. `status` is stored verbatim; the parser deliberately does not
/// map it to a fixed enum so an unrecognized future value survives round-trip.
struct CodexGoalEvent {
    objective: String,
    status: Option<String>,
    thread_id: Option<String>,
    tokens_used: Option<i64>,
    time_used_seconds: Option<i64>,
    created_at: Option<i64>,
    updated_at: Option<i64>,
}

impl CodexGoalEvent {
    /// Key used to collapse identical consecutive lifecycle states within one
    /// parse pass. Token/time drift on the same `(objective, status)` is
    /// progress within a state, not a transition, so it does not open a new row.
    fn dedup_key(&self) -> (String, Option<String>) {
        (self.objective.clone(), self.status.clone())
    }

    fn metadata(&self) -> Value {
        let mut goal = serde_json::Map::new();
        goal.insert(
            "source".to_string(),
            Value::String("codex_thread_goal".to_string()),
        );
        goal.insert(
            "source_event".to_string(),
            Value::String("thread_goal_updated".to_string()),
        );
        goal.insert(
            "objective".to_string(),
            Value::String(self.objective.clone()),
        );
        if let Some(status) = &self.status {
            goal.insert("status".to_string(), Value::String(status.clone()));
        }
        if let Some(thread_id) = &self.thread_id {
            goal.insert("thread_id".to_string(), Value::String(thread_id.clone()));
        }
        if let Some(tokens_used) = self.tokens_used {
            goal.insert("tokens_used".to_string(), Value::from(tokens_used));
        }
        if let Some(time_used_seconds) = self.time_used_seconds {
            goal.insert(
                "time_used_seconds".to_string(),
                Value::from(time_used_seconds),
            );
        }
        if let Some(created_at) = self.created_at {
            goal.insert("created_at".to_string(), Value::from(created_at));
        }
        if let Some(updated_at) = self.updated_at {
            goal.insert("updated_at".to_string(), Value::from(updated_at));
        }
        Value::Object(goal)
    }
}

/// Parse a `thread_goal_updated` `event_msg` into a [`CodexGoalEvent`], or
/// `None` for any other line. A goal with an empty/absent objective is skipped
/// (there is nothing to catalog or search).
fn codex_goal_event_from_line(record: &Value) -> Option<CodexGoalEvent> {
    if record.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("thread_goal_updated") {
        return None;
    }
    let goal = payload.get("goal")?;
    let objective = goal
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())?
        .to_string();
    Some(CodexGoalEvent {
        objective,
        status: goal
            .get("status")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|status| !status.is_empty())
            .map(str::to_string),
        thread_id: goal
            .get("threadId")
            .and_then(Value::as_str)
            .or_else(|| payload.get("threadId").and_then(Value::as_str))
            .filter(|thread_id| !thread_id.is_empty())
            .map(str::to_string),
        tokens_used: goal.get("tokensUsed").and_then(Value::as_i64),
        time_used_seconds: goal.get("timeUsedSeconds").and_then(Value::as_i64),
        created_at: goal.get("createdAt").and_then(Value::as_i64),
        updated_at: goal.get("updatedAt").and_then(Value::as_i64),
    })
}

/// Build the compact `goal` session row: the objective as searchable text, the
/// lifecycle fields in `metadata_json`. Role `system` matches the other
/// non-conversational Codex rows (goal context, compaction summaries).
fn goal_event_message(
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    timestamp: Option<i64>,
    event: &CodexGoalEvent,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "system".to_string(),
        timestamp,
        ordinal: offset,
        text: event.objective.clone(),
        kind: Some("goal".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&event.metadata()).ok(),
    }
}

fn collect_response_item_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(collect_response_item_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                return text.to_string();
            }
            ["content", "message", "item"]
                .iter()
                .filter_map(|key| map.get(*key))
                .map(collect_response_item_text)
                .find(|text| !text.is_empty())
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn timestamp_from_record(record: &Value) -> Option<i64> {
    record
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .map(|secs| secs as i64)
}

fn compacted_summary_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
    depth: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("compacted") {
        return None;
    }
    let payload = record.get("payload")?;
    let replacement_history_count = payload
        .get("replacement_history")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let compaction = payload
        .get("replacement_history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history
                .iter()
                .rev()
                .find(|entry| entry.get("type").and_then(Value::as_str) == Some("compaction"))
        });
    let plaintext = payload
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let encrypted = compaction
        .and_then(|entry| entry.get("encrypted_content"))
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty());
    let summary_body = if plaintext.is_some() {
        "plaintext"
    } else if encrypted {
        "encrypted"
    } else {
        "unavailable"
    };
    let timestamp_text = record
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("unknown time");
    let text = plaintext.map_or_else(
        || {
            format!(
                "Codex context compaction at {timestamp_text}. Summary body is {summary_body} in the rollout; replacement history entries: {replacement_history_count}."
            )
        },
        str::to_string,
    );

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_context_compacted".to_string()),
    );
    metadata.insert(
        "source_event".to_string(),
        Value::String("compacted".to_string()),
    );
    metadata.insert(
        "summary_body".to_string(),
        Value::String(summary_body.to_string()),
    );
    metadata.insert(
        "replacement_history_count".to_string(),
        Value::from(replacement_history_count as i64),
    );
    metadata.insert(
        "codex_compaction_depth".to_string(),
        Value::from(depth.max(1)),
    );
    metadata.insert("source_offset".to_string(), Value::from(offset));
    metadata.insert("encrypted".to_string(), Value::from(encrypted));

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "assistant".to_string(),
        timestamp: timestamp_from_record(record),
        ordinal: offset,
        text,
        kind: Some("summary".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

struct CodexGoalContext {
    objective: String,
    tokens_used: Option<i64>,
    token_budget: Option<i64>,
    token_budget_unbounded: bool,
    tokens_remaining: Option<i64>,
    tokens_remaining_unbounded: bool,
}

impl CodexGoalContext {
    fn storage_text(&self) -> String {
        format!("Codex active goal: {}", self.objective)
    }

    fn metadata(&self) -> Value {
        let mut goal = serde_json::Map::new();
        goal.insert("source".to_string(), Value::String("goal".to_string()));
        goal.insert(
            "objective".to_string(),
            Value::String(self.objective.clone()),
        );
        if let Some(tokens_used) = self.tokens_used {
            goal.insert("tokens_used".to_string(), Value::from(tokens_used));
        }
        if let Some(token_budget) = self.token_budget {
            goal.insert("token_budget".to_string(), Value::from(token_budget));
        }
        if self.token_budget_unbounded {
            goal.insert("token_budget_unbounded".to_string(), Value::from(true));
        }
        if let Some(tokens_remaining) = self.tokens_remaining {
            goal.insert(
                "tokens_remaining".to_string(),
                Value::from(tokens_remaining),
            );
        }
        if self.tokens_remaining_unbounded {
            goal.insert("tokens_remaining_unbounded".to_string(), Value::from(true));
        }
        Value::Object(goal)
    }
}

fn codex_goal_context_from_text(text: &str) -> Option<CodexGoalContext> {
    const START: &str = "<codex_internal_context source=\"goal\">";
    const END: &str = "</codex_internal_context>";
    let start = text.find(START)?;
    if !text[..start].trim().is_empty() {
        return None;
    }
    let after_start = &text[start + START.len()..];
    let end = after_start.find(END)?;
    if !after_start[end + END.len()..].trim().is_empty() {
        return None;
    }
    let body = &after_start[..end];
    let objective = tag_body(body, "objective")?.trim();
    if objective.is_empty() {
        return None;
    }
    let token_budget_line = budget_line_value(body, "Token budget:");
    let tokens_remaining_line = budget_line_value(body, "Tokens remaining:");
    Some(CodexGoalContext {
        objective: objective.to_string(),
        tokens_used: budget_line_value(body, "Tokens used:").and_then(parse_budget_count),
        token_budget: token_budget_line.and_then(parse_budget_count),
        token_budget_unbounded: token_budget_line.is_some_and(is_unbounded_budget_value),
        tokens_remaining: tokens_remaining_line.and_then(parse_budget_count),
        tokens_remaining_unbounded: tokens_remaining_line.is_some_and(is_unbounded_budget_value),
    })
}

fn tag_body<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let after_start = text.split_once(&start_tag)?.1;
    let body = after_start.split_once(&end_tag)?.0;
    Some(body)
}

fn budget_line_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("- ")?.trim().strip_prefix(prefix))
        .or_else(|| {
            text.lines()
                .map(str::trim)
                .find_map(|line| line.strip_prefix(prefix))
        })
        .map(str::trim)
}

fn parse_budget_count(value: &str) -> Option<i64> {
    let digits = value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<i64>().ok()
    }
}

fn is_unbounded_budget_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "none" | "unbounded"
    )
}

fn goal_context_from_line(
    record: &Value,
    meta: &CodexMeta,
    model: Option<&str>,
    path: &Path,
    offset: i64,
) -> Option<SessionMessageRecord> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    let text = collect_response_item_text(payload.get("content").unwrap_or(payload));
    if !is_goal_context_text(&text) {
        return None;
    }

    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_goal_context".to_string()),
    );
    metadata.insert(
        "source_event".to_string(),
        Value::String("response_item".to_string()),
    );
    metadata.insert("source_offset".to_string(), Value::from(offset));

    Some(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id: format!("{}:{offset}", meta.session_id),
        session_id: meta.session_id.clone(),
        role: "system".to_string(),
        timestamp: timestamp_from_record(record),
        ordinal: offset,
        text,
        kind: Some("context".to_string()),
        model: model.map(str::to_string),
        tool_names: None,
        source_path: Some(path.to_string_lossy().to_string()),
        source_offset: Some(offset),
        metadata_json: serde_json::to_string(&Value::Object(metadata)).ok(),
    })
}

fn is_goal_context_text(text: &str) -> bool {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return false;
    };
    let header = header.trim_end_matches(':').to_ascii_lowercase();
    if header != "current goal for this thread" && header != "active goal for this thread" {
        return false;
    }

    let mut has_objective = false;
    let mut has_budget = false;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        has_objective |= lower.starts_with("objective:");
        has_budget |=
            lower.starts_with("remaining token budget:") || lower.starts_with("token budget:");
    }
    has_objective && has_budget
}

fn message_metadata(payload: &Value, goal_context: Option<&CodexGoalContext>) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert(
        "source".to_string(),
        Value::String("codex_rollout".to_string()),
    );
    if let Some(goal_context) = goal_context {
        metadata.insert(
            "codex_internal_context".to_string(),
            Value::String("goal".to_string()),
        );
        metadata.insert("codex_goal".to_string(), goal_context.metadata());
    }
    append_tool_calls_metadata(&mut metadata, payload);
    Value::Object(metadata)
}

/// Accumulates per-API-call `token_count` usage across one turn's tool loop.
///
/// Codex emits one `token_count` event per API call: the tool-loop calls
/// report *during* the turn (before the final `agent_message`) and the final
/// call reports right after it. Real rollouts on this machine showed ~64% of
/// input spend in those mid-turn reports, so honest cost accounting must sum
/// every call rather than keep only the one following the assistant reply.
/// Consecutive events whose cumulative `total_token_usage.total_tokens` did
/// not advance are duplicate reports of the same call and are skipped.
///
/// Counters are normalized for the savings dashboard's additive pricing
/// (Anthropic semantics): `OpenAI` `input_tokens` *includes*
/// `cached_input_tokens`, so the cached portion is split out into
/// `cache_read_input_tokens` and `input_tokens` keeps only the uncached
/// remainder.
#[derive(Default)]
pub(crate) struct CodexTurnUsage {
    input: i64,
    output: i64,
    cache_read: i64,
    reasoning: i64,
    total: i64,
    seen: bool,
    last_cumulative: Option<i64>,
}

impl CodexTurnUsage {
    /// Consume a rollout line when it is a `token_count` event, adding its
    /// per-call counters to the running turn sums. Returns `true` for every
    /// `token_count` line (even malformed or duplicate ones, which add
    /// nothing) and `false` for any other line kind.
    pub(crate) fn observe(&mut self, record: &Value) -> bool {
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            return false;
        }
        let Some(payload) = record.get("payload") else {
            return false;
        };
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            return false;
        }
        let Some(info) = payload.get("info") else {
            return true;
        };
        let cumulative = info
            .pointer("/total_token_usage/total_tokens")
            .and_then(Value::as_i64);
        if cumulative.is_some() && cumulative == self.last_cumulative {
            return true;
        }
        if cumulative.is_some() {
            self.last_cumulative = cumulative;
        }
        let Some(last) = info
            .get("last_token_usage")
            .or_else(|| info.get("total_token_usage"))
        else {
            return true;
        };
        let input = last
            .get("input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let output = last
            .get("output_tokens")
            .or_else(|| last.get("completion_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let cached = last
            .get("cached_input_tokens")
            .or_else(|| last.get("cache_read_input_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let reasoning = last
            .get("reasoning_output_tokens")
            .or_else(|| last.get("reasoning_tokens"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0);
        let total = last
            .get("total_tokens")
            .and_then(Value::as_i64)
            .or(cumulative)
            .unwrap_or_else(|| input.saturating_add(output).saturating_add(reasoning));
        if input == 0 && output == 0 && cached == 0 && reasoning == 0 && total == 0 {
            return true;
        }
        self.input = self
            .input
            .saturating_add((input.saturating_sub(cached)).max(0));
        self.cache_read = self.cache_read.saturating_add(cached);
        self.reasoning = self.reasoning.saturating_add(reasoning);
        self.output = self
            .output
            .saturating_add(output.max(0).saturating_add(reasoning));
        self.total = self.total.saturating_add(total.max(0));
        self.seen = true;
        true
    }

    /// The summed counters as a dashboard-shaped usage object, resetting the
    /// turn sums (the cumulative-total dedup guard survives across turns).
    pub(crate) fn take(&mut self) -> Option<Value> {
        if !self.seen {
            return None;
        }
        let mut usage = serde_json::Map::new();
        usage.insert("input_tokens".to_string(), Value::from(self.input));
        usage.insert("output_tokens".to_string(), Value::from(self.output));
        if self.cache_read > 0 {
            usage.insert(
                "cache_read_input_tokens".to_string(),
                Value::from(self.cache_read),
            );
        }
        if self.reasoning > 0 {
            usage.insert("reasoning_tokens".to_string(), Value::from(self.reasoning));
        }
        if self.total > 0 {
            usage.insert("total_tokens".to_string(), Value::from(self.total));
        }
        self.input = 0;
        self.output = 0;
        self.cache_read = 0;
        self.reasoning = 0;
        self.total = 0;
        self.seen = false;
        Some(Value::Object(usage))
    }
}

/// Add `add`'s numeric counters field-wise into `existing` (both are usage
/// objects). Used when several flushes land on the same assistant message
/// (e.g. an aborted turn with no reply of its own).
pub(crate) fn merge_usage_counters(existing: &mut Value, add: &Value) {
    let (Some(map), Some(add_map)) = (existing.as_object_mut(), add.as_object()) else {
        return;
    };
    for (key, value) in add_map {
        if let Some(count) = value.as_i64() {
            let current = map.get(key).and_then(Value::as_i64).unwrap_or(0);
            map.insert(key.clone(), Value::from(current.saturating_add(count)));
        }
    }
}

/// Attach the finished turn's summed usage to the most recent assistant
/// message of the batch (the reply the turn's `token_count` events report
/// on), merging additively when that message already carries usage.
fn flush_turn_usage(messages: &mut [SessionMessageRecord], turn_usage: &mut CodexTurnUsage) {
    let Some(usage) = turn_usage.take() else {
        return;
    };
    let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| message.role == "assistant")
    else {
        return;
    };
    let mut metadata = message
        .metadata_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    match metadata.get_mut("usage") {
        Some(existing) => merge_usage_counters(existing, &usage),
        None => {
            metadata.insert("usage".to_string(), usage);
        }
    }
    if let Ok(serialized) = serde_json::to_string(&Value::Object(metadata)) {
        message.metadata_json = Some(serialized);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod goal_event_tests {
    use super::*;
    use serde_json::json;

    fn goal_event_line(objective: &str, status: &str) -> Value {
        json!({
            "timestamp": "2026-07-08T08:49:29.711Z",
            "type": "event_msg",
            "payload": {
                "type": "thread_goal_updated",
                "threadId": "thread-1",
                "goal": {
                    "threadId": "thread-1",
                    "objective": objective,
                    "status": status,
                    "tokensUsed": 42,
                    "timeUsedSeconds": 7,
                    "createdAt": 1_783_500_569i64,
                    "updatedAt": 1_783_500_600i64
                }
            }
        })
    }

    #[test]
    fn parses_goal_event_into_row_with_metadata() {
        let event =
            codex_goal_event_from_line(&goal_event_line("ship the parser", "active")).unwrap();
        let meta = CodexMeta {
            cwd: std::path::PathBuf::from("/tmp/project"),
            session_id: "sess-1".to_string(),
            model: None,
            git: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            agent_nickname: None,
            agent_role: None,
            thread_source: None,
        };
        let message = goal_event_message(
            &meta,
            Some("gpt-5.5"),
            std::path::Path::new("/tmp/rollout.jsonl"),
            128,
            Some(1_783_500_600),
            &event,
        );
        assert_eq!(message.role, "system");
        assert_eq!(message.kind.as_deref(), Some("goal"));
        assert_eq!(message.text, "ship the parser");
        assert_eq!(message.ordinal, 128);
        let metadata: Value =
            serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["source"], "codex_thread_goal");
        assert_eq!(metadata["source_event"], "thread_goal_updated");
        assert_eq!(metadata["status"], "active");
        assert_eq!(metadata["thread_id"], "thread-1");
        assert_eq!(metadata["tokens_used"], 42);
        assert_eq!(metadata["time_used_seconds"], 7);
        assert_eq!(metadata["created_at"], 1_783_500_569i64);
        assert_eq!(metadata["updated_at"], 1_783_500_600i64);
    }

    #[test]
    fn consecutive_identical_states_share_a_dedup_key() {
        let a = codex_goal_event_from_line(&goal_event_line("same goal", "active")).unwrap();
        // Same objective+status, only token/time drift -> same dedup key (skipped).
        let mut drift = goal_event_line("same goal", "active");
        drift["payload"]["goal"]["tokensUsed"] = json!(9999);
        drift["payload"]["goal"]["timeUsedSeconds"] = json!(321);
        let b = codex_goal_event_from_line(&drift).unwrap();
        assert_eq!(a.dedup_key(), b.dedup_key());
        // A status transition is a distinct key (new row).
        let c = codex_goal_event_from_line(&goal_event_line("same goal", "paused")).unwrap();
        assert_ne!(a.dedup_key(), c.dedup_key());
    }

    #[test]
    fn unknown_status_is_carried_through_verbatim() {
        let event =
            codex_goal_event_from_line(&goal_event_line("do the thing", "completed")).unwrap();
        assert_eq!(event.status.as_deref(), Some("completed"));
        let metadata = event.metadata();
        assert_eq!(metadata["status"], "completed");
    }

    #[test]
    fn missing_status_and_objective_are_handled_gracefully() {
        // No status key at all -> status None, still a valid goal row.
        let mut no_status = goal_event_line("objective only", "active");
        no_status["payload"]["goal"]
            .as_object_mut()
            .unwrap()
            .remove("status");
        let event = codex_goal_event_from_line(&no_status).unwrap();
        assert!(event.status.is_none());
        assert!(!event.metadata().as_object().unwrap().contains_key("status"));
        // Empty objective -> no goal row (nothing to catalog).
        let empty = goal_event_line("   ", "active");
        assert!(codex_goal_event_from_line(&empty).is_none());
    }

    #[test]
    fn non_goal_event_lines_are_ignored() {
        let token_count = json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {}}
        });
        assert!(codex_goal_event_from_line(&token_count).is_none());
        let user = json!({
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "hi"}
        });
        assert!(codex_goal_event_from_line(&user).is_none());
    }

    #[test]
    fn exposed_reasoning_carries_visibility_without_claiming_hidden_content() {
        let payload = json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "visible summary"}],
        });
        let metadata = response_item_tool_metadata("reasoning", &payload, None, None);
        assert_eq!(metadata["reasoning_visibility"], "provider_exposed");
        assert_eq!(metadata["reasoning_retention"], "provider_exposed");
        assert!(metadata.get("encrypted_content").is_none());
    }

    #[test]
    fn observation_admission_routes_project_and_profile_records_by_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        let project_src = project_root.join("src");
        let other = temp.path().join("other");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let status = std::process::Command::new(crate::git::git_program())
            .args(["init", "--quiet"])
            .current_dir(&project_root)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");

        let project_id = ProjectId::new("project-id").unwrap();
        let project = CodexObservationAdmission::Project {
            root: &project_root,
            project_id: project_id.clone(),
        };
        let linked_root = temp.path().join("linked-worktree");
        std::fs::create_dir_all(&linked_root).unwrap();
        let linked = CodexObservationAdmission::Project {
            root: &linked_root,
            project_id,
        };
        assert_eq!(project.scope(), linked.scope());
        assert!(project.accepts(Some(&project_src)));
        assert!(!project.accepts(Some(&other)));

        let registered = vec![project_root];
        let profile = CodexObservationAdmission::Profile {
            session_id: Some("session-1"),
            registered_roots: &registered,
        };
        assert!(!profile.accepts(Some(&project_src)));
        assert!(profile.accepts(Some(&other)));
        assert!(profile.accepts(None));
        assert!(profile.accepts_session("session-1"));
        assert!(!profile.accepts_session("session-2"));
    }

    #[test]
    fn native_record_identity_is_stable_across_json_formatting() {
        let compact: Value = serde_json::from_str(
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"redacted"}}"#,
        )
        .unwrap();
        let spaced: Value = serde_json::from_str(
            r#"{ "payload": { "message": "redacted", "type": "agent_message" }, "type": "event_msg" }"#,
        )
        .unwrap();
        assert_eq!(
            codex_native_record_id("session-redacted", &compact)
                .unwrap()
                .as_str(),
            codex_native_record_id("session-redacted", &spaced)
                .unwrap()
                .as_str()
        );
    }

    #[test]
    fn canonical_codex_record_is_typed_and_redacts_provider_bags() {
        let native = json!({
            "timestamp": "2026-07-08T08:49:29Z",
            "type": "response_item",
            "cwd": "/secret/project",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call-redacted",
                "arguments": {"path": "/secret/project", "token": "credential-redacted"}
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(40, 80).unwrap();
        let record_id = codex_native_record_id("session-redacted", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "session-redacted",
            Some("session-redacted"),
            record_id.clone(),
            range,
        )
        .unwrap();
        let rendered = format!("{envelope:?}");
        assert!(rendered.contains("ToolInvocation"));
        assert!(rendered.contains("FileBytes"));
        assert!(rendered.contains(record_id.as_str()));
        assert!(!rendered.contains("/secret/project"));
        assert!(!rendered.contains("credential-redacted"));
    }

    #[test]
    fn codex_turn_context_sets_native_turn_and_thread_relations() {
        let native = json!({
            "type": "turn_context",
            "payload": {
                "turn_id": "turn-native-1",
                "cwd": "/secret/project",
                "model": "gpt-5.5"
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 40).unwrap();
        let record_id = codex_native_record_id("thread-redacted", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "thread-redacted",
            Some("thread-redacted"),
            record_id,
            range,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["session_id"], "thread-redacted");
        assert_eq!(relations["thread_id"], "thread-redacted");
        assert_eq!(relations["turn_id"], "turn-native-1");
        assert!(relations.get("message_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Unknown {
                native_kind,
                state: CanonicalUnknownStateV1::Unsupported,
            } if native_kind == "turn_context"
        )));
    }

    #[test]
    fn codex_session_meta_subagent_sets_native_agent_lineage() {
        let native = json!({
            "type": "session_meta",
            "payload": {
                "id": "child-thread",
                "cwd": "/secret/project",
                "thread_source": "subagent",
                "agent_nickname": "worker-a",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": "parent-thread",
                            "agent_nickname": "worker-a"
                        }
                    }
                }
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 50).unwrap();
        let record_id = codex_native_record_id("child-thread", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "child-thread",
            Some("child-thread"),
            record_id,
            range,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "child-thread");
        assert_eq!(relations["agent_id"], "child-thread");
        assert_eq!(relations["parent_agent_id"], "parent-thread");
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("message_id").is_none());
    }

    #[test]
    fn codex_response_without_turn_id_leaves_turn_unset() {
        let native = json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello"}]
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
        let record_id = codex_native_record_id("thread-redacted", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "thread-redacted",
            Some("thread-redacted"),
            record_id.clone(),
            range,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "thread-redacted");
        assert_eq!(relations["message_id"], record_id.as_str());
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
    }

    #[test]
    fn codex_canonical_agent_identity_ignores_nickname_and_role() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let native = |nickname: &str, role: &str| {
            json!({
                "type": "session_meta",
                "payload": {
                    "id": "child-thread",
                    "cwd": project,
                    "thread_source": "subagent",
                    "agent_nickname": nickname,
                    "agent_role": role,
                    "source": {
                        "subagent": {
                            "thread_spawn": {
                                "parent_thread_id": "parent-thread",
                                "agent_nickname": nickname,
                                "agent_role": role
                            }
                        }
                    }
                }
            })
        };
        let write_rollout = |name: &str, record: &Value| {
            let path = temp.path().join(name);
            let contents = format!(
                "{}\n{}\n",
                serde_json::to_string(record).unwrap(),
                json!({
                    "type": "event_msg",
                    "payload": {"type": "agent_message", "message": "done"}
                })
            );
            std::fs::write(&path, contents).unwrap();
            path
        };

        let first_native = native("Euler", "explorer");
        let renamed_native = native("Gauss", "reviewer");
        let first_path = write_rollout("first.jsonl", &first_native);
        let renamed_path = write_rollout("renamed.jsonl", &renamed_native);
        let first_meta = session_meta_with_provenance(&first_path).unwrap();
        let renamed_meta = session_meta_with_provenance(&renamed_path).unwrap();
        assert_eq!(first_meta.native_thread_id.as_deref(), Some("child-thread"));
        assert_eq!(
            renamed_meta.native_thread_id.as_deref(),
            Some("child-thread")
        );
        assert_eq!(first_meta.meta.agent_id.as_deref(), Some("Euler"));
        assert_eq!(renamed_meta.meta.agent_id.as_deref(), Some("Gauss"));

        let source = CodexSource::with_home(temp.path());
        let first_parsed = source
            .parse_new(&first_path, StoredCursor::default(), &project, None)
            .unwrap();
        let renamed_parsed = source
            .parse_new(&renamed_path, StoredCursor::default(), &project, None)
            .unwrap();
        assert_eq!(first_parsed.draft.agent_id.as_deref(), Some("Euler"));
        assert_eq!(renamed_parsed.draft.agent_id.as_deref(), Some("Gauss"));

        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let first_record_id = codex_native_record_id("child-thread", &first_native).unwrap();
        let renamed_record_id = codex_native_record_id("child-thread", &renamed_native).unwrap();
        assert_ne!(first_record_id, renamed_record_id);
        let first_envelope = normalize_codex_observation(
            &first_native,
            "child-thread",
            first_meta.native_thread_id.as_deref(),
            first_record_id,
            range,
        )
        .unwrap();
        let renamed_envelope = normalize_codex_observation(
            &renamed_native,
            "child-thread",
            renamed_meta.native_thread_id.as_deref(),
            renamed_record_id,
            range,
        )
        .unwrap();
        let first_relations = serde_json::to_value(first_envelope.relations()).unwrap();
        let renamed_relations = serde_json::to_value(renamed_envelope.relations()).unwrap();
        assert_eq!(first_relations["agent_id"], "child-thread");
        assert_eq!(renamed_relations["agent_id"], "child-thread");
        assert_eq!(first_relations["parent_agent_id"], "parent-thread");
        assert_eq!(renamed_relations["parent_agent_id"], "parent-thread");
    }

    #[test]
    fn codex_subagent_filename_fallback_does_not_invent_canonical_identity() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let native = json!({
            "type": "session_meta",
            "payload": {
                "cwd": project,
                "thread_source": "subagent",
                "agent_nickname": "mutable-label",
                "agent_role": "mutable-role",
                "forked_from_id": "parent-thread"
            }
        });
        let path = temp.path().join("rollout-filename.jsonl");
        let contents = format!(
            "{}\n{}\n",
            native,
            json!({
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": "legacy rollout"}
            })
        );
        std::fs::write(&path, contents).unwrap();

        let meta = session_meta_with_provenance(&path).unwrap();
        assert_eq!(meta.meta.session_id, "rollout-filename");
        assert!(meta.native_thread_id.is_none());
        assert_eq!(meta.meta.agent_id.as_deref(), Some("mutable-label"));
        let parsed = CodexSource::with_home(temp.path())
            .parse_new(&path, StoredCursor::default(), &project, None)
            .unwrap();
        assert_eq!(parsed.draft.agent_id.as_deref(), Some("mutable-label"));

        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = codex_native_record_id("rollout-filename", &native).unwrap();
        let envelope =
            normalize_codex_observation(&native, "rollout-filename", None, record_id, range)
                .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert!(relations.get("thread_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert_eq!(relations["parent_agent_id"], "parent-thread");
    }

    fn load_codex_golden_input(name: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/provider_normalization/codex/{name}.input.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn load_codex_golden_expected(name: &str, stable_record_id: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/provider_normalization/codex/{name}.expected_envelope.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(path).unwrap().replace(
            "\"$STABLE_RECORD_ID\"",
            &serde_json::to_string(stable_record_id).unwrap(),
        );
        serde_json::from_str(&raw).unwrap()
    }

    fn assert_codex_golden_envelope(
        name: &str,
        session_id: &str,
        native_thread_id: Option<&str>,
        range: tracedecay_domain::ObservationSourceRangeV1,
    ) {
        let native = load_codex_golden_input(name);
        // Stable record id is content-addressed provider-parser evidence
        // (`codex_native_record_id`), not a hand-built canonical identity.
        let record_id = codex_native_record_id(session_id, &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            session_id,
            native_thread_id,
            record_id.clone(),
            range,
        )
        .unwrap();
        let actual = serde_json::to_value(&envelope).unwrap();
        let expected = load_codex_golden_expected(name, record_id.as_str());
        assert_eq!(
            actual, expected,
            "Codex golden envelope mismatch for {name}: parser projection must match checked-in expected envelope"
        );
        // Hostile lookalike / provider bags must not survive normalization.
        let rendered = actual.to_string();
        assert!(!rendered.contains("/secret/project"));
        assert!(!rendered.contains("credential-redacted"));
        assert!(!rendered.contains("/redacted/project"));
    }

    #[test]
    fn codex_checked_in_agent_message_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        assert_codex_golden_envelope(
            "agent_message",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn codex_checked_in_session_meta_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        assert_codex_golden_envelope(
            "session_meta",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn codex_checked_in_function_call_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(40, 80).unwrap();
        assert_codex_golden_envelope(
            "function_call",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn codex_checked_in_thread_goal_updated_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        assert_codex_golden_envelope(
            "thread_goal_updated",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn thread_goal_updated_maps_nested_goal_to_workflow_lifecycle() {
        // Binding shape: goal_event_line / write_codex_rollout_with_goal_events.
        let native = goal_event_line("phlogiston pipeline overhaul", "active");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = codex_native_record_id("thread-1", &native).unwrap();
        let envelope =
            normalize_codex_observation(&native, "thread-1", Some("thread-1"), record_id, range)
                .unwrap();
        let facts = envelope.facts();
        assert_eq!(facts.len(), 1);
        match &facts[0] {
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
                provider_reference,
                item_id,
                parent_reference,
                list_reference,
                state,
                status,
                item_order,
                revision,
                event_sequence,
                content,
            } => {
                assert_eq!(provider_reference.as_deref(), Some("thread-1"));
                assert_eq!(status.as_deref(), Some("active"));
                assert!(item_id.is_none());
                assert!(parent_reference.is_none());
                assert!(list_reference.is_none());
                assert!(state.is_none());
                assert!(item_order.is_none());
                assert!(revision.is_none());
                assert!(event_sequence.is_none());
                let content = content.as_ref().expect("native goal object");
                assert_eq!(content["objective"], "phlogiston pipeline overhaul");
                assert_eq!(content["status"], "active");
                assert_eq!(content["threadId"], "thread-1");
                assert_eq!(content["tokensUsed"], 42);
                assert!(content.get("revision").is_none());
            }
            other => panic!("expected WorkflowLifecycle Goal, got {other:?}"),
        }
    }

    #[test]
    fn update_plan_preserves_arguments_as_workflow_lifecycle_plan() {
        // Binding shape: write_codex_rollout_with_structured_events / update_plan_row.
        let native = json!({
            "timestamp": "2026-01-03T00:00:03.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "update_plan",
                "call_id": "call-plan-1",
                "arguments": "{\"explanation\":\"why\",\"plan\":[{\"step\":\"sweep telemetry\",\"status\":\"in_progress\"},{\"step\":\"ship\",\"status\":\"pending\"}]}"
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(10, 20).unwrap();
        let record_id = codex_native_record_id("codex-structured", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "codex-structured",
            Some("codex-structured"),
            record_id,
            range,
        )
        .unwrap();
        let facts = envelope.facts();
        assert_eq!(facts.len(), 2);
        assert!(matches!(
            &facts[0],
            CanonicalObservationFactV1::ToolInvocation { name, arguments, .. }
                if name == "update_plan" && arguments.is_null()
        ));
        match &facts[1] {
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Plan,
                provider_reference,
                item_id,
                revision,
                status,
                item_order,
                content,
                ..
            } => {
                assert_eq!(provider_reference.as_deref(), Some("call-plan-1"));
                assert!(item_id.is_none());
                assert!(revision.is_none());
                assert!(status.is_none());
                assert!(item_order.is_none());
                let content = content.as_ref().expect("preserved update_plan arguments");
                assert_eq!(content["explanation"], "why");
                assert_eq!(content["plan"][0]["step"], "sweep telemetry");
                assert_eq!(content["plan"][0]["status"], "in_progress");
                assert_eq!(content["plan"][1]["step"], "ship");
                assert_eq!(content["plan"][1]["status"], "pending");
            }
            other => panic!("expected WorkflowLifecycle Plan, got {other:?}"),
        }
    }

    #[test]
    fn task_complete_and_turn_events_map_exactly_without_lookalikes() {
        // Binding shape: write_codex_rollout_with_structured_events /
        // task_events_become_turn_boundary_rows — singular task_complete only.
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        for (payload, expected_state, expected_status) in [
            (
                json!({
                    "type": "task_started",
                    "turn_id": "turn-1",
                    "started_at": 1_782_000_000i64,
                    "model_context_window": 258_400
                }),
                "task_started",
                None,
            ),
            (
                json!({
                    "type": "task_complete",
                    "turn_id": "turn-1",
                    "duration_ms": 8000,
                    "time_to_first_token_ms": 900,
                    "last_agent_message": "must not become content"
                }),
                "task_complete",
                None,
            ),
            (
                json!({
                    "type": "turn_aborted",
                    "turn_id": "turn-2",
                    "reason": "interrupted",
                    "duration_ms": 5626
                }),
                "turn_aborted",
                None,
            ),
        ] {
            let expected_turn_id = payload
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let native = json!({
                "timestamp": "2026-01-03T00:00:09.000Z",
                "type": "event_msg",
                "payload": payload
            });
            let record_id = codex_native_record_id("turn-session", &native).unwrap();
            let envelope = normalize_codex_observation(
                &native,
                "turn-session",
                Some("turn-session"),
                record_id,
                range,
            )
            .unwrap();
            match &envelope.facts()[0] {
                CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
                    provider_reference,
                    state,
                    status,
                    revision,
                    content,
                    ..
                } => {
                    assert_eq!(state.as_deref(), Some(expected_state));
                    assert_eq!(status.as_deref(), expected_status);
                    assert!(revision.is_none());
                    assert_eq!(provider_reference.as_deref(), expected_turn_id.as_deref());
                    let content = content.as_ref().expect("turn content");
                    assert_eq!(content["type"], expected_state);
                    assert!(content.get("last_agent_message").is_none());
                    if expected_state == "turn_aborted" {
                        assert_eq!(content["reason"], "interrupted");
                    }
                }
                other => {
                    panic!("expected WorkflowLifecycle Task for {expected_state}, got {other:?}")
                }
            }
        }

        // Lookalikes must not become Task lifecycle.
        for lookalike in ["task_completed", "task_failed"] {
            let native = json!({
                "timestamp": "2026-01-03T00:00:09.000Z",
                "type": "event_msg",
                "payload": {"type": lookalike, "turn_id": "turn-x"}
            });
            let record_id = codex_native_record_id("turn-session", &native).unwrap();
            let envelope = normalize_codex_observation(
                &native,
                "turn-session",
                Some("turn-session"),
                record_id,
                range,
            )
            .unwrap();
            assert!(
                matches!(
                    &envelope.facts()[0],
                    CanonicalObservationFactV1::Unknown {
                        native_kind,
                        state: CanonicalUnknownStateV1::Unsupported,
                    } if native_kind == lookalike
                ),
                "lookalike {lookalike} must stay Unknown, got {:?}",
                envelope.facts()
            );
        }
    }

    #[test]
    fn goal_context_response_item_remains_message_only() {
        // Goal-context prose is not a fixture-backed native lifecycle record.
        // It must remain a Message rather than becoming inferred Goal state.
        let native = json!({
            "timestamp": "2026-01-01T00:00:15.100Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<codex_internal_context source=\"goal\"><objective>ensure all provider session messages are ingested</objective>\nToken budget: 12000\nTokens remaining: 11000</codex_internal_context>"
                }]
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = codex_native_record_id("codex-goal-context", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "codex-goal-context",
            Some("codex-goal-context"),
            record_id,
            range,
        )
        .unwrap();
        let facts = envelope.facts();
        assert_eq!(facts.len(), 1);
        assert!(matches!(
            &facts[0],
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::User,
                ..
            }
        ));
    }
}
