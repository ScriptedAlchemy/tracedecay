//! Doctor Storage-family producers (Plan 38 §7 → Plan 09 §PR14).
//!
//! These pure functions map the storage read models onto the landed
//! [`DoctorFindingV1`] contract, wrapped in a [`DoctorStorageFindingV1`] that
//! carries the typed subclass. They never invent a finding family or evidence
//! state: the family is always [`DoctorFindingFamilyV1::Storage`], the typed
//! subclass is attached as a [`DoctorStorageFindingKindV1`] on the wrapper (Plan
//! 38 §7 review S1 — the kind is a value on the finding, not a slug a consumer
//! must parse out of an evidence string), and the evidence state is chosen so
//! that an observed retention/size problem is `Degraded`/`Stale` (never
//! healthy), an unobservable source is `Unsupported`/`Denied`/`Unknown`, and
//! only a genuinely clean, fully-covered observation is
//! `HealthyCompleteCoverage`. The evidence reference still namespaces the
//! subclass slug for stable provenance, but the typed kind is the source of
//! truth.
//!
//! A budget overage is *never* silent (Plan 38 §7): [`over_budget_finding`]
//! always yields a non-healthy finding when the store is over budget.

use crate::doctor::{
    DoctorCoverageCompletenessV1, DoctorCoverageStatementV1, DoctorEvidenceRefV1,
    DoctorEvidenceReferenceV1, DoctorEvidenceStateV1, DoctorFindingFamilyV1, DoctorFindingV1,
    DoctorOwningOperationRefV1, DoctorRemediationKindV1, DoctorRemediationRefV1,
    DoctorStorageFindingKindV1, DoctorStorageFindingV1,
};
use crate::error::ApplicationContractError;

use super::debris::IncidentDebrisScanV1;
use super::identity::StoreKeyV1;
use super::inventory::{OrphanStoreRecordV1, RetentionBacklogRecordV1, StaleBranchDbRecordV1};
use super::telemetry::{StorageTelemetryReadV1, StoreBudgetEvaluationV1, StoreSizeBudgetV1};

/// Stable slug for a storage finding subclass, embedded in the evidence
/// reference so a consumer can recover the subclass from a `Storage` finding.
const fn kind_slug(kind: DoctorStorageFindingKindV1) -> &'static str {
    match kind {
        DoctorStorageFindingKindV1::OverBudgetStore => "over_budget_store",
        DoctorStorageFindingKindV1::OrphanStore => "orphan_store",
        DoctorStorageFindingKindV1::StaleBranchDbs => "stale_branch_dbs",
        DoctorStorageFindingKindV1::IncidentDebrisPresent => "incident_debris_present",
        DoctorStorageFindingKindV1::RetentionBacklog => "retention_backlog",
    }
}

/// Owning application operation a Doctor finding references for remediation.
/// Doctor never repairs; it names the operation the owner would invoke.
const fn owning_operation(kind: DoctorStorageFindingKindV1) -> &'static str {
    match kind {
        DoctorStorageFindingKindV1::OverBudgetStore
        | DoctorStorageFindingKindV1::RetentionBacklog => {
            "use-case.application.storage.retention-collect"
        }
        DoctorStorageFindingKindV1::OrphanStore => {
            "use-case.application.storage.collect-orphan-store"
        }
        DoctorStorageFindingKindV1::StaleBranchDbs => "use-case.application.storage.branch-gc",
        DoctorStorageFindingKindV1::IncidentDebrisPresent => {
            "use-case.application.storage.quarantine-and-collect-debris"
        }
    }
}

/// Build a single Storage-family evidence reference of the form
/// `storage.<kind>.<store>.<detail>`, bounded to the evidence reference limit.
fn evidence(
    kind: DoctorStorageFindingKindV1,
    store: &StoreKeyV1,
    detail: &str,
) -> Result<DoctorEvidenceRefV1, ApplicationContractError> {
    // Bound the store/detail so the composed reference stays within the 512-byte
    // identifier budget without risking a mid-character truncation panic.
    let reference = format!(
        "storage.{}.{}.{}",
        kind_slug(kind),
        truncate_ascii(store.as_str(), 200),
        truncate_ascii(detail, 200),
    );
    Ok(DoctorEvidenceRefV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceReferenceV1::new(reference)?,
    ))
}

