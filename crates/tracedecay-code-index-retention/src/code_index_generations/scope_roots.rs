//! Liveness-based retention for whole code-index scope roots.
//!
//! Generation retention operates *within* one scope root
//! (`code-index-v1/<sha256(canonical_project_root)>/`). Every process opens
//! exactly one scope from the project root it was handed, so no journey ever
//! enumerates the siblings. A profile therefore accumulates scope trees
//! belonging to project roots that no longer exist — and those bytes are
//! unreachable by generation retention and uncounted by any report.
//!
//! This module closes that gap under the same discipline as generation
//! retention: journal, quarantine, durable receipt, then unlink. It is
//! deliberately harder to trigger than generation retention, because the unit
//! of collection is an entire directory tree rather than one superseded file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_text::is_lowercase_hex;
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};

#[cfg(test)]
use super::SCOPE_RETENTION_QUARANTINE_DIRECTORY;
use super::journal::{
    BoundedJournalSpec, clear_journal, journal_path, load_journal, persist_journal,
};
use super::locking::{acquire_code_generation_store_lock, acquire_scope_retention_lock};
use super::receipt_store;
use super::receipt_store::{ReceiptStoreSpec, receipt_digest_file_component};
use super::scope_quarantine::{ScopeDirectoryIdentityV1, ScopeQuarantineAuthority};
use super::{
    CodeGenerationRetentionErrorV1, CodeGenerationRetentionModeV1,
    MAX_SCOPE_BINDING_CLEANUP_INTENT_BYTES, MAX_SCOPE_TRANSACTION_BYTES,
    SCOPE_BINDING_CLEANUP_INTENT_FILE, SCOPE_BINDING_CLEANUP_INTENT_SCHEMA,
    SCOPE_RETENTION_RECEIPT_SCHEMA, SCOPE_RETENTION_RECEIPTS_DIRECTORY,
    SCOPE_RETENTION_TRANSACTION_FILE, SCOPE_RETENTION_TRANSACTION_SCHEMA,
    SCOPE_ROOT_LIVENESS_PROOF_SCHEMA, STORE_LOCK_FILE, TRANSACTION_FILE, code_index_scope_hash,
    storage,
};

const SCOPE_TRANSACTION_JOURNAL: BoundedJournalSpec<ScopeRootRetentionTransactionV1> =
    BoundedJournalSpec {
        file_name: SCOPE_RETENTION_TRANSACTION_FILE,
        max_bytes: MAX_SCOPE_TRANSACTION_BYTES,
        label: "scope reconciliation transaction",
        write_context: "code-index-scope-retention-transaction",
        validate: validate_scope_transaction,
    };

const SCOPE_BINDING_CLEANUP_INTENT_JOURNAL: BoundedJournalSpec<ScopeRootBindingCleanupIntentV1> =
    BoundedJournalSpec {
        file_name: SCOPE_BINDING_CLEANUP_INTENT_FILE,
        max_bytes: MAX_SCOPE_BINDING_CLEANUP_INTENT_BYTES,
        label: "scope binding cleanup intent",
        write_context: "code-index-scope-binding-cleanup-intent",
        validate: validate_scope_binding_cleanup_intent,
    };

const SCOPE_RECEIPT_STORE: ReceiptStoreSpec = ReceiptStoreSpec {
    directory: SCOPE_RETENTION_RECEIPTS_DIRECTORY,
    label: "scope reconciliation receipt",
};

/// One `code-index-v1/` scope directory whose scope hash matches no live
/// canonical project root.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StrandedCodeIndexScopeV1 {
    /// The directory name, which is `hex(sha256(canonical_project_root))`.
    pub scope_hash: String,
    /// Total payload bytes under the scope, excluding retention lock files.
    pub size_bytes: u64,
    /// Newest mtime anywhere in the scope, in unix seconds. Drives the age gate.
    pub newest_mtime_secs: i64,
}
/// Why a stranded scope was left alone even though nothing live names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrandedScopeRefusalV1 {
    /// The scope has an unfinished generation-retention journal. Recovering it
    /// belongs to that scope's own owner; collecting it here would destroy the
    /// evidence that recovery needs.
    PendingGenerationRetention,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefusedCodeIndexScopeV1 {
    pub scope: StrandedCodeIndexScopeV1,
    pub refusal: StrandedScopeRefusalV1,
}

/// The reconciliation decision for one `code-index-v1/` store root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRootRetentionPlanV1 {
    /// Scope hashes derived from the live canonical roots the caller proved.
    pub live_scope_hashes: BTreeSet<String>,
    /// Scope directories on disk that matched a live root.
    pub live_scope_count: usize,
    /// Directory entries that are not scope roots at all (receipts, quarantine,
    /// lock files). Reported so an unexpected layout is visible rather than
    /// silently swept into "stranded".
    pub unrecognized_entry_count: usize,
    pub minimum_stranding_age_secs: i64,
    /// Stranded, past the age gate, and free of a pending journal.
    pub collectable_scopes: Vec<StrandedCodeIndexScopeV1>,
    /// Stranded but touched too recently to be called abandoned.
    pub retained_immature_scopes: Vec<StrandedCodeIndexScopeV1>,
    /// Stranded but structurally refused.
    pub refused_scopes: Vec<RefusedCodeIndexScopeV1>,
    /// Present only when the canonical production authorities sealed this plan
    /// for Apply. Raw-root observation plans deliberately leave it absent.
    liveness_proof: Option<ScopeRootLivenessProofV1>,
}

