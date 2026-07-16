//! Hermes Agent transcript source.
//!
//! Hermes does not write transcript files: every conversation lives in a
//! per-profile `SQLite` store at `<profile>/state.db` (tables `sessions` +
//! `messages`), where `<profile>` is `~/.hermes` for the default profile or
//! `~/.hermes/profiles/<name>` for named profiles. A profile maps to exactly
//! one ingest target only when provenance proves a real code project: a
//! legacy `plugins.tracedecay.project_root` pin or the session row's `cwd`.
//! For projectless/gateway sessions, one completed turn may instead prove its
//! project through structured tool-call routing (`project_path`,
//! `project_root`, or a nested project selector). Only that turn is admitted to
//! the project scope; an entire long-running multi-project chat is never assigned
//! by inference.
//! Profile directories are never `TraceDecay` project identities.
//!
//! Each bounded `SQLite` row is admitted through the shared observation privacy,
//! cursor, persistence, duplicate, collision, and projection-queue authority.
//! `SQLite` row ids are generation-local ordering evidence only; native identity
//! is derived from immutable Hermes session and message evidence.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde_json::{Value, json};
#[cfg(any(unix, windows))]
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, CanonicalReasoningVisibilityV1,
    ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, ProjectId, ProviderId, RetentionClass, SessionId,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::agents::hermes::read_config_pinned_project_root;
use crate::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::{
    MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    parse_normalized_observation_record_v1,
};
use crate::sessions::ingest_byte_budget::IngestByteBudget;
use crate::sessions::shared::{
    ProjectRootMatcher, StoredCursor, TranscriptIngestStats, path_belongs_to_project,
};
use crate::sessions::source::{STRICT_JSONL_BATCH_BYTES, SqliteFileIdentityError};
const PROVIDER: &str = "hermes";
const OBSERVATION_RETENTION: &str = "transcript.hermes.v1";
/// Maximum messages joined per `SQLite` page (row-count bound before collection).
const CHUNK_ROWS: usize = 2000;
/// Per-value payload bound before `String` materialization (observation record cap).
const MAX_HERMES_VALUE_BYTES: usize = MAX_OBSERVATION_RECORD_BYTES;
/// Identity/text metadata bound (matches `SessionId` canonical max of 512 bytes).
const MAX_HERMES_IDENTITY_BYTES: usize = 512;
/// Cumulative SQL-measured bytes admitted into one page (reuses JSONL batch bound).
const MAX_HERMES_PAGE_BYTES: u64 = STRICT_JSONL_BATCH_BYTES;
const MAX_HERMES_PROJECTIONS_PER_DRAIN: usize = 256;

/// Result of a Hermes sweep with one aggregate logical source-byte budget.
#[derive(Debug, Default, Clone)]
pub struct HermesSweepOutcome {
    pub stats: TranscriptIngestStats,
    pub bytes_consumed: u64,
    pub deferred_by_byte_cap: bool,
}

/// Ingests Hermes sessions proven to belong to `project_root` into the
/// daemon-authorized canonical `project_id` scope in `db`.
///
/// Discovery is bounded to the default user integration (`~/.hermes`) and its
/// immediate named-profile children; environment overrides are ignored.
pub async fn ingest_for_project(
    db: &GlobalDb,
    project_root: &Path,
    project_id: ProjectId,
) -> TranscriptIngestStats {
    ingest_for_project_capped(db, project_root, project_id, None)
        .await
        .stats
}

/// [`ingest_for_project`] with one aggregate logical source-byte budget shared
/// across every discovered Hermes profile.
pub async fn ingest_for_project_capped(
    db: &GlobalDb,
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    let homes = crate::sessions::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default();
    ingest_homes_capped(db, &homes, project_root, project_id, max_new_bytes).await
}

/// One project-store destination for a shared Hermes source sweep.
#[derive(Clone)]
pub struct ProjectIngestDestination<'a> {
    pub db: &'a GlobalDb,
    pub project_root: &'a Path,
    pub project_id: ProjectId,
}

/// Ingests Hermes history for several registered projects while opening and
/// scanning each profile `state.db` only once. Every destination retains its
/// own authoritative source cursor, advanced atomically with canonical
/// observation persistence or typed complete-record coverage.
pub async fn ingest_for_projects(
    destinations: &[ProjectIngestDestination<'_>],
) -> TranscriptIngestStats {
    let homes = crate::sessions::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default();
    ingest_homes_for_projects(&homes, destinations).await
}

/// Test seam for [`ingest_for_projects`].
pub async fn ingest_homes_for_projects(
    hermes_homes: &[PathBuf],
    destinations: &[ProjectIngestDestination<'_>],
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for source in all_profile_sources(hermes_homes) {
        let eligible = destinations
            .iter()
            .filter(|destination| {
                source_is_candidate_for_project(&source, destination.project_root)
            })
            .cloned()
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            continue;
        }
        match try_ingest_state_db_for_projects(&source, &eligible).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping shared Hermes transcript source"
            ),
        }
    }
    for destination in destinations {
        let scope = ObservationScopeV1::Project {
            project_id: destination.project_id.clone(),
        };
        if let Err(error) = drain_hermes_projections(destination.db, &scope).await {
            tracing::debug!(error, "Hermes shared projection drain deferred");
        }
    }
    stats
}

/// [`ingest_for_project`] with explicit Hermes home directories — the test
/// seam for pointing the sweep at a temporary home instead of the real
/// `~/.hermes`.
pub async fn ingest_homes(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_id: ProjectId,
) -> TranscriptIngestStats {
    ingest_homes_capped(db, hermes_homes, project_root, project_id, None)
        .await
        .stats
}

pub async fn ingest_homes_capped(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    project_root: &Path,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    let mut outcome = HermesSweepOutcome::default();
    let mut budget = match max_new_bytes {
        Some(limit) => IngestByteBudget::bounded(limit),
        None => IngestByteBudget::unbounded(),
    };
    for source in candidate_state_dbs(hermes_homes, project_root) {
        match try_ingest_state_db_bounded(
            db,
            &source,
            project_root,
            project_id.clone(),
            &mut budget,
        )
        .await
        {
            Ok(source_stats) => outcome.stats = outcome.stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping Hermes transcript source"
            ),
        }
    }
    let scope = ObservationScopeV1::Project { project_id };
    if let Err(error) = drain_hermes_projections(db, &scope).await {
        tracing::debug!(error, "Hermes project projection drain deferred");
    }
    outcome.bytes_consumed = budget.consumed();
    outcome.deferred_by_byte_cap = budget.deferred();
    outcome
}

/// Ingests canonical historical Hermes observations into the profile scope with
/// one aggregate logical source-byte budget shared across every discovered
/// Hermes profile. Project ingestion separately admits each turn to every
/// registered project it touched using the same stable message IDs.
pub async fn ingest_user_sessions_capped(
    db: &GlobalDb,
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    let homes = crate::sessions::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default();
    ingest_user_homes_capped(db, &homes, registered_roots, max_new_bytes).await
}

pub async fn ingest_user_homes(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
) -> TranscriptIngestStats {
    ingest_user_homes_capped(db, hermes_homes, registered_roots, None)
        .await
        .stats
}

pub async fn ingest_user_homes_capped(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
    max_new_bytes: Option<u64>,
) -> HermesSweepOutcome {
    let mut outcome = HermesSweepOutcome::default();
    let mut budget = match max_new_bytes {
        Some(limit) => IngestByteBudget::bounded(limit),
        None => IngestByteBudget::unbounded(),
    };
    for source in all_profile_sources(hermes_homes) {
        match try_ingest_user_state_db_bounded(db, &source, registered_roots, &mut budget).await {
            Ok(source_stats) => outcome.stats = outcome.stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping projectless Hermes transcript source"
            ),
        }
    }
    if let Err(error) = drain_hermes_projections(db, &ObservationScopeV1::Profile).await {
        tracing::debug!(error, "Hermes profile projection drain deferred");
    }
    outcome.bytes_consumed = budget.consumed();
    outcome.deferred_by_byte_cap = budget.deferred();
    outcome
}

/// Strict one-time import for a legacy profile whose project pin was already
/// resolved by the migration layer. Unlike the normal catch-up sweep, any
/// open/query/write failure is returned so callers retain the pin and source.
pub(crate) async fn ingest_legacy_pinned_profile(
    db: &GlobalDb,
    profile_dir: &Path,
    project_root: &Path,
    project_id: ProjectId,
) -> Result<TranscriptIngestStats, String> {
    let state_db = profile_dir.join("state.db");
    if !state_db.is_file() {
        return Ok(TranscriptIngestStats::default());
    }
    let legacy_project_pin = read_config_pinned_project_root(&profile_dir.join("config.yaml"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            format!(
                "legacy Hermes state store '{}' has no project pin",
                state_db.display()
            )
        })?;
    let source = HermesProfileSource {
        state_db,
        legacy_project_pin: Some(legacy_project_pin),
        profile: profile_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
    };
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let stats = try_ingest_state_db(db, &source, project_root, project_id).await?;
    drain_hermes_projections(db, &scope).await?;
    Ok(stats)
}

/// Locates the `state.db` of every profile that maps to `project_root`.
///
/// A legacy project pin may associate an entire profile. Otherwise the
/// profile is only a bounded candidate source and each session must carry a
/// matching code-project cwd.
///
struct HermesProfileSource {
    state_db: PathBuf,
    legacy_project_pin: Option<PathBuf>,
    profile: Option<String>,
}

fn all_profile_sources(hermes_homes: &[PathBuf]) -> Vec<HermesProfileSource> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for home in hermes_homes {
        let mut profiles = vec![(home.clone(), None)];
        if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
            profiles.extend(entries.filter_map(|entry| {
                let path = entry.ok()?.path();
                path.is_dir().then(|| {
                    let name = path.file_name()?.to_str()?.to_string();
                    Some((path, Some(name)))
                })?
            }));
        }
        for (profile_dir, profile) in profiles {
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    legacy_project_pin: read_config_pinned_project_root(
                        &profile_dir.join("config.yaml"),
                    )
                    .map(PathBuf::from),
                    profile,
                });
            }
        }
    }
    out
}

fn candidate_state_dbs(hermes_homes: &[PathBuf], project_root: &Path) -> Vec<HermesProfileSource> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let project_is_real = crate::worktree::git_worktree_root(project_root).is_some()
        || crate::config::has_project_database(project_root);
    for home in hermes_homes {
        let mut candidates: Vec<(PathBuf, Option<String>)> = vec![(home.clone(), None)];
        if let Ok(entries) = std::fs::read_dir(home.join("profiles")) {
            let mut profiles = entries
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    entry.file_type().ok()?.is_dir().then(|| entry.path())
                })
                .collect::<Vec<_>>();
            profiles.sort();
            for profile_dir in profiles {
                let name = profile_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string);
                candidates.push((profile_dir, name));
            }
        }
        for (profile_dir, profile) in candidates {
            let legacy_project_pin =
                read_config_pinned_project_root(&profile_dir.join("config.yaml"))
                    .map(PathBuf::from);
            if legacy_project_pin
                .as_deref()
                .is_some_and(|pin| !path_belongs_to_project(pin, project_root))
                || (legacy_project_pin.is_none() && !project_is_real)
            {
                continue;
            }
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    legacy_project_pin,
                    profile,
                });
            }
        }
    }
    out
}

