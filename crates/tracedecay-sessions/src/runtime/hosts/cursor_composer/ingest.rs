//! Cursor composer sweep orchestration: [`CursorComposerSource`] discovery,
//! `state.vscdb` envelope/bubble ingestion, `store.db` sweeps, and coverage
//! advancement.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_capture::cursor_composer::composer_todos_have_admittable_items;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ProjectId, ProviderId, SessionId,
};
use tracedecay_store::observation::ObservationCoverageReason;

use crate::admission::HostAdmission;
use crate::observation::{CaptureObservationOutcome, ObservationCancellation};
use crate::runtime::ingest_byte_budget::IngestByteBudget;
use crate::runtime::shared::{ProjectMembership, ProjectRootMatcherCache, TranscriptScopeMatcher};
use crate::runtime::source::{
    TranscriptIngestError, TranscriptIngestResult, canonical_framed_sha256,
    run_blocking_transcript_section,
};
use crate::runtime::store_access::SESSION_MESSAGE_ID_LOOKUP_MAX;

use super::capture::{
    build_cursor_composer_capture_request_for_project,
    build_cursor_composer_envelope_capture_request_for_project, composer_envelope_todo_checkpoint,
    cursor_composer_envelope_source,
};
use super::sqlite::{
    BoundedSqliteValue, COMPOSER_KEY_SCAN_PAGE, ComposerProject, DEFAULT_COMPOSER_SWEEP_BYTES,
    MAX_COMPOSER_ENVELOPE_BYTES, MAX_COMPOSER_SQLITE_KEY_BYTES, composer_budget_bytes,
    composer_id_from_envelope_key, composer_source_charge, envelope_project, fetch_bubble_bounded,
    fetch_kv_text_bounded, open_readonly_immutable, scan_composer_keys_page, workspace_hash,
};
use super::store::{
    MAX_COMPOSER_STORE_BLOB_VISITS, StoreWalkOutcome, order_store_messages_bounded,
    read_store_meta_bounded,
};
use super::{CursorComposerSweepOutcome, CursorComposerSweepResult, PROVIDER};
/// Default ceiling on how many *new/changed* composer sessions one sweep pass
/// ingests, so the first backfill of thousands of sessions never blocks
/// startup; already-watermarked sessions are skipped cheaply and do not count.
pub const DEFAULT_COMPOSER_ENVELOPE_CAP: usize = 256;

const COMPOSER_SCAN_FRONTIER_KEY_PREFIX: &str = "cursor-composer.scan.";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ComposerScanFrontier {
    after_key: Option<String>,
}

pub(super) fn directory_entry_is_real_dir(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_dir())
}

