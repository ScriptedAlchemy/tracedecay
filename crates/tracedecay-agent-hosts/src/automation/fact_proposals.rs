//! Authority-backed automation fact proposals.
//!
//! `fact_proposals.json` was the legacy authority.  It is now read once as a
//! bounded legacy import and then archived.  The separate projection file is
//! strictly post-commit display metadata; proposal state, CAS, and applied
//! facts always come from [`MemoryApplication`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_domain::{ActorId, LocatorDigest, ProvenanceId};
use tracedecay_store::{
    ProjectMemoryFactProposalImportV1, ProjectMemoryFactProposalLegacyRecordV1,
    ProjectMemoryFactProposalPromotionDispositionV1, ProjectMemoryFactProposalPromotionV1,
    ProjectMemoryFactProposalRecordV1, ProjectMemoryFactProposalStateV1, FactCompatibilityStore,
};

use super::config_error;
use crate::application::memory::{
    MemoryApplication, MemoryApplicationError, automation_fact_proposal_add_command,
    legacy_proposal_add_command, with_automation_run_id,
};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::{AddFactOutcome, AddFactRequest, MemoryCategory};
use crate::privacy::sanitize_provider_metadata_text;
use crate::tracedecay::current_timestamp;

/// Historical source, consumed only by the one-time importer.
const FACT_PROPOSALS_FILENAME: &str = "fact_proposals.json";
/// Best-effort post-commit presentation cache. Never read to authorize work.
const FACT_PROPOSAL_PROJECTION_FILENAME: &str = "fact_proposals.projection.json";
const FACT_PROPOSAL_ARCHIVE_DIRECTORY: &str = "fact_proposals.archive";
const MAX_LEGACY_IMPORT_RECORDS: usize = 1_000;

static FACT_PROPOSAL_STORE_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static FACT_PROPOSAL_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProposalState {
    PendingApproval,
    /// Legacy-only input state. The durable fact authority never persists an applying state.
    Applying,
    Applied,
    Rejected,
    Quarantined,
}

impl FactProposalState {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().replace('-', "_").as_str() {
            "pending" | "pending_approval" => Ok(Self::PendingApproval),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "rejected" | "rejected_validation" => Ok(Self::Rejected),
            "quarantined" => Ok(Self::Quarantined),
            other => Err(config_error(format!(
                "unknown fact proposal state '{other}'; expected pending_approval, applying, applied, rejected, or quarantined"
            ))),
        }
    }
}

/// Compatibility/display shape retained for dashboard and run-ledger JSON.
/// It is a projection, not a persistence authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactProposalRecord {
    pub schema_version: u32,
    pub proposal_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_hash: Option<String>,
    pub state: FactProposalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_fact_request: Option<AddFactRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Canonical durable fact identity. Never coerce this into a numeric mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_canonical_fact_id: Option<String>,
    /// Legacy numeric mapping, populated only when the authority has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<i64>,
    /// Legacy display-only field. New authority-backed projections leave it
    /// empty rather than manufacturing a legacy write outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_outcome: Option<AddFactOutcome>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "crate::serde_util::is_default")]
    pub duplicate_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_duplicate_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded_contents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactProposalStore {
    pub schema_version: u32,
    #[serde(default)]
    pub proposals: Vec<FactProposalRecord>,
}

impl Default for FactProposalStore {
    fn default() -> Self {
        Self {
            schema_version: 2,
            proposals: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct ProjectionMetadata<'a> {
    run_id: Option<&'a str>,
    evidence_hash: Option<&'a str>,
    observed_at: Option<i64>,
    proposal: Option<&'a Value>,
    validation: Option<&'a Value>,
}

impl ProjectionMetadata<'_> {
    const fn read_only() -> Self {
        Self {
            run_id: None,
            evidence_hash: None,
            observed_at: None,
            proposal: None,
            validation: None,
        }
    }
}

pub fn fact_proposals_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(FACT_PROPOSALS_FILENAME)
}

pub fn fact_proposal_projection_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(FACT_PROPOSAL_PROJECTION_FILENAME)
}