impl ScopeRootRetentionPlanV1 {
    #[must_use]
    pub fn liveness_proof(&self) -> Option<&ScopeRootLivenessProofV1> {
        self.liveness_proof.as_ref()
    }

    /// Every scope no live root names, whatever this pass decided to do about
    /// it. This is the number a storage report or Doctor finding must publish:
    /// the gap is "unreachable bytes", not "bytes we happened to collect".
    #[must_use]
    pub fn stranded_scope_count(&self) -> u64 {
        (self.collectable_scopes.len()
            + self.retained_immature_scopes.len()
            + self.refused_scopes.len()) as u64
    }

    #[must_use]
    pub fn stranded_scope_bytes(&self) -> u64 {
        self.collectable_scopes
            .iter()
            .chain(self.retained_immature_scopes.iter())
            .chain(self.refused_scopes.iter().map(|refused| &refused.scope))
            .fold(0_u64, |total, scope| total.saturating_add(scope.size_bytes))
    }

    #[must_use]
    pub fn collectable_scope_bytes(&self) -> u64 {
        total_scope_bytes(&self.collectable_scopes)
    }
}

/// Terminal receipt for one bounded liveness authority. `revision` identifies
/// the exact snapshot while `digest` covers every row in that snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootAuthorityReceiptV1 {
    pub revision: String,
    pub terminal_count: u64,
    pub digest: String,
}

/// Exact relational source bound to one physical code-index scope at the
/// vector census revision recorded by the proof.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootCandidateBindingV1 {
    pub scope_hash: String,
    pub source_scope: tracedecay_store::StoreShardIdV1,
    pub vector_census_revision: String,
    pub live: bool,
}

/// Complete, revision-bound proof used by scope collection. Every authority is
/// explicit so adding a new liveness source cannot silently omit it from the
/// digest or from crash replay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootLivenessProofV1 {
    pub schema: String,
    pub proof_digest: String,
    pub live_scope_hashes: BTreeSet<String>,
    pub registered_roots: ScopeRootAuthorityReceiptV1,
    pub git_worktrees: ScopeRootAuthorityReceiptV1,
    pub mounted_leases: ScopeRootAuthorityReceiptV1,
    pub configuration_roots: ScopeRootAuthorityReceiptV1,
    pub vector_census: ScopeRootAuthorityReceiptV1,
    pub vector_dependencies: ScopeRootAuthorityReceiptV1,
    pub candidate_binding: ScopeRootCandidateBindingV1,
}

#[derive(Serialize)]
pub(super) struct ScopeRootLivenessProofMaterialV1<'a> {
    schema: &'static str,
    live_scope_hashes: &'a BTreeSet<String>,
    registered_roots: &'a ScopeRootAuthorityReceiptV1,
    git_worktrees: &'a ScopeRootAuthorityReceiptV1,
    mounted_leases: &'a ScopeRootAuthorityReceiptV1,
    configuration_roots: &'a ScopeRootAuthorityReceiptV1,
    vector_census: &'a ScopeRootAuthorityReceiptV1,
    vector_dependencies: &'a ScopeRootAuthorityReceiptV1,
    candidate_binding: &'a ScopeRootCandidateBindingV1,
}

