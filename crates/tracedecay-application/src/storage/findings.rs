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
use super::inventory::{
    CodeGenerationRetentionRecordV1, OrphanStoreRecordV1, RetentionBacklogRecordV1,
    StaleBranchDbRecordV1,
};
use super::telemetry::{
    StorageTelemetryReadV1, StoreBudgetEvaluationV1, StoreSizeBudgetV1, TableGrowthDoctorEvidenceV1,
};

/// Stable slug for a storage finding subclass, embedded in the evidence
/// reference so a consumer can recover the subclass from a `Storage` finding.
const fn kind_slug(kind: DoctorStorageFindingKindV1) -> &'static str {
    match kind {
        DoctorStorageFindingKindV1::OverBudgetStore => "over_budget_store",
        DoctorStorageFindingKindV1::OrphanStore => "orphan_store",
        DoctorStorageFindingKindV1::StaleBranchDbs => "stale_branch_dbs",
        DoctorStorageFindingKindV1::IncidentDebrisPresent => "incident_debris_present",
        DoctorStorageFindingKindV1::RetentionBacklog => "retention_backlog",
        DoctorStorageFindingKindV1::TableGrowth => "table_growth",
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
        DoctorStorageFindingKindV1::TableGrowth => "use-case.application.storage.telemetry.read",
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
        truncate_at_char_boundary(store.as_str(), 200),
        truncate_at_char_boundary(detail, 200),
    );
    Ok(DoctorEvidenceRefV1::new(
        DoctorFindingFamilyV1::Storage,
        DoctorEvidenceReferenceV1::new(reference)?,
    ))
}

