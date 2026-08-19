//! Daemon-owned at-rest privacy remediation.
//!
//! Project-open spawns one bounded background rescan per adopted project
//! store after fail-closed admission has finished; it never blocks admission
//! or retrieval. The rescan re-runs the current in-process detector over
//! persisted project-memory facts, redacts what the detector can make safe,
//! quarantines what it cannot, and settles every mutation through the
//! canonical curation authority so a durable curation receipt records what
//! changed. No scanner binary runs and no unsanitized payload is persisted.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_store::{FactReadControl, FactWriteControl};
use tracedecay_usecases::memory::{
    PrivacyRemediationTriggerV1, ProjectMemoryPrivacyRemediationReceiptV1,
};

use crate::errors::Result;
use crate::tracedecay::TraceDecay;

/// Spawns the bounded background rescan for one adopted project store.
pub(crate) fn spawn_project_memory_privacy_remediation(graph: Arc<TraceDecay>) {
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
                    redacted_facts = receipt.redacted_facts,
                    quarantined_facts = receipt.quarantined_facts,
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

/// One-shot commit gate: the rescan settles exactly one curation batch, and a
/// second commit attempt under the same control is refused.
fn remediation_write_control() -> FactWriteControl {
    let granted = Arc::new(AtomicBool::new(false));
    FactWriteControl::new(
        Arc::new(|| false),
        Arc::new(move || {
            granted
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }),
    )
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

    /// Persists one fact exactly as an ingest path running an older vendored
    /// ruleset could have: the receipt binds the raw payload without the
    /// current detector rules ever evaluating it. The store's write firewall
    /// pins the sanitizer revision string, so the legacy condition being
    /// simulated is a ruleset refresh within the pinned revision.
    async fn seed_legacy_fact(
        database: &crate::db::Database,
        owner: &FactOwnerV1,
        label: &str,
        content: &str,
        metadata: Value,
    ) {
        let mut tags = Vec::new();
        let mut entities = Vec::new();
        let payload_reference = FactPayloadV1::canonicalize_material(
            content,
            FactCategoryV1::Project,
            &mut tags,
            &mut entities,
            &metadata,
            None,
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
        let command = ProjectMemoryFactAddMaterialV1::new(
            owner.clone(),
            content.to_owned(),
            FactCategoryV1::Project,
            None,
            tags,
            entities,
            metadata,
            receipt,
            None,
            Confidence::new(0.8).expect("legacy trust"),
            None,
        )
        .expect("legacy fact material")
        .into_command(
            ProvenanceId::new(format!("operation.privacy-legacy.{label}"))
                .expect("legacy operation id"),
        )
        .expect("legacy fact command");
        DatabaseFactStore::new(database)
            .add_project_memory_fact(command, &remediation_write_control())
            .await
            .expect("persist legacy fact");
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

    #[tokio::test]
    async fn at_rest_rescan_quarantines_and_redacts_legacy_detector_hits() {
        let temp = TempDir::new().expect("privacy remediation fixture root");
        let profile_root = temp.path().join("profile");
        let project_id =
            ProjectId::new("project.privacy-remediation.fixture").expect("project id");
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
            json!({"fixture": "clean"}),
        )
        .await;
        seed_legacy_fact(
            &database,
            &owner,
            "redactable",
            &format!("deploys authenticate with the token {}", secret()),
            json!({"fixture": "redactable"}),
        )
        .await;
        seed_legacy_fact(
            &database,
            &owner,
            "quarantinable",
            "the staging credentials map is keyed by raw token",
            json!({ secret(): "staging" }),
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
        assert_eq!(receipt.scanned_facts, 3);
        assert_eq!(receipt.clean_facts, 1);
        assert_eq!(receipt.redacted_facts, 1);
        assert_eq!(receipt.quarantined_facts, 1);
        let curation = receipt
            .curation_receipt
            .as_ref()
            .expect("remediation hits settle one durable curation receipt");
        assert_eq!(curation.facts_updated(), 1);
        assert_eq!(curation.facts_removed(), 1);

        // Served content no longer carries the secret anywhere, and the
        // quarantined fact stopped being served entirely.
        let served = served_contents(&memory, &owner).await;
        assert_eq!(served.len(), 2, "the quarantined fact must not serve");
        assert!(
            served.iter().all(|content| !content.contains(&secret())),
            "no served fact may retain the detector hit"
        );
        assert!(
            served
                .iter()
                .any(|content| content.contains("deploys authenticate with the token")),
            "the redactable fact must stay served with sanitized content"
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
        assert_eq!(second.scanned_facts, 2);
        assert_eq!(second.clean_facts, 2);
        assert_eq!(second.redacted_facts, 0);
        assert_eq!(second.quarantined_facts, 0);
        assert!(second.curation_receipt.is_none());
    }
}
