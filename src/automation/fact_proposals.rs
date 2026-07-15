use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::config_error;
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::memory::trust::DEFAULT_TRUST;
use crate::memory::types::{AddFactOutcome, AddFactRequest};
use crate::tracedecay::current_timestamp;

const FACT_PROPOSALS_FILENAME: &str = "fact_proposals.json";
static FACT_PROPOSAL_STORE_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static FACT_PROPOSAL_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactProposalState {
    PendingApproval,
    Applying,
    Applied,
    Rejected,
}

impl FactProposalState {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().replace('-', "_").as_str() {
            "pending" | "pending_approval" => Ok(Self::PendingApproval),
            "applying" => Ok(Self::Applying),
            "applied" => Ok(Self::Applied),
            "rejected" | "rejected_validation" => Ok(Self::Rejected),
            other => Err(config_error(format!(
                "unknown fact proposal state '{other}'; expected pending_approval, applying, applied, or rejected"
            ))),
        }
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_fact_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_outcome: Option<AddFactOutcome>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Later near-duplicate proposals folded into this record.
    #[serde(default, skip_serializing_if = "crate::serde_util::is_default")]
    pub duplicate_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_duplicate_run_id: Option<String>,
    /// Capped content samples from folded near-duplicate proposals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded_contents: Vec<String>,
}

const MAX_FOLDED_CONTENTS: usize = 10;
const FOLDED_CONTENT_MAX_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactProposalStore {
    pub schema_version: u32,
    #[serde(default)]
    pub proposals: Vec<FactProposalRecord>,
}

impl Default for FactProposalStore {
    fn default() -> Self {
        Self {
            schema_version: 1,
            proposals: Vec::new(),
        }
    }
}

pub fn fact_proposals_path(dashboard_root: &Path) -> PathBuf {
    dashboard_root.join(FACT_PROPOSALS_FILENAME)
}

pub async fn load_fact_proposal_store(dashboard_root: &Path) -> Result<FactProposalStore> {
    load_fact_proposal_store_unlocked(dashboard_root).await
}