/// Truncate at a char boundary, keeping the reference identifier valid.
fn truncate_ascii(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn remediation(
    kind: DoctorStorageFindingKindV1,
) -> Result<DoctorRemediationRefV1, ApplicationContractError> {
    Ok(DoctorRemediationRefV1::new(
        DoctorOwningOperationRefV1::new(owning_operation(kind))?,
        DoctorRemediationKindV1::Action,
    ))
}

fn coverage(
    completeness: DoctorCoverageCompletenessV1,
    statement: &str,
) -> Result<DoctorCoverageStatementV1, ApplicationContractError> {
    DoctorCoverageStatementV1::new(completeness, statement)
}

/// Map an observed retention/size problem into a non-healthy Storage finding
/// with a remediation reference to the owning collection operation.
fn problem_finding(
    kind: DoctorStorageFindingKindV1,
    store: &StoreKeyV1,
    state: DoctorEvidenceStateV1,
    completeness: DoctorCoverageCompletenessV1,
    detail: &str,
    coverage_statement: &str,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence(kind, store, detail)?],
        coverage(completeness, coverage_statement)?,
        Some(remediation(kind)?),
    )
}

/// Map an unobservable evidence source (unsupported/denied/unknown) into a
/// Storage finding that carries the honest non-healthy state and no remediation.
fn unobservable_finding(
    kind: DoctorStorageFindingKindV1,
    store: &StoreKeyV1,
    state: DoctorEvidenceStateV1,
    detail: &str,
    coverage_statement: &str,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence(kind, store, detail)?],
        coverage(DoctorCoverageCompletenessV1::Unknown, coverage_statement)?,
        None,
    )
}

/// Map a clean observation into either a healthy finding (complete coverage) or
/// an honest non-healthy `Partial` finding (incomplete coverage). Never carries
/// remediation, per the healthy-finding invariant.
fn clean_finding(
    kind: DoctorStorageFindingKindV1,
    store: &StoreKeyV1,
    completeness: DoctorCoverageCompletenessV1,
    detail: &str,
    coverage_statement: &str,
) -> Result<DoctorFindingV1, ApplicationContractError> {
    let state = match completeness {
        DoctorCoverageCompletenessV1::Complete => DoctorEvidenceStateV1::HealthyCompleteCoverage,
        DoctorCoverageCompletenessV1::Partial | DoctorCoverageCompletenessV1::Unknown => {
            DoctorEvidenceStateV1::Partial
        }
    };
    DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence(kind, store, detail)?],
        coverage(completeness, coverage_statement)?,
        None,
    )
}

/// Produce the `OverBudgetStore` finding from a telemetry read and its budget.
///
/// An over-budget store is *always* a non-healthy finding — the budget is never
/// silently ignored (Plan 38 §7). Unobservable telemetry yields an honest
/// unsupported/denied/unknown finding, and a within-budget store yields a
/// healthy finding only when coverage is genuinely complete.
pub fn over_budget_finding(
    budget: &StoreSizeBudgetV1,
    read: &StorageTelemetryReadV1,
    completeness: DoctorCoverageCompletenessV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    let kind = DoctorStorageFindingKindV1::OverBudgetStore;
    let finding = match read {
        StorageTelemetryReadV1::Observed { sample } => match budget.evaluate(sample)? {
            StoreBudgetEvaluationV1::OverBudget {
                observed, overage, ..
            } => problem_finding(
                kind,
                &sample.store,
                DoctorEvidenceStateV1::Degraded,
                completeness,
                &format!("observed-{}b.overage-{}b", observed.get(), overage.get()),
                "store size observed against soft budget",
            )?,
            StoreBudgetEvaluationV1::WithinBudget { observed, .. } => clean_finding(
                kind,
                &sample.store,
                completeness,
                &format!("observed-{}b.within-budget", observed.get()),
                "store size observed within soft budget",
            )?,
        },
        StorageTelemetryReadV1::ObservedBytes {
            store, total_bytes, ..
        } => {
            budget.validate()?;
            if budget.store != *store {
                return Err(ApplicationContractError::Inconsistent {
                    field: "storage budget store mismatch",
                });
            }
            if *total_bytes > budget.soft_limit_bytes {
                problem_finding(
                    kind,
                    store,
                    DoctorEvidenceStateV1::Degraded,
                    completeness,
                    &format!(
                        "observed-{}b.overage-{}b",
                        total_bytes.get(),
                        total_bytes.saturating_sub(budget.soft_limit_bytes).get()
                    ),
                    "store size observed against soft budget",
                )?
            } else {
                clean_finding(
                    kind,
                    store,
                    completeness,
                    &format!("observed-{}b.within-budget", total_bytes.get()),
                    "store size observed within soft budget",
                )?
            }
        }
        StorageTelemetryReadV1::Unsupported { store } => unobservable_finding(
            kind,
            store,
            DoctorEvidenceStateV1::Unsupported,
            "telemetry-unsupported",
            "store size telemetry unsupported on this platform",
        )?,
        StorageTelemetryReadV1::Denied { store } => unobservable_finding(
            kind,
            store,
            DoctorEvidenceStateV1::Denied,
            "telemetry-denied",
            "store size telemetry read denied",
        )?,
        StorageTelemetryReadV1::Unknown { store } => unobservable_finding(
            kind,
            store,
            DoctorEvidenceStateV1::Unknown,
            "telemetry-unknown",
            "store size telemetry undetermined",
        )?,
    };
    DoctorStorageFindingV1::new(kind, finding)
}

