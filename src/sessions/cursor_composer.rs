//! Cursor **composer** transcript ingestion.
//!
//! Cursor's primary chat history does not live in the
//! `~/.cursor/projects/<slug>/agent-transcripts/**.jsonl` files that
//! [`crate::sessions::cursor`] sweeps — those cover only a slice of activity.
//! The bulk lives in two SQLite-backed stores this module reads **strictly
//! read-only**:
//!
//! 1. The global `~/.config/Cursor/User/globalStorage/state.vscdb` — a
//!    single-table (`cursorDiskKV`) key/value store with:
//!    * `composerData:<composerId>` — one JSON *session envelope* per chat
//!      (name, createdAt/lastUpdatedAt, model, workspace path, an ordered
//!      `fullConversationHeadersOnly` list of bubble ids, todos, git repos, …).
//!    * `bubbleId:<composerId>:<bubbleId>` — one JSON *message record* per turn
//!      (text, thinking, `toolFormerData`, tokenCount, commits, pullRequests …).
//! 2. The newer per-session `~/.cursor/chats/<ws-hash>/<agentId>/store.db` — a
//!    content-addressed blob DAG (`meta` + `blobs`) walked from
//!    `latestRootBlobId`. Best-effort: the plain-JSON `{role,content}` leaf
//!    blobs are ingested; protobuf-framed leaves are tolerated but skipped.
//!
//! ## Read-only safety
//!
//! The live `state.vscdb` here is ~21 GB / 1.4M rows. We open it with a
//! `file:…?immutable=1&mode=ro` URI (`SQLite` skips all locking and never writes
//! a `-wal`/`-shm`), and we only ever issue **indexed** lookups: a single
//! bounded range scan over the `composerData:` key prefix and primary-key
//! (`key = ?`) point lookups for bubbles. No full-table scans.
//!
//! ## Incremental + dedupe
//!
//! Each composer source advances through its ordered bubbles using the
//! authoritative observation cursor. The cursor is compare-and-swap bound to
//! the snapshot generation and `SnapshotOrder`, so a sweep replays only
//! uncovered positions. Because a composer session id equals the stem of its
//! JSONL transcript for ~94% of sessions, the composer sweep runs *before* the
//! JSONL [`crate::sessions::cursor::CursorSweepSource`] and hands it the set of
//! composer-owned session ids to skip, so the richer composer rows win and no
//! message row is ever double-ingested.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use libsql::{Builder, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationEvidenceV1, CanonicalObservationFactV1, CanonicalObservationRelationsV1,
    CanonicalReasoningVisibilityV1, CanonicalUnknownStateV1, CanonicalWorkflowEvidenceKindV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionStatus,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::shared::path_belongs_to_project;
use crate::sessions::source::TranscriptIngestError;

/// `SQLITE_OPEN_URI` — not exposed by libsql's [`OpenFlags`], so we OR the raw
/// bit in (libsql forwards `flags.bits()` verbatim to `sqlite3_open_v2`). This
/// makes `SQLite` interpret the `file:…?immutable=1` URI filename.
const SQLITE_OPEN_URI: i32 = 0x0000_0040;

/// Provider id shared with the JSONL Cursor source so both land in the same
/// per-project `sessions.db` namespace and dedupe by `(provider, message_id)`.
const PROVIDER: &str = "cursor";
const COMPOSER_OBSERVATION_RETENTION: &str = "retention.provider-observation";

pub fn build_cursor_composer_capture_request(
    composer_id: &str,
    bubble_id: &str,
    bubble: &Value,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    position: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> Result<CaptureObservationRequest, String> {
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer position: {error}"))?;
    let encoded = serde_json::to_vec(bubble)
        .map_err(|error| format!("could not encode Cursor composer bubble: {error}"))?;
    let native_record_id = cursor_composer_native_record_id(composer_id, bubble_id)?;
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            normalize_cursor_composer_observation(
                &native,
                composer_id,
                native_record_id.clone(),
                range,
                position,
            )
        },
    )
    .map_err(|error| format!("could not parse Cursor composer bubble: {error}"))?;
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)
            .map_err(|error| format!("invalid Cursor provider id: {error}"))?,
        SessionId::new(composer_id)
            .map_err(|error| format!("invalid Cursor composer id: {error}"))?,
    )
    .map_err(|error| format!("invalid Cursor composer source: {error}"))?;
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        native_record_id,
    )
    .map_err(|error| format!("invalid Cursor composer identity: {error}"))?;
    CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        RetentionClass::new(COMPOSER_OBSERVATION_RETENTION)
            .map_err(|error| format!("invalid Cursor composer retention: {error}"))?,
        ObservationCancellation::default(),
    )
    .map_err(|error| format!("invalid Cursor composer capture request: {error}"))
}

