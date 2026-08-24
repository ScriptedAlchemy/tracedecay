use tracedecay_application::doctor::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1,
};

use crate::retention::orphan_stores::{
    OrphanStoreFinding, StoreDisposition, UnregisteredStoreFinding, UnverifiableReason,
};

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
/// its observed disposition. Returns `None` when the store identity cannot form
/// a valid evidence reference.
pub(crate) fn orphan_store_doctor_finding(
    finding: &OrphanStoreFinding,
) -> Option<DoctorStorageFindingV1> {
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
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::retention::orphan_stores::{StoreContentFence, StoreDirectoryFence};

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
            project_dir_name: "proj_ghost".to_string(),
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
        // A control character is scrubbed so kernel construction never rejects it.
        assert!(!bounded_statement("a\nb").contains('\n'));
    }
}
