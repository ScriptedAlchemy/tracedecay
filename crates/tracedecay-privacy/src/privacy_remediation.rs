//! At-rest privacy remediation after fail-closed project admission.
//!
//! Project-open spawns one bounded background rescan per adopted project
//! store after admission has finished; it never blocks admission or retrieval.
//! The caller passes the admitted project and its grant — not a composition-root
//! handle. Grant expiry, cancellation, and commit denial fail closed. The
//! rescan re-runs the current in-process detector over the persisted stores
//! the caller already opened. Project-memory detector hits are terminally
//! quarantined so historical payloads are erased; LCM raw messages settle
//! through their canonical remediation authority. Durable receipts record
//! every mutation, and no scanner binary runs.

use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use thiserror::Error;
use tracedecay_domain::{ProjectId, UtcMicros};
use tracedecay_store::{FactReadControl, FactWriteControl};

/// Project identity admitted by project-open before background remediation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedPrivacyProjectV1 {
    project_id: ProjectId,
    project_label: String,
}

impl AdmittedPrivacyProjectV1 {
    pub fn new(project_id: ProjectId, project_root: impl Display) -> Self {
        Self {
            project_id,
            project_label: project_root.to_string(),
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn project_label(&self) -> &str {
        &self.project_label
    }
}

/// Why at-rest remediation must not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum PrivacyRemediationDeniedV1 {
    #[error("privacy remediation grant is expired")]
    Expired,
    #[error("privacy remediation grant is cancelled")]
    Cancelled,
}

/// Grant authorizing one at-rest remediation of an admitted project.
///
/// Expiry and cancellation fail closed: spawn does not admit the task, and an
/// already-running pass stops before the next store authority is invoked.
#[derive(Clone, Debug)]
pub struct PrivacyRemediationGrantV1 {
    expires_at: UtcMicros,
    observed_at: UtcMicros,
    cancelled: Arc<AtomicBool>,
}

impl PrivacyRemediationGrantV1 {
    pub fn new(expires_at: UtcMicros, observed_at: UtcMicros) -> Self {
        Self {
            expires_at,
            observed_at,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn authorize(&self, observed_at: UtcMicros) -> Result<(), PrivacyRemediationDeniedV1> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(PrivacyRemediationDeniedV1::Cancelled);
        }
        if observed_at >= self.expires_at || self.observed_at >= self.expires_at {
            return Err(PrivacyRemediationDeniedV1::Expired);
        }
        Ok(())
    }
}

/// Truthful memory-rescan counts logged after an admitted pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivacyMemoryRemediationOutcomeV1 {
    pub detector_revision: String,
    pub superseded_payloads_scanned: u64,
    pub superseded_payloads_purged: u64,
    pub scanned_facts: u64,
    pub clean_facts: u64,
    pub quarantined_facts: u64,
    pub curation_batches: usize,
}

/// Truthful LCM-rescan outcome logged after an admitted pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrivacyLcmRemediationOutcomeV1 {
    AlreadyCurrent,
    Completed {
        detector_revision: String,
        scanned_rows: u64,
        clean_rows: u64,
        remediated_rows: u64,
        protected_rows: u64,
        unavailable_payload_rows: u64,
    },
}

/// Spawns the bounded background rescan for one admitted project store.
///
/// Returns `false` when the grant is already expired or cancelled, or when
/// the caller's spawn admission refuses the task. Project-open must not fail
/// on a refused spawn: remediation never blocks admission.
pub fn spawn_at_rest_privacy_remediation<Memory, Lcm, MemoryError, LcmError>(
    spawn: impl FnOnce(Pin<Box<dyn Future<Output = ()> + Send>>) -> bool,
    project: AdmittedPrivacyProjectV1,
    grant: PrivacyRemediationGrantV1,
    memory: Memory,
    lcm: Lcm,
    now: impl Fn() -> UtcMicros + Send + Sync + 'static,
) -> bool
where
    Memory:
        Future<Output = Result<PrivacyMemoryRemediationOutcomeV1, MemoryError>> + Send + 'static,
    Lcm: Future<Output = Result<PrivacyLcmRemediationOutcomeV1, LcmError>> + Send + 'static,
    MemoryError: Display + Send + 'static,
    LcmError: Display + Send + 'static,
{
    if grant.authorize(now()).is_err() {
        return false;
    }
    spawn(Box::pin(run_at_rest_privacy_remediation(
        project, grant, memory, lcm, now,
    )))
}