impl ScopeRootLivenessProofV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        live_scope_hashes: BTreeSet<String>,
        registered_roots: ScopeRootAuthorityReceiptV1,
        git_worktrees: ScopeRootAuthorityReceiptV1,
        mounted_leases: ScopeRootAuthorityReceiptV1,
        configuration_roots: ScopeRootAuthorityReceiptV1,
        vector_census: ScopeRootAuthorityReceiptV1,
        vector_dependencies: ScopeRootAuthorityReceiptV1,
        candidate_binding: ScopeRootCandidateBindingV1,
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        let mut proof = Self {
            schema: SCOPE_ROOT_LIVENESS_PROOF_SCHEMA.to_owned(),
            proof_digest: String::new(),
            live_scope_hashes,
            registered_roots,
            git_worktrees,
            mounted_leases,
            configuration_roots,
            vector_census,
            vector_dependencies,
            candidate_binding,
        };
        proof.refresh_digest()?;
        validate_scope_root_liveness_proof(&proof)?;
        Ok(proof)
    }

    pub(super) fn refresh_digest(&mut self) -> Result<(), CodeGenerationRetentionErrorV1> {
        self.proof_digest = scope_root_liveness_proof_digest(self)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootBindingCleanupReplayV1 {
    pub scope_hash: String,
    pub source_scope: tracedecay_store::StoreShardIdV1,
    pub liveness_proof: ScopeRootLivenessProofV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScopeRootRetentionReceiptV1 {
    pub schema: String,
    pub receipt_digest: String,
    /// The exact live set the decision was made against, so a receipt can be
    /// audited without re-deriving it.
    pub live_scope_hashes: BTreeSet<String>,
    pub liveness_proof: ScopeRootLivenessProofV1,
    pub minimum_stranding_age_secs: i64,
    pub collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    pub reclaimed_bytes: u64,
    pub completed_at_micros: i64,
}

#[derive(Serialize)]
pub(super) struct ScopeReceiptMaterial<'a> {
    schema: &'static str,
    live_scope_hashes: &'a BTreeSet<String>,
    liveness_proof: &'a ScopeRootLivenessProofV1,
    minimum_stranding_age_secs: i64,
    collected_scopes: &'a [StrandedCodeIndexScopeV1],
    reclaimed_bytes: u64,
    completed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopeRootRetentionTransactionV1 {
    pub(super) schema: String,
    pub(super) receipt: ScopeRootRetentionReceiptV1,
    pub(super) scope_identities: BTreeMap<String, ScopeDirectoryIdentityV1>,
}

/// A durable promise to remove one semantic source-scope binding only after
/// the corresponding scope-root receipt has committed. This lives beside the
/// filesystem transaction because deleting the source scope would otherwise
/// erase the only place an interrupted relational cleanup could be recovered.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopeRootBindingCleanupIntentV1 {
    schema: String,
    scope_hash: String,
    source_scope: tracedecay_store::StoreShardIdV1,
    liveness_proof: ScopeRootLivenessProofV1,
    receipt: ScopeRootRetentionReceiptV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeRootRetentionReportV1 {
    pub plan: ScopeRootRetentionPlanV1,
    pub collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    pub receipt: Option<ScopeRootRetentionReceiptV1>,
}

/// Read-only classification of every scope directory under one
/// `code-index-v1/` store root against caller-supplied roots.
///
/// This API never seals an Apply-capable plan. Production collection uses
/// [`plan_scope_root_retention_with_liveness_proof`] after the canonical
/// authorities have produced a complete revision-bound receipt.
#[hotpath::measure(label = "usecases.retention.plan_scope")]
pub fn plan_scope_root_retention(
    store_root: &Path,
    live_canonical_roots: &BTreeSet<PathBuf>,
    minimum_stranding_age_secs: i64,
    now_secs: i64,
) -> Result<ScopeRootRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if live_canonical_roots.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope reconciliation refused an empty live-root set".to_owned(),
        ));
    }
    let live_scope_hashes = live_canonical_roots
        .iter()
        .map(|root| code_index_scope_hash(root))
        .collect::<BTreeSet<_>>();
    plan_scope_root_retention_from_hashes(
        store_root,
        &live_scope_hashes,
        minimum_stranding_age_secs,
        now_secs,
    )
}

/// Plan an Apply-capable scope pass from the complete canonical liveness proof.
/// The physical executor will require the exact proof again immediately before
/// quarantine, which turns every authority revision into a compare-and-swap.
pub fn plan_scope_root_retention_with_liveness_proof(
    store_root: &Path,
    liveness_proof: ScopeRootLivenessProofV1,
    minimum_stranding_age_secs: i64,
    now_secs: i64,
) -> Result<ScopeRootRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    validate_scope_root_liveness_proof(&liveness_proof)?;
    let mut plan = plan_scope_root_retention_from_hashes(
        store_root,
        &liveness_proof.live_scope_hashes,
        minimum_stranding_age_secs,
        now_secs,
    )?;
    if liveness_proof.candidate_binding.live
        || liveness_proof
            .live_scope_hashes
            .contains(&liveness_proof.candidate_binding.scope_hash)
        || !plan
            .collectable_scopes
            .iter()
            .any(|scope| scope.scope_hash == liveness_proof.candidate_binding.scope_hash)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness proof does not authorize its exact collection candidate".to_owned(),
        ));
    }
    plan.collectable_scopes
        .retain(|scope| scope.scope_hash == liveness_proof.candidate_binding.scope_hash);
    plan.liveness_proof = Some(liveness_proof);
    Ok(plan)
}

pub(super) fn plan_scope_root_retention_from_hashes(
    store_root: &Path,
    live_scope_hashes: &BTreeSet<String>,
    minimum_stranding_age_secs: i64,
    now_secs: i64,
) -> Result<ScopeRootRetentionPlanV1, CodeGenerationRetentionErrorV1> {
    if live_scope_hashes.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope reconciliation refused an empty live-root set".to_owned(),
        ));
    }
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope reconciliation recovery is pending".to_owned(),
        ));
    }

    let entries = std::fs::read_dir(store_root).map_err(storage)?;
    let mut plan = ScopeRootRetentionPlanV1 {
        live_scope_hashes: live_scope_hashes.clone(),
        live_scope_count: 0,
        unrecognized_entry_count: 0,
        minimum_stranding_age_secs,
        collectable_scopes: Vec::new(),
        retained_immature_scopes: Vec::new(),
        refused_scopes: Vec::new(),
        liveness_proof: None,
    };

    for entry in entries {
        let entry = entry.map_err(storage)?;
        let file_type = entry.file_type().map_err(storage)?;
        if !file_type.is_dir() {
            continue;
        }
        let Some(scope_hash) = entry.file_name().to_str().map(str::to_owned) else {
            plan.unrecognized_entry_count = plan.unrecognized_entry_count.saturating_add(1);
            continue;
        };
        // Only a directory literally named `hex(sha256(root))` is a scope. This
        // is what keeps the receipts and quarantine directories — and anything
        // else a future layout adds — structurally uncollectable.
        if !is_code_index_scope_hash(&scope_hash) {
            plan.unrecognized_entry_count = plan.unrecognized_entry_count.saturating_add(1);
            continue;
        }
        if live_scope_hashes.contains(&scope_hash) {
            plan.live_scope_count = plan.live_scope_count.saturating_add(1);
            continue;
        }

        let scope_root = entry.path();
        let (size_bytes, newest_mtime_secs) = measure_scope_tree(&scope_root)?;
        let scope = StrandedCodeIndexScopeV1 {
            scope_hash,
            size_bytes,
            newest_mtime_secs,
        };
        if scope_root.join(TRANSACTION_FILE).exists() {
            plan.refused_scopes.push(RefusedCodeIndexScopeV1 {
                scope,
                refusal: StrandedScopeRefusalV1::PendingGenerationRetention,
            });
            continue;
        }
        if now_secs.saturating_sub(newest_mtime_secs) < minimum_stranding_age_secs {
            plan.retained_immature_scopes.push(scope);
            continue;
        }
        plan.collectable_scopes.push(scope);
    }

    plan.collectable_scopes
        .sort_by(|left, right| left.scope_hash.cmp(&right.scope_hash));
    plan.retained_immature_scopes
        .sort_by(|left, right| left.scope_hash.cmp(&right.scope_hash));
    plan.refused_scopes
        .sort_by(|left, right| left.scope.scope_hash.cmp(&right.scope.scope_hash));
    Ok(plan)
}

