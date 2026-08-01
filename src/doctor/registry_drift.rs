use std::path::{Path, PathBuf};

use tracedecay_application::doctor::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1,
};

use crate::retention::orphan_stores::{
    OrphanStoreFinding, StoreDisposition, UnregisteredStoreFinding, UnverifiableReason,
};

/// Owning operation Doctor names for collecting a dead-identity orphan store.
const ORPHAN_COLLECT_OP: &str = "retention.orphan_store_sweep";
/// Owning operation Doctor names for re-linking a moved-repository store —
/// this reconciliation path (`doctor::registry_drift`), per Plan 38 §2.
const ORPHAN_RELINK_OP: &str = "doctor.registry_drift.relink";
/// Remediation for a store whose liveness could not be determined: inspect the
/// identity, never collect it.
const ORPHAN_INSPECT_OP: &str = "doctor.registry_drift.inspect";
/// Owning operation Doctor name for collecting a directory with no registry
/// trace at all (plan 38 §2's disjoint "unregistered store" audit class).
const UNREGISTERED_COLLECT_OP: &str = "retention.unregistered_store_sweep";
/// Coverage/evidence identifier byte ceiling shared by the kernel constructors.
const DOCTOR_TEXT_LIMIT: usize = 512;

/// Clamps a human coverage statement to the kernel's bounds: trimmed, free of
/// control characters, and within the identifier byte ceiling, so construction
/// never fails on an over-long store path.
fn bounded_statement(statement: &str) -> String {
    let cleaned: String = statement
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.len() <= DOCTOR_TEXT_LIMIT {
        return cleaned.to_string();
    }
    // Truncate on a char boundary at or below the ceiling.
    let mut end = DOCTOR_TEXT_LIMIT;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].trim().to_string()
}

/// Maps one classified orphan-store finding (Plan 38 §2) onto the typed Doctor
/// [`DoctorStorageFindingV1`] the application kernel defines. `Live` stores are
/// never a retention concern and yield `None`; orphaned and re-linkable stores
/// both surface as [`DoctorStorageFindingKindV1::OrphanStore`], each carrying
/// the remediation that fits its disposition (collect vs re-link). Returns
/// `None` when the store identity cannot form a valid evidence reference.
pub(crate) fn orphan_store_doctor_finding(
    finding: &OrphanStoreFinding,
) -> Option<DoctorStorageFindingV1> {
    let (state, remediation_op, statement) = match &finding.disposition {
        StoreDisposition::Live => return None,
        // Liveness was never proven either way, so this must not carry the
        // collect remediation — the sweep would delete a possibly-live store.
        StoreDisposition::Unverifiable { reason } => (
            DoctorEvidenceStateV1::Unknown,
            ORPHAN_INSPECT_OP,
            format!(
                "store '{}' (project '{}') has unverifiable liveness ({}): {} bytes, not collectable",
                finding.store_id,
                finding.project_id,
                match reason {
                    UnverifiableReason::RootInspectionFailed => "a root could not be inspected",
                    UnverifiableReason::ManifestUnreadable =>
                        "the store manifest is missing or malformed",
                },
                finding.size_bytes
            ),
        ),
        StoreDisposition::Orphaned => (
            DoctorEvidenceStateV1::Degraded,
            ORPHAN_COLLECT_OP,
            format!(
                "orphan store '{}' (project '{}') has no live root: {} bytes, idle {}s",
                finding.store_id, finding.project_id, finding.size_bytes, finding.age_secs
            ),
        ),
        StoreDisposition::Relinkable { live_root } => (
            DoctorEvidenceStateV1::Stale,
            ORPHAN_RELINK_OP,
            format!(
                "store '{}' (project '{}') is re-linkable to live root '{}': {} bytes",
                finding.store_id,
                finding.project_id,
                live_root.display(),
                finding.size_bytes
            ),
        ),
    };
    let reference = DoctorEvidenceReferenceV1::new(finding.store_id.clone()).ok()?;
    let evidence = DoctorEvidenceRefV1::new(DoctorFindingFamilyV1::Storage, reference);
    let completeness = if matches!(finding.disposition, StoreDisposition::Unverifiable { .. }) {
        DoctorCoverageCompletenessV1::Unknown
    } else {
        DoctorCoverageCompletenessV1::Complete
    };
    let coverage =
        DoctorCoverageStatementV1::new(completeness, bounded_statement(&statement)).ok()?;
    let remediation = DoctorRemediationRefV1::new(
        DoctorOwningOperationRefV1::new(remediation_op).ok()?,
        DoctorRemediationKindV1::Action,
    );
    let core = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence],
        coverage,
        Some(remediation),
    )
    .ok()?;
    DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, core).ok()
}