pub(super) fn path_is_regular_file_no_follow(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn discover_chat_store_dbs(
    chats_dir: &Path,
    workspace_paths: &HashMap<String, String>,
    scope_matcher: &TranscriptScopeMatcher,
    project_scoped: bool,
) -> Vec<(PathBuf, String)> {
    let Ok(ws_entries) = std::fs::read_dir(chats_dir) else {
        return Vec::new();
    };
    let mut stores = Vec::new();
    for ws_entry in ws_entries.flatten() {
        if !directory_entry_is_real_dir(&ws_entry) {
            continue;
        }
        let ws_hash = ws_entry.file_name().to_string_lossy().to_string();
        let Some(path) = workspace_paths.get(&ws_hash) else {
            continue;
        };
        if scope_matcher.membership(Some(Path::new(path))) != ProjectMembership::Match {
            continue;
        }
        let project_path = if project_scoped {
            path.clone()
        } else {
            "user".to_string()
        };
        let Ok(agent_entries) = std::fs::read_dir(ws_entry.path()) else {
            continue;
        };
        for agent_entry in agent_entries.flatten() {
            if !directory_entry_is_real_dir(&agent_entry) {
                continue;
            }
            let store_path = agent_entry.path().join("store.db");
            if !path_is_regular_file_no_follow(&store_path) {
                continue;
            }
            stores.push((store_path, project_path.clone()));
        }
    }
    stores
}

struct ComposerIngestContext<'facade, 'root> {
    facade: &'facade dyn HostAdmission,
    scope: ObservationScopeV1,
    project_root: Option<&'root Path>,
    registered_roots: &'root [PathBuf],
    cancellation: &'root ObservationCancellation,
    /// The source's matcher cache, so repeated sweeps reuse one git identity
    /// resolution per root/workspace path.
    matchers: &'root ProjectRootMatcherCache,
}

impl ComposerIngestContext<'_, '_> {
    /// Resolve this sweep's scope boundary once, rather than per composer
    /// envelope and per workspace directory.
    fn scope_matcher(&self) -> TranscriptScopeMatcher {
        self.project_root.map_or_else(
            || TranscriptScopeMatcher::profile_cached(self.registered_roots, self.matchers),
            |root| TranscriptScopeMatcher::project_cached(root, self.matchers),
        )
    }

    /// The project label stored for an accepted workspace: its real path under
    /// project scope, the shared `"user"` bucket under profile scope.
    fn scoped_project_label(&self, workspace_path: &str) -> String {
        if self.project_root.is_some() {
            workspace_path.to_string()
        } else {
            "user".to_string()
        }
    }
}

async fn drain_composer_projection_queue(
    context: &ComposerIngestContext<'_, '_>,
) -> TranscriptIngestResult<crate::runtime::cursor::projection::CursorProjectionDrainStats> {
    crate::runtime::cursor::projection::drain_cursor_observation_projections_with_sessions(
        context.facade,
        &context.scope,
        context.cancellation,
    )
    .await
}

fn composer_cancellation_error() -> TranscriptIngestError {
    TranscriptIngestError::Cancelled { provider: PROVIDER }
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

#[cfg(windows)]
pub(super) fn snapshot_generation(path: &Path) -> Option<ObservationSourceGenerationV1> {
    let identity = tracedecay_runtime_core::db::sqlite_generation_identity(path).ok()?;
    ObservationSourceGenerationV1::new(identity).ok()
}

fn composer_scan_frontier_key(
    path: &Path,
    generation: ObservationSourceGenerationV1,
) -> Result<String, String> {
    let canonical = path.canonicalize().map_err(|error| {
        format!(
            "could not resolve Cursor composer state identity '{}': {error}",
            path.display()
        )
    })?;
    let generation_bytes = generation.file_id().to_be_bytes();
    let path_identity = canonical_framed_sha256(
        b"cursor-composer-scan-frontier",
        &[canonical.as_os_str().as_encoded_bytes(), &generation_bytes],
    );
    Ok(format!(
        "{COMPOSER_SCAN_FRONTIER_KEY_PREFIX}{path_identity}"
    ))
}

fn decode_composer_scan_frontier(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let frontier = serde_json::from_str::<ComposerScanFrontier>(value)
        .map_err(|error| format!("invalid Cursor composer scan frontier: {error}"))?;
    if frontier.after_key.as_ref().is_some_and(|key| {
        composer_id_from_envelope_key(key).is_none()
            || key.len() as u64 > MAX_COMPOSER_SQLITE_KEY_BYTES
    }) {
        return Err("invalid Cursor composer scan frontier key".to_string());
    }
    Ok(frontier.after_key)
}

struct ComposerCoverageContext<'facade> {
    facade: &'facade dyn HostAdmission,
    scope: &'facade ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    cancellation: &'facade ObservationCancellation,
}

async fn advance_composer_coverage(
    context: ComposerCoverageContext<'_>,
    source: ObservationSourceIdentityV1,
    position: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
    reason: ObservationCoverageReason,
    receipt: Option<tracedecay_domain::SanitizationReceiptV1>,
) -> Result<(), String> {
    let range =
        tracedecay_domain::ObservationSourceRangeV1::new(position, position.saturating_add(1))
            .map_err(|error| format!("invalid Cursor composer coverage range: {error}"))?;
    crate::runtime::snapshot_observation::advance_snapshot_coverage_maybe(
        context.facade,
        PROVIDER,
        source,
        range,
        expected_cursor,
        context.scope.clone(),
        context.generation,
        reason,
        receipt,
        context.cancellation,
    )
    .await
    .map_err(|error| error.to_string())
}

/// Read-only Cursor composer store source rooted at a home directory.
pub struct CursorComposerSource {
    state_db_path: PathBuf,
    chats_dir: PathBuf,
    /// Source-lifetime cache so sweeps resolve git identity once per
    /// root/workspace path instead of once per envelope or chats directory.
    project_matchers: ProjectRootMatcherCache,
}

impl CursorComposerSource {
    /// Source rooted at the real user home. `None` when it cannot be resolved.
    pub fn new() -> Option<Self> {
        let home = crate::runtime::home_dir()?;
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
            project_matchers: ProjectRootMatcherCache::default(),
        }
    }

    #[hotpath::skip]
    pub async fn ingest_capped_with_cancellation(
        &self,
        admission: &dyn HostAdmission,
        project_root: &Path,
        project_id: ProjectId,
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
        cancellation: &ObservationCancellation,
    ) -> CursorComposerSweepResult {
        let context = ComposerIngestContext {
            facade: admission,
            scope: ObservationScopeV1::Project { project_id },
            project_root: Some(project_root),
            registered_roots: &[],
            cancellation,
            matchers: &self.project_matchers,
        };
        self.ingest_with_context(&context, envelope_cap, max_new_bytes)
            .await
    }

    #[hotpath::skip]
    pub async fn ingest_user_capped_with_cancellation(
        &self,
        admission: &dyn HostAdmission,
        registered_roots: &[PathBuf],
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
        cancellation: &ObservationCancellation,
    ) -> CursorComposerSweepResult {
        let context = ComposerIngestContext {
            facade: admission,
            scope: ObservationScopeV1::Profile,
            project_root: None,
            registered_roots,
            cancellation,
            matchers: &self.project_matchers,
        };
        self.ingest_with_context(&context, envelope_cap, max_new_bytes)
            .await
    }

    #[hotpath::skip]
    async fn ingest_with_context(
        &self,
        context: &ComposerIngestContext<'_, '_>,
        envelope_cap: usize,
        max_new_bytes: Option<u64>,
    ) -> CursorComposerSweepResult {
        let mut outcome = CursorComposerSweepOutcome::default();
        let mut byte_budget =
            IngestByteBudget::bounded(max_new_bytes.unwrap_or(DEFAULT_COMPOSER_SWEEP_BYTES));
        if context.cancellation.is_cancelled() {
            return Err(outcome.terminated(composer_cancellation_error(), 0, false));
        }
        let projection_stats = match drain_composer_projection_queue(context).await {
            Ok(stats) => stats,
            Err(error) => return Err(outcome.terminated(error, 0, false)),
        };
        outcome.add_projection(
            projection_stats.session_ids,
            projection_stats.messages_upserted,
            projection_stats.source_deferred,
        );
        if context.cancellation.is_cancelled() {
            return Err(outcome.terminated(composer_cancellation_error(), 0, false));
        }
        let mut workspace_paths = HashMap::new();
        self.ingest_state_vscdb(
            context,
            envelope_cap,
            &mut byte_budget,
            &mut outcome,
            &mut workspace_paths,
        )
        .await;
        if context.cancellation.is_cancelled() {
            return Err(outcome.terminated(
                composer_cancellation_error(),
                byte_budget.consumed(),
                byte_budget.deferred(),
            ));
        }
        self.ingest_chat_store_dbs(context, &workspace_paths, &mut byte_budget, &mut outcome)
            .await;
        if context.cancellation.is_cancelled() {
            return Err(outcome.terminated(
                composer_cancellation_error(),
                byte_budget.consumed(),
                byte_budget.deferred(),
            ));
        }
        let projection_stats = match drain_composer_projection_queue(context).await {
            Ok(stats) => stats,
            Err(error) => {
                return Err(outcome.terminated(
                    error,
                    byte_budget.consumed(),
                    byte_budget.deferred(),
                ));
            }
        };
        outcome.add_projection(
            projection_stats.session_ids,
            projection_stats.messages_upserted,
            projection_stats.source_deferred,
        );
        if context.cancellation.is_cancelled() {
            return Err(outcome.terminated(
                composer_cancellation_error(),
                byte_budget.consumed(),
                byte_budget.deferred(),
            ));
        }
        Ok(outcome.finished(byte_budget.consumed(), byte_budget.deferred()))
    }

    #[hotpath::skip]
    async fn ingest_state_vscdb(
        &self,
        context: &ComposerIngestContext<'_, '_>,
        envelope_cap: usize,
        byte_budget: &mut IngestByteBudget,
        outcome: &mut CursorComposerSweepOutcome,
        workspace_paths: &mut HashMap<String, String>,
    ) {
        if context.cancellation.is_cancelled() {
            return;
        }
        if !hotpath::measure_block!(
            "sessions.hosts.cursor_composer.state_db_stat_blocking",
            run_blocking_transcript_section(|| self.state_db_path.is_file())
        ) {
            return;
        }
        let ro = match open_readonly_immutable(&self.state_db_path).await {
            Ok(ro) => ro,
            Err(error) => {
                tracing::debug!(
                    state_db = %self.state_db_path.display(),
                    error,
                    "Cursor composer state database open failed closed"
                );
                byte_budget.defer();
                return;
            }
        };
        let conn = &ro.conn;
        let state_generation = ro.generation;
        let frontier_key = match composer_scan_frontier_key(&ro.canonical_path, state_generation) {
            Ok(key) => key,
            Err(error) => {
                tracing::debug!(
                    state_db = %self.state_db_path.display(),
                    error,
                    "Cursor composer scan frontier identity failed closed"
                );
                byte_budget.defer();
                return;
            }
        };
        let expected_frontier = match context
            .facade
            .read_session_backfill_state(&context.scope, &frontier_key)
            .await
        {
            Ok(frontier) => frontier,
            Err(_) => {
                byte_budget.defer();
                return;
            }
        };
        let initial_after = match decode_composer_scan_frontier(expected_frontier.as_deref()) {
            Ok(frontier) => frontier,
            Err(error) => {
                tracing::debug!(
                    state_db = %self.state_db_path.display(),
                    error,
                    "Cursor composer scan frontier is invalid"
                );
                byte_budget.defer();
                return;
            }
        };
        let scope_matcher = hotpath::measure_block!(
            "sessions.hosts.cursor_composer.state_scope_blocking",
            run_blocking_transcript_section(|| context.scope_matcher())
        );
        // Indexed prefix scan of keys + byte lengths only — never SELECT full
        // envelope text here. Point-fetch materializes only when the UTF-8 byte
        // length fits both ceilings. Keyset pagination over the `cursorDiskKV`
        // primary key reproduces the original index-ordered streaming scan
        // while holding at most one page of keys in memory.
        let mut ingested_this_pass = 0usize;
        let mut scanned_this_pass = 0usize;
        let mut scan_after = initial_after.clone();
        let mut last_scanned_key = initial_after.clone();
        let mut reached_end = false;
        'scan: loop {
            if context.cancellation.is_cancelled() {
                break;
            }
            if scanned_this_pass >= MAX_COMPOSER_STORE_BLOB_VISITS {
                byte_budget.defer();
                break;
            }
            let page =
                match scan_composer_keys_page(conn, scan_after.as_deref(), COMPOSER_KEY_SCAN_PAGE)
                    .await
                {
                    Ok(page) => page,
                    Err(error) => {
                        tracing::debug!(
                            state_db = %self.state_db_path.display(),
                            error,
                            "Cursor composer key scan failed closed"
                        );
                        byte_budget.defer();
                        break;
                    }
                };
            let Some(last_key) = page.last().map(|(key, _)| key.clone()) else {
                reached_end = true;
                break;
            };
            let page_full = page.len() == COMPOSER_KEY_SCAN_PAGE;
            for (key, nbytes) in page {
                if context.cancellation.is_cancelled() {
                    break 'scan;
                }
                if scanned_this_pass >= MAX_COMPOSER_STORE_BLOB_VISITS {
                    byte_budget.defer();
                    break 'scan;
                }
                scanned_this_pass += 1;
                if nbytes > MAX_COMPOSER_ENVELOPE_BYTES {
                    if !byte_budget
                        .try_consume(nbytes.min(MAX_COMPOSER_ENVELOPE_BYTES.saturating_add(1)))
                    {
                        break 'scan;
                    }
                    last_scanned_key = Some(key);
                    continue;
                }
                if byte_budget.exhausted() {
                    byte_budget.defer();
                    break 'scan;
                }
                if byte_budget
                    .remaining()
                    .is_some_and(|remaining| nbytes > remaining)
                {
                    byte_budget.defer();
                    break 'scan;
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
                        break 'scan;
                    }
                    BoundedSqliteValue::Oversized { .. }
                    | BoundedSqliteValue::Malformed { .. }
                    | BoundedSqliteValue::Missing => {
                        last_scanned_key = Some(key);
                        continue;
                    }
                    BoundedSqliteValue::Corrupt => {
                        byte_budget.defer();
                        break 'scan;
                    }
                };
                if !byte_budget.try_consume(nbytes) {
                    break 'scan;
                }
                let Ok(envelope) = serde_json::from_str::<Value>(&value) else {
                    last_scanned_key = Some(key);
                    continue;
                };
                let Some(composer_id) = envelope
                    .get("composerId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() as u64 <= MAX_COMPOSER_SQLITE_KEY_BYTES)
                    .map(str::to_string)
                    .or_else(|| composer_id_from_envelope_key(&key).map(str::to_string))
                else {
                    last_scanned_key = Some(key);
                    continue;
                };
                let Some(project) = envelope_project(&envelope) else {
                    last_scanned_key = Some(key);
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
                // `Unknown` (bounded git timeout) stops before the envelope's
                // watermark, so the next sweep re-resolves membership instead
                // of misfiling or starving the session behind a growing tail.
                let project_membership = hotpath::measure_block!(
                    "sessions.hosts.cursor_composer.envelope_scope_blocking",
                    run_blocking_transcript_section(
                        || scope_matcher.membership(Some(Path::new(&project.path)))
                    )
                );
                match project_membership {
                    ProjectMembership::Match => {}
                    ProjectMembership::NoMatch => {
                        last_scanned_key = Some(key);
                        continue;
                    }
                    ProjectMembership::Unknown => {
                        byte_budget.defer();
                        break 'scan;
                    }
                }
                let selected_project = ComposerProject {
                    path: context.scoped_project_label(&project.path),
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
                    byte_budget.defer();
                    break 'scan;
                }
                let mut message_ids = headers
                    .iter()
                    .filter_map(|header| header.get("bubbleId").and_then(Value::as_str))
                    .filter(|bubble_id| {
                        format!("bubbleId:{composer_id}:{bubble_id}").len() as u64
                            <= MAX_COMPOSER_SQLITE_KEY_BYTES
                    })
                    .map(|bubble_id| format!("{composer_id}:{bubble_id}"))
                    .collect::<Vec<_>>();
                message_ids.sort_unstable();
                message_ids.dedup();
                let mut existing_message_ids = HashSet::with_capacity(message_ids.len());
                let mut identity_lookup_available = true;
                for message_id_batch in message_ids.chunks(SESSION_MESSAGE_ID_LOOKUP_MAX) {
                    match context
                        .facade
                        .existing_session_message_ids(
                            &context.scope,
                            PROVIDER,
                            message_id_batch.to_vec(),
                        )
                        .await
                    {
                        Ok(existing) => existing_message_ids.extend(existing),
                        Err(_) => {
                            byte_budget.defer();
                            identity_lookup_available = false;
                            break;
                        }
                    }
                }
                if !identity_lookup_available {
                    break 'scan;
                }
                let generation = state_generation;
                let mut session_accepted = false;
                if composer_todos_have_admittable_items(&envelope)
                    && let Some(todo_checkpoint) = composer_envelope_todo_checkpoint(&envelope)
                    && let Ok(envelope_source) = cursor_composer_envelope_source(&composer_id)
                {
                    let envelope_expected_cursor = match context
                        .facade
                        .get_source_cursor(&envelope_source, &context.scope)
                        .await
                    {
                        Ok(cursor) => cursor,
                        Err(_) => {
                            byte_budget.defer();
                            break 'scan;
                        }
                    };
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
                        && let Ok(request) =
                            build_cursor_composer_envelope_capture_request_for_project(
                                &composer_id,
                                &envelope,
                                Some(&selected_project.path),
                                context.scope.clone(),
                                generation,
                                envelope_expected_cursor,
                                context.cancellation,
                            )
                    {
                        match context.facade.capture_observation(request).await {
                            Ok(CaptureObservationOutcome::Persisted { .. })
                            | Ok(CaptureObservationOutcome::AcceptedForReplay { .. }) => {
                                session_accepted = true;
                            }
                            Err(_) => {
                                byte_budget.defer();
                                break 'scan;
                            }
                            _ => {}
                        }
                    }
                }
                for (position, header) in headers.iter().enumerate() {
                    if context.cancellation.is_cancelled() {
                        break;
                    }
                    let Some(bubble_id) = header.get("bubbleId").and_then(Value::as_str) else {
                        continue;
                    };
                    if existing_message_ids.contains(&format!("{composer_id}:{bubble_id}")) {
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
                        byte_budget.defer();
                        break 'scan;
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
                        break 'scan;
                    }
                    match fetch_bubble_bounded(
                        conn,
                        &composer_id,
                        bubble_id,
                        byte_budget.remaining(),
                    )
                    .await
                    {
                        BoundedSqliteValue::Missing => {
                            byte_budget.defer();
                            break 'scan;
                        }
                        BoundedSqliteValue::Oversized { byte_len } => {
                            if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                                break 'scan;
                            }
                            if advance_composer_coverage(
                                ComposerCoverageContext {
                                    facade: context.facade,
                                    scope: &context.scope,
                                    generation,
                                    cancellation: context.cancellation,
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
                                break 'scan;
                            }
                        }
                        BoundedSqliteValue::Malformed { byte_len } => {
                            if !byte_budget.try_consume(composer_source_charge(byte_len)) {
                                break 'scan;
                            }
                            if advance_composer_coverage(
                                ComposerCoverageContext {
                                    facade: context.facade,
                                    scope: &context.scope,
                                    generation,
                                    cancellation: context.cancellation,
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
                                break 'scan;
                            }
                        }
                        BoundedSqliteValue::BudgetExceeded { .. } => {
                            byte_budget.defer();
                            break 'scan;
                        }
                        BoundedSqliteValue::Corrupt => {
                            byte_budget.defer();
                            break 'scan;
                        }
                        BoundedSqliteValue::Ready {
                            byte_len,
                            value: bubble,
                        } => {
                            if !byte_budget
                                .try_consume(byte_len.max(composer_budget_bytes(&bubble)))
                            {
                                break 'scan;
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
                                context.cancellation,
                            );
                            let Ok(request) = request else {
                                if advance_composer_coverage(
                                    ComposerCoverageContext {
                                        facade: context.facade,
                                        scope: &context.scope,
                                        generation,
                                        cancellation: context.cancellation,
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
                                    break 'scan;
                                }
                                continue;
                            };
                            match context.facade.capture_observation(request).await {
                                Ok(CaptureObservationOutcome::Persisted { .. })
                                | Ok(CaptureObservationOutcome::AcceptedForReplay { .. }) => {
                                    session_accepted = true;
                                }
                                Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                                    if advance_composer_coverage(
                                        ComposerCoverageContext {
                                            facade: context.facade,
                                            scope: &context.scope,
                                            generation,
                                            cancellation: context.cancellation,
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
                                        break 'scan;
                                    }
                                }
                                Ok(CaptureObservationOutcome::Quarantined { receipt, .. }) => {
                                    if advance_composer_coverage(
                                        ComposerCoverageContext {
                                            facade: context.facade,
                                            scope: &context.scope,
                                            generation,
                                            cancellation: context.cancellation,
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
                                        break 'scan;
                                    }
                                }
                                Err(_) => {
                                    byte_budget.defer();
                                    break 'scan;
                                }
                            }
                        }
                    }
                }
                if session_accepted {
                    ingested_this_pass += 1;
                }
                // All headers are now durable duplicates, persisted, or covered by
                // a terminal disposition. Transient exits above leave this key as
                // the next pass's first candidate.
                last_scanned_key = Some(key);
            }
            if !page_full {
                reached_end = true;
                break;
            }
            scan_after = Some(last_key);
        }
        if context.cancellation.is_cancelled() {
            return;
        }
        let next_after = (!reached_end).then_some(last_scanned_key).flatten();
        if next_after != initial_after {
            let replacement = match serde_json::to_string(&ComposerScanFrontier {
                after_key: next_after,
            }) {
                Ok(replacement) => replacement,
                Err(_) => {
                    byte_budget.defer();
                    return;
                }
            };
            match context
                .facade
                .compare_and_swap_session_backfill_state(
                    &context.scope,
                    &frontier_key,
                    expected_frontier.as_deref(),
                    &replacement,
                )
                .await
            {
                Ok(true) => {}
                Ok(false) | Err(_) => byte_budget.defer(),
            }
        }
    }

    #[hotpath::skip]
    async fn ingest_chat_store_dbs(
        &self,
        context: &ComposerIngestContext<'_, '_>,
        workspace_paths: &HashMap<String, String>,
        byte_budget: &mut IngestByteBudget,
        outcome: &mut CursorComposerSweepOutcome,
    ) {
        let stores = hotpath::measure_block!(
            "sessions.hosts.cursor_composer.discover_stores_blocking",
            run_blocking_transcript_section(|| {
                discover_chat_store_dbs(
                    &self.chats_dir,
                    workspace_paths,
                    &context.scope_matcher(),
                    context.project_root.is_some(),
                )
            })
        );
        for (store_path, project_path) in stores {
            if context.cancellation.is_cancelled() {
                return;
            }
            self.ingest_one_store_db(context, &store_path, &project_path, byte_budget, outcome)
                .await;
        }
    }

    #[hotpath::skip]
    async fn ingest_one_store_db(
        &self,
        context: &ComposerIngestContext<'_, '_>,
        store_path: &Path,
        project_path: &str,
        byte_budget: &mut IngestByteBudget,
        outcome: &mut CursorComposerSweepOutcome,
    ) {
        if context.cancellation.is_cancelled() {
            return;
        }
        let ro = match open_readonly_immutable(store_path).await {
            Ok(ro) => ro,
            Err(error) => {
                tracing::debug!(
                    store_db = %store_path.display(),
                    error,
                    "Cursor composer store database open failed closed"
                );
                byte_budget.defer();
                return;
            }
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
            BoundedSqliteValue::Corrupt => {
                byte_budget.defer();
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

        let generation = ro.generation;
        let Ok(source) = cursor_composer_source(&session_id) else {
            return;
        };
        for (ordinal, (role, content, source_bytes)) in ordered.into_iter().enumerate() {
            if context.cancellation.is_cancelled() {
                break;
            }
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
            let text = tracedecay_lcm::message_storage_text(&content);
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
                context.cancellation,
            );
            let Ok(request) = request else {
                if advance_composer_coverage(
                    ComposerCoverageContext {
                        facade: context.facade,
                        scope: &context.scope,
                        generation,
                        cancellation: context.cancellation,
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
                Ok(CaptureObservationOutcome::Persisted { .. })
                | Ok(CaptureObservationOutcome::AcceptedForReplay { .. }) => {}
                Ok(CaptureObservationOutcome::Rejected { receipt, .. }) => {
                    if advance_composer_coverage(
                        ComposerCoverageContext {
                            facade: context.facade,
                            scope: &context.scope,
                            generation,
                            cancellation: context.cancellation,
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
                            cancellation: context.cancellation,
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
                Err(_) => {
                    byte_budget.defer();
                    return;
                }
            }
        }
    }
}
