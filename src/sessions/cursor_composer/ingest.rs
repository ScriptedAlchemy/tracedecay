//! Cursor composer sweep orchestration: [`CursorComposerSource`] discovery,
//! `state.vscdb` envelope/bubble ingestion, `store.db` sweeps, and coverage
//! advancement.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ProjectId, ProviderId, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::ObservationCoverageReason;

use crate::application::host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
use crate::application::observation::{CaptureObservationOutcome, ObservationCancellation};
use crate::global_db::GlobalDb;
use crate::sessions::ingest_byte_budget::IngestByteBudget;
use crate::sessions::shared::path_belongs_to_project;

use super::PROVIDER;
use super::capture::{
    build_cursor_composer_capture_request_for_project,
    build_cursor_composer_envelope_capture_request_for_project, composer_envelope_todo_checkpoint,
    cursor_composer_envelope_source,
};
use super::observation::composer_todos_have_admittable_items;
use super::sqlite::{
    BoundedSqliteValue, ComposerProject, DEFAULT_COMPOSER_SWEEP_BYTES, MAX_COMPOSER_ENVELOPE_BYTES,
    MAX_COMPOSER_SQLITE_KEY_BYTES, composer_budget_bytes, composer_id_from_envelope_key,
    composer_source_charge, envelope_project, fetch_bubble_bounded, fetch_kv_text_bounded,
    open_readonly_immutable, workspace_hash,
};
use super::store::{
    MAX_COMPOSER_STORE_BLOB_VISITS, StoreWalkOutcome, order_store_messages_bounded,
    read_store_meta_bounded,
};
/// Default ceiling on how many *new/changed* composer sessions one sweep pass
/// ingests, so the first backfill of thousands of sessions never blocks
/// startup; already-watermarked sessions are skipped cheaply and do not count.
pub const DEFAULT_COMPOSER_ENVELOPE_CAP: usize = 256;

struct ComposerIngestContext<'facade, 'db, 'root> {
    db: &'db GlobalDb,
    facade: &'facade HostAdmissionFacade<'db>,
    scope: ObservationScopeV1,
    project_root: Option<&'root Path>,
    registered_roots: &'root [PathBuf],
}

async fn drain_composer_projection_queue(context: &ComposerIngestContext<'_, '_, '_>) {
    if let Err(error) = crate::sessions::claude_observation::drain_projection_queue(
        context.facade,
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

pub(crate) fn snapshot_generation(path: &Path) -> Option<ObservationSourceGenerationV1> {
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
        let admission = HostAdmissionFacade::new(HostAdmissionAuthorities::for_project(
            db,
            project_id.clone(),
        ));
        self.ingest_capped_with_admission(
            db,
            project_root,
            project_id,
            &admission,
            envelope_cap,
            max_new_bytes,
        )
        .await
    }

    /// Project startup-sweep variant whose authority has already been prepared
    /// by the caller from the authoritative project identity and privacy policy.
    pub(crate) async fn ingest_capped_with_admission(
        &self,
        db: &GlobalDb,
        project_root: &Path,
        project_id: ProjectId,
        admission: &HostAdmissionFacade<'_>,
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
    ) -> CursorComposerSweepOutcome {
        let context = ComposerIngestContext {
            db,
            facade: admission,
            scope: ObservationScopeV1::Project { project_id },
            project_root: Some(project_root),
            registered_roots: &[],
        };
        self.ingest_with_context(&context, envelope_cap, max_new_bytes)
            .await
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
        let admission = HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(db));
        let context = ComposerIngestContext {
            db,
            facade: &admission,
            scope: ObservationScopeV1::Profile,
            project_root: None,
            registered_roots,
        };
        self.ingest_with_context(&context, envelope_cap, max_new_bytes)
            .await
    }

    async fn ingest_with_context(
        &self,
        context: &ComposerIngestContext<'_, '_, '_>,
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
    ) -> CursorComposerSweepOutcome {
        let mut outcome = CursorComposerSweepOutcome::default();
        let mut byte_budget =
            IngestByteBudget::bounded(max_new_bytes.unwrap_or(DEFAULT_COMPOSER_SWEEP_BYTES));
        drain_composer_projection_queue(context).await;
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(
            context,
            envelope_cap,
            &mut byte_budget,
            &mut outcome,
            &mut workspace_paths,
        )
        .await;
        self.ingest_chat_store_dbs(context, &workspace_paths, &mut byte_budget, &mut outcome)
            .await;
        drain_composer_projection_queue(context).await;
        outcome.bytes_consumed = byte_budget.consumed();
        outcome.deferred_by_byte_cap = byte_budget.deferred();
        outcome
    }

    async fn ingest_state_vscdb(
        &self,
        context: &ComposerIngestContext<'_, '_, '_>,
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
                    if matches!(*persisted, ObservationPersistOutcome::Committed(_)) {
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
                                facade: context.facade,
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
                                facade: context.facade,
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
                                    facade: context.facade,
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
                                if matches!(*persisted, ObservationPersistOutcome::Committed(_)) {
                                    messages = messages.saturating_add(1);
                                }
                            }
                            Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                                if advance_composer_coverage(
                                    ComposerCoverageContext {
                                        facade: context.facade,
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
                                        facade: context.facade,
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
        context: &ComposerIngestContext<'_, '_, '_>,
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
        context: &ComposerIngestContext<'_, '_, '_>,
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
                        facade: context.facade,
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
                    if matches!(*persisted, ObservationPersistOutcome::Committed(_)) {
                        messages = messages.saturating_add(1);
                    }
                }
                Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                    if advance_composer_coverage(
                        ComposerCoverageContext {
                            facade: context.facade,
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
                            facade: context.facade,
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