fn source_is_candidate_for_project(source: &HermesProfileSource, project_root: &Path) -> bool {
    if source
        .legacy_project_pin
        .as_deref()
        .is_some_and(|pin| !path_belongs_to_project(pin, project_root))
    {
        return false;
    }
    source.legacy_project_pin.is_some()
        || crate::worktree::git_worktree_root(project_root).is_some()
        || crate::config::has_project_database(project_root)
}

/// One joined `messages` × `sessions` row read past the cursor.
struct HermesRow {
    id: i64,
    session_id: String,
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    tool_name: Option<String>,
    tool_calls: Option<String>,
    timestamp: Option<f64>,
    session_model: Option<String>,
    parent_session_id: Option<String>,
    session_cwd: Option<String>,
    session_source: Option<String>,
    session_title: Option<String>,
    session_started_at: Option<f64>,
    session_ended_at: Option<f64>,
    session_input_tokens: Option<i64>,
    session_output_tokens: Option<i64>,
    session_cache_read_tokens: Option<i64>,
    session_cache_write_tokens: Option<i64>,
    session_reasoning_tokens: Option<i64>,
    /// `messages.active` soft-delete flag (0 = rewound/undone turn). Legacy
    /// stores without the column read as 1.
    active: i64,
    /// Set when SQL `typeof`/`length` rejected a column before materialization.
    sql_value_oversized: bool,
    /// Sum of SQL `length()` charges for text/blob columns (not Rust `String::len`).
    sql_measured_bytes: u64,
}

/// One bounded `SQLite` page: row count, per-value, and cumulative byte caps applied
/// before `String`/`Vec` materialization.
struct HermesPageRead {
    items: Vec<HermesRow>,
    new_cursor: StoredCursor,
    /// More rows remain at the authority, but the page byte budget stopped collection.
    truncated_by_byte_budget: bool,
}

fn text_bytes<const N: usize>(values: [Option<&str>; N]) -> u64 {
    values.into_iter().flatten().fold(0_u64, |total, value| {
        total.saturating_add(u64::try_from(value.len()).unwrap_or(u64::MAX))
    })
}

fn hermes_native_payload_bytes(row: &HermesRow) -> u64 {
    let text_bytes = text_bytes([
        Some(row.session_id.as_str()),
        Some(row.role.as_str()),
        row.content.as_deref(),
        row.reasoning.as_deref(),
        row.tool_name.as_deref(),
        row.tool_calls.as_deref(),
        row.session_model.as_deref(),
        row.parent_session_id.as_deref(),
        row.session_source.as_deref(),
        row.session_title.as_deref(),
    ]);
    let scalar_count = u64::from(row.timestamp.is_some())
        .saturating_add(u64::from(row.session_started_at.is_some()))
        .saturating_add(u64::from(row.session_ended_at.is_some()))
        .saturating_add(u64::from(row.session_input_tokens.is_some()))
        .saturating_add(u64::from(row.session_output_tokens.is_some()))
        .saturating_add(u64::from(row.session_cache_read_tokens.is_some()))
        .saturating_add(u64::from(row.session_cache_write_tokens.is_some()))
        .saturating_add(u64::from(row.session_reasoning_tokens.is_some()));
    text_bytes.saturating_add(scalar_count.saturating_mul(8))
}

fn hermes_row_bytes(row: &HermesRow) -> u64 {
    hermes_native_payload_bytes(row)
        .saturating_add(text_bytes([row.session_cwd.as_deref()]))
        .saturating_add(16)
}

fn hermes_budget_bytes(row: &HermesRow) -> u64 {
    let capped = u64::try_from(MAX_OBSERVATION_RECORD_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    // Prefer SQL-measured length (includes rejected oversize/blob sizes) so
    // hostile values charge the pass budget without being materialized.
    row.sql_measured_bytes
        .max(hermes_row_bytes(row))
        .min(capped)
}

fn hermes_page_row_charge(sql_measured_bytes: u64) -> u64 {
    let capped = u64::try_from(MAX_HERMES_VALUE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    sql_measured_bytes.min(capped)
}

struct HermesObservationRecord {
    native: Value,
    native_record_id: ObservationId,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
}

#[derive(Clone)]
struct HermesProjectionMetadata {
    project_path: Option<String>,
    location_path: Option<String>,
    profile: Option<String>,
    location_provenance: Option<&'static str>,
}

fn project_projection_metadata(
    row: &HermesRow,
    source: &HermesProfileSource,
    authority_project_root: &Path,
    location_provenance: &'static str,
) -> HermesProjectionMetadata {
    let presentation_path = match location_provenance {
        "profile_pin" => source.legacy_project_pin.as_deref(),
        "session_cwd" => row.session_cwd.as_deref().map(Path::new),
        _ => None,
    }
    .filter(|path| path.is_absolute() && path_belongs_to_project(path, authority_project_root))
    .unwrap_or(authority_project_root);
    HermesProjectionMetadata {
        project_path: Some(authority_project_root.to_string_lossy().into_owned()),
        location_path: Some(presentation_path.to_string_lossy().into_owned()),
        profile: source.profile.clone(),
        location_provenance: Some(location_provenance),
    }
}

enum HermesAdmissionAction {
    Capture(Box<CaptureObservationRequest>),
    Cover(ObservationCoverageReason),
}

struct HermesAdmission {
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    action: HermesAdmissionAction,
}

fn observation_source(row: &HermesRow) -> Result<ObservationSourceIdentityV1, String> {
    let provider = ProviderId::new(PROVIDER).map_err(|_| "invalid Hermes provider".to_string())?;
    let session_id =
        SessionId::new(&row.session_id).map_err(|_| "invalid Hermes session id".to_string())?;
    ObservationSourceIdentityV1::for_provider(provider, session_id)
        .map_err(|_| "invalid Hermes observation source".to_string())
}

#[derive(serde::Deserialize)]
struct HermesNativeObservation {
    session_id: String,
    parent_session_id: Option<String>,
    role: String,
    content: Option<String>,
    reasoning: Option<String>,
    model: Option<String>,
    tool_name: Option<String>,
    tool_calls: Option<Value>,
    timestamp: Option<f64>,
    usage: HermesNativeUsage,
    project_path: Option<String>,
    location_path: Option<String>,
    title: Option<String>,
    started_at: Option<f64>,
    ended_at: Option<f64>,
    source: Option<String>,
    profile: Option<String>,
    location_provenance: Option<String>,
}

#[derive(serde::Deserialize)]
struct HermesNativeUsage {
    #[serde(rename = "input_tokens")]
    input: Option<i64>,
    #[serde(rename = "output_tokens")]
    output: Option<i64>,
    #[serde(rename = "cache_read_tokens")]
    cache_read: Option<i64>,
    #[serde(rename = "cache_write_tokens")]
    cache_write: Option<i64>,
    #[serde(rename = "reasoning_tokens")]
    reasoning: Option<i64>,
}

fn native_observation_record(
    row: &HermesRow,
    projection: &HermesProjectionMetadata,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
) -> Result<HermesObservationRecord, ObservationCoverageReason> {
    if [row.timestamp, row.session_started_at, row.session_ended_at]
        .into_iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(ObservationCoverageReason::MalformedFrame);
    }
    let tool_calls = match row
        .tool_calls
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => Some(
            serde_json::from_str::<Value>(value)
                .map_err(|_| ObservationCoverageReason::MalformedFrame)?,
        ),
        None => None,
    };
    let native = json!({
        "session_id": row.session_id,
        "parent_session_id": row.parent_session_id,
        "role": row.role,
        "content": row.content,
        "reasoning": row.reasoning,
        "model": row.session_model,
        "tool_name": row.tool_name,
        "tool_calls": tool_calls,
        "timestamp": row.timestamp,
        "project_path": projection.project_path,
        "location_path": projection.location_path,
        "title": row.session_title,
        "started_at": row.session_started_at,
        "ended_at": row.session_ended_at,
        "source": row.session_source,
        "profile": projection.profile,
        "location_provenance": projection.location_provenance,
        "usage": {
            "input_tokens": row.session_input_tokens,
            "output_tokens": row.session_output_tokens,
            "cache_read_tokens": row.session_cache_read_tokens,
            "cache_write_tokens": row.session_cache_write_tokens,
            "reasoning_tokens": row.session_reasoning_tokens,
        },
    });
    let native_record_id = stable_native_id("hermes.native", &immutable_message_evidence(&native))
        .map_err(|()| ObservationCoverageReason::MalformedFrame)?;
    Ok(HermesObservationRecord {
        native,
        native_record_id,
        source,
        range,
    })
}

fn immutable_message_evidence(native: &Value) -> Value {
    json!({
        "session_id": native.get("session_id"),
        "role": native.get("role"),
        "content": native.get("content"),
        "reasoning": native.get("reasoning"),
        "tool_name": native.get("tool_name"),
        "tool_calls": native.get("tool_calls"),
        "timestamp_bits": native
            .get("timestamp")
            .and_then(Value::as_f64)
            .map(f64::to_bits),
    })
}

fn stable_native_id(prefix: &str, evidence: &Value) -> Result<ObservationId, ()> {
    let digest = PayloadReferenceV1::for_payload(evidence).map_err(|_| ())?;
    ObservationId::new(format!("{prefix}.{}", digest.digest().as_str())).map_err(|_| ())
}

fn normalize_native_observation(
    native: Value,
    range: ObservationSourceRangeV1,
) -> Result<CanonicalObservationEnvelopeV1, ObservationRecordParseErrorV1> {
    let native: HermesNativeObservation =
        serde_json::from_value(native).map_err(|_| ObservationRecordParseErrorV1::Malformed)?;
    let provider = ProviderId::new(PROVIDER)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let session_id = SessionId::new(&native.session_id)
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    // Preserve Hermes' public V1 message identity while the observation keeps
    // its content-derived idempotency key. SQLite row IDs are native ordering
    // evidence and remain stable for the lifetime of one state database.
    let message_id = ObservationId::new(format!("{}:{}", native.session_id, range.end()))
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let identity_evidence = json!({
        "session_id": native.session_id,
        "role": native.role,
        "content": native.content,
        "reasoning": native.reasoning,
        "tool_name": native.tool_name,
        "tool_calls": native.tool_calls,
        "timestamp_bits": native.timestamp.map(f64::to_bits),
    });
    let stable_record_id = stable_native_id("hermes.native", &identity_evidence)
        .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let agent_id = stable_native_id("hermes.session", &json!(native.session_id))
        .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    let mut relations = CanonicalObservationRelationsV1::new(session_id)
        .with_message_id(message_id)
        .with_agent_id(agent_id);
    if let Some(parent_session_id) = native.parent_session_id.as_deref() {
        let parent_session_id = SessionId::new(parent_session_id)
            .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        let parent_agent_id = stable_native_id("hermes.session", &json!(parent_session_id))
            .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        relations = relations
            .with_parent_session_id(parent_session_id)
            .with_parent_agent_id(parent_agent_id);
    }

    let role = canonical_message_role(&native.role)?;
    let mut facts = vec![CanonicalObservationFactV1::Session {
        project_path: native.project_path,
        location_path: native.location_path,
        transcript_path: None,
        title: native.title,
        started_at: native.started_at.map(|value| value as i64),
        ended_at: native.ended_at.map(|value| value as i64),
        source: Some("hermes_state_db".to_string()),
        native_source: native.source,
        profile: native.profile,
        location_provenance: native.location_provenance,
    }];
    // Message carries provider-authored content only. Empty assistant rows keep
    // typed Reasoning / ToolInvocation facts projectable instead of synthesizing
    // reasoning text or tool_calls JSON into Message.content.
    if let Some(content) = native
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .map(|content| Value::String(content.to_string()))
    {
        facts.push(CanonicalObservationFactV1::Message {
            role,
            content,
            model: native.model.clone(),
            timestamp: native.timestamp.map(|value| value as i64),
        });
    }
    if role == CanonicalMessageRoleV1::Tool {
        facts.push(CanonicalObservationFactV1::ToolResult {
            invocation_id: None,
            content: native.content.clone().map_or(Value::Null, Value::String),
            success: None,
        });
    }
    append_tool_invocations(&mut facts, native.tool_calls.as_ref(), &stable_record_id)?;

    let (visibility, content) = match native.reasoning {
        Some(content) => (
            CanonicalReasoningVisibilityV1::Visible,
            Some(Value::String(content)),
        ),
        None if role == CanonicalMessageRoleV1::Assistant => {
            (CanonicalReasoningVisibilityV1::Unavailable, None)
        }
        None => (CanonicalReasoningVisibilityV1::NotApplicable, None),
    };
    facts.push(CanonicalObservationFactV1::Reasoning {
        visibility,
        content,
    });
    // Reasoning before Usage so reasoning-only rows project as reasoning_visible
    // instead of an empty usage fallback when Message is absent.
    if let Some((
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    )) = canonical_usage(&native.usage)?
    {
        facts.push(CanonicalObservationFactV1::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            reasoning_tokens,
        });
    }

    let mut evidence =
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SqliteRowId, range)
            .with_native_sequence(range.end());
    if let Some(timestamp) = native.timestamp {
        evidence = evidence.with_native_timestamp(timestamp as i64);
    }
    CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        stable_record_id,
        relations,
        facts,
        evidence,
    )
    .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
}