/// Collect the one stranded scope whose exact semantic binding-cleanup intent
/// was durably recorded, under the journal → quarantine → durable receipt →
/// unlink ordering generation retention uses.
#[hotpath::measure(label = "usecases.retention.execute_scope")]
pub fn execute_scope_root_retention(
    store_root: &Path,
    plan: ScopeRootRetentionPlanV1,
    revalidated_liveness_proof: &ScopeRootLivenessProofV1,
    mode: CodeGenerationRetentionModeV1,
    now_secs: i64,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReportV1, CodeGenerationRetentionErrorV1> {
    if mode == CodeGenerationRetentionModeV1::DryRun || plan.collectable_scopes.is_empty() {
        return Ok(ScopeRootRetentionReportV1 {
            plan,
            collected_scopes: Vec::new(),
            receipt: None,
        });
    }

    validate_scope_root_liveness_proof(revalidated_liveness_proof)?;
    let planned_liveness_proof = plan.liveness_proof.as_ref().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope Apply requires a canonical proof-bound plan".to_owned(),
        )
    })?;
    if planned_liveness_proof != revalidated_liveness_proof {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness authority changed before quarantine".to_owned(),
        ));
    }
    let candidate = plan.collectable_scopes.first().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup requires a singleton collection plan".to_owned(),
        )
    })?;
    let expected_binding_cleanup_intent = ScopeRootBindingCleanupIntentV1 {
        schema: SCOPE_BINDING_CLEANUP_INTENT_SCHEMA.to_owned(),
        scope_hash: candidate.scope_hash.clone(),
        source_scope: revalidated_liveness_proof
            .candidate_binding
            .source_scope
            .clone(),
        liveness_proof: revalidated_liveness_proof.clone(),
        receipt: binding_cleanup_receipt(&plan, &candidate.scope_hash, completed_at)?,
    };
    validate_scope_binding_cleanup_intent(&expected_binding_cleanup_intent)?;

    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    recover_pending_scope_transaction_unlocked(store_root)?;
    if plan.liveness_proof.as_ref() != Some(revalidated_liveness_proof) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness authority changed at the quarantine boundary".to_owned(),
        ));
    }
    match load_scope_binding_cleanup_intent(store_root)? {
        Some(intent) if intent == expected_binding_cleanup_intent => {}
        Some(_) => {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope binding cleanup intent does not match the collection plan".to_owned(),
            ));
        }
        None => {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope collection requires a durable binding cleanup intent".to_owned(),
            ));
        }
    }

    // Re-verify every candidate under the pass lock, and hold each scope's own
    // generation-retention lock while doing so, so a concurrent generation pass
    // in that scope cannot be running.
    let mut scope_locks = Vec::with_capacity(plan.collectable_scopes.len());
    let mut collected = Vec::with_capacity(plan.collectable_scopes.len());
    for scope in &plan.collectable_scopes {
        if plan.live_scope_hashes.contains(&scope.scope_hash) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "code-index scope reconciliation planned a live scope for collection".to_owned(),
            ));
        }
        let scope_root = scope_root_path(store_root, &scope.scope_hash)?;
        if !scope_directory_exists(&scope_root)? {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' disappeared after the reconciliation mark phase",
                scope.scope_hash
            )));
        }
        scope_locks.push(acquire_code_generation_store_lock(&scope_root)?);
        if scope_root.join(TRANSACTION_FILE).exists() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' has a pending generation-retention journal",
                scope.scope_hash
            )));
        }
        let (size_bytes, newest_mtime_secs) = measure_scope_tree(&scope_root)?;
        if size_bytes != scope.size_bytes || newest_mtime_secs != scope.newest_mtime_secs {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' changed after the reconciliation mark phase",
                scope.scope_hash
            )));
        }
        if now_secs.saturating_sub(newest_mtime_secs) < plan.minimum_stranding_age_secs {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "stranded scope '{}' is younger than the minimum stranding age",
                scope.scope_hash
            )));
        }
        collected.push(scope.clone());
    }

    let receipt = expected_binding_cleanup_intent.receipt;
    let mut quarantine =
        ScopeQuarantineAuthority::prepare(store_root, &receipt.receipt_digest, &collected)?;
    let transaction = ScopeRootRetentionTransactionV1 {
        schema: SCOPE_RETENTION_TRANSACTION_SCHEMA.to_owned(),
        receipt: receipt.clone(),
        scope_identities: quarantine.scope_identities().clone(),
    };
    persist_scope_transaction(store_root, &transaction)?;

    let result = (|| {
        quarantine.stage(&transaction.receipt.collected_scopes)?;
        write_scope_receipt(store_root, &receipt)?;
        quarantine.cleanup_committed(&transaction.receipt.collected_scopes)?;
        clear_scope_transaction(store_root)
    })();
    if let Err(error) = result {
        if !scope_receipt_is_durable(store_root, &receipt)? {
            quarantine.rollback(&transaction.receipt.collected_scopes)?;
            clear_scope_transaction(store_root)?;
        }
        return Err(error);
    }

    drop(scope_locks);
    Ok(ScopeRootRetentionReportV1 {
        plan,
        collected_scopes: collected,
        receipt: Some(receipt),
    })
}

