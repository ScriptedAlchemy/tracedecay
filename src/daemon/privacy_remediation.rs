//! Daemon-owned at-rest privacy remediation.
//!
//! Project-open spawns one bounded background rescan per adopted project
//! store after fail-closed admission has finished; it never blocks admission
//! or retrieval. The rescan re-runs the current in-process detector over the
//! persisted stores it owns. Project-memory detector hits are terminally
//! quarantined so historical payloads are erased; LCM raw messages settle
//! through their canonical remediation authority. Durable receipts record
//! every mutation, and no scanner binary runs.

use std::sync::Arc;

use tracedecay_store::{FactReadControl, FactWriteControl};
use tracedecay_usecases::memory::{
    PrivacyRemediationTriggerV1, ProjectMemoryPrivacyRemediationReceiptV1,
};

use crate::errors::Result;
use crate::global_db::{LcmPrivacyRescanOutcomeV1, RegisteredGlobalDbLeaseV1};
use crate::tracedecay::TraceDecay;

/// Spawns the bounded background rescan for one adopted project store.
pub(crate) fn spawn_at_rest_privacy_remediation(
    graph: Arc<TraceDecay>,
    session_db: RegisteredGlobalDbLeaseV1,
) {
    tokio::spawn(async move {
        let project = graph.project_root().display().to_string();
        match run_project_memory_privacy_remediation(&graph).await {
            Ok(receipt) => {
                tracing::info!(
                    event = "project_memory_privacy_remediation",
                    project = %project,
                    detector_revision = %receipt.detector_revision,
                    scanned_facts = receipt.scanned_facts,
                    clean_facts = receipt.clean_facts,
                    quarantined_facts = receipt.quarantined_facts,
                    curation_batches = receipt.curation_receipts.len(),
                );
            }
            Err(error) => {
                tracing::warn!(
                    event = "project_memory_privacy_remediation_failed",
                    project = %project,
                    %error,
                );
            }
        }
        match session_db.lcm_privacy_rescan_raw_messages().await {
            Ok(LcmPrivacyRescanOutcomeV1::AlreadyCurrent) => {}
            Ok(LcmPrivacyRescanOutcomeV1::Completed(receipt)) => {
                tracing::info!(
                    event = "lcm_privacy_remediation",
                    project = %project,
                    detector_revision = %receipt.detector_revision,
                    scanned_rows = receipt.scanned_rows,
                    clean_rows = receipt.clean_rows,
                    remediated_rows = receipt.remediated_rows,
                    protected_rows = receipt.protected_rows,
                    unavailable_payload_rows = receipt.unavailable_payload_rows,
                );
            }
            Err(error) => {
                tracing::warn!(
                    event = "lcm_privacy_remediation_failed",
                    project = %project,
                    %error,
                );
            }
        }
    });
}

async fn run_project_memory_privacy_remediation(
    graph: &TraceDecay,
) -> Result<ProjectMemoryPrivacyRemediationReceiptV1> {
    let memory = graph.project_memory_application().await?;
    memory
        .privacy_remediation_rescan(
            PrivacyRemediationTriggerV1::DetectorRevisionAdoption,
            &remediation_read_control(),
            &remediation_write_control(),
        )
        .await
        .map_err(tracedecay_usecases::memory::memory_application_error)
}

fn remediation_read_control() -> FactReadControl {
    FactReadControl::new(Arc::new(|| false))
}

/// The owner bounds every commit to one read page; the control admits each
/// canonical page receipt until that finite scan completes.
fn remediation_write_control() -> FactWriteControl {
    FactWriteControl::new(Arc::new(|| false), Arc::new(|| true))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tracedecay_domain::{
        ComponentVersion, Confidence, FactCategoryV1, FactOwnerV1, FactPayloadV1, ProjectId,
        ProvenanceId, SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
        SanitizerDispositionV1, SensitivityV1,
    };
    use tracedecay_store::{
        ProjectMemoryFactAddMaterialV1, ProjectMemoryFactListQueryV1,
        ProjectMemoryFactProjectionV1, ProjectMemoryFactStore,
    };
    use tracedecay_usecases::memory::{MemoryApplication, PrivacyRemediationTriggerV1};

    use super::{remediation_read_control, remediation_write_control};
    use crate::daemon::profile_identity;
    use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
    use crate::store::DatabaseFactStore;

    fn secret() -> String {
        ["sk", "-test-", "1234567890abcdef"].concat()
    }

    fn enrolled_root(base: &Path, project_id: &ProjectId) -> PathBuf {
        let root = base.join(project_id.as_str());
        std::fs::create_dir_all(&root).expect("project root");
        crate::storage::pin_fixture_repository_identity(&root, project_id.as_str())
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
        let sanitizer_version = ComponentVersion::new(
            tracedecay_runtime_core::privacy::MEMORY_FACT_SANITIZER_VERSION_V1,
        )
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
        database: &crate::db::Database,
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
        database: &crate::db::Database,
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

    #[tokio::test]
    async fn at_rest_rescan_quarantines_and_erases_legacy_detector_hits() {
        let temp = TempDir::new().expect("privacy remediation fixture root");
        let profile_root = temp.path().join("profile");
        let project_id = ProjectId::new("project.privacy-remediation.fixture").expect("project id");
        let project_root = enrolled_root(temp.path(), &project_id);
        let _database_scope =
            crate::db::enter_daemon_database_scope(&profile_root, 43, "privacy remediation test")
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
    async fn remediation_commits_more_than_one_curation_batch_without_leaving_secret_bytes() {
        let home = TempDir::new().expect("isolated home");
        let profile_root = home.path().join("profile");
        let project_id = ProjectId::new("project.privacy-remediation-many").expect("project id");
        let project_root = enrolled_root(home.path(), &project_id);
        let _database_scope = crate::db::enter_daemon_database_scope(
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