pub async fn load_fact_proposal_store(dashboard_root: &Path) -> Result<FactProposalStore> {
    load_projection_store_unlocked(dashboard_root).await
}

pub async fn save_fact_proposal_store(
    dashboard_root: &Path,
    store: &FactProposalStore,
) -> Result<()> {
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    save_projection_store_unlocked(dashboard_root, store).await
}

async fn load_projection_store_unlocked(dashboard_root: &Path) -> Result<FactProposalStore> {
    let path = fact_proposal_projection_path(dashboard_root);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FactProposalStore::default());
        }
        Err(error) => {
            return Err(config_error(format!(
                "failed to read fact proposal projection '{}': {error}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        config_error(format!(
            "failed to parse fact proposal projection '{}': {error}",
            path.display()
        ))
    })
}

async fn save_projection_store_unlocked(
    dashboard_root: &Path,
    store: &FactProposalStore,
) -> Result<()> {
    let path = fact_proposal_projection_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            config_error(format!(
                "failed to create fact proposal projection directory '{}': {error}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(store).map_err(TraceDecayError::from)?;
    let nonce = FACT_PROPOSAL_TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(
        ".{FACT_PROPOSAL_PROJECTION_FILENAME}.{}.{}.{}.tmp",
        std::process::id(),
        crate::runtime_identity::process_run_id(),
        nonce
    ));
    crate::db::DatabaseAuthority::publish_record_atomically(
        &temporary,
        &path,
        &bytes,
        "fact proposal projection",
    )
}

fn fact_proposal_store_lock(dashboard_root: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let key = dashboard_root.to_path_buf();
    let mut locks = FACT_PROPOSAL_STORE_LOCKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Imports the historical sidecar through the authority, then archives its
/// exact bytes. A failed import leaves the source untouched for retry.
pub async fn import_legacy_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
) -> Result<()> {
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    let legacy_path = fact_proposals_path(dashboard_root);
    let bytes = match tokio::fs::read(&legacy_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(config_error(format!(
                "failed to read legacy fact proposal sidecar '{}': {error}",
                legacy_path.display()
            )));
        }
    };
    let legacy_store = serde_json::from_slice::<FactProposalStore>(&bytes).map_err(|error| {
        config_error(format!(
            "failed to parse legacy fact proposal sidecar '{}': {error}",
            legacy_path.display()
        ))
    })?;
    let sidecar_digest = sidecar_digest(&bytes)?;
    let mut imported = Vec::new();
    for (index, legacy) in legacy_store.proposals.into_iter().enumerate() {
        let legacy_proposal_id = i64::try_from(index + 1)
            .map_err(|_| config_error("legacy fact proposal sidecar has too many records"))?;
        let Some(request) = legacy.add_fact_request else {
            // The original bytes are kept in the archive; a missing request
            // cannot be truthfully reconstructed into a fact assertion.
            continue;
        };
        let Ok(command) = legacy_proposal_add_command(
            memory.owner().clone(),
            sidecar_digest.clone(),
            legacy_proposal_id,
            request,
        ) else {
            continue;
        };
        let Ok(command) = with_automation_run_id(command, &legacy.run_id) else {
            continue;
        };
        let record = ProjectMemoryFactProposalLegacyRecordV1::new(
            legacy_proposal_id,
            import_state(legacy.state),
            command,
        )
        .map_err(store_error)?;
        imported.push(record);
    }
    for records in imported.chunks(MAX_LEGACY_IMPORT_RECORDS) {
        let request = ProjectMemoryFactProposalImportV1::new(
            memory.owner().clone(),
            memory.compatibility_scope().source_store_id().clone(),
            sidecar_digest.clone(),
            records.to_vec(),
        )
        .map_err(store_error)?;
        let receipt = memory
            .import_legacy_compatibility_fact_proposals(request)
            .await
            .map_err(memory_error)?;
        if receipt.imported_count() + receipt.quarantined_count() != records.len() {
            return Err(config_error(
                "legacy proposal import receipt did not account for every submitted record",
            ));
        }
    }
    archive_legacy_sidecar(dashboard_root, &legacy_path, &bytes, &sidecar_digest).await
}