fn canonical_message_role(
    role: &str,
) -> Result<CanonicalMessageRoleV1, ObservationRecordParseErrorV1> {
    match role {
        "user" => Ok(CanonicalMessageRoleV1::User),
        "assistant" => Ok(CanonicalMessageRoleV1::Assistant),
        "system" => Ok(CanonicalMessageRoleV1::System),
        "tool" => Ok(CanonicalMessageRoleV1::Tool),
        _ => Err(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope),
    }
}

fn append_tool_invocations(
    facts: &mut Vec<CanonicalObservationFactV1>,
    tool_calls: Option<&Value>,
    message_id: &ObservationId,
) -> Result<(), ObservationRecordParseErrorV1> {
    let Some(tool_calls) = tool_calls else {
        return Ok(());
    };
    let calls = tool_calls
        .as_array()
        .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
    for call in calls {
        let name = call
            .pointer("/function/name")
            .or_else(|| call.get("name"))
            .and_then(Value::as_str)
            .ok_or(ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        let arguments = call
            .pointer("/function/arguments")
            .or_else(|| call.get("arguments"))
            .cloned()
            .unwrap_or(Value::Null);
        let arguments = match arguments {
            Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
            value => value,
        };
        let invocation_evidence = match call.get("id") {
            Some(native_id) => json!({
                "message_id": message_id.as_str(),
                "native_tool_id": native_id,
            }),
            None => json!({
                "message_id": message_id.as_str(),
                "tool_call": call,
            }),
        };
        let invocation_id = stable_native_id("hermes.tool", &invocation_evidence)
            .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        facts.push(CanonicalObservationFactV1::ToolInvocation {
            invocation_id,
            name: name.to_owned(),
            arguments,
        });
    }
    Ok(())
}

type CanonicalUsage = (
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<u64>,
);

fn canonical_usage(
    usage: &HermesNativeUsage,
) -> Result<Option<CanonicalUsage>, ObservationRecordParseErrorV1> {
    let usage = (
        nonnegative_token_count(usage.input)?,
        nonnegative_token_count(usage.output)?,
        nonnegative_token_count(usage.cache_read)?,
        nonnegative_token_count(usage.cache_write)?,
        nonnegative_token_count(usage.reasoning)?,
    );
    Ok((usage.0.is_some()
        || usage.1.is_some()
        || usage.2.is_some()
        || usage.3.is_some()
        || usage.4.is_some())
    .then_some(usage))
}

fn nonnegative_token_count(
    value: Option<i64>,
) -> Result<Option<u64>, ObservationRecordParseErrorV1> {
    value
        .map(u64::try_from)
        .transpose()
        .map_err(|_| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)
}

#[allow(clippy::too_many_arguments)]
fn prepare_observation_row(
    row: &HermesRow,
    projection: Option<&HermesProjectionMetadata>,
    scope: &ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    file_identity: u64,
    resume_fingerprint: u64,
) -> Result<HermesAdmission, String> {
    let source = observation_source(row)?;
    let start = expected_cursor.as_ref().map_or(0, |cursor| {
        if cursor.generation() == generation {
            cursor.position()
        } else {
            0
        }
    });
    let end = u64::try_from(row.id).map_err(|_| "invalid Hermes SQLite row id".to_string())?;
    let range = ObservationSourceRangeV1::new(start, end)
        .map_err(|_| "invalid Hermes SQLite row range".to_string())?;
    let coverage = if row.sql_value_oversized
        || hermes_native_payload_bytes(row)
            > u64::try_from(MAX_OBSERVATION_RECORD_BYTES).unwrap_or(u64::MAX)
    {
        Some(ObservationCoverageReason::OversizedFrame)
    } else if projection.is_none() || row.active == 0 {
        Some(ObservationCoverageReason::OutOfScope)
    } else if !matches!(row.role.as_str(), "user" | "assistant" | "tool" | "system") {
        Some(ObservationCoverageReason::UnsupportedFact)
    } else if row
        .content
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        && row
            .reasoning
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && row
            .tool_calls
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && row
            .tool_name
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        Some(ObservationCoverageReason::BlankFrame)
    } else {
        None
    };
    let action = if let Some(reason) = coverage {
        HermesAdmissionAction::Cover(reason)
    } else {
        let projection = projection
            .ok_or_else(|| "admitted Hermes row has no projection metadata".to_string())?;
        match native_observation_record(row, projection, source.clone(), range) {
            Err(reason) => HermesAdmissionAction::Cover(reason),
            Ok(normalized) => {
                let encoded = serde_json::to_vec(&normalized.native)
                    .map_err(|_| "could not encode Hermes observation".to_string())?;
                if encoded.len() > MAX_OBSERVATION_RECORD_BYTES {
                    HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
                } else {
                    match parse_normalized_observation_record_v1(
                        &encoded,
                        normalized.range,
                        ObservationOrderingDomainV1::SqliteRowId,
                        |native| normalize_native_observation(native, normalized.range),
                    ) {
                        Err(
                            ObservationRecordParseErrorV1::TooLarge
                            | ObservationRecordParseErrorV1::CanonicalEnvelopeTooLarge,
                        ) => {
                            HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
                        }
                        Err(ObservationRecordParseErrorV1::Empty) => {
                            HermesAdmissionAction::Cover(ObservationCoverageReason::BlankFrame)
                        }
                        Err(_) => {
                            HermesAdmissionAction::Cover(ObservationCoverageReason::MalformedFrame)
                        }
                        Ok(parsed) => {
                            let identity = ObservationIdentityMaterialV1::for_native_record(
                                normalized.source,
                                scope.clone(),
                                generation,
                                normalized.range,
                                ObservationOrderingDomainV1::SqliteRowId,
                                normalized.native_record_id,
                            )
                            .map_err(|_| "invalid Hermes observation identity".to_string())?;
                            let retention = RetentionClass::new(OBSERVATION_RETENTION)
                                .map_err(|_| "invalid Hermes retention class".to_string())?;
                            let request = CaptureObservationRequest::new(
                                parsed,
                                identity,
                                expected_cursor.clone(),
                                retention,
                                ObservationCancellation::default(),
                            )
                            .map_err(|_| "invalid Hermes capture request".to_string())?
                            .with_resume_checkpoint(file_identity, resume_fingerprint);
                            HermesAdmissionAction::Capture(Box::new(request))
                        }
                    }
                }
            }
        }
    };
    Ok(HermesAdmission {
        source,
        range,
        expected_cursor,
        action,
    })
}

fn sqlite_incarnation(path: &Path) -> Result<(ObservationSourceGenerationV1, u64, u64), String> {
    let file_identity =
        crate::sessions::source::sqlite_generation_identity(path).map_err(|error| {
            match error {
                SqliteFileIdentityError::Open => "could not open Hermes SQLite authority",
                SqliteFileIdentityError::Inspect => "could not inspect Hermes SQLite authority",
                SqliteFileIdentityError::Identify => "could not identify Hermes SQLite authority",
                SqliteFileIdentityError::Unavailable => {
                    "Hermes SQLite physical identity is unavailable"
                }
            }
            .to_string()
        })?;
    let resume_fingerprint = sqlite_resume_fingerprint(path, file_identity)?;
    let generation = ObservationSourceGenerationV1::new(file_identity)
        .map_err(|_| "invalid Hermes SQLite generation".to_string())?;
    Ok((generation, file_identity, resume_fingerprint))
}

#[cfg(unix)]
fn sqlite_resume_fingerprint(path: &Path, file_identity: u64) -> Result<u64, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|_| "could not inspect Hermes SQLite authority".to_string())?;
    let mut resume_hasher = Sha256::new();
    resume_hasher.update(file_identity.to_le_bytes());
    resume_hasher.update(metadata.len().to_le_bytes());
    resume_hasher.update(file_mtime_secs(path).to_le_bytes());
    let resume_digest = resume_hasher.finalize();
    let mut resume_bytes = [0_u8; 8];
    resume_bytes.copy_from_slice(&resume_digest[..8]);
    Ok(u64::from_le_bytes(resume_bytes))
}

#[cfg(windows)]
fn sqlite_resume_fingerprint(path: &Path, file_identity: u64) -> Result<u64, String> {
    use std::os::windows::fs::MetadataExt;

    let metadata = std::fs::metadata(path)
        .map_err(|_| "could not inspect Hermes SQLite authority".to_string())?;
    let mut resume_hasher = Sha256::new();
    resume_hasher.update(file_identity.to_le_bytes());
    resume_hasher.update(metadata.len().to_le_bytes());
    resume_hasher.update(metadata.last_write_time().to_le_bytes());
    let resume_digest = resume_hasher.finalize();
    let mut resume_bytes = [0_u8; 8];
    resume_bytes.copy_from_slice(&resume_digest[..8]);
    Ok(u64::from_le_bytes(resume_bytes))
}

#[cfg(not(any(unix, windows)))]
fn sqlite_resume_fingerprint(path: &Path, _file_identity: u64) -> Result<u64, String> {
    let _ = path;
    Err("Hermes SQLite physical identity is unavailable".to_string())
}

#[allow(clippy::too_many_arguments)]
async fn advance_coverage(
    facade: &HostAdmissionFacade<'_>,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    reason: ObservationCoverageReason,
    receipt: Option<tracedecay_domain::SanitizationReceiptV1>,
    file_identity: u64,
    resume_fingerprint: u64,
) -> Result<(), String> {
    let advance = match receipt {
        Some(receipt) => ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            expected_cursor,
            range,
            reason,
            receipt,
        ),
        None => ObservationCursorAdvance::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            expected_cursor,
            range,
            reason,
        ),
    }
    .map_err(|_| "invalid Hermes coverage transition".to_string())?
    .with_resume_checkpoint(file_identity, resume_fingerprint);
    facade
        .advance_non_durable_source_cursor(advance, ObservationCancellation::default())
        .await
        .map(|_| ())
        .map_err(host_admission_error)
}