/// Maps one unregistered-store-directory finding (plan 38 §2's disjoint
/// on-disk-only audit class) onto the typed Doctor [`DoctorStorageFindingV1`].
/// Reported under the same [`DoctorStorageFindingKindV1::OrphanStore`] kind as
/// [`orphan_store_doctor_finding`] — both describe payload the registry no
/// longer resolves to a live root — the evidence text distinguishes the two:
/// this class never had a registry row to begin with, rather than one whose
/// root vanished. Returns `None` only when the identifier cannot form a valid
/// evidence reference.
pub(crate) fn unregistered_store_doctor_finding(
    finding: &UnregisteredStoreFinding,
) -> Option<DoctorStorageFindingV1> {
    let statement = format!(
        "unregistered store directory '{}' has no registry row at all: {} bytes, idle {}s",
        finding.project_dir_name, finding.size_bytes, finding.age_secs
    );
    let reference = DoctorEvidenceReferenceV1::new(finding.project_dir_name.clone()).ok()?;
    let evidence = DoctorEvidenceRefV1::new(DoctorFindingFamilyV1::Storage, reference);
    let coverage = DoctorCoverageStatementV1::new(
        DoctorCoverageCompletenessV1::Complete,
        bounded_statement(&statement),
    )
    .ok()?;
    let remediation = DoctorRemediationRefV1::new(
        DoctorOwningOperationRefV1::new(UNREGISTERED_COLLECT_OP).ok()?,
        DoctorRemediationKindV1::Action,
    );
    let core = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceStateV1::Degraded,
        vec![evidence],
        coverage,
        Some(remediation),
    )
    .ok()?;
    DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, core).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RegistryDriftFinding {
    pub(super) project_id: String,
    pub(super) store_id: String,
    pub(super) field: &'static str,
    pub(super) registry_value: String,
    pub(super) manifest_value: String,
    pub(super) manifest_path: PathBuf,
    manifest: crate::storage::StoreManifest,
    registry_path: Option<PathBuf>,
}