/// Migration must never make an old or corrupt sidecar gate the canonical
/// product path. An explicit importer can surface its error; routine calls
/// retry opportunistically and continue against the authority.
async fn best_effort_legacy_import<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
) {
    let _ = import_legacy_fact_proposals(memory, dashboard_root).await;
}

async fn archive_legacy_sidecar(
    dashboard_root: &Path,
    legacy_path: &Path,
    bytes: &[u8],
    digest: &LocatorDigest,
) -> Result<()> {
    let archive_directory = dashboard_root.join(FACT_PROPOSAL_ARCHIVE_DIRECTORY);
    tokio::fs::create_dir_all(&archive_directory)
        .await
        .map_err(|error| {
            config_error(format!(
                "failed to create fact proposal archive '{}': {error}",
                archive_directory.display()
            ))
        })?;
    let archive_path =
        archive_directory.join(format!("{}.json", digest.as_str().replace(':', "-")));
    match tokio::fs::rename(legacy_path, &archive_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let archived = tokio::fs::read(&archive_path).await.map_err(|read_error| {
                config_error(format!(
                    "failed to read existing fact proposal archive '{}': {read_error}",
                    archive_path.display()
                ))
            })?;
            if archived != bytes {
                return Err(config_error(format!(
                    "legacy fact proposal archive '{}' conflicts with source bytes",
                    archive_path.display()
                )));
            }
            tokio::fs::remove_file(legacy_path).await.map_err(|remove_error| {
                config_error(format!(
                    "failed to retire duplicate legacy fact proposal sidecar '{}': {remove_error}",
                    legacy_path.display()
                ))
            })
        }
        Err(error) => Err(config_error(format!(
            "failed to archive legacy fact proposal sidecar '{}': {error}",
            legacy_path.display()
        ))),
    }
}

pub async fn record_session_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    run_id: &str,
    evidence_hash: Option<&str>,
    accepted_facts: &[Value],
    rejected_facts: &[Value],
) -> Result<Vec<FactProposalRecord>> {
    best_effort_legacy_import(memory, dashboard_root).await;
    let mut records = Vec::with_capacity(accepted_facts.len() + rejected_facts.len());
    let observed_at = current_timestamp();
    let evidence_hash = bounded_metadata_text(evidence_hash, 160);
    let submitter = proposal_actor("automation:session-reflector")?;
    let mut submitted_semantic_keys = HashSet::new();
    let mut submitted_proposal_ids = HashSet::new();

    for (index, value) in accepted_facts.iter().enumerate() {
        let proposal_id = proposal_id(run_id, index, value);
        let request = value
            .get("add_fact_request")
            .cloned()
            .ok_or_else(|| config_error("accepted fact proposal missing add_fact_request"))
            .and_then(|request| {
                serde_json::from_value::<AddFactRequest>(request).map_err(|error| {
                    config_error(format!("invalid accepted fact add_fact_request: {error}"))
                })
            });
        let Ok(request) = request else {
            records.push(rejected_projection(
                &proposal_id,
                run_id,
                evidence_hash.as_deref(),
                FactProposalState::Quarantined,
                "automation proposal could not be reconstructed",
                observed_at,
            ));
            continue;
        };
        let Ok(command) = automation_fact_proposal_add_command(
            memory.owner().clone(),
            request,
            run_id,
            &proposal_id,
            Some(submitter.clone()),
        ) else {
            records.push(rejected_projection(
                &proposal_id,
                run_id,
                evidence_hash.as_deref(),
                FactProposalState::Quarantined,
                "automation proposal was rejected by memory privacy validation",
                observed_at,
            ));
            continue;
        };
        let semantic_key = (
            command.category(),
            normalize_fact_content(command.content()),
        );
        if !submitted_semantic_keys.insert(semantic_key) {
            // Preserve the legacy exact-duplicate contract: different
            // evidence annotations for the same fact assertion are a partial
            // no-op, not a second proposal or promotion.
            continue;
        }
        let authoritative_id = ProvenanceId::new(proposal_id.clone()).map_err(store_error)?;
        let proposal = memory
            .submit_compatibility_fact_proposal(authoritative_id, command, Some(submitter.clone()))
            .await
            .map_err(memory_error)?;
        if !submitted_proposal_ids.insert(proposal.proposal_id().as_str().to_string()) {
            // The authority collapsed this exact canonical command/digest into
            // an earlier proposal. Keep one display record so a duplicate
            // model item remains a partial no-op, as under the legacy store.
            continue;
        }
        records.push(
            project_authoritative_record(
                dashboard_root,
                &proposal,
                ProjectionMetadata {
                    run_id: Some(run_id),
                    evidence_hash: evidence_hash.as_deref(),
                    observed_at: Some(observed_at),
                    proposal: value.get("proposal"),
                    validation: value.get("validation"),
                },
            )
            .await
            .unwrap_or_else(|_| {
                record_from_authority(
                    &proposal,
                    None,
                    ProjectionMetadata {
                        run_id: Some(run_id),
                        evidence_hash: evidence_hash.as_deref(),
                        observed_at: Some(observed_at),
                        proposal: value.get("proposal"),
                        validation: value.get("validation"),
                    },
                )
            }),
        );
    }

    for (index, _) in rejected_facts.iter().enumerate() {
        records.push(rejected_projection(
            &proposal_id(run_id, accepted_facts.len() + index, &rejected_facts[index]),
            run_id,
            evidence_hash.as_deref(),
            FactProposalState::Rejected,
            "automation proposal was rejected before authority submission",
            observed_at,
        ));
    }
    Ok(records)
}