fn host_admission_error(outcome: HostAdmissionOutcome) -> String {
    crate::sessions::snapshot_observation::host_admission_status_message("Hermes", outcome.status)
}

async fn drain_hermes_projections(db: &GlobalDb, scope: &ObservationScopeV1) -> Result<(), String> {
    let authorities = match scope {
        ObservationScopeV1::Project { project_id } => {
            HostAdmissionAuthorities::for_project(db, project_id.clone())
        }
        ObservationScopeV1::Profile => HostAdmissionAuthorities::for_profile(db),
    };
    let facade = HostAdmissionFacade::new(authorities);
    loop {
        let outcome = facade
            .drain_projection_queue(
                PROVIDER,
                scope,
                &ObservationCancellation::default(),
                MAX_HERMES_PROJECTIONS_PER_DRAIN,
            )
            .await
            .map_err(host_admission_error)?;
        let processed = outcome
            .projected
            .saturating_add(outcome.skipped)
            .saturating_add(outcome.exact_duplicates);
        if processed < u64::try_from(MAX_HERMES_PROJECTIONS_PER_DRAIN).unwrap_or(u64::MAX) {
            return Ok(());
        }
    }
}

async fn admit_rows(
    db: &GlobalDb,
    rows: &[HermesRow],
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    resume_fingerprint: u64,
    route: impl Fn(&HermesRow) -> Option<HermesProjectionMetadata>,
) -> Result<TranscriptIngestStats, String> {
    let authorities = match &scope {
        ObservationScopeV1::Project { project_id } => {
            HostAdmissionAuthorities::for_project(db, project_id.clone())
        }
        ObservationScopeV1::Profile => HostAdmissionAuthorities::for_profile(db),
    };
    let facade = HostAdmissionFacade::new(authorities);
    let mut stats = TranscriptIngestStats::default();
    let mut sessions = BTreeSet::new();
    for row in rows {
        let source = observation_source(row)?;
        let expected_cursor = facade
            .get_source_cursor(&source, &scope)
            .await
            .map_err(host_admission_error)?;
        let end = u64::try_from(row.id).map_err(|_| "invalid Hermes SQLite row id".to_string())?;
        if expected_cursor
            .as_ref()
            .is_some_and(|cursor| cursor.generation() == generation && cursor.position() >= end)
        {
            continue;
        }
        let admission = prepare_observation_row(
            row,
            route(row).as_ref(),
            &scope,
            generation,
            expected_cursor,
            file_identity,
            resume_fingerprint,
        )?;
        let HermesAdmission {
            source,
            range,
            expected_cursor,
            action,
        } = admission;
        match action {
            HermesAdmissionAction::Cover(reason) => {
                advance_coverage(
                    &facade,
                    source,
                    range,
                    expected_cursor,
                    scope.clone(),
                    generation,
                    reason,
                    None,
                    file_identity,
                    resume_fingerprint,
                )
                .await?;
            }
            HermesAdmissionAction::Capture(request) => {
                match facade
                    .capture_observation(*request)
                    .await
                    .map_err(host_admission_error)?
                {
                    CaptureObservationOutcome::Persisted { outcome, .. } => {
                        if matches!(outcome, ObservationPersistOutcome::Committed(_)) {
                            stats.messages_upserted = stats.messages_upserted.saturating_add(1);
                        }
                        sessions.insert(row.session_id.clone());
                    }
                    CaptureObservationOutcome::Rejected { receipt, .. } => {
                        advance_coverage(
                            &facade,
                            source,
                            range,
                            expected_cursor,
                            scope.clone(),
                            generation,
                            ObservationCoverageReason::SanitizerRejected,
                            Some(receipt),
                            file_identity,
                            resume_fingerprint,
                        )
                        .await?;
                    }
                    CaptureObservationOutcome::Quarantined { receipt, .. } => {
                        advance_coverage(
                            &facade,
                            source,
                            range,
                            expected_cursor,
                            scope.clone(),
                            generation,
                            ObservationCoverageReason::SanitizerQuarantined,
                            Some(receipt),
                            file_identity,
                            resume_fingerprint,
                        )
                        .await?;
                    }
                }
            }
        }
    }
    stats.sessions_upserted = sessions.len() as u64;
    Ok(stats)
}

/// Column names of the `messages` table — `active` (v12 rewind soft-delete)
/// and `reasoning` arrived in later Hermes schema revisions, so the sweep
/// probes before selecting to stay readable on legacy stores.
async fn message_columns(
    conn: &libsql::Connection,
) -> Result<std::collections::BTreeSet<String>, String> {
    table_columns(conn, "messages").await
}

async fn table_columns(
    conn: &libsql::Connection,
    table: &str,
) -> Result<std::collections::BTreeSet<String>, String> {
    let mut out = std::collections::BTreeSet::new();
    let query = format!("SELECT name FROM pragma_table_info('{table}')");
    let mut rows = conn
        .query(&query, ())
        .await
        .map_err(|_| "could not inspect Hermes SQLite schema".to_string())?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|_| "could not read Hermes SQLite schema".to_string())?
    {
        let name = row
            .get::<String>(0)
            .map_err(|_| "Hermes SQLite schema row is malformed".to_string())?;
        out.insert(name);
    }
    if out.is_empty() {
        return Err("Hermes SQLite authority is incomplete".to_string());
    }
    Ok(out)
}

fn sql_byte_len(expr: &str) -> String {
    format!("length(CAST({expr} AS BLOB))")
}

/// Returns the column only when it is TEXT, within `max_bytes`, and the whole
/// row fits the current cumulative page budget. Hostile BLOB/`zeroblob`
/// values and rows deferred to the next page never appear in the result set.
fn sql_bounded_text(expr: &str, max_bytes: usize, row_fits_budget: &str) -> String {
    let byte_len = sql_byte_len(expr);
    format!(
        "CASE WHEN ({row_fits_budget}) AND typeof({expr}) = 'text' \
              AND {byte_len} <= {max_bytes} THEN {expr} ELSE NULL END"
    )
}

/// Returns only `SQLite`'s fixed-size numeric representations. `SQLite` columns
/// are dynamically typed, so selecting a nominal REAL/INTEGER column directly
/// could otherwise materialize an attacker-controlled TEXT/BLOB value.
fn sql_bounded_number(expr: &str) -> String {
    format!("CASE WHEN typeof({expr}) IN ('integer', 'real') THEN {expr} ELSE NULL END")
}

/// SQL UTF-8/blob byte charge without returning the value. Caps each column at
/// `max_bytes + 1` so oversized/zeroblob sizes cannot inflate page accounting
/// unboundedly while still signaling oversize.
fn sql_capped_len(expr: &str, max_bytes: usize) -> String {
    let cap = max_bytes.saturating_add(1);
    let byte_len = sql_byte_len(expr);
    format!(
        "CASE
            WHEN {expr} IS NULL THEN 0
            WHEN typeof({expr}) IN ('text', 'blob') AND {byte_len} > {max_bytes} THEN {cap}
            WHEN typeof({expr}) IN ('text', 'blob') THEN {byte_len}
            WHEN typeof({expr}) IN ('integer', 'real') THEN length(CAST({expr} AS BLOB))
            ELSE {cap}
         END"
    )
}

fn sql_value_oversized(expr: &str, max_bytes: usize) -> String {
    let byte_len = sql_byte_len(expr);
    format!(
        "CASE
            WHEN {expr} IS NULL THEN 0
            WHEN typeof({expr}) = 'text' AND {byte_len} <= {max_bytes} THEN 0
            ELSE 1
         END"
    )
}

fn select_new_messages_sql(
    message_columns: &std::collections::BTreeSet<String>,
    session_columns: &std::collections::BTreeSet<String>,
) -> String {
    let reasoning_raw = if message_columns.contains("reasoning") {
        "m.reasoning"
    } else {
        "NULL"
    };
    let active_expr = if message_columns.contains("active") {
        "m.active"
    } else {
        "1"
    };
    let session_cwd_raw = if session_columns.contains("cwd") {
        "s.cwd"
    } else {
        "NULL"
    };
    let session_source_raw = if session_columns.contains("source") {
        "s.source"
    } else {
        "NULL"
    };
    let session_title_raw = if session_columns.contains("title") {
        "s.title"
    } else {
        "NULL"
    };
    let session_started_at = if session_columns.contains("started_at") {
        "s.started_at"
    } else {
        "NULL"
    };
    let session_ended_at = if session_columns.contains("ended_at") {
        "s.ended_at"
    } else {
        "NULL"
    };
    let id_max = MAX_HERMES_IDENTITY_BYTES;
    let value_max = MAX_HERMES_VALUE_BYTES;
    let measured = format!(
        "{session_id_len} + {role_len} + {content_len} + {reasoning_len} + {tool_name_len} + {tool_calls_len} + {model_len} + {parent_len} + {cwd_len} + {source_len} + {title_len}",
        session_id_len = sql_capped_len("m.session_id", id_max),
        role_len = sql_capped_len("m.role", id_max),
        content_len = sql_capped_len("m.content", value_max),
        reasoning_len = sql_capped_len(reasoning_raw, value_max),
        tool_name_len = sql_capped_len("m.tool_name", id_max),
        tool_calls_len = sql_capped_len("m.tool_calls", value_max),
        model_len = sql_capped_len("s.model", id_max),
        parent_len = sql_capped_len("s.parent_session_id", id_max),
        cwd_len = sql_capped_len(session_cwd_raw, value_max),
        source_len = sql_capped_len(session_source_raw, id_max),
        title_len = sql_capped_len(session_title_raw, value_max),
    );
    let oversized = format!(
        "CASE WHEN ({session_id_os} + {role_os} + {content_os} + {reasoning_os} + {tool_name_os} + {tool_calls_os} + {model_os} + {parent_os} + {cwd_os} + {source_os} + {title_os}) > 0 THEN 1 ELSE 0 END",
        session_id_os = sql_value_oversized("m.session_id", id_max),
        role_os = sql_value_oversized("m.role", id_max),
        content_os = sql_value_oversized("m.content", value_max),
        reasoning_os = sql_value_oversized(reasoning_raw, value_max),
        tool_name_os = sql_value_oversized("m.tool_name", id_max),
        tool_calls_os = sql_value_oversized("m.tool_calls", value_max),
        model_os = sql_value_oversized("s.model", id_max),
        parent_os = sql_value_oversized("s.parent_session_id", id_max),
        cwd_os = sql_value_oversized(session_cwd_raw, value_max),
        source_os = sql_value_oversized(session_source_raw, id_max),
        title_os = sql_value_oversized(session_title_raw, value_max),
    );
    let row_fits_budget = format!("({measured}) <= ?2");
    let session_id = sql_bounded_text("m.session_id", id_max, &row_fits_budget);
    let role = sql_bounded_text("m.role", id_max, &row_fits_budget);
    let content = sql_bounded_text("m.content", value_max, &row_fits_budget);
    let reasoning = sql_bounded_text(reasoning_raw, value_max, &row_fits_budget);
    let tool_name = sql_bounded_text("m.tool_name", id_max, &row_fits_budget);
    let tool_calls = sql_bounded_text("m.tool_calls", value_max, &row_fits_budget);
    let model = sql_bounded_text("s.model", id_max, &row_fits_budget);
    let parent_session_id = sql_bounded_text("s.parent_session_id", id_max, &row_fits_budget);
    let session_cwd = sql_bounded_text(session_cwd_raw, value_max, &row_fits_budget);
    let session_source = sql_bounded_text(session_source_raw, id_max, &row_fits_budget);
    let session_title = sql_bounded_text(session_title_raw, value_max, &row_fits_budget);
    let timestamp = sql_bounded_number("m.timestamp");
    let session_started_at = sql_bounded_number(session_started_at);
    let session_ended_at = sql_bounded_number(session_ended_at);
    let input_tokens = sql_bounded_number("s.input_tokens");
    let output_tokens = sql_bounded_number("s.output_tokens");
    let cache_read_tokens = sql_bounded_number("s.cache_read_tokens");
    let cache_write_tokens = sql_bounded_number("s.cache_write_tokens");
    let reasoning_tokens = sql_bounded_number("s.reasoning_tokens");
    let active = sql_bounded_number(active_expr);
    let typed_oversized = format!(
        "CASE WHEN ({oversized}) > 0 OR ({measured}) > {MAX_HERMES_PAGE_BYTES} \
              THEN 1 ELSE 0 END"
    );
    format!(
        "SELECT m.id,
                {session_id},
                {role},
                {content},
                {reasoning},
                {tool_name},
                {tool_calls},
                {timestamp},
                {model},
                {parent_session_id},
                {session_cwd},
                {session_source},
                {session_title},
                {session_started_at},
                {session_ended_at},
                {input_tokens}, {output_tokens}, {cache_read_tokens}, {cache_write_tokens},
                {reasoning_tokens}, {active},
                CAST(({measured}) AS INTEGER) AS measured_bytes,
                CAST(({typed_oversized}) AS INTEGER) AS value_oversized,
                CAST(({row_fits_budget}) AS INTEGER) AS row_fits_budget
         FROM messages m LEFT JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?1
         ORDER BY m.id
         LIMIT 1"
    )
}