#[hotpath::measure(label = "daemon.privacy.remediate", future = true)]
pub async fn run_at_rest_privacy_remediation<Memory, Lcm, MemoryError, LcmError>(
    project: AdmittedPrivacyProjectV1,
    grant: PrivacyRemediationGrantV1,
    memory: Memory,
    lcm: Lcm,
    now: impl Fn() -> UtcMicros + Send + Sync + 'static,
) where
    Memory: Future<Output = Result<PrivacyMemoryRemediationOutcomeV1, MemoryError>>,
    Lcm: Future<Output = Result<PrivacyLcmRemediationOutcomeV1, LcmError>>,
    MemoryError: Display,
    LcmError: Display,
{
    let project = project.project_label();
    if let Err(error) = grant.authorize(now()) {
        tracing::warn!(
            event = "project_memory_privacy_remediation_failed",
            project = %project,
            %error,
        );
        return;
    }
    match memory.await {
        Ok(receipt) => {
            hotpath::gauge!("daemon.privacy.remediation.memory_completed_total").inc(1_u64);
            tracing::info!(
                event = "project_memory_privacy_remediation",
                project = %project,
                detector_revision = %receipt.detector_revision,
                superseded_payloads_scanned = receipt.superseded_payloads_scanned,
                superseded_payloads_purged = receipt.superseded_payloads_purged,
                scanned_facts = receipt.scanned_facts,
                clean_facts = receipt.clean_facts,
                quarantined_facts = receipt.quarantined_facts,
                curation_batches = receipt.curation_batches,
            );
        }
        Err(error) => {
            hotpath::gauge!("daemon.privacy.remediation.memory_failed_total").inc(1_u64);
            tracing::warn!(
                event = "project_memory_privacy_remediation_failed",
                project = %project,
                %error,
            );
        }
    }
    if let Err(error) = grant.authorize(now()) {
        tracing::warn!(
            event = "lcm_privacy_remediation_failed",
            project = %project,
            %error,
        );
        return;
    }
    match lcm.await {
        Ok(PrivacyLcmRemediationOutcomeV1::AlreadyCurrent) => {
            hotpath::gauge!("daemon.privacy.remediation.lcm_current_total").inc(1_u64);
        }
        Ok(PrivacyLcmRemediationOutcomeV1::Completed {
            detector_revision,
            scanned_rows,
            clean_rows,
            remediated_rows,
            protected_rows,
            unavailable_payload_rows,
        }) => {
            hotpath::gauge!("daemon.privacy.remediation.lcm_completed_total").inc(1_u64);
            tracing::info!(
                event = "lcm_privacy_remediation",
                project = %project,
                detector_revision = %detector_revision,
                scanned_rows,
                clean_rows,
                remediated_rows,
                protected_rows,
                unavailable_payload_rows,
            );
        }
        Err(error) => {
            hotpath::gauge!("daemon.privacy.remediation.lcm_failed_total").inc(1_u64);
            tracing::warn!(
                event = "lcm_privacy_remediation_failed",
                project = %project,
                %error,
            );
        }
    }
}

pub fn remediation_read_control() -> FactReadControl {
    FactReadControl::new(Arc::new(|| false))
}

/// Read control that fails closed when the grant expires or is cancelled.
pub fn granted_remediation_read_control(
    grant: &PrivacyRemediationGrantV1,
    now: impl Fn() -> UtcMicros + Send + Sync + 'static,
) -> FactReadControl {
    let grant = grant.clone();
    FactReadControl::new(Arc::new(move || grant.authorize(now()).is_err()))
}

/// The owner bounds every commit to one read page; the control admits each
/// canonical page receipt until that finite scan completes.
pub fn remediation_write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