pub async fn capture_cursor_composer_observation(
    db: &GlobalDb,
    request: CaptureObservationRequest,
) -> Result<CaptureObservationOutcome, TranscriptIngestError> {
    let authorities = match request.scope() {
        ObservationScopeV1::Project { .. } => HostAdmissionAuthorities::new(Some(db), None),
        ObservationScopeV1::Profile => HostAdmissionAuthorities::new(None, Some(db)),
    };
    HostAdmissionFacade::new(authorities)
        .capture_observation(request)
        .await
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })
}

fn normalize_cursor_composer_observation(
    native: &Value,
    composer_id: &str,
    stable_record_id: ObservationId,
    range: tracedecay_domain::ObservationSourceRangeV1,
    position: u64,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let timestamp = bubble_epoch(native, "createdAt");
    let relations = CanonicalObservationRelationsV1::new(
        SessionId::new(composer_id)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
    )
    .with_message_id(stable_record_id.clone());
    let mut facts = Vec::new();

    if let Some(text) = native
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        facts.push(CanonicalObservationFactV1::Message {
            role: match native.get("type").and_then(Value::as_i64) {
                Some(1) => CanonicalMessageRoleV1::User,
                Some(2) => CanonicalMessageRoleV1::Assistant,
                _ => CanonicalMessageRoleV1::Unknown,
            },
            content: Value::String(text.to_string()),
            model: ["model", "modelId", "modelName"]
                .into_iter()
                .find_map(|key| native.get(key).and_then(Value::as_str))
                .filter(|model| !model.trim().is_empty())
                .map(str::to_string),
            timestamp,
        });
    }

    if let Some(tool) = native.get("toolFormerData").filter(|tool| !tool.is_null()) {
        let invocation_id = composer_observation_id(
            tool.get("toolCallId")
                .or_else(|| tool.get("id"))
                .and_then(Value::as_str),
            &stable_record_id,
        );
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("tool")
            .to_string();
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id: invocation_id.clone(),
            name,
            arguments: Value::Null,
        });
        if tool.get("result").is_some_and(|result| !result.is_null()) {
            facts.push(CanonicalObservationFactV1::ToolResult {
                invocation_id: Some(invocation_id),
                content: Value::Null,
                success: tool
                    .get("status")
                    .and_then(Value::as_str)
                    .map(|status| matches!(status, "completed" | "success" | "succeeded")),
            });
        }
    }

    if let Some(thinking) = native
        .pointer("/thinking/text")
        .filter(|thinking| !thinking.is_null())
        .cloned()
    {
        facts.push(CanonicalObservationFactV1::Reasoning {
            visibility: CanonicalReasoningVisibilityV1::Visible,
            content: Some(thinking),
        });
    }

    if let Some(token_count) = native.get("tokenCount") {
        let input_tokens = composer_canonical_u64(token_count.get("inputTokens"));
        let output_tokens = composer_canonical_u64(token_count.get("outputTokens"));
        if input_tokens.is_some() || output_tokens.is_some() {
            facts.push(CanonicalObservationFactV1::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            });
        }
    }

    append_composer_git_facts(native, &mut facts);
    if let Some(todos) = native.get("todos").and_then(Value::as_array) {
        let items = todos
            .iter()
            .filter_map(|todo| todo.get("content").and_then(Value::as_str))
            .filter(|content| !content.trim().is_empty())
            .map(|content| Value::String(content.to_string()))
            .collect::<Vec<_>>();
        if !items.is_empty() {
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::Plan,
                reference: None,
                content: Some(Value::Array(items)),
            });
        }
    }
    if native
        .get("isCompacted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        facts.push(CanonicalObservationFactV1::Compaction {
            summary: native.get("text").cloned(),
            input_tokens: None,
            output_tokens: None,
        });
    }
    if facts.is_empty() {
        facts.push(CanonicalObservationFactV1::Unknown {
            native_kind: "bubble".to_string(),
            state: CanonicalUnknownStateV1::Absent,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range)
            .with_native_sequence(position);
    if let Some(timestamp) = timestamp {
        evidence = evidence.with_native_timestamp(timestamp);
    }
    CanonicalObservationEnvelopeV1::new(
        ProviderId::new(PROVIDER)
            .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)?,
        "bubble",
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::NormalizationFailed)
}