/// Incrementally scans one Hermes `state.db`; each bounded page is admitted
/// against its session-scoped authoritative SQLite-row cursor. The caller
/// decides whether a source error is runtime noise or migration-blocking.
async fn try_ingest_state_db(
    db: &GlobalDb,
    source: &HermesProfileSource,
    project_root: &Path,
    project_id: ProjectId,
) -> Result<TranscriptIngestStats, String> {
    let mut budget = IngestByteBudget::unbounded();
    try_ingest_state_db_bounded(db, source, project_root, project_id, &mut budget).await
}

/// Opens a Hermes `state.db` read-only and derives everything a page sweep
/// needs before its first read: the physical incarnation and the column-probed
/// bounded SELECT.
async fn open_state_source(
    source: &HermesProfileSource,
) -> Result<
    (
        libsql::Connection,
        ObservationSourceGenerationV1,
        u64,
        u64,
        String,
    ),
    String,
> {
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let (generation, file_identity, resume_fingerprint) = sqlite_incarnation(state_db)?;
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await?,
        &table_columns(&conn, "sessions").await?,
    );
    Ok((
        conn,
        generation,
        file_identity,
        resume_fingerprint,
        select_sql,
    ))
}

/// Drives one single-destination bounded page sweep. Each page is truncated to
/// the shared byte budget, routed by `route_page`, and admitted against the
/// destination's authoritative SQLite-row cursor.
#[allow(clippy::too_many_arguments)]
async fn ingest_bounded_pages<F, R>(
    db: &GlobalDb,
    conn: &libsql::Connection,
    select_sql: &str,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    resume_fingerprint: u64,
    budget: &mut IngestByteBudget,
    mut route_page: F,
) -> Result<TranscriptIngestStats, String>
where
    F: FnMut(&[HermesRow]) -> R,
    R: Fn(&HermesRow) -> Option<HermesProjectionMetadata>,
{
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        let new = read_new_rows_strict(conn, select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let bounded_count = new
            .items
            .iter()
            .take_while(|row| budget.try_consume(hermes_budget_bytes(row)))
            .count();
        if bounded_count == 0 {
            return Ok(stats);
        }
        let bounded = &new.items[..bounded_count];
        let route = route_page(bounded);
        let admitted = admit_rows(
            db,
            bounded,
            scope.clone(),
            generation,
            file_identity,
            resume_fingerprint,
            route,
        )
        .await?;
        stats.messages_upserted = stats
            .messages_upserted
            .saturating_add(admitted.messages_upserted);
        stats.sessions_upserted = stats
            .sessions_upserted
            .saturating_add(admitted.sessions_upserted);
        read_cursor.position = bounded
            .last()
            .and_then(|row| u64::try_from(row.id).ok())
            .unwrap_or(read_cursor.position);
        if bounded_count < row_count {
            return Ok(stats);
        }
        if new.truncated_by_byte_budget {
            continue;
        }
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

async fn try_ingest_state_db_bounded(
    db: &GlobalDb,
    source: &HermesProfileSource,
    project_root: &Path,
    project_id: ProjectId,
    budget: &mut IngestByteBudget,
) -> Result<TranscriptIngestStats, String> {
    let (conn, generation, file_identity, resume_fingerprint, select_sql) =
        open_state_source(source).await?;
    let scope = ObservationScopeV1::Project { project_id };
    ingest_bounded_pages(
        db,
        &conn,
        &select_sql,
        scope,
        generation,
        file_identity,
        resume_fingerprint,
        budget,
        |bounded| {
            let locations = turn_project_locations(bounded, project_root, source);
            move |row: &HermesRow| {
                locations.get(&row.id).copied().map(|provenance| {
                    project_projection_metadata(row, source, project_root, provenance)
                })
            }
        },
    )
    .await
}

/// Shared-source equivalent of [`try_ingest_state_db`]. The `SQLite` page is read
/// once, then each destination independently admits routed rows against its own
/// authoritative observation cursor.
async fn try_ingest_state_db_for_projects(
    source: &HermesProfileSource,
    destinations: &[ProjectIngestDestination<'_>],
) -> Result<TranscriptIngestStats, String> {
    let (conn, generation, file_identity, resume_fingerprint, select_sql) =
        open_state_source(source).await?;
    let scopes = destinations
        .iter()
        .map(|destination| ObservationScopeV1::Project {
            project_id: destination.project_id.clone(),
        })
        .collect::<Vec<_>>();
    let destination_matchers = destinations
        .par_iter()
        .map(|destination| ProjectRootMatcher::new(destination.project_root))
        .collect::<Vec<_>>();
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        // Per-page route cache: avoid unbounded growth across many SQLite pages.
        let mut destination_routes = HashMap::<PathBuf, Vec<usize>>::new();
        let locations = turn_project_locations_for_destinations(
            &new.items,
            &destination_matchers,
            source,
            &mut destination_routes,
        );
        for (index, destination) in destinations.iter().enumerate() {
            let admitted = admit_rows(
                destination.db,
                &new.items,
                scopes[index].clone(),
                generation,
                file_identity,
                resume_fingerprint,
                |row| {
                    locations[index]
                        .by_row_id
                        .get(&row.id)
                        .copied()
                        .map(|provenance| {
                            project_projection_metadata(
                                row,
                                source,
                                destination.project_root,
                                provenance,
                            )
                        })
                },
            )
            .await?;
            stats.messages_upserted = stats
                .messages_upserted
                .saturating_add(admitted.messages_upserted);
            stats.sessions_upserted = stats
                .sessions_upserted
                .saturating_add(admitted.sessions_upserted);
        }
        read_cursor.position = new.new_cursor.position;
        if new.truncated_by_byte_budget {
            continue;
        }
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

async fn try_ingest_user_state_db_bounded(
    db: &GlobalDb,
    source: &HermesProfileSource,
    _registered_roots: &[PathBuf],
    budget: &mut IngestByteBudget,
) -> Result<TranscriptIngestStats, String> {
    let (conn, generation, file_identity, resume_fingerprint, select_sql) =
        open_state_source(source).await?;
    ingest_bounded_pages(
        db,
        &conn,
        &select_sql,
        ObservationScopeV1::Profile,
        generation,
        file_identity,
        resume_fingerprint,
        budget,
        |bounded| {
            let locations = user_turn_locations(bounded, source);
            let profile = source.profile.clone();
            let fallback_provenance = source
                .legacy_project_pin
                .as_ref()
                .map_or("session_cwd", |_| "profile_pin");
            move |row: &HermesRow| {
                locations
                    .contains(&row.id)
                    .then(|| HermesProjectionMetadata {
                        project_path: None,
                        location_path: None,
                        profile: profile.clone(),
                        location_provenance: Some(fallback_provenance),
                    })
            }
        },
    )
    .await
}

/// Opens a Hermes `state.db` strictly read-only so the sweep can never write
/// to (or create) another agent's live store.
async fn open_read_only_strict(path: &Path) -> Result<libsql::Connection, String> {
    let db = libsql::Builder::new_local(path)
        .flags(libsql::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .map_err(|error| format!("could not open '{}' read-only: {error}", path.display()))?;
    db.connect()
        .map_err(|error| format!("could not connect to '{}': {error}", path.display()))
}

async fn read_new_rows_strict(
    conn: &libsql::Connection,
    select_sql: &str,
    prev: StoredCursor,
) -> Result<HermesPageRead, String> {
    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    let mut page_bytes = 0_u64;
    let mut truncated_by_byte_budget = false;
    while items.len() < CHUNK_ROWS {
        let remaining = MAX_HERMES_PAGE_BYTES.saturating_sub(page_bytes);
        let mut rows = conn
            .query(
                select_sql,
                libsql::params![max_rowid as i64, remaining as i64],
            )
            .await
            .map_err(|error| format!("could not query legacy Hermes state rows: {error}"))?;
        let row = rows
            .next()
            .await
            .map_err(|error| format!("could not read legacy Hermes state row: {error}"))?;
        let Some(row) = row else {
            break;
        };
        let rowid = row
            .get::<i64>(0)
            .map_err(|error| format!("legacy Hermes state row has no id: {error}"))?;
        // Columns 21..23 are SQL byte/typeof/budget aggregates — integers only.
        let measured = row_i64_flag(&row, 21).max(0) as u64;
        let charge = hermes_page_row_charge(measured);
        let row_fits_budget = row_i64_flag(&row, 23) != 0;
        if !row_fits_budget && !items.is_empty() {
            // SQL returned NULL for every text payload in this row, so defer it
            // without allocating the value that would cross the page budget.
            truncated_by_byte_budget = true;
            break;
        }
        let mapped = map_row(rowid, &row, measured)
            .ok_or_else(|| format!("legacy Hermes state row {rowid} is malformed"))?;
        page_bytes = page_bytes.saturating_add(charge);
        max_rowid = max_rowid.max(rowid as u64);
        items.push(mapped);
        if page_bytes >= MAX_HERMES_PAGE_BYTES {
            truncated_by_byte_budget = true;
            break;
        }
    }
    if items.len() >= CHUNK_ROWS {
        truncated_by_byte_budget = true;
    }
    Ok(HermesPageRead {
        items,
        new_cursor: StoredCursor {
            position: max_rowid,
            mtime: 0,
            file_id: 0,
        },
        truncated_by_byte_budget,
    })
}

fn row_i64_flag(row: &libsql::Row, idx: i32) -> i64 {
    row.get::<i64>(idx)
        .or_else(|_| row.get::<Option<i64>>(idx).map(|value| value.unwrap_or(0)))
        .or_else(|_| row.get::<f64>(idx).map(|value| value as i64))
        .unwrap_or(0)
}

fn row_optional_f64(row: &libsql::Row, idx: i32) -> Option<f64> {
    row.get::<Option<f64>>(idx).ok().flatten().or_else(|| {
        row.get::<Option<i64>>(idx)
            .ok()
            .flatten()
            .map(|value| value as f64)
    })
}

fn map_row(rowid: i64, row: &libsql::Row, sql_measured_bytes: u64) -> Option<HermesRow> {
    let sql_value_oversized = row_i64_flag(row, 22) != 0;
    let session_id = match row.get::<Option<String>>(1).ok().flatten() {
        Some(id) if !id.is_empty() => id,
        // Rejected/oversized session_id never materializes the hostile value; use a
        // deterministic cover identity so the row can advance without payload leakage.
        _ if sql_value_oversized => format!("hermes.oversized.{rowid}"),
        _ => return None,
    };
    Some(HermesRow {
        id: rowid,
        session_id,
        role: row
            .get::<Option<String>>(2)
            .ok()
            .flatten()
            .unwrap_or_default(),
        content: row.get::<Option<String>>(3).ok().flatten(),
        reasoning: row.get::<Option<String>>(4).ok().flatten(),
        tool_name: row.get::<Option<String>>(5).ok().flatten(),
        tool_calls: row.get::<Option<String>>(6).ok().flatten(),
        timestamp: row_optional_f64(row, 7),
        session_model: row.get::<Option<String>>(8).ok().flatten(),
        parent_session_id: row.get::<Option<String>>(9).ok().flatten(),
        session_cwd: row.get::<Option<String>>(10).ok().flatten(),
        session_source: row.get::<Option<String>>(11).ok().flatten(),
        session_title: row.get::<Option<String>>(12).ok().flatten(),
        session_started_at: row_optional_f64(row, 13),
        session_ended_at: row_optional_f64(row, 14),
        session_input_tokens: row.get::<Option<i64>>(15).ok().flatten(),
        session_output_tokens: row.get::<Option<i64>>(16).ok().flatten(),
        session_cache_read_tokens: row.get::<Option<i64>>(17).ok().flatten(),
        session_cache_write_tokens: row.get::<Option<i64>>(18).ok().flatten(),
        session_reasoning_tokens: row.get::<Option<i64>>(19).ok().flatten(),
        active: row.get::<Option<i64>>(20).ok().flatten().unwrap_or(1),
        sql_value_oversized,
        sql_measured_bytes,
    })
}

fn user_turn_locations(rows: &[HermesRow], source: &HermesProfileSource) -> HashSet<i64> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashSet::new();
    for session_rows in by_session.into_values() {
        let has_fallback = source.legacy_project_pin.is_some()
            || session_rows
                .iter()
                .any(|row| Path::new(row.session_cwd.as_deref().unwrap_or_default()).is_absolute())
            || source.state_db.parent().is_some();
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_user_turn(&turn, has_fallback, &mut locations);
                turn.clear();
            }
            turn.push(row);
        }
        assign_user_turn(&turn, has_fallback, &mut locations);
    }
    locations
}

fn assign_user_turn(rows: &[&HermesRow], has_fallback: bool, locations: &mut HashSet<i64>) {
    if rows
        .iter()
        .flat_map(|row| structured_tool_project_paths(row))
        .next_back()
        .is_none()
        && !has_fallback
    {
        return;
    }
    locations.extend(rows.iter().map(|row| row.id));
}

fn turn_project_locations(
    rows: &[HermesRow],
    project_root: &Path,
    source: &HermesProfileSource,
) -> HashMap<i64, &'static str> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashMap::new();
    for session_rows in by_session.into_values() {
        let has_fallback = session_rows
            .iter()
            .any(|row| session_is_candidate_for_project(row, project_root, source));
        let fallback_provenance = source
            .legacy_project_pin
            .as_ref()
            .map_or("session_cwd", |_| "profile_pin");
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_location(
                    &turn,
                    project_root,
                    has_fallback,
                    fallback_provenance,
                    &mut locations,
                );
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_location(
            &turn,
            project_root,
            has_fallback,
            fallback_provenance,
            &mut locations,
        );
    }
    locations
}

struct DestinationTurnLocations {
    by_row_id: HashMap<i64, &'static str>,
}

fn turn_project_locations_for_destinations(
    rows: &[HermesRow],
    destination_matchers: &[ProjectRootMatcher],
    source: &HermesProfileSource,
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Vec<DestinationTurnLocations> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = (0..destination_matchers.len())
        .map(|_| DestinationTurnLocations {
            by_row_id: HashMap::new(),
        })
        .collect::<Vec<_>>();
    for session_rows in by_session.into_values() {
        let fallback_provenance = source
            .legacy_project_pin
            .as_ref()
            .map_or("session_cwd", |_| "profile_pin");
        let fallback_candidates = if let Some(pin) = source.legacy_project_pin.as_ref() {
            vec![pin.clone()]
        } else {
            let mut seen = BTreeSet::new();
            session_rows
                .iter()
                .filter_map(|row| {
                    let cwd = PathBuf::from(row.session_cwd.as_deref()?.trim());
                    (cwd.is_absolute() && seen.insert(cwd.clone())).then_some(cwd)
                })
                .collect::<Vec<_>>()
        };
        let mut fallbacks = vec![false; destination_matchers.len()];
        for cwd in fallback_candidates {
            for destination_index in
                matching_destinations(&cwd, destination_matchers, destination_routes)
            {
                fallbacks[destination_index] = true;
            }
        }
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_locations_for_destinations(
                    &turn,
                    destination_matchers,
                    &fallbacks,
                    fallback_provenance,
                    &mut locations,
                    destination_routes,
                );
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_locations_for_destinations(
            &turn,
            destination_matchers,
            &fallbacks,
            fallback_provenance,
            &mut locations,
            destination_routes,
        );
    }
    locations
}

fn assign_turn_locations_for_destinations(
    rows: &[&HermesRow],
    destination_matchers: &[ProjectRootMatcher],
    fallbacks: &[bool],
    fallback_provenance: &'static str,
    locations: &mut [DestinationTurnLocations],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let mut selected = vec![false; destination_matchers.len()];
    let has_explicit_paths = !explicit_paths.is_empty();
    if has_explicit_paths {
        for path in explicit_paths {
            for destination_index in
                matching_destinations(&path, destination_matchers, destination_routes)
            {
                selected[destination_index] = true;
            }
        }
    } else {
        selected.copy_from_slice(fallbacks);
    }
    let provenance = if has_explicit_paths {
        "tool_project_path"
    } else {
        fallback_provenance
    };
    for (selected, destination) in selected.into_iter().zip(locations) {
        if selected {
            destination
                .by_row_id
                .extend(rows.iter().map(|row| (row.id, provenance)));
        }
    }
}

fn matching_destinations(
    path: &Path,
    destination_matchers: &[ProjectRootMatcher],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) -> Vec<usize> {
    if let Some(indices) = destination_routes.get(path) {
        return indices.clone();
    }
    let indices = destination_matchers
        .iter()
        .enumerate()
        .filter_map(|(index, matcher)| matcher.contains(path).then_some(index))
        .collect::<Vec<_>>();
    destination_routes.insert(path.to_path_buf(), indices.clone());
    indices
}

fn assign_turn_location(
    rows: &[&HermesRow],
    project_root: &Path,
    has_fallback: bool,
    fallback_provenance: &'static str,
    locations: &mut HashMap<i64, &'static str>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let explicit = !explicit_paths.is_empty()
        && explicit_paths
            .iter()
            .any(|path| path_belongs_to_project(path, project_root));
    if explicit || (explicit_paths.is_empty() && has_fallback) {
        let provenance = if explicit {
            "tool_project_path"
        } else {
            fallback_provenance
        };
        locations.extend(rows.iter().map(|row| (row.id, provenance)));
    }
}

fn structured_tool_project_paths(row: &HermesRow) -> Vec<PathBuf> {
    let Some(raw) = row.tool_calls.as_deref() else {
        return Vec::new();
    };
    let Ok(calls) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    let calls = calls.as_array().map_or(&[] as &[Value], Vec::as_slice);
    for call in calls {
        let arguments = call
            .pointer("/function/arguments")
            .or_else(|| call.get("arguments"));
        let parsed;
        let arguments = match arguments {
            Some(Value::String(raw)) => {
                parsed = serde_json::from_str::<Value>(raw).unwrap_or(Value::Null);
                &parsed
            }
            Some(value) => value,
            None => continue,
        };
        for value in [
            arguments.get("project_root"),
            arguments.get("project_path"),
            arguments.pointer("/project_selector/path"),
            arguments.get("cwd"),
            arguments.get("workdir"),
        ]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                paths.push(path);
            }
        }
    }
    paths
}