async fn load_fact_proposal_store_unlocked(dashboard_root: &Path) -> Result<FactProposalStore> {
    let path = fact_proposals_path(dashboard_root);
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FactProposalStore::default());
        }
        Err(e) => {
            return Err(config_error(format!(
                "failed to read fact proposal store '{}': {e}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| {
        config_error(format!(
            "failed to parse fact proposal store '{}': {e}",
            path.display()
        ))
    })
}

pub async fn save_fact_proposal_store(
    dashboard_root: &Path,
    store: &FactProposalStore,
) -> Result<()> {
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    save_fact_proposal_store_unlocked(dashboard_root, store).await
}

async fn save_fact_proposal_store_unlocked(
    dashboard_root: &Path,
    store: &FactProposalStore,
) -> Result<()> {
    let path = fact_proposals_path(dashboard_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            config_error(format!(
                "failed to create fact proposal directory '{}': {e}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(store).map_err(TraceDecayError::from)?;
    let nonce = FACT_PROPOSAL_TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(
        ".{FACT_PROPOSALS_FILENAME}.{}.{}.{}.tmp",
        std::process::id(),
        crate::runtime_identity::process_run_id(),
        nonce
    ));
    // Keep publication synchronous: once it starts, async cancellation cannot
    // drop the mutation lock while a stale replacement is still in flight.
    crate::db::DatabaseAuthority::publish_record_atomically(
        &temporary,
        &path,
        &bytes,
        "fact proposal store",
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

pub async fn list_fact_proposals(
    dashboard_root: &Path,
    state: Option<FactProposalState>,
    limit: usize,
) -> Result<Vec<FactProposalRecord>> {
    let mut proposals = load_fact_proposal_store(dashboard_root).await?.proposals;
    if let Some(state) = state {
        proposals.retain(|proposal| proposal.state == state);
    }
    proposals.sort_by_key(|proposal| std::cmp::Reverse(proposal.created_at));
    proposals.truncate(limit);
    Ok(proposals)
}

pub async fn list_applying_fact_proposals(
    dashboard_root: &Path,
) -> Result<Vec<FactProposalRecord>> {
    let mut proposals = load_fact_proposal_store(dashboard_root).await?.proposals;
    proposals.retain(|proposal| proposal.state == FactProposalState::Applying);
    proposals.sort_by_key(|proposal| std::cmp::Reverse(proposal.created_at));
    Ok(proposals)
}

pub async fn load_fact_proposal(
    dashboard_root: &Path,
    proposal_id: &str,
) -> Result<Option<FactProposalRecord>> {
    Ok(load_fact_proposal_store(dashboard_root)
        .await?
        .proposals
        .into_iter()
        .find(|proposal| proposal.proposal_id == proposal_id))
}

pub async fn record_session_fact_proposals(
    dashboard_root: &Path,
    run_id: &str,
    evidence_hash: Option<&str>,
    accepted_facts: &[Value],
    rejected_facts: &[Value],
) -> Result<Vec<FactProposalRecord>> {
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut store = load_fact_proposal_store_unlocked(dashboard_root).await?;
    let mut records = Vec::new();
    let now = current_timestamp();
    for (index, value) in accepted_facts.iter().enumerate() {
        let add_fact_request = value
            .get("add_fact_request")
            .cloned()
            .ok_or_else(|| config_error("accepted fact proposal missing add_fact_request"))?;
        let add_fact_request = serde_json::from_value::<AddFactRequest>(add_fact_request)
            .map_err(|e| config_error(format!("invalid accepted fact add_fact_request: {e}")))?;
        let incoming_tokens = crate::memory::similarity::content_tokens(&add_fact_request.content);
        if let Some(existing_idx) = find_exact_resumable_proposal(&store, &add_fact_request) {
            fold_duplicate(&mut store.proposals[existing_idx], run_id, now, None);
            continue;
        }
        if applied_exact_match_exists(&store, &add_fact_request) {
            continue;
        }
        if let Some(existing_idx) =
            find_similar_pending_proposal(&store, &add_fact_request, &incoming_tokens)
        {
            fold_duplicate(
                &mut store.proposals[existing_idx],
                run_id,
                now,
                Some(&add_fact_request.content),
            );
            continue;
        }
        let proposal = value.get("proposal").cloned();
        let validation = value.get("validation").cloned();
        let record = FactProposalRecord {
            schema_version: 1,
            proposal_id: proposal_id(run_id, index, value),
            run_id: run_id.to_string(),
            evidence_hash: evidence_hash.map(ToOwned::to_owned),
            state: FactProposalState::PendingApproval,
            add_fact_request: Some(add_fact_request),
            proposal,
            validation_reason: None,
            validation,
            reviewer: None,
            applied_fact_id: None,
            apply_outcome: None,
            created_at: now,
            updated_at: now,
            duplicate_count: 0,
            last_duplicate_run_id: None,
            folded_contents: Vec::new(),
        };
        records.push(record.clone());
        store.proposals.push(record);
    }
    for (index, value) in rejected_facts.iter().enumerate() {
        let record = FactProposalRecord {
            schema_version: 1,
            proposal_id: proposal_id(run_id, accepted_facts.len() + index, value),
            run_id: run_id.to_string(),
            evidence_hash: evidence_hash.map(ToOwned::to_owned),
            state: FactProposalState::Rejected,
            add_fact_request: None,
            proposal: value.get("proposal").cloned(),
            validation_reason: value
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            validation: value.get("validation").cloned(),
            reviewer: Some("validator".to_string()),
            applied_fact_id: None,
            apply_outcome: None,
            created_at: now,
            updated_at: now,
            duplicate_count: 0,
            last_duplicate_run_id: None,
            folded_contents: Vec::new(),
        };
        records.push(record.clone());
        store.proposals.push(record);
    }
    save_fact_proposal_store_unlocked(dashboard_root, &store).await?;
    Ok(records)
}

const MIN_SIMILARITY_TOKENS: usize = 8;

fn fold_duplicate(
    target: &mut FactProposalRecord,
    run_id: &str,
    now: i64,
    discarded_content: Option<&str>,
) {
    target.duplicate_count = target.duplicate_count.saturating_add(1);
    target.last_duplicate_run_id = Some(run_id.to_string());
    target.updated_at = now;
    if let Some(content) = discarded_content
        && target.folded_contents.len() < MAX_FOLDED_CONTENTS
    {
        let truncated: String = content.chars().take(FOLDED_CONTENT_MAX_CHARS).collect();
        target.folded_contents.push(truncated);
    }
}

fn find_similar_pending_proposal(
    store: &FactProposalStore,
    request: &AddFactRequest,
    request_tokens: &std::collections::BTreeSet<String>,
) -> Option<usize> {
    if request_tokens.len() < MIN_SIMILARITY_TOKENS {
        return None;
    }
    store.proposals.iter().position(|proposal| {
        if proposal.state != FactProposalState::PendingApproval {
            return false;
        }
        let Some(existing) = proposal.add_fact_request.as_ref() else {
            return false;
        };
        if existing.category != request.category {
            return false;
        }
        let existing_tokens = crate::memory::similarity::content_tokens(&existing.content);
        if existing_tokens.len() < MIN_SIMILARITY_TOKENS {
            return false;
        }
        let (_, token_overlap, overlap_coefficient) =
            crate::memory::similarity::lexical_overlap_tokens(&existing_tokens, request_tokens);
        token_overlap >= 0.45 && overlap_coefficient >= 0.65
    })
}

fn find_exact_resumable_proposal(
    store: &FactProposalStore,
    request: &AddFactRequest,
) -> Option<usize> {
    let content = normalize_fact_content(&request.content);
    store.proposals.iter().position(|proposal| {
        matches!(
            proposal.state,
            FactProposalState::PendingApproval | FactProposalState::Applying
        ) && proposal.add_fact_request.as_ref().is_some_and(|existing| {
            existing.category == request.category
                && normalize_fact_content(&existing.content) == content
        })
    })
}

fn applied_exact_match_exists(store: &FactProposalStore, request: &AddFactRequest) -> bool {
    let content = normalize_fact_content(&request.content);
    store.proposals.iter().any(|proposal| {
        proposal.state == FactProposalState::Applied
            && proposal.add_fact_request.as_ref().is_some_and(|existing| {
                existing.category == request.category
                    && normalize_fact_content(&existing.content) == content
            })
    })
}

fn normalize_fact_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub async fn apply_fact_proposal(
    dashboard_root: &Path,
    db: &Database,
    proposal_id: &str,
    reviewer: Option<String>,
) -> Result<FactProposalRecord> {
    apply_fact_proposal_inner(dashboard_root, db, proposal_id, reviewer, || Ok(())).await
}

async fn apply_fact_proposal_inner<F>(
    dashboard_root: &Path,
    db: &Database,
    proposal_id: &str,
    reviewer: Option<String>,
    after_fact_commit: F,
) -> Result<FactProposalRecord>
where
    F: FnOnce() -> Result<()>,
{
    let writer = db.writer_connection("apply fact proposal").await?;
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut store = load_fact_proposal_store_unlocked(dashboard_root).await?;
    let record_index = store
        .proposals
        .iter()
        .position(|proposal| proposal.proposal_id == proposal_id)
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    let record = &mut store.proposals[record_index];
    match record.state {
        FactProposalState::Applied => return Ok(record.clone()),
        FactProposalState::Rejected if record.apply_outcome.is_some() => {
            return Ok(record.clone());
        }
        FactProposalState::Rejected => {
            return Err(config_error(format!(
                "fact proposal '{proposal_id}' is not pending approval"
            )));
        }
        FactProposalState::PendingApproval => {
            record.state = FactProposalState::Applying;
            record.reviewer.clone_from(&reviewer);
            record.updated_at = current_timestamp();
            save_fact_proposal_store_unlocked(dashboard_root, &store).await?;
        }
        FactProposalState::Applying => {
            if record.reviewer.is_none() {
                record.reviewer.clone_from(&reviewer);
            }
        }
    }
    let Some(request) = store.proposals[record_index].add_fact_request.clone() else {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' has no add_fact_request"
        )));
    };
    let outcome = writer
        .memory_store()
        .add_fact(request, DEFAULT_TRUST)
        .await?;
    after_fact_commit()?;
    let record = &mut store.proposals[record_index];
    record.updated_at = current_timestamp();
    record.applied_fact_id = outcome.fact.as_ref().map(|fact| fact.fact_id);
    record.apply_outcome = Some(outcome.clone());
    if outcome.fact.is_some() {
        record.state = FactProposalState::Applied;
    } else {
        record.state = FactProposalState::Rejected;
        record.validation_reason.clone_from(&outcome.diff.reason);
    }
    let updated = record.clone();
    save_fact_proposal_store_unlocked(dashboard_root, &store).await?;
    Ok(updated)
}

pub async fn reject_fact_proposal(
    dashboard_root: &Path,
    proposal_id: &str,
    reviewer: Option<String>,
    reason: Option<String>,
) -> Result<FactProposalRecord> {
    let lock = fact_proposal_store_lock(dashboard_root);
    let _guard = lock.lock().await;
    let mut store = load_fact_proposal_store_unlocked(dashboard_root).await?;
    let record = store
        .proposals
        .iter_mut()
        .find(|proposal| proposal.proposal_id == proposal_id)
        .ok_or_else(|| config_error(format!("fact proposal '{proposal_id}' not found")))?;
    if record.state != FactProposalState::PendingApproval {
        return Err(config_error(format!(
            "fact proposal '{proposal_id}' is not pending approval"
        )));
    }
    record.state = FactProposalState::Rejected;
    record.reviewer = reviewer;
    record.validation_reason = reason;
    record.updated_at = current_timestamp();
    let updated = record.clone();
    save_fact_proposal_store_unlocked(dashboard_root, &store).await?;
    Ok(updated)
}

fn proposal_id(run_id: &str, index: usize, value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(index.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(value.to_string().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("fact_{}", &digest[..16])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn accepted_fact(content: &str) -> Value {
        json!({
            "add_fact_request": {
                "content": content,
                "category": "project",
                "source": null,
                "tags": ["automation"],
                "entities": ["TraceDecay"],
                "trust": 0.9,
                "metadata": {"origin": "fact-proposal-test"}
            }
        })
    }

    async fn test_database(temp: &tempfile::TempDir) -> Database {
        let path = temp.path().join("memory.db");
        let authority =
            crate::db::DatabaseAuthority::acquire_test(&path, "fact proposal atomicity test")
                .unwrap();
        crate::db::Database::initialize(&path, &authority)
            .await
            .unwrap()
            .0
    }

    async fn matching_fact_count(db: &Database, content: &str) -> i64 {
        let mut rows = db
            .conn()
            .query(
                "SELECT COUNT(*) FROM memory_facts WHERE content = ?1",
                libsql::params![content],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    #[tokio::test]
    async fn apply_recovers_durable_intent_without_duplicate_fact() {
        let temp = tempfile::tempdir().unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let db = test_database(&temp).await;
        let content = "Fact proposal apply is recoverable after database commit";
        let records = record_session_fact_proposals(
            &dashboard_root,
            "run-1",
            None,
            &[accepted_fact(content)],
            &[],
        )
        .await
        .unwrap();
        let proposal_id = &records[0].proposal_id;

        let error = apply_fact_proposal_inner(
            &dashboard_root,
            &db,
            proposal_id,
            Some("first-reviewer".to_string()),
            || Err(config_error("injected cancellation after fact commit")),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected cancellation"));
        let interrupted = load_fact_proposal(&dashboard_root, proposal_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(interrupted.state, FactProposalState::Applying);
        assert_eq!(matching_fact_count(&db, content).await, 1);
        assert_eq!(
            list_applying_fact_proposals(&dashboard_root)
                .await
                .unwrap()
                .len(),
            1
        );

        let folded = record_session_fact_proposals(
            &dashboard_root,
            "run-2",
            None,
            &[accepted_fact(content)],
            &[],
        )
        .await
        .unwrap();
        assert!(folded.is_empty());
        assert_eq!(
            load_fact_proposal(&dashboard_root, proposal_id)
                .await
                .unwrap()
                .unwrap()
                .duplicate_count,
            1
        );

        drop(db);
        let db = test_database(&temp).await;
        let recovered = apply_fact_proposal(&dashboard_root, &db, proposal_id, None)
            .await
            .unwrap();
        assert_eq!(recovered.state, FactProposalState::Applied);
        assert_eq!(recovered.reviewer.as_deref(), Some("first-reviewer"));
        assert_eq!(matching_fact_count(&db, content).await, 1);
        let repeated = apply_fact_proposal(
            &dashboard_root,
            &db,
            proposal_id,
            Some("second-reviewer".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(repeated.applied_fact_id, recovered.applied_fact_id);
        assert_eq!(repeated.reviewer, recovered.reviewer);
        assert_eq!(matching_fact_count(&db, content).await, 1);
        assert!(
            list_applying_fact_proposals(&dashboard_root)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_record_and_apply_preserve_both_mutations() {
        let temp = tempfile::tempdir().unwrap();
        let dashboard_root = temp.path().join("dashboard");
        let db = test_database(&temp).await;
        let first = record_session_fact_proposals(
            &dashboard_root,
            "run-1",
            None,
            &[accepted_fact("First concurrent fact proposal")],
            &[],
        )
        .await
        .unwrap();
        let second = accepted_fact("Second concurrent fact proposal");

        let (applied, recorded) = tokio::join!(
            apply_fact_proposal(
                &dashboard_root,
                &db,
                &first[0].proposal_id,
                Some("reviewer".to_string())
            ),
            record_session_fact_proposals(
                &dashboard_root,
                "run-2",
                None,
                std::slice::from_ref(&second),
                &[]
            )
        );
        assert_eq!(applied.unwrap().state, FactProposalState::Applied);
        assert_eq!(recorded.unwrap().len(), 1);

        let store = load_fact_proposal_store(&dashboard_root).await.unwrap();
        assert_eq!(store.proposals.len(), 2);
        assert_eq!(
            store
                .proposals
                .iter()
                .filter(|proposal| proposal.state == FactProposalState::Applied)
                .count(),
            1
        );
        assert!(
            std::fs::read_dir(&dashboard_root)
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
    }
}
