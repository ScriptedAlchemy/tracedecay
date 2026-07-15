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
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionStatus,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::global_db::GlobalDb;
use crate::privacy::{
    MAX_OBSERVATION_RECORD_BYTES, ObservationRecordParseErrorV1,
    parse_normalized_observation_record_v1,
};
use crate::sessions::shared::{
    NewRows, ProjectRootMatcher, StoredCursor, TranscriptIngestStats, path_belongs_to_project,
};
const PROVIDER: &str = "hermes";
const OBSERVATION_RETENTION: &str = "transcript.hermes.v1";
const CHUNK_ROWS: usize = 2000;

/// Ingests Hermes sessions proven to belong to `project_root` into `db`.
///
/// Discovery is bounded to the default user integration (`~/.hermes`) and its
/// immediate named-profile children; environment overrides are ignored.
pub async fn ingest_for_project(db: &GlobalDb, project_root: &Path) -> TranscriptIngestStats {
    let homes = crate::sessions::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default();
    ingest_homes(db, &homes, project_root).await
}

/// One project-store destination for a shared Hermes source sweep.
#[derive(Clone, Copy)]
pub struct ProjectIngestDestination<'a> {
    pub db: &'a GlobalDb,
    pub project_root: &'a Path,
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
            .copied()
            .filter(|destination| {
                source_is_candidate_for_project(&source, destination.project_root)
            })
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
    stats
}

/// [`ingest_for_project`] with explicit Hermes home directories — the test
/// seam for pointing the sweep at a temporary home instead of the real
/// `~/.hermes`.
pub async fn ingest_homes(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    project_root: &Path,
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for source in candidate_state_dbs(hermes_homes, project_root) {
        match try_ingest_state_db(db, &source, project_root).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping Hermes transcript source"
            ),
        }
    }
    stats
}

/// Ingests canonical historical Hermes observations into the profile scope.
/// Project ingestion separately admits each turn to every registered project it
/// touched using the same stable message IDs.
pub async fn ingest_user_sessions(
    db: &GlobalDb,
    registered_roots: &[PathBuf],
) -> TranscriptIngestStats {
    let homes = crate::sessions::home_dir()
        .map(|home| vec![home.join(".hermes")])
        .unwrap_or_default();
    ingest_user_homes(db, &homes, registered_roots).await
}

pub async fn ingest_user_homes(
    db: &GlobalDb,
    hermes_homes: &[PathBuf],
    registered_roots: &[PathBuf],
) -> TranscriptIngestStats {
    let mut stats = TranscriptIngestStats::default();
    for source in all_profile_sources(hermes_homes) {
        match try_ingest_user_state_db(db, &source, registered_roots).await {
            Ok(source_stats) => stats = stats.merge(source_stats),
            Err(error) => tracing::debug!(
                state_db = %source.state_db.display(),
                error,
                "skipping projectless Hermes transcript source"
            ),
        }
    }
    stats
}

