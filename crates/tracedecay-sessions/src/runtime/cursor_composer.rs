//! Cursor **composer** transcript ingestion.
//!
//! Cursor's primary chat history does not live in the
//! `~/.cursor/projects/<slug>/agent-transcripts/**.jsonl` files that
//! [`crate::runtime::cursor`] sweeps — those cover only a slice of activity.
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
//! Each composer session's watermark (its bubble/header count, since
//! `lastUpdatedAt` is `null` for the vast majority of envelopes) is persisted
//! in the shared `parse_offsets` table under a `cursor-composer:<composerId>`
//! key, so a sweep re-reads a session's bubbles only when it grew. Because a
//! composer session id equals the stem of its JSONL transcript for ~94% of
//! sessions, the composer sweep runs *before* the JSONL
//! [`crate::runtime::cursor::CursorSweepSource`] and hands it the set of
//! composer-owned session ids to skip, so the richer composer rows win and no
//! message row is ever double-ingested.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use libsql::{Builder, OpenFlags};
use serde_json::{Value, json};

use crate::runtime::shared::path_belongs_to_project;
use crate::runtime::source::{StoredCursor, TranscriptIngestStore};
use crate::{SessionMessageRecord, SessionRecord};

/// `SQLITE_OPEN_URI` — not exposed by libsql's [`OpenFlags`], so we OR the raw
/// bit in (libsql forwards `flags.bits()` verbatim to `sqlite3_open_v2`). This
/// makes `SQLite` interpret the `file:…?immutable=1` URI filename.
const SQLITE_OPEN_URI: i32 = 0x0000_0040;

/// Provider id shared with the JSONL Cursor source so both land in the same
/// per-project `sessions.db` namespace and dedupe by `(provider, message_id)`.
const PROVIDER: &str = "cursor";

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

impl CursorComposerSource {
    /// Source rooted at the real user home. `None` when it cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = super::home_dir()?;
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
    pub async fn ingest<S>(
        &self,
        db: &S,
        project_root: &Path,
        envelope_cap: usize,
    ) -> CursorComposerSweepOutcome
    where
        S: TranscriptIngestStore,
    {
        let mut outcome = CursorComposerSweepOutcome::default();
        // ws-hash -> workspace fsPath, harvested from envelopes so per-session
        // store.db files (which key only by ws-hash) can be scoped to a project.
        let mut workspace_paths: HashMap<String, String> = HashMap::new();
        self.ingest_state_vscdb(
            db,
            Some(project_root),
            &[],
            envelope_cap,
            &mut outcome,
            &mut workspace_paths,
        )
        .await;
        self.ingest_chat_store_dbs(db, Some(project_root), &[], &workspace_paths, &mut outcome)
            .await;
        outcome
    }

    pub async fn ingest_user<S>(
        &self,
        db: &S,
        registered_roots: &[PathBuf],
        envelope_cap: usize,
    ) -> CursorComposerSweepOutcome
    where
        S: TranscriptIngestStore,
    {
        let mut outcome = CursorComposerSweepOutcome::default();
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(
            db,
            None,
            registered_roots,
            envelope_cap,
            &mut outcome,
            &mut workspace_paths,
        )
        .await;
        self.ingest_chat_store_dbs(db, None, registered_roots, &workspace_paths, &mut outcome)
            .await;
        outcome
    }