pub async fn list_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    state: Option<FactProposalState>,
    limit: usize,
) -> Result<Vec<FactProposalRecord>> {
    best_effort_legacy_import(memory, dashboard_root).await;
    if limit == 0 || state == Some(FactProposalState::Applying) {
        return Ok(Vec::new());
    }
    let limit = limit.min(MAX_LEGACY_IMPORT_RECORDS);
    let page = memory
        .list_compatibility_fact_proposals(state.map(compatibility_state), None, limit)
        .await
        .map_err(memory_error)?;
    let projection = load_fact_proposal_store(dashboard_root)
        .await
        .unwrap_or_default();
    let projection_order: HashMap<&str, usize> = projection
        .proposals
        .iter()
        .enumerate()
        .map(|(index, record)| (record.proposal_id.as_str(), index))
        .collect();
    let mut rendered = page
        .proposals()
        .iter()
        .map(|proposal| {
            let previous = projection
                .proposals
                .iter()
                .find(|record| record.proposal_id == proposal.proposal_id().as_str());
            record_from_authority(proposal, previous, ProjectionMetadata::read_only())
        })
        .collect::<Vec<_>>();
    rendered.sort_by(|left, right| {
        projection_order
            .get(left.proposal_id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
            .cmp(
                &projection_order
                    .get(right.proposal_id.as_str())
                    .copied()
                    .unwrap_or(usize::MAX),
            )
            .then_with(|| left.proposal_id.cmp(&right.proposal_id))
    });
    Ok(rendered)
}

pub async fn load_fact_proposal<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    proposal_id: &str,
) -> Result<Option<FactProposalRecord>> {
    best_effort_legacy_import(memory, dashboard_root).await;
    let proposal_id = ProvenanceId::new(proposal_id.to_string()).map_err(store_error)?;
    let proposal = memory
        .get_compatibility_fact_proposal(proposal_id)
        .await
        .map_err(memory_error)?;
    let projection = load_fact_proposal_store(dashboard_root)
        .await
        .unwrap_or_default();
    Ok(proposal.map(|proposal| {
        let previous = projection
            .proposals
            .iter()
            .find(|record| record.proposal_id == proposal.proposal_id().as_str());
        record_from_authority(&proposal, previous, ProjectionMetadata::read_only())
    }))
}