/// Strict one-time import for a legacy profile whose project pin was already
/// resolved by the migration layer. Unlike the normal catch-up sweep, any
/// open/query/write failure is returned so callers retain the pin and source.
pub(crate) async fn ingest_legacy_pinned_profile(
    db: &GlobalDb,
    profile_dir: &Path,
    project_root: &Path,
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
    };
    try_ingest_state_db(db, &source, project_root).await
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
        for (profile_dir, _) in profiles {
            let state_db = profile_dir.join("state.db");
            if state_db.is_file() && seen.insert(state_db.clone()) {
                out.push(HermesProfileSource {
                    state_db,
                    legacy_project_pin: read_config_pinned_project_root(
                        &profile_dir.join("config.yaml"),
                    )
                    .map(PathBuf::from),
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
        for (profile_dir, _) in candidates {
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
    session_input_tokens: Option<i64>,
    session_output_tokens: Option<i64>,
    session_cache_read_tokens: Option<i64>,
    session_cache_write_tokens: Option<i64>,
    session_reasoning_tokens: Option<i64>,
    /// `messages.active` soft-delete flag (0 = rewound/undone turn). Legacy
    /// stores without the column read as 1.
    active: i64,
}

struct HermesObservationRecord {
    native: Value,
    native_record_id: ObservationId,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
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
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
) -> Result<HermesObservationRecord, ObservationCoverageReason> {
    if row.timestamp.is_some_and(|value| !value.is_finite()) {
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
        .with_message_id(stable_record_id.clone())
        .with_agent_id(agent_id);
    if let Some(parent_session_id) = native.parent_session_id.as_deref() {
        let parent_agent_id = stable_native_id("hermes.session", &json!(parent_session_id))
            .map_err(|()| ObservationRecordParseErrorV1::InvalidCanonicalEnvelope)?;
        relations = relations.with_parent_agent_id(parent_agent_id);
    }

    let role = canonical_message_role(&native.role)?;
    let mut facts = Vec::new();
    if role == CanonicalMessageRoleV1::Tool {
        facts.push(CanonicalObservationFactV1::ToolResult {
            invocation_id: None,
            content: native.content.clone().map_or(Value::Null, Value::String),
            success: None,
        });
    } else {
        facts.push(CanonicalObservationFactV1::Message {
            role,
            content: native.content.clone().map_or(Value::Null, Value::String),
            model: native.model.clone(),
            timestamp: native.timestamp.map(|value| value as i64),
        });
    }
    append_tool_invocations(&mut facts, native.tool_calls.as_ref(), &stable_record_id)?;

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
    admitted: bool,
    scope: ObservationScopeV1,
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
    let coverage = if !admitted || row.active == 0 {
        Some(ObservationCoverageReason::OutOfScope)
    } else if !matches!(row.role.as_str(), "user" | "assistant" | "tool" | "system") {
        Some(ObservationCoverageReason::UnsupportedFact)
    } else {
        None
    };
    let action = if let Some(reason) = coverage {
        HermesAdmissionAction::Cover(reason)
    } else {
        match native_observation_record(row, source.clone(), range) {
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
                                scope,
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
    let metadata = std::fs::metadata(path)
        .map_err(|_| "could not inspect Hermes SQLite authority".to_string())?;
    #[cfg(unix)]
    let physical = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let physical = {
        return Err("Hermes SQLite physical identity is unavailable".to_string());
    };
    let mut identity_hasher = Sha256::new();
    identity_hasher.update(physical.0.to_le_bytes());
    identity_hasher.update(physical.1.to_le_bytes());
    let identity_digest = identity_hasher.finalize();
    let mut identity_bytes = [0_u8; 8];
    identity_bytes.copy_from_slice(&identity_digest[..8]);
    let file_identity = u64::from_le_bytes(identity_bytes).max(1);

    let mut resume_hasher = Sha256::new();
    resume_hasher.update(file_identity.to_le_bytes());
    resume_hasher.update(metadata.len().to_le_bytes());
    resume_hasher.update(file_mtime_secs(path).to_le_bytes());
    let resume_digest = resume_hasher.finalize();
    let mut resume_bytes = [0_u8; 8];
    resume_bytes.copy_from_slice(&resume_digest[..8]);
    let resume_fingerprint = u64::from_le_bytes(resume_bytes);
    let generation = ObservationSourceGenerationV1::new(file_identity)
        .map_err(|_| "invalid Hermes SQLite generation".to_string())?;
    Ok((generation, file_identity, resume_fingerprint))
}

fn project_scope(project_root: &Path) -> Result<ObservationScopeV1, String> {
    let layout = crate::storage::resolve_layout_for_current_profile(project_root)
        .map_err(|_| "could not resolve Hermes project identity".to_string())?;
    let project_id = layout
        .identity
        .project_id
        .ok_or_else(|| "Hermes project identity is unavailable".to_string())?;
    let project_id =
        ProjectId::new(project_id).map_err(|_| "invalid Hermes project identity".to_string())?;
    Ok(ObservationScopeV1::Project { project_id })
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
    match outcome.status {
        HostAdmissionStatus::Backpressured => "Hermes observation admission was backpressured",
        HostAdmissionStatus::Unavailable => "Hermes observation authority is unavailable",
        HostAdmissionStatus::Unknown => "Hermes observation provider is unsupported",
        HostAdmissionStatus::Degraded => "Hermes observation admission was degraded",
        HostAdmissionStatus::Supported
        | HostAdmissionStatus::AcceptedForReplay
        | HostAdmissionStatus::Committed
        | HostAdmissionStatus::ExactDuplicate => "Hermes observation admission was incomplete",
    }
    .to_string()
}

async fn admit_rows(
    db: &GlobalDb,
    rows: &[HermesRow],
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    resume_fingerprint: u64,
    route: impl Fn(&HermesRow) -> bool,
) -> Result<TranscriptIngestStats, String> {
    let authorities = match scope {
        ObservationScopeV1::Project { .. } => HostAdmissionAuthorities::new(Some(db), None),
        ObservationScopeV1::Profile => HostAdmissionAuthorities::new(None, Some(db)),
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
            route(row),
            scope.clone(),
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

fn select_new_messages_sql(
    message_columns: &std::collections::BTreeSet<String>,
    session_columns: &std::collections::BTreeSet<String>,
) -> String {
    let reasoning_expr = if message_columns.contains("reasoning") {
        "m.reasoning"
    } else {
        "NULL"
    };
    let active_expr = if message_columns.contains("active") {
        "m.active"
    } else {
        "1"
    };
    let session_cwd_expr = if session_columns.contains("cwd") {
        "s.cwd"
    } else {
        "NULL"
    };
    format!(
        "SELECT m.id, m.session_id, m.role, m.content, {reasoning_expr}, m.tool_name,
                m.tool_calls, m.timestamp,
                s.model, s.parent_session_id, {session_cwd_expr},
                s.input_tokens, s.output_tokens, s.cache_read_tokens, s.cache_write_tokens,
                s.reasoning_tokens, {active_expr}
         FROM messages m LEFT JOIN sessions s ON s.id = m.session_id
         WHERE m.id > ?
         ORDER BY m.id
         LIMIT {CHUNK_ROWS}"
    )
}

/// Incrementally scans one Hermes `state.db`; each fully materialized row is
/// admitted against its session-scoped authoritative SQLite-row cursor. The
/// caller decides whether a source error is runtime noise or migration-blocking.
async fn try_ingest_state_db(
    db: &GlobalDb,
    source: &HermesProfileSource,
    project_root: &Path,
) -> Result<TranscriptIngestStats, String> {
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let (generation, file_identity, resume_fingerprint) = sqlite_incarnation(state_db)?;
    let scope = project_scope(project_root)?;
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await?,
        &table_columns(&conn, "sessions").await?,
    );
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let locations = turn_project_locations(&new.items, project_root, source);
        let admitted = admit_rows(
            db,
            &new.items,
            scope.clone(),
            generation,
            file_identity,
            resume_fingerprint,
            |row| locations.contains(&row.id),
        )
        .await?;
        stats.messages_upserted = stats
            .messages_upserted
            .saturating_add(admitted.messages_upserted);
        stats.sessions_upserted = stats
            .sessions_upserted
            .saturating_add(admitted.sessions_upserted);
        read_cursor.position = new.new_cursor.position;
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

/// Shared-source equivalent of [`try_ingest_state_db`]. The `SQLite` page is read
/// once, then each destination independently admits routed rows against its own
/// authoritative observation cursor.
async fn try_ingest_state_db_for_projects(
    source: &HermesProfileSource,
    destinations: &[ProjectIngestDestination<'_>],
) -> Result<TranscriptIngestStats, String> {
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let (generation, file_identity, resume_fingerprint) = sqlite_incarnation(state_db)?;
    let scopes = destinations
        .iter()
        .map(|destination| project_scope(destination.project_root))
        .collect::<Result<Vec<_>, _>>()?;
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await?,
        &table_columns(&conn, "sessions").await?,
    );
    let destination_matchers = destinations
        .par_iter()
        .map(|destination| ProjectRootMatcher::new(destination.project_root))
        .collect::<Vec<_>>();
    let mut destination_routes = HashMap::<PathBuf, Vec<usize>>::new();
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
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
                |row| locations[index].by_row_id.contains(&row.id),
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
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
}

async fn try_ingest_user_state_db(
    db: &GlobalDb,
    source: &HermesProfileSource,
    _registered_roots: &[PathBuf],
) -> Result<TranscriptIngestStats, String> {
    let state_db = &source.state_db;
    let conn = open_read_only_strict(state_db).await?;
    let (generation, file_identity, resume_fingerprint) = sqlite_incarnation(state_db)?;
    let scope = ObservationScopeV1::Profile;
    let select_sql = select_new_messages_sql(
        &message_columns(&conn).await?,
        &table_columns(&conn, "sessions").await?,
    );
    let mut read_cursor = StoredCursor::default();
    let mut stats = TranscriptIngestStats::default();
    loop {
        let new = read_new_rows_strict(&conn, &select_sql, read_cursor).await?;
        let row_count = new.items.len();
        if row_count == 0 {
            return Ok(stats);
        }
        let locations = user_turn_locations(&new.items, source);
        let admitted = admit_rows(
            db,
            &new.items,
            scope.clone(),
            generation,
            file_identity,
            resume_fingerprint,
            |row| locations.contains(&row.id),
        )
        .await?;
        stats.messages_upserted = stats
            .messages_upserted
            .saturating_add(admitted.messages_upserted);
        stats.sessions_upserted = stats
            .sessions_upserted
            .saturating_add(admitted.sessions_upserted);
        read_cursor.position = new.new_cursor.position;
        if row_count < CHUNK_ROWS {
            return Ok(stats);
        }
    }
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
) -> Result<NewRows<HermesRow>, String> {
    let mut rows = conn
        .query(select_sql, libsql::params![prev.position as i64])
        .await
        .map_err(|error| format!("could not query legacy Hermes state rows: {error}"))?;
    let mut items = Vec::new();
    let mut max_rowid = prev.position;
    loop {
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
        max_rowid = max_rowid.max(rowid as u64);
        items.push(
            map_row(rowid, &row)
                .ok_or_else(|| format!("legacy Hermes state row {rowid} is malformed"))?,
        );
    }
    Ok(NewRows {
        items,
        new_cursor: StoredCursor {
            position: max_rowid,
            mtime: 0,
            file_id: 0,
        },
    })
}

fn map_row(rowid: i64, row: &libsql::Row) -> Option<HermesRow> {
    Some(HermesRow {
        id: rowid,
        session_id: row.get::<String>(1).ok()?,
        role: row.get::<String>(2).unwrap_or_default(),
        content: row.get::<Option<String>>(3).ok().flatten(),
        reasoning: row.get::<Option<String>>(4).ok().flatten(),
        tool_name: row.get::<Option<String>>(5).ok().flatten(),
        tool_calls: row.get::<Option<String>>(6).ok().flatten(),
        timestamp: row.get::<Option<f64>>(7).ok().flatten(),
        session_model: row.get::<Option<String>>(8).ok().flatten(),
        parent_session_id: row.get::<Option<String>>(9).ok().flatten(),
        session_cwd: row.get::<Option<String>>(10).ok().flatten(),
        session_input_tokens: row.get::<Option<i64>>(11).ok().flatten(),
        session_output_tokens: row.get::<Option<i64>>(12).ok().flatten(),
        session_cache_read_tokens: row.get::<Option<i64>>(13).ok().flatten(),
        session_cache_write_tokens: row.get::<Option<i64>>(14).ok().flatten(),
        session_reasoning_tokens: row.get::<Option<i64>>(15).ok().flatten(),
        active: row.get::<Option<i64>>(16).ok().flatten().unwrap_or(1),
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
) -> HashSet<i64> {
    let mut by_session: HashMap<&str, Vec<&HermesRow>> = HashMap::new();
    for row in rows {
        by_session.entry(&row.session_id).or_default().push(row);
    }
    let mut locations = HashSet::new();
    for session_rows in by_session.into_values() {
        let has_fallback = session_rows
            .iter()
            .any(|row| session_is_candidate_for_project(row, project_root, source));
        let mut turn = Vec::new();
        for row in session_rows {
            if row.role == "user" && !turn.is_empty() {
                assign_turn_location(&turn, project_root, has_fallback, &mut locations);
                turn.clear();
            }
            turn.push(row);
        }
        assign_turn_location(&turn, project_root, has_fallback, &mut locations);
    }
    locations
}

struct DestinationTurnLocations {
    by_row_id: HashSet<i64>,
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
            by_row_id: HashSet::new(),
        })
        .collect::<Vec<_>>();
    for session_rows in by_session.into_values() {
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
    locations: &mut [DestinationTurnLocations],
    destination_routes: &mut HashMap<PathBuf, Vec<usize>>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    let mut selected = vec![false; destination_matchers.len()];
    if explicit_paths.is_empty() {
        selected.copy_from_slice(fallbacks);
    } else {
        for path in explicit_paths {
            for destination_index in
                matching_destinations(&path, destination_matchers, destination_routes)
            {
                selected[destination_index] = true;
            }
        }
    }
    for (selected, destination) in selected.into_iter().zip(locations) {
        if selected {
            destination.by_row_id.extend(rows.iter().map(|row| row.id));
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
    locations: &mut HashSet<i64>,
) {
    let explicit_paths = rows
        .iter()
        .rev()
        .flat_map(|row| structured_tool_project_paths(row))
        .collect::<Vec<_>>();
    if (!explicit_paths.is_empty()
        && explicit_paths
            .iter()
            .any(|path| path_belongs_to_project(path, project_root)))
        || (explicit_paths.is_empty() && has_fallback)
    {
        locations.extend(rows.iter().map(|row| row.id));
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
            session_input_tokens: Some(10),
            session_output_tokens: Some(5),
            session_cache_read_tokens: Some(4),
            session_cache_write_tokens: Some(3),
            session_reasoning_tokens: Some(2),
            active: 1,
        }
    }

    fn normalized(row: &HermesRow, start: u64) -> HermesObservationRecord {
        let source = observation_source(row).unwrap();
        let range = ObservationSourceRangeV1::new(start, row.id as u64).unwrap();
        native_observation_record(row, source, range).unwrap()
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
            envelope.relations().message_id(),
            Some(envelope.stable_record_id())
        );
        assert!(envelope.relations().agent_id().is_some());
        assert!(envelope.relations().parent_agent_id().is_some());
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
    fn tool_message_becomes_typed_result_without_generic_message_blob() {
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
        assert!(
            !envelope
                .facts()
                .iter()
                .any(|fact| matches!(fact, CanonicalObservationFactV1::Message { .. }))
        );
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
            true,
            ObservationScopeV1::Profile,
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
            true,
            ObservationScopeV1::Profile,
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
            true,
            ObservationScopeV1::Profile,
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
            false,
            ObservationScopeV1::Profile,
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
            true,
            ObservationScopeV1::Profile,
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
            true,
            ObservationScopeV1::Profile,
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
}
