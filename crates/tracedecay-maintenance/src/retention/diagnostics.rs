//! Read-only retention diagnostics owned beside the retention kernels.

use std::path::{Path, PathBuf};

use tracedecay_contracts::doctor::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1, DoctorStorageFindingV1,
    storage_family_read,
};
use tracedecay_contracts::storage::{StoreKeyV1, retention_backlog_finding};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_lcm::LcmRetentionConfig;

use super::orphan_stores::{
    OrphanStoreFinding, StoreDisposition, UnregisteredStoreFinding, UnverifiableReason,
};

const MAX_SYNCHRONOUS_SCAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_ENTRIES: usize = 4_096;
const DOCTOR_TEXT_LIMIT: usize = 512;

fn permits_synchronous_session_retention_backlog(database_path: &Path) -> bool {
    ["", "-wal", "-shm"]
        .into_iter()
        .try_fold(0_u64, |total, suffix| {
            let mut path = database_path.as_os_str().to_os_string();
            path.push(suffix);
            match std::fs::metadata(PathBuf::from(path)) {
                Ok(metadata) => total
                    .checked_add(metadata.len())
                    .filter(|size| *size <= MAX_SYNCHRONOUS_SCAN_BYTES),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(total),
                Err(_) => None,
            }
        })
        .is_some()
}

/// Reads the configured session-retention backlog without acquiring a writer
/// or turning incomplete coverage into a healthy empty result.
#[hotpath::measure(label = "maintenance.diagnostics.session_retention", future = true)]
pub async fn collect_session_retention_findings(
    sessions: &RegisteredGlobalDb,
    retention: &LcmRetentionConfig,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    if !permits_synchronous_session_retention_backlog(sessions.db_path()) {
        return DoctorStorageFamilyReadV1::Unknown;
    }
    let Some(file_name) = sessions
        .db_path()
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(store) = StoreKeyV1::new(file_name.to_owned()) else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(snapshot) = sessions.read_snapshot().await else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    let Ok(records) = tracedecay_lcm::retention::read_session_retention_backlog(
        &snapshot,
        store,
        retention,
        observed_at_secs,
    )
    .await
    else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    hotpath::gauge!("maintenance.diagnostics.session_retention_records_total")
        .inc(records.len() as u64);
    let mut findings = Vec::with_capacity(records.len());
    for record in records {
        let Ok(finding) =
            retention_backlog_finding(&record, DoctorCoverageCompletenessV1::Complete)
        else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

pub struct ProfileStorageFindingsV1 {
    pub orphan_stores: DoctorStorageFamilyReadV1,
    pub unregistered_stores: DoctorStorageFamilyReadV1,
    pub incident_debris: DoctorStorageFamilyReadV1,
}

#[hotpath::measure(label = "maintenance.diagnostics.profile_storage", future = true)]
pub async fn collect_profile_storage_findings(
    global_db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    observed_at_secs: i64,
) -> ProfileStorageFindingsV1 {
    let scan_root = profile_root.join("projects");
    let permitted = tokio::task::spawn_blocking(move || {
        hotpath::measure_block!(
            "maintenance.diagnostics.profile_scan",
            permits_synchronous_exhaustive_scan(&scan_root)
        )
    })
    .await
    .is_ok_and(|permitted| permitted);
    if !permitted {
        return ProfileStorageFindingsV1::unknown();
    }
    let (registered_census, unregistered) = tokio::join!(
        super::orphan_stores::build_store_census(global_db, profile_root),
        collect_unregistered_store_findings(
            global_db,
            profile_root,
            retention_secs,
            observed_at_secs,
        ),
    );
    let (orphan_stores, incident_debris) = registered_census.as_deref().map_or(
        (
            DoctorStorageFamilyReadV1::Unknown,
            DoctorStorageFamilyReadV1::Unknown,
        ),
        |census| {
            (
                orphan_store_findings_from_census(census, retention_secs, observed_at_secs),
                incident_debris_findings_from_census(census, profile_root, observed_at_secs),
            )
        },
    );
    ProfileStorageFindingsV1 {
        orphan_stores,
        unregistered_stores: unregistered,
        incident_debris,
    }
}

impl ProfileStorageFindingsV1 {
    fn unknown() -> Self {
        Self {
            orphan_stores: DoctorStorageFamilyReadV1::Unknown,
            unregistered_stores: DoctorStorageFamilyReadV1::Unknown,
            incident_debris: DoctorStorageFamilyReadV1::Unknown,
        }
    }
}

fn orphan_store_findings_from_census(
    census: &[super::orphan_stores::StoreCensusEntry],
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let classified = super::orphan_stores::classify_stores(census, now);
    let plan = super::orphan_stores::plan_collection(classified, retention_secs);
    storage_family_read(
        plan.collect
            .iter()
            .chain(plan.retained_immature.iter())
            .chain(plan.relink.iter())
            .filter_map(orphan_store_doctor_finding)
            .collect(),
    )
}

async fn collect_unregistered_store_findings(
    global_db: &RegisteredGlobalDb,
    profile_root: &Path,
    retention_secs: i64,
    now: i64,
) -> DoctorStorageFamilyReadV1 {
    let report = super::orphan_stores::sweep_unregistered_stores(
        global_db,
        profile_root,
        retention_secs,
        now,
        false,
    )
    .await;
    let Ok(report) = report else {
        return DoctorStorageFamilyReadV1::Unknown;
    };
    hotpath::gauge!("maintenance.diagnostics.unregistered_stores_total")
        .inc((report.plan.collect.len() + report.plan.retained_immature.len()) as u64);
    storage_family_read(
        report
            .plan
            .collect
            .iter()
            .chain(report.plan.retained_immature.iter())
            .filter_map(unregistered_store_doctor_finding)
            .collect(),
    )
}

fn incident_debris_findings_from_census(
    census: &[super::orphan_stores::StoreCensusEntry],
    profile_root: &Path,
    observed_at_secs: i64,
) -> DoctorStorageFamilyReadV1 {
    let mut findings = Vec::new();
    for entry in census {
        let Ok(scan) =
            super::incident_debris::scan_incident_debris(entry, profile_root, observed_at_secs)
        else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        let Ok(finding) = tracedecay_contracts::storage::incident_debris_finding(&scan) else {
            return DoctorStorageFamilyReadV1::Unknown;
        };
        findings.push(finding);
    }
    storage_family_read(findings)
}

fn permits_synchronous_exhaustive_scan(root: &Path) -> bool {
    let mut pending = vec![root.to_path_buf()];
    let mut observed_bytes = 0_u64;
    let mut observed_entries = 0_usize;
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            observed_entries = observed_entries.saturating_add(1);
            if observed_entries > MAX_SYNCHRONOUS_EXHAUSTIVE_SCAN_ENTRIES {
                return false;
            }
            let Ok(file_type) = entry.file_type() else {
                return false;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                return false;
            }
            let Ok(metadata) = entry.metadata() else {
                return false;
            };
            observed_bytes = observed_bytes.saturating_add(metadata.len());
            if observed_bytes > MAX_SYNCHRONOUS_SCAN_BYTES {
                return false;
            }
        }
    }
    true
}