/// Finish or undo an interrupted scope-reconciliation transaction.
#[hotpath::measure(label = "usecases.retention.recover_scope")]
pub fn recover_scope_root_retention(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if !store_root.is_dir() {
        return Ok(());
    }
    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    recover_pending_scope_transaction_unlocked(store_root)
}

/// Persist the relational cleanup that must follow one exact scope-root
/// collection before the scope can be physically quarantined.
///
/// The receipt is derived from the same singleton plan and timestamp that
/// `execute_scope_root_retention` will use. That makes a later replay depend
/// on the durable filesystem decision, rather than on a newly derived plan.
pub fn prepare_scope_root_binding_cleanup(
    store_root: &Path,
    plan: &ScopeRootRetentionPlanV1,
    scope_hash: &str,
    source_scope: &tracedecay_store::StoreShardIdV1,
    revalidated_liveness_proof: &ScopeRootLivenessProofV1,
    completed_at: UtcMicros,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    validate_scope_root_liveness_proof(revalidated_liveness_proof)?;
    if plan.liveness_proof.as_ref() != Some(revalidated_liveness_proof)
        || revalidated_liveness_proof.candidate_binding.scope_hash != scope_hash
        || revalidated_liveness_proof.candidate_binding.source_scope != *source_scope
        || revalidated_liveness_proof.candidate_binding.live
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope cleanup intent does not match its revalidated liveness proof".to_owned(),
        ));
    }
    let receipt = binding_cleanup_receipt(plan, scope_hash, completed_at)?;
    let intent = ScopeRootBindingCleanupIntentV1 {
        schema: SCOPE_BINDING_CLEANUP_INTENT_SCHEMA.to_owned(),
        scope_hash: scope_hash.to_owned(),
        source_scope: source_scope.clone(),
        liveness_proof: revalidated_liveness_proof.clone(),
        receipt,
    };
    validate_scope_binding_cleanup_intent(&intent)?;

    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup cannot begin while filesystem recovery is pending".to_owned(),
        ));
    }
    match load_scope_binding_cleanup_intent(store_root)? {
        None => persist_scope_binding_cleanup_intent(store_root, &intent),
        Some(existing) if existing == intent => Ok(()),
        Some(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "a different scope binding cleanup intent is already pending".to_owned(),
        )),
    }
}

/// Return the exact binding whose cleanup must be replayed after filesystem
/// transaction recovery. A rolled-back filesystem transaction clears its
/// intent; every other state that cannot prove either outcome is unsafe.
pub fn recover_scope_root_binding_cleanup(
    store_root: &Path,
) -> Result<Option<ScopeRootBindingCleanupReplayV1>, CodeGenerationRetentionErrorV1> {
    if !store_root.is_dir() {
        return Ok(None);
    }
    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup requires filesystem transaction recovery first".to_owned(),
        ));
    }
    let Some(intent) = load_scope_binding_cleanup_intent(store_root)? else {
        return Ok(None);
    };
    let source_exists = scope_directory_exists(&scope_root_path(store_root, &intent.scope_hash)?)?;
    if scope_receipt_is_durable(store_root, &intent.receipt)? {
        if source_exists {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope binding cleanup receipt is durable but its source scope remains".to_owned(),
            ));
        }
        return Ok(Some(ScopeRootBindingCleanupReplayV1 {
            scope_hash: intent.scope_hash,
            source_scope: intent.source_scope,
            liveness_proof: intent.liveness_proof,
        }));
    }
    if source_exists {
        clear_scope_binding_cleanup_intent(store_root)?;
        return Ok(None);
    }
    Err(CodeGenerationRetentionErrorV1::UnsafeState(
        "scope binding cleanup cannot prove whether its source scope was collected".to_owned(),
    ))
}

/// Clear a replayed binding-cleanup intent only after the exact receipt is
/// durable and its exact source scope is absent.
pub fn complete_scope_root_binding_cleanup(
    store_root: &Path,
    replay: &ScopeRootBindingCleanupReplayV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let _pass_lock = acquire_scope_retention_lock(store_root)?;
    if scope_transaction_path(store_root).exists() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup cannot complete while filesystem recovery is pending".to_owned(),
        ));
    }
    let intent = load_scope_binding_cleanup_intent(store_root)?.ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion has no pending intent".to_owned(),
        )
    })?;
    if intent.scope_hash != replay.scope_hash
        || intent.source_scope != replay.source_scope
        || intent.liveness_proof != replay.liveness_proof
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion does not match its pending intent".to_owned(),
        ));
    }
    if !scope_receipt_is_durable(store_root, &intent.receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion has no durable filesystem receipt".to_owned(),
        ));
    }
    if scope_directory_exists(&scope_root_path(store_root, &replay.scope_hash)?)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup completion found its source scope present".to_owned(),
        ));
    }
    clear_scope_binding_cleanup_intent(store_root)
}