    async fn ingest_state_vscdb<S>(
        &self,
        db: &S,
        project_root: Option<&Path>,
        registered_roots: &[PathBuf],
        envelope_cap: usize,
        outcome: &mut CursorComposerSweepOutcome,
        workspace_paths: &mut HashMap<String, String>,
    ) where
        S: TranscriptIngestStore,
    {
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
            let selected_project = match project_root {
                Some(root) if path_belongs_to_project(Path::new(&project.path), root) => {
                    ComposerProject {
                        path: project.path.clone(),
                    }
                }
                Some(_) => continue,
                None if registered_roots
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
            let watermark = headers.len() as u64;
            let offset_key = format!("cursor-composer:{composer_id}");
            let prev = db.load_cursor(&offset_key).await;
            let last_updated = epoch_secs_u64(envelope_epoch(&envelope, "lastUpdatedAt"));
            // Unchanged since last pass -> skip without touching bubbles.
            if watermark != 0 && watermark <= prev.position && prev.mtime == last_updated {
                continue;
            }
            if ingested_this_pass >= envelope_cap {
                // Deferred to a later pass; still owned so JSONL stands down.
                continue;
            }

            let messages = self
                .build_composer_messages(conn, composer_id, &envelope, &headers)
                .await;
            if messages.is_empty() {
                continue;
            }
            let session = composer_session(composer_id, &envelope, &selected_project, &messages);
            let advanced = StoredCursor {
                position: watermark,
                mtime: last_updated,
                file_id: 0,
            };
            if db
                .upsert_transcript(&session, &messages, &[], &[], &offset_key, advanced)
                .await
            {
                ingested_this_pass += 1;
                outcome.add(1, messages.len() as u64);
            }
        }
    }