fn append_composer_git_facts(native: &Value, facts: &mut Vec<CanonicalObservationFactV1>) {
    if let Some(commits) = native.get("commits").and_then(Value::as_array) {
        for commit in commits {
            let reference = ["hash", "sha", "id"]
                .into_iter()
                .find_map(|key| commit.get(key).and_then(Value::as_str))
                .map(str::to_string);
            facts.push(CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::Commit,
                reference,
                content: None,
            });
        }
    }
    if native
        .get("gitDiffs")
        .and_then(Value::as_array)
        .is_some_and(|diffs| !diffs.is_empty())
    {
        facts.push(CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::Diff,
            reference: None,
            content: None,
        });
    }
    if let Some(pull_requests) = native.get("pullRequests").and_then(Value::as_array) {
        for pull_request in pull_requests {
            let reference = ["url", "htmlUrl", "html_url", "id"]
                .into_iter()
                .find_map(|key| pull_request.get(key).and_then(Value::as_str))
                .map(str::to_string);
            facts.push(CanonicalObservationFactV1::Git {
                evidence_kind: CanonicalGitEvidenceKindV1::PullRequest,
                reference: reference.clone(),
                content: None,
            });
            facts.push(CanonicalObservationFactV1::Workflow {
                evidence_kind: CanonicalWorkflowEvidenceKindV1::PullRequest,
                reference,
                content: None,
            });
        }
    }
}

fn composer_observation_id(native_id: Option<&str>, fallback: &ObservationId) -> ObservationId {
    native_id
        .and_then(|native_id| ObservationId::new(native_id).ok())
        .unwrap_or_else(|| fallback.clone())
}

fn composer_canonical_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
    })
}

fn cursor_composer_native_record_id(
    composer_id: &str,
    bubble_id: &str,
) -> Result<ObservationId, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.cursor-composer-native-record.v1\0");
    hasher.update(composer_id.as_bytes());
    hasher.update([0]);
    hasher.update(bubble_id.as_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}")
            .map_err(|error| format!("could not encode Cursor composer identity: {error}"))?;
    }
    ObservationId::new(format!("cursor.composer.sha256:{encoded}"))
        .map_err(|error| format!("invalid Cursor composer native identity: {error}"))
}

/// Default ceiling on how many *new/changed* composer sessions one sweep pass
/// ingests, so the first backfill of thousands of sessions never blocks
/// startup; already-watermarked sessions are skipped cheaply and do not count.
pub const DEFAULT_COMPOSER_ENVELOPE_CAP: usize = 256;

/// Outcome of one composer sweep pass.
#[derive(Debug, Default, Clone)]
pub struct CursorComposerSweepOutcome {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
    /// Every composer session id that belongs to the swept project (whether
    /// ingested this pass or deferred by the cap). The JSONL sweep skips these
    /// so the two Cursor sources never double-ingest the same session.
    pub owned_session_ids: HashSet<String>,
}

impl CursorComposerSweepOutcome {
    fn add(&mut self, sessions: u64, messages: u64) {
        self.sessions_upserted = self.sessions_upserted.saturating_add(sessions);
        self.messages_upserted = self.messages_upserted.saturating_add(messages);
    }
}

/// Read-only Cursor composer store source rooted at a home directory.
pub struct CursorComposerSource {
    state_db_path: PathBuf,
    chats_dir: PathBuf,
}

struct ComposerIngestContext<'a> {
    facade: HostAdmissionFacade<'a>,
    scope: ObservationScopeV1,
    project_root: Option<&'a Path>,
    registered_roots: &'a [PathBuf],
}