pub(super) fn recover_pending_scope_transaction_unlocked(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    let Some(transaction) = load_scope_transaction(store_root)? else {
        return Ok(());
    };
    let mut quarantine = ScopeQuarantineAuthority::recover(
        store_root,
        &transaction.receipt.receipt_digest,
        transaction.scope_identities.clone(),
    )?;
    if scope_receipt_is_durable(store_root, &transaction.receipt)? {
        quarantine.cleanup_committed(&transaction.receipt.collected_scopes)?;
    } else {
        quarantine.rollback(&transaction.receipt.collected_scopes)?;
    }
    clear_scope_transaction(store_root)
}

pub(super) fn scope_transaction_path(store_root: &Path) -> PathBuf {
    journal_path(store_root, &SCOPE_TRANSACTION_JOURNAL)
}

#[cfg(test)]
pub(super) fn scope_stage_root(
    store_root: &Path,
    receipt: &ScopeRootRetentionReceiptV1,
) -> PathBuf {
    store_root
        .join(SCOPE_RETENTION_QUARANTINE_DIRECTORY)
        .join(&receipt.receipt_digest)
}

#[cfg(test)]
pub(super) fn scope_receipt_path(
    store_root: &Path,
    receipt: &ScopeRootRetentionReceiptV1,
) -> PathBuf {
    receipt_store::receipt_path(store_root, &SCOPE_RECEIPT_STORE, &receipt.receipt_digest)
}

/// Join a scope hash onto the store root, refusing anything that is not a bare
/// 64-character hex component. Every destructive path in this section is built
/// through here, so no journal value can escape the store root.
pub(super) fn scope_root_path(
    store_root: &Path,
    scope_hash: &str,
) -> Result<PathBuf, CodeGenerationRetentionErrorV1> {
    if !is_code_index_scope_hash(scope_hash) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "code-index scope name is not a SHA-256 path component".to_owned(),
        ));
    }
    Ok(store_root.join(scope_hash))
}

pub(super) fn is_code_index_scope_hash(value: &str) -> bool {
    is_lowercase_hex(value, 64)
}

/// Payload bytes and newest mtime for one scope tree.
///
/// Retention lock files and the scope root's own directory mtime are excluded
/// deliberately: acquiring the scope lock creates that file and stamps that
/// directory, so including them would make the execution-time "nothing changed
/// since the mark phase" fence unsatisfiable. Symlinks are refused outright —
/// nothing in a code-index scope creates them, and a tree that is about to be
/// renamed and unlinked is the wrong place to start interpreting them.
pub(super) fn measure_scope_tree(
    scope_root: &Path,
) -> Result<(u64, i64), CodeGenerationRetentionErrorV1> {
    let mut total_bytes = 0_u64;
    let mut newest_mtime = i64::MIN;
    let mut pending = vec![scope_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if directory != scope_root {
            newest_mtime = newest_mtime.max(directory_mtime_secs(&directory)?);
        }
        for entry in std::fs::read_dir(&directory).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let file_type = entry.file_type().map_err(storage)?;
            if file_type.is_symlink() {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "code-index scope '{}' contains a symlink",
                    scope_root.display()
                )));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                    "code-index scope '{}' contains a non-regular file",
                    scope_root.display()
                )));
            }
            if entry.file_name().to_str() == Some(STORE_LOCK_FILE) {
                continue;
            }
            let metadata = entry.metadata().map_err(storage)?;
            total_bytes = total_bytes.saturating_add(metadata.len());
            newest_mtime = newest_mtime.max(mtime_secs(&metadata)?);
        }
    }
    Ok((
        total_bytes,
        if newest_mtime == i64::MIN {
            0
        } else {
            newest_mtime
        },
    ))
}

pub(super) fn directory_mtime_secs(path: &Path) -> Result<i64, CodeGenerationRetentionErrorV1> {
    mtime_secs(&std::fs::symlink_metadata(path).map_err(storage)?)
}

pub(super) fn mtime_secs(
    metadata: &std::fs::Metadata,
) -> Result<i64, CodeGenerationRetentionErrorV1> {
    let modified = metadata.modified().map_err(storage)?;
    let seconds = match modified.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    };
    Ok(seconds)
}

pub(super) fn total_scope_bytes(scopes: &[StrandedCodeIndexScopeV1]) -> u64 {
    scopes
        .iter()
        .fold(0_u64, |total, scope| total.saturating_add(scope.size_bytes))
}

pub(super) fn build_scope_receipt(
    plan: &ScopeRootRetentionPlanV1,
    collected_scopes: Vec<StrandedCodeIndexScopeV1>,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReceiptV1, CodeGenerationRetentionErrorV1> {
    let liveness_proof = plan.liveness_proof.clone().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope receipt requires a canonical liveness proof".to_owned(),
        )
    })?;
    validate_scope_root_liveness_proof(&liveness_proof)?;
    let reclaimed_bytes = total_scope_bytes(&collected_scopes);
    let mut receipt = ScopeRootRetentionReceiptV1 {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA.to_owned(),
        receipt_digest: String::new(),
        live_scope_hashes: plan.live_scope_hashes.clone(),
        liveness_proof,
        minimum_stranding_age_secs: plan.minimum_stranding_age_secs,
        collected_scopes,
        reclaimed_bytes,
        completed_at_micros: completed_at.0,
    };
    receipt.receipt_digest = scope_receipt_digest(&receipt)?;
    Ok(receipt)
}