fn bounded_statement(statement: &str) -> String {
    let cleaned: String = statement
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.len() <= DOCTOR_TEXT_LIMIT {
        return cleaned.to_string();
    }
    let mut end = DOCTOR_TEXT_LIMIT;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].trim().to_string()
}

fn orphan_store_doctor_finding(finding: &OrphanStoreFinding) -> Option<DoctorStorageFindingV1> {
    let (state, statement) = match &finding.disposition {
        StoreDisposition::Live => return None,
        StoreDisposition::Unverifiable { reason } => (
            DoctorEvidenceStateV1::Unknown,
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
            format!(
                "orphan store '{}' (project '{}') has no live root: {} bytes, idle {}s",
                finding.store_id, finding.project_id, finding.size_bytes, finding.age_secs
            ),
        ),
        StoreDisposition::Relinkable { live_root } => (
            DoctorEvidenceStateV1::Stale,
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
    let core = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence],
        coverage,
    )
    .ok()?;
    DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, core).ok()
}

fn unregistered_store_doctor_finding(
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
    let core = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceStateV1::Degraded,
        vec![evidence],
        coverage,
    )
    .ok()?;
    DoctorStorageFindingV1::new(DoctorStorageFindingKindV1::OrphanStore, core).ok()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::orphan_stores::{
        OrphanStoreFinding, StoreContentFence, StoreDirectoryFence, StoreDisposition,
        UnregisteredStoreFinding,
    };
    use super::*;

    #[test]
    fn retention_backlog_budget_counts_database_wal_and_shm() {
        let temporary = tempfile::tempdir().expect("retention backlog budget");
        let database = temporary.path().join("sessions.db");
        std::fs::write(&database, b"small").expect("database fixture");
        assert!(permits_synchronous_session_retention_backlog(&database));

        std::fs::File::create(temporary.path().join("sessions.db-wal"))
            .expect("WAL fixture")
            .set_len(MAX_SYNCHRONOUS_SCAN_BYTES + 1)
            .expect("oversized WAL fixture");
        assert!(!permits_synchronous_session_retention_backlog(&database));
    }

    fn orphan_finding(disposition: StoreDisposition) -> OrphanStoreFinding {
        OrphanStoreFinding {
            project_id: "proj_orphan".to_owned(),
            store_id: "store_orphan".to_owned(),
            data_root: PathBuf::from("/tmp/does-not-exist/store_orphan"),
            disposition,
            age_secs: 1_000_000,
            size_bytes: 42_000,
            expected_store_relpath: "stores/store_orphan".to_owned(),
            expected_created_at: 0,
            expected_last_write_at: None,
            expected_payload_mtime_secs: 0,
            expected_data_root_fence: StoreDirectoryFence::Unverifiable,
            expected_content_fence: StoreContentFence::Unverifiable,
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
            project_dir_name: "proj_ghost".to_owned(),
            data_root: PathBuf::from("/tmp/does-not-exist/proj_ghost"),
            age_secs: 1_000_000,
            size_bytes: 4096,
            expected_payload_mtime_secs: 0,
            expected_data_root_fence: StoreDirectoryFence::Unverifiable,
            expected_content_fence: StoreContentFence::Unverifiable,
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
        assert!(!bounded_statement("a\nb").contains('\n'));
    }

    #[test]
    fn synchronous_exhaustive_scans_are_bounded_before_work_starts() {
        let temporary = tempfile::TempDir::new().expect("scan budget");
        let small = temporary.path().join("small");
        std::fs::create_dir(&small).expect("small root");
        std::fs::write(small.join("payload"), b"small").expect("small payload");
        assert!(permits_synchronous_exhaustive_scan(&small));

        let large = temporary.path().join("large");
        std::fs::create_dir(&large).expect("large root");
        std::fs::File::create(large.join("payload"))
            .expect("large payload")
            .set_len(MAX_SYNCHRONOUS_SCAN_BYTES + 1)
            .expect("oversized payload");
        assert!(!permits_synchronous_exhaustive_scan(&large));
    }
}