impl CursorComposerSource {
    /// Source rooted at the real user home. `None` when it cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::sessions::home_dir()?;
        Some(Self::with_home(&home))
    }

    /// Source rooted at `<home>` (used by tests). Resolves both the global
    /// `state.vscdb` and the per-session `chats` directory.
    pub fn with_home(home: &Path) -> Self {
        Self {
            state_db_path: home
                .join(".config")
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb"),
            chats_dir: home.join(".cursor").join("chats"),
        }
    }

    /// Ingest every composer session (and per-session `store.db` chat) that
    /// belongs to `project_root` into `db`, bounded to `envelope_cap`
    /// newly-changed sessions this pass. Fail-open: any DB/parse error yields
    /// the outcome so far rather than propagating.
    pub async fn ingest(
        &self,
        db: &GlobalDb,
        project_root: &Path,
        envelope_cap: usize,
    ) -> CursorComposerSweepOutcome {
        let mut outcome = CursorComposerSweepOutcome::default();
        let Ok(scope) = project_scope(project_root) else {
            return outcome;
        };
        let context = ComposerIngestContext {
            facade: HostAdmissionFacade::new(HostAdmissionAuthorities::new(Some(db), None)),
            scope,
            project_root: Some(project_root),
            registered_roots: &[],
        };
        // ws-hash -> workspace fsPath, harvested from envelopes so per-session
        // store.db files (which key only by ws-hash) can be scoped to a project.
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(&context, envelope_cap, &mut outcome, &mut workspace_paths)
            .await;
        self.ingest_chat_store_dbs(&context, &workspace_paths, &mut outcome)
            .await;
        outcome
    }

    pub async fn ingest_user(
        &self,
        db: &GlobalDb,
        registered_roots: &[PathBuf],
        envelope_cap: usize,
    ) -> CursorComposerSweepOutcome {
        let mut outcome = CursorComposerSweepOutcome::default();
        let context = ComposerIngestContext {
            facade: HostAdmissionFacade::new(HostAdmissionAuthorities::new(None, Some(db))),
            scope: ObservationScopeV1::Profile,
            project_root: None,
            registered_roots,
        };
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(&context, envelope_cap, &mut outcome, &mut workspace_paths)
            .await;
        self.ingest_chat_store_dbs(&context, &workspace_paths, &mut outcome)
            .await;
        outcome
    }

    async fn ingest_state_vscdb(
        &self,
        context: &ComposerIngestContext<'_>,
        envelope_cap: usize,
        outcome: &mut CursorComposerSweepOutcome,
        workspace_paths: &mut HashMap<String, String>,
    ) {
        if !self.state_db_path.is_file() {
            return;
        }
        let Some(ro) = open_readonly_immutable(&self.state_db_path).await else {
            return;
        };
        let conn = &ro.conn;
        // Bounded, index-backed range scan over just the composerData prefix.
        let Ok(mut rows) = conn
            .query(
                "SELECT key, value FROM cursorDiskKV \
                 WHERE key >= 'composerData:' AND key < 'composerData;'",
                (),
            )
            .await
        else {
            return;
        };

        let mut ingested_this_pass = 0usize;
        while let Ok(Some(row)) = rows.next().await {
            let Ok(value) = row.get::<String>(1) else {
                continue;
            };
            let Ok(envelope) = serde_json::from_str::<Value>(&value) else {
                continue;
            };
            let Some(composer_id) = envelope
                .get("composerId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            let Some(project) = envelope_project(&envelope) else {
                continue;
            };
            if let Some(ws_hash) = workspace_hash(&envelope) {
                workspace_paths
                    .entry(ws_hash)
                    .or_insert_with(|| project.path.clone());
            }
            let _selected_project = match context.project_root {
                Some(root) if path_belongs_to_project(Path::new(&project.path), root) => {
                    ComposerProject {
                        path: project.path.clone(),
                    }
                }
                Some(_) => continue,
                None if context
                    .registered_roots
                    .iter()
                    .any(|root| path_belongs_to_project(Path::new(&project.path), root)) =>
                {
                    continue;
                }
                None => ComposerProject {
                    path: "user".to_string(),
                },
            };
            // Own this session for JSONL dedupe regardless of the per-pass cap.
            outcome.owned_session_ids.insert(composer_id.to_string());

            let headers = envelope
                .get("fullConversationHeadersOnly")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if ingested_this_pass >= envelope_cap {
                // Deferred to a later pass; still owned so JSONL stands down.
                continue;
            }
            let Some(generation) = snapshot_generation(&self.state_db_path) else {
                continue;
            };
            let mut session_accepted = false;
            let mut messages = 0_u64;
            for (position, header) in headers.iter().enumerate() {
                let Some(bubble_id) = header.get("bubbleId").and_then(Value::as_str) else {
                    continue;
                };
                let position = position as u64;
                let Ok(source) = cursor_composer_source(composer_id) else {
                    break;
                };
                let Ok(expected_cursor) = context
                    .facade
                    .get_source_cursor(&source, &context.scope)
                    .await
                else {
                    break;
                };
                if expected_cursor.as_ref().is_some_and(|cursor| {
                    cursor.generation() == generation
                        && cursor.position() >= position.saturating_add(1)
                }) {
                    continue;
                }
                let Some(bubble) = fetch_bubble(conn, composer_id, bubble_id).await else {
                    continue;
                };
                let Ok(request) = build_cursor_composer_capture_request(
                    composer_id,
                    bubble_id,
                    &bubble,
                    context.scope.clone(),
                    generation,
                    position,
                    expected_cursor.clone(),
                ) else {
                    continue;
                };
                match context.facade.capture_observation(request).await {
                    Ok(CaptureObservationOutcome::Persisted {
                        outcome: persisted, ..
                    }) => {
                        session_accepted = true;
                        if matches!(persisted, ObservationPersistOutcome::Committed(_)) {
                            messages = messages.saturating_add(1);
                        }
                    }
                    Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                        if advance_composer_coverage(
                            ComposerCoverageContext {
                                facade: &context.facade,
                                scope: &context.scope,
                                generation,
                            },
                            source,
                            position,
                            expected_cursor,
                            ObservationCoverageReason::SanitizerRejected,
                            receipt,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Ok(CaptureObservationOutcome::Quarantined { receipt, .. }) => {
                        if advance_composer_coverage(
                            ComposerCoverageContext {
                                facade: &context.facade,
                                scope: &context.scope,
                                generation,
                            },
                            source,
                            position,
                            expected_cursor,
                            ObservationCoverageReason::SanitizerQuarantined,
                            receipt,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            if session_accepted {
                ingested_this_pass += 1;
                outcome.add(1, messages);
            }
        }
    }

    async fn ingest_chat_store_dbs(
        &self,
        context: &ComposerIngestContext<'_>,
        workspace_paths: &HashMap<String, String>,
        outcome: &mut CursorComposerSweepOutcome,
    ) {
        let Ok(ws_entries) = std::fs::read_dir(&self.chats_dir) else {
            return;
        };
        for ws_entry in ws_entries.flatten() {
            if !ws_entry.path().is_dir() {
                continue;
            }
            let ws_hash = ws_entry.file_name().to_string_lossy().to_string();
            // Scope by ws-hash -> project mapping harvested from the envelopes.
            let _project_path = match (workspace_paths.get(&ws_hash), context.project_root) {
                (Some(path), Some(root)) if path_belongs_to_project(Path::new(path), root) => {
                    path.clone()
                }
                (Some(_), Some(_)) | (None, _) => continue,
                (Some(path), None)
                    if context
                        .registered_roots
                        .iter()
                        .any(|root| path_belongs_to_project(Path::new(path), root)) =>
                {
                    continue;
                }
                (Some(_), None) => "user".to_string(),
            };
            let Ok(agent_entries) = std::fs::read_dir(ws_entry.path()) else {
                continue;
            };
            for agent_entry in agent_entries.flatten() {
                let store_path = agent_entry.path().join("store.db");
                if !store_path.is_file() {
                    continue;
                }
                self.ingest_one_store_db(context, &store_path, outcome)
                    .await;
            }
        }
    }

    async fn ingest_one_store_db(
        &self,
        context: &ComposerIngestContext<'_>,
        store_path: &Path,
        outcome: &mut CursorComposerSweepOutcome,
    ) {
        let Some(ro) = open_readonly_immutable(store_path).await else {
            return;
        };
        let conn = &ro.conn;
        let Some(meta) = read_store_meta(conn).await else {
            return;
        };
        let blobs = read_store_blobs(conn).await;
        if blobs.is_empty() {
            return;
        }
        let ordered = order_store_messages(&blobs, meta.latest_root_blob_id.as_deref());
        if ordered.is_empty() {
            return;
        }
        let session_id = format!("cursor-chat:{}", meta.agent_id);
        outcome.owned_session_ids.insert(session_id.clone());

        let Some(generation) = snapshot_generation(store_path) else {
            return;
        };
        let Ok(source) = cursor_composer_source(&session_id) else {
            return;
        };
        let mut session_accepted = false;
        let mut messages = 0_u64;
        for (ordinal, (role, content)) in ordered.iter().enumerate() {
            let position = ordinal as u64;
            let Ok(expected_cursor) = context
                .facade
                .get_source_cursor(&source, &context.scope)
                .await
            else {
                return;
            };
            if expected_cursor.as_ref().is_some_and(|cursor| {
                cursor.generation() == generation && cursor.position() >= position.saturating_add(1)
            }) {
                continue;
            }
            let text = crate::sessions::shared::message_storage_text(content);
            if text.trim().is_empty() {
                continue;
            }
            let bubble = json!({
                "type": if role == "user" { 1 } else { 2 },
                "text": text,
                "createdAt": meta.created_at.map(|seconds| seconds.saturating_mul(1000)),
            });
            let Ok(request) = build_cursor_composer_capture_request(
                &session_id,
                &format!("chat:{ordinal}"),
                &bubble,
                context.scope.clone(),
                generation,
                position,
                expected_cursor.clone(),
            ) else {
                continue;
            };
            match context.facade.capture_observation(request).await {
                Ok(CaptureObservationOutcome::Persisted {
                    outcome: persisted, ..
                }) => {
                    session_accepted = true;
                    if matches!(persisted, ObservationPersistOutcome::Committed(_)) {
                        messages = messages.saturating_add(1);
                    }
                }
                Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                    if advance_composer_coverage(
                        ComposerCoverageContext {
                            facade: &context.facade,
                            scope: &context.scope,
                            generation,
                        },
                        source.clone(),
                        position,
                        expected_cursor,
                        ObservationCoverageReason::SanitizerRejected,
                        receipt,
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
                Ok(CaptureObservationOutcome::Quarantined { receipt, .. }) => {
                    if advance_composer_coverage(
                        ComposerCoverageContext {
                            facade: &context.facade,
                            scope: &context.scope,
                            generation,
                        },
                        source.clone(),
                        position,
                        expected_cursor,
                        ObservationCoverageReason::SanitizerQuarantined,
                        receipt,
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        if session_accepted {
            outcome.add(1, messages);
        }
    }
}

fn cursor_composer_source(composer_id: &str) -> Result<ObservationSourceIdentityV1, String> {
    ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER)
            .map_err(|error| format!("invalid Cursor provider id: {error}"))?,
        SessionId::new(composer_id)
            .map_err(|error| format!("invalid Cursor composer id: {error}"))?,
    )
    .map_err(|error| format!("invalid Cursor composer source: {error}"))
}

fn snapshot_generation(path: &Path) -> Option<ObservationSourceGenerationV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::metadata(path).ok()?;
        let mut hasher = Sha256::new();
        hasher.update(metadata.dev().to_le_bytes());
        hasher.update(metadata.ino().to_le_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        ObservationSourceGenerationV1::new(u64::from_le_bytes(bytes).max(1)).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

struct ComposerCoverageContext<'facade, 'db> {
    facade: &'facade HostAdmissionFacade<'db>,
    scope: &'facade ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
}

async fn advance_composer_coverage(
    context: ComposerCoverageContext<'_, '_>,
    source: ObservationSourceIdentityV1,
    position: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
    reason: ObservationCoverageReason,
    receipt: tracedecay_domain::SanitizationReceiptV1,
) -> Result<(), String> {
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer coverage range: {error}"))?;
    let advance = ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
        source,
        context.scope.clone(),
        context.generation,
        ObservationOrderingDomainV1::SnapshotOrder,
        expected_cursor,
        range,
        reason,
        receipt,
    )
    .map_err(|error| format!("invalid Cursor composer coverage transition: {error}"))?;
    context
        .facade
        .advance_non_durable_source_cursor(advance, ObservationCancellation::default())
        .await
        .map(|_| ())
        .map_err(host_admission_error)
}

fn project_scope(project_root: &Path) -> Result<ObservationScopeV1, String> {
    let layout = crate::storage::resolve_layout_for_current_profile(project_root)
        .map_err(|_| "could not resolve Cursor project identity".to_string())?;
    let project_id = layout
        .identity
        .project_id
        .ok_or_else(|| "Cursor project identity is unavailable".to_string())?;
    let project_id =
        ProjectId::new(project_id).map_err(|_| "invalid Cursor project identity".to_string())?;
    Ok(ObservationScopeV1::Project { project_id })
}

fn host_admission_error(outcome: HostAdmissionOutcome) -> String {
    match outcome.status {
        HostAdmissionStatus::Backpressured => "Cursor observation admission was backpressured",
        HostAdmissionStatus::Unavailable => "Cursor observation authority is unavailable",
        HostAdmissionStatus::Unknown => "Cursor observation provider is unsupported",
        HostAdmissionStatus::Degraded => "Cursor observation admission was degraded",
        HostAdmissionStatus::Supported
        | HostAdmissionStatus::AcceptedForReplay
        | HostAdmissionStatus::Committed
        | HostAdmissionStatus::ExactDuplicate => "Cursor observation admission was incomplete",
    }
    .to_string()
}

/// Resolved project for a composer envelope.
struct ComposerProject {
    path: String,
}

/// A read-only connection paired with its owning [`libsql::Database`] so the
/// underlying handle stays alive for the connection's lifetime.
struct ReadOnlyDb {
    _db: libsql::Database,
    conn: libsql::Connection,
}

/// Open a `SQLite` file strictly read-only and immutable (no locking, no
/// `-wal`/`-shm` writes) via a `file:…?immutable=1&mode=ro` URI.
async fn open_readonly_immutable(db_path: &Path) -> Option<ReadOnlyDb> {
    let uri = immutable_ro_uri(db_path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::from_bits_retain(SQLITE_OPEN_URI);
    let db = Builder::new_local(uri).flags(flags).build().await.ok()?;
    let conn = db.connect().ok()?;
    // Belt-and-suspenders against ever mutating the live store.
    let _ = conn.execute_batch("PRAGMA query_only = ON;").await;
    Some(ReadOnlyDb { _db: db, conn })
}

/// Build a `file:` URI whose path is percent-encoded for the characters `SQLite`
/// treats specially in URI filenames (`?`, `#`, `%`). Returns `None` for
/// non-UTF-8 paths.
fn immutable_ro_uri(db_path: &Path) -> Option<String> {
    let raw = db_path.to_str()?;
    let mut encoded = String::with_capacity(raw.len() + 24);
    for ch in raw.chars() {
        match ch {
            '?' => encoded.push_str("%3f"),
            '#' => encoded.push_str("%23"),
            '%' => encoded.push_str("%25"),
            other => encoded.push(other),
        }
    }
    Some(format!("file:{encoded}?immutable=1&mode=ro"))
}

async fn fetch_bubble(
    conn: &libsql::Connection,
    composer_id: &str,
    bubble_id: &str,
) -> Option<Value> {
    let key = format!("bubbleId:{composer_id}:{bubble_id}");
    let mut rows = conn
        .query(
            "SELECT value FROM cursorDiskKV WHERE key = ?1",
            libsql::params![key],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let value = row.get::<String>(0).ok()?;
    serde_json::from_str::<Value>(&value).ok()
}

fn envelope_project(envelope: &Value) -> Option<ComposerProject> {
    if let Some(uri) = envelope
        .get("workspaceIdentifier")
        .and_then(|w| w.get("uri"))
    {
        for key in ["fsPath", "path"] {
            if let Some(path) = uri
                .get(key)
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                return Some(ComposerProject {
                    path: path.to_string(),
                });
            }
        }
    }
    if let Some(repos) = envelope.get("trackedGitRepos").and_then(Value::as_array) {
        for repo in repos {
            if let Some(path) = repo
                .get("repoPath")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
            {
                return Some(ComposerProject {
                    path: path.to_string(),
                });
            }
        }
    }
    None
}

fn workspace_hash(envelope: &Value) -> Option<String> {
    envelope
        .get("workspaceIdentifier")
        .and_then(|w| w.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn bubble_epoch(bubble: &Value, key: &str) -> Option<i64> {
    epoch_ms_to_secs(bubble.get(key).and_then(Value::as_i64))
}

fn epoch_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.filter(|v| *v > 0).map(|v| v / 1000)
}

// ---------------------------------------------------------------------------
// store.db blob-DAG reader
// ---------------------------------------------------------------------------

struct StoreMeta {
    agent_id: String,
    latest_root_blob_id: Option<String>,
    created_at: Option<i64>,
}

async fn read_store_meta(conn: &libsql::Connection) -> Option<StoreMeta> {
    let mut rows = conn
        .query("SELECT value FROM meta WHERE key = '0'", ())
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let hex = row.get::<String>(0).ok()?;
    let bytes = decode_hex(&hex)?;
    let meta = serde_json::from_slice::<Value>(&bytes).ok()?;
    let agent_id = meta.get("agentId").and_then(Value::as_str)?.to_string();
    Some(StoreMeta {
        agent_id,
        latest_root_blob_id: meta
            .get("latestRootBlobId")
            .and_then(Value::as_str)
            .map(str::to_string),
        created_at: epoch_ms_to_secs(meta.get("createdAt").and_then(Value::as_i64)),
    })
}

/// All `(blob_id, raw_bytes)` in the store's `blobs` table.
async fn read_store_blobs(conn: &libsql::Connection) -> Vec<(String, Vec<u8>)> {
    let Ok(mut rows) = conn.query("SELECT id, data FROM blobs", ()).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let Ok(id) = row.get::<String>(0) else {
            continue;
        };
        let data = row
            .get::<Vec<u8>>(1)
            .or_else(|_| row.get::<String>(1).map(String::into_bytes));
        if let Ok(data) = data {
            out.push((id, data));
        }
    }
    out
}

/// Walk the blob DAG from `root` and return the ordered `(role, content)` of
/// every plain-JSON message leaf. Protobuf node blobs are traversed for their
/// length-32 child references; protobuf leaf blobs are tolerated but skipped.
/// Falls back to id-sorted order when the DAG cannot be walked.
fn order_store_messages(blobs: &[(String, Vec<u8>)], root: Option<&str>) -> Vec<(String, Value)> {
    let by_id: HashMap<&str, &[u8]> = blobs
        .iter()
        .map(|(id, data)| (id.as_str(), data.as_slice()))
        .collect();
    let mut ordered = Vec::new();

    if let Some(root) = root {
        let mut visited = HashSet::new();
        walk_store_blob(root, &by_id, &mut visited, &mut ordered);
        if !ordered.is_empty() {
            return ordered;
        }
    }

    // Fallback: id-sorted JSON leaves.
    let mut ids: Vec<&str> = by_id.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        if let Some(message) = store_blob_message(by_id[id]) {
            ordered.push(message);
        }
    }
    ordered
}

fn walk_store_blob<'a>(
    id: &str,
    by_id: &HashMap<&'a str, &'a [u8]>,
    visited: &mut HashSet<String>,
    ordered: &mut Vec<(String, Value)>,
) {
    if !visited.insert(id.to_string()) {
        return;
    }
    let Some(bytes) = by_id.get(id) else {
        return;
    };
    if let Some(message) = store_blob_message(bytes) {
        ordered.push(message);
        return;
    }
    for child in protobuf_child_refs(bytes) {
        if by_id.contains_key(child.as_str()) {
            walk_store_blob(&child, by_id, visited, ordered);
        }
    }
}

/// A JSON message leaf is a JSON object carrying a `role` field.
fn store_blob_message(bytes: &[u8]) -> Option<(String, Value)> {
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    let role = value.get("role").and_then(Value::as_str)?.to_string();
    let content = value.get("content").cloned().unwrap_or(Value::Null);
    Some((role, content))
}

/// Extract length-delimited field-1 entries that are exactly 32 bytes long and
/// hex-encode them — the content-addressed child ids of a DAG node blob. A
/// light protobuf scanner that skips unrelated fields by wire type.
fn protobuf_child_refs(bytes: &[u8]) -> Vec<String> {
    let mut refs = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some((tag, next)) = read_varint(bytes, i) else {
            break;
        };
        i = next;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match wire {
            0 => {
                // varint
                let Some((_, next)) = read_varint(bytes, i) else {
                    break;
                };
                i = next;
            }
            1 => i += 8, // 64-bit
            5 => i += 4, // 32-bit
            2 => {
                // length-delimited
                let Some((len, next)) = read_varint(bytes, i) else {
                    break;
                };
                i = next;
                let len = len as usize;
                if i + len > bytes.len() {
                    break;
                }
                if field == 1 && len == 32 {
                    refs.push(encode_hex(&bytes[i..i + len]));
                }
                i += len;
            }
            _ => break,
        }
    }
    refs
}

fn read_varint(bytes: &[u8], start: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    let mut i = start;
    while i < bytes.len() {
        let byte = bytes[i];
        result |= u64::from(byte & 0x7f) << shift;
        i += 1;
        if byte & 0x80 == 0 {
            return Some((result, i));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_capture_request_uses_snapshot_order_and_native_bubble_identity() {
        let bubble = json!({
            "type": 2,
            "text": "redacted fixture",
        });
        let request = build_cursor_composer_capture_request(
            "composer-redacted",
            "bubble-redacted",
            &bubble,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            7,
            None,
        );
        assert!(request.is_ok());
        assert_eq!(
            cursor_composer_native_record_id("composer-redacted", "bubble-redacted")
                .unwrap()
                .as_str(),
            cursor_composer_native_record_id("composer-redacted", "bubble-redacted")
                .unwrap()
                .as_str()
        );
    }

    #[test]
    fn canonical_composer_bubble_is_snapshot_typed_and_redacted() {
        let native = json!({
            "type": 2,
            "text": "redacted response",
            "createdAt": 1_783_500_600_000_i64,
            "workspaceIdentifier": {"uri": {"fsPath": "/secret/workspace"}},
            "toolFormerData": {
                "name": "Read",
                "toolCallId": "tool-redacted",
                "params": {"path": "/secret/workspace/file.rs", "token": "credential-redacted"},
                "result": {"body": "secret result"},
                "status": "completed"
            },
            "thinking": {"text": "provider-visible summary"},
            "tokenCount": {"inputTokens": 11, "outputTokens": 7},
            "commits": [{"sha": "abc123"}],
            "pullRequests": [{"url": "https://example.invalid/pr/1"}],
            "todos": [{"content": "redacted plan item"}]
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(7, 8).unwrap();
        let record_id =
            cursor_composer_native_record_id("composer-redacted", "bubble-redacted").unwrap();
        let envelope = normalize_cursor_composer_observation(
            &native,
            "composer-redacted",
            record_id.clone(),
            range,
            7,
        )
        .unwrap();
        let rendered = format!("{envelope:?}");
        for fact in [
            "Message",
            "ToolInvocation",
            "ToolResult",
            "Reasoning",
            "Usage",
            "Git",
            "Workflow",
        ] {
            assert!(rendered.contains(fact), "missing canonical fact {fact}");
        }
        assert!(rendered.contains("SnapshotOrder"));
        assert!(rendered.contains(record_id.as_str()));
        assert!(!rendered.contains("/secret/workspace"));
        assert!(!rendered.contains("credential-redacted"));
        assert!(!rendered.contains("secret result"));
    }
}