pub(super) fn scope_receipt_digest(
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    let material = ScopeReceiptMaterial {
        schema: SCOPE_RETENTION_RECEIPT_SCHEMA,
        live_scope_hashes: &receipt.live_scope_hashes,
        liveness_proof: &receipt.liveness_proof,
        minimum_stranding_age_secs: receipt.minimum_stranding_age_secs,
        collected_scopes: &receipt.collected_scopes,
        reclaimed_bytes: receipt.reclaimed_bytes,
        completed_at_micros: receipt.completed_at_micros,
    };
    let digest = canonical_sha256(&material)
        .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))?;
    receipt_digest_file_component(&SCOPE_RECEIPT_STORE, digest.as_str())
}

pub(super) fn binding_cleanup_receipt(
    plan: &ScopeRootRetentionPlanV1,
    scope_hash: &str,
    completed_at: UtcMicros,
) -> Result<ScopeRootRetentionReceiptV1, CodeGenerationRetentionErrorV1> {
    if plan.collectable_scopes.len() != 1 {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup requires a singleton collection plan".to_owned(),
        ));
    }
    let candidate = plan.collectable_scopes.first().ok_or_else(|| {
        CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup singleton plan has no candidate".to_owned(),
        )
    })?;
    if candidate.scope_hash != scope_hash {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup candidate does not match its collection plan".to_owned(),
        ));
    }
    build_scope_receipt(plan, vec![candidate.clone()], completed_at)
}

pub(super) fn validate_scope_receipt(
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if receipt.schema != SCOPE_RETENTION_RECEIPT_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt has an incompatible schema".to_owned(),
        ));
    }
    // Every writer emits lowercase digests, so mixed case is forgery or
    // corruption, never a legitimate receipt.
    if !is_lowercase_hex(&receipt.receipt_digest, 64) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest is not a SHA-256 file component".to_owned(),
        ));
    }
    if receipt.receipt_digest != scope_receipt_digest(receipt)? {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt digest does not match its contents".to_owned(),
        ));
    }
    if receipt.live_scope_hashes.is_empty() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction records an empty live-root set".to_owned(),
        ));
    }
    validate_scope_root_liveness_proof(&receipt.liveness_proof)?;
    if receipt.live_scope_hashes != receipt.liveness_proof.live_scope_hashes {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation receipt liveness set does not match its proof".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    for scope in &receipt.collected_scopes {
        if !is_code_index_scope_hash(&scope.scope_hash) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope reconciliation transaction names a non-scope directory".to_owned(),
            ));
        }
        if !seen.insert(scope.scope_hash.clone()) {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(
                "scope reconciliation transaction has duplicate scopes".to_owned(),
            ));
        }
    }
    if seen.is_empty()
        || !seen.is_disjoint(&receipt.live_scope_hashes)
        || receipt.reclaimed_bytes != total_scope_bytes(&receipt.collected_scopes)
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction violates liveness or byte invariants".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_scope_transaction(
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if transaction.schema != SCOPE_RETENTION_TRANSACTION_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction has an incompatible schema".to_owned(),
        ));
    }
    validate_scope_receipt(&transaction.receipt)?;
    let collected = transaction
        .receipt
        .collected_scopes
        .iter()
        .map(|scope| scope.scope_hash.as_str())
        .collect::<BTreeSet<_>>();
    let fenced = transaction
        .scope_identities
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if collected != fenced {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope reconciliation transaction does not fence every collected scope identity"
                .to_owned(),
        ));
    }
    // Every identity field defaults, so a row from before the durable file-id
    // fence deserializes rather than failing on a missing field. Name the
    // refusal here instead: an identity of `(0, 0)` proves nothing about the
    // directory, and recovery must not restore or unlink on timestamps alone.
    for (scope_hash, identity) in &transaction.scope_identities {
        if !identity.has_durable_file_id() {
            return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
                "scope reconciliation transaction carries no durable filesystem identity for \
                 '{scope_hash}'; the quarantine must be rebuilt from a fresh proof"
            )));
        }
    }
    Ok(())
}

pub(super) fn scope_root_liveness_proof_digest(
    proof: &ScopeRootLivenessProofV1,
) -> Result<String, CodeGenerationRetentionErrorV1> {
    canonical_sha256(&ScopeRootLivenessProofMaterialV1 {
        schema: SCOPE_ROOT_LIVENESS_PROOF_SCHEMA,
        live_scope_hashes: &proof.live_scope_hashes,
        registered_roots: &proof.registered_roots,
        git_worktrees: &proof.git_worktrees,
        mounted_leases: &proof.mounted_leases,
        configuration_roots: &proof.configuration_roots,
        vector_census: &proof.vector_census,
        vector_dependencies: &proof.vector_dependencies,
        candidate_binding: &proof.candidate_binding,
    })
    .map(|digest| digest.as_str().to_owned())
    .map_err(|error| CodeGenerationRetentionErrorV1::UnsafeState(error.to_string()))
}