/// Produce the `OrphanStore` finding from an orphan inventory record.
pub fn orphan_store_finding(
    record: &OrphanStoreRecordV1,
    completeness: DoctorCoverageCompletenessV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    record.validate()?;
    let kind = DoctorStorageFindingKindV1::OrphanStore;
    let finding = if record.is_orphan() {
        problem_finding(
            kind,
            &record.store,
            DoctorEvidenceStateV1::Degraded,
            completeness,
            &format!(
                "age-{}us.size-{}b",
                record.age_micros(),
                record.size_bytes.get()
            ),
            "store identity no longer resolves to a live repository root",
        )?
    } else {
        clean_finding(
            kind,
            &record.store,
            completeness,
            "identity-resolves",
            "store identity resolves to a live repository root",
        )?
    };
    DoctorStorageFindingV1::new(kind, finding)
}

/// Produce the `StaleBranchDbs` finding from a branch-DB inventory record.
///
/// A branch DB whose ref is gone is `Stale` — its evidence is behind the live
/// git-ref watermark — and references the branch-GC operation.
pub fn stale_branch_dbs_finding(
    record: &StaleBranchDbRecordV1,
    completeness: DoctorCoverageCompletenessV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    let kind = DoctorStorageFindingKindV1::StaleBranchDbs;
    let finding = if record.is_stale() {
        problem_finding(
            kind,
            &record.store,
            DoctorEvidenceStateV1::Stale,
            completeness,
            &format!(
                "branch-{}.size-{}b",
                truncate_ascii(record.branch.as_str(), 120),
                record.size_bytes.get()
            ),
            "branch-scoped store whose git ref is gone awaits lifecycle removal",
        )?
    } else {
        clean_finding(
            kind,
            &record.store,
            completeness,
            "branch-ref-present",
            "branch-scoped store's git ref is still live",
        )?
    };
    DoctorStorageFindingV1::new(kind, finding)
}

/// Produce the `IncidentDebrisPresent` finding from a debris scan.
///
/// Present debris is `Degraded`. An empty scan is healthy only when the sibling
/// listing was exhaustive; a truncated listing can never assert a clean result.
pub fn incident_debris_finding(
    scan: &IncidentDebrisScanV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    let kind = DoctorStorageFindingKindV1::IncidentDebrisPresent;
    let finding = if !scan.is_empty() {
        // Debris present is observed regardless of listing completeness, but the
        // count is only exact when the listing is complete.
        let completeness = if scan.listing_complete {
            DoctorCoverageCompletenessV1::Complete
        } else {
            DoctorCoverageCompletenessV1::Partial
        };
        problem_finding(
            kind,
            &scan.store,
            DoctorEvidenceStateV1::Degraded,
            completeness,
            &format!(
                "count-{}.bytes-{}b",
                scan.artifact_count(),
                scan.total_bytes().get()
            ),
            "quarantine-eligible incident artifacts present beside a live store",
        )?
    } else if scan.listing_complete {
        clean_finding(
            kind,
            &scan.store,
            DoctorCoverageCompletenessV1::Complete,
            "no-debris",
            "no incident debris beside store; sibling listing exhaustive",
        )?
    } else {
        // Empty but the listing was truncated: cannot claim clean.
        clean_finding(
            kind,
            &scan.store,
            DoctorCoverageCompletenessV1::Partial,
            "no-debris-partial-listing",
            "no debris found but sibling listing was truncated",
        )?
    };
    DoctorStorageFindingV1::new(kind, finding)
}