/// Recheck expiry and cancellation at the canonical commit boundary.
pub fn granted_remediation_write_control(
    grant: &PrivacyRemediationGrantV1,
    now: impl Fn() -> UtcMicros + Send + Sync + Clone + 'static,
) -> FactWriteControl {
    let commit_grant = grant.clone();
    let read_control = granted_remediation_read_control(grant, now.clone());
    FactWriteControl::new(
        Arc::new(move || read_control.interrupted()),
        Arc::new(move || commit_grant.authorize(now()).is_ok()),
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tracedecay_domain::{
        ComponentVersion, Confidence, FactCategoryV1, FactOwnerV1, FactPayloadV1, ProjectId,
        ProvenanceId, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1, UtcMicros,
    };
    use tracedecay_session_memory::memory::{MemoryApplication, PrivacyRemediationTriggerV1};
    use tracedecay_store::{
        FactWriteControl, ProjectMemoryFactAddMaterialV1, ProjectMemoryFactIdV1,
        ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1, ProjectMemoryFactStore,
        ProjectMemoryFactUpdateCommandV1, ProjectMemoryFactUpdatePatchV1,
    };

    use super::{
        AdmittedPrivacyProjectV1, PrivacyLcmRemediationOutcomeV1,
        PrivacyMemoryRemediationOutcomeV1, PrivacyRemediationDeniedV1, PrivacyRemediationGrantV1,
        remediation_read_control, remediation_write_control, spawn_at_rest_privacy_remediation,
    };
    use tracedecay_daemon_identity::profile_identity;
    use tracedecay_session_memory::fact_store::DatabaseFactStore;
    use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

    fn secret() -> String {
        ["sk", "-test-", "1234567890abcdef"].concat()
    }

    #[test]
    fn expired_grant_fails_closed_without_spawning() {
        let project = AdmittedPrivacyProjectV1::new(
            ProjectId::new("project.privacy-grant-expired").expect("project id"),
            "/tmp/privacy-grant-expired",
        );
        let grant = PrivacyRemediationGrantV1::new(UtcMicros(10), UtcMicros(10));
        let spawned = spawn_at_rest_privacy_remediation(
            |_| panic!("expired grant must not spawn"),
            project,
            grant,
            async { Ok::<_, PrivacyRemediationDeniedV1>(memory_outcome()) },
            async {
                Ok::<_, PrivacyRemediationDeniedV1>(PrivacyLcmRemediationOutcomeV1::AlreadyCurrent)
            },
            || UtcMicros(1),
        );
        assert!(!spawned);
    }

    #[test]
    fn cancelled_grant_fails_closed_without_spawning() {
        let project = AdmittedPrivacyProjectV1::new(
            ProjectId::new("project.privacy-grant-cancelled").expect("project id"),
            "/tmp/privacy-grant-cancelled",
        );
        let grant = PrivacyRemediationGrantV1::new(UtcMicros(20), UtcMicros(1));
        grant.cancel();
        let spawned = spawn_at_rest_privacy_remediation(
            |_| panic!("cancelled grant must not spawn"),
            project,
            grant,
            async { Ok::<_, PrivacyRemediationDeniedV1>(memory_outcome()) },
            async {
                Ok::<_, PrivacyRemediationDeniedV1>(PrivacyLcmRemediationOutcomeV1::AlreadyCurrent)
            },
            || UtcMicros(1),
        );
        assert!(!spawned);
    }

    #[test]
    fn admitted_grant_hands_the_task_to_spawn() {
        let project = AdmittedPrivacyProjectV1::new(
            ProjectId::new("project.privacy-grant-admitted").expect("project id"),
            "/tmp/privacy-grant-admitted",
        );
        let grant = PrivacyRemediationGrantV1::new(UtcMicros(20), UtcMicros(1));
        let spawned = spawn_at_rest_privacy_remediation(
            |_| true,
            project,
            grant,
            async { Ok::<_, PrivacyRemediationDeniedV1>(memory_outcome()) },
            async {
                Ok::<_, PrivacyRemediationDeniedV1>(PrivacyLcmRemediationOutcomeV1::AlreadyCurrent)
            },
            || UtcMicros(1),
        );
        assert!(spawned);
    }

    #[tokio::test]
    async fn grant_expiring_during_memory_scan_withholds_lcm() {
        let now = Arc::new(std::sync::atomic::AtomicI64::new(1));
        let scan_now = Arc::clone(&now);
        let lcm_polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lcm_marker = Arc::clone(&lcm_polled);
        super::run_at_rest_privacy_remediation(
            AdmittedPrivacyProjectV1::new(
                ProjectId::new("project.privacy-expiry-transition").expect("project id"),
                "/tmp/privacy-expiry-transition",
            ),
            PrivacyRemediationGrantV1::new(UtcMicros(20), UtcMicros(1)),
            async move {
                scan_now.store(20, std::sync::atomic::Ordering::Release);
                Ok::<_, PrivacyRemediationDeniedV1>(memory_outcome())
            },
            async move {
                lcm_marker.store(true, std::sync::atomic::Ordering::Release);
                Ok::<_, PrivacyRemediationDeniedV1>(PrivacyLcmRemediationOutcomeV1::AlreadyCurrent)
            },
            move || UtcMicros(now.load(std::sync::atomic::Ordering::Acquire)),
        )
        .await;
        assert!(!lcm_polled.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn grant_controls_recheck_expiry_and_cancellation_at_commit() {
        let now = Arc::new(std::sync::atomic::AtomicI64::new(1));
        let clock = {
            let now = Arc::clone(&now);
            move || UtcMicros(now.load(std::sync::atomic::Ordering::Acquire))
        };
        let grant = PrivacyRemediationGrantV1::new(UtcMicros(20), UtcMicros(1));
        let read = super::granted_remediation_read_control(&grant, clock.clone());
        let write = super::granted_remediation_write_control(&grant, clock);
        assert!(!read.interrupted());
        assert!(write.try_begin_commit());
        now.store(20, std::sync::atomic::Ordering::Release);
        assert!(read.interrupted());
        assert!(write.interrupted());
        assert!(!write.try_begin_commit());
        now.store(1, std::sync::atomic::Ordering::Release);
        grant.cancel();
        assert!(read.interrupted());
        assert!(!write.try_begin_commit());
    }

    fn memory_outcome() -> PrivacyMemoryRemediationOutcomeV1 {
        PrivacyMemoryRemediationOutcomeV1 {
            detector_revision: "privacy.memory-fact.v1".to_owned(),
            superseded_payloads_scanned: 0,
            superseded_payloads_purged: 0,
            scanned_facts: 0,
            clean_facts: 0,
            quarantined_facts: 0,
            curation_batches: 0,
        }
    }

    fn enrolled_root(base: &Path, project_id: &ProjectId) -> PathBuf {
        let root = base.join(project_id.as_str());
        std::fs::create_dir_all(&root).expect("project root");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &root,
            project_id.as_str(),
        )
        .expect("project enrollment");
        root
    }

    /// The memory-fact receipt identity recipe, restated here as the reverse
    /// authority so the fixture can write exactly what an older binary (same
    /// pinned revision string, older vendored detector rules) wrote: a
    /// receipt-bound raw payload the current detector rules never evaluated.
    fn legacy_receipt_id(
        payload_reference: &tracedecay_domain::PayloadReferenceV1,
        sanitizer_version: &ComponentVersion,
        disposition: SanitizerDispositionV1,
        sensitivity: SensitivityV1,
    ) -> SanitizationReceiptId {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        for part in [
            b"tracedecay.privacy.memory-fact.receipt.v1\0".as_slice(),
            sanitizer_version.as_str().as_bytes(),
            disposition.as_str().as_bytes(),
            sensitivity.as_str().as_bytes(),
            payload_reference.digest().as_str().as_bytes(),
            &payload_reference.byte_len().to_be_bytes(),
        ] {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part);
        }
        SanitizationReceiptId::new(format!(
            "memory-fact-receipt.v1.{}",
            hex::encode(hasher.finalize())
        ))
        .expect("legacy receipt id")
    }

    /// Receipt-bound raw payload material exactly as an ingest path running
    /// an older vendored ruleset could have persisted it: the receipt binds
    /// the payload without the current detector rules ever evaluating it. The
    /// store's write firewall pins the sanitizer revision string, so the
    /// legacy condition being simulated is a ruleset refresh within the
    /// pinned revision.
    fn legacy_fact_material(
        owner: &FactOwnerV1,
        content: &str,
        source_label: Option<&str>,
        metadata: Value,
    ) -> ProjectMemoryFactAddMaterialV1 {
        let mut tags = Vec::new();
        let mut entities = Vec::new();
        let payload_reference = FactPayloadV1::canonicalize_material(
            content,
            FactCategoryV1::Project,
            &mut tags,
            &mut entities,
            &metadata,
            source_label,
        )
        .expect("legacy payload reference");
        let sanitizer_version = ComponentVersion::new(crate::MEMORY_FACT_SANITIZER_VERSION_V1)
            .expect("pinned detector revision");
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                legacy_receipt_id(
                    &payload_reference,
                    &sanitizer_version,
                    SanitizerDispositionV1::Accepted,
                    SensitivityV1::NonSensitive,
                ),
                sanitizer_version,
            )
            .expect("legacy receipt reference"),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(payload_reference),
        )
        .expect("legacy sanitization receipt");
        ProjectMemoryFactAddMaterialV1::new(
            owner.clone(),
            content.to_owned(),
            FactCategoryV1::Project,
            source_label.map(str::to_owned),
            tags,
            entities,
            metadata,
            receipt,
            None,
            Confidence::new(0.8).expect("legacy trust"),
            None,
        )
        .expect("legacy fact material")
    }

    async fn seed_legacy_fact(
        database: &tracedecay_runtime_core::db::Database,
        owner: &FactOwnerV1,
        label: &str,
        content: &str,
        source_label: Option<&str>,
        metadata: Value,
    ) -> tracedecay_store::ProjectMemoryFactAddOutcomeV1 {
        let command = legacy_fact_material(owner, content, source_label, metadata)
            .into_command(
                ProvenanceId::new(format!("operation.privacy-legacy.{label}"))
                    .expect("legacy operation id"),
            )
            .expect("legacy fact command");
        DatabaseFactStore::new(database)
            .add_project_memory_fact(command, &remediation_write_control())
            .await
            .expect("persist legacy fact")
    }

    async fn served_contents(
        memory: &MemoryApplication<DatabaseFactStore<'_>>,
        owner: &FactOwnerV1,
    ) -> Vec<String> {
        let page = memory
            .list_project_memory_facts(
                ProjectMemoryFactListQueryV1::new(owner.clone(), None, None, None, 64)
                    .expect("list query"),
                &remediation_read_control(),
            )
            .await
            .expect("list served facts");
        page.facts()
            .iter()
            .filter_map(|projection| match projection {
                ProjectMemoryFactProjectionV1::Available(fact) => Some(fact.content().to_owned()),
                ProjectMemoryFactProjectionV1::Unavailable(_) => None,
            })
            .collect()
    }

    async fn persisted_payload_rows_containing(
        database: &tracedecay_runtime_core::db::Database,
        marker: &str,
    ) -> i64 {
        database
            .query_scalar_i64_with_text(
                "inspect at-rest privacy remediation payloads",
                "SELECT COUNT(*) FROM memory_v2_assertion_payloads
                 WHERE payload_json LIKE '%' || ?1 || '%'
                    OR content LIKE '%' || ?1 || '%'",
                marker,
            )
            .await
            .expect("inspect persisted memory payloads")
    }

    async fn assertion_payload_purge_receipts(
        database: &tracedecay_runtime_core::db::Database,
    ) -> i64 {
        database
            .query_scalar_i64(
                "inspect at-rest privacy purge receipts",
                "SELECT COUNT(*) FROM memory_v2_assertion_payload_purges",
            )
            .await
            .expect("inspect persisted privacy purge receipts")
    }

    async fn orphaned_payload_fts_rows(database: &tracedecay_runtime_core::db::Database) -> i64 {
        database
            .query_scalar_i64(
                "inspect at-rest privacy FTS cleanup",
                "SELECT COUNT(*)
                 FROM memory_v2_assertion_payloads_fts AS fts
                 LEFT JOIN memory_v2_assertion_payloads AS payloads ON payloads.rowid = fts.rowid
                 WHERE payloads.rowid IS NULL",
            )
            .await
            .expect("inspect payload FTS cleanup")
    }

    struct LegacyPayloadRow {
        assertion_id: String,
        fact_id: String,
        owner_kind: String,
        project_id: String,
        payload_json: String,
        content: String,
    }

    async fn capture_payload_row(
        database: &tracedecay_runtime_core::db::Database,
        fact_id: &tracedecay_domain::FactId,
    ) -> LegacyPayloadRow {
        let mut rows = database
            .read_connection()
            .query(
                "SELECT assertion_id, fact_id, owner_kind, project_id, payload_json, content
                 FROM memory_v2_assertion_payloads WHERE fact_id = ?1",
                [fact_id.as_str()],
            )
            .await
            .expect("read legacy payload row");
        let row = rows
            .next()
            .await
            .expect("read legacy payload result")
            .expect("legacy payload row exists");
        LegacyPayloadRow {
            assertion_id: row.get(0).expect("assertion id"),
            fact_id: row.get(1).expect("fact id"),
            owner_kind: row.get(2).expect("owner kind"),
            project_id: row.get(3).expect("project id"),
            payload_json: row.get(4).expect("payload json"),
            content: row.get(5).expect("payload content"),
        }
    }

    /// Reconstructs the exact persisted shape an older binary left after it
    /// superseded a secret-bearing assertion without an explicit purge
    /// receipt. The final immutable trigger is restored before remediation.
    async fn restore_pre_purge_superseded_payload(
        database: &tracedecay_runtime_core::db::Database,
        payload: &LegacyPayloadRow,
    ) {
        let transaction = database
            .begin_write_transaction("restore pre-purge superseded payload fixture")
            .await
            .expect("database transaction");
        transaction
            .execute_batch(
                "DROP TRIGGER memory_v2_assertion_payload_purges_no_delete;
                 DELETE FROM memory_v2_assertion_payload_purges;
                 CREATE TRIGGER memory_v2_assertion_payload_purges_no_delete
                 BEFORE DELETE ON memory_v2_assertion_payload_purges BEGIN
                     SELECT RAISE(ABORT, 'memory_v2 assertion payload purge receipts are immutable');
                 END;",
            )
            .await
            .expect("restore pre-purge receipt shape");
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_payloads(
                    assertion_id, fact_id, owner_kind, project_id, payload_json, content
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                tracedecay_runtime_core::db::engine::params![
                    payload.assertion_id.as_str(),
                    payload.fact_id.as_str(),
                    payload.owner_kind.as_str(),
                    payload.project_id.as_str(),
                    payload.payload_json.as_str(),
                    payload.content.as_str(),
                ],
            )
            .await
            .expect("restore superseded payload from pre-purge binary");
        transaction
            .commit()
            .await
            .expect("commit pre-purge superseded payload fixture");
    }

    #[tokio::test]
    async fn at_rest_rescan_quarantines_and_erases_legacy_detector_hits() {
        let temp = TempDir::new().expect("privacy remediation fixture root");
        let profile_root = temp.path().join("profile");
        let project_id = ProjectId::new("project.privacy-remediation.fixture").expect("project id");
        let project_root = enrolled_root(temp.path(), &project_id);
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            43,
            "privacy remediation test",
        )
        .expect("daemon database scope");
        let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("daemon registry");
        let database = registry
            .project_memory(project_id.clone(), [project_root.clone()])
            .await
            .expect("project memory authority");
        let owner = FactOwnerV1::Project {
            project_id: project_id.clone(),
        };

        seed_legacy_fact(
            &database,
            &owner,
            "clean",
            "the retry budget is three attempts",
            None,
            json!({"fixture": "clean"}),
        )
        .await;
        seed_legacy_fact(
            &database,
            &owner,
            "redactable",
            &format!("deploys authenticate with the token {}", secret()),
            None,
            json!({"fixture": "redactable"}),
        )
        .await;
        seed_legacy_fact(
            &database,
            &owner,
            "quarantinable",
            "the staging credentials map is keyed by raw token",
            None,
            json!({ secret(): "staging" }),
        )
        .await;
        seed_legacy_fact(
            &database,
            &owner,
            "structured-source-label",
            "the deployment source is recorded",
            Some(r#"{"provider":{"vault_passphrase":"ordinary-value"}}"#),
            json!({"fixture": "structured-source-label"}),
        )
        .await;

        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database))
            .expect("owner-bound memory application");
        let receipt = memory
            .privacy_remediation_rescan(
                PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                &remediation_read_control(),
                &remediation_write_control(),
            )
            .await
            .expect("at-rest privacy rescan");

        assert_eq!(
            receipt.trigger,
            PrivacyRemediationTriggerV1::DetectorRevisionAdoption
        );
        assert_eq!(receipt.scanned_facts, 4);
        assert_eq!(receipt.clean_facts, 1);
        assert_eq!(receipt.quarantined_facts, 3);
        let curation = receipt
            .curation_receipts
            .first()
            .expect("remediation hits settle one durable curation receipt");
        assert_eq!(curation.facts_updated(), 0);
        assert_eq!(curation.facts_removed(), 3);

        // Detector-hit facts stopped being served entirely.
        let served = served_contents(&memory, &owner).await;
        assert_eq!(served.len(), 1, "quarantined facts must not serve");
        assert!(
            served.iter().all(|content| !content.contains(&secret())),
            "no served fact may retain the detector hit"
        );
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            0,
            "detector hits must be physically absent from every assertion payload row"
        );
        assert_eq!(
            persisted_payload_rows_containing(&database, "ordinary-value").await,
            0,
            "structured source-label findings must be erased from assertion payload rows"
        );

        // A second pass over the remediated store is clean and settles no
        // further mutation: the rescan is idempotent.
        let second = memory
            .privacy_remediation_rescan(
                PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                &remediation_read_control(),
                &remediation_write_control(),
            )
            .await
            .expect("idempotent rescan");
        assert_eq!(second.scanned_facts, 1);
        assert_eq!(second.clean_facts, 1);
        assert_eq!(second.quarantined_facts, 0);
        assert!(second.curation_receipts.is_empty());
    }

    #[tokio::test]
    async fn at_rest_rescan_commit_denial_fails_closed_without_mutation() {
        let temp = TempDir::new().expect("privacy remediation denial fixture root");
        let profile_root = temp.path().join("profile");
        let project_id = ProjectId::new("project.privacy-remediation.denial").expect("project id");
        let project_root = enrolled_root(temp.path(), &project_id);
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            44,
            "privacy remediation denial test",
        )
        .expect("daemon database scope");
        let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("daemon registry");
        let database = registry
            .project_memory(project_id.clone(), [project_root])
            .await
            .expect("project memory authority");
        let owner = FactOwnerV1::Project { project_id };

        seed_legacy_fact(
            &database,
            &owner,
            "content-hit",
            &format!("deploys authenticate with the token {}", secret()),
            None,
            json!({"fixture": "content-hit"}),
        )
        .await;
        seed_legacy_fact(
            &database,
            &owner,
            "metadata-hit",
            "the staging credentials map is keyed by raw token",
            None,
            json!({secret(): "staging"}),
        )
        .await;

        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database))
            .expect("owner-bound memory application");
        let before = served_contents(&memory, &owner).await;
        assert_eq!(before.len(), 2, "the denial fixture must serve both facts");
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            2,
            "the denial fixture must persist two detector-hit payload rows"
        );

        let denied = FactWriteControl::new(Arc::new(|| false), Arc::new(|| false));
        let refusal = memory
            .privacy_remediation_rescan(
                PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                &remediation_read_control(),
                &denied,
            )
            .await;
        assert!(
            refusal.is_err(),
            "a denied commit gate must fail the rescan closed, not settle silently"
        );

        assert_eq!(
            served_contents(&memory, &owner).await,
            before,
            "the refused curation batch must not change served facts"
        );
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            2,
            "the refused curation batch must not partially erase payload rows"
        );
        assert_eq!(
            assertion_payload_purge_receipts(&database).await,
            0,
            "a refused curation batch must not mint purge receipts"
        );

        let receipt = memory
            .privacy_remediation_rescan(
                PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                &remediation_read_control(),
                &remediation_write_control(),
            )
            .await
            .expect("admitted retry after commit refusal");
        assert_eq!(receipt.scanned_facts, 2);
        assert_eq!(receipt.clean_facts, 0);
        assert_eq!(receipt.quarantined_facts, 2);
        assert_eq!(
            receipt
                .curation_receipts
                .iter()
                .map(tracedecay_store::ProjectMemoryFactCurationReceiptV1::facts_removed)
                .sum::<u64>(),
            2
        );
        assert!(served_contents(&memory, &owner).await.is_empty());
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            0,
            "the admitted retry must erase every detector-hit payload"
        );
    }

    #[tokio::test]
    async fn at_rest_rescan_purges_detector_flagged_history_already_superseded_by_clean_content() {
        let temp = TempDir::new().expect("privacy remediation fixture root");
        let profile_root = temp.path().join("profile");
        let project_id =
            ProjectId::new("project.privacy-remediation-superseded").expect("project id");
        let project_root = enrolled_root(temp.path(), &project_id);
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            43,
            "superseded privacy remediation test",
        )
        .expect("daemon database scope");
        let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("daemon registry");
        let database = registry
            .project_memory(project_id.clone(), [project_root])
            .await
            .expect("project memory authority");
        let owner = FactOwnerV1::Project { project_id };

        let added = seed_legacy_fact(
            &database,
            &owner,
            "superseded-dirty",
            &format!("deployment credential is {}", secret()),
            None,
            json!({"fixture": "superseded-dirty"}),
        )
        .await;
        let legacy_payload = capture_payload_row(&database, added.fact().fact_id()).await;
        let target = ProjectMemoryFactIdV1::new(owner.clone(), added.fact().fact_id().clone())
            .expect("owner-bound legacy fact");
        DatabaseFactStore::new(&database)
            .update_project_memory_fact(
                ProjectMemoryFactUpdateCommandV1::new(
                    target,
                    ProvenanceId::new("operation.privacy-clean-correction")
                        .expect("correction operation id"),
                    None,
                    ProjectMemoryFactUpdatePatchV1::new(
                        Some("deployment authentication uses the managed vault".to_owned()),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                    .expect("clean correction patch"),
                    None,
                )
                .expect("clean correction command"),
                &remediation_write_control(),
            )
            .await
            .expect("supersede legacy secret with clean content");

        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            0,
            "the canonical correction boundary must purge the detector-flagged predecessor"
        );
        assert_eq!(assertion_payload_purge_receipts(&database).await, 1);
        assert_eq!(orphaned_payload_fts_rows(&database).await, 0);

        restore_pre_purge_superseded_payload(&database, &legacy_payload).await;
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            1,
            "the rollout fixture must contain one pre-existing superseded secret"
        );
        assert_eq!(assertion_payload_purge_receipts(&database).await, 0);

        let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&database))
            .expect("owner-bound memory application");
        let receipt = memory
            .privacy_remediation_rescan(
                PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                &remediation_read_control(),
                &remediation_write_control(),
            )
            .await
            .expect("at-rest privacy rescan");
        assert_eq!(receipt.superseded_payloads_scanned, 1);
        assert_eq!(receipt.superseded_payloads_purged, 1);
        assert_eq!(receipt.scanned_facts, 1);
        assert_eq!(receipt.clean_facts, 1);
        assert_eq!(receipt.quarantined_facts, 0);
        assert_eq!(served_contents(&memory, &owner).await.len(), 1);
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            0
        );
        assert_eq!(assertion_payload_purge_receipts(&database).await, 1);
        assert_eq!(orphaned_payload_fts_rows(&database).await, 0);
    }

    #[tokio::test]
    async fn remediation_commits_more_than_one_curation_batch_without_leaving_secret_bytes() {
        let home = TempDir::new().expect("isolated home");
        let profile_root = home.path().join("profile");
        let project_id = ProjectId::new("project.privacy-remediation-many").expect("project id");
        let project_root = enrolled_root(home.path(), &project_id);
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            43,
            "privacy remediation batch test",
        )
        .expect("daemon database scope");
        let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
        let registry = DaemonSessionRuntimeRegistryV1::open(identity)
            .await
            .expect("daemon registry");
        let database = registry
            .project_memory(project_id.clone(), [project_root])
            .await
            .expect("project memory authority");
        let owner = FactOwnerV1::Project {
            project_id: project_id.clone(),
        };

        // Per-write graph publication makes 257 sequential adds quadratic in
        // store size (and past the CI slow-timeout ceiling), so the bulk is
        // seeded through one store-level curation batch: one commit for 256
        // dirty facts, one ordinary add for the 257th. The clean anchor fact
        // supplies the reviewed evidence reference every curation add
        // requires.
        let anchor = seed_legacy_fact(
            &database,
            &owner,
            "anchor",
            "the retry budget is three attempts",
            None,
            json!({"fixture": "anchor"}),
        )
        .await;
        let ProjectMemoryFactProjectionV1::Available(anchor) = anchor.fact() else {
            panic!("the anchor fact must be served");
        };
        let seed_confidence = Confidence::new(0.9).expect("seed confidence");
        let outer_operation_id = ProvenanceId::new("operation.privacy-legacy.batch-seed")
            .expect("seed batch operation id");
        let operations = (0..256_usize)
            .map(|index| {
                let child_operation_id =
                    tracedecay_store::derive_project_memory_fact_curation_child_operation_id(
                        &outer_operation_id,
                        index,
                        tracedecay_store::ProjectMemoryFactCurationMutationKindV1::Add,
                    )
                    .expect("seed child operation id");
                let command = legacy_fact_material(
                    &owner,
                    &format!("credential {index} is {}", secret()),
                    None,
                    json!({"fixture": "many-dirty", "index": index}),
                )
                .into_command(child_operation_id)
                .expect("seed add command");
                let evidence = tracedecay_store::ProjectMemoryFactCurationEvidenceV1::new(
                    &owner,
                    vec![tracedecay_store::ProjectMemoryFactCurationReviewRefV1::new(
                        tracedecay_store::ProjectMemoryFactIdV1::new(
                            owner.clone(),
                            anchor.fact_id().clone(),
                        )
                        .expect("anchor fact identity"),
                        anchor.last_event_id().clone(),
                    )],
                    seed_confidence,
                    "legacy privacy fixture seed".to_owned(),
                )
                .expect("seed evidence");
                tracedecay_store::ProjectMemoryFactCurationOperationV1::Add(
                    tracedecay_store::ProjectMemoryFactCurationAddV1::new(command, evidence)
                        .expect("seed curation add"),
                )
            })
            .collect::<Vec<_>>();
        let seed_batch = tracedecay_store::ProjectMemoryFactCurationBatchV1::new(
            owner.clone(),
            outer_operation_id,
            None,
            seed_confidence,
            operations,
        )
        .expect("seed curation batch");
        DatabaseFactStore::new(&database)
            .apply_project_memory_fact_curation(seed_batch, &remediation_write_control())
            .await
            .expect("persist bulk legacy facts");
        seed_legacy_fact(
            &database,
            &owner,
            "dirty-tail",
            &format!("credential tail is {}", secret()),
            None,
            json!({"fixture": "many-dirty", "index": "tail"}),
        )
        .await;

        let memory = MemoryApplication::new(owner, DatabaseFactStore::new(&database))
            .expect("owner-bound memory application");
        let receipt = memory
            .privacy_remediation_rescan(
                PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
                &remediation_read_control(),
                &remediation_write_control(),
            )
            .await
            .expect("every bounded remediation batch commits");

        assert_eq!(receipt.scanned_facts, 258);
        assert_eq!(receipt.clean_facts, 1, "only the anchor fact is clean");
        assert_eq!(receipt.quarantined_facts, 257);
        assert_eq!(receipt.curation_receipts.len(), 5);
        assert_eq!(
            receipt
                .curation_receipts
                .iter()
                .map(tracedecay_store::ProjectMemoryFactCurationReceiptV1::facts_removed)
                .sum::<u64>(),
            257
        );
        assert_eq!(
            persisted_payload_rows_containing(&database, &secret()).await,
            0,
            "no batch may leave secret-bearing assertion payloads behind"
        );
    }
}