    /// Fetch and map every bubble referenced by the envelope's ordered header
    /// list into provider-neutral rows.
    async fn build_composer_messages(
        &self,
        conn: &libsql::Connection,
        composer_id: &str,
        envelope: &Value,
        headers: &[Value],
    ) -> Vec<SessionMessageRecord> {
        let model = envelope
            .get("modelConfig")
            .and_then(|c| c.get("modelName"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut messages = Vec::new();
        let mut ordinal: i64 = 0;
        for header in headers {
            let Some(bubble_id) = header.get("bubbleId").and_then(Value::as_str) else {
                continue;
            };
            let Some(bubble) = fetch_bubble(conn, composer_id, bubble_id).await else {
                continue;
            };
            append_bubble_rows(
                &mut messages,
                &mut ordinal,
                composer_id,
                bubble_id,
                &bubble,
                model.as_deref(),
            );
        }
        append_plan_row(&mut messages, &mut ordinal, composer_id, envelope);
        messages
    }

    async fn ingest_chat_store_dbs<S>(
        &self,
        db: &S,
        project_root: Option<&Path>,
        registered_roots: &[PathBuf],
        workspace_paths: &HashMap<String, String>,
        outcome: &mut CursorComposerSweepOutcome,
    ) where
        S: TranscriptIngestStore,
    {
        let Ok(ws_entries) = std::fs::read_dir(&self.chats_dir) else {
            return;
        };
        for ws_entry in ws_entries.flatten() {
            if !ws_entry.path().is_dir() {
                continue;
            }
            let ws_hash = ws_entry.file_name().to_string_lossy().to_string();
            // Scope by ws-hash -> project mapping harvested from the envelopes.
            let project_path = match (workspace_paths.get(&ws_hash), project_root) {
                (Some(path), Some(root)) if path_belongs_to_project(Path::new(path), root) => {
                    path.clone()
                }
                (Some(_), Some(_)) | (None, _) => continue,
                (Some(path), None)
                    if registered_roots
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
                self.ingest_one_store_db(db, &store_path, &project_path, outcome)
                    .await;
            }
        }
    }

    async fn ingest_one_store_db<S>(
        &self,
        db: &S,
        store_path: &Path,
        project_path: &str,
        outcome: &mut CursorComposerSweepOutcome,
    ) where
        S: TranscriptIngestStore,
    {
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

        let offset_key = format!("cursor-chat:{}", meta.agent_id);
        let prev = db.load_cursor(&offset_key).await;
        let watermark = ordered.len() as u64;
        let created_secs = epoch_secs_u64(meta.created_at);
        if watermark != 0 && watermark <= prev.position && prev.mtime == created_secs {
            return;
        }

        let mut messages = Vec::new();
        for (ordinal, (role, content)) in ordered.iter().enumerate() {
            let text = crate::runtime::shared::message_storage_text(content);
            if text.trim().is_empty() {
                continue;
            }
            messages.push(SessionMessageRecord {
                provider: PROVIDER.to_string(),
                message_id: format!("{session_id}:{ordinal}"),
                session_id: session_id.clone(),
                role: role.clone(),
                timestamp: meta.created_at,
                ordinal: ordinal as i64,
                text,
                kind: Some("message".to_string()),
                model: None,
                tool_names: None,
                source_path: Some(store_path.to_string_lossy().to_string()),
                source_offset: Some(ordinal as i64),
                metadata_json: serde_json::to_string(&json!({
                    "source": "cursor_chat_store",
                    "agent_id": meta.agent_id,
                    "chat_mode": meta.mode,
                }))
                .ok(),
            });
        }
        if messages.is_empty() {
            return;
        }
        let session = SessionRecord {
            provider: PROVIDER.to_string(),
            session_id: session_id.clone(),
            project_key: project_path.to_string(),
            project_path: project_path.to_string(),
            title: meta
                .name
                .clone()
                .or_else(|| crate::runtime::shared::title_from_messages(&messages)),
            started_at: meta.created_at,
            ended_at: messages.last().and_then(|m| m.timestamp),
            transcript_path: Some(store_path.to_string_lossy().to_string()),
            metadata_json: serde_json::to_string(&json!({
                "source": "cursor_chat_store",
                "agent_id": meta.agent_id,
                "chat_mode": meta.mode,
            }))
            .ok(),
            parent_session_id: None,
            is_subagent: false,
            agent_id: Some(meta.agent_id.clone()),
            parent_tool_use_id: None,
        };
        let advanced = StoredCursor {
            position: watermark,
            mtime: created_secs,
            file_id: 0,
        };
        if db
            .upsert_transcript(&session, &messages, &[], &[], &offset_key, advanced)
            .await
        {
            outcome.add(1, messages.len() as u64);
        }
    }
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

/// Emit the provider-neutral rows for one bubble, in transcript order:
/// tool call(s) → reasoning → message text, followed by any PR links.
fn append_bubble_rows(
    messages: &mut Vec<SessionMessageRecord>,
    ordinal: &mut i64,
    composer_id: &str,
    bubble_id: &str,
    bubble: &Value,
    model: Option<&str>,
) {
    let role = bubble_role(bubble);
    let timestamp = bubble_epoch(bubble, "createdAt");
    let usage = bubble_usage(bubble);

    // Tool call (`toolFormerData`).
    if let Some(tfd) = bubble.get("toolFormerData").filter(|v| !v.is_null()) {
        let name = tfd.get("name").and_then(Value::as_str).unwrap_or("tool");
        let status = tfd.get("status").and_then(Value::as_str).unwrap_or("");
        let kind = if is_edit_tool(name) {
            "file_edit"
        } else {
            "tool_call"
        };
        let metadata = json!({
            "source": "cursor_composer",
            "tool": tfd.get("tool").cloned().unwrap_or(Value::Null),
            "tool_name": name,
            "status": status,
            "tool_call_id": tfd.get("toolCallId").cloned().unwrap_or(Value::Null),
            "params_bytes": json_field_len(tfd.get("params")),
            "result_bytes": json_field_len(tfd.get("result")),
        });
        push_row(
            messages,
            ordinal,
            format!("{composer_id}:{bubble_id}:tool"),
            composer_id,
            &role,
            timestamp,
            format!("{name} ({status})").trim().to_string(),
            kind,
            model,
            Some(name.to_string()),
            &metadata,
        );
    }

    // Reasoning / thinking.
    if let Some(thinking) = bubble
        .get("thinking")
        .and_then(|t| t.get("text"))
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    {
        push_row(
            messages,
            ordinal,
            format!("{composer_id}:{bubble_id}:thinking"),
            composer_id,
            &role,
            timestamp,
            thinking.to_string(),
            "reasoning",
            model,
            None,
            &json!({ "source": "cursor_composer" }),
        );
    }

    // Visible message text.
    if let Some(text) = bubble
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
    {
        let mut metadata = json!({
            "source": "cursor_composer",
            "bubble_type": bubble.get("type").cloned().unwrap_or(Value::Null),
        });
        merge_git_metadata(&mut metadata, bubble);
        if let Some(usage) = usage.clone() {
            metadata["usage"] = usage;
        }
        push_row(
            messages,
            ordinal,
            format!("{composer_id}:{bubble_id}"),
            composer_id,
            &role,
            timestamp,
            text.to_string(),
            "message",
            model,
            None,
            &metadata,
        );
    }

    // Pull-request links.
    if let Some(prs) = bubble.get("pullRequests").and_then(Value::as_array) {
        for (index, pr) in prs.iter().enumerate() {
            push_row(
                messages,
                ordinal,
                format!("{composer_id}:{bubble_id}:pr:{index}"),
                composer_id,
                &role,
                timestamp,
                pr_link_text(pr),
                "pr_link",
                model,
                None,
                &json!({ "source": "cursor_composer", "pull_request": pr.clone() }),
            );
        }
    }
}

/// One `plan` row per session carrying the envelope's todo list.
fn append_plan_row(
    messages: &mut Vec<SessionMessageRecord>,
    ordinal: &mut i64,
    composer_id: &str,
    envelope: &Value,
) {
    let Some(todos) = envelope.get("todos").and_then(Value::as_array) else {
        return;
    };
    if todos.is_empty() {
        return;
    }
    let text = todos
        .iter()
        .filter_map(|t| t.get("content").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return;
    }
    let items: Vec<Value> = todos
        .iter()
        .map(|t| {
            json!({
                "id": t.get("id").cloned().unwrap_or(Value::Null),
                "content": t.get("content").cloned().unwrap_or(Value::Null),
                "status": t.get("status").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    push_row(
        messages,
        ordinal,
        format!("{composer_id}:plan"),
        composer_id,
        "assistant",
        None,
        text,
        "plan",
        None,
        None,
        &json!({ "source": "cursor_composer", "todos": items }),
    );
}

#[allow(clippy::too_many_arguments)]
fn push_row(
    messages: &mut Vec<SessionMessageRecord>,
    ordinal: &mut i64,
    message_id: String,
    composer_id: &str,
    role: &str,
    timestamp: Option<i64>,
    text: String,
    kind: &str,
    model: Option<&str>,
    tool_names: Option<String>,
    metadata: &Value,
) {
    let current = *ordinal;
    *ordinal += 1;
    messages.push(SessionMessageRecord {
        provider: PROVIDER.to_string(),
        message_id,
        session_id: composer_id.to_string(),
        role: role.to_string(),
        timestamp,
        ordinal: current,
        text,
        kind: Some(kind.to_string()),
        model: model.map(str::to_string),
        tool_names,
        source_path: None,
        source_offset: Some(current),
        metadata_json: serde_json::to_string(metadata).ok(),
    });
}

fn composer_session(
    composer_id: &str,
    envelope: &Value,
    project: &ComposerProject,
    messages: &[SessionMessageRecord],
) -> SessionRecord {
    let created = envelope_epoch(envelope, "createdAt");
    let ended = envelope_epoch(envelope, "lastUpdatedAt")
        .or_else(|| messages.last().and_then(|m| m.timestamp));
    let title = envelope
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .or_else(|| crate::runtime::shared::title_from_messages(messages));
    let mut metadata = json!({
        "source": "cursor_composer",
        "composer_id": composer_id,
        "unified_mode": envelope.get("unifiedMode").cloned().unwrap_or(Value::Null),
        "subagent_composer_ids": envelope.get("subagentComposerIds").cloned().unwrap_or(Value::Null),
        "context_tokens_used": envelope.get("contextTokensUsed").cloned().unwrap_or(Value::Null),
    });
    if let Some(breakdown) = envelope.get("promptTokenBreakdown") {
        metadata["prompt_token_breakdown"] = breakdown.clone();
    }
    if let Some(repos) = envelope.get("trackedGitRepos") {
        metadata["tracked_git_repos"] = repos.clone();
    }
    SessionRecord {
        provider: PROVIDER.to_string(),
        session_id: composer_id.to_string(),
        project_key: project.path.clone(),
        project_path: project.path.clone(),
        title,
        started_at: created,
        ended_at: ended,
        transcript_path: Some(format!("cursor-composer:{composer_id}")),
        metadata_json: serde_json::to_string(&metadata).ok(),
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

/// Map Cursor bubble `type` to a provider-neutral role (1 = user, 2 =
/// assistant); anything else defaults to assistant so tool/reasoning rows stay
/// attributed to the model side.
fn bubble_role(bubble: &Value) -> String {
    match bubble.get("type").and_then(Value::as_i64) {
        Some(1) => "user".to_string(),
        _ => "assistant".to_string(),
    }
}

/// Cursor stores token counts as `{inputTokens,outputTokens}` (camelCase),
/// which the shared usage extractor does not recognize — normalize to the
/// `snake_case` shape the savings dashboard reads.
fn bubble_usage(bubble: &Value) -> Option<Value> {
    let counts = bubble.get("tokenCount")?;
    let input = counts.get("inputTokens").and_then(Value::as_i64);
    let output = counts.get("outputTokens").and_then(Value::as_i64);
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(json!({
        "input_tokens": input.unwrap_or(0),
        "output_tokens": output.unwrap_or(0),
    }))
}

fn merge_git_metadata(metadata: &mut Value, bubble: &Value) {
    for (src, dst) in [
        ("commits", "commits"),
        ("gitDiffs", "git_diffs"),
        ("pullRequests", "pull_requests"),
    ] {
        if let Some(value) = bubble.get(src).filter(|v| {
            v.as_array().is_some_and(|a| !a.is_empty()) || (!v.is_array() && !v.is_null())
        }) {
            metadata[dst] = value.clone();
        }
    }
}

fn pr_link_text(pr: &Value) -> String {
    for key in ["url", "htmlUrl", "html_url", "title", "name"] {
        if let Some(value) = pr
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            return value.to_string();
        }
    }
    serde_json::to_string(pr).unwrap_or_default()
}

fn is_edit_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "edit",
        "apply",
        "write",
        "create_file",
        "search_replace",
        "delete_file",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn json_field_len(value: Option<&Value>) -> u64 {
    value.map_or(0, |v| {
        v.as_str().map_or_else(
            || serde_json::to_string(v).map(|s| s.len()).unwrap_or(0),
            str::len,
        ) as u64
    })
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

/// Envelope epoch fields are milliseconds; convert to the seconds the session
/// tables use. Zero/absent yields `None`.
fn envelope_epoch(envelope: &Value, key: &str) -> Option<i64> {
    epoch_ms_to_secs(envelope.get(key).and_then(Value::as_i64))
}

fn bubble_epoch(bubble: &Value, key: &str) -> Option<i64> {
    epoch_ms_to_secs(bubble.get(key).and_then(Value::as_i64))
}

fn epoch_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.filter(|v| *v > 0).map(|v| v / 1000)
}

/// Epoch seconds as the `u64` the `parse_offsets.mtime` column stores (0 when
/// absent), used as part of the composer watermark.
fn epoch_secs_u64(secs: Option<i64>) -> u64 {
    u64::try_from(secs.unwrap_or(0)).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// store.db blob-DAG reader
// ---------------------------------------------------------------------------

struct StoreMeta {
    agent_id: String,
    latest_root_blob_id: Option<String>,
    name: Option<String>,
    mode: Option<String>,
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
        name: meta
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty())
            .map(str::to_string),
        mode: meta.get("mode").and_then(Value::as_str).map(str::to_string),
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
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