pub(super) fn validate_scope_root_authority_receipt(
    name: &str,
    receipt: &ScopeRootAuthorityReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if receipt.revision.is_empty() || ManifestDigest::new(receipt.digest.clone()).is_err() {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "{name} liveness authority receipt has an invalid revision or digest"
        )));
    }
    Ok(())
}

pub(super) fn validate_scope_root_liveness_proof(
    proof: &ScopeRootLivenessProofV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if proof.schema != SCOPE_ROOT_LIVENESS_PROOF_SCHEMA
        || proof.live_scope_hashes.is_empty()
        || proof
            .live_scope_hashes
            .iter()
            .any(|scope_hash| !is_code_index_scope_hash(scope_hash))
        || !is_code_index_scope_hash(&proof.candidate_binding.scope_hash)
        || proof.candidate_binding.vector_census_revision != proof.vector_census.revision
        || (!proof.candidate_binding.live
            && proof
                .live_scope_hashes
                .contains(&proof.candidate_binding.scope_hash))
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness proof violates its structural authority contract".to_owned(),
        ));
    }
    for (name, receipt) in [
        ("registered-root", &proof.registered_roots),
        ("git-worktree", &proof.git_worktrees),
        ("mounted-lease", &proof.mounted_leases),
        ("configuration-root", &proof.configuration_roots),
        ("vector-census", &proof.vector_census),
        ("vector-dependency", &proof.vector_dependencies),
    ] {
        validate_scope_root_authority_receipt(name, receipt)?;
    }
    if proof.registered_roots.terminal_count == 0
        || proof.git_worktrees.terminal_count == 0
        || ManifestDigest::new(proof.proof_digest.clone()).is_err()
        || proof.proof_digest != scope_root_liveness_proof_digest(proof)?
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope liveness proof is incomplete or its digest does not match".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_scope_binding_cleanup_intent(
    intent: &ScopeRootBindingCleanupIntentV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    if intent.schema != SCOPE_BINDING_CLEANUP_INTENT_SCHEMA {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup intent has an incompatible schema".to_owned(),
        ));
    }
    if !is_code_index_scope_hash(&intent.scope_hash) {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup intent names a non-scope directory".to_owned(),
        ));
    }
    validate_scope_root_liveness_proof(&intent.liveness_proof)?;
    validate_scope_receipt(&intent.receipt)?;
    if intent.receipt.collected_scopes.len() != 1
        || intent.receipt.collected_scopes[0].scope_hash != intent.scope_hash
        || intent.liveness_proof.candidate_binding.scope_hash != intent.scope_hash
        || intent.liveness_proof.candidate_binding.source_scope != intent.source_scope
        || intent.liveness_proof.candidate_binding.live
        || intent.receipt.liveness_proof != intent.liveness_proof
    {
        return Err(CodeGenerationRetentionErrorV1::UnsafeState(
            "scope binding cleanup intent does not bind exactly one matching scope".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn persist_scope_transaction(
    store_root: &Path,
    transaction: &ScopeRootRetentionTransactionV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    persist_journal(store_root, &SCOPE_TRANSACTION_JOURNAL, transaction)
}

pub(super) fn load_scope_transaction(
    store_root: &Path,
) -> Result<Option<ScopeRootRetentionTransactionV1>, CodeGenerationRetentionErrorV1> {
    load_journal(store_root, &SCOPE_TRANSACTION_JOURNAL)
}

pub(super) fn clear_scope_transaction(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    clear_journal(store_root, &SCOPE_TRANSACTION_JOURNAL)
}

pub(super) fn persist_scope_binding_cleanup_intent(
    store_root: &Path,
    intent: &ScopeRootBindingCleanupIntentV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    persist_journal(store_root, &SCOPE_BINDING_CLEANUP_INTENT_JOURNAL, intent)
}

pub(super) fn load_scope_binding_cleanup_intent(
    store_root: &Path,
) -> Result<Option<ScopeRootBindingCleanupIntentV1>, CodeGenerationRetentionErrorV1> {
    load_journal(store_root, &SCOPE_BINDING_CLEANUP_INTENT_JOURNAL)
}

pub(super) fn clear_scope_binding_cleanup_intent(
    store_root: &Path,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    clear_journal(store_root, &SCOPE_BINDING_CLEANUP_INTENT_JOURNAL)
}

pub(super) fn scope_receipt_is_durable(
    store_root: &Path,
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<bool, CodeGenerationRetentionErrorV1> {
    receipt_store::receipt_is_durable(
        store_root,
        &SCOPE_RECEIPT_STORE,
        &receipt.receipt_digest,
        receipt,
    )
}

pub(super) fn write_scope_receipt(
    store_root: &Path,
    receipt: &ScopeRootRetentionReceiptV1,
) -> Result<(), CodeGenerationRetentionErrorV1> {
    receipt_store::write_receipt(
        store_root,
        &SCOPE_RECEIPT_STORE,
        &receipt.receipt_digest,
        receipt,
    )
}

pub(super) fn scope_directory_exists(path: &Path) -> Result<bool, CodeGenerationRetentionErrorV1> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(true),
        Ok(_) => Err(CodeGenerationRetentionErrorV1::UnsafeState(format!(
            "code-index scope path '{}' is not a directory",
            path.display()
        ))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(storage(error)),
    }
}
