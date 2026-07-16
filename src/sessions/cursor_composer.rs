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
//! (`key = ?`) point lookups for bubbles. No full-table scans. Every `TEXT` /
//! `BLOB` payload is length-gated in SQL (`length` + conditional materialize)
//! against the shared observation / JSONL frame ceilings and the pass byte
//! budget before any Rust `String`/`Vec`/`serde_json::Value` allocation.
//! `store.db` blobs are fetched by id while walking the reachable DAG — never
//! collected via `SELECT id, data FROM blobs`.
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
#[cfg(test)]
use tracedecay_domain::{CanonicalObservationFactV1, CanonicalWorkflowSemanticKindV1};
use tracedecay_domain::{
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::ObservationCoverageReason;

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::{MAX_OBSERVATION_RECORD_BYTES, parse_normalized_observation_record_v1};
use crate::sessions::ingest_byte_budget::IngestByteBudget;
use crate::sessions::shared::path_belongs_to_project;
use crate::sessions::snapshot_observation::MAX_SNAPSHOT_METADATA_BYTES;
use crate::sessions::source::{MAX_JSONL_RECORD_BYTES, TranscriptIngestError};

mod observation;

use observation::{
    composer_todos_have_admittable_items, normalize_cursor_composer_envelope_observation,
    normalize_cursor_composer_observation_with_message_id,
};
#[cfg(test)]
pub(crate) use observation::{
    normalize_cursor_composer_observation,
    normalize_cursor_composer_observation_with_projected_message_id,
};

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
    build_cursor_composer_capture_request_for_project(
        composer_id,
        bubble_id,
        bubble,
        None,
        None,
        scope,
        generation,
        position,
        expected_cursor,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_cursor_composer_capture_request_for_project(
    composer_id: &str,
    bubble_id: &str,
    bubble: &Value,
    project_path: Option<&str>,
    envelope: Option<&Value>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    position: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> Result<CaptureObservationRequest, String> {
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer position: {error}"))?;
    let native = composer_observation_with_session(bubble, project_path, envelope);
    let encoded = serde_json::to_vec(&native)
        .map_err(|error| format!("could not encode Cursor composer bubble: {error}"))?;
    let native_record_id = cursor_composer_native_record_id(composer_id, bubble_id)?;
    let projected_message_id = ObservationId::new(format!("{composer_id}:{bubble_id}"))
        .map_err(|error| format!("invalid Cursor composer V1 message identity: {error}"))?;
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            normalize_cursor_composer_observation_with_message_id(
                &native,
                composer_id,
                native_record_id.clone(),
                projected_message_id.clone(),
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

fn composer_observation_with_session(
    bubble: &Value,
    project_path: Option<&str>,
    envelope: Option<&Value>,
) -> Value {
    let mut native = bubble.clone();
    if let Some(object) = native.as_object_mut() {
        if let Some(project_path) = project_path {
            object.insert(
                "tracedecayProjectPath".to_string(),
                Value::String(project_path.to_string()),
            );
        }
        if let Some(envelope) = envelope {
            for (key, value) in [
                ("tracedecaySessionTitle", envelope.get("name")),
                (
                    "tracedecaySessionModel",
                    envelope.pointer("/modelConfig/modelName"),
                ),
                ("tracedecaySessionStartedAt", envelope.get("createdAt")),
                ("tracedecaySessionEndedAt", envelope.get("lastUpdatedAt")),
            ] {
                if let Some(value) = value.filter(|value| !value.is_null()) {
                    object.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    native
}

pub async fn capture_cursor_composer_observation(
    db: &GlobalDb,
    request: CaptureObservationRequest,
) -> Result<CaptureObservationOutcome, TranscriptIngestError> {
    let authorities = match request.scope() {
        ObservationScopeV1::Project { project_id } => {
            HostAdmissionAuthorities::for_project(db, project_id.clone())
        }
        ObservationScopeV1::Profile => HostAdmissionAuthorities::for_profile(db),
    };
    HostAdmissionFacade::new(authorities)
        .capture_observation(request)
        .await
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })
}

/// Capture a `composerData:<composerId>` envelope observation when native
/// `todos[{id,content,status}]` are present. Uses a distinct `source_key` so the
/// bubble cursor stream is not displaced by envelope list updates.
///
/// Envelope native identity includes a todo checkpoint (authentic
/// `lastUpdatedAt` when present, otherwise a deterministic content fingerprint
/// over native todo id/content/status/order) so pending→completed and content
/// or order revisions admit as new observations without inventing
/// `WorkflowLifecycle.revision`.
pub fn build_cursor_composer_envelope_capture_request(
    composer_id: &str,
    envelope: &Value,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> Result<CaptureObservationRequest, String> {
    build_cursor_composer_envelope_capture_request_for_project(
        composer_id,
        envelope,
        None,
        scope,
        generation,
        expected_cursor,
    )
}

fn build_cursor_composer_envelope_capture_request_for_project(
    composer_id: &str,
    envelope: &Value,
    project_path: Option<&str>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> Result<CaptureObservationRequest, String> {
    let position = expected_cursor
        .as_ref()
        .filter(|cursor| cursor.generation() == generation)
        .map_or(0, ObservationSourceCursorV1::position);
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer envelope position: {error}"))?;
    let checkpoint = composer_envelope_todo_checkpoint(envelope)
        .ok_or_else(|| "Cursor composer envelope has no admittable todo checkpoint".to_string())?;
    let encoded = serde_json::to_vec(envelope)
        .map_err(|error| format!("could not encode Cursor composer envelope: {error}"))?;
    let native_record_id = cursor_composer_envelope_native_record_id(composer_id, checkpoint)?;
    let parsed = parse_normalized_observation_record_v1(
        &encoded,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        |native| {
            normalize_cursor_composer_envelope_observation(
                &native,
                composer_id,
                project_path,
                native_record_id.clone(),
                range,
                position,
            )
        },
    )
    .map_err(|error| format!("could not parse Cursor composer envelope: {error}"))?;
    let source = cursor_composer_envelope_source(composer_id)?;
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        generation,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        native_record_id,
    )
    .map_err(|error| format!("invalid Cursor composer envelope identity: {error}"))?;
    CaptureObservationRequest::new(
        parsed,
        identity,
        expected_cursor,
        RetentionClass::new(COMPOSER_OBSERVATION_RETENTION)
            .map_err(|error| format!("invalid Cursor composer retention: {error}"))?,
        ObservationCancellation::default(),
    )
    .map_err(|error| format!("invalid Cursor composer envelope capture request: {error}"))
    .map(|request| request.with_resume_checkpoint(generation.file_id(), checkpoint))
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

/// Checkpoint for mutable envelope todos. The checked-in provider fixture has
/// `lastUpdatedAt: null`, so use only native todo id/content/status in provider
/// array order and never invent revision semantics.
fn composer_envelope_todo_checkpoint(native: &Value) -> Option<u64> {
    let todos = native.get("todos")?.as_array()?;
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.cursor-composer-todo-checkpoint.v1\0");
    let mut any = false;
    for (index, todo) in todos.iter().enumerate() {
        let Some(item_id) = todo
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        let Some(content) = todo
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty())
        else {
            continue;
        };
        any = true;
        hasher.update(u64::try_from(index).ok()?.to_le_bytes());
        hasher.update(u64::try_from(item_id.len()).ok()?.to_le_bytes());
        hasher.update(item_id.as_bytes());
        hasher.update(u64::try_from(content.len()).ok()?.to_le_bytes());
        hasher.update(content.as_bytes());
        if let Some(status) = todo
            .get("status")
            .and_then(Value::as_str)
            .filter(|status| !status.trim().is_empty())
        {
            hasher.update([1]);
            hasher.update(u64::try_from(status.len()).ok()?.to_le_bytes());
            hasher.update(status.as_bytes());
        } else {
            hasher.update([0]);
        }
    }
    if !any {
        return None;
    }
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    Some(u64::from_le_bytes(bytes).max(1))
}

fn cursor_composer_envelope_native_record_id(
    composer_id: &str,
    checkpoint: u64,
) -> Result<ObservationId, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.cursor-composer-envelope.v1\0");
    hasher.update(composer_id.as_bytes());
    hasher.update([0]);
    hasher.update(checkpoint.to_le_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|error| {
            format!("could not encode Cursor composer envelope identity: {error}")
        })?;
    }
    ObservationId::new(format!("cursor.composer.envelope.sha256:{encoded}"))
        .map_err(|error| format!("invalid Cursor composer envelope native identity: {error}"))
}

fn cursor_composer_envelope_source(
    composer_id: &str,
) -> Result<ObservationSourceIdentityV1, String> {
    let source_key = SessionId::new(format!("{composer_id}:composerData"))
        .map_err(|error| format!("invalid Cursor composer envelope source key: {error}"))?;
    ObservationSourceIdentityV1::for_provider_source(
        ProviderId::new(PROVIDER)
            .map_err(|error| format!("invalid Cursor provider id: {error}"))?,
        SessionId::new(composer_id)
            .map_err(|error| format!("invalid Cursor composer id: {error}"))?,
        source_key,
    )
    .map_err(|error| format!("invalid Cursor composer envelope source: {error}"))
}

/// Default ceiling on how many *new/changed* composer sessions one sweep pass
/// ingests, so the first backfill of thousands of sessions never blocks
/// startup; already-watermarked sessions are skipped cheaply and do not count.
pub const DEFAULT_COMPOSER_ENVELOPE_CAP: usize = 256;

/// Maximum bytes materializable for one `composerData:` session envelope.
/// Reuses the JSONL frame ceiling so long header lists stay within one
/// transcript-frame-sized allocation.
const MAX_COMPOSER_ENVELOPE_BYTES: u64 = MAX_JSONL_RECORD_BYTES as u64;
/// Default cumulative sweep ceiling: one maximum-size envelope plus the byte
/// needed by bounded readers to prove that a record crossed the ceiling.
const DEFAULT_COMPOSER_SWEEP_BYTES: u64 = MAX_COMPOSER_ENVELOPE_BYTES + 1;

/// Maximum bytes materializable for `store.db` `meta` hex/JSON.
const MAX_COMPOSER_STORE_META_BYTES: u64 = MAX_SNAPSHOT_METADATA_BYTES;
/// `meta.value` is hexadecimal text, so its encoded byte ceiling is twice the
/// decoded metadata ceiling.
const MAX_COMPOSER_STORE_META_HEX_BYTES: u64 = MAX_COMPOSER_STORE_META_BYTES * 2;

/// Cap on DAG blob visits / child refs per store — aligns with the default
/// ingest discovery unit ceiling (`IngestPassBounds::discovered_units`).
const MAX_COMPOSER_STORE_BLOB_VISITS: usize = 4096;

/// Maximum UTF-8 bytes in one `SQLite` key / blob id.
const MAX_COMPOSER_SQLITE_KEY_BYTES: u64 = 512;

/// Outcome of one composer sweep pass.
#[derive(Debug, Default, Clone)]
pub struct CursorComposerSweepOutcome {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
    /// Serialized bytes of new observation payloads processed by this pass.
    pub bytes_consumed: u64,
    /// At least one new observation was deferred by the aggregate byte cap.
    pub deferred_by_byte_cap: bool,
    /// Bounded set of composer session ids observed during the sweep. The
    /// JSONL sweep skips these so the two Cursor sources do not double-ingest
    /// the same session within the bounded discovery window.
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
    db: &'a GlobalDb,
    facade: HostAdmissionFacade<'a>,
    scope: ObservationScopeV1,
    project_root: Option<&'a Path>,
    registered_roots: &'a [PathBuf],
}

/// Outcome of a length-gated `SQLite` text/blob fetch that never materializes
/// oversized or over-budget payloads into `Rust`.
#[derive(Debug)]
enum BoundedSqliteValue<T> {
    Missing,
    Ready { byte_len: u64, value: T },
    Oversized { byte_len: u64 },
    BudgetExceeded { byte_len: u64 },
    Malformed { byte_len: u64 },
}

fn effective_sqlite_cap(max_bytes: u64, remaining: Option<u64>) -> u64 {
    match remaining {
        Some(remaining) => remaining.min(max_bytes),
        None => max_bytes,
    }
}

fn composer_payload_bytes(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .ok()
        .and_then(|encoded| u64::try_from(encoded.len()).ok())
        .unwrap_or(u64::MAX)
}

fn max_composer_record_bytes() -> u64 {
    u64::try_from(MAX_OBSERVATION_RECORD_BYTES).unwrap_or(u64::MAX)
}

fn composer_source_charge(bytes: u64) -> u64 {
    bytes.min(max_composer_record_bytes().saturating_add(1))
}

fn composer_budget_bytes(value: &Value) -> u64 {
    composer_payload_bytes(value).min(max_composer_record_bytes().saturating_add(1))
}

fn composer_id_from_envelope_key(key: &str) -> Option<&str> {
    key.strip_prefix("composerData:")
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
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
        project_id: ProjectId,
        envelope_cap: usize,
    ) -> CursorComposerSweepOutcome {
        self.ingest_capped(
            db,
            project_root,
            project_id,
            envelope_cap,
            Some(DEFAULT_COMPOSER_SWEEP_BYTES),
        )
        .await
    }

    /// [`Self::ingest`] with one aggregate serialized-payload byte budget
    /// shared across every composer store discovered during the pass.
    pub async fn ingest_capped(
        &self,
        db: &GlobalDb,
        project_root: &Path,
        project_id: ProjectId,
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
    ) -> CursorComposerSweepOutcome {
        let mut outcome = CursorComposerSweepOutcome::default();
        let mut byte_budget =
            IngestByteBudget::bounded(max_new_bytes.unwrap_or(DEFAULT_COMPOSER_SWEEP_BYTES));
        let context = ComposerIngestContext {
            db,
            facade: HostAdmissionFacade::new(HostAdmissionAuthorities::for_project(
                db,
                project_id.clone(),
            )),
            scope: ObservationScopeV1::Project { project_id },
            project_root: Some(project_root),
            registered_roots: &[],
        };
        drain_composer_projection_queue(&context).await;
        // ws-hash -> workspace fsPath, harvested from envelopes so per-session
        // store.db files (which key only by ws-hash) can be scoped to a project.
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(
            &context,
            envelope_cap,
            &mut byte_budget,
            &mut outcome,
            &mut workspace_paths,
        )
        .await;
        self.ingest_chat_store_dbs(&context, &workspace_paths, &mut byte_budget, &mut outcome)
            .await;
        drain_composer_projection_queue(&context).await;
        outcome.bytes_consumed = byte_budget.consumed();
        outcome.deferred_by_byte_cap = byte_budget.deferred();
        outcome
    }

    pub async fn ingest_user(
        &self,
        db: &GlobalDb,
        registered_roots: &[PathBuf],
        envelope_cap: usize,
    ) -> CursorComposerSweepOutcome {
        self.ingest_user_capped(
            db,
            registered_roots,
            envelope_cap,
            Some(DEFAULT_COMPOSER_SWEEP_BYTES),
        )
        .await
    }

    /// [`Self::ingest_user`] with one aggregate serialized-payload byte budget
    /// shared across every composer store discovered during the pass.
    pub async fn ingest_user_capped(
        &self,
        db: &GlobalDb,
        registered_roots: &[PathBuf],
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
    ) -> CursorComposerSweepOutcome {
        let mut outcome = CursorComposerSweepOutcome::default();
        let mut byte_budget =
            IngestByteBudget::bounded(max_new_bytes.unwrap_or(DEFAULT_COMPOSER_SWEEP_BYTES));
        let context = ComposerIngestContext {
            db,
            facade: HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(db)),
            scope: ObservationScopeV1::Profile,
            project_root: None,
            registered_roots,
        };
        drain_composer_projection_queue(&context).await;
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(
            &context,
            envelope_cap,
            &mut byte_budget,
            &mut outcome,
            &mut workspace_paths,
        )
        .await;
        self.ingest_chat_store_dbs(&context, &workspace_paths, &mut byte_budget, &mut outcome)
            .await;
        drain_composer_projection_queue(&context).await;
        outcome.bytes_consumed = byte_budget.consumed();
        outcome.deferred_by_byte_cap = byte_budget.deferred();
        outcome
    }

    async fn ingest_state_vscdb(
        &self,
        context: &ComposerIngestContext<'_>,
        envelope_cap: usize,
        byte_budget: &mut IngestByteBudget,
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
        // Indexed prefix scan of keys + byte lengths only — never SELECT full
        // envelope text here. Point-fetch materializes only when the UTF-8 byte
        // length fits both ceilings.
        let Ok(mut rows) = conn
            .query(
                "SELECT key, length(CAST(value AS BLOB)) AS nbytes \
                 FROM cursorDiskKV \
                 WHERE key >= 'composerData:' AND key < 'composerData;' \
                   AND length(CAST(key AS BLOB)) <= ?1",
                libsql::params![MAX_COMPOSER_SQLITE_KEY_BYTES as i64],
            )
            .await
        else {
            return;
        };

        let mut ingested_this_pass = 0usize;
        while let Ok(Some(row)) = rows.next().await {
            let Ok(key) = row.get::<String>(0) else {
                continue;
            };
            let Some(nbytes) = row.get::<i64>(1).ok().filter(|n| *n >= 0).map(|n| n as u64) else {
                continue;
            };
            if nbytes > MAX_COMPOSER_ENVELOPE_BYTES {
                if !byte_budget
                    .try_consume(nbytes.min(MAX_COMPOSER_ENVELOPE_BYTES.saturating_add(1)))
                {
                    break;
                }
                continue;
            }
            if byte_budget.exhausted() {
                byte_budget.defer();
                break;
            }
            if byte_budget
                .remaining()
                .is_some_and(|remaining| nbytes > remaining)
            {
                byte_budget.defer();
                break;
            }
            let value = match fetch_kv_text_bounded(
                conn,
                &key,
                MAX_COMPOSER_ENVELOPE_BYTES,
                byte_budget.remaining(),
            )
            .await
            {
                BoundedSqliteValue::Ready { value, .. } => value,
                BoundedSqliteValue::BudgetExceeded { .. } => {
                    byte_budget.defer();
                    break;
                }
                BoundedSqliteValue::Oversized { .. }
                | BoundedSqliteValue::Malformed { .. }
                | BoundedSqliteValue::Missing => continue,
            };
            if !byte_budget.try_consume(nbytes) {
                break;
            }
            let Ok(envelope) = serde_json::from_str::<Value>(&value) else {
                continue;
            };
            let Some(composer_id) = envelope
                .get("composerId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
                .map(str::to_string)
                .or_else(|| composer_id_from_envelope_key(&key).map(str::to_string))
            else {
                continue;
            };
            let Some(project) = envelope_project(&envelope) else {
                continue;
            };
            if let Some(ws_hash) = workspace_hash(&envelope) {
                if workspace_paths.contains_key(&ws_hash)
                    || workspace_paths.len() < MAX_COMPOSER_STORE_BLOB_VISITS
                {
                    workspace_paths
                        .entry(ws_hash)
                        .or_insert_with(|| project.path.clone());
                } else {
                    byte_budget.defer();
                }
            }
            let selected_project = match context.project_root {
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
            // Keep JSONL dedupe state bounded independently of SQLite row count.
            if outcome.owned_session_ids.contains(&composer_id)
                || outcome.owned_session_ids.len() < MAX_COMPOSER_STORE_BLOB_VISITS
            {
                outcome.owned_session_ids.insert(composer_id.clone());
            } else {
                byte_budget.defer();
            }

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
            if composer_todos_have_admittable_items(&envelope)
                && let Some(todo_checkpoint) = composer_envelope_todo_checkpoint(&envelope)
                && let Ok(envelope_source) = cursor_composer_envelope_source(&composer_id)
                && let Ok(envelope_expected_cursor) = context
                    .facade
                    .get_source_cursor(&envelope_source, &context.scope)
                    .await
            {
                // Same generation + position is not enough: envelope todos mutate
                // in place. Skip only when the stored resume fingerprint still
                // matches the current todo checkpoint.
                let envelope_already_covered =
                    envelope_expected_cursor.as_ref().is_some_and(|cursor| {
                        cursor.generation() == generation
                            && cursor.position() >= 1
                            && cursor.resume_fingerprint() == Some(todo_checkpoint)
                    });
                if !envelope_already_covered
                    && let Ok(request) = build_cursor_composer_envelope_capture_request_for_project(
                        &composer_id,
                        &envelope,
                        Some(&selected_project.path),
                        context.scope.clone(),
                        generation,
                        envelope_expected_cursor,
                    )
                    && let Ok(outcome) = context.facade.capture_observation(request).await
                    && let CaptureObservationOutcome::Persisted {
                        outcome: persisted, ..
                    } = outcome
                {
                    session_accepted = true;
                    if matches!(persisted, ObservationPersistOutcome::Committed(_)) {
                        messages = messages.saturating_add(1);
                    }
                }
            }
            for (position, header) in headers.iter().enumerate() {
                let Some(bubble_id) = header.get("bubbleId").and_then(Value::as_str) else {
                    continue;
                };
                if context
                    .db
                    .get_session_message(PROVIDER, &format!("{composer_id}:{bubble_id}"))
                    .await
                    .is_some()
                {
                    continue;
                }
                let header_position = position as u64;
                let Ok(source) = cursor_composer_source(&composer_id) else {
                    break;
                };
                let Ok(expected_cursor) = context
                    .facade
                    .get_source_cursor(&source, &context.scope)
                    .await
                else {
                    break;
                };
                let position = expected_cursor.as_ref().map_or(header_position, |cursor| {
                    if cursor.generation() == generation {
                        cursor.position().max(header_position)
                    } else {
                        header_position
                    }
                });
                if byte_budget.exhausted() {
                    byte_budget.defer();
                    break;
                }
                match fetch_bubble_bounded(conn, &composer_id, bubble_id, byte_budget.remaining())
                    .await
                {
                    BoundedSqliteValue::Missing => {}
                    BoundedSqliteValue::Oversized { byte_len } => {
                        if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                            break;
                        }
                        if advance_composer_coverage(
                            ComposerCoverageContext {
                                facade: &context.facade,
                                scope: &context.scope,
                                generation,
                            },
                            source,
                            position,
                            expected_cursor,
                            ObservationCoverageReason::OversizedFrame,
                            None,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    BoundedSqliteValue::Malformed { byte_len } => {
                        if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                            break;
                        }
                        if advance_composer_coverage(
                            ComposerCoverageContext {
                                facade: &context.facade,
                                scope: &context.scope,
                                generation,
                            },
                            source,
                            position,
                            expected_cursor,
                            ObservationCoverageReason::MalformedFrame,
                            None,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    BoundedSqliteValue::BudgetExceeded { .. } => {
                        byte_budget.defer();
                        break;
                    }
                    BoundedSqliteValue::Ready {
                        byte_len,
                        value: bubble,
                    } => {
                        if !byte_budget.try_consume(byte_len.max(composer_budget_bytes(&bubble))) {
                            break;
                        }
                        let request = build_cursor_composer_capture_request_for_project(
                            &composer_id,
                            bubble_id,
                            &bubble,
                            Some(&selected_project.path),
                            Some(&envelope),
                            context.scope.clone(),
                            generation,
                            position,
                            expected_cursor.clone(),
                        );
                        let Ok(request) = request else {
                            if advance_composer_coverage(
                                ComposerCoverageContext {
                                    facade: &context.facade,
                                    scope: &context.scope,
                                    generation,
                                },
                                source,
                                position,
                                expected_cursor,
                                ObservationCoverageReason::MalformedFrame,
                                None,
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
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
                                    Some(receipt),
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
                                    Some(receipt),
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
        byte_budget: &mut IngestByteBudget,
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
            let project_path = match (workspace_paths.get(&ws_hash), context.project_root) {
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
                self.ingest_one_store_db(context, &store_path, &project_path, byte_budget, outcome)
                    .await;
            }
        }
    }

    async fn ingest_one_store_db(
        &self,
        context: &ComposerIngestContext<'_>,
        store_path: &Path,
        project_path: &str,
        byte_budget: &mut IngestByteBudget,
        outcome: &mut CursorComposerSweepOutcome,
    ) {
        let Some(ro) = open_readonly_immutable(store_path).await else {
            return;
        };
        let conn = &ro.conn;
        let meta = match read_store_meta_bounded(conn, byte_budget.remaining()).await {
            BoundedSqliteValue::Ready { byte_len, value } => {
                if !byte_budget.try_consume(byte_len) {
                    return;
                }
                value
            }
            BoundedSqliteValue::BudgetExceeded { .. } => {
                byte_budget.defer();
                return;
            }
            BoundedSqliteValue::Oversized { byte_len }
            | BoundedSqliteValue::Malformed { byte_len } => {
                let _ = byte_budget.try_consume(composer_source_charge(byte_len));
                return;
            }
            BoundedSqliteValue::Missing => return,
        };
        let session_id = format!("cursor-chat:{}", meta.agent_id);
        if outcome.owned_session_ids.contains(&session_id)
            || outcome.owned_session_ids.len() < MAX_COMPOSER_STORE_BLOB_VISITS
        {
            outcome.owned_session_ids.insert(session_id.clone());
        } else {
            byte_budget.defer();
        }

        let ordered = match order_store_messages_bounded(
            conn,
            meta.latest_root_blob_id.as_deref(),
            byte_budget,
        )
        .await
        {
            StoreWalkOutcome::Messages(messages) => messages,
            StoreWalkOutcome::DeferredEmpty => return,
        };
        if ordered.is_empty() {
            return;
        }

        let Some(generation) = snapshot_generation(store_path) else {
            return;
        };
        let Ok(source) = cursor_composer_source(&session_id) else {
            return;
        };
        let mut session_accepted = false;
        let mut messages = 0_u64;
        for (ordinal, (role, content, source_bytes)) in ordered.into_iter().enumerate() {
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
            if byte_budget.exhausted() {
                byte_budget.defer();
                break;
            }
            let text = crate::sessions::shared::message_storage_text(&content);
            if text.trim().is_empty() {
                continue;
            }
            let bubble = json!({
                "type": if role == "user" { 1 } else { 2 },
                "text": text,
                "createdAt": meta.created_at.map(|seconds| seconds.saturating_mul(1000)),
                "tracedecayTranscriptPath": store_path.to_string_lossy(),
            });
            // Reachable blob bytes were charged during the SQL-gated DAG walk.
            // Charge only observation-payload inflation beyond that source size.
            let payload = composer_budget_bytes(&bubble);
            if payload > source_bytes && !byte_budget.try_consume(payload - source_bytes) {
                break;
            }
            let request = build_cursor_composer_capture_request_for_project(
                &session_id,
                &ordinal.to_string(),
                &bubble,
                Some(project_path),
                None,
                context.scope.clone(),
                generation,
                position,
                expected_cursor.clone(),
            );
            let Ok(request) = request else {
                if advance_composer_coverage(
                    ComposerCoverageContext {
                        facade: &context.facade,
                        scope: &context.scope,
                        generation,
                    },
                    source.clone(),
                    position,
                    expected_cursor,
                    ObservationCoverageReason::MalformedFrame,
                    None,
                )
                .await
                .is_err()
                {
                    break;
                }
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
                        Some(receipt),
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
                        Some(receipt),
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

async fn drain_composer_projection_queue(context: &ComposerIngestContext<'_>) {
    if let Err(error) = crate::sessions::claude_observation::drain_projection_queue(
        &context.facade,
        &context.scope,
        &ObservationCancellation::default(),
    )
    .await
    {
        tracing::debug!(?error, "Cursor composer projection drain deferred");
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
    let identity = crate::sessions::source::sqlite_generation_identity(path).ok()?;
    ObservationSourceGenerationV1::new(identity).ok()
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
    receipt: Option<tracedecay_domain::SanitizationReceiptV1>,
) -> Result<(), String> {
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer coverage range: {error}"))?;
    crate::sessions::snapshot_observation::advance_snapshot_coverage_maybe(
        context.facade,
        PROVIDER,
        source,
        range,
        expected_cursor,
        context.scope.clone(),
        context.generation,
        reason,
        receipt,
        &ObservationCancellation::default(),
    )
    .await
    .map_err(|error| error.to_string())
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
    let uri = crate::sqlite_read_snapshot::immutable_uri(db_path).ok()?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::from_bits_retain(SQLITE_OPEN_URI);
    let db = Builder::new_local(uri).flags(flags).build().await.ok()?;
    let conn = db.connect().ok()?;
    // Belt-and-suspenders against ever mutating the live store.
    let _ = conn.execute_batch("PRAGMA query_only = ON;").await;
    Some(ReadOnlyDb { _db: db, conn })
}

async fn fetch_kv_text_bounded(
    conn: &libsql::Connection,
    key: &str,
    max_bytes: u64,
    remaining: Option<u64>,
) -> BoundedSqliteValue<String> {
    let effective_cap = effective_sqlite_cap(max_bytes, remaining);
    let Ok(mut rows) = conn
        .query(
            "SELECT length(CAST(value AS BLOB)) AS nbytes, \
             CASE WHEN length(CAST(value AS BLOB)) <= ?1 THEN value ELSE NULL END AS payload \
             FROM cursorDiskKV WHERE key = ?2",
            libsql::params![effective_cap as i64, key],
        )
        .await
    else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(Some(row)) = rows.next().await else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(nbytes_i) = row.get::<i64>(0) else {
        return BoundedSqliteValue::Missing;
    };
    if nbytes_i < 0 {
        return BoundedSqliteValue::Missing;
    }
    let byte_len = nbytes_i as u64;
    match row.get::<String>(1) {
        Ok(value) => BoundedSqliteValue::Ready { byte_len, value },
        Err(_) if byte_len > max_bytes => BoundedSqliteValue::Oversized { byte_len },
        Err(_) if remaining.is_some_and(|cap| byte_len > cap) => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        Err(_) => BoundedSqliteValue::Missing,
    }
}

async fn fetch_bubble_bounded(
    conn: &libsql::Connection,
    composer_id: &str,
    bubble_id: &str,
    remaining: Option<u64>,
) -> BoundedSqliteValue<Value> {
    let key = format!("bubbleId:{composer_id}:{bubble_id}");
    if key.len() as u64 > MAX_COMPOSER_SQLITE_KEY_BYTES {
        return BoundedSqliteValue::Missing;
    }
    match fetch_kv_text_bounded(conn, &key, max_composer_record_bytes(), remaining).await {
        BoundedSqliteValue::Missing => BoundedSqliteValue::Missing,
        BoundedSqliteValue::Oversized { byte_len } => BoundedSqliteValue::Oversized { byte_len },
        BoundedSqliteValue::BudgetExceeded { byte_len } => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        BoundedSqliteValue::Malformed { byte_len } => BoundedSqliteValue::Malformed { byte_len },
        BoundedSqliteValue::Ready { byte_len, value } => {
            match serde_json::from_str::<Value>(&value) {
                Ok(parsed) => BoundedSqliteValue::Ready {
                    byte_len,
                    value: parsed,
                },
                Err(_) => BoundedSqliteValue::Malformed { byte_len },
            }
        }
    }
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
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
        .map(str::to_string)
}

fn bubble_epoch(bubble: &Value, key: &str) -> Option<i64> {
    epoch_ms_to_secs(bubble.get(key).and_then(Value::as_i64))
}

fn epoch_ms_to_secs(ms: Option<i64>) -> Option<i64> {
    ms.filter(|v| *v > 0).map(|v| v / 1000)
}

// ---------------------------------------------------------------------------
// store.db blob-DAG reader (SQL length-gated, reachable-only)
// ---------------------------------------------------------------------------

struct StoreMeta {
    agent_id: String,
    latest_root_blob_id: Option<String>,
    created_at: Option<i64>,
}

enum StoreWalkOutcome {
    Messages(Vec<(String, Value, u64)>),
    DeferredEmpty,
}

async fn read_store_meta_bounded(
    conn: &libsql::Connection,
    remaining: Option<u64>,
) -> BoundedSqliteValue<StoreMeta> {
    let decoded_cap = effective_sqlite_cap(MAX_COMPOSER_STORE_META_BYTES, remaining);
    let encoded_cap = decoded_cap.saturating_mul(2);
    let Ok(mut rows) = conn
        .query(
            "SELECT length(CAST(value AS BLOB)) AS nbytes, \
             CASE WHEN length(CAST(value AS BLOB)) <= ?1 THEN value ELSE NULL END AS payload \
             FROM meta WHERE key = '0'",
            libsql::params![encoded_cap as i64],
        )
        .await
    else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(Some(row)) = rows.next().await else {
        return BoundedSqliteValue::Missing;
    };
    let Some(encoded_bytes) = row.get::<i64>(0).ok().filter(|n| *n >= 0).map(|n| n as u64) else {
        return BoundedSqliteValue::Missing;
    };
    let decoded_bytes = encoded_bytes.saturating_add(1) / 2;
    if encoded_bytes > MAX_COMPOSER_STORE_META_HEX_BYTES {
        return BoundedSqliteValue::Oversized {
            byte_len: decoded_bytes,
        };
    }
    if remaining.is_some_and(|cap| decoded_bytes > cap) {
        return BoundedSqliteValue::BudgetExceeded {
            byte_len: decoded_bytes,
        };
    }
    let Ok(hex) = row.get::<String>(1) else {
        return BoundedSqliteValue::Malformed {
            byte_len: decoded_bytes,
        };
    };
    let Some(bytes) = decode_hex(&hex) else {
        return BoundedSqliteValue::Malformed {
            byte_len: decoded_bytes,
        };
    };
    if bytes.len() as u64 > MAX_COMPOSER_STORE_META_BYTES {
        return BoundedSqliteValue::Oversized {
            byte_len: bytes.len() as u64,
        };
    }
    let Ok(meta) = serde_json::from_slice::<Value>(&bytes) else {
        return BoundedSqliteValue::Malformed {
            byte_len: bytes.len() as u64,
        };
    };
    let Some(agent_id) = meta
        .get("agentId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
        .map(str::to_string)
    else {
        return BoundedSqliteValue::Malformed {
            byte_len: bytes.len() as u64,
        };
    };
    BoundedSqliteValue::Ready {
        byte_len: bytes.len() as u64,
        value: StoreMeta {
            agent_id,
            latest_root_blob_id: meta
                .get("latestRootBlobId")
                .and_then(Value::as_str)
                .filter(|id| id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
                .map(str::to_string),
            created_at: epoch_ms_to_secs(meta.get("createdAt").and_then(Value::as_i64)),
        },
    }
}

async fn fetch_store_blob_bounded(
    conn: &libsql::Connection,
    blob_id: &str,
    remaining: Option<u64>,
) -> BoundedSqliteValue<Vec<u8>> {
    if blob_id.is_empty() || blob_id.len() as u64 > MAX_COMPOSER_SQLITE_KEY_BYTES {
        return BoundedSqliteValue::Missing;
    }
    let max_bytes = max_composer_record_bytes();
    let effective_cap = effective_sqlite_cap(max_bytes, remaining);
    let Ok(mut rows) = conn
        .query(
            "SELECT length(CAST(data AS BLOB)) AS nbytes, \
             CASE WHEN length(CAST(data AS BLOB)) <= ?1 THEN data ELSE NULL END AS payload \
             FROM blobs WHERE id = ?2",
            libsql::params![effective_cap as i64, blob_id],
        )
        .await
    else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(Some(row)) = rows.next().await else {
        return BoundedSqliteValue::Missing;
    };
    let Ok(nbytes_i) = row.get::<i64>(0) else {
        return BoundedSqliteValue::Missing;
    };
    if nbytes_i < 0 {
        return BoundedSqliteValue::Missing;
    }
    let byte_len = nbytes_i as u64;
    let data = row
        .get::<Vec<u8>>(1)
        .or_else(|_| row.get::<String>(1).map(String::into_bytes));
    match data {
        Ok(value) => BoundedSqliteValue::Ready { byte_len, value },
        Err(_) if byte_len > max_bytes => BoundedSqliteValue::Oversized { byte_len },
        Err(_) if remaining.is_some_and(|cap| byte_len > cap) => {
            BoundedSqliteValue::BudgetExceeded { byte_len }
        }
        Err(_) => BoundedSqliteValue::Missing,
    }
}

/// Walk the blob DAG from `root` (or id-sorted fallback), fetching each blob by
/// primary key with SQL length/budget gates. Never `SELECT`s the full `blobs`
/// table. Charges reachable blob bytes against `byte_budget` as they materialize.
async fn order_store_messages_bounded(
    conn: &libsql::Connection,
    root: Option<&str>,
    byte_budget: &mut IngestByteBudget,
) -> StoreWalkOutcome {
    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    let mut deferred = false;

    if let Some(root) = root {
        walk_store_blob_bounded(
            conn,
            root,
            byte_budget,
            &mut visited,
            &mut ordered,
            &mut deferred,
        )
        .await;
        if deferred && ordered.is_empty() {
            return StoreWalkOutcome::DeferredEmpty;
        }
        if !ordered.is_empty() {
            return StoreWalkOutcome::Messages(ordered);
        }
    }

    // Fallback: id-sorted leaf scan — ids only first, then length-gated fetches.
    let Ok(mut rows) = conn
        .query(
            "SELECT id FROM blobs \
             WHERE length(CAST(id AS BLOB)) <= ?1 \
             ORDER BY id \
             LIMIT ?2",
            libsql::params![
                MAX_COMPOSER_SQLITE_KEY_BYTES as i64,
                MAX_COMPOSER_STORE_BLOB_VISITS as i64
            ],
        )
        .await
    else {
        return StoreWalkOutcome::Messages(ordered);
    };
    while let Ok(Some(row)) = rows.next().await {
        if visited.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
            byte_budget.defer();
            break;
        }
        let Ok(id) = row.get::<String>(0) else {
            continue;
        };
        if !visited.insert(id.clone()) {
            continue;
        }
        if byte_budget.exhausted() {
            byte_budget.defer();
            deferred = true;
            break;
        }
        match fetch_store_blob_bounded(conn, &id, byte_budget.remaining()).await {
            BoundedSqliteValue::Ready { byte_len, value } => {
                if !byte_budget.try_consume(byte_len) {
                    deferred = true;
                    break;
                }
                if let Some((role, content)) = store_blob_message(&value) {
                    ordered.push((role, content, byte_len));
                }
            }
            BoundedSqliteValue::BudgetExceeded { .. } => {
                byte_budget.defer();
                deferred = true;
                break;
            }
            BoundedSqliteValue::Oversized { byte_len }
            | BoundedSqliteValue::Malformed { byte_len } => {
                if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                    deferred = true;
                    break;
                }
            }
            BoundedSqliteValue::Missing => {}
        }
    }
    if deferred && ordered.is_empty() {
        StoreWalkOutcome::DeferredEmpty
    } else {
        StoreWalkOutcome::Messages(ordered)
    }
}

async fn walk_store_blob_bounded(
    conn: &libsql::Connection,
    id: &str,
    byte_budget: &mut IngestByteBudget,
    visited: &mut HashSet<String>,
    ordered: &mut Vec<(String, Value, u64)>,
    deferred: &mut bool,
) {
    if *deferred || visited.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
        if visited.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
            byte_budget.defer();
            *deferred = true;
        }
        return;
    }
    if !visited.insert(id.to_string()) {
        return;
    }
    if byte_budget.exhausted() {
        byte_budget.defer();
        *deferred = true;
        return;
    }
    match fetch_store_blob_bounded(conn, id, byte_budget.remaining()).await {
        BoundedSqliteValue::Missing => {}
        BoundedSqliteValue::Oversized { byte_len } | BoundedSqliteValue::Malformed { byte_len } => {
            if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                *deferred = true;
            }
        }
        BoundedSqliteValue::BudgetExceeded { .. } => {
            byte_budget.defer();
            *deferred = true;
        }
        BoundedSqliteValue::Ready { byte_len, value } => {
            if !byte_budget.try_consume(byte_len) {
                *deferred = true;
                return;
            }
            if let Some((role, content)) = store_blob_message(&value) {
                ordered.push((role, content, byte_len));
                return;
            }
            let children = protobuf_child_refs(&value);
            for child in children.into_iter().take(MAX_COMPOSER_STORE_BLOB_VISITS) {
                if *deferred {
                    return;
                }
                Box::pin(walk_store_blob_bounded(
                    conn,
                    &child,
                    byte_budget,
                    visited,
                    ordered,
                    deferred,
                ))
                .await;
            }
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
        if refs.len() >= MAX_COMPOSER_STORE_BLOB_VISITS {
            break;
        }
        let Some((tag, next)) = read_varint(bytes, i) else {
            break;
        };
        i = next;
        let field = tag >> 3;
        let wire = tag & 0x7;
        match wire {
            0 => {
                let Some((_, next)) = read_varint(bytes, i) else {
                    break;
                };
                i = next;
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
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

    #[cfg(windows)]
    #[test]
    fn windows_snapshot_generation_is_stable_across_appends() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.vscdb");
        std::fs::write(&path, b"before").unwrap();
        let before = snapshot_generation(&path).expect("Windows file identity");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"after")
            .unwrap();

        assert_eq!(snapshot_generation(&path), Some(before));
    }

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
        assert!(!rendered.contains("TodoList") && !rendered.contains("todo_list"));
        assert!(rendered.contains("SnapshotOrder"));
        assert!(rendered.contains(record_id.as_str()));
        assert!(!rendered.contains("/secret/workspace"));
        assert!(!rendered.contains("credential-redacted"));
        assert!(!rendered.contains("secret result"));
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "composer-redacted");
        assert_eq!(relations["message_id"], record_id.as_str());
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
    }

    #[test]
    fn composer_bubble_without_turn_field_leaves_turn_unset() {
        let native = json!({
            "bubbleId": "bubble-1",
            "type": 1,
            "text": "hello from composer"
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = cursor_composer_native_record_id("composer-native", "bubble-1").unwrap();
        let envelope = normalize_cursor_composer_observation(
            &native,
            "composer-native",
            record_id.clone(),
            range,
            0,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["session_id"], "composer-native");
        assert_eq!(relations["thread_id"], "composer-native");
        assert_eq!(relations["message_id"], record_id.as_str());
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
    }

    /// Exact assistant bubble fields from
    /// `tests/transcript_ingest_suite/cursor_composer.rs`
    /// (`composer_envelope_and_bubbles_ingest_rows`). Provider-parser evidence is
    /// the Cursor composer `bubbleId` payload (`type`/`text`/`toolFormerData`/
    /// `thinking`/`tokenCount`); expected output is the canonical envelope with
    /// Cursor provider + bubble-id native provenance.
    #[test]
    fn fixture_backed_composer_assistant_bubble_reaches_canonical_envelope() {
        let native: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.input.json"
        ))
        .expect("Cursor composer golden input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble.expected_envelope.json"
        ))
        .expect("Cursor composer golden expected envelope");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(1, 2).unwrap();
        let record_id = cursor_composer_native_record_id("comp-1", "b-asst").unwrap();
        let envelope =
            normalize_cursor_composer_observation(&native, "comp-1", record_id.clone(), range, 1)
                .unwrap();
        assert_eq!(
            envelope.provider().as_str(),
            expected["provider"].as_str().unwrap()
        );
        assert_eq!(
            envelope.native_record_kind(),
            expected["native_record_kind"].as_str().unwrap()
        );
        assert_eq!(envelope.stable_record_id().as_str(), record_id.as_str());
        let actual = serde_json::to_value(&envelope).unwrap();
        assert_eq!(actual["version"], expected["version"]);
        assert_eq!(actual["evidence"], expected["evidence"]);
        let fact_kinds = actual["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fact| fact["kind"].as_str().unwrap())
            .collect::<Vec<_>>();
        let expected_fact_kinds = expected["fact_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fact_kinds, expected_fact_kinds);
        let relations = actual["relations"].as_object().unwrap();
        assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
        assert_eq!(relations["thread_id"], expected["relations"]["thread_id"]);
        assert_eq!(relations["message_id"], record_id.as_str());
        for absent in expected["relations"]["absent"].as_array().unwrap() {
            assert!(relations.get(absent.as_str().unwrap()).is_none());
        }
        let rendered = actual.to_string();
        for required in expected["encoded_must_contain"].as_array().unwrap() {
            assert!(rendered.contains(required.as_str().unwrap()));
        }
        for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
            assert!(!rendered.contains(rejected.as_str().unwrap()));
        }
    }

    /// Checked-in `composerData` envelope `todos[{id,content,status}]` map to
    /// `WorkflowLifecycle` `TodoList` + `TodoItem` facts with native order and refs.
    #[test]
    fn fixture_backed_composer_envelope_todos_reach_workflow_lifecycle() {
        let native: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
        ))
        .expect("Cursor composer envelope todos golden input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.expected_envelope.json"
        ))
        .expect("Cursor composer envelope todos expected envelope");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        // Fixture lastUpdatedAt is null — checkpoint is the content fingerprint.
        assert!(native.get("lastUpdatedAt").is_some_and(Value::is_null));
        let checkpoint = composer_envelope_todo_checkpoint(&native)
            .expect("fixture todos must yield a content fingerprint checkpoint");
        let record_id = cursor_composer_envelope_native_record_id("comp-1", checkpoint).unwrap();
        let envelope = normalize_cursor_composer_envelope_observation(
            &native,
            "comp-1",
            None,
            record_id.clone(),
            range,
            0,
        )
        .unwrap();
        assert_eq!(
            envelope.provider().as_str(),
            expected["provider"].as_str().unwrap()
        );
        assert_eq!(
            envelope.native_record_kind(),
            expected["native_record_kind"].as_str().unwrap()
        );
        assert_eq!(envelope.stable_record_id().as_str(), record_id.as_str());
        let actual = serde_json::to_value(&envelope).unwrap();
        assert_eq!(actual["version"], expected["version"]);
        assert_eq!(actual["evidence"], expected["evidence"]);
        let fact_kinds = actual["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fact| fact["kind"].as_str().unwrap())
            .collect::<Vec<_>>();
        let expected_fact_kinds = expected["fact_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fact_kinds, expected_fact_kinds);
        let expected_lifecycle = expected["workflow_lifecycle"].as_array().unwrap();
        let actual_facts = actual["facts"].as_array().unwrap();
        assert_eq!(actual_facts.len(), expected_lifecycle.len());
        for (actual_fact, expected_fact) in actual_facts.iter().zip(expected_lifecycle.iter()) {
            assert_eq!(actual_fact["semantic_kind"], expected_fact["semantic_kind"]);
            assert_eq!(
                actual_fact["provider_reference"],
                expected_fact["provider_reference"]
            );
            if let Some(item_id) = expected_fact.get("item_id") {
                assert_eq!(actual_fact["item_id"], *item_id);
            }
            if let Some(list_reference) = expected_fact.get("list_reference") {
                assert_eq!(actual_fact["list_reference"], *list_reference);
            }
            if let Some(status) = expected_fact.get("status") {
                assert_eq!(actual_fact["status"], *status);
            }
            if let Some(item_order) = expected_fact.get("item_order") {
                assert_eq!(actual_fact["item_order"], *item_order);
            }
            if let Some(content) = expected_fact.get("content") {
                assert_eq!(actual_fact["content"], *content);
            }
            for absent in expected_fact["absent"].as_array().unwrap() {
                assert!(actual_fact.get(absent.as_str().unwrap()).is_none());
            }
        }
        let relations = actual["relations"].as_object().unwrap();
        assert_eq!(relations["session_id"], expected["relations"]["session_id"]);
        assert_eq!(relations["thread_id"], expected["relations"]["thread_id"]);
        for absent in expected["relations"]["absent"].as_array().unwrap() {
            assert!(relations.get(absent.as_str().unwrap()).is_none());
        }
        let rendered = actual.to_string();
        for required in expected["encoded_must_contain"].as_array().unwrap() {
            assert!(rendered.contains(required.as_str().unwrap()));
        }
        for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
            assert!(!rendered.contains(rejected.as_str().unwrap()));
        }
    }

    #[test]
    fn envelope_todo_checkpoint_uses_fixture_backed_content_fingerprint() {
        let native: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/envelope_todos.input.json"
        ))
        .expect("Cursor composer envelope todos golden input");
        assert!(native.get("lastUpdatedAt").is_some_and(Value::is_null));
        let baseline = composer_envelope_todo_checkpoint(&native).unwrap();
        let mut pending_second = native.clone();
        pending_second["todos"][1]["status"] = Value::String("completed".to_string());
        let updated = composer_envelope_todo_checkpoint(&pending_second).unwrap();
        assert_ne!(
            baseline, updated,
            "pending→completed must change the content fingerprint checkpoint"
        );
        assert_ne!(
            cursor_composer_envelope_native_record_id("comp-1", baseline).unwrap(),
            cursor_composer_envelope_native_record_id("comp-1", updated).unwrap()
        );
        let mut edited = native.clone();
        edited["todos"][1]["content"] = Value::String("Second todo revised".to_string());
        assert_ne!(
            baseline,
            composer_envelope_todo_checkpoint(&edited).unwrap(),
            "content edits must change the checkpoint"
        );
        let mut reordered = native.clone();
        reordered["todos"].as_array_mut().unwrap().swap(0, 1);
        assert_ne!(
            baseline,
            composer_envelope_todo_checkpoint(&reordered).unwrap(),
            "native array-order changes must change the checkpoint"
        );
    }

    /// Bubble text + todos co-locate `Message` and `WorkflowLifecycle` facts.
    #[test]
    fn fixture_backed_composer_bubble_colocates_message_and_todo_lifecycle() {
        let native: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.input.json"
        ))
        .expect("Cursor composer bubble+todos golden input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/cursor_composer/assistant_bubble_with_todos.expected_envelope.json"
        ))
        .expect("Cursor composer bubble+todos expected envelope");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(1, 2).unwrap();
        let record_id = cursor_composer_native_record_id("comp-1", "b-todos").unwrap();
        let envelope =
            normalize_cursor_composer_observation(&native, "comp-1", record_id.clone(), range, 1)
                .unwrap();
        let actual = serde_json::to_value(&envelope).unwrap();
        let fact_kinds = actual["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|fact| fact["kind"].as_str().unwrap())
            .collect::<Vec<_>>();
        let expected_fact_kinds = expected["fact_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fact_kinds, expected_fact_kinds);
        assert!(
            envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. })),
            "message fact must remain co-located"
        );
        assert!(
            envelope.facts().iter().any(|fact| matches!(
                fact,
                CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::TodoList,
                    ..
                }
            )),
            "todo list fact required"
        );
        let items: Vec<_> = envelope
            .facts()
            .iter()
            .filter_map(|fact| match fact {
                CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                    item_id,
                    status,
                    item_order,
                    content,
                    list_reference,
                    ..
                } => Some((item_id, status, item_order, content, list_reference)),
                _ => None,
            })
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0.as_deref(), Some("t1"));
        assert_eq!(items[0].1.as_deref(), Some("completed"));
        assert_eq!(*items[0].2, Some(0));
        assert_eq!(
            items[0].3.as_ref().and_then(Value::as_str),
            Some("First todo")
        );
        assert_eq!(items[0].4.as_deref(), Some("comp-1"));
        assert_eq!(items[1].0.as_deref(), Some("t2"));
        assert_eq!(items[1].1.as_deref(), Some("pending"));
        assert_eq!(*items[1].2, Some(1));
        assert_eq!(
            items[1].3.as_ref().and_then(Value::as_str),
            Some("Second todo")
        );
        assert_eq!(items[1].4.as_deref(), Some("comp-1"));
        let rendered = actual.to_string();
        for required in expected["encoded_must_contain"].as_array().unwrap() {
            assert!(rendered.contains(required.as_str().unwrap()));
        }
        assert!(!rendered.contains("\"revision\""));
    }

    #[test]
    fn composer_todo_without_native_id_is_not_promoted() {
        let native = json!({
            "type": 2,
            "text": "Working the checklist.",
            "todos": [
                {"content": "No stable identity", "status": "pending"},
                {"id": "t2", "content": "Native identity", "status": "completed"}
            ]
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(1, 2).unwrap();
        let record_id = cursor_composer_native_record_id("comp-1", "b-todos").unwrap();
        let envelope =
            normalize_cursor_composer_observation(&native, "comp-1", record_id, range, 1).unwrap();
        let items = envelope
            .facts()
            .iter()
            .filter_map(|fact| match fact {
                CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::TodoItem,
                    item_id,
                    item_order,
                    ..
                } => Some((item_id.as_deref(), *item_order)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(items, vec![(Some("t2"), Some(1))]);
    }

    /// Exact provider bool `isCompacted: true` remains the only compaction
    /// promotion path; lookalike keys/string forms stay ignored.
    #[test]
    fn composer_is_compacted_true_promotes_compaction_fact() {
        let native = json!({
            "type": 2,
            "text": "post-compaction bubble",
            "isCompacted": true,
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id =
            cursor_composer_native_record_id("composer-compacted", "bubble-compacted").unwrap();
        let envelope = normalize_cursor_composer_observation(
            &native,
            "composer-compacted",
            record_id,
            range,
            0,
        )
        .unwrap();
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Compaction {
                summary: Some(Value::String(text)),
                ..
            } if text == "post-compaction bubble"
        )));
    }

    async fn open_temp_kv_db_with_rows(rows: &[(&str, &str)]) -> (tempfile::TempDir, ReadOnlyDb) {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state.vscdb");
        {
            let db = Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=DELETE;\n\
                 CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
            )
            .await
            .unwrap();
            for (key, value) in rows {
                conn.execute(
                    "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                    libsql::params![*key, *value],
                )
                .await
                .unwrap();
            }
        }
        let ro = open_readonly_immutable(&path).await.expect("open readonly");
        (tmp, ro)
    }

    async fn open_temp_kv_db_with_sql(setup_sql: &str) -> (tempfile::TempDir, ReadOnlyDb) {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("state.vscdb");
        {
            let db = Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=DELETE;\n\
                 CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
            )
            .await
            .unwrap();
            conn.execute_batch(setup_sql).await.unwrap();
        }
        let ro = open_readonly_immutable(&path).await.expect("open readonly");
        (tmp, ro)
    }

    #[tokio::test]
    async fn sql_length_gate_rejects_oversized_bubble_built_in_sql() {
        // Hostile TEXT is constructed entirely in SQL (hex(zeroblob)) so the
        // product fetch never receives a pre-built Rust String of that value.
        let setup = "INSERT INTO cursorDiskKV(key, value) \
             SELECT 'bubbleId:comp:hostile', hex(zeroblob(33));";
        let (tmp, ro) = open_temp_kv_db_with_sql(setup).await;
        let _keep = tmp;

        match fetch_kv_text_bounded(&ro.conn, "bubbleId:comp:hostile", 64, None).await {
            BoundedSqliteValue::Oversized { byte_len } => {
                assert_eq!(byte_len, 66);
            }
            other => panic!("expected Oversized, got {other:?}"),
        }
        match fetch_bubble_bounded(&ro.conn, "comp", "hostile", None).await {
            // 66 bytes is under the real 1 MiB record ceiling; complete non-JSON
            // text receives typed malformed coverage rather than disappearing.
            BoundedSqliteValue::Malformed { byte_len } => assert_eq!(byte_len, 66),
            other => panic!("unexpected bubble outcome {other:?}"),
        }
    }

    #[tokio::test]
    async fn sql_length_gate_counts_utf8_bytes_not_characters() {
        // SQLite length(TEXT) would report 40 characters and incorrectly admit
        // this 80-byte value under a 64-byte ceiling. Construct it in SQL so no
        // product Rust code pre-materializes the hostile text.
        let setup = "INSERT INTO cursorDiskKV(key, value) \
             SELECT 'bubbleId:comp:multibyte', \
                    replace(hex(zeroblob(40)), '00', 'é');";
        let (tmp, ro) = open_temp_kv_db_with_sql(setup).await;
        let _keep = tmp;

        match fetch_kv_text_bounded(&ro.conn, "bubbleId:comp:multibyte", 64, None).await {
            BoundedSqliteValue::Oversized { byte_len } => assert_eq!(byte_len, 80),
            other => panic!("expected UTF-8 byte Oversized, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sql_budget_gate_defers_before_materializing_bubble_text() {
        let (tmp, ro) =
            open_temp_kv_db_with_rows(&[("bubbleId:comp:b1", r#"{"type":1,"text":"hello"}"#)])
                .await;
        let _keep = tmp;

        match fetch_bubble_bounded(&ro.conn, "comp", "b1", Some(4)).await {
            BoundedSqliteValue::BudgetExceeded { byte_len } => {
                assert!(byte_len > 4);
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn store_blob_zeroblob_is_skipped_without_full_table_select() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("store.db");
        let root = "aa".repeat(32);
        {
            let db = Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=DELETE;\n\
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);\n\
                 CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);",
            )
            .await
            .unwrap();
            let leaf = "bb".repeat(32);
            let meta = serde_json::json!({
                "agentId": "agent-adv",
                "latestRootBlobId": root,
                "createdAt": 1_700_000_000_000i64,
            });
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('0', ?1)",
                libsql::params![encode_hex(meta.to_string().as_bytes())],
            )
            .await
            .unwrap();
            let hostile = (max_composer_record_bytes() as i64).saturating_add(64);
            conn.execute(
                "INSERT INTO blobs(id, data) VALUES (?1, zeroblob(?2))",
                libsql::params![root.clone(), hostile],
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO blobs(id, data) VALUES (?1, ?2)",
                libsql::params![
                    leaf,
                    libsql::Value::Blob(
                        serde_json::json!({"role":"user","content":"reachable"})
                            .to_string()
                            .into_bytes()
                    )
                ],
            )
            .await
            .unwrap();
        }

        let ro = open_readonly_immutable(&path).await.unwrap();
        let mut budget = IngestByteBudget::bounded(DEFAULT_COMPOSER_SWEEP_BYTES);
        let outcome = order_store_messages_bounded(&ro.conn, Some(&root), &mut budget).await;
        // Hostile root is skipped (oversized); fallback id-sort still finds the leaf.
        match outcome {
            StoreWalkOutcome::Messages(messages) => {
                assert!(
                    messages.iter().any(|(role, _, _)| role == "user"),
                    "bounded fallback should still reach the valid leaf"
                );
            }
            StoreWalkOutcome::DeferredEmpty => panic!("default sweep budget should reach leaf"),
        }
    }

    #[test]
    fn configured_composer_sqlite_bounds_match_shared_pr6_ceilings() {
        assert_eq!(max_composer_record_bytes(), 1_048_576);
        assert_eq!(MAX_COMPOSER_ENVELOPE_BYTES, 16 * 1024 * 1024);
        assert_eq!(DEFAULT_COMPOSER_SWEEP_BYTES, 16 * 1024 * 1024 + 1);
        assert_eq!(MAX_COMPOSER_STORE_META_BYTES, 256 * 1024);
        assert_eq!(MAX_COMPOSER_STORE_META_HEX_BYTES, 512 * 1024);
        assert_eq!(MAX_COMPOSER_STORE_BLOB_VISITS, 4096);
        assert_eq!(MAX_COMPOSER_SQLITE_KEY_BYTES, 512);
    }
}