/// Truncate to at most `max` bytes, cutting at a char boundary so the result
/// stays valid UTF-8 (and a truncated reference identifier stays well formed).
pub(crate) fn truncate_at_char_boundary(value: &str, max: usize) -> String {
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

/// Wrap one prepared table-growth evidence item in the canonical typed Storage
/// finding. Significant growth is informational and carries no remediation;
/// baseline and unavailable reads retain their exact non-healthy evidence state.
pub fn table_growth_finding(
    evidence_item: &TableGrowthDoctorEvidenceV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    let kind = DoctorStorageFindingKindV1::TableGrowth;
    let (store, state, completeness, detail, statement) = match evidence_item {
        TableGrowthDoctorEvidenceV1::SignificantGrowth {
            store,
            table,
            previous_bytes,
            current_bytes,
            growth_bytes,
            previous_observed_at,
            current_observed_at,
        } => (
            store,
            DoctorEvidenceStateV1::HealthyCompleteCoverage,
            DoctorCoverageCompletenessV1::Complete,
            format!(
                "table-{}.previous-{}b.current-{}b.growth-{}b.from-{}us.to-{}us",
                table.as_str(),
                previous_bytes.get(),
                current_bytes.get(),
                growth_bytes.get(),
                previous_observed_at.0,
                current_observed_at.0,
            ),
            "table payload growth crossed the informational significance threshold",
        ),
        TableGrowthDoctorEvidenceV1::BaselineEstablished {
            store,
            observed_at,
            tables_observed,
        } => (
            store,
            DoctorEvidenceStateV1::Partial,
            DoctorCoverageCompletenessV1::Partial,
            format!(
                "baseline-pending.tables-{tables_observed}.observed-at-{}us",
                observed_at.0
            ),
            "table payload baseline established; growth needs a subsequent observation",
        ),
        TableGrowthDoctorEvidenceV1::TableBaselinePending {
            store,
            table,
            current_bytes,
            observed_at,
        } => (
            store,
            DoctorEvidenceStateV1::Partial,
            DoctorCoverageCompletenessV1::Partial,
            format!(
                "table-{}.baseline-pending.current-{}b.observed-at-{}us",
                table.as_str(),
                current_bytes.get(),
                observed_at.0,
            ),
            "table has no previous payload watermark; growth remains baseline-pending",
        ),
        TableGrowthDoctorEvidenceV1::Unsupported { store } => (
            store,
            DoctorEvidenceStateV1::Unsupported,
            DoctorCoverageCompletenessV1::Unknown,
            "unsupported".to_string(),
            "table payload growth measurement is unsupported",
        ),
        TableGrowthDoctorEvidenceV1::Denied { store } => (
            store,
            DoctorEvidenceStateV1::Denied,
            DoctorCoverageCompletenessV1::Unknown,
            "denied".to_string(),
            "table payload growth measurement was denied",
        ),
        TableGrowthDoctorEvidenceV1::Unknown { store } => (
            store,
            DoctorEvidenceStateV1::Unknown,
            DoctorCoverageCompletenessV1::Unknown,
            "unknown".to_string(),
            "table payload growth measurement is unavailable",
        ),
    };
    let finding = DoctorFindingV1::new(
        DoctorFindingFamilyV1::Storage,
        state,
        vec![evidence(kind, store, &detail)?],
        coverage(completeness, statement)?,
        None,
    )?;
    DoctorStorageFindingV1::new(kind, finding)
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
                truncate_at_char_boundary(record.branch.as_str(), 120),
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
                truncate_at_char_boundary(record.table.as_str(), 100),
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

/// Report immutable code-generation retention with the total superseded
/// footprint, the exact liveness-based collectable subset, and the disjoint
/// stranded-scope class one level up.
///
/// Both classes share one finding because they describe the same store from the
/// owner's point of view — "how many code-index bytes are being held that
/// nothing reads". They are reported as separate numbers because a scope-local
/// generation census structurally cannot see a stranded sibling scope, and
/// folding the two totals together would let a clean generation census hide
/// gigabytes of unreachable directories.
pub fn code_generation_retention_finding(
    record: &CodeGenerationRetentionRecordV1,
    completeness: DoctorCoverageCompletenessV1,
) -> Result<DoctorStorageFindingV1, ApplicationContractError> {
    record.validate()?;
    let kind = DoctorStorageFindingKindV1::RetentionBacklog;
    let detail = format!(
        "superseded-{}.bytes-{}b.collectable-{}.collectable-bytes-{}b.stranded-scopes-{}.stranded-scope-bytes-{}b",
        record.superseded_generation_count,
        record.superseded_generation_bytes.get(),
        record.collectable_generation_count,
        record.collectable_generation_bytes.get(),
        record.stranded_scope_count,
        record.stranded_scope_bytes.get(),
    );
    let finding = match (
        record.has_collectable_generations(),
        record.has_stranded_scopes(),
    ) {
        (_, true) => problem_finding(
            kind,
            &record.store,
            DoctorEvidenceStateV1::Stale,
            completeness,
            &detail,
            "code-index scope roots whose project root no longer exists hold bytes no scope-local retention pass can reach",
        )?,
        (true, false) => problem_finding(
            kind,
            &record.store,
            DoctorEvidenceStateV1::Stale,
            completeness,
            &detail,
            "superseded code generations outside active, vector-readable, and rollback-floor liveness await collection",
        )?,
        (false, false) => clean_finding(
            kind,
            &record.store,
            completeness,
            &detail,
            "superseded code generations are bounded by exact liveness and rollback floor; every scope root resolves to a live project root",
        )?,
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
    use crate::storage::telemetry::{StoreSizeSampleV1, TableGrowthDoctorEvidenceV1};
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

    // --- TableGrowth ---------------------------------------------------------

    #[test]
    fn significant_table_growth_is_informational_without_remediation() {
        let finding = table_growth_finding(&TableGrowthDoctorEvidenceV1::SignificantGrowth {
            store: store(),
            table: TableNameV1::new("messages").expect("valid"),
            previous_bytes: StorageByteSizeV1(10 * 1024 * 1024),
            current_bytes: StorageByteSizeV1(11 * 1024 * 1024),
            growth_bytes: StorageByteSizeV1(1024 * 1024),
            previous_observed_at: UtcMicros(1),
            current_observed_at: UtcMicros(2),
        })
        .expect("finding");

        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::TableGrowth);
        assert!(finding.finding().state().is_healthy_complete());
        assert!(finding.finding().remediation().is_none());
        assert!(only_evidence(&finding).contains("table-messages"));
        assert!(only_evidence(&finding).contains("growth-1048576b"));
    }

    #[test]
    fn table_growth_baseline_is_partial_and_unknown_has_no_zero_measurement() {
        let baseline = table_growth_finding(&TableGrowthDoctorEvidenceV1::BaselineEstablished {
            store: store(),
            observed_at: UtcMicros(2),
            tables_observed: 3,
        })
        .expect("baseline finding");
        assert_eq!(baseline.finding().state(), DoctorEvidenceStateV1::Partial);
        assert!(only_evidence(&baseline).contains("baseline-pending"));

        let unknown =
            table_growth_finding(&TableGrowthDoctorEvidenceV1::Unknown { store: store() })
                .expect("unknown finding");
        assert_eq!(unknown.finding().state(), DoctorEvidenceStateV1::Unknown);
        assert!(!only_evidence(&unknown).contains("0b"));
        assert!(unknown.finding().remediation().is_none());
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

    #[test]
    fn code_generation_retention_reports_superseded_count_and_bytes() {
        let record = super::super::inventory::CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 27,
            superseded_generation_bytes: StorageByteSizeV1(22_980_254_208),
            collectable_generation_count: 24,
            collectable_generation_bytes: StorageByteSizeV1(20_600_000_000),
            stranded_scope_count: 0,
            stranded_scope_bytes: StorageByteSizeV1(0),
        };

        let finding =
            code_generation_retention_finding(&record, DoctorCoverageCompletenessV1::Complete)
                .expect("finding");

        assert_eq!(finding.kind(), DoctorStorageFindingKindV1::RetentionBacklog);
        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Stale);
        assert!(only_evidence(&finding).contains("superseded-27"));
        assert!(only_evidence(&finding).contains("bytes-22980254208b"));
        assert!(only_evidence(&finding).contains("collectable-24"));
    }

    /// A scope root whose project root is gone is unreachable by the per-scope
    /// generation census, so a clean generation plan must never present it as
    /// healthy.
    #[test]
    fn stranded_code_index_scopes_are_reported_even_when_generations_are_clean() {
        let record = super::super::inventory::CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 4,
            superseded_generation_bytes: StorageByteSizeV1(4_000),
            collectable_generation_count: 0,
            collectable_generation_bytes: StorageByteSizeV1(0),
            stranded_scope_count: 2,
            stranded_scope_bytes: StorageByteSizeV1(7_730_941_132),
        };

        let finding =
            code_generation_retention_finding(&record, DoctorCoverageCompletenessV1::Complete)
                .expect("finding");

        assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Stale);
        assert!(only_evidence(&finding).contains("stranded-scopes-2"));
        assert!(only_evidence(&finding).contains("stranded-scope-bytes-7730941132b"));
        assert!(finding.finding().remediation().is_some());
    }

    #[test]
    fn code_index_retention_is_clean_only_without_collectable_or_stranded_bytes() {
        let record = super::super::inventory::CodeGenerationRetentionRecordV1 {
            store: StoreKeyV1::new("code-index-v1").expect("valid"),
            superseded_generation_count: 3,
            superseded_generation_bytes: StorageByteSizeV1(3_000),
            collectable_generation_count: 0,
            collectable_generation_bytes: StorageByteSizeV1(0),
            stranded_scope_count: 0,
            stranded_scope_bytes: StorageByteSizeV1(0),
        };

        let finding =
            code_generation_retention_finding(&record, DoctorCoverageCompletenessV1::Complete)
                .expect("finding");

        assert!(finding.finding().state().is_healthy_complete());
        assert!(only_evidence(&finding).contains("stranded-scopes-0"));
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