/// There is deliberately no authoritative `Applying` state.
pub async fn list_applying_fact_proposals<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
) -> Result<Vec<FactProposalRecord>> {
    best_effort_legacy_import(memory, dashboard_root).await;
    Ok(Vec::new())
}

pub async fn apply_fact_proposal<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    proposal_id: &str,
    reviewer: Option<String>,
) -> Result<FactProposalRecord> {
    Ok(
        apply_fact_proposal_with_result(memory, dashboard_root, proposal_id, reviewer)
            .await?
            .record,
    )
}

/// Authority-backed apply result. `newly_promoted` is an atomic store receipt,
/// not an inference from the final proposal state.
#[derive(Debug, Clone, PartialEq)]
pub struct FactProposalApplyResult {
    pub record: FactProposalRecord,
    pub newly_promoted: bool,
}

pub async fn apply_fact_proposal_with_result<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    proposal_id: &str,
    reviewer: Option<String>,
) -> Result<FactProposalApplyResult> {
    best_effort_legacy_import(memory, dashboard_root).await;
    let proposal_id = ProvenanceId::new(proposal_id.to_string()).map_err(store_error)?;
    let current = memory
        .get_compatibility_fact_proposal(proposal_id.clone())
        .await
        .map_err(memory_error)?
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if current.state() == ProjectMemoryFactProposalStateV1::Applied {
        return Ok(FactProposalApplyResult {
            record: render_authority_record(dashboard_root, &current).await,
            newly_promoted: false,
        });
    }
    if current.state() != ProjectMemoryFactProposalStateV1::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    let reviewer_actor = proposal_actor("automation:proposal-review")?;
    let request = ProjectMemoryFactProposalPromotionV1::new(
        memory.owner().clone(),
        proposal_id,
        current.revision(),
        Some(reviewer_actor),
    )
    .map_err(store_error)?;
    let promotion = memory
        .promote_compatibility_fact_proposal_with_disposition(request)
        .await
        .map_err(memory_error)?;
    let proposal = promotion.proposal();
    let display_reviewer = bounded_metadata_text(reviewer.as_deref(), 160);
    let record = project_authoritative_record(
        dashboard_root,
        proposal,
        ProjectionMetadata {
            run_id: None,
            evidence_hash: None,
            observed_at: Some(current_timestamp()),
            proposal: None,
            validation: None,
        },
    )
    .await
    .map_or_else(
        |_| render_authority_record_sync(proposal),
        |mut record| {
            if display_reviewer.is_some() {
                record.reviewer = display_reviewer;
            }
            record
        },
    );
    Ok(FactProposalApplyResult {
        record,
        newly_promoted: matches!(
            promotion.disposition(),
            ProjectMemoryFactProposalPromotionDispositionV1::NewlyPromoted
        ),
    })
}

pub async fn reject_fact_proposal<A: FactCompatibilityStore>(
    memory: &MemoryApplication<A>,
    dashboard_root: &Path,
    proposal_id: &str,
    reviewer: Option<String>,
    reason: Option<String>,
) -> Result<FactProposalRecord> {
    best_effort_legacy_import(memory, dashboard_root).await;
    let proposal_id = ProvenanceId::new(proposal_id.to_string()).map_err(store_error)?;
    let current = memory
        .get_compatibility_fact_proposal(proposal_id.clone())
        .await
        .map_err(memory_error)?
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if current.state() == ProjectMemoryFactProposalStateV1::Rejected {
        return Ok(render_authority_record(dashboard_root, &current).await);
    }
    if current.state() != ProjectMemoryFactProposalStateV1::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    let reviewer_actor = proposal_actor("automation:proposal-review")?;
    let reason = sanitized_reason(reason);
    let proposal = memory
        .reject_compatibility_fact_proposal(proposal_id, current.revision(), reviewer_actor, reason)
        .await
        .map_err(memory_error)?;
    let display_reviewer = bounded_metadata_text(reviewer.as_deref(), 160);
    let record = project_authoritative_record(
        dashboard_root,
        &proposal,
        ProjectionMetadata {
            run_id: None,
            evidence_hash: None,
            observed_at: Some(current_timestamp()),
            proposal: None,
            validation: None,
        },
    )
    .await
    .map_or_else(
        |_| render_authority_record_sync(&proposal),
        |mut record| {
            if display_reviewer.is_some() {
                record.reviewer = display_reviewer;
            }
            record
        },
    );
    Ok(record)
}