pub(super) async fn registry_drift_findings(
    global_db: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
) -> Vec<RegistryDriftFinding> {
    let mut findings = Vec::new();
    let Ok(projects) = global_db.list_code_projects(usize::MAX).await else {
        return findings;
    };
    for project in projects {
        let Ok(Some(context)) = global_db
            .project_registry_context_by_id(&project.project_id)
            .await
        else {
            continue;
        };
        for store_context in context.stores {
            let store = store_context.store;
            let Some(manifest_path) = resolve_registry_manifest_path(profile_root, &store) else {
                continue;
            };
            let Ok(manifest) = crate::storage::read_store_manifest(&manifest_path) else {
                continue;
            };
            let manifest_project_id = manifest
                .project_id
                .as_deref()
                .unwrap_or("<missing>")
                .to_string();
            if manifest_project_id != store.project_id {
                findings.push(RegistryDriftFinding {
                    project_id: project.project_id.clone(),
                    store_id: store.store_id.clone(),
                    field: "project_id",
                    registry_value: store.project_id.clone(),
                    manifest_value: manifest_project_id,
                    manifest_path: manifest_path.clone(),
                    manifest: manifest.clone(),
                    registry_path: None,
                });
            }

            let registry_project_root = PathBuf::from(&project.canonical_root);
            let manifest_project_root = manifest.project_root.clone();
            if registry_project_root != manifest_project_root {
                findings.push(RegistryDriftFinding {
                    project_id: project.project_id.clone(),
                    store_id: store.store_id.clone(),
                    field: "project_root",
                    registry_value: project.canonical_root.clone(),
                    manifest_value: manifest_project_root.display().to_string(),
                    manifest_path: manifest_path.clone(),
                    manifest: manifest.clone(),
                    registry_path: Some(registry_project_root.clone()),
                });
            }
        }
    }
    findings
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledStoreRoot {
    pub store_id: String,
    pub manifest_path: PathBuf,
    pub config_path: Option<PathBuf>,
}

#[cfg(test)]
async fn reconcile_drifted_store_roots(
    global_db: &crate::global_db::RegisteredGlobalDb,
    profile_root: &Path,
) -> (Vec<ReconciledStoreRoot>, Vec<String>) {
    let findings = registry_drift_findings(global_db, profile_root).await;
    reconcile_drifted_store_roots_from_findings(&findings)
}

pub(super) fn reconcile_drifted_store_roots_from_findings(
    findings: &[RegistryDriftFinding],
) -> (Vec<ReconciledStoreRoot>, Vec<String>) {
    let mut reconciled = Vec::new();
    let mut warnings = Vec::new();

    for finding in findings {
        if finding.field != "project_root" {
            continue;
        }
        let Some(canonical) = finding.registry_path.as_deref() else {
            continue;
        };
        if !canonical.exists() {
            continue;
        }

        match reconcile_one_store_root(finding, canonical) {
            Ok(Some(entry)) => reconciled.push(entry),
            Ok(None) => {}
            Err(message) => warnings.push(message),
        }
    }

    (reconciled, warnings)
}

/// Rewrites one drifted manifest onto its canonical root. Returns `None` when
/// the manifest already agrees with the registry and nothing was written, so a
/// reconciliation entry is reported only for a root this pass actually moved.
fn reconcile_one_store_root(
    finding: &RegistryDriftFinding,
    canonical: &Path,
) -> std::result::Result<Option<ReconciledStoreRoot>, String> {
    if finding.manifest.project_root == canonical {
        return Ok(None);
    }

    let mut manifest = finding.manifest.clone();
    manifest.project_root = canonical.to_path_buf();
    crate::storage::write_store_manifest_to_path(&finding.manifest_path, &manifest).map_err(|e| {
        format!(
            "could not rewrite store manifest '{}': {e}",
            finding.manifest_path.display()
        )
    })?;

    Ok(Some(ReconciledStoreRoot {
        store_id: finding.store_id.clone(),
        manifest_path: finding.manifest_path.clone(),
        // `config.json` is read-only legacy migration input. Its root
        // metadata is neither drift authority nor a repair target.
        config_path: None,
    }))
}

fn resolve_registry_manifest_path(
    profile_root: &Path,
    store: &crate::global_db::StoreInstanceRecord,
) -> Option<PathBuf> {
    if store.storage_mode != "profile_sharded" {
        return None;
    }
    let store_relpath = super::registry_relpath(&store.store_relpath);
    let manifest_relpath = store
        .manifest_relpath
        .as_ref()
        .map(|relpath| super::registry_relpath(relpath));
    for profile_root in super::registry_profile_roots(profile_root) {
        let Ok(data_root) =
            crate::storage::StoreArtifactPath::resolve(&profile_root, &store_relpath)
        else {
            continue;
        };
        let data_root = data_root.absolute_path();
        if let Some(relpath) = manifest_relpath.as_ref() {
            for root in [&profile_root, &data_root] {
                let Ok(path) = crate::storage::StoreArtifactPath::resolve(root, relpath) else {
                    continue;
                };
                let path = path.absolute_path();
                if path.is_file() {
                    return Some(path);
                }
            }
        } else {
            let path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::global_db::StoreInstanceUpsert;
    use crate::storage::{STORE_MANIFEST_SCHEMA_VERSION, StorageMode, StoreKind, StoreManifest};

    const STORE_ID: &str = "store_reconcile_test";
    const PROJECT_ID: &str = "proj_reconcile_test";

    fn orphan_finding(disposition: StoreDisposition) -> OrphanStoreFinding {
        OrphanStoreFinding {
            project_id: "proj_orphan".to_string(),
            store_id: "store_orphan".to_string(),
            data_root: PathBuf::from("/tmp/does-not-exist/store_orphan"),
            disposition,
            age_secs: 1_000_000,
            size_bytes: 42_000,
            expected_store_relpath: "stores/store_orphan".to_string(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: 0,
            expected_manifest_bytes: None,
            graph_scope_relpaths: Vec::new(),
        }
    }

    #[test]
    fn live_store_yields_no_doctor_finding() {
        assert!(orphan_store_doctor_finding(&orphan_finding(StoreDisposition::Live)).is_none());
    }

    #[test]
    fn orphaned_store_maps_to_degraded_orphan_store_finding() {
        let typed = orphan_store_doctor_finding(&orphan_finding(StoreDisposition::Orphaned))
            .expect("orphaned store produces a typed finding");
        assert_eq!(typed.kind(), DoctorStorageFindingKindV1::OrphanStore);
    }

    #[test]
    fn relinkable_store_maps_to_orphan_store_finding() {
        let typed = orphan_store_doctor_finding(&orphan_finding(StoreDisposition::Relinkable {
            live_root: PathBuf::from("/live/moved/root"),
        }))
        .expect("relinkable store produces a typed finding");
        assert_eq!(typed.kind(), DoctorStorageFindingKindV1::OrphanStore);
    }

    #[test]
    fn unregistered_store_maps_to_orphan_store_finding() {
        let finding = UnregisteredStoreFinding {
            project_dir_name: "proj_ghost".to_string(),
            data_root: PathBuf::from("/tmp/does-not-exist/proj_ghost"),
            age_secs: 1_000_000,
            size_bytes: 4096,
            expected_payload_mtime_secs: 0,
        };
        let typed = unregistered_store_doctor_finding(&finding)
            .expect("unregistered directory produces a typed finding");
        assert_eq!(typed.kind(), DoctorStorageFindingKindV1::OrphanStore);
    }

    #[test]
    fn bounded_statement_clamps_over_long_paths() {
        let long = "x".repeat(DOCTOR_TEXT_LIMIT * 2);
        let clamped = bounded_statement(&long);
        assert!(clamped.len() <= DOCTOR_TEXT_LIMIT);
        // A control character is scrubbed so kernel construction never rejects it.
        assert!(!bounded_statement("a\nb").contains('\n'));
    }

    struct Fixture {
        runtime: crate::doctor::DoctorTestRuntime,
        _tmp: tempfile::TempDir,
        profile_root: PathBuf,
        current_root: PathBuf,
        stale_root: PathBuf,
        manifest_path: PathBuf,
        config_path: PathBuf,
    }

    async fn build_fixture() -> Fixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_root = tmp.path().join("profile");
        let current_root = tmp.path().join("current-name");
        let stale_root = tmp.path().join("old-name");
        std::fs::create_dir_all(&current_root).unwrap();
        let data_root = profile_root.join("stores").join(STORE_ID);
        std::fs::create_dir_all(&data_root).unwrap();
        let manifest_path = data_root.join(crate::storage::STORE_MANIFEST_FILENAME);
        let config_path = data_root.join(crate::config::CONFIG_FILENAME);

        let manifest = StoreManifest {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some(PROJECT_ID.to_string()),
            store_kind: StoreKind::CodeProject,
            storage_mode: StorageMode::ProfileSharded,
            project_root: stale_root.clone(),
            data_root: data_root.clone(),
            graph_db_relpath: PathBuf::from(crate::config::DB_FILENAME),
            sessions_db_relpath: PathBuf::from("sessions.db"),
            branch_meta_relpath: PathBuf::from(crate::storage::BRANCH_META_FILENAME),
        };
        std::fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let config = crate::config::TraceDecayConfig {
            root_dir: stale_root.to_string_lossy().to_string(),
            ..crate::config::TraceDecayConfig::default()
        };
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        let runtime =
            crate::doctor::DoctorTestRuntime::open(&profile_root, "registry-drift-tests").await;
        let global_db = runtime.database();
        global_db
            .upsert_code_project(PROJECT_ID, &current_root, None, None, None)
            .await
            .unwrap();
        global_db
            .upsert_store_instance(StoreInstanceUpsert {
                store_id: STORE_ID.to_string(),
                project_id: PROJECT_ID.to_string(),
                store_kind: "project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: format!("stores/{STORE_ID}"),
                manifest_relpath: None,
                last_verified_at: None,
                last_write_at: None,
            })
            .await
            .unwrap();

        Fixture {
            runtime,
            _tmp: tmp,
            profile_root,
            current_root,
            stale_root,
            manifest_path,
            config_path,
        }
    }

    fn manifest_root(path: &Path) -> PathBuf {
        crate::storage::read_store_manifest(path)
            .unwrap()
            .project_root
    }

    fn config_root_dir(path: &Path) -> String {
        crate::config::load_config_from_path(Path::new("/"), path)
            .unwrap()
            .root_dir
    }

    fn comparable_path(path: &Path) -> String {
        let normalized = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let text = normalized.to_string_lossy();
        text.strip_prefix(r"\\?\")
            .unwrap_or(&text)
            .replace('\\', "/")
    }

    #[tokio::test]
    async fn detection_does_not_mutate() {
        let fx = build_fixture().await;

        let findings = registry_drift_findings(fx.runtime.database(), &fx.profile_root).await;
        let root_drift: Vec<_> = findings
            .iter()
            .filter(|f| f.field == "project_root")
            .collect();
        assert_eq!(root_drift.len(), 1, "should detect project_root drift");
        assert_eq!(manifest_root(&fx.manifest_path), fx.stale_root);
        assert_eq!(
            config_root_dir(&fx.config_path),
            fx.stale_root.to_string_lossy()
        );
    }

    #[tokio::test]
    async fn reconcile_rewrites_then_is_idempotent() {
        let fx = build_fixture().await;
        let canonical = fx.current_root.canonicalize().unwrap();
        let legacy_config_before = std::fs::read_to_string(&fx.config_path).unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(fx.runtime.database(), &fx.profile_root).await;
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(reconciled.len(), 1, "one store should be reconciled");
        assert_eq!(reconciled[0].store_id, STORE_ID);
        assert_eq!(
            reconciled[0].config_path, None,
            "legacy config input must not be reconciled or rewritten"
        );

        assert_eq!(manifest_root(&fx.manifest_path), canonical);
        assert_eq!(
            config_root_dir(&fx.config_path),
            fx.stale_root.to_string_lossy()
        );
        assert_eq!(
            std::fs::read_to_string(&fx.config_path).unwrap(),
            legacy_config_before,
            "manifest reconciliation must not write config.json"
        );

        let healed = crate::storage::read_store_manifest(&fx.manifest_path).unwrap();
        assert_eq!(healed.project_id.as_deref(), Some(PROJECT_ID));
        assert_eq!(
            healed.graph_db_relpath,
            PathBuf::from(crate::config::DB_FILENAME)
        );

        let post = registry_drift_findings(fx.runtime.database(), &fx.profile_root).await;
        assert!(
            post.iter().all(|f| f.field != "project_root"),
            "project_root drift should be resolved: {post:?}"
        );
        let (again, warnings) =
            reconcile_drifted_store_roots(fx.runtime.database(), &fx.profile_root).await;
        assert!(again.is_empty(), "second run must be a no-op: {again:?}");
        assert!(
            warnings.is_empty(),
            "no warnings on no-op run: {warnings:?}"
        );
    }

    #[tokio::test]
    async fn manifest_reconcile_does_not_read_or_rewrite_legacy_config_input() {
        let fx = build_fixture().await;
        let canonical = fx.current_root.canonicalize().unwrap();
        std::fs::write(&fx.config_path, "{ not json").unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(fx.runtime.database(), &fx.profile_root).await;
        assert_eq!(
            reconciled.len(),
            1,
            "manifest rewrite should still be reported"
        );
        assert_eq!(reconciled[0].store_id, STORE_ID);
        assert_eq!(
            comparable_path(&reconciled[0].manifest_path),
            comparable_path(&fx.manifest_path.canonicalize().unwrap())
        );
        assert_eq!(
            reconciled[0].config_path, None,
            "legacy config input must not be reported as reconciled"
        );
        assert_eq!(manifest_root(&fx.manifest_path), canonical);
        assert!(
            warnings.is_empty(),
            "invalid read-only legacy input must not block manifest reconciliation: {warnings:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&fx.config_path).unwrap(),
            "{ not json"
        );
    }

    #[tokio::test]
    async fn missing_canonical_root_is_not_healed() {
        let fx = build_fixture().await;
        std::fs::remove_dir_all(&fx.current_root).unwrap();

        let (reconciled, warnings) =
            reconcile_drifted_store_roots(fx.runtime.database(), &fx.profile_root).await;
        assert!(reconciled.is_empty(), "must not heal a nonexistent root");
        assert!(
            warnings.is_empty(),
            "gate is silent, not a warning: {warnings:?}"
        );
        assert_eq!(manifest_root(&fx.manifest_path), fx.stale_root);
    }
}