fn session_is_candidate_for_project(
    row: &HermesRow,
    project_root: &Path,
    source: &HermesProfileSource,
) -> bool {
    source.legacy_project_pin.is_some()
        || row.session_cwd.as_deref().is_some_and(|cwd| {
            let cwd = Path::new(cwd.trim());
            cwd.is_absolute() && path_belongs_to_project(cwd, project_root)
        })
}

#[cfg(unix)]
fn file_mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod observation_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_sqlite_incarnation_keeps_identity_and_refreshes_resume_fingerprint() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        std::fs::write(&path, b"before").unwrap();
        let (before_generation, before_identity, before_resume) =
            sqlite_incarnation(&path).expect("initial Windows SQLite identity");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"after")
            .unwrap();

        let (after_generation, after_identity, after_resume) =
            sqlite_incarnation(&path).expect("updated Windows SQLite identity");
        assert_eq!(after_generation, before_generation);
        assert_eq!(after_identity, before_identity);
        assert_ne!(after_resume, before_resume);
    }

    fn fixture(row_id: i64) -> HermesRow {
        HermesRow {
            id: row_id,
            session_id: "session-redacted".to_string(),
            role: "assistant".to_string(),
            content: Some("safe fixture content".to_string()),
            reasoning: Some("safe redacted reasoning".to_string()),
            tool_name: Some("terminal".to_string()),
            tool_calls: Some(
                json!([{
                    "id": "tool-redacted",
                    "function": {"name": "terminal", "arguments": "{}"}
                }])
                .to_string(),
            ),
            timestamp: Some(1_750_000_000.0),
            session_model: Some("model-redacted".to_string()),
            parent_session_id: Some("parent-redacted".to_string()),
            session_cwd: None,
            session_source: Some("tui".to_string()),
            session_title: Some("Safe fixture".to_string()),
            session_started_at: Some(1_750_000_000.0),
            session_ended_at: Some(1_750_000_001.0),
            session_input_tokens: Some(10),
            session_output_tokens: Some(5),
            session_cache_read_tokens: Some(4),
            session_cache_write_tokens: Some(3),
            session_reasoning_tokens: Some(2),
            active: 1,
            sql_value_oversized: false,
            sql_measured_bytes: 0,
        }
    }

    fn normalized(row: &HermesRow, start: u64) -> HermesObservationRecord {
        let source = observation_source(row).unwrap();
        let range = ObservationSourceRangeV1::new(start, row.id as u64).unwrap();
        native_observation_record(row, &fixture_projection(), source, range).unwrap()
    }

    fn fixture_projection() -> HermesProjectionMetadata {
        HermesProjectionMetadata {
            project_path: None,
            location_path: None,
            profile: Some("fixture".to_string()),
            location_provenance: None,
        }
    }

    fn canonical(row: &HermesRow, start: u64) -> CanonicalObservationEnvelopeV1 {
        let record = normalized(row, start);
        let encoded = serde_json::to_vec(&record.native).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &encoded,
            record.range,
            ObservationOrderingDomainV1::SqliteRowId,
            |native| normalize_native_observation(native, record.range),
        )
        .unwrap();
        serde_json::from_value(parsed.value().clone()).unwrap()
    }

    #[test]
    fn native_identity_does_not_depend_on_sqlite_row_id() {
        let first = normalized(&fixture(7), 0);
        let relocated = normalized(&fixture(700), 0);
        assert_eq!(first.native_record_id, relocated.native_record_id);
        assert_ne!(first.range, relocated.range);
    }

    #[test]
    fn native_identity_does_not_depend_on_routing_path() {
        let mut first_row = fixture(7);
        first_row.session_cwd = Some("/redacted/first".to_string());
        let mut relocated_row = fixture(7);
        relocated_row.session_cwd = Some("/redacted/second".to_string());
        let first = normalized(&first_row, 0);
        let relocated = normalized(&relocated_row, 0);
        assert_eq!(first.native_record_id, relocated.native_record_id);
        assert_eq!(first.native, relocated.native);
    }

    #[test]
    fn normalized_payload_contains_only_typed_canonical_facts() {
        let envelope = canonical(&fixture(7), 0);
        assert_eq!(envelope.provider().as_str(), PROVIDER);
        assert_eq!(envelope.native_record_kind(), "message");
        assert_eq!(
            envelope.relations().session_id().as_str(),
            "session-redacted"
        );
        assert_eq!(
            envelope.relations().message_id().map(ObservationId::as_str),
            Some("session-redacted:7")
        );
        assert!(envelope.relations().agent_id().is_some());
        assert!(envelope.relations().parent_agent_id().is_some());
        assert_eq!(
            envelope
                .relations()
                .parent_session_id()
                .map(SessionId::as_str),
            Some("parent-redacted")
        );
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        // Hermes has no native thread/turn identifiers; leave those unset.
        assert!(relations.get("thread_id").is_none());
        assert!(relations.get("turn_id").is_none());
        assert_eq!(
            envelope.evidence().ordering_domain(),
            ObservationOrderingDomainV1::SqliteRowId
        );
        assert_eq!(
            envelope.evidence().range(),
            ObservationSourceRangeV1::new(0, 7).unwrap()
        );
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Session {
                project_path: None,
                location_path: None,
                transcript_path: None,
                title: Some(title),
                started_at: Some(1_750_000_000),
                ended_at: Some(1_750_000_001),
                source: Some(source),
                native_source: Some(native_source),
                profile: Some(profile),
                location_provenance: None,
            } if title == "Safe fixture"
                && source == "hermes_state_db"
                && native_source == "tui"
                && profile == "fixture"
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: Value::String(content),
                model: Some(model),
                timestamp: Some(1_750_000_000),
            } if content == "safe fixture content" && model == "model-redacted"
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolInvocation {
                name,
                arguments,
                ..
            } if name == "terminal" && arguments == &json!({})
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cache_read_tokens: Some(4),
                cache_write_tokens: Some(3),
                reasoning_tokens: Some(2),
            }
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Visible,
                content: Some(Value::String(content)),
            } if content == "safe redacted reasoning"
        )));

        let canonical = serde_json::to_value(&envelope).unwrap();
        for forbidden in ["hermes", "routing", "cwd", "provenance", "metadata"] {
            assert!(canonical.get(forbidden).is_none());
        }
    }

    #[test]
    fn sanitizer_preserves_non_sensitive_v1_message_identity() {
        let mut row = fixture(7);
        row.session_id = "20260101_000000_abc123".to_string();
        let record = normalized(&row, 0);
        let encoded = serde_json::to_vec(&record.native).unwrap();
        let parsed = parse_normalized_observation_record_v1(
            &encoded,
            record.range,
            ObservationOrderingDomainV1::SqliteRowId,
            |native| normalize_native_observation(native, record.range),
        )
        .unwrap();
        let identity = ObservationIdentityMaterialV1::for_native_record(
            record.source,
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            record.range,
            ObservationOrderingDomainV1::SqliteRowId,
            record.native_record_id,
        )
        .unwrap();
        let outcome = crate::privacy::ClaudeRecordSanitizerV1::observation_v1()
            .unwrap()
            .sanitize_parsed(
                parsed,
                identity,
                RetentionClass::new(OBSERVATION_RETENTION).unwrap(),
            )
            .unwrap();
        let crate::privacy::ObservationSanitizationOutcomeV1::Durable { observation, .. } = outcome
        else {
            panic!("safe Hermes fixture must remain durable");
        };
        let envelope: CanonicalObservationEnvelopeV1 =
            serde_json::from_value(observation.payload().clone()).unwrap();
        assert_eq!(
            envelope.relations().message_id().map(ObservationId::as_str),
            Some("20260101_000000_abc123:7")
        );
    }

    #[tokio::test]
    async fn projection_drain_projects_v1_message_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = GlobalDb::open_at(&tmp.path().join("sessions.db"))
            .await
            .unwrap();
        let row = fixture(7);
        let stats = admit_rows(
            &db,
            std::slice::from_ref(&row),
            ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(1).unwrap(),
            1,
            1,
            |_| Some(fixture_projection()),
        )
        .await
        .unwrap();
        assert_eq!(stats.messages_upserted, 1);
        drain_hermes_projections(&db, &ObservationScopeV1::Profile)
            .await
            .unwrap();
        assert!(
            db.get_session_message(PROVIDER, "session-redacted:7")
                .await
                .is_some()
        );
    }

    #[test]
    fn canonical_identity_and_parent_relation_match_native_evidence() {
        let row = fixture(7);
        let record = normalized(&row, 0);
        let expected_record_id = record.native_record_id.clone();
        let expected_parent =
            stable_native_id("hermes.session", &json!("parent-redacted")).unwrap();
        let envelope = normalize_native_observation(record.native, record.range).unwrap();
        assert_eq!(envelope.stable_record_id(), &expected_record_id);
        assert_eq!(
            envelope.relations().parent_agent_id(),
            Some(&expected_parent)
        );
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert!(relations.get("thread_id").is_none());
        assert!(relations.get("turn_id").is_none());
    }

    #[test]
    fn assistant_without_reasoning_is_typed_unavailable() {
        let mut row = fixture(7);
        row.reasoning = None;
        let envelope = canonical(&row, 0);
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Unavailable,
                content: None,
            }
        )));
    }

    #[test]
    fn empty_assistant_content_keeps_typed_tool_or_reasoning_without_message() {
        let mut tool_row = fixture(7);
        tool_row.content = Some(String::new());
        tool_row.reasoning = None;
        let tool = canonical(&tool_row, 0);
        assert!(
            tool.facts()
                .iter()
                .all(|fact| { !matches!(fact, CanonicalObservationFactV1::Message { .. }) })
        );
        assert!(tool.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolInvocation {
                name,
                ..
            } if name == "terminal"
        )));

        let mut reasoning_row = fixture(8);
        reasoning_row.content = Some(String::new());
        reasoning_row.tool_calls = None;
        let reasoning = canonical(&reasoning_row, 0);
        assert!(
            reasoning
                .facts()
                .iter()
                .all(|fact| { !matches!(fact, CanonicalObservationFactV1::Message { .. }) })
        );
        assert!(reasoning.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::Visible,
                content: Some(Value::String(content)),
            } if content == "safe redacted reasoning"
        )));
    }

    #[test]
    fn tool_message_preserves_authored_message_and_typed_result() {
        let mut row = fixture(7);
        row.role = "tool".to_string();
        row.reasoning = None;
        row.tool_calls = None;
        let envelope = canonical(&row, 0);
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolResult {
                invocation_id: None,
                content: Value::String(content),
                success: None,
            } if content == "safe fixture content"
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Tool,
                content: Value::String(content),
                model: Some(model),
                timestamp: Some(1_750_000_000),
            } if content == "safe fixture content" && model == "model-redacted"
        )));
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Reasoning {
                visibility: CanonicalReasoningVisibilityV1::NotApplicable,
                content: None,
            }
        )));
    }

    #[test]
    fn same_generation_resumes_sqlite_ordering() {
        let row = fixture(42);
        let source = observation_source(&row).unwrap();
        let generation = ObservationSourceGenerationV1::new(17).unwrap();
        let expected = ObservationSourceCursorV1::for_ordering(
            source,
            ObservationScopeV1::Profile,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            20,
        )
        .unwrap();
        let admission = prepare_observation_row(
            &row,
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            generation,
            Some(expected),
            23,
            29,
        )
        .unwrap();
        assert_eq!(admission.range.start(), 20);
        assert_eq!(admission.range.end(), 42);
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Capture(_)
        ));
    }

    #[test]
    fn replacement_generation_restarts_sqlite_ordering() {
        let row = fixture(42);
        let source = observation_source(&row).unwrap();
        let old_generation = ObservationSourceGenerationV1::new(17).unwrap();
        let expected = ObservationSourceCursorV1::for_ordering(
            source,
            ObservationScopeV1::Profile,
            old_generation,
            ObservationOrderingDomainV1::SqliteRowId,
            900,
        )
        .unwrap();
        let admission = prepare_observation_row(
            &row,
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(18).unwrap(),
            Some(expected),
            23,
            29,
        )
        .unwrap();
        assert_eq!(admission.range.start(), 0);
        assert_eq!(admission.range.end(), 42);
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Capture(_)
        ));
    }

    #[test]
    fn malformed_tool_calls_are_complete_typed_coverage() {
        let mut row = fixture(7);
        row.tool_calls = Some("{not-json".to_string());
        let admission = prepare_observation_row(
            &row,
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(17).unwrap(),
            None,
            23,
            29,
        )
        .unwrap();
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Cover(ObservationCoverageReason::MalformedFrame)
        ));
    }

    #[test]
    fn missing_route_is_complete_out_of_scope_coverage() {
        let admission = prepare_observation_row(
            &fixture(7),
            None,
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(17).unwrap(),
            None,
            23,
            29,
        )
        .unwrap();
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Cover(ObservationCoverageReason::OutOfScope)
        ));
    }

    #[test]
    fn oversized_record_is_complete_typed_coverage() {
        let mut row = fixture(7);
        row.content = Some("x".repeat(MAX_OBSERVATION_RECORD_BYTES));
        let admission = prepare_observation_row(
            &row,
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(17).unwrap(),
            None,
            23,
            29,
        )
        .unwrap();
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
        ));
    }

    #[test]
    fn sql_preflight_oversized_is_typed_coverage_without_payload() {
        let mut row = fixture(7);
        row.content = None;
        row.sql_value_oversized = true;
        row.sql_measured_bytes = (MAX_HERMES_VALUE_BYTES as u64).saturating_add(1);
        let admission = prepare_observation_row(
            &row,
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(17).unwrap(),
            None,
            23,
            29,
        )
        .unwrap();
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
        ));
    }

    #[test]
    fn excessive_structure_is_complete_malformed_coverage() {
        let mut nested = Value::String("redacted".to_string());
        for _ in 0..128 {
            nested = json!({ "nested": nested });
        }
        let mut row = fixture(7);
        row.tool_calls = Some(nested.to_string());
        let admission = prepare_observation_row(
            &row,
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(17).unwrap(),
            None,
            23,
            29,
        )
        .unwrap();
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Cover(ObservationCoverageReason::MalformedFrame)
        ));
    }

    #[test]
    fn fixture_backed_hermes_tool_call_reaches_canonical_envelope() {
        // Exact assistant tool-call shape from
        // tests/transcript_ingest_suite/hermes.rs::write_hermes_profile.
        // Provider-parser path: native_observation_record → normalize_native_observation.
        let input: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/hermes/assistant_tool_call.input.json"
        ))
        .expect("Hermes golden input");
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/hermes/assistant_tool_call.expected_envelope.json"
        ))
        .expect("Hermes golden expected envelope");
        let tool_calls = input["tool_calls"].clone();
        let mut row = fixture(input["row_id"].as_i64().unwrap());
        row.session_id = input["session_id"].as_str().unwrap().to_string();
        row.role = input["role"].as_str().unwrap().to_string();
        row.content = input["content"].as_str().map(str::to_string);
        row.reasoning = None;
        row.tool_name = None;
        row.tool_calls = Some(tool_calls.to_string());
        row.timestamp = input["timestamp"].as_f64();
        row.session_model = input["session_model"].as_str().map(str::to_string);
        row.parent_session_id = None;
        row.session_input_tokens = input["session_input_tokens"].as_i64();
        row.session_output_tokens = input["session_output_tokens"].as_i64();
        row.session_cache_read_tokens = input["session_cache_read_tokens"].as_i64();
        row.session_cache_write_tokens = input["session_cache_write_tokens"].as_i64();
        row.session_reasoning_tokens = input["session_reasoning_tokens"].as_i64();

        let record = normalized(&row, 0);
        let native: Value = serde_json::from_value(record.native.clone()).unwrap();
        assert_eq!(native["role"], "assistant");
        assert_eq!(native["tool_calls"], tool_calls);
        assert!(native.get("cwd").is_none());
        assert!(native.get("routing").is_none());

        let envelope = canonical(&row, 0);
        let actual = serde_json::to_value(&envelope).unwrap();
        assert_eq!(actual["version"], expected["version"]);
        assert_eq!(actual["provider"], expected["provider"]);
        assert_eq!(actual["native_record_kind"], expected["native_record_kind"]);
        assert_eq!(actual["evidence"], expected["evidence"]);
        let fact_kinds = actual["facts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|fact| fact["kind"] != "session")
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
        assert_eq!(
            relations.get("agent_id").is_some(),
            expected["relations"]["agent_id_present"].as_bool().unwrap()
        );
        for absent in expected["relations"]["absent"].as_array().unwrap() {
            assert!(relations.get(absent.as_str().unwrap()).is_none());
        }
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::ToolInvocation {
                name,
                arguments,
                ..
            } if name == "terminal"
                && arguments.get("command").and_then(Value::as_str)
                    == Some("cargo test billing")
        )));
        assert!(
            envelope
                .facts()
                .iter()
                .all(|fact| !matches!(fact, CanonicalObservationFactV1::Message { .. })),
            "empty-content Hermes tool-call turn must not synthesize a Message fact"
        );
        let encoded = actual.to_string();
        for required in expected["encoded_must_contain"].as_array().unwrap() {
            assert!(encoded.contains(required.as_str().unwrap()));
        }
        for rejected in expected["encoded_must_not_contain"].as_array().unwrap() {
            assert!(!encoded.contains(rejected.as_str().unwrap()));
        }
        assert!(
            envelope.facts().iter().all(|fact| {
                !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. })
            }),
            "Hermes checked-in fixture must not emit WorkflowLifecycle"
        );
    }

    #[test]
    fn hermes_workflow_lookalike_fields_do_not_emit_workflow_lifecycle() {
        let input: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/provider_normalization/hermes/workflow_lookalike.input.json"
        ))
        .expect("Hermes workflow lookalike input");
        let mut row = fixture(input["row_id"].as_i64().unwrap());
        row.session_id = input["session_id"].as_str().unwrap().to_string();
        row.role = input["role"].as_str().unwrap().to_string();
        row.content = input["content"].as_str().map(str::to_string);
        row.reasoning = None;
        row.tool_name = None;
        row.tool_calls = Some(input["tool_calls"].to_string());
        row.timestamp = input["timestamp"].as_f64();
        row.session_model = input["session_model"].as_str().map(str::to_string);
        row.parent_session_id = None;
        row.session_input_tokens = input["session_input_tokens"].as_i64();
        row.session_output_tokens = input["session_output_tokens"].as_i64();
        row.session_cache_read_tokens = input["session_cache_read_tokens"].as_i64();
        row.session_cache_write_tokens = input["session_cache_write_tokens"].as_i64();
        row.session_reasoning_tokens = input["session_reasoning_tokens"].as_i64();

        let record = normalized(&row, 0);
        let mut native = record.native.clone();
        if let Some(object) = native.as_object_mut() {
            for key in ["workflow", "todos", "thread_goal_updated"] {
                if let Some(value) = input.get(key) {
                    object.insert(key.to_string(), value.clone());
                }
            }
        }
        let envelope = normalize_native_observation(native, record.range)
            .expect("Hermes must ignore unknown workflow lookalike bags");
        assert!(
            envelope.facts().iter().all(|fact| {
                !matches!(fact, CanonicalObservationFactV1::WorkflowLifecycle { .. })
            }),
            "Hermes workflow lookalikes must not become WorkflowLifecycle"
        );
        let encoded = serde_json::to_string(&envelope).unwrap();
        for rejected in [
            "hermes-hostile-task",
            "todo-hostile-1",
            "invented todo",
            "invented goal",
        ] {
            assert!(
                !encoded.contains(rejected),
                "{rejected} must not survive Hermes normalization"
            );
        }
    }

    /// Hostile `zeroblob` content is rejected by SQL length/typeof before any
    /// Rust String/Vec materialization of the payload. Mirrors production by
    /// writing then reopening read-only.
    #[tokio::test]
    async fn zeroblob_content_is_covered_without_materializing_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let db = libsql::Builder::new_local(&path).build().await.unwrap();
            let conn = db.connect().unwrap();
            conn.execute(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    model TEXT,
                    parent_session_id TEXT,
                    cwd TEXT,
                    input_tokens INTEGER,
                    output_tokens INTEGER,
                    cache_read_tokens INTEGER,
                    cache_write_tokens INTEGER,
                    reasoning_tokens INTEGER
                )",
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "CREATE TABLE messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT,
                    tool_name TEXT,
                    tool_calls TEXT,
                    timestamp REAL NOT NULL,
                    reasoning TEXT,
                    active INTEGER NOT NULL DEFAULT 1
                )",
                (),
            )
            .await
            .unwrap();
            // Generate the hostile value inside SQLite — never as a Rust String/Vec.
            let hostile_bytes = MAX_HERMES_VALUE_BYTES.saturating_add(1);
            conn.execute(
                &format!(
                    "INSERT INTO sessions (id, model, input_tokens)
                     VALUES ('sess-zeroblob', 'model', zeroblob({hostile_bytes}))"
                ),
                (),
            )
            .await
            .unwrap();
            conn.execute(
                &format!(
                    "INSERT INTO messages (session_id, role, content, timestamp)
                     VALUES ('sess-zeroblob', 'user', zeroblob({hostile_bytes}), 1.0)"
                ),
                (),
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES ('sess-zeroblob', 'assistant', 'safe trailing row', 2.0)",
                (),
            )
            .await
            .unwrap();
        }

        let conn = open_read_only_strict(&path).await.unwrap();
        let message_cols = message_columns(&conn).await.unwrap();
        let session_cols = table_columns(&conn, "sessions").await.unwrap();
        let select_sql = select_new_messages_sql(&message_cols, &session_cols);
        let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(page.items[0].sql_value_oversized);
        assert!(page.items[0].content.is_none());
        assert!(
            page.items[0].session_input_tokens.is_none(),
            "dynamic BLOB in INTEGER column must be nulled in SQL"
        );
        assert!(
            page.items[0].sql_measured_bytes > MAX_HERMES_VALUE_BYTES as u64,
            "SQL length charge must reflect hostile size without materializing it"
        );
        assert!(!page.items[1].sql_value_oversized);
        assert_eq!(page.items[1].content.as_deref(), Some("safe trailing row"));

        let admission = prepare_observation_row(
            &page.items[0],
            Some(&fixture_projection()),
            &ObservationScopeV1::Profile,
            ObservationSourceGenerationV1::new(17).unwrap(),
            None,
            23,
            29,
        )
        .unwrap();
        assert!(matches!(
            admission.action,
            HermesAdmissionAction::Cover(ObservationCoverageReason::OversizedFrame)
        ));
    }

    #[tokio::test]
    async fn page_byte_budget_stops_collection_before_unbounded_growth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let db = libsql::Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                model TEXT,
                parent_session_id TEXT,
                cwd TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cache_read_tokens INTEGER,
                cache_write_tokens INTEGER,
                reasoning_tokens INTEGER
            )",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_name TEXT,
                tool_calls TEXT,
                timestamp REAL NOT NULL,
                reasoning TEXT,
                active INTEGER NOT NULL DEFAULT 1
            )",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, model) VALUES ('sess-page', 'model')",
            (),
        )
        .await
        .unwrap();
        // Build three max-sized TEXT payloads inside SQLite. The product path
        // must gate the second row against the remaining page bytes before
        // libsql materializes it as a Rust String.
        let sqlite_blob_bytes = MAX_HERMES_VALUE_BYTES / 2;
        for index in 0..3 {
            conn.execute(
                &format!(
                    "INSERT INTO messages (session_id, role, content, timestamp)
                     SELECT 'sess-page', 'user', hex(zeroblob({sqlite_blob_bytes})), ?1"
                ),
                libsql::params![f64::from(index)],
            )
            .await
            .unwrap();
        }

        let message_cols = message_columns(&conn).await.unwrap();
        let session_cols = table_columns(&conn, "sessions").await.unwrap();
        let select_sql = select_new_messages_sql(&message_cols, &session_cols);
        let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
            .await
            .unwrap();
        assert_eq!(
            page.items.len(),
            1,
            "the second max-sized row must be deferred before String materialization"
        );
        assert!(page.truncated_by_byte_budget);
        assert!(page.items.iter().all(|row| {
            row.content
                .as_ref()
                .is_none_or(|c| c.len() <= MAX_HERMES_VALUE_BYTES)
        }));

        let next = read_new_rows_strict(&conn, &select_sql, page.new_cursor)
            .await
            .unwrap();
        assert_eq!(next.items.len(), 1, "deferred row must resume next page");
    }

    #[tokio::test]
    async fn utf8_byte_gate_rejects_multibyte_text_before_materialization() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let db = libsql::Builder::new_local(&path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY, model TEXT, parent_session_id TEXT, cwd TEXT,
                input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER,
                cache_write_tokens INTEGER, reasoning_tokens INTEGER
            )",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                role TEXT NOT NULL, content TEXT, tool_name TEXT, tool_calls TEXT,
                timestamp REAL NOT NULL, reasoning TEXT, active INTEGER NOT NULL DEFAULT 1
            )",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, model) VALUES ('sess-utf8', 'model')",
            (),
        )
        .await
        .unwrap();
        // 600,000 `é` code points are 1,200,000 UTF-8 bytes. SQLite
        // length(TEXT) would undercount this below the 1 MiB byte ceiling.
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             SELECT 'sess-utf8', 'user',
                    replace(hex(zeroblob(600000)), '00', 'é'), 1.0",
            (),
        )
        .await
        .unwrap();

        let message_cols = message_columns(&conn).await.unwrap();
        let session_cols = table_columns(&conn, "sessions").await.unwrap();
        let select_sql = select_new_messages_sql(&message_cols, &session_cols);
        let page = read_new_rows_strict(&conn, &select_sql, StoredCursor::default())
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.items[0].sql_value_oversized);
        assert!(page.items[0].content.is_none());
        assert!(
            page.items[0].sql_measured_bytes > MAX_HERMES_VALUE_BYTES as u64,
            "UTF-8 byte length must drive the typed oversized outcome"
        );
    }
}