async fn project_authoritative_record(
    dashboard_root: &Path,
    proposal: &ProjectMemoryFactProposalRecordV1,
    metadata: ProjectionMetadata<'_>,
) -> Result<FactProposalRecord> {
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut store = load_projection_store_unlocked(dashboard_root).await?;
    let previous = store
        .proposals
        .iter()
        .find(|record| record.proposal_id == proposal.proposal_id().as_str())
        .cloned();
    let record = record_from_authority(proposal, previous.as_ref(), metadata);
    if let Some(index) = store
        .proposals
        .iter()
        .position(|entry| entry.proposal_id == record.proposal_id)
    {
        // Projection order is display-only metadata, but it preserves the
        // user-visible source order while authority state remains canonical.
        store.proposals[index] = record.clone();
    } else {
        store.proposals.push(record.clone());
    }
    save_projection_store_unlocked(dashboard_root, &store).await?;
    Ok(record)
}

async fn render_authority_record(
    dashboard_root: &Path,
    proposal: &ProjectMemoryFactProposalRecordV1,
) -> FactProposalRecord {
    let projection = load_fact_proposal_store(dashboard_root)
        .await
        .unwrap_or_default();
    let previous = projection
        .proposals
        .iter()
        .find(|record| record.proposal_id == proposal.proposal_id().as_str());
    record_from_authority(proposal, previous, ProjectionMetadata::read_only())
}

fn render_authority_record_sync(
    proposal: &ProjectMemoryFactProposalRecordV1,
) -> FactProposalRecord {
    record_from_authority(proposal, None, ProjectionMetadata::read_only())
}

fn record_from_authority(
    proposal: &ProjectMemoryFactProposalRecordV1,
    previous: Option<&FactProposalRecord>,
    metadata: ProjectionMetadata<'_>,
) -> FactProposalRecord {
    let observed_at = metadata.observed_at.unwrap_or(0);
    let created_at = previous.map_or(observed_at, |record| record.created_at);
    let updated_at = metadata
        .observed_at
        .or_else(|| previous.map(|record| record.updated_at))
        .unwrap_or(0);
    FactProposalRecord {
        schema_version: 2,
        proposal_id: proposal.proposal_id().as_str().to_string(),
        run_id: metadata
            .run_id
            .map(ToOwned::to_owned)
            .or_else(|| previous.map(|record| record.run_id.clone()))
            .or_else(|| proposal.automation_run_id().map(ToOwned::to_owned))
            .unwrap_or_else(|| "unknown".to_string()),
        evidence_hash: metadata
            .evidence_hash
            .and_then(|value| bounded_metadata_text(Some(value), 160))
            .or_else(|| previous.and_then(|record| record.evidence_hash.clone())),
        state: display_state(proposal.state()),
        add_fact_request: Some(add_request_from_command(proposal.request())),
        proposal: metadata
            .proposal
            .cloned()
            .or_else(|| previous.and_then(|record| record.proposal.clone())),
        validation_reason: proposal.reason().map(ToOwned::to_owned),
        validation: metadata
            .validation
            .cloned()
            .or_else(|| previous.and_then(|record| record.validation.clone())),
        reviewer: proposal.reviewer().map(|actor| actor.as_str().to_string()),
        applied_canonical_fact_id: proposal
            .applied_fact_id()
            .map(|fact_id| fact_id.as_str().to_string()),
        applied_fact_id: proposal.legacy_fact_id(),
        apply_outcome: None,
        created_at,
        updated_at,
        duplicate_count: 0,
        last_duplicate_run_id: None,
        folded_contents: Vec::new(),
    }
}