/// Produce the `RetentionBacklog` finding from a retention backlog record.
///
/// Backlog past the retention window is `Stale` — evidence held past its
/// watermark — and references the retention-collection operation.
pub fn retention_backlog_finding(
    record: &RetentionBacklogRecordV1,
    completeness: DoctorCoverageCompletenessV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    record.validate()?;
    let kind = DoctorStorageFindingKindV1::RetentionBacklog;
    let finding = if record.has_backlog() {
        problem_finding(
            kind,
            &record.store,
            DoctorEvidenceStateV1::Stale,
            completeness,
            &format!(
                "table-{}.bytes-{}b",
                truncate_ascii(record.table.as_str(), 100),
                record.past_window_bytes.get()
            ),
            "retention-eligible rows are past their window awaiting collection",
        )?
    } else {
        clean_finding(
            kind,
            &record.store,
            completeness,
            "no-backlog",
            "no retention-eligible rows past the window",
        )?
    };
    DoctorStorageFindingV1::new(kind, finding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::debris::{IncidentDebrisArtifactV1, IncidentDebrisScanV1};
    use crate::storage::identity::{
        BranchRefV1, RelativeArtifactPathV1, StorageByteSizeV1, TableNameV1,
    };
    use crate::storage::telemetry::StoreSizeSampleV1;
    use tracedecay_domain::UtcMicros;

    fn store() -> StoreKeyV1 {
        StoreKeyV1::new("sessions.db").expect("valid")
    }

    fn budget(limit: u64) -> StoreSizeBudgetV1 {
        StoreSizeBudgetV1 {
            store: store(),
            soft_limit_bytes: StorageByteSizeV1(limit),
        }
    }

    fn sample(page_count: u64) -> StoreSizeSampleV1 {
        StoreSizeSampleV1 {
            store: store(),
            page_size_bytes: 4096,
            page_count,
            freelist_pages: 0,
            observed_at: UtcMicros(1),
        }
    }

    fn only_evidence(finding: &DoctorStorageFindingV1) -> &str {
        finding.finding().evidence()[0].reference().as_str()
    }

    // --- OverBudgetStore -----------------------------------------------------

    #[test]
    fn over_budget_store_produces_degraded_finding_with_remediation() {
        // 100 pages * 4096 = 409_600 > 300_000 budget.
        let read = StorageTelemetryReadV1::Observed {
            sample: sample(100),
        };
        let finding = over_budget_finding(
            &budget(300_000),
            &read,
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("finding");
        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::OverBudgetStore);
        assert_eq!(finding.finding().family(), DoctorFindingFamilyV1::Storage);
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
        assert!(finding.finding().remediation().is_some());
        assert!(only_evidence(&finding).starts_with("storage.over_budget_store."));
    }

    #[test]
    fn within_budget_complete_coverage_is_healthy_without_remediation() {
        // 10 pages * 4096 = 40_960 < 300_000 budget.
        let read = StorageTelemetryReadV1::Observed { sample: sample(10) };
        let finding = over_budget_finding(
            &budget(300_000),
            &read,
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("finding");
        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::OverBudgetStore);
        assert!(finding.finding().state().is_healthy_complete());
        assert!(finding.finding().remediation().is_none());
        assert!(finding.finding().coverage().is_complete());
    }

    #[test]
    fn within_budget_partial_coverage_is_not_healthy() {
        let read = StorageTelemetryReadV1::Observed { sample: sample(10) };
        let finding = over_budget_finding(
            &budget(300_000),
            &read,
            DoctorCoverageCompletenessV1::Partial,
        )
        .expect("finding");
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Partial);
        assert!(!finding.finding().state().is_healthy_complete());
    }

    #[test]
    fn unsupported_telemetry_maps_to_unsupported_state() {
        let read = StorageTelemetryReadV1::Unsupported { store: store() };
        let finding = over_budget_finding(
            &budget(300_000),
            &read,
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("finding");
        assert_eq!(
            finding.finding().state(),
            DoctorEvidenceStateV1::Unsupported
        );
        assert!(finding.finding().remediation().is_none());
    }

    #[test]
    fn denied_and_unknown_telemetry_map_to_their_states() {
        for (read, expected) in [
            (
                StorageTelemetryReadV1::Denied { store: store() },
                DoctorEvidenceStateV1::Denied,
            ),
            (
                StorageTelemetryReadV1::Unknown { store: store() },
                DoctorEvidenceStateV1::Unknown,
            ),
        ] {
            let finding = over_budget_finding(
                &budget(300_000),
                &read,
                DoctorCoverageCompletenessV1::Complete,
            )
            .expect("finding");
            assert_eq!(finding.finding().state(), expected);
        }
    }

    // --- OrphanStore ---------------------------------------------------------

    #[test]
    fn orphan_store_produces_degraded_finding() {
        let record = OrphanStoreRecordV1 {
            store: store(),
            identity_resolves: false,
            size_bytes: StorageByteSizeV1(41_000_000_000),
            first_unresolved_at: UtcMicros(100),
            observed_at: UtcMicros(1_000),
        };
        let finding =
            orphan_store_finding(&record, DoctorCoverageCompletenessV1::Complete).expect("finding");
        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::OrphanStore);
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
        assert!(only_evidence(&finding).starts_with("storage.orphan_store."));
        assert!(finding.finding().remediation().is_some());
    }

    #[test]
    fn resolved_store_produces_healthy_finding() {
        let record = OrphanStoreRecordV1 {
            store: store(),
            identity_resolves: true,
            size_bytes: StorageByteSizeV1(1_000),
            first_unresolved_at: UtcMicros(100),
            observed_at: UtcMicros(1_000),
        };
        let finding =
            orphan_store_finding(&record, DoctorCoverageCompletenessV1::Complete).expect("finding");
        assert!(finding.finding().state().is_healthy_complete());
    }

    // --- StaleBranchDbs ------------------------------------------------------

    #[test]
    fn stale_branch_dbs_produces_stale_finding() {
        let record = StaleBranchDbRecordV1 {
            store: StoreKeyV1::new("branches/feature-x").expect("valid"),
            branch: BranchRefV1::new("feature-x").expect("valid"),
            ref_present: false,
            size_bytes: StorageByteSizeV1(40_000_000_000),
        };
        let finding = stale_branch_dbs_finding(&record, DoctorCoverageCompletenessV1::Complete)
            .expect("finding");
        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::StaleBranchDbs);
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Stale);
        assert!(only_evidence(&finding).starts_with("storage.stale_branch_dbs."));
    }

    #[test]
    fn live_branch_produces_healthy_finding() {
        let record = StaleBranchDbRecordV1 {
            store: StoreKeyV1::new("branches/main").expect("valid"),
            branch: BranchRefV1::new("main").expect("valid"),
            ref_present: true,
            size_bytes: StorageByteSizeV1(1_000),
        };
        let finding = stale_branch_dbs_finding(&record, DoctorCoverageCompletenessV1::Complete)
            .expect("finding");
        assert!(finding.finding().state().is_healthy_complete());
    }

    // --- IncidentDebrisPresent ----------------------------------------------

    fn debris_artifact(bytes: u64) -> IncidentDebrisArtifactV1 {
        let path = RelativeArtifactPathV1::new("sessions.db.corrupt-1721692800").expect("valid");
        IncidentDebrisArtifactV1::classify_path(
            store(),
            path,
            StorageByteSizeV1(bytes),
            UtcMicros(1),
        )
        .expect("ok")
        .expect("debris")
    }

    #[test]
    fn incident_debris_present_produces_degraded_finding() {
        let scan = IncidentDebrisScanV1 {
            store: store(),
            artifacts: vec![debris_artifact(800_000_000)],
            listing_complete: true,
        };
        let finding = incident_debris_finding(&scan).expect("finding");
        assert_eq!(
            finding.kind(),
            DoctorStorageFindingKindV1::IncidentDebrisPresent
        );
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
        assert!(only_evidence(&finding).starts_with("storage.incident_debris_present."));
        assert!(finding.finding().remediation().is_some());
    }

    #[test]
    fn empty_complete_debris_scan_is_healthy() {
        let scan = IncidentDebrisScanV1 {
            store: store(),
            artifacts: Vec::new(),
            listing_complete: true,
        };
        let finding = incident_debris_finding(&scan).expect("finding");
        assert!(finding.finding().state().is_healthy_complete());
        assert!(finding.finding().remediation().is_none());
    }

    #[test]
    fn empty_partial_debris_scan_is_not_healthy() {
        let scan = IncidentDebrisScanV1 {
            store: store(),
            artifacts: Vec::new(),
            listing_complete: false,
        };
        let finding = incident_debris_finding(&scan).expect("finding");
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Partial);
    }

    // --- RetentionBacklog ----------------------------------------------------

    #[test]
    fn retention_backlog_produces_stale_finding() {
        let record = RetentionBacklogRecordV1 {
            store: store(),
            table: TableNameV1::new("lcm_raw_messages").expect("valid"),
            past_window_bytes: StorageByteSizeV1(3_800_000_000),
            oldest_past_window_at: UtcMicros(10),
            window_watermark_at: UtcMicros(1_000),
        };
        let finding = retention_backlog_finding(&record, DoctorCoverageCompletenessV1::Complete)
            .expect("finding");
        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::RetentionBacklog);
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Stale);
        assert!(only_evidence(&finding).starts_with("storage.retention_backlog."));
    }

    #[test]
    fn no_retention_backlog_produces_healthy_finding() {
        let record = RetentionBacklogRecordV1 {
            store: store(),
            table: TableNameV1::new("lcm_raw_messages").expect("valid"),
            past_window_bytes: StorageByteSizeV1::ZERO,
            oldest_past_window_at: UtcMicros(10),
            window_watermark_at: UtcMicros(1_000),
        };
        let finding = retention_backlog_finding(&record, DoctorCoverageCompletenessV1::Complete)
            .expect("finding");
        assert!(finding.finding().state().is_healthy_complete());
    }

    // --- Cross-cutting -------------------------------------------------------

    #[test]
    fn all_five_finding_kinds_are_producible_and_family_storage() {
        let over = over_budget_finding(
            &budget(1),
            &StorageTelemetryReadV1::Observed {
                sample: sample(100),
            },
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("over budget");
        let orphan = orphan_store_finding(
            &OrphanStoreRecordV1 {
                store: store(),
                identity_resolves: false,
                size_bytes: StorageByteSizeV1(1),
                first_unresolved_at: UtcMicros(1),
                observed_at: UtcMicros(2),
            },
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("orphan");
        let stale = stale_branch_dbs_finding(
            &StaleBranchDbRecordV1 {
                store: store(),
                branch: BranchRefV1::new("gone").expect("valid"),
                ref_present: false,
                size_bytes: StorageByteSizeV1(1),
            },
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("stale");
        let debris = incident_debris_finding(&IncidentDebrisScanV1 {
            store: store(),
            artifacts: vec![debris_artifact(1)],
            listing_complete: true,
        })
        .expect("debris");
        let backlog = retention_backlog_finding(
            &RetentionBacklogRecordV1 {
                store: store(),
                table: TableNameV1::new("observations").expect("valid"),
                past_window_bytes: StorageByteSizeV1(1),
                oldest_past_window_at: UtcMicros(1),
                window_watermark_at: UtcMicros(2),
            },
            DoctorCoverageCompletenessV1::Complete,
        )
        .expect("backlog");

        // Each producer attaches its typed subclass to the finding (Plan 38 §7
        // review S1) — the kind is recovered by value, not by parsing evidence.
        assert_eq!(over.kind(), DoctorStorageFindingKindV1::OverBudgetStore);
        assert_eq!(orphan.kind(), DoctorStorageFindingKindV1::OrphanStore);
        assert_eq!(stale.kind(), DoctorStorageFindingKindV1::StaleBranchDbs);
        assert_eq!(
            debris.kind(),
            DoctorStorageFindingKindV1::IncidentDebrisPresent
        );
        assert_eq!(backlog.kind(), DoctorStorageFindingKindV1::RetentionBacklog);

        for finding in [&over, &orphan, &stale, &debris, &backlog] {
            assert_eq!(finding.finding().family(), DoctorFindingFamilyV1::Storage);
            assert!(!finding.finding().state().is_healthy_complete());
            assert!(finding.finding().remediation().is_some());
        }
    }
}