fn rejected_projection(
    proposal_id: &str,
    run_id: &str,
    evidence_hash: Option<&str>,
    state: FactProposalState,
    reason: &str,
    observed_at: i64,
) -> FactProposalRecord {
    FactProposalRecord {
        schema_version: 2,
        proposal_id: proposal_id.to_string(),
        run_id: run_id.to_string(),
        evidence_hash: bounded_metadata_text(evidence_hash, 160),
        state,
        add_fact_request: None,
        proposal: None,
        validation_reason: Some(reason.to_string()),
        validation: None,
        reviewer: Some("automation:session-reflector".to_string()),
        applied_canonical_fact_id: None,
        applied_fact_id: None,
        apply_outcome: None,
        created_at: observed_at,
        updated_at: observed_at,
        duplicate_count: 0,
        last_duplicate_run_id: None,
        folded_contents: Vec::new(),
    }
}

fn add_request_from_command(
    command: &tracedecay_store::ProjectMemoryFactAddCommandV1,
) -> AddFactRequest {
    AddFactRequest {
        content: command.content().to_string(),
        category: MemoryCategory::from(command.category()),
        source: command.source().map(ToOwned::to_owned),
        tags: command.tags().to_vec(),
        entities: command.entities().to_vec(),
        trust: Some(command.default_trust().as_f64()),
        metadata: command.metadata().clone(),
    }
}

const fn compatibility_state(state: FactProposalState) -> ProjectMemoryFactProposalStateV1 {
    match state {
        FactProposalState::PendingApproval | FactProposalState::Applying => {
            ProjectMemoryFactProposalStateV1::PendingApproval
        }
        FactProposalState::Applied => ProjectMemoryFactProposalStateV1::Applied,
        FactProposalState::Rejected => ProjectMemoryFactProposalStateV1::Rejected,
        FactProposalState::Quarantined => ProjectMemoryFactProposalStateV1::Quarantined,
    }
}

const fn import_state(state: FactProposalState) -> ProjectMemoryFactProposalStateV1 {
    compatibility_state(state)
}

const fn display_state(state: ProjectMemoryFactProposalStateV1) -> FactProposalState {
    match state {
        ProjectMemoryFactProposalStateV1::PendingApproval
        | ProjectMemoryFactProposalStateV1::Applying => FactProposalState::PendingApproval,
        ProjectMemoryFactProposalStateV1::Applied => FactProposalState::Applied,
        ProjectMemoryFactProposalStateV1::Rejected => FactProposalState::Rejected,
        ProjectMemoryFactProposalStateV1::Quarantined => FactProposalState::Quarantined,
    }
}

fn sidecar_digest(bytes: &[u8]) -> Result<LocatorDigest> {
    LocatorDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
        .map_err(store_error)
}

fn proposal_actor(value: &str) -> Result<ActorId> {
    ActorId::new(value.to_string()).map_err(store_error)
}

fn sanitized_reason(reason: Option<String>) -> String {
    bounded_metadata_text(reason.as_deref(), 512)
        .unwrap_or_else(|| "rejected by reviewer".to_string())
}

fn bounded_metadata_text(value: Option<&str>, maximum: usize) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return None;
    }
    sanitize_provider_metadata_text(value)
        .filter(|sanitized| !sanitized.trim().is_empty() && sanitized.len() <= maximum)
}

fn proposal_id(run_id: &str, index: usize, value: &Value) -> String {
    let mut hasher = Sha256::new();
    let index = index.to_string();
    let value = value.to_string();
    for component in [run_id.as_bytes(), index.as_bytes(), value.as_bytes()] {
        hasher.update((component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    format!("fact_{}", &hex::encode(hasher.finalize())[..16])
}

fn normalize_fact_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn memory_error(error: MemoryApplicationError) -> TraceDecayError {
    config_error(format!("fact proposal authority failed: {error}"))
}

fn store_error(error: impl std::fmt::Display) -> TraceDecayError {
    config_error(format!("fact proposal contract is invalid: {error}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "fact_proposals_test.rs"]
mod fact_proposals_test;
